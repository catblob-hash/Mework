//! Scoring a tool call's original blocks.
//!
//! A find tool cuts its material into original blocks ([`super::chunk`]) — a function, a log
//! record, a console entry, a path — and every block is scored on its own against the query. Several
//! blocks share one request, each with a question of its own, as many as the request budget holds;
//! a block too long for one question is scored in parts and scores what its best part scores. What
//! goes back to the model is whole blocks: every block at or above the threshold, however long.
//!
//! Requests run on a bounded pool of worker threads that are detached rather than joined, so
//! stopping the turn returns at once while any in-flight request finishes on its own.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::cancel::CancelSignal;

use super::chunk::{batches, request_budget};
use super::jev::{Jev, ScoreRubric};
use super::DecisionError;

/// Concurrent requests in flight for one call. The rate itself is held by the pacer in
/// [`super::jev`], which every tool call shares; this only bounds how many threads wait on it.
/// TypeSafe's own cookbooks run four to eight at a time against the public endpoint.
pub const WORKERS: usize = 8;
/// How often the receive loop looks at the cancellation signal while requests are in flight.
const CANCELLATION_PROBE_INTERVAL: Duration = Duration::from_millis(100);

/// What one question is about: `text` is what the model judges, `label` where it came from
/// (`candidate.source`), `context` optional surrounding material the rubric may refer to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub label: String,
    pub text: String,
    pub context: Option<String>,
}

impl Candidate {
    pub fn new(label: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            text: text.into(),
            context: None,
        }
    }
}

/// One original block to score: how it is addressed, and the parts the decision model is shown —
/// the whole block, or several parts of one too long for a single question, each with the
/// positions it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub label: String,
    pub parts: Vec<Part>,
    pub context: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    pub text: String,
    pub first: usize,
    pub last: usize,
}

impl Block {
    /// A block shown whole.
    pub fn whole(label: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            parts: vec![Part {
                text: text.into(),
                first: 0,
                last: 0,
            }],
            context: None,
        }
    }

    /// A block shown in the given parts: `(text, first, last)`, as [`super::chunk::Blocks::parts`]
    /// cuts them.
    pub fn in_parts(label: impl Into<String>, parts: Vec<(String, usize, usize)>) -> Self {
        Self {
            label: label.into(),
            parts: parts
                .into_iter()
                .map(|(text, first, last)| Part { text, first, last })
                .collect(),
            context: None,
        }
    }

    pub fn with_context(mut self, context: impl Into<String>) -> Self {
        self.context = Some(context.into());
        self
    }
}

/// Something that turns a query and candidates into `0..=1` scores.
pub trait Scorer: Send + Sync {
    fn score(&self, query: &str, candidate: &Candidate) -> Result<f64, DecisionError>;

    /// One request about every candidate at once. `Err` is the whole request failing; `None` is
    /// one answer missing from a request that otherwise came back. By default each candidate is
    /// scored on its own, a transient failure leaving just that one unscored.
    fn score_batch(
        &self,
        query: &str,
        candidates: &[Candidate],
    ) -> Result<Vec<Option<f64>>, DecisionError> {
        candidates
            .iter()
            .map(|candidate| match self.score(query, candidate) {
                Ok(score) => Ok(Some(score)),
                Err(DecisionError::Transient(_)) => Ok(None),
                Err(error) => Err(error),
            })
            .collect()
    }
}

/// The production scorer: one Jev request per batch, one `score` question per candidate, against
/// a fixed rubric. The candidates are the state's `candidates` array, and question `i` points at
/// `candidates[i]`.
pub struct JevScorer {
    pub jev: Jev,
    pub rubric: ScoreRubric,
}

impl Scorer for JevScorer {
    fn score(&self, query: &str, candidate: &Candidate) -> Result<f64, DecisionError> {
        self.score_batch(query, std::slice::from_ref(candidate))?
            .into_iter()
            .next()
            .flatten()
            .ok_or_else(|| DecisionError::Transient("Jev returned no answer".to_owned()))
    }

    fn score_batch(
        &self,
        query: &str,
        candidates: &[Candidate],
    ) -> Result<Vec<Option<f64>>, DecisionError> {
        let candidates = candidates
            .iter()
            .map(|candidate| {
                let mut value = json!({ "source": candidate.label, "text": candidate.text });
                if let Some(context) = &candidate.context {
                    value["context"] = Value::String(context.clone());
                }
                value
            })
            .collect::<Vec<_>>();
        let count = candidates.len();
        let state = json!({ "query": query, "candidates": candidates });
        Ok(self
            .jev
            .score_each(state, &self.rubric, count)?
            .into_iter()
            .map(|answer| answer.map(|answer| answer.normalized()))
            .collect())
    }
}

/// Every block's score: its best part's, with which part that was.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scores {
    /// Per block: `(score, part index)` of its best-scoring part, `None` when no part was scored.
    pub best: Vec<Option<(f64, usize)>>,
    pub requests: usize,
    /// Requests that failed transiently; their blocks count as unscored.
    pub failed_requests: usize,
}

/// Scores every part of every block, several to a request as the budget allows.
///
/// A configuration error or a stopped turn aborts the call. A transient failure leaves that
/// request's parts unscored and is counted; when every request fails, that is the failure.
pub fn score_blocks(
    scorer: Arc<dyn Scorer>,
    query: &str,
    blocks: &[Block],
    cancel: &CancelSignal,
) -> Result<Scores, DecisionError> {
    let items = blocks
        .iter()
        .enumerate()
        .flat_map(|(index, block)| {
            block.parts.iter().enumerate().map(move |(part, piece)| {
                let mut candidate = Candidate::new(block.label.clone(), piece.text.clone());
                candidate.context = block.context.clone();
                (index, part, candidate)
            })
        })
        .collect::<Vec<_>>();
    let mut scores = Scores {
        best: vec![None; blocks.len()],
        ..Scores::default()
    };
    if items.is_empty() {
        return Ok(scores);
    }
    let sizes = items
        .iter()
        .map(|(_, _, candidate)| {
            candidate.text.chars().count()
                + candidate
                    .context
                    .as_ref()
                    .map_or(0, |context| context.chars().count())
        })
        .collect::<Vec<_>>();
    let plan = batches(&sizes, request_budget(sizes.iter().sum()));
    let candidates = Arc::new(
        items
            .iter()
            .map(|(_, _, candidate)| candidate.clone())
            .collect::<Vec<_>>(),
    );
    let query = Arc::new(query.to_owned());
    let outcomes = {
        let candidates = Arc::clone(&candidates);
        map_bounded(
            Arc::new(plan.clone()),
            cancel,
            move |range: &std::ops::Range<usize>| {
                scorer.score_batch(&query, &candidates[range.clone()])
            },
        )?
    };
    scores.requests = outcomes.len();
    let mut first_error = None;
    for (range, outcome) in plan.into_iter().zip(outcomes) {
        match outcome {
            Ok(answers) => {
                for (offset, answer) in answers.into_iter().enumerate() {
                    let Some(score) = answer else { continue };
                    let Some((block, part, _)) = items.get(range.start + offset) else {
                        continue;
                    };
                    let score = super::round3(score);
                    let best = &mut scores.best[*block];
                    if best.is_none_or(|(current, _)| score > current) {
                        *best = Some((score, *part));
                    }
                }
            }
            Err(error @ (DecisionError::Config(_) | DecisionError::Cancelled)) => return Err(error),
            Err(error @ DecisionError::Transient(_)) => {
                scores.failed_requests += 1;
                first_error.get_or_insert(error);
            }
        }
    }
    if scores.best.iter().all(Option::is_none) {
        // Nothing at all came back: that is one failure to report, not a result with no hits.
        return Err(first_error.unwrap_or_else(|| {
            DecisionError::Transient("The decision model returned no scores".into())
        }));
    }
    Ok(scores)
}

/// A block that cleared the threshold.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    /// Index into the blocks the call scored.
    pub block: usize,
    pub label: String,
    pub score: f64,
    /// For a block scored in parts, the positions of the part that scored best.
    pub part: Option<(usize, usize)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchReport {
    /// Highest score first.
    pub hits: Vec<Hit>,
    /// Blocks the call scored.
    pub blocks: usize,
    pub requests: usize,
    /// The best scores, for a "nothing cleared the threshold" answer that still tells the model
    /// how close the material came. Highest first, at most three.
    pub near_misses: Vec<(String, f64)>,
    pub failed_requests: usize,
    /// Blocks no request managed to score.
    pub unscored: usize,
}

/// Scores `blocks` and reports every one at or above `threshold`.
pub fn search(
    scorer: Arc<dyn Scorer>,
    query: &str,
    threshold: f64,
    blocks: &[Block],
    cancel: &CancelSignal,
) -> Result<SearchReport, DecisionError> {
    let scores = score_blocks(scorer, query, blocks, cancel)?;
    Ok(report(blocks, &scores, threshold))
}

/// The report for blocks already scored.
pub fn report(blocks: &[Block], scores: &Scores, threshold: f64) -> SearchReport {
    let mut ranked = scores
        .best
        .iter()
        .enumerate()
        .filter_map(|(index, best)| best.map(|(score, part)| (index, score, part)))
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| compare_scores(right.1, left.1).then(left.0.cmp(&right.0)));
    let hit = |&(index, score, part): &(usize, f64, usize)| {
        let block = &blocks[index];
        Hit {
            block: index,
            label: block.label.clone(),
            score,
            part: (block.parts.len() > 1).then(|| (block.parts[part].first, block.parts[part].last)),
        }
    };
    SearchReport {
        hits: ranked
            .iter()
            .filter(|(_, score, _)| *score >= threshold)
            .map(hit)
            .collect(),
        blocks: blocks.len(),
        requests: scores.requests,
        near_misses: ranked
            .iter()
            .take(3)
            .map(|&(index, score, _)| (blocks[index].label.clone(), score))
            .collect(),
        failed_requests: scores.failed_requests,
        unscored: scores.best.iter().filter(|best| best.is_none()).count(),
    }
}

/// `3 chunks of x` / `1 chunk of x`: the subject's first word is the unit being counted.
fn counted(count: usize, subject: &str) -> String {
    if count != 1 {
        return format!("{count} {subject}");
    }
    let (head, tail) = subject.split_once(' ').unwrap_or((subject, ""));
    let head = head
        .strip_suffix("ies")
        .map(|stem| format!("{stem}y"))
        .or_else(|| head.strip_suffix('s').map(str::to_owned))
        .unwrap_or_else(|| head.to_owned());
    if tail.is_empty() {
        format!("1 {head}")
    } else {
        format!("1 {head} {tail}")
    }
}

pub(crate) fn compare_scores(left: f64, right: f64) -> std::cmp::Ordering {
    left.partial_cmp(&right).unwrap_or(std::cmp::Ordering::Equal)
}

/// Runs `run` over `items` on [`WORKERS`] detached threads and returns the results in input
/// order. Returns `Cancelled` as soon as the signal is raised; workers notice the stop flag
/// before taking their next item and exit on their own.
pub(crate) fn map_bounded<T, R, F>(
    items: Arc<Vec<T>>,
    cancel: &CancelSignal,
    run: F,
) -> Result<Vec<R>, DecisionError>
where
    T: Send + Sync + 'static,
    R: Send + 'static,
    F: Fn(&T) -> R + Send + Sync + 'static,
{
    let total = items.len();
    if total == 0 {
        return Ok(Vec::new());
    }
    let run = Arc::new(run);
    let next = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = mpsc::channel();
    for _ in 0..WORKERS.min(total) {
        let items = Arc::clone(&items);
        let run = Arc::clone(&run);
        let next = Arc::clone(&next);
        let stop = Arc::clone(&stop);
        let sender = sender.clone();
        std::thread::spawn(move || loop {
            if stop.load(Ordering::Acquire) {
                return;
            }
            let index = next.fetch_add(1, Ordering::AcqRel);
            if index >= items.len() {
                return;
            }
            let result = run(&items[index]);
            if sender.send((index, result)).is_err() {
                return;
            }
        });
    }
    drop(sender);
    let mut slots: Vec<Option<R>> = (0..total).map(|_| None).collect();
    let mut received = 0;
    while received < total {
        if cancel.cancelled() {
            stop.store(true, Ordering::Release);
            return Err(DecisionError::Cancelled);
        }
        match receiver.recv_timeout(CANCELLATION_PROBE_INTERVAL) {
            Ok((index, result)) => {
                slots[index] = Some(result);
                received += 1;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    slots
        .into_iter()
        .map(|slot| {
            slot.ok_or_else(|| {
                DecisionError::Transient("A decision-model worker terminated unexpectedly".into())
            })
        })
        .collect()
}

/// The summary lines every find tool opens with: how many blocks cleared, how many were scored in
/// how many requests, the best scores when nothing cleared, and what went unscored. `subject`
/// names the blocks as a plural noun phrase whose first word is the countable unit ("blocks of
/// src/app.ts", "entries under src", "page elements") — that word loses its plural when exactly
/// one was scored.
pub fn render_summary(report: &SearchReport, query: &str, threshold: f64, subject: &str) -> String {
    let mut lines = Vec::new();
    if report.hits.is_empty() {
        lines.push(format!(
            "No {subject} scored at or above {threshold:.3} for query {query:?} ({} scored, {}).",
            report.blocks,
            counted(report.requests, "requests")
        ));
        if !report.near_misses.is_empty() {
            let best = report
                .near_misses
                .iter()
                .map(|(label, score)| format!("{label} ({score:.3})"))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!(
                "Highest scores: {best}. Lower the threshold to see them."
            ));
        }
    } else {
        lines.push(format!(
            "{} hit{} at or above {threshold:.3} for query {query:?} ({} scored, {}).",
            report.hits.len(),
            if report.hits.len() == 1 { "" } else { "s" },
            counted(report.blocks, subject),
            counted(report.requests, "requests")
        ));
    }
    if report.failed_requests > 0 || report.unscored > 0 {
        lines.push(format!(
            "{} failed and {} left unscored; retry if the hits look incomplete.",
            counted(report.failed_requests, "requests"),
            counted(report.unscored, subject)
        ));
    }
    lines.join("\n")
}

/// The summary, then every hit: its label and score, and — for a block `body` gives text for —
/// the whole block under it. A block that was scored in parts says which part matched best.
pub fn render_report(
    report: &SearchReport,
    query: &str,
    threshold: f64,
    subject: &str,
    body: impl Fn(&Hit) -> Option<String>,
) -> String {
    let mut lines = vec![render_summary(report, query, threshold, subject)];
    for hit in &report.hits {
        let part = hit
            .part
            .map(|(first, last)| {
                if first == last {
                    format!(", best match in line {first}")
                } else {
                    format!(", best match in lines {first}-{last}")
                }
            })
            .unwrap_or_default();
        match body(hit) {
            Some(text) => {
                lines.push(String::new());
                lines.push(format!("--- {} (score {:.3}{part}) ---", hit.label, hit.score));
                lines.push(text);
            }
            None => lines.push(format!("{} (score {:.3}{part})", hit.label, hit.score)),
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Scores by keyword: the fraction of query words the text contains. Records every call.
    struct KeywordScorer {
        calls: Mutex<Vec<String>>,
        delay: Duration,
    }

    impl KeywordScorer {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                delay: Duration::ZERO,
            })
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Scorer for KeywordScorer {
        fn score(&self, query: &str, candidate: &Candidate) -> Result<f64, DecisionError> {
            std::thread::sleep(self.delay);
            self.calls.lock().unwrap().push(candidate.text.clone());
            if candidate.text.contains("BOOM") {
                return Err(DecisionError::Transient("boom".into()));
            }
            let words = query.split_whitespace().collect::<Vec<_>>();
            let hits = words
                .iter()
                .filter(|word| candidate.text.contains(*word))
                .count();
            Ok(hits as f64 / words.len() as f64)
        }
    }

    fn block(label: &str, text: &str) -> Block {
        Block::whole(label, text)
    }

    /// Every block is scored on its own and comes back whole; the ones below the threshold do
    /// not, but the best of them are named when nothing clears.
    #[test]
    fn every_block_at_or_above_the_threshold_comes_back() {
        let scorer = KeywordScorer::new();
        let blocks = vec![
            block("A", "nothing here"),
            block("B", "alpha beta"),
            block("C", "alpha"),
        ];
        let report = search(scorer.clone(), "alpha beta", 0.5, &blocks, &CancelSignal::default())
            .unwrap();
        let hits = report
            .hits
            .iter()
            .map(|hit| (hit.label.as_str(), hit.score))
            .collect::<Vec<_>>();
        assert_eq!(hits, vec![("B", 1.0), ("C", 0.5)]);
        assert_eq!(report.blocks, 3);
        // Three small blocks share one request.
        assert_eq!(report.requests, 1);
        assert_eq!(scorer.calls().len(), 3);

        let none = search(scorer, "zeta", 0.5, &blocks, &CancelSignal::default()).unwrap();
        assert!(none.hits.is_empty());
        assert_eq!(none.near_misses.len(), 3);
    }

    /// A block scored in parts scores its best part, comes back once, and says which part.
    #[test]
    fn a_block_in_parts_scores_its_best_part() {
        let scorer = KeywordScorer::new();
        let blocks = vec![Block::in_parts(
            "src/a.rs lines 1-300",
            vec![
                ("nothing".to_owned(), 1, 100),
                ("alpha".to_owned(), 101, 200),
                ("alpha beta".to_owned(), 201, 300),
            ],
        )];
        let report = search(scorer, "alpha beta", 0.4, &blocks, &CancelSignal::default()).unwrap();
        assert_eq!(report.hits.len(), 1);
        assert_eq!(report.hits[0].score, 1.0);
        assert_eq!(report.hits[0].part, Some((201, 300)));
        let text = render_report(&report, "alpha beta", 0.4, "blocks of src/a.rs", |_| {
            Some("the whole block".to_owned())
        });
        assert!(
            text.contains("--- src/a.rs lines 1-300 (score 1.000, best match in lines 201-300) ---\nthe whole block"),
            "{text}"
        );
    }

    /// Size decides how many blocks share a request: large blocks take a request each.
    #[test]
    fn large_blocks_take_more_requests() {
        let scorer = KeywordScorer::new();
        let blocks = (0..4)
            .map(|index| block(&index.to_string(), &"x".repeat(1_400)))
            .collect::<Vec<_>>();
        let report = search(scorer, "alpha", 0.5, &blocks, &CancelSignal::default()).unwrap();
        assert_eq!(report.requests, 4);
    }

    #[test]
    fn empty_inputs_cost_nothing() {
        let report =
            search(KeywordScorer::new(), "alpha", 0.5, &[], &CancelSignal::default()).unwrap();
        assert_eq!(report, SearchReport::default());
    }

    /// One unscored block is counted, not fatal; every request failing is the failure itself.
    #[test]
    fn transient_failures_are_counted_not_fatal_unless_total() {
        /// Fails the whole request whenever one of its candidates says BOOM.
        struct Flaky;
        impl Scorer for Flaky {
            fn score(&self, _: &str, _: &Candidate) -> Result<f64, DecisionError> {
                Ok(1.0)
            }
            fn score_batch(
                &self,
                _: &str,
                candidates: &[Candidate],
            ) -> Result<Vec<Option<f64>>, DecisionError> {
                if candidates.iter().any(|candidate| candidate.text.contains("BOOM")) {
                    return Err(DecisionError::Transient("boom".into()));
                }
                Ok(vec![Some(1.0); candidates.len()])
            }
        }
        let blocks = vec![block("A", &"a".repeat(1_400)), block("B", &"BOOM".repeat(400))];
        let report = search(Arc::new(Flaky), "a", 0.5, &blocks, &CancelSignal::default()).unwrap();
        assert_eq!(report.failed_requests, 1);
        assert_eq!(report.unscored, 1);
        assert_eq!(report.hits.len(), 1);
        let summary = render_summary(&report, "a", 0.5, "blocks");
        assert!(summary.contains("1 request failed and 1 block left unscored"), "{summary}");

        let error = search(
            Arc::new(Flaky),
            "a",
            0.5,
            &[block("B", "BOOM")],
            &CancelSignal::default(),
        )
        .expect_err("total failure");
        assert_eq!(error, DecisionError::Transient("boom".into()));
    }

    #[test]
    fn a_configuration_error_aborts_the_call() {
        struct Broken;
        impl Scorer for Broken {
            fn score(&self, _: &str, _: &Candidate) -> Result<f64, DecisionError> {
                Err(DecisionError::Config("no key".into()))
            }
        }
        let error = search(
            Arc::new(Broken),
            "alpha",
            0.5,
            &[block("A", "alpha"), block("B", "alpha")],
            &CancelSignal::default(),
        )
        .expect_err("config error");
        assert_eq!(error, DecisionError::Config("no key".into()));
    }

    /// Stopping the turn returns promptly even while slow requests are in flight, and the
    /// workers do not go on to take the remaining batches.
    #[test]
    fn cancellation_returns_without_waiting_for_in_flight_requests() {
        let scorer = Arc::new(KeywordScorer {
            calls: Mutex::new(Vec::new()),
            delay: Duration::from_millis(400),
        });
        let flag = Arc::new(AtomicBool::new(false));
        let cancel = CancelSignal::from_flag(Arc::clone(&flag));
        let blocks = (0..WORKERS * 4)
            .map(|index| block(&index.to_string(), &"x".repeat(1_400)))
            .collect::<Vec<_>>();
        let flag_for_thread = Arc::clone(&flag);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            flag_for_thread.store(true, Ordering::Release);
        });
        let started = std::time::Instant::now();
        let error = search(
            scorer.clone() as Arc<dyn Scorer>,
            "alpha",
            0.5,
            &blocks,
            &cancel,
        )
        .expect_err("cancelled");
        assert_eq!(error, DecisionError::Cancelled);
        assert!(
            started.elapsed() < Duration::from_millis(1_500),
            "returned after {:?}",
            started.elapsed()
        );
        // Let the in-flight wave finish, then check nothing beyond it was taken.
        std::thread::sleep(Duration::from_millis(600));
        assert!(scorer.calls().len() <= WORKERS * 2, "{}", scorer.calls().len());
    }

    #[test]
    fn map_bounded_keeps_input_order_under_concurrency() {
        let items = Arc::new((0..40).collect::<Vec<u64>>());
        let results = map_bounded(items, &CancelSignal::default(), |value: &u64| {
            std::thread::sleep(Duration::from_millis((value % 5) * 3));
            value * 2
        })
        .unwrap();
        assert_eq!(results, (0..40).map(|value| value * 2).collect::<Vec<_>>());
    }

    #[test]
    fn the_report_is_rendered_for_the_model() {
        let report = SearchReport {
            hits: vec![
                Hit {
                    block: 0,
                    label: "src/auth.rs lines 10-12".into(),
                    score: 0.9,
                    part: None,
                },
                Hit {
                    block: 1,
                    label: "src/auth.rs lines 40-80".into(),
                    score: 0.7,
                    part: None,
                },
            ],
            blocks: 4,
            requests: 1,
            ..SearchReport::default()
        };
        let text = render_report(&report, "login handler", 0.6, "blocks of src/auth.rs", |hit| {
            Some(format!("body {}", hit.block))
        });
        assert!(text.starts_with("2 hits at or above 0.600 for query \"login handler\" (4 blocks of src/auth.rs scored, 1 request)."), "{text}");
        assert!(text.contains("--- src/auth.rs lines 10-12 (score 0.900) ---\nbody 0"), "{text}");

        let empty = SearchReport {
            blocks: 3,
            requests: 2,
            near_misses: vec![("lines 1-16".into(), 0.333)],
            ..SearchReport::default()
        };
        let text = render_report(&empty, "x", 0.5, "blocks", |_| None);
        assert!(text.starts_with("No blocks scored at or above 0.500 for query \"x\" (3 scored, 2 requests)."), "{text}");
        assert!(text.contains("Highest scores: lines 1-16 (0.333)"), "{text}");

        let paths = SearchReport {
            hits: vec![Hit {
                block: 0,
                label: "src/a.ts".into(),
                score: 1.0,
                part: None,
            }],
            blocks: 1,
            requests: 1,
            ..SearchReport::default()
        };
        let text = render_report(&paths, "x", 0.5, "entries under src", |_| None);
        assert!(text.starts_with("1 hit at or above 0.500 for query \"x\" (1 entry under src scored, 1 request)."), "{text}");
        assert!(text.ends_with("src/a.ts (score 1.000)"), "{text}");
    }
}
