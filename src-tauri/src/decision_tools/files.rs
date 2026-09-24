//! `find_content`: which parts of one file contain what a query asks for.
//!
//! The executor hands over the file's text (host or remote); this module cuts the requested
//! line range into the file's original blocks, scores every block, and renders every block at
//! or above the threshold whole, each line with its number in the file, so a hit can go straight
//! into `read` or `edit`.
//!
//! The blocks are the best the file offers: a language server's innermost document symbols when
//! one answers ([`super::symbols`]), a Markdown file's sections, and otherwise its paragraphs of
//! indented blocks.

use std::path::Path;

use crate::cancel::CancelSignal;
use crate::decision_model::chunk::Blocks;
use crate::decision_model::jev::RELEVANCE_RUBRIC;
use crate::decision_model::search::{render_report, search};
use crate::decision_model::{parse_query, parse_threshold};
use crate::model::{JsonObject, ResolvedLanguage};
use crate::security::ExecutionScope;
use crate::state::AppState;
use crate::tool_executor::optional_u64_value;

use super::{failure, jev_scorer, line_blocks, numbered};

/// The line range a call asks for: `start_line` (1-based, default 1) to `end_line`
/// (inclusive, default the end of the file). Unlike `read`, there is no line cap: scoring a
/// whole large file in one call is what this tool is for.
pub(crate) fn parse_line_range(input: &JsonObject) -> Result<(usize, Option<usize>), String> {
    let start_line = optional_u64_value(input, "start_line")?.unwrap_or(1);
    let end_line = optional_u64_value(input, "end_line")?;
    if start_line == 0 {
        return Err("start_line must begin at 1".into());
    }
    if let Some(end_line) = end_line {
        if end_line < start_line {
            return Err("end_line cannot be less than start_line".into());
        }
    }
    Ok((
        usize::try_from(start_line).unwrap_or(usize::MAX),
        end_line.map(|line| usize::try_from(line).unwrap_or(usize::MAX)),
    ))
}

/// `find_content` on this machine: the file read under the usual scope rules, and cut along
/// the symbols of the language server that claims it when one may be used.
pub(crate) fn find_content_local(
    workspace: &Path,
    input: &JsonObject,
    scope: &ExecutionScope,
    state: &AppState,
    language: ResolvedLanguage,
    cancel: &CancelSignal,
) -> Result<String, String> {
    let (display, content, path) =
        crate::tool_executor::read_text_for_scoring(workspace, input, scope)?;
    find_content(
        &display,
        &content,
        || {
            super::symbols::document_symbols(
                &state.lsp_servers,
                workspace,
                &path,
                &content,
                language,
            )
        },
        input,
        cancel,
    )
}

/// Scores the requested range of `content`, which came from `display_path` — every line of it,
/// in as many requests as its chunks take. `symbols` is asked for the file's document symbols
/// (1-based line spans) only once the call is known to score something.
pub(crate) fn find_content(
    display_path: &str,
    content: &str,
    symbols: impl FnOnce() -> Option<Vec<(usize, usize)>>,
    input: &JsonObject,
    cancel: &CancelSignal,
) -> Result<String, String> {
    let query = parse_query(input, "query")?;
    let threshold = parse_threshold(input)?;
    let (start_line, end_line) = parse_line_range(input)?;
    let lines = content.lines().collect::<Vec<_>>();
    if lines.is_empty() {
        return Ok(format!("{display_path} is empty; there is nothing to score."));
    }
    if start_line > lines.len() {
        return Err(format!(
            "start_line {start_line} is beyond the end of {display_path} ({} lines)",
            lines.len()
        ));
    }
    let end_line = end_line.map_or(lines.len(), |line| line.min(lines.len()));
    let scorer = jev_scorer(RELEVANCE_RUBRIC)?;
    let region = lines[start_line - 1..end_line].join("\n");
    let material = content_blocks(display_path, &region, start_line, symbols);
    let blocks = line_blocks(&material, display_path, start_line);
    let report = search(scorer, &query, threshold, &blocks, cancel).map_err(failure)?;
    Ok(render_report(
        &report,
        &query,
        threshold,
        &format!("blocks of {display_path}"),
        |hit| Some(numbered(&material, material.spans()[hit.block], start_line)),
    ))
}

/// How a region of a file is cut. Markdown by its sections, which no language server is
/// needed for; code by its symbols when a server reported them, their spans moved into the
/// region's own numbering and clipped to it; anything else by paragraphs of indented blocks.
fn content_blocks(
    display_path: &str,
    region: &str,
    start_line: usize,
    symbols: impl FnOnce() -> Option<Vec<(usize, usize)>>,
) -> Blocks {
    if is_markdown(display_path) {
        return Blocks::markdown(region);
    }
    let Some(spans) = symbols() else {
        return Blocks::text(region);
    };
    let count = region.lines().count();
    let offset = start_line - 1;
    let spans = spans
        .into_iter()
        .filter(|&(first, last)| last > offset && first <= offset + count && first <= last)
        .map(|(first, last)| {
            (
                first.saturating_sub(offset).max(1),
                (last - offset).min(count),
            )
        })
        .collect::<Vec<_>>();
    Blocks::symbols(region, &spans)
}

fn is_markdown(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "md" | "markdown" | "mdx"
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn input(pairs: &[(&str, serde_json::Value)]) -> JsonObject {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect()
    }

    #[test]
    fn the_line_range_defaults_to_the_whole_file_and_has_no_cap() {
        assert_eq!(parse_line_range(&input(&[])).unwrap(), (1, None));
        assert_eq!(
            parse_line_range(&input(&[("start_line", json!(40)), ("end_line", json!(9000))]))
                .unwrap(),
            (40, Some(9000))
        );
        assert!(parse_line_range(&input(&[("start_line", json!(0))])).is_err());
        assert!(parse_line_range(&input(&[("start_line", json!(5)), ("end_line", json!(4))]))
            .is_err());
    }

    /// The blocks follow the file: Markdown by section whatever a server says, code by the
    /// symbols it was given — moved into the region's numbering — and indentation otherwise;
    /// labels and line numbers are the file's own.
    #[test]
    fn the_region_is_cut_into_the_best_blocks_the_file_offers() {
        let code = "fn a() {\n    one();\n}\n\nfn b() {\n    two();\n}";
        let blocks = content_blocks("src/a.rs", code, 1, || None);
        let labels = line_blocks(&blocks, "src/a.rs", 1)
            .into_iter()
            .map(|block| block.label)
            .collect::<Vec<_>>();
        assert_eq!(labels, vec!["src/a.rs lines 1-3", "src/a.rs lines 5-7"]);

        // A region starting at line 11: the server's spans are in file lines, and one of them
        // began above the region.
        let asked = std::cell::Cell::new(false);
        let symbols = content_blocks("src/a.rs", code, 11, || {
            asked.set(true);
            Some(vec![(9, 13), (15, 17), (40, 50)])
        });
        assert!(asked.get());
        let labels = line_blocks(&symbols, "src/a.rs", 11)
            .into_iter()
            .map(|block| block.label)
            .collect::<Vec<_>>();
        assert_eq!(labels, vec!["src/a.rs lines 11-13", "src/a.rs lines 15-17"]);
        // The model reads the lines back numbered as the file numbers them.
        assert_eq!(
            numbered(&symbols, symbols.spans()[1], 11),
            "    15\tfn b() {\n    16\t    two();\n    17\t}"
        );

        // Markdown never asks a server.
        let markdown = content_blocks("docs/guide.MD", "# A\ntext\n# B\nmore", 1, || {
            panic!("a Markdown file needs no language server")
        });
        assert_eq!(markdown.spans(), &[(1, 2), (3, 4)]);
    }

    /// Argument errors surface before any credential is read or request made.
    #[test]
    fn missing_arguments_fail_before_the_decision_model_is_consulted() {
        let cancel = CancelSignal::default();
        let error = find_content("a.txt", "x", || None, &input(&[("threshold", json!(0.5))]), &cancel)
            .unwrap_err();
        assert!(error.contains("query"), "{error}");
        let error = find_content("a.txt", "x", || None, &input(&[("query", json!("q"))]), &cancel)
            .unwrap_err();
        assert!(error.contains("threshold"), "{error}");
        let error = find_content(
            "a.txt",
            "x\ny",
            || None,
            &input(&[
                ("query", json!("q")),
                ("threshold", json!(0.5)),
                ("start_line", json!(9)),
            ]),
            &cancel,
        )
        .unwrap_err();
        assert!(error.contains("beyond the end"), "{error}");
        let text = find_content(
            "a.txt",
            "",
            || None,
            &input(&[("query", json!("q")), ("threshold", json!(0.5))]),
            &cancel,
        )
        .unwrap();
        assert!(text.contains("is empty"), "{text}");
    }
}
