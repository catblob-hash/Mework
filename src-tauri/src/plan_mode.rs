//! Plan mode: the security level in which the model may read and think but not
//! change anything, and the host-initiated switch out of it.
//!
//! Every other security-level change is the user picking one in the composer,
//! which arrives through `conversations::update`. This one is different: it is
//! the host acting on an approval the user gave inside a tool call, in the
//! middle of a turn. It therefore has to move three things that the renderer
//! path moves for free — the live cell the running turn reads, the persisted
//! setting SQLite and the document snapshot agree on, and the renderer's own
//! view of which level is selected.

use std::path::Path;

use crate::{
    api::{failed_tool_execution, ToolCall, ToolExecution},
    model::{ConversationPlan, JsonObject, PlanStatus, RunModelRequest, SecurityLevel, ToolResult},
    push_events::AppPushEvent,
    security::RiskLevel,
    state::AppState,
    tool_prompt::{PendingToolPrompt, PromptKind, PromptOwner, ToolPromptDecision},
};

pub(crate) const PLAN_TOOL: &str = "plan";
pub(crate) const EXIT_PLAN_MODE_TOOL: &str = "exit_plan_mode";
pub(crate) const ENTER_PLAN_MODE_TOOL: &str = "enter_plan_mode";

/// A plan document is a document, not a codebase dump. Long enough for any real
/// plan, short enough that a runaway write cannot fill the database.
const MAX_PLAN_CHARS: usize = 200_000;
/// The card is one line above the composer; the plan itself is in the panel.
const MAX_PLAN_TITLE_CHARS: usize = 240;

/// True for the three tools whose availability the host derives from the
/// security level rather than from any list a user or a role can edit.
pub(crate) fn is_plan_mode_tool_name(name: &str) -> bool {
    matches!(name, PLAN_TOOL | EXIT_PLAN_MODE_TOOL | ENTER_PLAN_MODE_TOOL)
}

/// Which plan tools exist for one step, given the level in force right now.
///
/// Derived per step rather than persisted because the level moves mid-turn: the
/// call that leaves plan mode must be the last step at which `exit_plan_mode`
/// is offered, and the next step must already offer `enter_plan_mode` instead.
///
/// Children get none of them. Plan mode belongs to the conversation the user is
/// talking to; a child neither writes that plan nor switches its parent's mode.
pub(crate) fn derived_tools(
    level: SecurityLevel,
    subagent_depth: usize,
) -> &'static [&'static str] {
    if subagent_depth > 0 {
        return &[];
    }
    match level {
        SecurityLevel::Plan => &[PLAN_TOOL, EXIT_PLAN_MODE_TOOL],
        SecurityLevel::RequestApproval | SecurityLevel::AllowEdits | SecurityLevel::FullAccess => {
            &[ENTER_PLAN_MODE_TOOL]
        }
    }
}

/// Moves a conversation to `level` on the host's initiative.
///
/// Persistence comes first and the live cell moves only once both stores hold
/// the new level: a write that fails leaves the turn where it was, and the tool
/// result says so, rather than running the rest of the turn under a level the
/// next run will not start in. The read-modify-write runs under the same
/// `storage_lock` the command layer holds, so a renderer commit cannot
/// interleave with it.
pub(crate) fn host_set_security_level(
    state: &AppState,
    app_data_path: &str,
    conversation_id: &str,
    level: SecurityLevel,
) -> Result<(), String> {
    if app_data_path.trim().is_empty() {
        return Err("This run has no application data directory, so it cannot change the security level".into());
    }
    let anchor = Path::new(app_data_path).join("document.v1.json");
    let guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let store = crate::conversations::store(&anchor)?;
    let workspaces = store.conversation_workspaces()?;
    let workspace_id = workspaces
        .get(conversation_id)
        .ok_or_else(|| format!("Conversation {conversation_id} is not in the conversation store"))?
        .clone();
    let mut conversation = store
        .conversation(conversation_id)?
        .ok_or_else(|| format!("Conversation {conversation_id} is not in the conversation store"))?;
    conversation.settings.security_level = level;
    // Metadata only: a run owns the timeline rows while this is called, and
    // rewriting them from this snapshot would drop whatever it persisted since.
    store.put_conversation_metadata(&workspace_id, &conversation)?;
    let stored = store
        .conversation(conversation_id)?
        .ok_or_else(|| format!("Conversation {conversation_id} could not be read back after the write"))?;
    crate::conversations::sync_snapshot(
        state,
        &anchor,
        &workspace_id,
        conversation_id,
        Some(stored.clone()),
    )?;
    if let Some(live) = state.live_security_level(conversation_id) {
        live.set(level);
    }
    drop(guard);

    state
        .push_events
        .publish(AppPushEvent::ConversationSecurityLevelChanged {
            conversation_id: conversation_id.to_owned(),
            security_level: level,
        });
    // The navigation callbacks enforcing browser admission run on the WebView's
    // own thread and read a cached level. Best effort: the next playwright call
    // pushes it again, so a conversation with no browser session is not an error.
    if let Ok(session_id) = state.browser.agent_tab_session_id(conversation_id) {
        state.browser.set_session_security_level(&session_id, level);
    }
    Ok(())
}

/// The conversation database beside this run's application-data root.
fn plan_store(
    app_data_path: &str,
) -> Result<std::sync::Arc<crate::conversation_store::ConversationStore>, String> {
    if app_data_path.trim().is_empty() {
        return Err("This run has no application data directory, so it cannot reach the plan".into());
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
///
/// Writing is gated on plan mode because the document exists to be reviewed
/// before implementation starts; reading is not, so the approved plan is still
/// retrievable in the mode the approval moved the conversation into.
pub(crate) fn run_plan_tool(
    request: &RunModelRequest,
    call: ToolCall,
    state: &AppState,
) -> ToolExecution {
    if request.subagent_depth > 0 {
        return failed_tool_execution(call, "plan cannot be used in agent contexts".into());
    }
    let action = text_argument(&call.input, "action").unwrap_or_default().trim().to_owned();
    let store = match plan_store(&request.app_data_path) {
        Ok(store) => store,
        Err(error) => return failed_tool_execution(call, error),
    };
    match action.as_str() {
        "write" => {
            if request.effective_security_level() != SecurityLevel::Plan {
                return failed_tool_execution(
                    call,
                    "The plan tool is only available in plan mode.".into(),
                );
            }
            let content = text_argument(&call.input, "content").unwrap_or_default().trim().to_owned();
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
                // A rewrite is a new draft even after a rejection: the user is
                // being asked again, about different text.
                status: PlanStatus::Draft,
                created_at: existing.map(|plan| plan.created_at).unwrap_or_else(|| now.clone()),
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

/// Presents the written plan for approval and, on approval, leaves plan mode.
///
/// The call blocks on the card: the model asked a question whose answer decides
/// what it may do next, so returning before the user answers would leave it
/// guessing at its own permissions.
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
    if request.effective_security_level() != SecurityLevel::Plan {
        return failed_tool_execution(
            call,
            "You are not in plan mode. To enter plan mode, call the enter_plan_mode tool first. If your plan was already approved, continue with implementation.".into(),
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

    let card = PendingToolPrompt {
        // Minted by the registry; see `ToolPromptRegistry::ask_answer`.
        prompt_id: String::new(),
        tool_name: EXIT_PLAN_MODE_TOOL.to_owned(),
        kind: PromptKind::PlanExit,
        label: "计划已就绪".to_owned(),
        summary: plan_title(&plan.markdown),
        risk_level: RiskLevel::Low.label_zh().to_owned(),
        reason: "模型已写好计划，等待你决定是否开始实施".to_owned(),
        requester: None,
        source_agent: None,
        source_call_id: None,
        allow_always_offered: false,
        mandatory: true,
    };
    let answer = match ask_plan_card(request, state, card) {
        Ok(answer) => answer,
        Err(error) => return failed_tool_execution(call, error),
    };

    let now = chrono::Utc::now().to_rfc3339();
    if !answer.decision.allows() {
        if let Err(error) =
            store.set_conversation_plan_status(&request.conversation_id, PlanStatus::Rejected, &now)
        {
            return failed_tool_execution(call, error);
        }
        publish_plan(
            state,
            &request.conversation_id,
            Some(ConversationPlan {
                status: PlanStatus::Rejected,
                updated_at: now,
                ..plan
            }),
        );
        // Success: the call did run and produced the answer the model asked
        // for. A failed result would read as "the tool broke", not "not yet".
        let feedback = answer
            .feedback
            .unwrap_or_else(|| "(no reason given)".to_owned());
        return succeeded(
            call,
            format!(
                "The user does not want to proceed with this plan yet and chose to stay in plan mode. The user said:\n{feedback}\n\nRevise the plan with the plan tool, then call exit_plan_mode again."
            ),
        );
    }

    // "Always" is the accept-edits answer and "once" the manual-approval one:
    // the two buttons on the card are two destinations, not two strengths.
    let (level, mode) = match answer.decision {
        ToolPromptDecision::AllowAlways => (SecurityLevel::AllowEdits, "accept edits"),
        _ => (SecurityLevel::RequestApproval, "manual approval"),
    };
    if let Err(error) = host_set_security_level(
        state,
        &request.app_data_path,
        &request.conversation_id,
        level,
    ) {
        return failed_tool_execution(call, error);
    }
    if let Err(error) =
        store.set_conversation_plan_status(&request.conversation_id, PlanStatus::Approved, &now)
    {
        return failed_tool_execution(call, error);
    }
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
    succeeded(
        call,
        format!(
            "User has approved your plan. You can now start coding. Start with updating your todo list if applicable.\n\nPermission mode is now {mode}. The plan stays available through the plan tool's read action.\n\n## Approved Plan:\n{markdown}"
        ),
    )
}

/// Asks the user to move the conversation into plan mode.
pub(crate) fn run_enter_plan_mode_tool(
    request: &RunModelRequest,
    call: ToolCall,
    state: &AppState,
) -> ToolExecution {
    if request.subagent_depth > 0 {
        return failed_tool_execution(
            call,
            "enter_plan_mode cannot be used in agent contexts".into(),
        );
    }
    if request.effective_security_level() == SecurityLevel::Plan {
        return failed_tool_execution(call, "You are already in plan mode.".into());
    }
    let card = PendingToolPrompt {
        prompt_id: String::new(),
        tool_name: ENTER_PLAN_MODE_TOOL.to_owned(),
        kind: PromptKind::PlanEnter,
        label: "进入计划模式".to_owned(),
        summary: "模型希望先探索代码并撰写计划，经你批准后再实施".to_owned(),
        risk_level: RiskLevel::Low.label_zh().to_owned(),
        reason: "模型希望先探索代码并撰写计划，经你批准后再实施".to_owned(),
        requester: None,
        source_agent: None,
        source_call_id: None,
        allow_always_offered: false,
        mandatory: true,
    };
    let answer = match ask_plan_card(request, state, card) {
        Ok(answer) => answer,
        Err(error) => return failed_tool_execution(call, error),
    };
    if !answer.decision.allows() {
        return succeeded(
            call,
            "The user declined to enter plan mode and wants you to start implementing now.".into(),
        );
    }
    if let Err(error) = host_set_security_level(
        state,
        &request.app_data_path,
        &request.conversation_id,
        SecurityLevel::Plan,
    ) {
        return failed_tool_execution(call, error);
    }
    succeeded(
        call,
        "Entered plan mode. You should now focus on exploring the codebase and designing an implementation approach.\n\nIn plan mode, you should:\n1. Thoroughly explore the codebase to understand existing patterns\n2. Identify similar features and architectural approaches\n3. Consider multiple approaches and their trade-offs\n4. Use ask_user if you need to clarify the approach\n5. Write the plan with the plan tool\n6. When ready, use exit_plan_mode to present your plan for approval\n\nRemember: DO NOT write or edit any files yet. This is a read-only exploration and planning phase."
            .to_owned(),
    )
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

    /// The whole availability rule, level by level. Plan mode is the only level
    /// that can write a plan or leave the mode, every other level offers the way
    /// in, and a child agent gets none of the three at any level.
    #[test]
    fn every_level_derives_its_own_plan_tools_and_children_derive_none() {
        for level in SecurityLevel::ALL {
            let expected: &[&str] = match level {
                SecurityLevel::Plan => &[PLAN_TOOL, EXIT_PLAN_MODE_TOOL],
                _ => &[ENTER_PLAN_MODE_TOOL],
            };
            assert_eq!(derived_tools(level, 0), expected, "{level:?} at depth 0");
            assert!(derived_tools(level, 1).is_empty(), "{level:?} at depth 1");
        }
        assert!(SecurityLevel::ALL
            .iter()
            .flat_map(|level| derived_tools(*level, 0))
            .all(|name| is_plan_mode_tool_name(name)));
    }

    /// The switch has to land in all three places at once: the running turn's
    /// cell, the database, and the document snapshot the run path reads.
    #[test]
    fn a_host_switch_moves_the_live_cell_the_database_and_the_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let app_data = directory.path();
        let document = crate::catalog::default_document(app_data);
        let anchor = app_data.join("document.v1.json");
        let state = AppState::default();
        state.document_store.acquire_process_authority(&anchor).unwrap();
        state.document_store.commit(&anchor, document.clone()).unwrap();
        let store = crate::conversations::store(&anchor).unwrap();
        let workspace = &document.workspaces[0];
        let conversation = &workspace.conversations[0];
        store.put_conversation(&workspace.id, conversation).unwrap();

        let cell = state.live_security_level_for_run(&conversation.id, SecurityLevel::Plan);

        host_set_security_level(
            &state,
            &app_data.to_string_lossy(),
            &conversation.id,
            SecurityLevel::AllowEdits,
        )
        .unwrap();

        assert_eq!(cell.get(), SecurityLevel::AllowEdits);
        assert_eq!(
            store
                .conversation(&conversation.id)
                .unwrap()
                .unwrap()
                .settings
                .security_level,
            SecurityLevel::AllowEdits
        );
        let snapshot = state.document_store.current_snapshot(&anchor).unwrap();
        assert_eq!(
            snapshot.workspaces[0].conversations[0]
                .settings
                .security_level,
            SecurityLevel::AllowEdits
        );
    }

    /// Nothing registers a cell outside a run, and the level still has to move:
    /// the persisted setting is the whole truth when no turn is in flight.
    #[test]
    fn a_switch_without_a_running_turn_still_persists() {
        let directory = tempfile::tempdir().unwrap();
        let app_data = directory.path();
        let document = crate::catalog::default_document(app_data);
        let anchor = app_data.join("document.v1.json");
        let state = AppState::default();
        state.document_store.acquire_process_authority(&anchor).unwrap();
        state.document_store.commit(&anchor, document.clone()).unwrap();
        let store = crate::conversations::store(&anchor).unwrap();
        let workspace = &document.workspaces[0];
        let conversation = &workspace.conversations[0];
        store.put_conversation(&workspace.id, conversation).unwrap();

        host_set_security_level(
            &state,
            &app_data.to_string_lossy(),
            &conversation.id,
            SecurityLevel::Plan,
        )
        .unwrap();

        assert!(state.live_security_level(&conversation.id).is_none());
        assert_eq!(
            store
                .conversation(&conversation.id)
                .unwrap()
                .unwrap()
                .settings
                .security_level,
            SecurityLevel::Plan
        );
    }
}
