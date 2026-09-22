//! The two-pass search every find tool runs.
//!
//! Pass one scores coarse candidates — chunks of lines, groups of paths, runs of page elements
//! — and keeps the ones at or above the threshold. Pass two refines each winner into its
//! pieces and scores those, so what goes back to the model is a small, precisely addressed
//! hit rather than a whole chunk. A winner whose pieces all fall below the threshold is kept
//! as it was: the chunk as a whole passed, and returning nothing for it would hide a hit.
//!
//! Requests run on a bounded pool of worker threads that are detached rather than joined, so
//! stopping the turn returns at once while any in-flight request finishes on its own.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::cancel::CancelSignal;

use super::jev::{Jev, ScoreRubric};
use super::{round3, DecisionError};

/// Concurrent requests in flight. TypeSafe allows 1,200 requests a minute; this keeps one
/// tool call well inside that while still finishing a coarse pass in a few seconds.
pub const WORKERS: usize = 8;
/// Stage-two requests one tool call may spend. Winners past the budget are returned coarse.
pub const REFINE_BUDGET: usize = 96;
/// How often the receive loop looks at the cancellation signal while requests are in flight.
const CANCELLATION_PROBE_INTERVAL: Duration = Duration::from_millis(100);

/// One thing to score. `text` is what the model judges; `label` is how a hit is addressed in
/// the tool output (`lines 12-30`, a path, `[42] button`); `context` is optional surrounding
/// material the rubric may refer to.
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

    pub fn with_context(mut self, context: impl Into<String>) -> Self {
        self.context = Some(context.into());
        self
    }
}

/// A candidate that cleared the threshold.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub candidate: Candidate,
    pub score: f64,
    /// False when the hit is a coarse chunk returned whole — its pieces did not clear the
    /// threshold on their own, or the refinement budget ran out first.
    pub refined: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchReport {
    /// Highest score first.
    pub hits: Vec<Hit>,
    /// Coarse candidates scored in pass one.
    pub coarse_count: usize,
    /// Requests actually made, both passes.
    pub requests: usize,
    /// The best coarse scores, for a "nothing cleared the threshold" answer that still tells
    /// the model how close the material came. Highest first, at most three.
    pub near_misses: Vec<(String, f64)>,
    /// Requests that failed transiently and were skipped; their candidates count as unscored.
    pub failed_requests: usize,
    pub refine_budget_exhausted: bool,
}

/// Something that turns a query and a candidate into a `0..=1` score.
pub trait Scorer: Send + Sync {
    fn score(&self, query: &str, candidate: &Candidate) -> Result<f64, DecisionError>;
}

/// The production scorer: one Jev `score` question per candidate against a fixed rubric.
pub struct JevScorer {
    pub jev: Jev,
    pub rubric: ScoreRubric,
}

impl Scorer for JevScorer {
    fn score(&self, query: &str, candidate: &Candidate) -> Result<f64, DecisionError> {
        let mut state = json!({
            "query": query,
            "candidate": {
                "source": candidate.label,
                "text": candidate.text,
            },
        });
        if let Some(context) = &candidate.context {
            state["candidate"]["context"] = Value::String(context.clone());
        }
        self.jev
            .score(state, &self.rubric)
            .map(|answer| answer.normalized())
    }
}

/// Scores `coarse` candidates, refines the winners with `refine`, and reports the hits.
pub fn search<F>(
    scorer: Arc<dyn Scorer>,
    query: &str,
    threshold: f64,
    coarse: Vec<Candidate>,
    refine: F,
    cancel: &CancelSignal,
) -> Result<SearchReport, DecisionError>
where
    F: Fn(&Candidate) -> Vec<Candidate>,
{
    let mut report = SearchReport {
        coarse_count: coarse.len(),
        ..SearchReport::default()
    };
    if coarse.is_empty() {
        return Ok(report);
    }
    let pass = score_all(&scorer, query, coarse, cancel, &mut report)?;
    if pass.failed == pass.scored.len() {
        // Nothing at all came back: that is one failure to report, not a result with no hits.
        return Err(pass.first_error.unwrap_or_else(|| {
            DecisionError::Transient("The decision model returned no scores".into())
        }));
    }
    let mut winners = pass
        .scored
        .into_iter()
        .filter_map(|(candidate, score)| score.map(|score| (candidate, score)))
        .collect::<Vec<_>>();
    winners.sort_by(|left, right| compare_scores(right.1, left.1));
    report.near_misses = winners
        .iter()
        .take(3)
        .map(|(candidate, score)| (candidate.label.clone(), *score))
        .collect();
    winners.retain(|(_, score)| *score >= threshold);

    // Refinement: collect every winner's pieces up to the budget, score them in one bounded
    // pass, then hand each winner either its passing pieces or itself.
    let mut pieces = Vec::new();
    let mut plans = Vec::new();
    let mut budget = REFINE_BUDGET;
    for (winner, score) in winners {
        let children = refine(&winner);
        if children.is_empty() {
            plans.push((winner, score, None));
            continue;
        }
        if children.len() > budget {
            report.refine_budget_exhausted = true;
            plans.push((winner, score, None));
            continue;
        }
        budget -= children.len();
        let range = pieces.len()..pieces.len() + children.len();
        pieces.extend(children);
        plans.push((winner, score, Some(range)));
    }
    let scored_pieces = if pieces.is_empty() {
        Vec::new()
    } else {
        // Refinement failures are not fatal: an unscored piece simply does not pass, and the
        // winner it belongs to is returned whole.
        score_all(&scorer, query, pieces, cancel, &mut report)?.scored
    };
    for (winner, score, range) in plans {
        let Some(range) = range else {
            report.hits.push(Hit {
                candidate: winner,
                score,
                refined: false,
            });
            continue;
        };
        let passing = scored_pieces[range]
            .iter()
            .filter_map(|(piece, piece_score)| {
                piece_score
                    .filter(|piece_score| *piece_score >= threshold)
                    .map(|piece_score| Hit {
                        candidate: piece.clone(),
                        score: piece_score,
                        refined: true,
                    })
            })
            .collect::<Vec<_>>();
        if passing.is_empty() {
            report.hits.push(Hit {
                candidate: winner,
                score,
                refined: false,
            });
        } else {
            report.hits.extend(passing);
        }
    }
    report
        .hits
        .sort_by(|left, right| compare_scores(right.score, left.score));
    Ok(report)
}

/// `3 chunks of x` / `1 chunk of x`: the subject's first word is the unit being counted.
fn counted(count: usize, subject: &str) -> String {
    if count != 1 {
        return format!("{count} {subject}");
    }
    let (head, tail) = subject.split_once(' ').unwrap_or((subject, ""));
    let head = head.strip_suffix('s').unwrap_or(head);
    if tail.is_empty() {
        format!("1 {head}")
    } else {
        format!("1 {head} {tail}")
    }
}

fn compare_scores(left: f64, right: f64) -> std::cmp::Ordering {
    left.partial_cmp(&right).unwrap_or(std::cmp::Ordering::Equal)
}

struct Pass {
    scored: Vec<(Candidate, Option<f64>)>,
    /// Transient failures in this pass alone.
    failed: usize,
    first_error: Option<DecisionError>,
}

/// Scores every candidate on the worker pool. A configuration error aborts the whole pass —
/// it will not fix itself on the next candidate. A transient failure leaves that candidate
/// unscored (`None`) and is counted, so the tool can say the pass was incomplete.
fn score_all(
    scorer: &Arc<dyn Scorer>,
    query: &str,
    candidates: Vec<Candidate>,
    cancel: &CancelSignal,
    report: &mut SearchReport,
) -> Result<Pass, DecisionError> {
    let query = Arc::new(query.to_owned());
    let candidates = Arc::new(candidates);
    let scorer = Arc::clone(scorer);
    let outcomes = map_bounded(
        Arc::clone(&candidates),
        cancel,
        move |candidate: &Candidate| scorer.score(&query, candidate),
    )?;
    report.requests += outcomes.len();
    let mut pass = Pass {
        scored: Vec::with_capacity(outcomes.len()),
        failed: 0,
        first_error: None,
    };
    for (candidate, outcome) in Arc::try_unwrap(candidates)
        .unwrap_or_else(|shared| (*shared).clone())
        .into_iter()
        .zip(outcomes)
    {
        match outcome {
            Ok(score) => pass.scored.push((candidate, Some(round3(score)))),
            Err(error @ DecisionError::Config(_)) | Err(error @ DecisionError::Cancelled) => {
                return Err(error)
            }
            Err(error @ DecisionError::Transient(_)) => {
                report.failed_requests += 1;
                pass.failed += 1;
                pass.first_error.get_or_insert(error);
                pass.scored.push((candidate, None));
            }
        }
    }
    Ok(pass)
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

/// Renders a report the way every find tool shows it: a one-line summary, then each hit as a
/// labelled block. `subject` names what was scored as a plural noun phrase whose first word is
/// the countable unit ("chunks of src/app.ts", "groups of entries under src", "runs of page
/// elements") — that word loses its plural when exactly one was scored. `show_text` is false
/// when the label *is* the content (a path).
pub fn render_report(
    report: &SearchReport,
    query: &str,
    threshold: f64,
    subject: &str,
    show_text: bool,
) -> String {
    let mut lines = Vec::new();
    if report.hits.is_empty() {
        lines.push(format!(
            "No {subject} scored at or above {threshold:.3} for query {query:?} ({} scored, {}).",
            report.coarse_count,
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
            counted(report.coarse_count, subject),
            counted(report.requests, "requests")
        ));
    }
    if report.failed_requests > 0 {
        lines.push(format!(
            "{} request{} failed and left their candidates unscored; retry if the hits look incomplete.",
            report.failed_requests,
            if report.failed_requests == 1 { "" } else { "s" }
        ));
    }
    if report.refine_budget_exhausted {
        lines.push(
            "Some hits are returned as whole chunks because the refinement budget ran out; \
             narrow the input or raise the threshold for finer hits."
                .to_owned(),
        );
    }
    for hit in &report.hits {
        let coarse = if hit.refined { "" } else { ", whole chunk" };
        if show_text {
            lines.push(String::new());
            lines.push(format!(
                "--- {} (score {:.3}{coarse}) ---",
                hit.candidate.label, hit.score
            ));
            lines.push(hit.candidate.text.clone());
        } else {
            lines.push(format!(
                "{} (score {:.3}{coarse})",
                hit.candidate.label, hit.score
            ));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
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
            self.calls.lock().unwrap().push(candidate.label.clone());
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

    fn chunk(label: &str, text: &str) -> Candidate {
        Candidate::new(label, text)
    }

    /// Refine by splitting on `|`.
    fn split_pipes(candidate: &Candidate) -> Vec<Candidate> {
        if !candidate.text.contains('|') {
            return Vec::new();
        }
        candidate
            .text
            .split('|')
            .enumerate()
            .map(|(index, part)| chunk(&format!("{}.{index}", candidate.label), part))
            .collect()
    }

    #[test]
    fn winners_are_refined_and_only_their_passing_pieces_come_back() {
        let scorer = KeywordScorer::new();
        let coarse = vec![
            chunk("A", "nothing|here|at all"),
            chunk("B", "alpha beta|gamma|alpha beta delta"),
            chunk("C", "alpha|zzz"),
        ];
        let report = search(
            scorer.clone(),
            "alpha beta",
            0.6,
            coarse,
            split_pipes,
            &CancelSignal::default(),
        )
        .unwrap();
        assert_eq!(report.coarse_count, 3);
        // Pass one: A=0, B=1, C=0.5 → only B wins; pass two scores B's three pieces.
        assert_eq!(report.requests, 6);
        let labels = report
            .hits
            .iter()
            .map(|hit| (hit.candidate.label.as_str(), hit.score, hit.refined))
            .collect::<Vec<_>>();
        assert_eq!(labels, vec![("B.0", 1.0, true), ("B.2", 1.0, true)]);
        assert_eq!(report.near_misses[0], ("B".to_owned(), 1.0));
        assert_eq!(report.near_misses[1], ("C".to_owned(), 0.5));
        assert!(!scorer.calls().contains(&"A.0".to_owned()), "losers are not refined");
    }

    /// A chunk that passes as a whole but whose pieces individually do not is still a hit.
    #[test]
    fn a_winner_whose_pieces_all_fail_is_kept_whole() {
        let scorer = KeywordScorer::new();
        let coarse = vec![chunk("A", "alpha|beta")];
        let report = search(
            scorer,
            "alpha beta",
            0.8,
            coarse,
            split_pipes,
            &CancelSignal::default(),
        )
        .unwrap();
        assert_eq!(report.hits.len(), 1);
        assert_eq!(report.hits[0].candidate.label, "A");
        assert!(!report.hits[0].refined);
        assert_eq!(report.hits[0].score, 1.0);
    }

    #[test]
    fn unrefinable_winners_and_empty_inputs_are_handled() {
        let scorer = KeywordScorer::new();
        let report = search(
            scorer.clone(),
            "alpha",
            0.5,
            vec![chunk("A", "alpha")],
            split_pipes,
            &CancelSignal::default(),
        )
        .unwrap();
        assert_eq!(report.hits.len(), 1);
        assert!(!report.hits[0].refined);
        assert_eq!(report.requests, 1);
        let empty = search(
            scorer,
            "alpha",
            0.5,
            Vec::new(),
            split_pipes,
            &CancelSignal::default(),
        )
        .unwrap();
        assert_eq!(empty, SearchReport::default());
    }

    #[test]
    fn the_refinement_budget_returns_late_winners_whole() {
        let scorer = KeywordScorer::new();
        let wide = (0..REFINE_BUDGET + 1)
            .map(|_| "alpha")
            .collect::<Vec<_>>()
            .join("|");
        let coarse = vec![chunk("W", &wide), chunk("N", "alpha|alpha")];
        let report = search(
            scorer,
            "alpha",
            0.5,
            coarse,
            split_pipes,
            &CancelSignal::default(),
        )
        .unwrap();
        assert!(report.refine_budget_exhausted);
        // W's pieces exceed the budget, so W is whole; N still gets refined within the budget.
        let whole = report
            .hits
            .iter()
            .find(|hit| hit.candidate.label == "W")
            .unwrap();
        assert!(!whole.refined);
        assert!(report.hits.iter().any(|hit| hit.candidate.label == "N.0" && hit.refined));
    }

    /// One transient failure skips a candidate; all of them failing is the failure itself.
    #[test]
    fn transient_failures_are_counted_not_fatal_unless_total() {
        let scorer = KeywordScorer::new();
        let report = search(
            scorer.clone(),
            "alpha",
            0.5,
            vec![chunk("A", "alpha"), chunk("B", "BOOM")],
            split_pipes,
            &CancelSignal::default(),
        )
        .unwrap();
        assert_eq!(report.failed_requests, 1);
        assert_eq!(report.hits.len(), 1);
        let error = search(
            scorer,
            "alpha",
            0.5,
            vec![chunk("B", "BOOM")],
            split_pipes,
            &CancelSignal::default(),
        )
        .expect_err("total failure");
        assert_eq!(error, DecisionError::Transient("boom".into()));
    }

    #[test]
    fn a_configuration_error_aborts_the_pass() {
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
            vec![chunk("A", "alpha"), chunk("B", "alpha")],
            split_pipes,
            &CancelSignal::default(),
        )
        .expect_err("config error");
        assert_eq!(error, DecisionError::Config("no key".into()));
    }

    /// Stopping the turn returns promptly even while slow requests are in flight, and the
    /// workers do not go on to take the remaining items.
    #[test]
    fn cancellation_returns_without_waiting_for_in_flight_requests() {
        let scorer = Arc::new(KeywordScorer {
            calls: Mutex::new(Vec::new()),
            delay: Duration::from_millis(400),
        });
        let flag = Arc::new(AtomicBool::new(false));
        let cancel = CancelSignal::from_flag(Arc::clone(&flag));
        let coarse = (0..WORKERS * 4)
            .map(|index| chunk(&index.to_string(), "alpha"))
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
            coarse,
            split_pipes,
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
        assert!(scorer.calls().len() <= WORKERS * 2, "{:?}", scorer.calls());
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
                    candidate: chunk("lines 10-12", "fn login() {}"),
                    score: 0.9,
                    refined: true,
                },
                Hit {
                    candidate: chunk("lines 40-80", "big"),
                    score: 0.7,
                    refined: false,
                },
            ],
            coarse_count: 4,
            requests: 10,
            near_misses: vec![],
            failed_requests: 1,
            refine_budget_exhausted: false,
        };
        let text = render_report(&report, "login handler", 0.6, "chunks of src/auth.rs", true);
        assert!(text.starts_with("2 hits at or above 0.600 for query \"login handler\" (4 chunks of src/auth.rs scored, 10 requests)."), "{text}");
        assert!(text.contains("1 request failed"), "{text}");
        assert!(text.contains("--- lines 10-12 (score 0.900) ---\nfn login() {}"), "{text}");
        assert!(text.contains("--- lines 40-80 (score 0.700, whole chunk) ---"), "{text}");

        let empty = SearchReport {
            coarse_count: 3,
            requests: 3,
            near_misses: vec![("lines 1-16".into(), 0.333)],
            ..SearchReport::default()
        };
        let text = render_report(&empty, "x", 0.5, "chunks", true);
        assert!(text.starts_with("No chunks scored at or above 0.500"), "{text}");
        assert!(text.contains("Highest scores: lines 1-16 (0.333)"), "{text}");

        let paths = SearchReport {
            hits: vec![Hit {
                candidate: chunk("src/a.ts", "src/a.ts"),
                score: 1.0,
                refined: true,
            }],
            coarse_count: 1,
            requests: 1,
            ..SearchReport::default()
        };
        let text = render_report(&paths, "x", 0.5, "files", false);
        assert!(text.ends_with("src/a.ts (score 1.000)"), "{text}");
    }
}
