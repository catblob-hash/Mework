//! `find_content`: which parts of one file contain what a query asks for.
//!
//! The executor hands over the file's text (host or remote); this module cuts the requested
//! line range into chunks, scores them, refines the winners, and renders the hits with the
//! file's own line numbers so a hit can go straight into `read`.

use std::collections::HashMap;

use crate::cancel::CancelSignal;
use crate::decision_model::chunk::{chunk_lines_from, refine_chunk, Chunk};
use crate::decision_model::jev::RELEVANCE_RUBRIC;
use crate::decision_model::search::{render_report, search, Candidate};
use crate::decision_model::{parse_query, parse_threshold};
use crate::model::JsonObject;
use crate::tool_executor::optional_u64_value;

use super::{failure, jev_scorer};

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

/// Builds the coarse candidates and the refinement map for a chunk plan. Labels carry the
/// source and the line range, which is both how Jev sees `candidate.source` and how the hit
/// is addressed in the output.
pub(crate) fn chunk_candidates(
    source: &str,
    chunks: &[Chunk],
) -> (Vec<Candidate>, HashMap<String, Chunk>) {
    let mut by_label = HashMap::with_capacity(chunks.len());
    let candidates = chunks
        .iter()
        .map(|chunk| {
            let label = format!("{source} {}", chunk.label());
            by_label.insert(label.clone(), chunk.clone());
            Candidate::new(label, chunk.text.clone())
        })
        .collect();
    (candidates, by_label)
}

/// The refinement step for line chunks: a winner is cut into its pieces, each labelled with
/// its own line range.
pub(crate) fn refine_line_chunk(
    source: &str,
    by_label: &HashMap<String, Chunk>,
    candidate: &Candidate,
) -> Vec<Candidate> {
    let Some(chunk) = by_label.get(&candidate.label) else {
        return Vec::new();
    };
    refine_chunk(chunk)
        .into_iter()
        .map(|piece| Candidate::new(format!("{source} {}", piece.label()), piece.text))
        .collect()
}

/// Scores the requested range of `content`, which came from `display_path`.
pub(crate) fn find_content(
    display_path: &str,
    content: &str,
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
    let region = lines[start_line - 1..end_line].join("\n");
    let plan = chunk_lines_from(&region, start_line);
    let scorer = jev_scorer(RELEVANCE_RUBRIC)?;
    let (coarse, by_label) = chunk_candidates(display_path, &plan.chunks);
    let report = search(
        scorer,
        &query,
        threshold,
        coarse,
        |candidate| refine_line_chunk(display_path, &by_label, candidate),
        cancel,
    )
    .map_err(failure)?;
    let mut text = render_report(
        &report,
        &query,
        threshold,
        &format!("chunks of {display_path}"),
        true,
    );
    if plan.covered_lines < plan.total_lines {
        text.push_str(&format!(
            "\n\nOnly lines {start_line}-{} of the requested {start_line}-{end_line} were scored; \
             call again with start_line {} to continue.",
            start_line + plan.covered_lines - 1,
            start_line + plan.covered_lines
        ));
    }
    Ok(text)
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

    #[test]
    fn chunk_candidates_are_labelled_with_source_and_range_and_refine_by_label() {
        let content = (1..=40)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let plan = chunk_lines_from(&content, 1);
        let (coarse, by_label) = chunk_candidates("src/a.ts", &plan.chunks);
        assert_eq!(coarse[0].label, "src/a.ts lines 1-16");
        assert_eq!(coarse[0].text.lines().count(), 16);
        let pieces = refine_line_chunk("src/a.ts", &by_label, &coarse[0]);
        assert_eq!(pieces.len(), 6);
        assert_eq!(pieces[0].label, "src/a.ts lines 1-3");
        assert_eq!(pieces[5].label, "src/a.ts line 16");
        // An unknown label — not one this plan produced — refines to nothing.
        assert!(refine_line_chunk("src/a.ts", &by_label, &Candidate::new("x", "y")).is_empty());
    }

    /// Argument errors surface before any credential is read or request made.
    #[test]
    fn missing_arguments_fail_before_the_decision_model_is_consulted() {
        let cancel = CancelSignal::default();
        let error = find_content("a.txt", "x", &input(&[("threshold", json!(0.5))]), &cancel)
            .unwrap_err();
        assert!(error.contains("query"), "{error}");
        let error = find_content("a.txt", "x", &input(&[("query", json!("q"))]), &cancel)
            .unwrap_err();
        assert!(error.contains("threshold"), "{error}");
        let error = find_content(
            "a.txt",
            "x\ny",
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
            &input(&[("query", json!("q")), ("threshold", json!(0.5))]),
            &cancel,
        )
        .unwrap();
        assert!(text.contains("is empty"), "{text}");
    }
}
