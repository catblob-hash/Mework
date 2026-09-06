//! Command layer for conversation data. Every renderer-initiated conversation
//! change passes through this module.
//!
//! The host is the sole writer of conversation prose. The renderer sends
//! intents to create, delete, reorder, edit metadata/settings, or replace prose
//! while idle under an optimistic concurrency guard.
//!
//! Every command follows this fixed order:
//! 1. Write SQLite (`conversation_store`) so data is durable on return.
//! 2. Update the `DocumentStore` memory snapshot from the database's authoritative
//!    result so host readers and the renderer observe the same state.

use std::collections::HashSet;
use std::path::Path;

use crate::{
    conversation_store::{self, ConversationStore},
    model::{AppDocument, Conversation},
    state::AppState,
};

/// Gets the conversation store for this application data directory.
pub(crate) fn store(path: &Path) -> Result<std::sync::Arc<ConversationStore>, String> {
    conversation_store::store_for(path)
}

/// Writes the database-authoritative conversation into the memory snapshot.
/// `None` means the conversation was deleted.
fn sync_snapshot(
    state: &AppState,
    path: &Path,
    workspace_id: &str,
    conversation_id: &str,
    conversation: Option<Conversation>,
) -> Result<(), String> {
    let current = state
        .document_store
        .current_snapshot(path)?;
    let mut document = (*current).clone();
    patch_conversation(
        &mut document,
        workspace_id,
        conversation_id,
        conversation,
    );
    reconcile_conversation_subdata(path, &current, &document);
    let rebound = crate::terminal_lifecycle::invalidated_conversations(&current, &document);
    state.document_store.commit(path, document)?;
    invalidate_rebound_conversations(state, &rebound);
    Ok(())
}

/// A moved or deleted conversation leaves terminals and MCP sessions bound to
/// its original cwd and workspace coordination key. Invalidate them here because
/// conversation ownership changes only at this layer.
///
/// Call only after the snapshot commit succeeds; otherwise ownership is unchanged
/// and closing sessions would lose live state.
fn invalidate_rebound_conversations(state: &AppState, rebound: &HashSet<String>) {
    if rebound.is_empty() {
        return;
    }
    state
        .terminals
        .close_conversations(rebound.iter().map(String::as_str));
    state
        .mcp_sessions
        .evict_conversations(rebound.iter().map(String::as_str));
}

/// Reclaims conversation-owned image attachments and workflow run directories.
/// It must run at conversation write boundaries so deleted conversations do not
/// retain their attachments or run records.
fn reconcile_conversation_subdata(path: &Path, previous: &AppDocument, next: &AppDocument) {
    let Some(app_data) = path.parent() else {
        return;
    };
    if let Err(error) = crate::image_attachments::ImageAttachmentStore::new(app_data)
        .reconcile_transition(previous, next)
    {
        eprintln!("对话已更新，但图片附件隔离回收将在下次保存或启动时重试：{error}");
    }
    crate::workflow_store::remove_removed_conversation_runs(app_data, previous, next);
}

/// Replaces, inserts, or removes a conversation in the memory document. Remove
/// it globally by ID first because a moved conversation may not be in
/// `workspace_id`.
fn patch_conversation(
    document: &mut AppDocument,
    workspace_id: &str,
    conversation_id: &str,
    conversation: Option<Conversation>,
) {
    for workspace in &mut document.workspaces {
        workspace
            .conversations
            .retain(|candidate| candidate.id != conversation_id);
    }
    let Some(conversation) = conversation else {
        return;
    };
    if let Some(workspace) = document
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id == workspace_id)
    {
        workspace.conversations.push(conversation);
    }
}

/// Reorders a workspace's conversations in the memory document to match the
/// database order.
fn resync_workspace(
    state: &AppState,
    path: &Path,
    workspace_id: &str,
) -> Result<(), String> {
    let store = store(path)?;
    let conversations = store.workspace_conversations(workspace_id)?;
    let current = state
        .document_store
        .current_snapshot(path)?;
    let mut document = (*current).clone();
    let moved = conversations
        .iter()
        .map(|conversation| conversation.id.clone())
        .collect::<HashSet<_>>();
    for workspace in &mut document.workspaces {
        if workspace.id == workspace_id {
            continue;
        }
        if workspace.conversations.iter().any(|conversation| moved.contains(&conversation.id)) {
            workspace.conversations = store.workspace_conversations(&workspace.id)?;
        }
    }
    if let Some(workspace) = document
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id == workspace_id)
    {
        workspace.conversations = conversations;
    }
    // Workspace moves occur through target-workspace reordering, so invalidate
    // the conversations whose old bindings changed on this path.
    let rebound = crate::terminal_lifecycle::invalidated_conversations(&current, &document);
    state.document_store.commit(path, document)?;
    invalidate_rebound_conversations(state, &rebound);
    Ok(())
}

/// Creates a conversation, including a fork target.
pub(crate) fn create(
    state: &AppState,
    path: &Path,
    workspace_id: &str,
    conversation: &Conversation,
) -> Result<Conversation, String> {
    create_with_fork_start(state, path, workspace_id, conversation, None)
}

pub(crate) fn create_with_fork_start(
    state: &AppState,
    path: &Path,
    workspace_id: &str,
    conversation: &Conversation,
    prompt_context_id: Option<&str>,
) -> Result<Conversation, String> {
    let store = store(path)?;
    if store.conversation(&conversation.id)?.is_some() {
        return Err(format!("对话 {} 已存在", conversation.id));
    }
    let mut next = conversation.clone();
    validate_incoming(state, path, workspace_id, &mut next)?;
    if let Some(prompt_context_id) = prompt_context_id {
        store.put_fork_conversation(workspace_id, &next, prompt_context_id)?;
    } else {
        store.put_conversation(workspace_id, &next)?;
    }
    if !next.settings.agent_definitions.is_empty() {
        // Named-agent definitions are security authorization and must be durably
        // stored before becoming in-memory authority.
        store.flush_durable()?;
    }
    let stored = store
        .conversation(&next.id)?
        .ok_or_else(|| format!("对话 {} 写入后读不回来", conversation.id))?;
    sync_snapshot(
        state,
        path,
        workspace_id,
        &conversation.id,
        Some(stored.clone()),
    )?;
    Ok(stored)
}

/// Deletes a conversation and all of its dependent data. Its children are
/// re-parented to its parent by the store, so the workspace is re-read
/// afterwards to carry those pointers into the snapshot.
pub(crate) fn delete(
    state: &AppState,
    path: &Path,
    workspace_id: &str,
    conversation_id: &str,
) -> Result<(), String> {
    let store = store(path)?;
    store.delete_conversation(conversation_id)?;
    state.retire_conversation_tasks(conversation_id);
    sync_snapshot(state, path, workspace_id, conversation_id, None)?;
    resync_workspace(state, path, workspace_id)
}

/// Applies a renderer-proposed whole-conversation update.
///
/// `expected_context_ids` is the renderer's main-timeline context ID sequence.
/// Accept prose changes only when it exactly matches the database sequence;
/// otherwise apply only metadata, settings, queued messages, and cancelled task
/// records. Never accept renderer prose changes during a run.
///
/// The run check comes before the read, and the caller holds `storage_lock`
/// throughout. A run that is active by then keeps the prose rows untouched
/// below; one that registers later cannot write a row until it has passed
/// `trusted_run_request` under the same lock, and one that finished earlier
/// had completed every write before it unregistered. So when no run is active
/// at the check, the snapshot read next is exactly what the replace overwrites.
pub(crate) fn update(
    state: &AppState,
    path: &Path,
    workspace_id: &str,
    proposal: &Conversation,
    expected_context_ids: &[String],
) -> Result<Conversation, String> {
    let store = store(path)?;
    let run_active = state.conversation_model_run_active(&proposal.id);
    let current = store
        .conversation(&proposal.id)?
        .ok_or_else(|| format!("对话 {} 不存在", proposal.id))?;
    let in_sync = !run_active
        && current
            .contexts
            .iter()
            .map(crate::model::ContextItem::id)
            .eq(expected_context_ids.iter().map(String::as_str));
    let mut next = proposal.clone();
    if !in_sync {
        // A renderer read model may lag the host. Preserve host prose and accept
        // only the proposed metadata changes.
        next.contexts = current.contexts.clone();
        next.branches = current.branches.clone();
    }
    validate_incoming(state, path, workspace_id, &mut next)?;
    let definitions_changed =
        crate::storage::conversation_agent_definitions_differ(Some(&current), &next);
    if in_sync {
        store.put_conversation(workspace_id, &next)?;
    } else {
        // The prose rows stay as they are on disk rather than being rewritten
        // from `current`: while a run is producing them, a card persisted after
        // that snapshot was read would otherwise be deleted by the rewrite.
        store.put_conversation_metadata(workspace_id, &next)?;
    }
    if definitions_changed {
        store.flush_durable()?;
    }
    let stored = store
        .conversation(&next.id)?
        .ok_or_else(|| format!("对话 {} 写入后读不回来", next.id))?;
    sync_snapshot(state, path, workspace_id, &next.id, Some(stored.clone()))?;
    Ok(stored)
}

/// Reorders and, when needed, moves a workspace's conversations.
pub(crate) fn reorder(
    state: &AppState,
    path: &Path,
    workspace_id: &str,
    conversation_ids: &[String],
) -> Result<(), String> {
    let store = store(path)?;
    store.set_workspace_order(workspace_id, conversation_ids)?;
    resync_workspace(state, path, workspace_id)
}

/// Reads authoritative conversation prose. The renderer aligns its read model
/// after every completed turn so persisted content is always visible.
pub(crate) fn load(path: &Path, conversation_id: &str) -> Result<Option<Conversation>, String> {
    store(path)?.conversation(conversation_id)
}

/// Validates a renderer-proposed conversation before writing. Host-produced
/// prose is already valid; this gate rejects invalid IDs, out-of-catalog tools,
/// and forged tool cards from renderer proposals.
fn validate_incoming(
    state: &AppState,
    path: &Path,
    workspace_id: &str,
    conversation: &mut Conversation,
) -> Result<(), String> {
    let current = state.document_store.current_snapshot(path)?;
    normalize_parent_pointer(&current, workspace_id, conversation);
    crate::storage::validate_incoming_conversation(&current, workspace_id, conversation, state)
}

/// A parent pointer is a sidebar hint, not authority, so a bad one is dropped
/// rather than refused: pointing at itself, at a conversation the document
/// does not hold, or into a cycle all become "top level". The tree builder in
/// the renderer applies the same rule, so both sides agree on what they draw.
fn normalize_parent_pointer(document: &AppDocument, workspace_id: &str, conversation: &mut Conversation) {
    let Some(parent) = conversation.parent_conversation_id.as_deref() else {
        return;
    };
    let parent_of = |id: &str| -> Option<Option<String>> {
        document
            .workspaces
            .iter()
            .filter(|workspace| workspace.id == workspace_id)
            .flat_map(|workspace| workspace.conversations.iter())
            .find(|candidate| candidate.id == id)
            .map(|candidate| candidate.parent_conversation_id.clone())
    };
    let mut cursor = Some(parent.to_owned());
    let mut hops = 0usize;
    while let Some(id) = cursor {
        if id == conversation.id || hops > document.workspaces.iter().map(|w| w.conversations.len()).sum::<usize>() {
            conversation.parent_conversation_id = None;
            return;
        }
        match parent_of(&id) {
            None => {
                conversation.parent_conversation_id = None;
                return;
            }
            Some(next) => cursor = next,
        }
        hops += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::default_document;
    use crate::conversation_store::ContextStatus;
    use crate::model::ContextItem;

    fn document_with_chain() -> (AppDocument, Conversation) {
        let directory = tempfile::tempdir().unwrap();
        let mut document = default_document(directory.path());
        let template = document.workspaces[0].conversations[0].clone();
        let mut root = template.clone();
        root.id = "conv_root".into();
        root.parent_conversation_id = None;
        let mut middle = template.clone();
        middle.id = "conv_middle".into();
        middle.parent_conversation_id = Some("conv_root".into());
        document.workspaces[0].conversations = vec![root, middle];
        (document, template)
    }

    #[test]
    fn reorder_refreshes_source_parent_edges_and_rejects_cross_workspace_updates() {
        let directory = tempfile::tempdir().unwrap();
        let (state, anchor, source_id, parent) = seeded(directory.path());
        let mut document = (*state.document_store.current_snapshot(&anchor).unwrap()).clone();
        let mut destination = document.workspaces[0].clone();
        destination.id = "workspace_destination".into();
        destination.conversations.clear();
        let destination_id = destination.id.clone();
        document.workspaces.push(destination);
        let mut child = parent.clone();
        child.id = "conv_child_move".into();
        child.parent_conversation_id = Some(parent.id.clone());
        document.workspaces[0].conversations.push(child.clone());
        state.document_store.commit(&anchor, document).unwrap();
        store(&anchor).unwrap().put_conversation(&source_id, &child).unwrap();
        reorder(&state, &anchor, &destination_id, &[parent.id.clone()]).unwrap();
        let snapshot = state.document_store.current_snapshot(&anchor).unwrap();
        let source = snapshot.workspaces.iter().find(|w| w.id == source_id).unwrap();
        assert_eq!(source.conversations.len(), 1);
        assert_eq!(source.conversations[0].parent_conversation_id, None);
        let mut stale = child.clone();
        normalize_parent_pointer(&snapshot, &source_id, &mut stale);
        assert_eq!(stale.parent_conversation_id, None);
        let stored = update(&state, &anchor, &source_id, &child, &[]).unwrap();
        assert_eq!(stored.parent_conversation_id, None);
    }

    #[test]
    fn a_parent_that_exists_is_kept_even_across_a_chain() {
        let (document, template) = document_with_chain();
        let mut leaf = template.clone();
        leaf.id = "conv_leaf".into();
        leaf.parent_conversation_id = Some("conv_middle".into());
        normalize_parent_pointer(&document, &document.workspaces[0].id, &mut leaf);
        assert_eq!(leaf.parent_conversation_id.as_deref(), Some("conv_middle"));
    }

    #[test]
    fn a_missing_self_or_cyclic_parent_becomes_top_level() {
        let (mut document, template) = document_with_chain();

        let mut dangling = template.clone();
        dangling.id = "conv_dangling".into();
        dangling.parent_conversation_id = Some("conv_gone".into());
        normalize_parent_pointer(&document, &document.workspaces[0].id, &mut dangling);
        assert_eq!(dangling.parent_conversation_id, None);

        let mut selfish = template.clone();
        selfish.id = "conv_self".into();
        selfish.parent_conversation_id = Some("conv_self".into());
        normalize_parent_pointer(&document, &document.workspaces[0].id, &mut selfish);
        assert_eq!(selfish.parent_conversation_id, None);

        // root -> middle already; proposing root under middle closes a loop.
        document.workspaces[0].conversations[1].parent_conversation_id = Some("conv_root".into());
        let mut root = document.workspaces[0].conversations[0].clone();
        root.parent_conversation_id = Some("conv_middle".into());
        normalize_parent_pointer(&document, &document.workspaces[0].id, &mut root);
        assert_eq!(root.parent_conversation_id, None);
    }

    /// A real store beside an anchor, holding the default document's first
    /// conversation; returns it as the store reads it back.
    fn seeded(directory: &Path) -> (AppState, std::path::PathBuf, String, Conversation) {
        let state = AppState::default();
        let document = default_document(directory);
        let anchor = directory.join("document.v1.json");
        state
            .document_store
            .acquire_process_authority(&anchor)
            .unwrap();
        state.document_store.commit(&anchor, document.clone()).unwrap();
        let store = store(&anchor).unwrap();
        let workspace = &document.workspaces[0];
        let source = &workspace.conversations[0];
        store.put_conversation(&workspace.id, source).unwrap();
        let stored = store.conversation(&source.id).unwrap().unwrap();
        (state, anchor, workspace.id.clone(), stored)
    }

    #[test]
    fn deleting_a_conversation_retires_only_its_runtime() {
        let directory = tempfile::tempdir().unwrap();
        let (state, anchor, workspace_id, stored) = seeded(directory.path());
        let old = state.conversation_tasks(&stored.id);
        let other = state.conversation_tasks("other-conversation");
        let (cancel, _) = state.begin_model_run("deleted-run", &stored.id).unwrap();
        let surface = std::sync::Arc::new(crate::state::TaskSurface {
            sink: std::sync::Arc::new(|_| Ok(())),
            approve: std::sync::Arc::new(|_, _, _, _| Ok(false)),
        });
        state.register_task_surface(&stored.id, surface.clone());
        assert!(state.task_surface(&stored.id).is_some());

        delete(&state, &anchor, &workspace_id, &stored.id).unwrap();

        assert!(state.task_surface(&stored.id).is_none());
        state.register_task_surface(&stored.id, surface);
        assert!(state.task_surface(&stored.id).is_none());
        assert!(state.existing_conversation_tasks(&stored.id).is_none());
        assert!(cancel.load(std::sync::atomic::Ordering::Acquire));
        assert!(std::sync::Arc::ptr_eq(
            &other,
            &state.existing_conversation_tasks("other-conversation").unwrap()
        ));
        // A late old run cannot recreate the map entry, even when it still owns
        // the previous runtime. Repeating the durable delete is harmless.
        drop(old);
        let _late = state.conversation_tasks(&stored.id);
        assert!(state.existing_conversation_tasks(&stored.id).is_none());
        assert!(state.begin_model_run("late-run", &stored.id).is_err());
        delete(&state, &anchor, &workspace_id, &stored.id).unwrap();
        assert!(!state.wake_pending_conversations().contains(&stored.id));
    }

    #[test]
    fn durable_delete_retires_runtime_even_when_snapshot_is_unavailable() {
        let directory = tempfile::tempdir().unwrap();
        let anchor = directory.path().join("document.v1.json");
        let state = AppState::default();
        let document = default_document(directory.path());
        let workspace = &document.workspaces[0];
        let conversation = &workspace.conversations[0];
        let store = store(&anchor).unwrap();
        store.put_conversation(&workspace.id, conversation).unwrap();
        let _tasks = state.conversation_tasks(&conversation.id);
        assert!(state.document_store.current_snapshot(&anchor).is_err());
        assert!(delete(&state, &anchor, &workspace.id, &conversation.id).is_err());
        assert!(store.conversation(&conversation.id).unwrap().is_none());
        assert!(state.existing_conversation_tasks(&conversation.id).is_none());
    }

    #[test]
    fn document_removal_retires_workspace_tasks_but_not_moved_conversations() {
        let directory = tempfile::tempdir().unwrap();
        let state = AppState::default();
        let previous = default_document(directory.path());
        let removed_id = previous.workspaces[0].conversations[0].id.clone();
        let removed = state.conversation_tasks(&removed_id);
        let mut next = previous.clone();
        next.workspaces.clear();
        state.retire_removed_conversation_tasks(&previous, &next);
        assert!(state.existing_conversation_tasks(&removed_id).is_none());
        drop(removed);

        let state = AppState::default();
        let moved = state.conversation_tasks(&removed_id);
        let mut next = previous.clone();
        next.workspaces[0].id = "new-workspace".into();
        state.retire_removed_conversation_tasks(&previous, &next);
        assert!(std::sync::Arc::ptr_eq(&moved, &state.existing_conversation_tasks(&removed_id).unwrap()));
    }

    fn assistant(id: &str, content: &str, round: usize) -> ContextItem {
        ContextItem::Assistant {
            id: id.into(),
            content: content.into(),
            round: Some(round),
            model_turn_id: Some(format!("turn-{round}")),
            interrupted: false,
            sources: Vec::new(),
            created_at: "2026-09-05T00:00:01.000Z".into(),
        }
    }

    fn context_ids(conversation: &Conversation) -> Vec<String> {
        conversation
            .contexts
            .iter()
            .map(|context| context.id().to_owned())
            .collect()
    }

    /// While a run owns the timeline, a renderer edit lands only its metadata.
    /// The rows the run has persisted stay as they are — including a row the
    /// edit's snapshot never saw and a row that is still streaming — instead of
    /// being replaced by a snapshot of the timeline read moments earlier.
    #[test]
    fn an_edit_during_a_run_never_rewrites_the_timeline_rows() {
        let directory = tempfile::tempdir().unwrap();
        let (state, anchor, workspace_id, stored) = seeded(directory.path());
        let store = store(&anchor).unwrap();
        let (_cancellation, _inbox) = state.begin_model_run("run-1", &stored.id).unwrap();

        // The renderer's edit is built from the timeline as it knew it; the
        // run persists more after that, and one of its rows is still streaming.
        let mut proposal = stored.clone();
        proposal.title = "renamed mid-run".into();
        proposal.settings.system_prompt = "be brief".into();
        let expected_ids = context_ids(&stored);
        store
            .upsert_contexts(
                &stored.id,
                &[assistant("ctx_after_snapshot", "done", 1)],
                ContextStatus::Settled,
            )
            .unwrap();
        store
            .upsert_contexts(
                &stored.id,
                &[assistant("ctx_live", "half written", 2)],
                ContextStatus::Streaming,
            )
            .unwrap();

        let returned = update(&state, &anchor, &workspace_id, &proposal, &expected_ids).unwrap();

        assert_eq!(returned.title, "renamed mid-run");
        assert_eq!(returned.settings.system_prompt, "be brief");
        let mut expected = expected_ids.clone();
        expected.extend(["ctx_after_snapshot".to_owned(), "ctx_live".to_owned()]);
        assert_eq!(context_ids(&returned), expected);
        // A full rewrite would have re-filed the streaming row as settled prose
        // and, with it, the run's claim to replace it in place or mark it
        // interrupted. Its status is what tells the two writes apart.
        assert_eq!(store.reconcile_streaming_in(&stored.id).unwrap(), 1);
        // The snapshot the renderer reads carries the same timeline.
        let snapshot = state.document_store.current_snapshot(&anchor).unwrap();
        let mirrored = snapshot
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.conversations.iter())
            .find(|conversation| conversation.id == stored.id)
            .unwrap();
        assert_eq!(context_ids(mirrored), expected);
        assert_eq!(mirrored.title, "renamed mid-run");
    }

    /// With no run active and the renderer's view of the timeline current, an
    /// edit may replace the prose as before.
    #[test]
    fn an_idle_in_sync_edit_still_replaces_the_prose() {
        let directory = tempfile::tempdir().unwrap();
        let (state, anchor, workspace_id, stored) = seeded(directory.path());
        let mut proposal = stored.clone();
        proposal
            .contexts
            .push(assistant("ctx_edit", "appended by the renderer", 1));

        let returned = update(
            &state,
            &anchor,
            &workspace_id,
            &proposal,
            &context_ids(&stored),
        )
        .unwrap();

        assert_eq!(context_ids(&returned), context_ids(&proposal));
    }

    /// A stale renderer view keeps host prose but still lands metadata; this is
    /// the same path the mid-run edit takes, so the prose rows are not rewritten
    /// here either.
    #[test]
    fn a_stale_idle_edit_keeps_host_prose_and_lands_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let (state, anchor, workspace_id, stored) = seeded(directory.path());
        let store = store(&anchor).unwrap();
        store
            .upsert_contexts(
                &stored.id,
                &[assistant("ctx_host", "host wrote this", 1)],
                ContextStatus::Settled,
            )
            .unwrap();
        let mut proposal = stored.clone();
        proposal.title = "renamed on a stale view".into();
        proposal.contexts = vec![assistant("ctx_edit", "would replace everything", 1)];

        let returned = update(
            &state,
            &anchor,
            &workspace_id,
            &proposal,
            &context_ids(&stored),
        )
        .unwrap();

        assert_eq!(returned.title, "renamed on a stale view");
        let mut expected = context_ids(&stored);
        expected.push("ctx_host".into());
        assert_eq!(context_ids(&returned), expected);
    }
}
