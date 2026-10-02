//! `ask_user`: Claude Code's AskUserQuestion, answered while the tool call
//! blocks.
//!
//! The call raises a question card through the same registry as approval
//! cards and waits on it. The user's answers come back as this call's tool
//! result — worded exactly as Claude Code words them — so the model reads the
//! answer where it asked the question, inside the same turn.
//!
//! The card has these ways out:
//!
//! * **Submit** — the answers the user gave (possibly not every question).
//! * **Close** (Esc / the close button / the review screen's Cancel) — the
//!   result says the user closed the card, and the turn ends there: closing a
//!   card is not a reason to ask the model anything more.
//! * **A composer message** — the composer stays live while the card is up.
//!   Sending from it steers the message into this run first and then answers
//!   the card with whatever was filled in (a close when nothing was). The
//!   result settles this call and the message follows it into the next round.
//! * **Chat about this** — the questions are declined with the user's partial
//!   answers attached, and the model is told to ask what the user wants to
//!   clarify. The turn goes on.
//!
//! Stopping the run while the card is up retracts it like any other card.
//!
//! As in Claude Code, answered calls execute with the answers merged into their
//! input (`answers` / `annotations`, keyed by question text). The timeline
//! keeps that input, while the model is replayed the input it actually wrote.

use serde_json::{Map, Value};

use crate::{
    api::{failed_tool_execution, ToolCall, ToolExecution},
    model::{ResolvedLanguage, RunModelRequest, ToolResult},
    orchestration::{parse_question, QuestionItemSpec, QuestionSpec},
    security::RiskLevel,
    state::AppState,
    tool_prompt::{PendingToolPrompt, PromptKind, PromptOwner, QuestionAction, QuestionResponse},
};

pub(crate) const ASK_USER_TOOL: &str = "ask_user";

/// Claude Code's result for a tool use the user rejected with a message; the
/// message follows on the next line.
pub(crate) const REJECT_MESSAGE_WITH_REASON_PREFIX: &str = "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). To tell you how to proceed, the user said:\n";

/// The result of a question card the user closed without answering.
pub(crate) const QUESTION_CLOSED: &str = "The user closed the question card without answering.";

/// Claude Code's result for a card submitted with nothing answered.
pub(crate) const NO_ANSWER: &str = "The user did not answer the questions.";

/// What a call later in the same round gets when the user ended the turn by
/// closing a question card before it could run.
pub(crate) const INTERRUPTED_CALL_SKIPPED: &str = "[Request interrupted by user for tool use]";

/// The stop reason of a turn that ended because the user closed a question
/// card. The turn ends without a final assistant message, and nothing failed.
pub(crate) const QUESTION_CLOSED_STOP_REASON: &str = "question_closed";

/// Whether this settled call is a question card the user closed. The turn ends
/// there unless a user message is waiting to follow the result (`steer_pending`)
/// — the user closed the card by sending that message.
pub(crate) fn ends_turn(execution: &ToolExecution, steer_pending: bool) -> bool {
    execution.call.name == ASK_USER_TOOL
        && execution.result.output == QUESTION_CLOSED
        && !steer_pending
}

/// Runs one `ask_user` call: validates it, raises the question card, blocks
/// until the user answers or closes it or the run stops, and returns what the
/// model reads.
pub(crate) fn run_ask_user_tool(
    request: &RunModelRequest,
    mut call: ToolCall,
    state: &AppState,
) -> ToolExecution {
    if request.subagent_depth > 0 {
        return failed_tool_execution(call, "ask_user cannot be used in agent contexts".into());
    }
    let spec = match parse_question(&call.input) {
        Ok(spec) => spec,
        Err(error) => return failed_tool_execution(call, error),
    };
    let language = crate::api::approval_card_language(state, &request.app_data_path);
    let card = question_card(&spec, language);
    let cancellation = state.model_run_cancellation_flag(&request.request_id);
    let cancellations: Vec<&std::sync::atomic::AtomicBool> =
        cancellation.iter().map(|flag| flag.as_ref()).collect();
    let answer = match crate::api::ask_announced_prompt(
        state,
        &request.conversation_id,
        PromptOwner::Run(request.request_id.clone()),
        true,
        RiskLevel::Low,
        &cancellations,
        card,
    ) {
        Ok(answer) => answer,
        Err(error) => return failed_tool_execution(call, error),
    };
    let (success, output) = match answer.question {
        Some(response) => match response.action {
            QuestionAction::Submit => {
                // Like Claude Code's `updatedInput`: the call runs with the
                // answers it collected, which is what the timeline shows.
                merge_answers_into_input(&mut call.input, &spec, &response);
                (true, answered_result(&spec, &response))
            }
            QuestionAction::Close => (true, QUESTION_CLOSED.to_owned()),
            QuestionAction::Chat => (
                false,
                format!(
                    "{REJECT_MESSAGE_WITH_REASON_PREFIX}{}",
                    clarify_feedback(&spec, &response)
                ),
            ),
        },
        // A bare denial is the card being closed through the generic path.
        None => (true, QUESTION_CLOSED.to_owned()),
    };
    ToolExecution {
        call,
        result: ToolResult {
            success,
            output,
            images: Vec::new(),
            diff: None,
            executed_at: chrono::Utc::now().to_rfc3339(),
            duration_ms: 0,
        },
        subagent: None,
    }
}

/// The card the renderer draws: the questions themselves ride along, the rest
/// is what lists and notifications show.
fn question_card(spec: &QuestionSpec, language: ResolvedLanguage) -> PendingToolPrompt {
    let english = language == ResolvedLanguage::EnUs;
    let questions = spec
        .questions
        .iter()
        .map(|question| {
            serde_json::json!({
                "question": question.question,
                "header": question.header,
                "multiSelect": question.multi_select,
                "options": question.options.iter().map(|option| {
                    let mut value = serde_json::json!({
                        "label": option.label,
                        "description": option.description,
                    });
                    if let Some(preview) = &option.preview {
                        value["preview"] = Value::String(preview.clone());
                    }
                    value
                }).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    PendingToolPrompt {
        // Minted by the registry; see `ToolPromptRegistry::ask_answer`.
        prompt_id: String::new(),
        tool_name: ASK_USER_TOOL.to_owned(),
        kind: PromptKind::Question,
        label: if english { "Answer questions?" } else { "回答问题？" }.to_owned(),
        summary: spec
            .questions
            .first()
            .map(|question| question.question.clone())
            .unwrap_or_default(),
        risk_level: RiskLevel::Low.label(language).to_owned(),
        reason: if english {
            "The model is waiting for your answers"
        } else {
            "模型在等你回答问题"
        }
        .to_owned(),
        requester: None,
        source_agent: None,
        source_call_id: None,
        allow_always_offered: false,
        mandatory: true,
        questions: Some(Value::Array(questions)),
    }
}

/// `answers` (question text → answer) and `annotations` (question text →
/// `{preview, notes}`), as Claude Code merges them into the call's input.
/// Whatever the model put under those keys is replaced: it cannot pre-answer.
fn merge_answers_into_input(
    input: &mut Map<String, Value>,
    spec: &QuestionSpec,
    response: &QuestionResponse,
) {
    let mut answers = Map::new();
    let mut annotations = Map::new();
    for (index, question) in spec.questions.iter().enumerate() {
        if let Some(answer) = raw_slot(&response.answers, index) {
            answers.insert(question.question.clone(), Value::String(answer.to_owned()));
        }
        let mut annotation = Map::new();
        if let Some(preview) = raw_slot(&response.previews, index) {
            annotation.insert("preview".into(), Value::String(preview.to_owned()));
        }
        if let Some(notes) = slot(&response.notes, index) {
            annotation.insert("notes".into(), Value::String(notes.to_owned()));
        }
        if !annotation.is_empty() {
            annotations.insert(question.question.clone(), Value::Object(annotation));
        }
    }
    input.insert("answers".into(), Value::Object(answers));
    input.insert("annotations".into(), Value::Object(annotations));
}

/// Claude Code's `mapToolResultToToolResultBlockParam` for an answered card.
///
/// One fragment per question that has an answer or notes, in question order:
/// `"question"="answer"` (or `"question"=(no option selected)`), then the
/// picked option's preview and the notes, the parts joined by a space and the
/// fragments by `", "`. Nothing is escaped. The wording depends on whether
/// every answer is "clean" — exactly the options offered, with no notes.
fn answered_result(spec: &QuestionSpec, response: &QuestionResponse) -> String {
    let mut fragments = Vec::new();
    let mut clean = true;
    for (index, question) in spec.questions.iter().enumerate() {
        let answer = raw_slot(&response.answers, index);
        let notes = slot(&response.notes, index);
        let preview = raw_slot(&response.previews, index);
        if notes.is_some() {
            clean = false;
        }
        if let Some(answer) = answer {
            if !answer_is_clean(question, answer) {
                clean = false;
            }
        }
        if answer.is_none() && notes.is_none() {
            continue;
        }
        let mut parts = vec![match answer {
            Some(answer) => format!("\"{}\"=\"{}\"", question.question, answer),
            None => format!("\"{}\"=(no option selected)", question.question),
        }];
        if let Some(preview) = preview {
            parts.push(format!("selected preview:\n{preview}"));
        }
        if let Some(notes) = notes {
            parts.push(format!("notes: {notes}"));
        }
        fragments.push(parts.join(" "));
    }
    if fragments.is_empty() {
        return NO_ANSWER.to_owned();
    }
    let joined = fragments.join(", ");
    if clean {
        format!("Your questions have been answered: {joined}. You can now continue with these answers in mind.")
    } else {
        format!("The user answered: {joined}. Read the answers carefully — they may request clarification, changes, or that you not proceed — and follow what they actually say.")
    }
}

/// An answer counts as clean when it is exactly an offered option — or, for a
/// multi-select question, a `", "`-joined list of offered options that joins
/// back to exactly the same text. Free text in the "Other" box is not clean.
fn answer_is_clean(question: &QuestionItemSpec, answer: &str) -> bool {
    let is_label = |item: &str| question.options.iter().any(|option| option.label == item);
    if is_label(answer) {
        return true;
    }
    if !question.multi_select {
        return false;
    }
    let Some(items) = split_multi_select(answer) else {
        return false;
    };
    join_multi_select(&items) == answer && items.iter().all(|item| is_label(item))
}

/// Claude Code's multi-select join: an item containing `", "` or a quote is
/// written as a JSON string, the rest verbatim.
pub(crate) fn join_multi_select(items: &[String]) -> String {
    items
        .iter()
        .map(|item| {
            if item.contains(", ") || item.contains('"') {
                serde_json::to_string(item).unwrap_or_else(|_| item.clone())
            } else {
                item.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Splits a joined multi-select answer back into its items, reading a
/// JSON-quoted item as one. `None` when a quoted item does not parse.
fn split_multi_select(answer: &str) -> Option<Vec<String>> {
    let mut items = Vec::new();
    let mut rest = answer;
    loop {
        if rest.starts_with('"') {
            // The shortest prefix that parses as a JSON string is the item.
            let mut end = None;
            let mut escaped = false;
            for (offset, character) in rest.char_indices().skip(1) {
                if escaped {
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == '"' {
                    end = Some(offset + 1);
                    break;
                }
            }
            let end = end?;
            items.push(serde_json::from_str::<String>(&rest[..end]).ok()?);
            rest = &rest[end..];
            if rest.is_empty() {
                return Some(items);
            }
            rest = rest.strip_prefix(", ")?;
        } else {
            match rest.find(", ") {
                Some(at) => {
                    items.push(rest[..at].to_owned());
                    rest = &rest[at + 2..];
                }
                None => {
                    items.push(rest.to_owned());
                    return Some(items);
                }
            }
        }
    }
}

/// The message "Chat about this" sends: the questions, with whatever the user
/// had answered so far. The 4-space indentation is Claude Code's, verbatim.
fn clarify_feedback(spec: &QuestionSpec, response: &QuestionResponse) -> String {
    let blocks = spec
        .questions
        .iter()
        .enumerate()
        .map(|(index, question)| {
            let mut lines = vec![format!("- \"{}\"", display_text(&question.question))];
            match raw_slot(&response.answers, index) {
                Some(answer) => lines.push(format!("  Answer: {answer}")),
                None => lines.push("  (No answer provided)".to_owned()),
            }
            if let Some(notes) = slot(&response.notes, index) {
                lines.push(format!("  User notes: {notes}"));
            }
            lines.join("\n")
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "The user wants to clarify these questions.\n    This means they may have additional information, context or questions for you.\n    Take their response into account and then reformulate the questions if appropriate.\n    Start by asking them what they would like to clarify.\n\n    Questions asked:\n{blocks}"
    )
}

/// The question as the card displays it: control and bidirectional-formatting
/// characters removed.
fn display_text(text: &str) -> String {
    text.chars()
        .filter(|character| {
            !(character.is_control() && *character != '\n' && *character != '\t'
                || matches!(
                    character,
                    '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
                ))
        })
        .collect()
}

/// A non-empty slot, as given. Answers are not trimmed: Claude Code hands the
/// "Other" text back exactly as typed.
fn raw_slot(values: &[Option<String>], index: usize) -> Option<&str> {
    values
        .get(index)
        .and_then(Option::as_deref)
        .filter(|value| !value.is_empty())
}

/// A slot that is not blank, trimmed — how notes are read.
fn slot(values: &[Option<String>], index: usize) -> Option<&str> {
    values
        .get(index)
        .and_then(Option::as_deref)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::QuestionOptionSpec;

    fn question(text: &str, multi_select: bool, labels: &[&str]) -> QuestionItemSpec {
        QuestionItemSpec {
            question: text.into(),
            header: "H".into(),
            options: labels
                .iter()
                .map(|label| QuestionOptionSpec {
                    label: (*label).into(),
                    description: "d".into(),
                    preview: None,
                })
                .collect(),
            multi_select,
        }
    }

    fn response(action: QuestionAction, answers: &[Option<&str>]) -> QuestionResponse {
        QuestionResponse {
            action,
            answers: answers.iter().map(|value| value.map(str::to_owned)).collect(),
            previews: Vec::new(),
            notes: Vec::new(),
        }
    }

    #[test]
    fn option_answers_read_as_answered_and_free_text_as_read_carefully() {
        let spec = QuestionSpec {
            questions: vec![
                question("Which DB?", false, &["Postgres", "SQLite"]),
                question("Which features?", true, &["Auth", "Billing"]),
            ],
        };
        assert_eq!(
            answered_result(
                &spec,
                &response(QuestionAction::Submit, &[Some("Postgres"), Some("Auth, Billing")])
            ),
            "Your questions have been answered: \"Which DB?\"=\"Postgres\", \"Which features?\"=\"Auth, Billing\". You can now continue with these answers in mind."
        );
        assert_eq!(
            answered_result(
                &spec,
                &response(QuestionAction::Submit, &[Some("use sqlite for now"), None])
            ),
            "The user answered: \"Which DB?\"=\"use sqlite for now\". Read the answers carefully — they may request clarification, changes, or that you not proceed — and follow what they actually say."
        );
        assert_eq!(
            answered_result(&spec, &response(QuestionAction::Submit, &[None, None])),
            NO_ANSWER
        );
    }

    #[test]
    fn previews_and_notes_follow_the_answer_and_notes_alone_still_count() {
        let spec = QuestionSpec {
            questions: vec![question("Layout?", false, &["Grid", "List"])],
        };
        let mut with_notes = response(QuestionAction::Submit, &[Some("Grid")]);
        with_notes.previews = vec![Some("[ ][ ]\n[ ][ ]".into())];
        with_notes.notes = vec![Some("make it wider".into())];
        assert_eq!(
            answered_result(&spec, &with_notes),
            "The user answered: \"Layout?\"=\"Grid\" selected preview:\n[ ][ ]\n[ ][ ] notes: make it wider. Read the answers carefully — they may request clarification, changes, or that you not proceed — and follow what they actually say."
        );
        let mut notes_only = response(QuestionAction::Submit, &[None]);
        notes_only.notes = vec![Some("none of these".into())];
        assert!(answered_result(&spec, &notes_only)
            .starts_with("The user answered: \"Layout?\"=(no option selected) notes: none of these. "));
    }

    #[test]
    fn multi_select_items_with_separators_are_quoted_and_still_clean() {
        let item = question("Pick", true, &["Red", "my, custom", "say \"hi\""]);
        let joined = join_multi_select(&[
            "Red".to_owned(),
            "my, custom".to_owned(),
            "say \"hi\"".to_owned(),
        ]);
        assert_eq!(joined, "Red, \"my, custom\", \"say \\\"hi\\\"\"");
        assert!(answer_is_clean(&item, &joined));
        assert!(!answer_is_clean(&item, "Red, Blue"));
        assert!(!answer_is_clean(&question("Pick", false, &["Red"]), "Red, Red"));
    }

    #[test]
    fn chat_about_this_lists_every_question_with_what_was_answered() {
        let spec = QuestionSpec {
            questions: vec![
                question("Which DB?", false, &["Postgres", "SQLite"]),
                question("Deploy where?", false, &["Fly", "AWS"]),
            ],
        };
        assert_eq!(
            clarify_feedback(&spec, &response(QuestionAction::Chat, &[Some("Postgres"), None])),
            "The user wants to clarify these questions.\n    This means they may have additional information, context or questions for you.\n    Take their response into account and then reformulate the questions if appropriate.\n    Start by asking them what they would like to clarify.\n\n    Questions asked:\n- \"Which DB?\"\n  Answer: Postgres\n- \"Deploy where?\"\n  (No answer provided)"
        );
    }

    #[test]
    fn answers_merge_into_the_input_keyed_by_question_text() {
        let spec = QuestionSpec {
            questions: vec![question("Which DB?", false, &["Postgres", "SQLite"])],
        };
        let mut input = Map::new();
        input.insert("answers".into(), serde_json::json!({"Which DB?": "forged"}));
        let mut submitted = response(QuestionAction::Submit, &[Some("SQLite")]);
        submitted.notes = vec![Some("  small  ".into())];
        merge_answers_into_input(&mut input, &spec, &submitted);
        assert_eq!(input["answers"], serde_json::json!({"Which DB?": "SQLite"}));
        assert_eq!(
            input["annotations"],
            serde_json::json!({"Which DB?": {"notes": "small"}})
        );
    }

    #[test]
    fn a_closed_card_ends_the_turn_unless_a_message_follows_it() {
        let execution = ToolExecution {
            call: ToolCall {
                id: "call".into(),
                name: ASK_USER_TOOL.into(),
                input: Map::new(),
            },
            result: ToolResult {
                success: true,
                output: QUESTION_CLOSED.into(),
                images: Vec::new(),
                diff: None,
                executed_at: String::new(),
                duration_ms: 0,
            },
            subagent: None,
        };
        assert!(ends_turn(&execution, false));
        assert!(!ends_turn(&execution, true));
    }
}
