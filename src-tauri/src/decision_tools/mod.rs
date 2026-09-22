//! The model-facing tools that run through the decision model.
//!
//! Each tool gathers bulk material the conversation model should not have to read — a file,
//! a directory listing, a command's output, a page's elements, a preview's logs — turns it into
//! candidates, and asks [`crate::decision_model`] to score or choose among them. Only what
//! clears the model's threshold, or the one element chosen, goes back as the tool result.
//!
//! The split with `tool_executor` is: the executor resolves *where* the material comes from
//! (which workspace, which file handle, which page session) under the usual scope rules, and
//! the functions here decide *what* of it the model sees.

pub mod files;
pub mod listing;
pub mod output;
pub mod preview;

use std::sync::Arc;

use crate::decision_model::jev::{Jev, ScoreRubric};
use crate::decision_model::search::{JevScorer, Scorer};
use crate::decision_model::DecisionError;

/// Catalog names of every tool that runs through the decision model. Mirrored by TS
/// `src/lib/taskTools.ts::DECISION_TOOL_NAMES`. They ship disabled in the seeded presets and
/// are listed one by one in the picker rather than folded into the preview row, because each
/// needs the TypeSafe key before it can do anything.
pub const DECISION_TOOL_NAMES: [&str; 10] = [
    "find_content",
    "find_files",
    "find_output",
    "bash_find_output",
    "powershell_find_output",
    "preview_find_element",
    "preview_find_logs",
    "preview_click_by_description",
    "preview_fill_by_description",
    "preview_inspect_by_description",
];

pub fn is_decision_tool_name(name: &str) -> bool {
    DECISION_TOOL_NAMES.contains(&name)
}

/// The scorer a tool call uses: the stored TypeSafe key behind a fixed rubric. A missing key
/// is reported in the words the model can relay to the user.
pub(crate) fn jev_scorer(rubric: ScoreRubric) -> Result<Arc<dyn Scorer>, String> {
    let jev = Jev::from_credentials().map_err(|error| error.message())?;
    Ok(Arc::new(JevScorer { jev, rubric }))
}

/// The client a choice tool uses.
pub(crate) fn jev_client() -> Result<Jev, String> {
    Jev::from_credentials().map_err(|error| error.message())
}

/// The tool-result text for a decision-model failure.
pub(crate) fn failure(error: DecisionError) -> String {
    error.message()
}
