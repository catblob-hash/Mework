//! Plan mode: a switch under the composer that asks the model to research,
//! write a plan and get it approved before it changes anything.
//!
//! Independent of the security level: the level decides what the model's calls
//! need whether or not a plan is being written. What the switch does:
//!
//! - **Guidance, appended.** Each time the switch goes on, the round boundary
//!   appends the plan-mode system prompt (`system.plan_mode`) at that point of
//!   the transcript (`system_append.rs`) — never into the system prompt itself,
//!   which would cost the whole prompt cache. Turned off by hand before a plan
//!   was approved, it appends `system.plan_mode_exit`; an approved plan needs
//!   no such note, its tool result says so.
//! - **Tools, once.** The first time, `plan` and `exit_plan_mode` join the tool
//!   set (`tool_append.rs`), and they stay: switching plan mode on again never
//!   appends them a second time.
//! - **Exit.** Approving a plan turns the switch off — the live cell at once,
//!   so the turn that asked can go on to implement it, and the persisted
//!   setting, which the renderer mirrors.
//! - **The repository is left alone.** `write` and `edit` refuse a target Git
//!   would count as a change — tracked, or inside a work tree and not ignored,
//!   a new file included (`git_core::write_changes_repository` here, the
//!   repository probe in `remote_files.rs` on another machine) — with
//!   `repository_write_refusal`. The run loop checks before the call meets a
//!   permission hook or an approval card (`tool_executor::plan_mode_refusal`),
//!   so the user is never asked about a write that would be refused. Nothing
//!   else is checked: shell commands are the guidance's to rule, and paths
//!   outside a repository or ignored by Git stay writable.
//! - **One way out.** Only `exit_plan_mode` puts the plan to the user. A turn
//!   that simply ends, ends: nothing reminds the model and nothing is put to
//!   the user on its behalf.

use std::path::Path;

use crate::{
    api::{failed_tool_execution, ToolCall, ToolExecution},
    model::{
        ContextItem, ConversationPlan, JsonObject, PlanStatus, ResolvedLanguage,
        RunModelRequest, ToolResult,
    },
    push_events::AppPushEvent,
    security::RiskLevel,
    state::AppState,
    tool_prompt::{PendingToolPrompt, PromptKind, PromptOwner},
};

pub(crate) const PLAN_TOOL: &str = "plan";
pub(crate) const EXIT_PLAN_MODE_TOOL: &str = "exit_plan_mode";

/// A plan document is a document, not a codebase dump. Long enough for any real
/// plan, short enough that a runaway write cannot fill the database.
const MAX_PLAN_CHARS: usize = 200_000;
/// The card is one line above the composer; the plan itself is in the panel.
const MAX_PLAN_TITLE_CHARS: usize = 240;

/// The topics of the system prompts plan mode appends (`system_append.rs`):
/// the guidance as the switch goes on, and the note as it goes off by hand.
pub(crate) const ENTER_TOPIC: &str = "plan-mode";
pub(crate) const EXIT_TOPIC: &str = "plan-mode-exit";
/// The same two as host notices in `box`, where the model or endpoint takes no
/// system message mid-conversation (`host_append::InstructionCarrier`).
pub(crate) const ENTER_NOTICE_KIND: &str = "plan_mode";
pub(crate) const EXIT_NOTICE_KIND: &str = "plan_mode_exit";

/// Which of plan mode's two messages `context` is, in either carrier:
/// `Some(true)` for the guidance, `Some(false)` for the exit note.
fn plan_mode_message(context: &ContextItem) -> Option<bool> {
    match crate::system_append::topic(context) {
        Some(ENTER_TOPIC) => return Some(true),
        Some(EXIT_TOPIC) => return Some(false),
        _ => {}
    }
    match crate::wire_history::host_notice_kind(context) {
        Some(ENTER_NOTICE_KIND) => Some(true),
        Some(EXIT_NOTICE_KIND) => Some(false),
        _ => None,
    }
}

/// How an approved `exit_plan_mode` result begins. The transcript reads plan
/// mode as ended at a result that does.
const APPROVED_RESULT_PREFIX: &str = "User has approved your plan.";

/// What `write` and `edit` answer in plan mode when the target is part of a Git
/// repository's content: tracked, or inside a work tree and not ignored, a new
/// file included. Everything else stays writable — a scratch file outside the
/// repository or under an ignored path, the plan itself — and nothing but these
/// two tools is checked. Only the run loop asks, before any approval card
/// (`tool_executor::plan_mode_refusal`), so a call the user replays by hand is
/// never refused.
pub(crate) fn repository_write_refusal(path: &str) -> String {
    format!(
        "Plan mode is on: {path} is part of a Git repository (tracked, or not ignored), and the repository stays as it is until plan mode ends. Put this change in your plan; once the user approves it through exit_plan_mode, plan mode ends and you can make it. Paths outside the repository or ignored by Git can be written now."
    )
}

/// True for the two tools whose availability the host derives from the
/// plan-mode switch rather than from any list a user or a role can edit.
pub(crate) fn is_plan_mode_tool_name(name: &str) -> bool {
    matches!(name, PLAN_TOOL | EXIT_PLAN_MODE_TOOL)
}

/// Which plan tools a step offers.
///
/// Both or neither. `plan_tools` is sticky (`RunModelRequest::plan_tools`):
/// once plan mode has been on, the pair stays on every later step, so the
/// model can read the plan after approval and switching plan mode on again
/// appends nothing. Children get neither. The plan belongs to the
/// conversation the user is talking to; a child neither writes it nor asks for
/// its approval.
pub(crate) fn derived_tools(plan_tools: bool, subagent_depth: usize) -> &'static [&'static str] {
    if plan_tools && subagent_depth == 0 {
        &[PLAN_TOOL, EXIT_PLAN_MODE_TOOL]
    } else {
        &[]
    }
}

/// Whether the transcript has offered the pair before: it holds the plan-mode
/// guidance, or a call to either tool.
pub(crate) fn transcript_offered_tools(contexts: &[ContextItem]) -> bool {
    contexts.iter().any(|context| {
        matches!(context, ContextItem::Tool { tool_name, .. } if is_plan_mode_tool_name(tool_name))
            || plan_mode_message(context) == Some(true)
    })
}

/// Whether the model was last told it is in plan mode, reading the transcript
/// back from its end: the appended guidance says it is, the exit note or an
/// approved plan says it is not, and a transcript with none of them never was.
pub(crate) fn transcript_in_plan_mode<'a>(
    contexts: impl DoubleEndedIterator<Item = &'a ContextItem>,
) -> bool {
    for context in contexts.rev() {
        if let Some(entered) = plan_mode_message(context) {
            return entered;
        }
        if let ContextItem::Tool {
            tool_name, result, ..
        } = context
        {
            if tool_name == EXIT_PLAN_MODE_TOOL
                && result.success
                && result.output.starts_with(APPROVED_RESULT_PREFIX)
            {
                return false;
            }
        }
    }
    false
}

/// The conversation database beside this run's application-data root.
fn plan_store(
    app_data_path: &str,
) -> Result<std::sync::Arc<crate::conversation_store::ConversationStore>, String> {
    if app_data_path.trim().is_empty() {
        return Err(
            "This run has no application data directory, so it cannot reach the plan".into(),
        );
    }
    crate::conversations::store(&Path::new(app_data_path).join("document.v1.json"))
}

/// Tells the renderer what the plan panel should show now. `None` says there is
/// nothing written yet.
fn publish_plan(state: &AppState, conversation_id: &str, plan: Option<ConversationPlan>) {
    state
        .push_events
        .publish(AppPushEvent::ConversationPlanUpdated {
            conversation_id: conversation_id.to_owned(),
            plan,
        });
}

fn succeeded(call: ToolCall, output: String) -> ToolExecution {
    ToolExecution {
        call,
        result: ToolResult {
            success: true,
            output,
            images: Vec::new(),
            diff: None,
            executed_at: chrono::Utc::now().to_rfc3339(),
            duration_ms: 0,
        },
        subagent: None,
    }
}

/// The plan's own first heading, for the one line the approval card shows.
fn plan_title(markdown: &str) -> String {
    let line = markdown
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let title = line.trim_start_matches('#').trim();
    crate::tool_prompt::escape_display_text(title)
        .chars()
        .take(MAX_PLAN_TITLE_CHARS)
        .collect()
}

fn text_argument<'a>(input: &'a JsonObject, key: &str) -> Option<&'a str> {
    input.get(key).and_then(serde_json::Value::as_str)
}

/// Reads or replaces the conversation's plan document.
pub(crate) fn run_plan_tool(
    request: &RunModelRequest,
    call: ToolCall,
    state: &AppState,
) -> ToolExecution {
    if request.subagent_depth > 0 {
        return failed_tool_execution(call, "plan cannot be used in agent contexts".into());
    }
    let action = text_argument(&call.input, "action")
        .unwrap_or_default()
        .trim()
        .to_owned();
    let store = match plan_store(&request.app_data_path) {
        Ok(store) => store,
        Err(error) => return failed_tool_execution(call, error),
    };
    match action.as_str() {
        "write" => {
            let content = text_argument(&call.input, "content")
                .unwrap_or_default()
                .trim()
                .to_owned();
            if content.is_empty() {
                return failed_tool_execution(
                    call,
                    "The write action needs the plan's Markdown in `content`.".into(),
                );
            }
            let characters = content.chars().count();
            if characters > MAX_PLAN_CHARS {
                return failed_tool_execution(
                    call,
                    format!(
                        "The plan is {characters} characters, over the {MAX_PLAN_CHARS} character limit. Write the plan, not the code."
                    ),
                );
            }
            let existing = match store.conversation_plan(&request.conversation_id) {
                Ok(plan) => plan,
                Err(error) => return failed_tool_execution(call, error),
            };
            let now = chrono::Utc::now().to_rfc3339();
            let plan = ConversationPlan {
                conversation_id: request.conversation_id.clone(),
                markdown: content,
                // A rewrite is a new draft even after feedback or approval: the
                // user is being asked again, about different text.
                status: PlanStatus::Draft,
                created_at: existing
                    .map(|plan| plan.created_at)
                    .unwrap_or_else(|| now.clone()),
                updated_at: now,
            };
            if let Err(error) = store.put_conversation_plan(&plan) {
                return failed_tool_execution(call, error);
            }
            publish_plan(state, &request.conversation_id, Some(plan));
            succeeded(
                call,
                format!(
                    "Plan saved ({characters} characters). The user can read it in the plan panel. Call exit_plan_mode when you are ready for review."
                ),
            )
        }
        "read" => match store.conversation_plan(&request.conversation_id) {
            Ok(Some(plan)) if !plan.markdown.trim().is_empty() => succeeded(call, plan.markdown),
            Ok(_) => succeeded(call, "No plan has been written yet.".into()),
            Err(error) => failed_tool_execution(call, error),
        },
        other => failed_tool_execution(
            call,
            format!("Unknown plan action `{other}`. Use `write` or `read`."),
        ),
    }
}

/// Presents the written plan for approval.
///
/// The call blocks on the card: the model asked a question whose answer decides
/// whether it starts implementing, so returning before the user answers would
/// leave it guessing. The card has no refusal: the user either approves or
/// writes what should change, and after feedback the model revises the plan and
/// asks again, as many times as it takes. Approval ends plan mode and moves
/// nothing else — the security level is the user's, and the plan tools stay.
pub(crate) fn run_exit_plan_mode_tool(
    request: &RunModelRequest,
    call: ToolCall,
    state: &AppState,
) -> ToolExecution {
    if request.subagent_depth > 0 {
        return failed_tool_execution(
            call,
            "exit_plan_mode cannot be used in agent contexts".into(),
        );
    }
    // The pair outlives plan mode, so the model can reach for this after the
    // switch went off; there is nothing to approve then.
    if !request.plan_mode_active() {
        return failed_tool_execution(
            call,
            "Plan mode is off, so there is no plan to ask approval for. The user turns plan mode on from the composer; until then, carry on with the task.".into(),
        );
    }
    let store = match plan_store(&request.app_data_path) {
        Ok(store) => store,
        Err(error) => return failed_tool_execution(call, error),
    };
    let plan = match store.conversation_plan(&request.conversation_id) {
        Ok(Some(plan)) if !plan.markdown.trim().is_empty() => plan,
        Ok(_) => {
            return failed_tool_execution(
                call,
                "No plan has been written yet. Write your plan with the plan tool before calling exit_plan_mode.".into(),
            )
        }
        Err(error) => return failed_tool_execution(call, error),
    };
    match review(request, state, &store, plan) {
        // Success either way: the call did run and produced the answer the
        // model asked for. A failed result would read as "the tool broke",
        // not "not yet".
        Ok(output) => succeeded(call, output),
        Err(error) => failed_tool_execution(call, error),
    }
}

/// Raises the approval card for `plan`, records the answer on the document,
/// and words it for the model. Approval also ends plan mode.
fn review(
    request: &RunModelRequest,
    state: &AppState,
    store: &crate::conversation_store::ConversationStore,
    plan: ConversationPlan,
) -> Result<String, String> {
    let language = crate::api::approval_card_language(state, &request.app_data_path);
    let english = language == ResolvedLanguage::EnUs;
    let card = PendingToolPrompt {
        // Minted by the registry; see `ToolPromptRegistry::ask_answer`.
        prompt_id: String::new(),
        tool_name: EXIT_PLAN_MODE_TOOL.to_owned(),
        kind: PromptKind::PlanExit,
        label: if english {
            "Plan ready"
        } else {
            "计划已就绪"
        }
        .to_owned(),
        summary: plan_title(&plan.markdown),
        risk_level: RiskLevel::Low.label(language).to_owned(),
        reason: if english {
            "The model has written a plan and is waiting for your approval or your feedback"
        } else {
            "模型已写好计划，等待你批准或提意见"
        }
        .to_owned(),
        requester: None,
        source_agent: None,
        source_call_id: None,
        allow_always_offered: false,
        mandatory: true,
        questions: None,
    };
    let answer = ask_plan_card(request, state, card)?;

    let now = chrono::Utc::now().to_rfc3339();
    if !answer.decision.allows() {
        // Feedback, not a refusal: `Rejected` is the stored word for "changes
        // requested", and the model is expected to come back with a revision.
        store.set_conversation_plan_status(&request.conversation_id, PlanStatus::Rejected, &now)?;
        publish_plan(
            state,
            &request.conversation_id,
            Some(ConversationPlan {
                status: PlanStatus::Rejected,
                updated_at: now,
                ..plan
            }),
        );
        let feedback = answer
            .feedback
            .filter(|feedback| !feedback.trim().is_empty())
            .unwrap_or_else(|| "(no feedback given)".to_owned());
        return Ok(format!(
            "The user has not approved this plan yet and left feedback:\n{feedback}\n\nRevise the plan with the plan tool to address it, then call exit_plan_mode again."
        ));
    }

    store.set_conversation_plan_status(&request.conversation_id, PlanStatus::Approved, &now)?;
    leave_plan_mode(request, state);
    let markdown = plan.markdown.clone();
    publish_plan(
        state,
        &request.conversation_id,
        Some(ConversationPlan {
            status: PlanStatus::Approved,
            updated_at: now,
            ..plan
        }),
    );
    Ok(format!(
        "{APPROVED_RESULT_PREFIX} You can now start implementing it.\n\nThe plan stays available through the plan tool's read action.\n\n## Approved Plan:\n{markdown}"
    ))
}

/// Ends plan mode once the user approved a plan: the live cell first, so the
/// next boundary of this very turn — which goes on to implement the plan —
/// already reads plan mode as off, then the persisted switch, which the
/// composer mirrors.
///
/// A failed write leaves the switch on disk for the user to turn off; the turn
/// itself already left plan mode, and its transcript says the plan was
/// approved, so the next boundary appends nothing either way.
fn leave_plan_mode(request: &RunModelRequest, state: &AppState) {
    if let Some(cell) = &request.live_plan_mode {
        cell.set(false);
    }
    if let Err(error) =
        persist_switch_off(state, Path::new(&request.app_data_path), &request.conversation_id)
    {
        eprintln!("计划已批准，但计划模式开关未能写回: {error}");
    }
}

/// Writes the conversation's plan-mode switch off on the host's initiative and
/// tells the renderer, the way the host writes a title (`helper_model`).
fn persist_switch_off(
    state: &AppState,
    app_data_path: &Path,
    conversation_id: &str,
) -> Result<(), String> {
    if app_data_path.as_os_str().is_empty() {
        return Ok(());
    }
    let anchor = app_data_path.join("document.v1.json");
    let guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let store = crate::conversations::store(&anchor)?;
    let Some(workspace_id) = store.conversation_workspaces()?.get(conversation_id).cloned() else {
        return Ok(());
    };
    let Some(mut conversation) = store.conversation(conversation_id)? else {
        return Ok(());
    };
    if conversation.settings.plan_mode_enabled {
        conversation.settings.plan_mode_enabled = false;
        store.put_conversation_metadata(&workspace_id, &conversation)?;
        let stored = store.conversation(conversation_id)?;
        crate::conversations::sync_snapshot(state, &anchor, &workspace_id, conversation_id, stored)?;
    }
    drop(guard);
    state
        .push_events
        .publish(AppPushEvent::ConversationPlanModeChanged {
            conversation_id: conversation_id.to_owned(),
            enabled: false,
        });
    Ok(())
}

/// Raises one plan card and blocks on it.
///
/// The card belongs to this run, and the only stop signal it watches is this
/// run's own: stopping generation must release the model, while an unrelated
/// task being stopped must not answer a question the user is looking at.
fn ask_plan_card(
    request: &RunModelRequest,
    state: &AppState,
    card: PendingToolPrompt,
) -> Result<crate::tool_prompt::PromptAnswer, String> {
    let cancellation = state.model_run_cancellation_flag(&request.request_id);
    let cancellations: Vec<&std::sync::atomic::AtomicBool> =
        cancellation.iter().map(|flag| flag.as_ref()).collect();
    crate::api::ask_announced_prompt(
        state,
        &request.conversation_id,
        PromptOwner::Run(request.request_id.clone()),
        true,
        RiskLevel::Low,
        &cancellations,
        card,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole availability rule: the setting offers both tools, never one,
    /// and a child agent gets neither whatever the setting says.
    #[test]
    fn the_setting_derives_both_plan_tools_and_children_derive_none() {
        assert_eq!(derived_tools(true, 0), &[PLAN_TOOL, EXIT_PLAN_MODE_TOOL]);
        assert!(derived_tools(false, 0).is_empty());
        assert!(derived_tools(true, 1).is_empty());
        assert!(derived_tools(true, 0)
            .iter()
            .all(|name| is_plan_mode_tool_name(name)));
    }

    /// An archive from when plan mode was a security level keeps its intent:
    /// the level becomes the strictest one and the plan-mode setting turns on,
    /// wherever the settings sit.
    #[test]
    fn the_legacy_plan_level_becomes_the_setting() {
        let mut value = serde_json::json!({
            "settings": { "securityLevel": "plan", "enabledTools": [] },
            "presets": [{ "settings": { "securityLevel": "allow_edits" } }],
            "lastConversationSettings": { "securityLevel": "plan" },
        });
        crate::model::migrate_legacy_plan_level(&mut value);
        assert_eq!(value["settings"]["securityLevel"], "request_approval");
        assert_eq!(value["settings"]["planModeEnabled"], true);
        assert_eq!(value["presets"][0]["settings"]["securityLevel"], "allow_edits");
        assert!(value["presets"][0]["settings"].get("planModeEnabled").is_none());
        assert_eq!(value["lastConversationSettings"]["planModeEnabled"], true);
        // A stray old value that skipped the migration still loads.
        let level: crate::model::SecurityLevel = serde_json::from_str("\"plan\"").unwrap();
        assert_eq!(level, crate::model::SecurityLevel::RequestApproval);
    }
}
