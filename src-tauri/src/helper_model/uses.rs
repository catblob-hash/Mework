//! The helper model's uses.
//!
//! Titles: when a conversation that has never had a title settled starts a
//! request, the chosen user message becomes its title at once, and the
//! model's title replaces it when ready. The message is the last context
//! item if that is a user message, else the first user message; with no user
//! message there is no title. A generated title, or a rename by the user,
//! settles the title for good. The model reads the whole message, attached
//! files and images included, as the main model does (cut to the service's
//! input limit; see `local_model::service`).
//!
//! Subagent titles ride the same switch: each `agent_spawn` a top-level run
//! executes gets a title made from its `prompt` alone — what the model asked
//! of the child, not the child's opening message, which a role's template may
//! wrap around it (`{input}`). The child's task row shows it as its subtitle
//! in place of the prompt's excerpt.
//!
//! Shell explanations: each shell command a top-level run executes gets a
//! one-line description, stored beside (not in) its tool card, so the card's
//! attested payload is untouched. A subagent's title is stored the same way,
//! beside the `agent_spawn` card.
//!
//! Error explanations: each tool call or shell command of a top-level run
//! that fails gets a few words on why, made from the error it returned and
//! stored beside its card in a table of its own (a failed command keeps its
//! description too). The card's title shows the error's first line until
//! the reason replaces it.
//!
//! Subagents: with "also for subagents" on, command and error explanations
//! reach subagents and workflow steps too, and each workflow step gets a
//! title made from its prompt, beside its step card. Their requests take the
//! background lane: the conversation's own go first, and a full queue
//! refuses theirs to make room.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use local_model::prompts::Task;
use local_model::service::{Lane, Message, PictureSource};

use super::prompt_for;
use crate::file_attachments::FileAttachmentStore;
use crate::image_attachments::ImageAttachmentStore;
use crate::model::{AppDocument, ContextItem, FileAttachment, GlobalSettings, ImageAttachment};
use crate::push_events::AppPushEvent;
use crate::state::AppState;

/// The characters of a placeholder title, like the renderer's own excerpt.
const PLACEHOLDER_CHARS: usize = 32;

/// A user message a title can be made from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Source<'a> {
    pub text: &'a str,
    pub images: &'a [ImageAttachment],
    pub files: &'a [FileAttachment],
}

impl Source<'_> {
    /// The placeholder title: an excerpt of the text, or the first
    /// attachment's name when there is none.
    fn placeholder(&self) -> String {
        let name = self.files.first().map(|file| file.name.as_str()).or(self.images.first().map(|image| image.name.as_str()));
        let text = if self.text.trim().is_empty() { name.unwrap_or_default() } else { self.text };
        let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
        flat.chars().take(PLACEHOLDER_CHARS).collect()
    }
}

fn is_user_message(context: &ContextItem) -> Option<Source<'_>> {
    match context {
        ContextItem::User { id, content, images, files, .. }
            if !id.starts_with(crate::wire_history::HOST_TASK_DELIVERY_CONTEXT_PREFIX)
                && (!content.trim().is_empty() || !images.is_empty() || !files.is_empty()) =>
        {
            Some(Source { text: content, images, files })
        }
        _ => None,
    }
}

/// The message a title is made from (see the module doc).
pub(crate) fn title_source(contexts: &[ContextItem]) -> Option<Source<'_>> {
    if let Some(source) = contexts.last().and_then(is_user_message) {
        return Some(source);
    }
    contexts.iter().find_map(is_user_message)
}

/// An attached image, decoded at the size the helper model reads it.
struct StoredImage {
    store: ImageAttachmentStore,
    image: ImageAttachment,
}

impl PictureSource for StoredImage {
    fn size(&self) -> (usize, usize) {
        (self.image.width as usize, self.image.height as usize)
    }

    fn render(&self, width: usize, height: usize) -> Result<Vec<u8>, String> {
        self.store.rgb(&self.image, width as u32, height as u32)
    }
}

/// The message as the main model reads it: each file as its
/// `<attached_file>` element (a file whose content is gone is left out).
fn message(app_data_path: &Path, text: String, images: Vec<ImageAttachment>, files: Vec<FileAttachment>) -> Message {
    let file_store = FileAttachmentStore::new(app_data_path);
    let files = files
        .iter()
        .filter_map(|file| match file_store.model_text(file) {
            Ok(text) => Some(crate::file_attachments::render_for_model(file, &text)),
            Err(error) => {
                eprintln!("本地模型无法读取附件 {}：{error}", file.name);
                None
            }
        })
        .collect();
    let image_store = ImageAttachmentStore::new(app_data_path);
    let images = images
        .into_iter()
        .map(|image| Box::new(StoredImage { store: image_store.clone(), image }) as Box<dyn PictureSource>)
        .collect();
    Message { text, files, images }
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
    let Some(source) = title_source(contexts) else { return };
    let placeholder = source.placeholder();
    let (text, images, files) = (source.text.to_string(), source.images.to_vec(), source.files.to_vec());
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
    if let Err(error) = write_title(state, app_data_path, conversation_id, &placeholder, false) {
        eprintln!("无法写入占位标题：{error}");
    }
    let prompt = prompt_for(settings, Task::Title);
    let state = state.clone();
    let app_data_path = app_data_path.to_path_buf();
    let conversation_id = conversation_id.to_string();
    // Reading the attachments (and decoding images) takes a moment; not on the run's thread.
    let spawned = std::thread::Builder::new().name("mework-local-model-title".into()).spawn({
        let (state, conversation_id) = (state.clone(), conversation_id.clone());
        move || {
            let message = message(&app_data_path, text, images, files);
            service.title(
                &prompt,
                &message,
                Lane::Foreground,
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
    });
    if let Err(error) = spawned {
        state.helper_model.titles_in_flight.lock().expect("titles").remove(&conversation_id);
        eprintln!("无法启动标题生成：{error}");
    }
}

/// Stores `text` beside the tool card `context_id` (as the reason it failed
/// when `error`) and tells the renderer, which finds it by the card's id or,
/// while the call runs, by `call_id`.
fn explain_card(
    state: &AppState,
    anchor: &Path,
    conversation_id: String,
    context_id: String,
    call_id: String,
    text: String,
    error: bool,
) {
    let stored = crate::conversations::store(anchor).and_then(|store| {
        if error {
            store.put_tool_error_explanation(&conversation_id, &context_id, &text)
        } else {
            store.put_tool_explanation(&conversation_id, &context_id, &text)
        }
    });
    if let Err(error) = stored {
        eprintln!("无法保存工具卡说明：{error}");
    }
    state.push_events.publish(AppPushEvent::ToolExplained { conversation_id, context_id, call_id, text, error });
}

/// A subagent's requests wait behind the conversation's own.
fn lane_of(request: &crate::model::RunModelRequest) -> Lane {
    if request.subagent_depth > 0 {
        Lane::Background
    } else {
        Lane::Foreground
    }
}

/// The document whose settings are in force, when the uses reach `request`:
/// always for a top-level run, for a subagent's only with "also for
/// subagents" on.
fn settings_for(state: &AppState, request: &crate::model::RunModelRequest) -> Option<Arc<AppDocument>> {
    let document = state.document_store.current_snapshot(&anchor(Path::new(&request.app_data_path))).ok()?;
    (request.subagent_depth == 0 || document.global_settings.appearance.local_model.subagents).then_some(document)
}

/// Called as the run loop starts executing any tool call; acts on shell
/// commands and `agent_spawn` calls (of subagents too, see the module doc).
pub(crate) fn on_tool_started(
    state: &AppState,
    request: &crate::model::RunModelRequest,
    round: usize,
    call_id: &str,
    tool_name: &str,
    input: &crate::model::JsonObject,
) {
    if request.app_data_path.is_empty() || !state.helper_model.is_ready() {
        return;
    }
    let app_data_path = Path::new(&request.app_data_path);
    let lane = lane_of(request);
    if tool_name == "agent_spawn" {
        let Some(task) = input.get("prompt").and_then(|value| value.as_str()) else { return };
        let Some(document) = settings_for(state, request) else { return };
        let context_id = crate::api::tool_context_id(&request.conversation_id, &request.request_id, round, call_id);
        let ids = [context_id.as_str()];
        on_agent_spawned(state, app_data_path, &request.conversation_id, &ids, call_id, task, lane, &document.global_settings);
        return;
    }
    let Some(kind) = crate::shell_backend::ShellBackend::of_tool(tool_name) else { return };
    let Some(command) = input.get("command").and_then(|value| value.as_str()) else { return };
    let Some(document) = settings_for(state, request) else { return };
    let context_id = crate::api::tool_context_id(&request.conversation_id, &request.request_id, round, call_id);
    on_shell_started(
        state,
        app_data_path,
        &request.conversation_id,
        &context_id,
        call_id,
        kind.display_name(),
        command,
        lane,
        &document.global_settings,
    );
}

/// Called as a workflow, which only a top-level run can start, spawns a step
/// whose card is `step_call_id` and whose task is `prompt`. With titles and
/// "also for subagents" on, the step gets a title like a spawned subagent's.
/// The card is announced under an id derived from the step's call id (inside
/// the workflow's card, with an empty run scope) and saved under the call id
/// itself, so the title goes beside both.
pub(crate) fn on_workflow_step_started(
    state: &AppState,
    parent: &crate::model::RunModelRequest,
    round: usize,
    step_call_id: &str,
    prompt: &str,
) {
    if parent.app_data_path.is_empty() || !state.helper_model.is_ready() {
        return;
    }
    let app_data_path = Path::new(&parent.app_data_path);
    let Ok(document) = state.document_store.current_snapshot(&anchor(app_data_path)) else { return };
    let settings = &document.global_settings;
    if !settings.appearance.local_model.subagents {
        return;
    }
    let ids = step_card_ids(&parent.conversation_id, round, step_call_id);
    let ids = [ids[0].as_str(), ids[1].as_str()];
    on_agent_spawned(state, app_data_path, &parent.conversation_id, &ids, step_call_id, prompt, Lane::Background, settings);
}

/// A workflow step card's ids: while it runs, as its announcement inside
/// the workflow's card is stamped; once saved, its call id.
fn step_card_ids(conversation_id: &str, round: usize, step_call_id: &str) -> [String; 2] {
    [crate::api::tool_context_id(conversation_id, "", round, step_call_id), step_call_id.to_string()]
}

/// Called when a run starts executing a shell tool call. `shell` is the
/// shell's display name ("Bash", "zsh", "PowerShell").
#[allow(clippy::too_many_arguments)]
pub(crate) fn on_shell_started(
    state: &AppState,
    app_data_path: &Path,
    conversation_id: &str,
    context_id: &str,
    call_id: &str,
    shell: &str,
    command: &str,
    lane: Lane,
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
        lane,
        Box::new(move |result| match result {
            Ok(Some(text)) => {
                // The command's task row reads the same summary as its subtitle.
                state.shell_tasks.explain(&conversation_id, &call_id, &text);
                explain_card(&state, &anchor, conversation_id, context_id, call_id, text, false);
            }
            Ok(None) => {}
            Err(error) => eprintln!("本地模型解释命令失败：{error}"),
        }),
    );
}

/// Called when a run starts executing an `agent_spawn` call (or a workflow
/// spawns a step) whose task is `task`. The title goes beside the card,
/// under each of `context_ids`, where the child's task row reads it as its
/// subtitle.
#[allow(clippy::too_many_arguments)]
pub(crate) fn on_agent_spawned(
    state: &AppState,
    app_data_path: &Path,
    conversation_id: &str,
    context_ids: &[&str],
    call_id: &str,
    task: &str,
    lane: Lane,
    settings: &GlobalSettings,
) {
    if !settings.appearance.local_model.titles || task.trim().is_empty() || !state.helper_model.is_ready() {
        return;
    }
    let Ok(service) = state.helper_model.service() else { return };
    let prompt = prompt_for(settings, Task::Title);
    let state = state.clone();
    let anchor = anchor(app_data_path);
    let conversation_id = conversation_id.to_string();
    let context_ids: Vec<String> = context_ids.iter().map(|id| id.to_string()).collect();
    let call_id = call_id.to_string();
    // Text alone: no images to decode, so this need not leave the run's thread.
    let message = Message { text: task.to_string(), ..Message::default() };
    service.title(
        &prompt,
        &message,
        lane,
        Box::new(move |result| match result {
            Ok(Some(title)) => {
                for context_id in context_ids {
                    explain_card(&state, &anchor, conversation_id.clone(), context_id, call_id.clone(), title.clone(), false);
                }
            }
            Ok(None) => {}
            Err(error) => eprintln!("本地模型生成子代理标题失败：{error}"),
        }),
    );
}

/// What a failed call is called in the error explanation's request: the
/// shell's name for a command, the tool's own name for an MCP tool, else the
/// wire name.
fn failed_tool_tag(tool_name: &str) -> String {
    if let Some(shell) = crate::shell_backend::ShellBackend::of_tool(tool_name) {
        return shell.display_name().to_lowercase();
    }
    let mut parts = tool_name.split("__");
    match (parts.next(), parts.next(), parts.next()) {
        (Some("mcp"), Some(_server), Some(tool)) if !tool.is_empty() => tool.to_string(),
        _ => tool_name.to_string(),
    }
}

/// Called once a run's tool call has failed with `error` (what the model
/// was told), its card being `context_id`.
pub(crate) fn on_tool_failed(
    state: &AppState,
    request: &crate::model::RunModelRequest,
    call_id: &str,
    tool_name: &str,
    error: &str,
    context_id: &str,
) {
    if request.app_data_path.is_empty() || error.trim().is_empty() || !state.helper_model.is_ready() {
        return;
    }
    let Some(document) = settings_for(state, request) else { return };
    let settings = &document.global_settings;
    if !settings.appearance.local_model.error_explanations {
        return;
    }
    let Ok(service) = state.helper_model.service() else { return };
    let prompt = prompt_for(settings, Task::Error);
    let state = state.clone();
    let anchor = anchor(Path::new(&request.app_data_path));
    let (conversation_id, context_id, call_id) =
        (request.conversation_id.clone(), context_id.to_string(), call_id.to_string());
    service.explain_error(
        &prompt,
        &failed_tool_tag(tool_name),
        error,
        lane_of(request),
        Box::new(move |result| match result {
            Ok(Some(text)) => explain_card(&state, &anchor, conversation_id, context_id, call_id, text, true),
            Ok(None) => {}
            Err(error) => eprintln!("本地模型解释错误失败：{error}"),
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

    fn text_of(contexts: &[ContextItem]) -> Option<&str> {
        title_source(contexts).map(|source| source.text)
    }

    #[test]
    fn picks_the_title_source() {
        assert_eq!(text_of(&[user("u1", "first"), assistant("x"), user("u2", "last")]), Some("last"));
        assert_eq!(text_of(&[user("u1", "first"), assistant("x")]), Some("first"));
        assert_eq!(text_of(&[assistant("x")]), None);
        assert_eq!(text_of(&[]), None);
        // Blank messages are not messages.
        assert_eq!(text_of(&[user("u1", "only"), user("u2", "  ")]), Some("only"));
    }

    #[test]
    fn a_message_of_attachments_alone_is_a_source() {
        let image: ContextItem = serde_json::from_value(serde_json::json!({
            "kind": "user", "id": "u2", "content": " ", "createdAt": "2026-09-26T00:00:00Z",
            "images": [{
                "id": "a".repeat(64), "name": "screenshot.png", "mime": "image/png",
                "width": 1920, "height": 1080, "bytes": 1000
            }]
        }))
        .unwrap();
        let contexts = [user("u1", "first"), image];
        let source = title_source(&contexts).unwrap();
        assert_eq!(source.images.len(), 1);
        assert_eq!(source.placeholder(), "screenshot.png");
    }

    #[test]
    fn a_step_title_goes_beside_the_step_card_as_the_renderer_knows_it() {
        use crate::model::ModelStreamEvent;
        let announced = crate::api::stamp_announced_context_id(
            ModelStreamEvent::SubagentEvent {
                round: 3,
                call_id: "workflow-call".into(),
                event: Box::new(ModelStreamEvent::ToolCallAnnounced {
                    round: 3,
                    call_id: "step-1".into(),
                    tool_name: crate::workflow::WORKFLOW_STEP_TOOL.into(),
                    context_id: String::new(),
                }),
            },
            "conv",
            "request-1",
        );
        let ModelStreamEvent::SubagentEvent { event, .. } = announced else { panic!("still nested") };
        let ModelStreamEvent::ToolCallAnnounced { context_id, .. } = *event else { panic!("still an announcement") };
        assert_eq!(step_card_ids("conv", 3, "step-1"), [context_id, "step-1".to_string()]);
    }

    #[test]
    fn names_the_failed_tool() {
        assert_eq!(failed_tool_tag("edit"), "edit");
        assert_eq!(failed_tool_tag("mcp__github_0123456789__create_issue__ab12"), "create_issue");
        assert_eq!(failed_tool_tag("mcp__broken"), "mcp__broken");
    }

    #[test]
    fn placeholder_is_a_flat_excerpt() {
        let source = |text| Source { text, images: &[], files: &[] };
        assert_eq!(source("fix\n  the   build").placeholder(), "fix the build");
        assert_eq!(source(&"字".repeat(40)).placeholder().chars().count(), 32);
    }
}
