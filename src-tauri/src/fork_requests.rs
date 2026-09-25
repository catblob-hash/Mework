//! Model-raised conversation forks.
//!
//! `fork` is the one tool whose effect is another conversation. The call
//! itself never waits: it raises a request and returns a receipt that says so,
//! and the model is never told what became of it. The decision is the user's at
//! every security level, full access included — the request becomes a
//! non-blocking card in the renderer's tray, where it is approved or denied at
//! leisure. An answered card leaves a [`ForkDecisionRecord`] the source
//! conversation's task bar draws; the model never sees that either.
//!
//! The child is a full conversation, not a task, and always a fresh agent: it
//! inherits the parent's `settings` verbatim (so it holds exactly the parent's
//! permissions), its worktree and run target, and nothing else. The parent's
//! timeline stays with the parent; the child starts from its prompt alone.
//! Nesting is a renderer concept: the only durable link is
//! `parent_conversation_id`.
//!
//! Requests are process-local like approval cards: a restart forgets them, and
//! a card nobody answers expires after [`REQUEST_TTL`]. The decisions they
//! produce are not: those live in the conversation store.

use std::{
    collections::HashMap,
    path::Path,
    sync::Mutex,
    time::{Duration, Instant},
};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    model::{ContextItem, Conversation, JsonObject, RunModelRequest},
    prompt_profile::PromptKey,
    push_events::AppPushEvent,
    state::AppState,
};

pub const FORK_TOOL: &str = "fork";

/// How long an unanswered request stays open. Mirrors the approval-card
/// timeout: a card the user never saw must not create a conversation weeks
/// later.
const REQUEST_TTL: Duration = Duration::from_secs(30 * 60);
/// A model looping on `fork` must not fill the tray; a saturated conversation
/// must not block another conversation's request.
const MAX_PENDING_PER_CONVERSATION: usize = 16;
/// The prompt is the child's first user message; keep it within the size a
/// composer message may have.
const MAX_PROMPT_CHARS: usize = 32_768;
/// The composer's own rule for a fresh conversation's title.
const TITLE_CHARS: usize = 32;

/// What the renderer needs to draw one card. Mirrored field for field by the
/// renderer's `PendingForkRequest` and flattened into the `forkRequested` push
/// event, so there is one shape for the card however it arrives.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingForkRequest {
    pub fork_id: String,
    pub workspace_id: String,
    pub source_conversation_id: String,
    /// Display-escaped: a title can carry the same bidi tricks a command can.
    pub source_title: String,
    pub prompt: String,
    pub requested_at: String,
}

/// One answered request, as the conversation store keeps it.
///
/// Model-invisible by construction: `fork` returns before the user decides, so
/// no receipt, task list or context can carry this. It exists so the user can
/// see in the source conversation's task bar what they answered, and so an
/// approved fork stays clickable after a reload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkDecisionRecord {
    pub fork_id: String,
    pub workspace_id: String,
    pub source_conversation_id: String,
    /// The title [`fork_title`] gives the child, computed even when the request
    /// was declined so a refused row reads like the conversation it would have
    /// been.
    pub title: String,
    pub prompt: String,
    pub requested_at: String,
    pub decided_at: String,
    pub approved: bool,
    /// Set exactly when the decision created a child.
    pub child_conversation_id: Option<String>,
}

struct Entry {
    card: PendingForkRequest,
    opened_at: Instant,
}

#[derive(Default)]
pub struct ForkRequestRegistry {
    pending: Mutex<HashMap<String, Entry>>,
}

impl ForkRequestRegistry {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Opens a request under a fresh id and returns the card as minted. Expired
    /// requests are swept here, which is the only moment anything sweeps them.
    pub fn open(&self, card: PendingForkRequest) -> Result<PendingForkRequest, String> {
        let mut pending = self.lock();
        let now = Instant::now();
        pending.retain(|_, entry| now.duration_since(entry.opened_at) < REQUEST_TTL);
        if pending
            .values()
            .filter(|entry| entry.card.source_conversation_id == card.source_conversation_id)
            .count()
            >= MAX_PENDING_PER_CONVERSATION
        {
            return Err(
                "Too many fork requests are awaiting a decision; wait for the user to answer them"
                    .into(),
            );
        }
        let fork_id = loop {
            let candidate = uuid::Uuid::new_v4().to_string();
            if !pending.contains_key(&candidate) {
                break candidate;
            }
        };
        let card = PendingForkRequest { fork_id, ..card };
        pending.insert(
            card.fork_id.clone(),
            Entry {
                card: card.clone(),
                opened_at: now,
            },
        );
        Ok(card)
    }

    /// Removes and returns a request. Unknown ids are `None` rather than an
    /// error at this layer so a double click cannot act twice.
    pub fn take(&self, fork_id: &str) -> Option<PendingForkRequest> {
        let entry = self.lock().remove(fork_id)?;
        if entry.opened_at.elapsed() >= REQUEST_TTL {
            return None;
        }
        Some(entry.card)
    }

    /// Every open card, oldest first, for a renderer that just (re)loaded.
    pub fn pending_cards(&self) -> Vec<PendingForkRequest> {
        let pending = self.lock();
        let now = Instant::now();
        let mut entries = pending
            .values()
            .filter(|entry| now.duration_since(entry.opened_at) < REQUEST_TTL)
            .collect::<Vec<_>>();
        entries.sort_by(|a, b| a.opened_at.cmp(&b.opened_at));
        entries
            .into_iter()
            .map(|entry| entry.card.clone())
            .collect()
    }

    /// Drops every request a deleted source conversation raised and returns
    /// the cards so the caller can retract them from the tray.
    pub fn retract_for_conversation(&self, conversation_id: &str) -> Vec<PendingForkRequest> {
        let mut pending = self.lock();
        let retracted = pending
            .iter()
            .filter(|(_, entry)| entry.card.source_conversation_id == conversation_id)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        retracted
            .iter()
            .filter_map(|id| pending.remove(id))
            .map(|entry| entry.card)
            .collect()
    }
}

/// The one argument, validated. Separate from the tool body so the rules are
/// testable without a run request.
fn parse_fork_arguments(input: &JsonObject) -> Result<String, String> {
    let prompt = input
        .get("prompt")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())
        .ok_or_else(|| "fork requires a non-empty `prompt`".to_owned())?;
    if prompt.chars().count() > MAX_PROMPT_CHARS {
        return Err(format!(
            "fork `prompt` exceeds {MAX_PROMPT_CHARS} characters"
        ));
    }
    Ok(prompt.to_owned())
}

/// The `fork` tool body: validates the arguments, opens the request and hands
/// the card to the tray. It never creates anything itself — the decision is the
/// user's at every access level, full access included — so there is one
/// receipt, and it says that nothing further will be delivered, because nothing
/// will.
pub fn run_fork_tool(
    request: &RunModelRequest,
    state: &AppState,
    input: &JsonObject,
) -> Result<String, String> {
    if request.subagent_depth > 0 {
        return Err("Only the main agent can fork the conversation".into());
    }
    let prompt = parse_fork_arguments(input)?;
    if request.app_data_path.trim().is_empty() {
        return Err(
            "This run has no application data directory, so it cannot create a conversation".into(),
        );
    }
    let anchor = Path::new(&request.app_data_path).join("document.v1.json");
    let store = crate::conversations::store(&anchor)?;
    let source = store
        .conversation(&request.conversation_id)?
        .ok_or_else(|| {
            format!(
                "Conversation {} is not in the conversation store",
                request.conversation_id
            )
        })?;
    let card = state.fork_requests().open(PendingForkRequest {
        fork_id: String::new(),
        workspace_id: request.workspace_id.clone(),
        source_conversation_id: request.conversation_id.clone(),
        source_title: crate::tool_prompt::escape_display_text(&source.title),
        prompt,
        requested_at: Utc::now().to_rfc3339(),
    })?;

    state
        .push_events
        .publish(AppPushEvent::ForkRequested { request: card });
    Ok(request
        .prompt_profile
        .text(PromptKey::ForkRequestSubmitted)
        .to_owned())
}

/// Answers a request on the user's behalf: performs it when approved, drops it
/// otherwise, records the decision either way and publishes the outcome so
/// every renderer surface takes the card down. Returns the child when one was
/// created.
pub fn resolve_fork_request(
    state: &AppState,
    anchor: &Path,
    fork_id: &str,
    approved: bool,
) -> Result<Option<Conversation>, String> {
    let card = state
        .fork_requests()
        .take(fork_id)
        .ok_or_else(|| "This fork request has ended or does not exist".to_owned())?;
    let child = if approved {
        Some(perform_fork(state, anchor, &card)?)
    } else {
        None
    };
    let record = ForkDecisionRecord {
        fork_id: card.fork_id.clone(),
        workspace_id: card.workspace_id.clone(),
        source_conversation_id: card.source_conversation_id.clone(),
        title: fork_title(&card.prompt),
        prompt: card.prompt.clone(),
        requested_at: card.requested_at.clone(),
        decided_at: Utc::now().to_rfc3339(),
        approved,
        child_conversation_id: child.as_ref().map(|child| child.id.clone()),
    };
    if let Err(error) =
        crate::conversations::store(anchor).and_then(|store| store.record_fork_decision(&record))
    {
        // An approved fork already committed its child. Failing here would
        // report the decision as not taken and invite a second request; the
        // event below still carries the record, so the task bar shows it until
        // the next reload.
        state
            .push_events
            .publish(AppPushEvent::DocumentWriteFailure {
                message: format!("分叉决定未能记录 / Fork decision was not recorded: {error}"),
            });
    }
    state.push_events.publish(AppPushEvent::ForkResolved {
        fork_id: card.fork_id,
        workspace_id: card.workspace_id,
        source_conversation_id: card.source_conversation_id,
        approved,
        child_conversation_id: child.as_ref().map(|child| child.id.clone()),
        decision: Some(record),
    });
    Ok(child)
}

/// Retracts every open request of a conversation that is being deleted. Nothing
/// is recorded: the user decided nothing, and the row would name a conversation
/// that no longer exists.
pub fn retract_requests_for_conversation(state: &AppState, conversation_id: &str) {
    for card in state
        .fork_requests()
        .retract_for_conversation(conversation_id)
    {
        state.push_events.publish(AppPushEvent::ForkResolved {
            fork_id: card.fork_id,
            workspace_id: card.workspace_id,
            source_conversation_id: card.source_conversation_id,
            approved: false,
            child_conversation_id: None,
            decision: None,
        });
    }
}

/// Creates the child conversation for one request.
///
/// The source is read from the conversation store, never from the renderer, and
/// is used only for the settings, worktree and run target the child inherits.
/// The prompt becomes the child's one user message, so the renderer can start
/// its run without composing anything.
pub fn perform_fork(
    state: &AppState,
    anchor: &Path,
    card: &PendingForkRequest,
) -> Result<Conversation, String> {
    let store = crate::conversations::store(anchor)?;
    let source = store
        .conversation(&card.source_conversation_id)?
        .ok_or_else(|| format!("源对话 {} 不存在", card.source_conversation_id))?;
    let child_id = format!("conv_{}", uuid::Uuid::new_v4());
    let now = Utc::now().to_rfc3339();

    let contexts = vec![ContextItem::User {
        id: format!("ctx_{}", uuid::Uuid::new_v4().simple()),
        content: card.prompt.clone(),
        images: Vec::new(),
        files: Vec::new(),
        created_at: now.clone(),
    }];

    let child = Conversation {
        id: child_id,
        title: fork_title(&card.prompt),
        created_at: now.clone(),
        updated_at: now,
        // The child holds exactly the parent's permissions: same level, same
        // tools, same prompt, same memory switches, same roles.
        settings: source.settings.clone(),
        contexts,
        queued_messages: Vec::new(),
        branches: Vec::new(),
        user_aborted_tasks: Vec::new(),
        worktrees: source.worktrees.clone(),
        run_target: source.run_target.clone(),
        // The child holds exactly the parent's grants, extra directories included.
        attached_workspaces: source.attached_workspaces.clone(),
        additional_directories: source.additional_directories.clone(),
        // The sidebar is two levels deep, so a fork of a child becomes that
        // child's sibling. The source's own pointer is already a root, so
        // borrowing it is the root without another walk.
        parent_conversation_id: source
            .parent_conversation_id
            .clone()
            .or_else(|| Some(source.id.clone())),
        // The child inherits the whole settings body, so it inherits which preset
        // that body came from.
        preset_id: source.preset_id.clone(),
        // Not inherited, unlike the preset. A template trace claims "this
        // timeline IS that template's message queue", and a fork's timeline is
        // one fresh prompt — never the queue itself.
        template_id: String::new(),
    };

    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    crate::conversations::create_with_fork_start(
        state,
        anchor,
        &card.workspace_id,
        &child,
        child.contexts.last().map(ContextItem::id),
    )
}

/// The composer's title rule: the first line of the first message, cut to 32
/// characters.
fn fork_title(prompt: &str) -> String {
    let first_line = prompt
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim();
    first_line.chars().take(TITLE_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{catalog::default_document, model::JsonObject};
    use serde_json::json;

    fn card(source: &str) -> PendingForkRequest {
        PendingForkRequest {
            fork_id: String::new(),
            workspace_id: "ws".into(),
            source_conversation_id: source.into(),
            source_title: "源对话".into(),
            prompt: "继续做 B".into(),
            requested_at: "2026-09-05T00:00:00Z".into(),
        }
    }

    #[test]
    fn a_request_is_minted_listed_and_taken_once() {
        let registry = ForkRequestRegistry::default();
        let opened = registry.open(card("conv_a")).unwrap();
        assert!(!opened.fork_id.is_empty());
        assert_eq!(registry.pending_cards(), vec![opened.clone()]);

        assert_eq!(registry.take(&opened.fork_id).unwrap(), opened);
        assert!(
            registry.take(&opened.fork_id).is_none(),
            "a card answers once"
        );
        assert!(registry.pending_cards().is_empty());
    }

    #[test]
    fn pending_cards_come_back_oldest_first() {
        let registry = ForkRequestRegistry::default();
        let first = registry.open(card("conv_a")).unwrap();
        std::thread::sleep(Duration::from_millis(2));
        let second = registry.open(card("conv_b")).unwrap();
        let listed = registry.pending_cards();
        assert_eq!(listed[0].fork_id, first.fork_id);
        assert_eq!(listed[1].fork_id, second.fork_id);
    }

    #[test]
    fn one_conversations_quota_does_not_block_another() {
        let registry = ForkRequestRegistry::default();
        for _ in 0..MAX_PENDING_PER_CONVERSATION {
            registry.open(card("conv_noisy")).unwrap();
        }
        assert!(registry.open(card("conv_noisy")).is_err());
        assert!(registry.open(card("conv_quiet")).is_ok());
    }

    #[test]
    fn retracting_a_conversation_drops_only_its_requests() {
        let registry = ForkRequestRegistry::default();
        let gone = registry.open(card("conv_a")).unwrap();
        let kept = registry.open(card("conv_b")).unwrap();
        let retracted = registry.retract_for_conversation("conv_a");
        assert_eq!(retracted, vec![gone]);
        assert_eq!(registry.pending_cards(), vec![kept]);
    }

    #[test]
    fn the_title_is_the_first_nonblank_line_cut_to_32_chars() {
        assert_eq!(fork_title("\n\n  hello world  \nsecond"), "hello world");
        let long = "字".repeat(50);
        assert_eq!(fork_title(&long).chars().count(), 32);
        assert_eq!(fork_title("   "), "");
    }

    /// Seeds a real store next to an anchor and returns the source conversation
    /// as the store holds it.
    fn seeded(directory: &Path) -> (AppState, std::path::PathBuf, Conversation) {
        let state = AppState::default();
        let document = default_document();
        let anchor = directory.join("document.v1.json");
        state
            .document_store
            .acquire_process_authority(&anchor)
            .unwrap();
        state
            .document_store
            .commit(&anchor, document.clone())
            .unwrap();
        let store = crate::conversations::store(&anchor).unwrap();
        let workspace = &document.workspaces[0];
        let source = &workspace.conversations[0];
        store.put_conversation(&workspace.id, source).unwrap();
        (
            state,
            anchor,
            store.conversation(&source.id).unwrap().unwrap(),
        )
    }

    #[test]
    fn performing_a_fork_starts_the_child_from_the_prompt_alone() {
        let directory = tempfile::tempdir().unwrap();
        let (state, anchor, source) = seeded(directory.path());
        let workspace_id = default_document().workspaces[0].id.clone();
        assert!(source.contexts.len() >= 2, "fixture must carry history");

        let request = PendingForkRequest {
            workspace_id,
            ..card(&source.id)
        };
        let child = perform_fork(&state, &anchor, &request).unwrap();

        assert_eq!(
            child.parent_conversation_id.as_deref(),
            Some(source.id.as_str())
        );
        assert_eq!(child.settings, source.settings);
        assert_eq!(child.title, "继续做 B");
        assert!(child.user_aborted_tasks.is_empty());
        // However much history the source carries, the child gets the prompt.
        assert_eq!(child.contexts.len(), 1);
        let ContextItem::User { content, .. } = &child.contexts[0] else {
            panic!("the prompt must be the child's only context");
        };
        assert_eq!(content, "继续做 B");

        // The child is in the store and in the snapshot, nested under its parent.
        let store = crate::conversations::store(&anchor).unwrap();
        let stored = store.conversation(&child.id).unwrap().unwrap();
        assert_eq!(
            stored.parent_conversation_id.as_deref(),
            Some(source.id.as_str())
        );
        let snapshot = state.document_store.current_snapshot(&anchor).unwrap();
        assert!(snapshot.workspaces[0]
            .conversations
            .iter()
            .any(|conversation| conversation.id == child.id));
    }

    #[test]
    fn a_fork_of_a_child_becomes_that_childs_sibling() {
        let directory = tempfile::tempdir().unwrap();
        let (state, anchor, source) = seeded(directory.path());
        let workspace_id = default_document().workspaces[0].id.clone();
        let request = PendingForkRequest {
            workspace_id: workspace_id.clone(),
            ..card(&source.id)
        };
        let child = perform_fork(&state, &anchor, &request).unwrap();

        let grandchild = perform_fork(
            &state,
            &anchor,
            &PendingForkRequest {
                workspace_id,
                source_conversation_id: child.id.clone(),
                ..card(&child.id)
            },
        )
        .unwrap();

        // Two levels, so the second fork hangs off the root rather than the child.
        assert_eq!(
            grandchild.parent_conversation_id.as_deref(),
            Some(source.id.as_str())
        );
    }

    #[test]
    fn the_arguments_are_validated_before_anything_is_opened() {
        let mut input = JsonObject::new();
        input.insert("prompt".into(), json!("   "));
        let error = parse_fork_arguments(&input).unwrap_err();
        assert!(error.contains("prompt"), "{error}");

        input.insert("prompt".into(), json!("  do it  "));
        assert_eq!(parse_fork_arguments(&input).unwrap(), "do it");

        input.insert("prompt".into(), json!("x".repeat(MAX_PROMPT_CHARS + 1)));
        assert!(parse_fork_arguments(&input).is_err());
    }

    /// A main-agent run at full access, pointed at a seeded store.
    fn fork_run_request(
        directory: &Path,
        workspace: &crate::model::Workspace,
        conversation_id: &str,
    ) -> RunModelRequest {
        RunModelRequest {
            provider: crate::model::ApiProvider {
                id: "p".into(),
                name: "P".into(),
                enabled: true,
                family: crate::model::ProviderFamily::OpenaiResponses,
                base_url: "http://127.0.0.1:1".into(),
                family_settings: Default::default(),
                endpoint_base_urls: Default::default(),
                notes: String::new(),
                models: Vec::new(),
                active_model_id: None,
            },
            web_search: Default::default(),
            native_search_call: false,
            web_search_enabled: false,
            native_fetch_call: None,
            run_environment: Default::default(),
            prompt_profile: Default::default(),
            global_memory_enabled: false,
            project_memory_enabled: false,
            skills: Vec::new(),
            added_skills: Vec::new(),
            mcp_tool_discovery: false,
            decision_parameter_modes: Default::default(),
            decision_miss_scoring: Default::default(),
            file_guard: Default::default(),
            deferred_tools: Vec::new(),
            model: crate::model::ModelProfile {
                id: "m".into(),
                name: String::new(),
                group: String::new(),
                context_window: None,
                max_output_tokens: None,
                capabilities: Default::default(),
                reasoning_content: Default::default(),
                prompt_cache: true,
            },
            reasoning_effort: Default::default(),
            conversation_id: conversation_id.to_owned(),
            workspace_id: workspace.id.clone(),
            memory_context_id: None,
            project_memory_context_id: None,
            agent_definition_binding: None,
            inherits_parent_model_memory: false,
            fork_model_binding: None,
            subagent_execution_mode_receipt: None,
            subagent_reserved_names: Vec::new(),
            memory_run_id: None,
            context_load_actor_name: None,
            workspace_path: workspace.path.clone(),
            additional_directories: Vec::new(),
            assembled_system_prompt: String::new(),
            enabled_tools: vec![FORK_TOOL.to_owned()],
            contexts: Vec::new(),
            ephemeral_contexts: Vec::new(),
            tools: Vec::new(),
            active_hooks: Vec::new(),
            security_level: crate::model::SecurityLevel::FullAccess,
            live_security_level: None,
            app_data_path: directory.to_string_lossy().into_owned(),
            mcp_servers: Vec::new(),
            mcp_bindings: Vec::new(),
            subagent_depth: 0,
            request_id: String::new(),
            subagent_name: None,
            subagent_call_id: None,
            wire_ledger_owner: None,
            agent_mailbox: Default::default(),
            steer_mailbox: Default::default(),
            task_cancel: crate::cancel::CancelSignal::default(),
            run_cancel: crate::cancel::CancelSignal::default(),
            output_schema: None,
            workspaces: Default::default(),
        }
    }

    /// Collects every push event the hub delivers while the test runs.
    fn collecting_events() -> (
        tauri::ipc::Channel<AppPushEvent>,
        std::sync::Arc<Mutex<Vec<serde_json::Value>>>,
    ) {
        let received = std::sync::Arc::new(Mutex::new(Vec::new()));
        let sink = received.clone();
        let channel = tauri::ipc::Channel::new(move |body| {
            let value = match body {
                tauri::ipc::InvokeResponseBody::Json(json) => serde_json::from_str(&json)?,
                tauri::ipc::InvokeResponseBody::Raw(bytes) => serde_json::to_value(bytes)?,
            };
            sink.lock().unwrap().push(value);
            Ok(())
        });
        (channel, received)
    }

    /// Full access delegates tool execution, not this decision: the card is the
    /// gate at every level, and the receipt is the same one every level returns.
    #[test]
    fn full_access_raises_a_card_instead_of_creating_the_child() {
        let directory = tempfile::tempdir().unwrap();
        let (state, anchor, source) = seeded(directory.path());
        let document = default_document();
        let (channel, events) = collecting_events();
        state.push_events.subscribe(channel);

        let request = fork_run_request(directory.path(), &document.workspaces[0], &source.id);
        assert_eq!(
            request.effective_security_level(),
            crate::model::SecurityLevel::FullAccess
        );
        let mut input = JsonObject::new();
        input.insert("prompt".into(), json!("继续做 B"));
        let receipt = run_fork_tool(&request, &state, &input).unwrap();

        // The one receipt, read from the profile the run carries rather than
        // spelled twice.
        assert_eq!(
            receipt,
            crate::prompt_profile::PromptProfile::builtin_english()
                .text(PromptKey::ForkRequestSubmitted)
        );

        let pending = state.fork_requests().pending_cards();
        assert_eq!(pending.len(), 1, "the card is waiting for the user");
        assert_eq!(pending[0].source_conversation_id, source.id);

        let published = events.lock().unwrap();
        assert_eq!(published.len(), 1, "one event: {published:?}");
        assert_eq!(published[0]["type"], "forkRequested");
        drop(published);

        // Nothing was created: no child intent, no decision.
        let store = crate::conversations::store(&anchor).unwrap();
        assert!(store.pending_fork_starts().unwrap().is_empty());
        assert!(store.fork_decisions(&source.id).unwrap().is_empty());
    }

    #[test]
    fn approving_records_the_decision_with_the_child_it_created() {
        let directory = tempfile::tempdir().unwrap();
        let (state, anchor, source) = seeded(directory.path());
        let document = default_document();
        let opened = state
            .fork_requests()
            .open(PendingForkRequest {
                workspace_id: document.workspaces[0].id.clone(),
                ..card(&source.id)
            })
            .unwrap();
        let (channel, events) = collecting_events();
        state.push_events.subscribe(channel);

        let child = resolve_fork_request(&state, &anchor, &opened.fork_id, true)
            .unwrap()
            .expect("an approved request creates a child");

        let store = crate::conversations::store(&anchor).unwrap();
        let recorded = store.fork_decisions(&source.id).unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].fork_id, opened.fork_id);
        assert!(recorded[0].approved);
        assert_eq!(recorded[0].title, "继续做 B");
        assert_eq!(recorded[0].prompt, "继续做 B");
        assert_eq!(recorded[0].requested_at, opened.requested_at);
        assert_eq!(
            recorded[0].child_conversation_id.as_deref(),
            Some(child.id.as_str())
        );

        let published = events.lock().unwrap();
        let resolved = published
            .iter()
            .find(|event| event["type"] == "forkResolved")
            .expect("the outcome is published");
        assert_eq!(resolved["decision"]["approved"], true);
        assert_eq!(resolved["decision"]["childConversationId"], child.id);
    }

    #[test]
    fn declining_records_the_refusal_and_creates_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let (state, anchor, source) = seeded(directory.path());
        let document = default_document();
        let opened = state
            .fork_requests()
            .open(PendingForkRequest {
                workspace_id: document.workspaces[0].id.clone(),
                ..card(&source.id)
            })
            .unwrap();

        assert!(
            resolve_fork_request(&state, &anchor, &opened.fork_id, false)
                .unwrap()
                .is_none()
        );

        let store = crate::conversations::store(&anchor).unwrap();
        let recorded = store.fork_decisions(&source.id).unwrap();
        assert_eq!(recorded.len(), 1);
        assert!(!recorded[0].approved);
        assert_eq!(recorded[0].child_conversation_id, None);
        assert!(store.pending_fork_starts().unwrap().is_empty());
    }

    /// A retraction is not an answer: the source conversation is going away, so
    /// there is nothing for its task bar to show and nothing to record.
    #[test]
    fn retracting_records_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let (state, anchor, source) = seeded(directory.path());
        let document = default_document();
        state
            .fork_requests()
            .open(PendingForkRequest {
                workspace_id: document.workspaces[0].id.clone(),
                ..card(&source.id)
            })
            .unwrap();
        let (channel, events) = collecting_events();
        state.push_events.subscribe(channel);

        retract_requests_for_conversation(&state, &source.id);

        let store = crate::conversations::store(&anchor).unwrap();
        assert!(store.fork_decisions(&source.id).unwrap().is_empty());
        let published = events.lock().unwrap();
        assert_eq!(published.len(), 1);
        assert_eq!(published[0]["type"], "forkResolved");
        assert_eq!(published[0]["approved"], false);
        assert!(published[0].get("decision").is_none(), "{:?}", published[0]);
    }
}
