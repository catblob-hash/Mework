//! `find_files`: which entries of a directory listing a query describes.
//!
//! The executor resolves the directory under the usual scope rules and walks it (here for a
//! local workspace, through the remote `ls` machinery for another machine); this module scores
//! every path of the listing on its own — many to a request — so what comes back is a handful of
//! paths with their scores rather than a directory the conversation model has to read.

use std::path::Path;
use std::sync::Arc;

use walkdir::WalkDir;

use crate::cancel::CancelSignal;
use crate::decision_model::chunk::Blocks;
use crate::decision_model::jev::RELEVANCE_RUBRIC;
use crate::decision_model::search::{render_summary, search, Block, Scorer, SearchReport};
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

/// The scoring pass, with the scorer passed in so the scoring and the rendering can be tested
/// without a key or a network.
fn find_files_with(
    scorer: Arc<dyn Scorer>,
    root: &str,
    mut entries: Vec<String>,
    query: &str,
    threshold: f64,
    cancel: &CancelSignal,
) -> Result<String, String> {
    let capped = entries.len() >= MAX_FIND_FILES_ENTRIES;
    entries.sort_unstable();
    let blocks = entry_blocks(entries);
    let report = search(scorer, query, threshold, &blocks, cancel).map_err(failure)?;
    Ok(render_entry_report(&report, query, threshold, root, capped))
}

/// Every path a block of its own, labelled by itself: for a listing there is nothing else to
/// judge, and the path *is* the answer.
fn entry_blocks(entries: Vec<String>) -> Vec<Block> {
    let listing = Blocks::entries(entries);
    listing
        .spans()
        .iter()
        .map(|&(position, _)| {
            let path = listing.text_of(position, position);
            Block::whole(path.clone(), path)
        })
        .collect()
}

/// `find_files`'s answer: the summary, a note when the walk stopped short, and one line per
/// path that cleared the threshold.
fn render_entry_report(
    report: &SearchReport,
    query: &str,
    threshold: f64,
    root: &str,
    capped: bool,
) -> String {
    let mut text = vec![render_summary(
        report,
        query,
        threshold,
        &format!("entries under {root}"),
    )];
    if capped {
        text.push(format!(
            "Only the first {MAX_FIND_FILES_ENTRIES} entries under {root} were listed; there are \
             more. Narrow the search with path or depth."
        ));
    }
    text.extend(
        report
            .hits
            .iter()
            .map(|hit| format!("{} (score {:.3})", hit.label, hit.score)),
    );
    text.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision_model::search::Candidate;
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

    /// A tree: `src/auth/` with two files, `src/util/` with `count` more.
    fn listing(count: usize) -> Vec<String> {
        let mut entries = vec![
            "src/".to_owned(),
            "src/auth/".to_owned(),
            "src/auth/login_handler.rs".to_owned(),
            "src/auth/session.rs".to_owned(),
            "src/util/".to_owned(),
        ];
        entries.extend((0..count).map(|index| format!("src/util/f{index:02}.rs")));
        entries
    }

    /// Every path is a block of its own, label and text alike.
    #[test]
    fn every_path_is_a_block_labelled_by_itself() {
        let blocks = entry_blocks(listing(3));
        assert_eq!(blocks.len(), 8);
        assert_eq!(blocks[2].label, "src/auth/login_handler.rs");
        assert_eq!(blocks[2].parts[0].text, "src/auth/login_handler.rs");
    }

    /// The whole pass: every path is scored, many to a request, and only the ones that clear the
    /// threshold come back — each as its own line, with its score.
    #[test]
    fn only_the_paths_that_clear_the_threshold_are_returned() {
        let scorer = KeywordScorer::new();
        let text = find_files_with(
            scorer.clone(),
            ".",
            listing(30),
            "login",
            0.5,
            &CancelSignal::default(),
        )
        .unwrap();
        assert!(
            text.starts_with("1 hit at or above 0.500 for query \"login\" (35 entries under . scored, 2 requests)."),
            "{text}"
        );
        assert!(text.ends_with("\nsrc/auth/login_handler.rs (score 1.000)"), "{text}");
        assert!(!text.contains("src/util/f00.rs"), "losers stay out: {text}");
        assert_eq!(scorer.calls().len(), 35, "every path is scored");
    }

    /// The note on a listing the walk cut short sits under the summary, above the paths.
    #[test]
    fn a_capped_listing_says_so_above_the_hits() {
        let report = SearchReport {
            hits: vec![crate::decision_model::search::Hit {
                block: 0,
                label: "src/a.rs".into(),
                score: 0.9,
                part: None,
            }],
            blocks: 1,
            requests: 1,
            ..SearchReport::default()
        };
        let text = render_entry_report(&report, "parser", 0.6, "src", true);
        let lines = text.lines().collect::<Vec<_>>();
        assert!(lines[0].starts_with("1 hit at or above 0.600"), "{text}");
        assert!(lines[1].starts_with("Only the first 20000 entries under src were listed"), "{text}");
        assert_eq!(lines[2], "src/a.rs (score 0.900)");
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
