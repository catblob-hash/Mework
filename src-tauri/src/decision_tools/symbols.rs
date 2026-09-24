//! Document symbols for `find_content`: the seams a language server sees in a file, so a chunk
//! boundary falls between two functions rather than through one.
//!
//! Best effort, bounded, and never a way around an approval. A server the workspace's own
//! `lsp.json` names is used only when it is already running — starting one is what the `lsp`
//! tool's approval card is for — while a server from the user's `lsp.json` or a built-in preset is
//! started exactly as the `lsp` tool would start it, since that tool asks nothing for them either.
//! The answer is awaited for [`SYMBOLS_DEADLINE`]: a cold server that needs longer finishes
//! starting on its own for the next call, and this one falls back to cutting by indentation.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::lsp_config;
use crate::lsp_servers::{path_to_uri, LspRegistry, ServerHost};
use crate::model::ResolvedLanguage;

/// How long a scoring call waits for a language server's symbols before cutting without them.
const SYMBOLS_DEADLINE: Duration = Duration::from_secs(5);

/// Unit tests never launch a real language server: an executor test that scores a `.rs` file
/// would otherwise start whatever rust-analyzer the machine has installed, and its answer would
/// decide the test. A test that means to reach a real one sets this.
#[cfg(test)]
pub(crate) static REAL_SERVERS_IN_TESTS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The line spans (1-based, inclusive) of the document symbols the server that claims `path`
/// reports, or `None` when no server may be used, none answers in time, or it reports nothing.
pub(crate) fn document_symbols(
    registry: &LspRegistry,
    workspace: &Path,
    path: &Path,
    text: &str,
    language: ResolvedLanguage,
) -> Option<Vec<(usize, usize)>> {
    #[cfg(test)]
    if !REAL_SERVERS_IN_TESTS.load(std::sync::atomic::Ordering::Acquire) {
        return None;
    }
    let configs = lsp_config::servers_for_workspace(workspace, language);
    let (config, _) = LspRegistry::config_for_path(&configs, path)?;
    if lsp_config::is_project_config(config)
        && !registry.is_running(&ServerHost::Local, config, workspace)
    {
        return None;
    }
    let (sender, receiver) = mpsc::channel();
    let registry = registry.clone();
    let workspace = workspace.to_path_buf();
    let path: PathBuf = path.to_path_buf();
    let text = text.to_owned();
    std::thread::spawn(move || {
        let answer = (|| {
            let (connection, language_id, _root) =
                registry.connection_for(&ServerHost::Local, &configs, &workspace, &path)?;
            if !connection.has_document(&path) {
                connection.sync_document(&path, &language_id, &text)?;
            }
            connection.request_with_retry(
                "textDocument/documentSymbol",
                json!({ "textDocument": { "uri": path_to_uri(&path) } }),
            )
        })();
        let _ = sender.send(answer);
    });
    let answer = receiver.recv_timeout(SYMBOLS_DEADLINE).ok()?.ok()?;
    let spans = symbol_spans(&answer);
    (!spans.is_empty()).then_some(spans)
}

/// Every symbol's line span out of a `textDocument/documentSymbol` answer, in either of its
/// shapes: nested `DocumentSymbol`s or flat `SymbolInformation`s. An LSP range ends *before* its
/// end position, so a range that ends at the start of a line does not cover that line.
pub(crate) fn symbol_spans(answer: &Value) -> Vec<(usize, usize)> {
    fn line_span(range: &Value) -> Option<(usize, usize)> {
        let start = range.pointer("/start/line")?.as_u64()? as usize;
        let end = range.pointer("/end/line")?.as_u64()? as usize;
        let end_character = range.pointer("/end/character")?.as_u64()?;
        let last = if end_character == 0 && end > start {
            end
        } else {
            end + 1
        };
        Some((start + 1, last.max(start + 1)))
    }
    fn walk(symbols: &[Value], spans: &mut Vec<(usize, usize)>) {
        for symbol in symbols {
            let range = symbol
                .get("range")
                .or_else(|| symbol.pointer("/location/range"));
            if let Some(span) = range.and_then(line_span) {
                spans.push(span);
            }
            if let Some(children) = symbol.get("children").and_then(Value::as_array) {
                walk(children, spans);
            }
        }
    }
    let mut spans = Vec::new();
    if let Some(symbols) = answer.as_array() {
        walk(symbols, &mut spans);
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_answer_shapes_give_one_based_line_spans() {
        let nested = json!([{
            "name": "Thing",
            "range": {"start": {"line": 8, "character": 0}, "end": {"line": 11, "character": 1}},
            "children": [{
                "name": "one",
                "range": {"start": {"line": 9, "character": 4}, "end": {"line": 9, "character": 20}}
            }]
        }]);
        assert_eq!(symbol_spans(&nested), vec![(9, 12), (10, 10)]);

        let flat = json!([{
            "name": "add",
            "location": {
                "uri": "file:///a.rs",
                "range": {"start": {"line": 4, "character": 0}, "end": {"line": 7, "character": 0}}
            }
        }]);
        assert_eq!(symbol_spans(&flat), vec![(5, 7)], "a range ending at column 0 stops a line short");
        assert!(symbol_spans(&json!(null)).is_empty());
        assert!(symbol_spans(&json!([{"name": "broken"}])).is_empty());
    }

    /// The whole path against a real rust-analyzer, when the machine has one on PATH:
    /// `cargo test --lib -- --ignored real_rust_analyzer`.
    #[test]
    #[ignore = "starts the rust-analyzer installed on this machine"]
    fn real_rust_analyzer_reports_the_function_spans() {
        if crate::environment_tools::resolve_on_path("rust-analyzer").is_none() {
            eprintln!("rust-analyzer is not on PATH; nothing to check");
            return;
        }
        REAL_SERVERS_IN_TESTS.store(true, std::sync::atomic::Ordering::Release);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("lib.rs");
        let text = "/// One.\nfn one() {\n    two();\n}\n\nfn two() {}\n";
        std::fs::write(&path, text).unwrap();
        let registry = LspRegistry::default();
        let mut spans = None;
        // A cold server may miss the first deadline; the second call finds it running.
        for _ in 0..6 {
            spans = document_symbols(
                &registry,
                directory.path(),
                &path,
                text,
                ResolvedLanguage::EnUs,
            );
            if spans.is_some() {
                break;
            }
        }
        registry.stop_all();
        let spans = spans.expect("rust-analyzer answered");
        assert!(spans.iter().any(|&(first, last)| first <= 2 && last == 4), "{spans:?}");
        assert!(spans.contains(&(6, 6)), "{spans:?}");
    }
}
