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
pub mod symbols;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::decision_model::chunk::Blocks;
use crate::decision_model::jev::{Jev, ScoreRubric};
use crate::decision_model::search::{Block, JevScorer, Scorer};
use crate::decision_model::DecisionError;
use crate::model::{ConversationSettings, DecisionParameterMode, JsonObject, RunModelRequest};

/// Catalog names of every tool that runs through the decision model. Mirrored by TS
/// `src/lib/taskTools.ts::DECISION_TOOL_NAMES`. They ship disabled in the seeded presets because
/// each needs the TypeSafe key before it can do anything.
pub const DECISION_TOOL_NAMES: [&str; 8] = [
    "find_content",
    "find_files",
    "find_output",
    "bash_find_output",
    "zsh_find_output",
    "sh_find_output",
    "powershell_find_output",
    "preview_find_logs",
];

pub fn is_decision_tool_name(name: &str) -> bool {
    DECISION_TOOL_NAMES.contains(&name)
}

/// The tools whose decision-model form is parameters of their own rather than a separate tool.
/// Mirrored by TS `src/lib/taskTools.ts::DECISION_PARAMETER_TOOL_NAMES`.
///
/// Each keeps its direct form — a selector, a level filter, the whole snapshot — and a
/// conversation decides per tool ([`ConversationSettings::decision_parameter_modes`]) whether the
/// decision parameters ([`decision_parameters`]) join it (`Augment`) or take its place
/// (`Replace`).
pub const DECISION_PARAMETER_TOOLS: [&str; 5] = [
    "preview_console_logs",
    "preview_snapshot",
    "preview_inspect",
    "preview_click",
    "preview_fill",
];

/// Every parameter that sends a call through the decision model, on any tool.
pub const DECISION_PARAMETERS: [&str; 2] = ["query", "threshold"];

/// The decision parameters one tool takes. The console and snapshot tools *score* their material,
/// so `threshold` is the cut; the element tools *choose* one element (or "none of the above"),
/// which has no score to cut at, so they take `query` alone.
pub fn decision_parameters(name: &str) -> &'static [&'static str] {
    match name {
        "preview_console_logs" | "preview_snapshot" => &["query", "threshold"],
        "preview_inspect" | "preview_click" | "preview_fill" => &["query"],
        _ => &[],
    }
}

pub fn takes_decision_parameters(name: &str) -> bool {
    DECISION_PARAMETER_TOOLS.contains(&name)
}

/// The parameters `Replace` withdraws: what the tool addresses its target by when it does not ask
/// the decision model. The console and snapshot tools have none to withdraw — their direct form is
/// the unfiltered answer itself, which `Replace` retires by making `query` required.
pub fn direct_parameters(name: &str) -> &'static [&'static str] {
    match name {
        "preview_inspect" | "preview_click" | "preview_fill" => &["selector"],
        _ => &[],
    }
}

/// The tools whose decision-model form *chooses* an element and so can miss: the ones a
/// conversation may ask to score every element line after a "none of the above"
/// ([`ConversationSettings::decision_miss_scoring`]). Mirrored by TS
/// `src/lib/taskTools.ts::MISS_SCORING_TOOL_NAMES`.
pub const MISS_SCORING_TOOLS: [&str; 3] = ["preview_inspect", "preview_click", "preview_fill"];

/// The element tools whose misses one run scores: what the conversation asks for, kept to the
/// tools this run gives a decision-parameter mode — a tool left on its direct form never reaches
/// the decision model, so it has no miss to score.
pub fn effective_miss_scoring(
    settings: &ConversationSettings,
    modes: &BTreeMap<String, DecisionParameterMode>,
) -> BTreeSet<String> {
    MISS_SCORING_TOOLS
        .iter()
        .filter(|name| settings.decision_miss_scoring.contains(**name) && modes.contains_key(**name))
        .map(|name| (*name).to_owned())
        .collect()
}

/// Whether a call asks for the decision model, which is what routes it there in the executor.
pub fn carries_decision_parameters(name: &str, input: &JsonObject) -> bool {
    decision_parameters(name)
        .iter()
        .any(|key| input.contains_key(*key))
}

/// Each enabled decision-parameter tool's mode for one run: what the conversation asks for,
/// widened by [`DecisionParameterMode::join`] to cover whatever the tool lock says the transcript
/// already holds. A tool whose mode comes out as "direct parameters only" has no entry.
pub fn effective_parameter_modes(
    settings: &ConversationSettings,
    enabled_tools: &[String],
) -> BTreeMap<String, DecisionParameterMode> {
    DECISION_PARAMETER_TOOLS
        .iter()
        .filter(|name| enabled_tools.iter().any(|enabled| enabled == *name))
        .filter_map(|name| {
            let requested = settings.decision_parameter_modes.get(*name).copied();
            let exposed = settings
                .tool_lock
                .as_ref()
                .filter(|lock| lock.tools.iter().any(|tool| tool == name));
            let mode = match exposed {
                Some(lock) => DecisionParameterMode::join(
                    lock.decision_parameter_modes.get(*name).copied(),
                    requested,
                ),
                None => requested,
            };
            mode.map(|mode| ((*name).to_owned(), mode))
        })
        .collect()
}

/// Writes each moded tool's schema onto its descriptor, the first rung of
/// `aisdk::tools::tool_schema`, so all three wire protocols see it and a child cloned from this
/// request's tools inherits it with its parent's modes.
pub(crate) fn inject_parameter_schemas(request: &mut RunModelRequest) {
    for (name, mode) in &request.decision_parameter_modes {
        let Some(base) = crate::builtin_schemas::builtin_tool_schema(name, &request.prompt_profile)
        else {
            continue;
        };
        let schema = crate::builtin_schemas::with_decision_parameters(base, name, *mode);
        if let Some(descriptor) = request.tools.iter_mut().find(|tool| &tool.name == name) {
            descriptor.input_schema = Some(schema);
        }
    }
}

/// Why a call's arguments do not fit the mode this run gave its tool, or `None` when they do.
///
/// The executor routes on the arguments alone — `query` present means the decision model — so
/// this is the gate that keeps a conversation's choice binding: a tool left on its direct form
/// never sends the page to the decision provider, a `Replace` tool never acts on a selector, and
/// an element tool is never handed a `threshold` it has no score to apply to.
pub(crate) fn parameter_mode_rejection(
    modes: &BTreeMap<String, DecisionParameterMode>,
    name: &str,
    input: &JsonObject,
) -> Option<String> {
    if !takes_decision_parameters(name) {
        return None;
    }
    let own = decision_parameters(name);
    let pair = own.join(" and ");
    let decision = carries_decision_parameters(name, input);
    let direct = direct_parameters(name)
        .iter()
        .find(|key| input.contains_key(**key));
    let mode = modes.get(name);
    if let Some(foreign) = DECISION_PARAMETERS
        .iter()
        .find(|key| input.contains_key(**key) && !own.contains(*key))
    {
        return Some(format!("{name} does not take {foreign}."));
    }
    match mode {
        None if decision => Some(format!(
            "{name} does not take {pair} in this conversation; call it with its own parameters."
        )),
        None => None,
        Some(DecisionParameterMode::Replace) => match direct {
            Some(direct) => Some(format!(
                "{name} does not take {direct} in this conversation; describe the target with \
                 {pair} instead."
            )),
            None if !decision => Some(format!(
                "{name} goes through the decision model in this conversation; pass {pair}."
            )),
            None => None,
        },
        Some(DecisionParameterMode::Augment) => match direct {
            Some(direct) if decision => Some(format!(
                "Pass {name} either {direct} or {pair}, not both."
            )),
            _ => None,
        },
    }
}

/// Line-addressed material as the decision model is shown it: one block per original block,
/// labelled `{source} line 7` / `{source} lines 7-9` in the numbering that starts at
/// `first_line`, and cut into parts when too long for one question.
pub(crate) fn line_blocks(blocks: &Blocks, source: &str, first_line: usize) -> Vec<Block> {
    let offset = first_line - 1;
    blocks
        .spans()
        .iter()
        .map(|&(first, last)| {
            let parts = blocks
                .parts(first, last)
                .into_iter()
                .map(|(text, start, end)| (text, start + offset, end + offset))
                .collect();
            Block::in_parts(
                format!(
                    "{source} {}",
                    range_label("line", "lines", first + offset, last + offset)
                ),
                parts,
            )
        })
        .collect()
}

/// A block of line-addressed material as the model reads it back: every line with its number,
/// `cat -n` style — the number right-aligned in six columns, then a tab. The decision model is
/// never shown the numbers; they would only be noise to it.
pub(crate) fn numbered(blocks: &Blocks, span: (usize, usize), first_line: usize) -> String {
    let offset = first_line - 1;
    blocks
        .lines(span.0, span.1)
        .iter()
        .enumerate()
        .map(|(index, line)| format!("{:>6}\t{line}", span.0 + offset + index))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `line 5` / `lines 5-9`, and the same for any other unit.
pub(crate) fn range_label(one: &str, many: &str, first: usize, last: usize) -> String {
    if first == last {
        format!("{one} {first}")
    } else {
        format!("{many} {first}-{last}")
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use DecisionParameterMode::{Augment, Replace};

    fn settings(value: serde_json::Value) -> ConversationSettings {
        serde_json::from_value(value).expect("conversation settings fixture")
    }

    fn input(value: serde_json::Value) -> JsonObject {
        value.as_object().expect("object input").clone()
    }

    /// `None` is "direct parameters only"; the join is the narrowest schema that accepts both.
    #[test]
    fn join_is_the_narrowest_schema_that_accepts_both() {
        let join = DecisionParameterMode::join;
        assert_eq!(join(None, None), None);
        assert_eq!(join(Some(Replace), Some(Replace)), Some(Replace));
        assert_eq!(join(Some(Augment), Some(Augment)), Some(Augment));
        assert_eq!(join(None, Some(Replace)), Some(Augment));
        assert_eq!(join(Some(Replace), None), Some(Augment));
        assert_eq!(join(None, Some(Augment)), Some(Augment));
        assert_eq!(join(Some(Replace), Some(Augment)), Some(Augment));
    }

    /// A run widens what the conversation asks for to cover every call shape the transcript may
    /// already hold, and only for tools it actually enables.
    #[test]
    fn a_run_widens_the_requested_mode_to_what_the_transcript_holds() {
        let settings = settings(json!({
            "enabledTools": [
                "preview_click", "preview_fill", "preview_snapshot", "preview_inspect",
                "preview_console_logs", "preview_eval"
            ],
            "decisionParameterModes": {
                "preview_click": "replace",
                "preview_fill": "replace",
                "preview_snapshot": "augment",
                "preview_console_logs": "bogus",
                "preview_eval": "augment",
                "preview_network": "augment"
            },
            "toolLock": {
                "tools": ["preview_click", "preview_inspect"],
                "decisionParameterModes": {"preview_inspect": "replace"}
            }
        }));
        let modes = effective_parameter_modes(&settings, &settings.enabled_tools);
        // Went out with its selector, so `Replace` would narrow it.
        assert_eq!(modes.get("preview_click"), Some(&Augment));
        // Never went out: the request stands.
        assert_eq!(modes.get("preview_fill"), Some(&Replace));
        assert_eq!(modes.get("preview_snapshot"), Some(&Augment));
        // Went out as `Replace`; asking for the direct form alone widens instead.
        assert_eq!(modes.get("preview_inspect"), Some(&Augment));
        // A mode this build does not know is dropped, not fatal.
        assert_eq!(modes.get("preview_console_logs"), None);
        // Only decision-parameter tools have a mode, and only enabled ones.
        assert_eq!(modes.get("preview_eval"), None);
        assert_eq!(modes.get("preview_network"), None);
        assert_eq!(modes.len(), 4);
    }

    /// A miss is scored only on an element tool this run hands a decision-parameter mode, and
    /// a settings entry this build cannot use is dropped rather than refused.
    #[test]
    fn misses_are_scored_only_where_the_run_can_miss() {
        let settings = settings(json!({
            "enabledTools": ["preview_click", "preview_fill", "preview_inspect", "preview_snapshot"],
            "decisionParameterModes": {
                "preview_click": "augment",
                "preview_inspect": "replace",
                "preview_snapshot": "augment"
            },
            "decisionMissScoring": [
                "preview_click", "preview_fill", "preview_snapshot", "preview_inspect", 7, null
            ]
        }));
        assert_eq!(
            settings.decision_miss_scoring,
            ["preview_click", "preview_fill", "preview_inspect", "preview_snapshot"]
                .map(String::from)
                .into_iter()
                .collect()
        );
        let modes = effective_parameter_modes(&settings, &settings.enabled_tools);
        assert_eq!(
            effective_miss_scoring(&settings, &modes),
            // `preview_fill` has no mode, so it never misses; `preview_snapshot` scores already.
            ["preview_click", "preview_inspect"].map(String::from).into_iter().collect()
        );
        // A document written before the field existed scores nothing.
        let bare = self::settings(json!({"enabledTools": []}));
        assert!(bare.decision_miss_scoring.is_empty());
        assert!(serde_json::to_value(&bare)
            .unwrap()
            .get("decisionMissScoring")
            .is_none());
    }

    #[test]
    fn a_call_must_fit_its_tools_mode() {
        let modes = BTreeMap::from([
            ("preview_click".to_owned(), Replace),
            ("preview_fill".to_owned(), Augment),
        ]);
        let reject = |name: &str, value: serde_json::Value| {
            parameter_mode_rejection(&modes, name, &input(value))
        };
        // No mode: the decision form is refused, the direct form passes.
        assert!(reject("preview_inspect", json!({"query": "the title"})).is_some());
        assert!(reject("preview_snapshot", json!({"threshold": 0.5})).is_some());
        assert_eq!(reject("preview_inspect", json!({"selector": "h1"})), None);
        // `Replace`: the decision form is the only one.
        assert!(reject("preview_click", json!({"selector": "button"}))
            .is_some_and(|reason| reason.contains("selector")));
        assert!(reject("preview_click", json!({"doubleClick": true})).is_some());
        assert_eq!(reject("preview_click", json!({"query": "save"})), None);
        // The element tools choose rather than score: there is no threshold to give them.
        assert!(reject("preview_click", json!({"query": "save", "threshold": 0.6}))
            .is_some_and(|reason| reason.contains("threshold")));
        // `Augment`: either form, never both.
        assert_eq!(reject("preview_fill", json!({"selector": "input", "value": ""})), None);
        assert_eq!(reject("preview_fill", json!({"query": "email", "value": ""})), None);
        assert!(reject(
            "preview_fill",
            json!({"selector": "input", "query": "email", "value": ""})
        )
        .is_some());
        // Tools without a decision form are none of this function's business.
        assert_eq!(reject("preview_eval", json!({"query": "x"})), None);
    }

    /// The console and snapshot tools score and have nothing to withdraw, so `Replace` only
    /// requires the pair; the element tools choose, take `query` alone, and withdraw their
    /// selector.
    #[test]
    fn only_the_element_tools_have_a_target_to_withdraw() {
        for name in DECISION_PARAMETER_TOOLS {
            let (direct, decision): (&[&str], &[&str]) = match name {
                "preview_console_logs" | "preview_snapshot" => (&[], &["query", "threshold"]),
                _ => (&["selector"], &["query"]),
            };
            assert_eq!(direct_parameters(name), direct, "{name}");
            assert_eq!(decision_parameters(name), decision, "{name}");
        }
        assert!(!takes_decision_parameters("preview_find_logs"));
        assert!(is_decision_tool_name("preview_find_logs"));
    }
}
