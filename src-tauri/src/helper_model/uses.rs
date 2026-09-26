//! The helper model's two uses.
//!
//! Titles: when a conversation that has never had a title settled starts a
//! request, the chosen user message becomes its title at once, and the
//! model's title replaces it when ready. The message is the last context
//! item if that is a user message, else the first user message; with no user
//! message there is no title. A generated title, or a rename by the user,
//! settles the title for good.
//!
//! Shell explanations: each shell command a top-level run executes gets a
//! one-line description, stored beside (not in) its tool card, so the card's
//! attested payload is untouched.

use std::path::{Path, PathBuf};

use local_model::prompts::Task;

use super::prompt_for;
use crate::model::{ContextItem, GlobalSettings};
use crate::push_events::AppPushEvent;
use crate::state::AppState;

/// The characters of a placeholder title, like the renderer's own excerpt.
const PLACEHOLDER_CHARS: usize = 32;

fn is_user_message(context: &ContextItem) -> Option<&str> {
    match context {
        ContextItem::User { id, content, .. }
            if !id.starts_with(crate::wire_history::HOST_TASK_DELIVERY_CONTEXT_PREFIX) && !content.trim().is_empty() =>
        {
            Some(content.as_str())
        }
        _ => None,
    }
}

/// The message a title is made from (see the module doc).
pub(crate) fn title_source(contexts: &[ContextItem]) -> Option<&str> {
    if let Some(text) = contexts.last().and_then(is_user_message) {
        return Some(text);
    }
    contexts.iter().find_map(is_user_message)
}

fn placeholder(message: &str) -> String {
    let flat: String = message.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.chars().take(PLACEHOLDER_CHARS).collect()
}

fn anchor(app_data_path: &Path) -> PathBuf {
    app_data_path.join("document.v1.json")
}

/// Writes `title` (and whether it is settled) on the host's initiative.
fn write_title(state: &AppState, app_data_path: &Path, conversation_id: &str, title: &str, settle: bool) -> Result<bool, String> {
    let anchor = anchor(app_data_path);
    let guard = state.storage_lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let store = crate::conversations::store(&anchor)?;
    if store.title_settled(conversation_id)? {
        return Ok(false);
    }
    let workspaces = store.conversation_workspaces()?;
    let Some(workspace_id) = workspaces.get(conversation_id).cloned() else { return Ok(false) };
    let Some(mut conversation) = store.conversation(conversation_id)? else { return Ok(false) };
    let replaced = std::mem::replace(&mut conversation.title, title.to_string());
    if replaced != title {
        store.put_conversation_metadata(&workspace_id, &conversation)?;
    }
    if settle {
        store.set_title_settled(conversation_id, true)?;
    }
    let stored = store.conversation(conversation_id)?;
    crate::conversations::sync_snapshot(state, &anchor, &workspace_id, conversation_id, stored)?;
    drop(guard);
    state.helper_model.remember_title(conversation_id, replaced, title.to_string());
    state.push_events.publish(AppPushEvent::ConversationTitleChanged {
        conversation_id: conversation_id.to_string(),
        title: title.to_string(),
        settled: settle,
    });
    Ok(true)
}

/// Called when a top-level run for `conversation_id` has started.
pub(crate) fn on_run_started(state: &AppState, app_data_path: &Path, conversation_id: &str, contexts: &[ContextItem], settings: &GlobalSettings) {
    if !settings.appearance.local_model.titles || !state.helper_model.is_ready() {
        return;
    }
    let Some(message) = title_source(contexts).map(str::to_string) else { return };
    match crate::conversations::store(&anchor(app_data_path)).and_then(|store| store.title_settled(conversation_id)) {
        Ok(false) => {}
        _ => return,
    }
    if !state.helper_model.titles_in_flight.lock().expect("titles").insert(conversation_id.to_string()) {
        return;
    }
    let service = match state.helper_model.service() {
        Ok(service) => service,
        Err(_) => {
            state.helper_model.titles_in_flight.lock().expect("titles").remove(conversation_id);
            return;
        }
    };
    if let Err(error) = write_title(state, app_data_path, conversation_id, &placeholder(&message), false) {
        eprintln!("无法写入占位标题：{error}");
    }
    let prompt = prompt_for(settings, Task::Title);
    let state = state.clone();
    let app_data_path = app_data_path.to_path_buf();
    let conversation_id = conversation_id.to_string();
    service.title(
        &prompt,
        &message,
        Box::new(move |result| {
            state.helper_model.titles_in_flight.lock().expect("titles").remove(&conversation_id);
            match result {
                Ok(Some(title)) => {
                    if let Err(error) = write_title(&state, &app_data_path, &conversation_id, &title, true) {
                        eprintln!("无法写入生成的标题：{error}");
                    }
                }
                // Left unsettled: the next request tries again.
                Ok(None) => {}
                Err(error) => eprintln!("本地模型生成标题失败：{error}"),
            }
        }),
    );
}

/// Called as the run loop starts executing any tool call; acts on shell
/// commands of top-level runs.
pub(crate) fn on_tool_started(
    state: &AppState,
    request: &crate::model::RunModelRequest,
    round: usize,
    call_id: &str,
    tool_name: &str,
    input: &crate::model::JsonObject,
) {
    if request.subagent_depth > 0 || request.app_data_path.is_empty() || !state.helper_model.is_ready() {
        return;
    }
    let Some(kind) = crate::shell_backend::ShellBackend::of_tool(tool_name) else { return };
    let Some(command) = input.get("command").and_then(|value| value.as_str()) else { return };
    let app_data_path = Path::new(&request.app_data_path);
    let Ok(document) = state.document_store.current_snapshot(&anchor(app_data_path)) else { return };
    let context_id = crate::api::tool_context_id(&request.conversation_id, &request.request_id, round, call_id);
    on_shell_started(
        state,
        app_data_path,
        &request.conversation_id,
        &context_id,
        call_id,
        kind.display_name(),
        command,
        &document.global_settings,
    );
}

/// Called when a top-level run starts executing a shell tool call. `shell`
/// is the shell's display name ("Bash", "zsh", "PowerShell").
#[allow(clippy::too_many_arguments)]
pub(crate) fn on_shell_started(
    state: &AppState,
    app_data_path: &Path,
    conversation_id: &str,
    context_id: &str,
    call_id: &str,
    shell: &str,
    command: &str,
    settings: &GlobalSettings,
) {
    if !settings.appearance.local_model.shell_explanations || command.trim().is_empty() || !state.helper_model.is_ready() {
        return;
    }
    let Ok(service) = state.helper_model.service() else { return };
    let prompt = prompt_for(settings, Task::Shell);
    let state = state.clone();
    let anchor = anchor(app_data_path);
    let (conversation_id, context_id, call_id) = (conversation_id.to_string(), context_id.to_string(), call_id.to_string());
    service.explain(
        &prompt,
        &shell.to_lowercase(),
        command,
        Box::new(move |result| match result {
            Ok(Some(text)) => {
                let stored = crate::conversations::store(&anchor)
                    .and_then(|store| store.put_tool_explanation(&conversation_id, &context_id, &text));
                if let Err(error) = stored {
                    eprintln!("无法保存命令说明：{error}");
                }
                state.push_events.publish(AppPushEvent::ToolExplained { conversation_id, context_id, call_id, text });
            }
            Ok(None) => {}
            Err(error) => eprintln!("本地模型解释命令失败：{error}"),
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(id: &str, content: &str) -> ContextItem {
        serde_json::from_value(serde_json::json!({
            "kind": "user", "id": id, "content": content, "createdAt": "2026-09-26T00:00:00Z"
        }))
        .unwrap()
    }

    fn assistant(content: &str) -> ContextItem {
        serde_json::from_value(serde_json::json!({
            "kind": "assistant", "id": "a1", "content": content, "createdAt": "2026-09-26T00:00:00Z"
        }))
        .unwrap()
    }

    #[test]
    fn picks_the_title_source() {
        assert_eq!(title_source(&[user("u1", "first"), assistant("x"), user("u2", "last")]), Some("last"));
        assert_eq!(title_source(&[user("u1", "first"), assistant("x")]), Some("first"));
        assert_eq!(title_source(&[assistant("x")]), None);
        assert_eq!(title_source(&[]), None);
        // Blank messages are not messages.
        assert_eq!(title_source(&[user("u1", "only"), user("u2", "  ")]), Some("only"));
    }

    #[test]
    fn placeholder_is_a_flat_excerpt() {
        assert_eq!(placeholder("fix\n  the   build"), "fix the build");
        assert_eq!(placeholder(&"字".repeat(40)).chars().count(), 32);
    }
}
