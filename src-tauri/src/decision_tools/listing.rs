//! `find_files`: which entries of a directory listing a query describes.
//!
//! The executor resolves the directory under the usual scope rules and walks it (here for a
//! local workspace, through the remote `ls` machinery for another machine); this module cuts
//! the listing into groups of consecutive paths, scores the groups, and re-scores the paths of
//! every group that clears the threshold, so what comes back is a handful of paths with their
//! scores rather than a directory the conversation model has to read.

use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use walkdir::WalkDir;

use crate::cancel::CancelSignal;
use crate::decision_model::chunk::group_entries;
use crate::decision_model::jev::RELEVANCE_RUBRIC;
use crate::decision_model::search::{render_report, search, Candidate, Scorer, SearchReport};
use crate::decision_model::{parse_query, parse_threshold};
use crate::model::JsonObject;
use crate::path_guard::{canonical_workspace, existing_path_is_allowed, relative_display,
                        resolve_existing_with_scope};
use crate::security::ExecutionScope;
use crate::tool_executor::{optional_string, optional_u64, MAX_PATH_CHARS};

use super::{failure, jev_scorer};

/// Entries one call lists before it stops walking. Ten times `ls`'s own cap: `ls` is a listing
/// a person reads, while this one is only ever read by the scorer, which sees it in groups.
pub(crate) const MAX_FIND_FILES_ENTRIES: usize = 20_000;

/// The coarse cut of a listing: at most this many groups, of at least this many paths each.
/// Same shape as the line chunker — the group count is what is bounded, so a large tree grows
/// its groups instead of its request count.
const MAX_ENTRY_GROUPS: usize = 48;
const MIN_ENTRIES_PER_GROUP: usize = 8;

/// `depth` counts the levels below the target, exactly as `ls` counts them. The default is
/// deeper than `ls`'s because nobody reads this listing: a `find_files` that only saw the
/// immediate children would miss the file it was asked for.
const DEFAULT_DEPTH: u64 = 6;

/// `find_files` on this machine: the same walk `ls` performs, under the same scope filter.
pub(crate) fn find_files_local(
    workspace: &Path,
    input: &JsonObject,
    scope: &ExecutionScope,
    cancel: &CancelSignal,
) -> Result<String, String> {
    let path = optional_string(input, "path", ".", MAX_PATH_CHARS, false)?;
    let depth = parse_depth(input)?;
    check_scoring_arguments(input)?;
    let workspace = canonical_workspace(workspace)?;
    let root = resolve_existing_with_scope(&workspace, &path, scope)?;
    if !root.is_dir() {
        return Err(format!("find_files target is not a directory: {path}"));
    }
    let mut entries = Vec::new();
    for entry in WalkDir::new(&root)
        .follow_links(false)
        .min_depth(1)
        .max_depth(depth as usize + 1)
        .into_iter()
        .filter_entry(|entry| existing_path_is_allowed(scope, entry.path()))
    {
        let entry = entry.map_err(|error| {
            let reason = error
                .io_error()
                .map(ToString::to_string)
                .unwrap_or_else(|| "directory traversal failed".into());
            format!("Failed to list directory {path}: {reason}")
        })?;
        if entries.len() >= MAX_FIND_FILES_ENTRIES {
            break;
        }
        let mut display = relative_display(&workspace, entry.path())
            .to_string_lossy()
            .replace('\\', "/");
        if entry.file_type().is_dir() {
            display.push('/');
        }
        entries.push(display);
    }
    entries.sort_unstable();
    find_files(&path, entries, input, cancel)
}

/// `find_files` on another machine: the listing is walked there and only the paths come over.
pub(crate) fn find_files_remote(
    target: &crate::remote_files::RemoteWorkspace<'_>,
    input: &JsonObject,
    cancel: &CancelSignal,
) -> Result<String, String> {
    let path = optional_string(input, "path", ".", MAX_PATH_CHARS, false)?;
    let depth = parse_depth(input)?;
    check_scoring_arguments(input)?;
    let entries = crate::remote_files::list_entries(target, &path, depth)?;
    find_files(&path, entries, input, cancel)
}

fn parse_depth(input: &JsonObject) -> Result<u64, String> {
    let depth = optional_u64(input, "depth", DEFAULT_DEPTH)?;
    if depth > 8 {
        return Err("Recursive depth cannot exceed 8".into());
    }
    Ok(depth)
}

/// Both legs check the scoring arguments before they walk anything: a missing query should not
/// cost a directory walk here or a round trip to another machine.
fn check_scoring_arguments(input: &JsonObject) -> Result<(), String> {
    parse_query(input, "query")?;
    parse_threshold(input)?;
    Ok(())
}

/// Scores a listing that has already been collected, whichever machine produced it.
fn find_files(
    root: &str,
    entries: Vec<String>,
    input: &JsonObject,
    cancel: &CancelSignal,
) -> Result<String, String> {
    let query = parse_query(input, "query")?;
    let threshold = parse_threshold(input)?;
    if entries.is_empty() {
        return Ok(format!("{root} has no entries to score."));
    }
    let scorer = jev_scorer(RELEVANCE_RUBRIC)?;
    find_files_with(scorer, root, entries, &query, threshold, cancel)
}

/// The scoring pass, with the scorer passed in so the grouping and the rendering can be tested
/// without a key or a network.
fn find_files_with(
    scorer: Arc<dyn Scorer>,
    root: &str,
    entries: Vec<String>,
    query: &str,
    threshold: f64,
    cancel: &CancelSignal,
) -> Result<String, String> {
    let capped = entries.len() >= MAX_FIND_FILES_ENTRIES;
    let (coarse, by_label) = entry_groups(root, &entries);
    let report = search(
        scorer,
        query,
        threshold,
        coarse,
        |candidate| refine_entry_group(&entries, &by_label, candidate),
        cancel,
    )
    .map_err(failure)?;
    Ok(render_entry_report(&report, query, threshold, root, capped))
}

/// The coarse candidates: runs of consecutive paths, labelled by their position in the listing
/// so a group that is never refined is still addressable.
fn entry_groups(root: &str, entries: &[String]) -> (Vec<Candidate>, HashMap<String, Range<usize>>) {
    let groups = group_entries(entries.len(), MAX_ENTRY_GROUPS, MIN_ENTRIES_PER_GROUP);
    let mut by_label = HashMap::with_capacity(groups.len());
    let candidates = groups
        .into_iter()
        .map(|range| {
            let label = group_label(root, &range);
            by_label.insert(label.clone(), range.clone());
            Candidate::new(label, entries[range].join("\n"))
        })
        .collect();
    (candidates, by_label)
}

/// A winning group refined into one candidate per path. The path is both the label and the
/// text: for a listing there is nothing else to judge, and the label *is* the answer.
fn refine_entry_group(
    entries: &[String],
    by_label: &HashMap<String, Range<usize>>,
    candidate: &Candidate,
) -> Vec<Candidate> {
    let Some(range) = by_label.get(&candidate.label) else {
        return Vec::new();
    };
    entries[range.clone()]
        .iter()
        .map(|entry| Candidate::new(entry.clone(), entry.clone()))
        .collect()
}

fn group_label(root: &str, range: &Range<usize>) -> String {
    format!("entries {}-{} of {root}", range.start + 1, range.end)
}

/// `find_files`'s answer: `render_report`'s own summary, then one line per path that cleared
/// the threshold, and — for a group the refinement pass never split — the group's paths under
/// a header that says so.
fn render_entry_report(
    report: &SearchReport,
    query: &str,
    threshold: f64,
    root: &str,
    capped: bool,
) -> String {
    let subject = format!("groups of entries under {root}");
    let mut text = vec![report_summary(report, query, threshold, &subject)];
    if capped {
        text.push(format!(
            "Only the first {MAX_FIND_FILES_ENTRIES} entries under {root} were listed; there are \
             more. Narrow the search with path or depth."
        ));
    }
    for hit in &report.hits {
        if hit.refined {
            text.push(format!("{} (score {:.3})", hit.candidate.label, hit.score));
            continue;
        }
        text.push(String::new());
        text.push(format!(
            "--- {} (score {:.3}, whole group) ---",
            hit.candidate.label, hit.score
        ));
        for line in hit.candidate.text.lines() {
            text.push(format!("  {line}"));
        }
    }
    text.join("\n")
}

/// The report's summary lines — everything `render_report` prints that is not a hit — so a tool
/// that renders its own hits still says "no entry groups scored…" in exactly the same words.
fn report_summary(report: &SearchReport, query: &str, threshold: f64, subject: &str) -> String {
    let rendered = render_report(report, query, threshold, subject, false);
    let summary = rendered.lines().count().saturating_sub(report.hits.len());
    rendered
        .lines()
        .take(summary)
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision_model::DecisionError;
    use serde_json::json;
    use std::sync::Mutex;

    /// Scores a candidate by whether its text mentions the query's words, and records what it
    /// was asked — the same shape as `search.rs`'s own `KeywordScorer`.
    struct KeywordScorer {
        calls: Mutex<Vec<String>>,
    }

    impl KeywordScorer {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
            })
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Scorer for KeywordScorer {
        fn score(&self, query: &str, candidate: &Candidate) -> Result<f64, DecisionError> {
            self.calls.lock().unwrap().push(candidate.label.clone());
            let words = query.split_whitespace().collect::<Vec<_>>();
            let hits = words
                .iter()
                .filter(|word| candidate.text.contains(*word))
                .count();
            Ok(hits as f64 / words.len() as f64)
        }
    }

    fn input(pairs: &[(&str, serde_json::Value)]) -> JsonObject {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect()
    }

    fn listing(count: usize) -> Vec<String> {
        (0..count).map(|index| format!("src/f{index:02}.rs")).collect()
    }

    #[test]
    fn a_listing_is_grouped_in_order_and_labelled_by_position() {
        let entries = listing(20);
        let (coarse, by_label) = entry_groups("src", &entries);
        assert_eq!(coarse.len(), 3);
        assert_eq!(coarse[0].label, "entries 1-8 of src");
        assert_eq!(coarse[2].label, "entries 17-20 of src");
        assert_eq!(coarse[0].text.lines().count(), 8);
        assert_eq!(coarse[0].text.lines().next().unwrap(), "src/f00.rs");
        // Refinement turns a group into one candidate per path, label and text alike.
        let pieces = refine_entry_group(&entries, &by_label, &coarse[2]);
        assert_eq!(pieces.len(), 4);
        assert_eq!(pieces[0].label, "src/f16.rs");
        assert_eq!(pieces[0].text, "src/f16.rs");
        // A label this plan never produced refines to nothing.
        assert!(refine_entry_group(&entries, &by_label, &Candidate::new("x", "y")).is_empty());
    }

    /// The whole pass: only the group that mentions the query is refined, and only the paths
    /// that clear the threshold come back — each as its own line, with its score.
    #[test]
    fn only_the_paths_of_a_winning_group_are_scored_and_returned() {
        let scorer = KeywordScorer::new();
        let mut entries = listing(16);
        entries[9] = "src/auth/login_handler.rs".into();
        let text = find_files_with(
            scorer.clone(),
            ".",
            entries,
            "login",
            0.5,
            &CancelSignal::default(),
        )
        .unwrap();
        assert!(
            text.starts_with("1 hit at or above 0.500 for query \"login\" (2 groups of entries under . scored, 10 requests)."),
            "{text}"
        );
        assert!(text.contains("src/auth/login_handler.rs (score 1.000)"), "{text}");
        assert!(!text.contains("src/f00.rs"), "losers stay out: {text}");
        // The first group never mentions the query, so its paths are never scored one by one.
        assert!(
            !scorer.calls().contains(&"src/f00.rs".to_owned()),
            "{:?}",
            scorer.calls()
        );
    }

    /// A group that passes as a whole but whose paths individually do not is printed entire,
    /// indented under a header that says the group was never split.
    #[test]
    fn an_unrefined_group_prints_its_paths_under_a_whole_group_header() {
        let report = SearchReport {
            hits: vec![crate::decision_model::search::Hit {
                candidate: Candidate::new("entries 1-3 of src", "src/a.rs\nsrc/b.rs\nsrc/c.rs"),
                score: 0.75,
                refined: false,
            }],
            coarse_count: 2,
            requests: 5,
            ..SearchReport::default()
        };
        let text = render_entry_report(&report, "parser", 0.6, "src", false);
        assert!(
            text.starts_with("1 hit at or above 0.600 for query \"parser\" (2 groups of entries under src scored, 5 requests)."),
            "{text}"
        );
        assert!(text.contains("\n--- entries 1-3 of src (score 0.750, whole group) ---\n  src/a.rs\n  src/b.rs\n  src/c.rs"), "{text}");
        // A refined hit is the path itself, with no indentation and no header.
        let refined = SearchReport {
            hits: vec![crate::decision_model::search::Hit {
                candidate: Candidate::new("src/a.rs", "src/a.rs"),
                score: 0.9,
                refined: true,
            }],
            coarse_count: 1,
            requests: 2,
            ..SearchReport::default()
        };
        let text = render_entry_report(&refined, "parser", 0.6, "src", true);
        assert!(text.ends_with("src/a.rs (score 0.900)"), "{text}");
        assert!(text.contains("Only the first 20000 entries under src were listed"), "{text}");
    }

    /// An empty directory is an answer, not a failure — and it costs no request.
    #[test]
    fn an_empty_directory_is_reported_without_consulting_the_model() {
        let arguments = input(&[("query", json!("anything")), ("threshold", json!(0.5))]);
        let text = find_files("src", Vec::new(), &arguments, &CancelSignal::default()).unwrap();
        assert_eq!(text, "src has no entries to score.");
    }

    /// Argument errors surface before any listing is walked or any credential read.
    #[test]
    fn missing_or_impossible_arguments_fail_before_anything_is_listed() {
        let cancel = CancelSignal::default();
        let workspace = tempfile::tempdir().unwrap();
        let scope = ExecutionScope::Unrestricted;
        let error = find_files_local(
            workspace.path(),
            &input(&[("threshold", json!(0.5))]),
            &scope,
            &cancel,
        )
        .unwrap_err();
        assert!(error.contains("query"), "{error}");
        let error = find_files_local(
            workspace.path(),
            &input(&[("query", json!("q"))]),
            &scope,
            &cancel,
        )
        .unwrap_err();
        assert!(error.contains("threshold"), "{error}");
        let error = find_files_local(
            workspace.path(),
            &input(&[
                ("query", json!("q")),
                ("threshold", json!(0.5)),
                ("depth", json!(9)),
            ]),
            &scope,
            &cancel,
        )
        .unwrap_err();
        assert!(error.contains("depth"), "{error}");
    }
}
