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
//!    result so host readers and the renderer observe the same state. The
//!    snapshot keeps the conversation's metadata and settings; its body is
//!    recorded for attachment reclamation and stripped (`attachment_refs`).
//!
//! Bodies are read with [`load`], which serves them from the shared memory pool
//! and falls back to the database when the pool has unloaded them.

use std::collections::HashSet;
use std::path::Path;

use crate::{
    attachment_refs::AttachmentRefs,
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
pub(crate) fn sync_snapshot(
    state: &AppState,
    path: &Path,
    workspace_id: &str,
    conversation_id: &str,
    conversation: Option<Conversation>,
) -> Result<(), String> {
    let (current, current_refs) = state.document_store.snapshot_with_refs(path)?;
    let mut document = (*current).clone();
    let mut refs = (*current_refs).clone();
    // The database's body is authoritative even when empty, which a stripped
    // conversation could not say for itself.
    let has_conversation = conversation.is_some();
    if let Some(conversation) = &conversation {
        refs.record(conversation);
    }
    patch_conversation(&mut document, workspace_id, conversation_id, conversation);
    refs.hollow(&mut document);
    reconcile_conversation_subdata(path, (&current, &current_refs), (&document, &refs));
    let rebound = crate::terminal_lifecycle::invalidated_conversations(&current, &document);
    let stored = has_conversation;
    state.document_store.commit_with_refs(path, document, refs)?;
    invalidate_rebound_conversations(state, &rebound);
    if stored {
        // A hand edit lands here; mirror it into the conversation's projection.
        crate::wire_history::on_conversation_committed(path, conversation_id);
    }
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

/// Reclaims conversation-owned image and file attachments and workflow run
/// directories. It must run at conversation write boundaries so deleted
/// conversations do not retain their attachments or run records.
fn reconcile_conversation_subdata(
    path: &Path,
    (previous, previous_refs): (&AppDocument, &AttachmentRefs),
    (next, next_refs): (&AppDocument, &AttachmentRefs),
) {
    let Some(app_data) = path.parent() else {
        return;
    };
    match template_image_ids(path) {
        // Template bodies are the one set of references no document names, so
        // reclaiming without them would read a template's own images as orphans.
        Err(error) => {
            eprintln!("对话已更新，但无法读取模板图片引用，本次不回收图片附件：{error}");
        }
        Ok(pinned) => {
            let mut referenced = next_refs.image_ids(next);
            referenced.extend(pinned);
            if let Err(error) = crate::image_attachments::ImageAttachmentStore::new(app_data)
                .reconcile_referenced(&previous_refs.image_ids(previous), &referenced)
            {
                eprintln!("对话已更新，但图片附件隔离回收将在下次保存或启动时重试：{error}");
            }
        }
    }
    match template_file_ids(path) {
        Err(error) => {
            eprintln!("对话已更新，但无法读取模板文件附件引用，本次不回收文件附件：{error}");
        }
        Ok(pinned) => {
            let mut referenced = next_refs.file_ids(next);
            referenced.extend(pinned);
            if let Err(error) = crate::file_attachments::FileAttachmentStore::new(app_data)
                .reconcile_referenced(&previous_refs.file_ids(previous), &referenced)
            {
                eprintln!("对话已更新，但文件附件隔离回收将在下次保存或启动时重试：{error}");
            }
        }
    }
    crate::workflow_store::remove_removed_conversation_runs(app_data, previous, next);
}

/// Image ids held by stored conversation templates, which live outside the document.
pub(crate) fn template_image_ids(path: &Path) -> Result<HashSet<String>, String> {
    store(path)?.template_image_ids()
}

/// File attachment ids held by stored conversation templates.
pub(crate) fn template_file_ids(path: &Path) -> Result<HashSet<String>, String> {
    store(path)?.template_file_ids()
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
fn resync_workspace(state: &AppState, path: &Path, workspace_id: &str) -> Result<(), String> {
    let store = store(path)?;
    // Shells: a reorder or deletion changes ownership and parent pointers, not
    // bodies, and the snapshot keeps no bodies anyway.
    let conversations = store.workspace_conversation_shells(workspace_id)?;
    let current = state.document_store.current_snapshot(path)?;
    let mut document = (*current).clone();
    let moved = conversations
        .iter()
        .map(|conversation| conversation.id.clone())
        .collect::<HashSet<_>>();
    for workspace in &mut document.workspaces {
        if workspace.id == workspace_id {
            continue;
        }
        if workspace
            .conversations
            .iter()
            .any(|conversation| moved.contains(&conversation.id))
        {
            workspace.conversations = store.workspace_conversation_shells(&workspace.id)?;
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
    validate_incoming(state, path, workspace_id, &mut next, None)?;
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
    if let Some(app_data) = path.parent() {
        crate::handoff::remove_notebook(app_data, conversation_id);
    }
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
    // The host just wrote a title (the local helper model's) that this commit
    // was built before hearing about: keep the host's.
    if state.helper_model.is_stale_title(&proposal.id, &next.title, &current.title) {
        next.title = current.title.clone();
    }
    if !in_sync {
        // A renderer read model may lag the host. Preserve host prose and accept
        // only the proposed metadata changes.
        next.contexts = current.contexts.clone();
        next.branches = current.branches.clone();
    }
    validate_incoming(state, path, workspace_id, &mut next, Some(&current))?;
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
    // The user may pick another level at any time — while a turn streams and
    // while an approval card waits included — and it takes effect at once: the
    // conversation's live cell is what the running turn, its children and any
    // worker left from an earlier turn read. The browser session caches the
    // level for its navigation callbacks; the next preview call refreshes it
    // anyway, so a conversation with no page is not an error.
    if let Some(live) = state.live_security_level(&next.id) {
        if live.get() != stored.settings.security_level {
            live.set(stored.settings.security_level);
            if let Ok(session_id) = crate::browser::preview_page_session_id(&next.id) {
                state
                    .browser
                    .set_session_security_level(&session_id, stored.settings.security_level);
            }
        }
    }
    // The plan-mode switch on the same terms: switched on or off mid-turn, the
    // next round boundary appends the guidance or the note that it ended, and
    // `exit_plan_mode` answers by the switch as it stands.
    if let Some(live) = state.live_plan_mode(&next.id) {
        live.set(stored.settings.plan_mode_enabled);
    }
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

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AttestEditedToolContextRequest {
    pub conversation_id: String,
    pub context_id: String,
    pub tool_name: String,
    pub input: serde_json::Map<String, serde_json::Value>,
    pub output: String,
    pub images: Vec<crate::model::ImageAttachment>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AttestEditedToolContextResponse {
    pub input: serde_json::Map<String, serde_json::Value>,
    pub result: crate::model::ToolResult,
    pub attestation: String,
}

/// Caller holds storage_lock, exactly as for update. Only committed timeline
/// roots (including branch roots), never child-transcript cards, are editable.
pub(crate) fn attest_edited_tool_context(
    state: &AppState,
    path: &Path,
    request: AttestEditedToolContextRequest,
) -> Result<AttestEditedToolContextResponse, String> {
    if state.conversation_model_run_active(&request.conversation_id) {
        return Err("对话正在运行，不能编辑工具记录".into());
    }
    let conversation =
        load(path, &request.conversation_id)?.ok_or_else(|| "对话不存在".to_owned())?;
    let context = conversation
        .contexts
        .iter()
        .chain(
            conversation
                .branches
                .iter()
                .flat_map(|branch| branch.contexts.iter()),
        )
        .find(|context| context.id() == request.context_id)
        .ok_or_else(|| "工具记录不存在".to_owned())?;
    let crate::model::ContextItem::Tool {
        tool_name,
        result,
        subagent,
        ..
    } = context
    else {
        return Err("上下文不是工具记录".into());
    };
    if tool_name != &request.tool_name {
        return Err("工具名称与已保存记录不一致".into());
    }
    if subagent.is_some() {
        return Err("带有子代理记录的工具卡不能手动编辑".into());
    }
    let mut edited = result.clone();
    edited.images = request.images;
    // Check images before changing output so the existing full-metadata,
    // ordered-subset comparator also protects every host-owned result field.
    if !crate::state::tool_result_is_exact_or_image_removal(result, &edited) {
        return Err("工具结果图片只能按原顺序保留或删除".into());
    }
    edited.output = request.output;
    let attestation = state.attest_tool_context(&crate::tool_attestation::AttestationSubject {
        conversation_id: &request.conversation_id,
        context_id: &request.context_id,
        tool_name,
        input: &request.input,
        requested_input: None,
        result: &edited,
        subagent: None,
    });
    if attestation.is_empty() {
        return Err("无法签发工具记录凭证".into());
    }
    Ok(AttestEditedToolContextResponse {
        input: request.input,
        result: edited,
        attestation,
    })
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AttestInsertedToolContextRequest {
    pub conversation_id: String,
    pub context_id: String,
    pub tool_name: String,
    pub input: serde_json::Map<String, serde_json::Value>,
    pub output: String,
}

/// Issues the attestation for a call the user wrote out by hand instead of running.
///
/// Kept apart from the edit command rather than folded into it as an "unknown id
/// means insert" branch: a mistyped id on an edit would then quietly mint a
/// second card instead of failing. The result is the host's own — a hand-written
/// card can carry text, never a diff, images, or a duration it did not spend.
pub(crate) fn attest_inserted_tool_context(
    state: &AppState,
    path: &Path,
    request: AttestInsertedToolContextRequest,
) -> Result<AttestEditedToolContextResponse, String> {
    if state.conversation_model_run_active(&request.conversation_id) {
        return Err("对话正在运行，不能编辑工具记录".into());
    }
    let conversation = load(path, &request.conversation_id)?;
    // A draft the user has not sent from yet has no row at all, and a
    // conversation with no rows has no card this id could collide with.
    // Refusing there would make a placed call the one context kind a new
    // conversation cannot hold — a manually executed one already works.
    let collides = conversation.is_some_and(|conversation| {
        conversation
            .contexts
            .iter()
            .chain(
                conversation
                    .branches
                    .iter()
                    .flat_map(|branch| branch.contexts.iter()),
            )
            .any(|context| context.id() == request.context_id)
    });
    if collides {
        return Err("该上下文编号已经存在".into());
    }
    let result = crate::model::ToolResult {
        success: true,
        output: request.output,
        images: Vec::new(),
        diff: None,
        executed_at: String::new(),
        duration_ms: 0,
    };
    let attestation = state.attest_tool_context(&crate::tool_attestation::AttestationSubject {
        conversation_id: &request.conversation_id,
        context_id: &request.context_id,
        tool_name: &request.tool_name,
        input: &request.input,
        requested_input: None,
        result: &result,
        subagent: None,
    });
    if attestation.is_empty() {
        return Err("无法签发工具记录凭证".into());
    }
    Ok(AttestEditedToolContextResponse {
        input: request.input,
        result,
        attestation,
    })
}

/// Reads authoritative conversation prose: from memory when the shared pool
/// holds the body, otherwise from the database. The renderer aligns its read
/// model after every completed turn so persisted content is always visible.
pub(crate) fn load(path: &Path, conversation_id: &str) -> Result<Option<Conversation>, String> {
    store(path)?.conversation(conversation_id)
}

/// Validates a renderer-proposed conversation before writing. Host-produced
/// prose is already valid; this gate rejects invalid IDs, out-of-catalog tools,
/// and forged tool cards from renderer proposals.
///
/// `previous` is the conversation's stored body when it has one: the snapshot
/// carries no bodies, and the tool-card check compares against the cards
/// already stored.
fn validate_incoming(
    state: &AppState,
    path: &Path,
    workspace_id: &str,
    conversation: &mut Conversation,
    previous: Option<&Conversation>,
) -> Result<(), String> {
    let current = state.document_store.current_snapshot(path)?;
    normalize_parent_pointer(&current, workspace_id, conversation);
    crate::storage::validate_incoming_conversation(
        &current,
        workspace_id,
        conversation,
        previous,
        state,
    )
}

/// A parent pointer is a sidebar hint, not authority, so a bad one is dropped
/// rather than refused: pointing at itself, at a conversation the document
/// does not hold, or into a cycle all become "top level". A pointer at a
/// conversation that is itself a child is rewritten to that child's own root,
/// which holds the sidebar to two levels. The tree builder in the renderer
/// applies the same rules, so both sides agree on what they draw.
fn normalize_parent_pointer(
    document: &AppDocument,
    workspace_id: &str,
    conversation: &mut Conversation,
) {
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
    let limit = document
        .workspaces
        .iter()
        .map(|w| w.conversations.len())
        .sum::<usize>();
    // The last id the walk confirmed present: once the walk runs out of parents
    // this is the topmost ancestor, which is the level the sidebar draws.
    let mut root = parent.to_owned();
    let mut cursor = Some(parent.to_owned());
    let mut hops = 0usize;
    while let Some(id) = cursor {
        if id == conversation.id || hops > limit {
            conversation.parent_conversation_id = None;
            return;
        }
        match parent_of(&id) {
            None => {
                conversation.parent_conversation_id = None;
                return;
            }
            Some(next) => {
                root = id;
                cursor = next;
            }
        }
        hops += 1;
    }
    conversation.parent_conversation_id = Some(root);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::default_document;
    use crate::conversation_store::ContextStatus;
    use crate::model::ContextItem;

    fn document_with_chain() -> (AppDocument, Conversation) {
        let mut document = default_document();
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
        store(&anchor)
            .unwrap()
            .put_conversation(&source_id, &child)
            .unwrap();
        reorder(&state, &anchor, &destination_id, &[parent.id.clone()]).unwrap();
        let snapshot = state.document_store.current_snapshot(&anchor).unwrap();
        let source = snapshot
            .workspaces
            .iter()
            .find(|w| w.id == source_id)
            .unwrap();
        assert_eq!(source.conversations.len(), 1);
        assert_eq!(source.conversations[0].parent_conversation_id, None);
        let mut stale = child.clone();
        normalize_parent_pointer(&snapshot, &source_id, &mut stale);
        assert_eq!(stale.parent_conversation_id, None);
        let stored = update(&state, &anchor, &source_id, &child, &[]).unwrap();
        assert_eq!(stored.parent_conversation_id, None);
    }

    #[test]
    fn a_parent_that_is_itself_a_child_collapses_to_the_root() {
        let (document, template) = document_with_chain();

        // Pointing at a root keeps the pointer as written.
        let mut child = template.clone();
        child.id = "conv_child".into();
        child.parent_conversation_id = Some("conv_root".into());
        normalize_parent_pointer(&document, &document.workspaces[0].id, &mut child);
        assert_eq!(child.parent_conversation_id.as_deref(), Some("conv_root"));

        // Pointing at a child rewrites to that child's root, holding the sidebar
        // to two levels however long the proposed chain is.
        let mut leaf = template.clone();
        leaf.id = "conv_leaf".into();
        leaf.parent_conversation_id = Some("conv_middle".into());
        normalize_parent_pointer(&document, &document.workspaces[0].id, &mut leaf);
        assert_eq!(leaf.parent_conversation_id.as_deref(), Some("conv_root"));
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
        let store = store(&anchor).unwrap();
        let workspace = &document.workspaces[0];
        let source = &workspace.conversations[0];
        store.put_conversation(&workspace.id, source).unwrap();
        let stored = store.conversation(&source.id).unwrap().unwrap();
        (state, anchor, workspace.id.clone(), stored)
    }

    fn edited_card_fixture(
        directory: &Path,
    ) -> (AppState, std::path::PathBuf, String, Conversation) {
        let (state, anchor, workspace_id, mut conversation) = seeded(directory);
        conversation.contexts = vec![serde_json::from_value(serde_json::json!({
            "kind": "tool", "id": "edited-card", "toolName": "shell",
            "input": {"command": "original"}, "requestedInput": {"command": "requested"},
            "result": {"success": false, "output": "original", "diff": "host diff",
                "executedAt": "2026-09-10T00:00:00Z", "durationMs": 42},
            "createdAt": "2026-09-10T00:00:00Z"
        }))
        .unwrap()];
        store(&anchor)
            .unwrap()
            .put_conversation(&workspace_id, &conversation)
            .unwrap();
        sync_snapshot(
            &state,
            &anchor,
            &workspace_id,
            &conversation.id,
            Some(conversation.clone()),
        )
        .unwrap();
        (state, anchor, workspace_id, conversation)
    }

    fn edit_request(conversation: &Conversation) -> AttestEditedToolContextRequest {
        AttestEditedToolContextRequest {
            conversation_id: conversation.id.clone(),
            context_id: "edited-card".into(),
            tool_name: "shell".into(),
            input: serde_json::json!({"command": "hand edited, not executed"})
                .as_object()
                .unwrap()
                .clone(),
            output: "hand edited result".into(),
            images: vec![],
        }
    }

    #[test]
    fn hand_edit_attestation_passes_normal_save_gate_but_unsigned_edit_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let (state, anchor, workspace_id, mut conversation) = edited_card_fixture(directory.path());
        let request = edit_request(&conversation);
        if let crate::model::ContextItem::Tool {
            input,
            requested_input,
            result,
            ..
        } = &mut conversation.contexts[0]
        {
            *input = request.input.clone();
            *requested_input = None;
            result.output = request.output.clone();
        }
        let ids = vec!["edited-card".to_owned()];
        assert!(update(&state, &anchor, &workspace_id, &conversation, &ids).is_err());
        let response = attest_edited_tool_context(&state, &anchor, request).unwrap();
        assert!(!response.result.success);
        assert_eq!(response.result.diff.as_deref(), Some("host diff"));
        assert_eq!(response.result.duration_ms, 42);
        assert_eq!(response.result.executed_at, "2026-09-10T00:00:00Z");
        if let crate::model::ContextItem::Tool {
            input,
            requested_input,
            result,
            ..
        } = &mut conversation.contexts[0]
        {
            *input = response.input;
            *requested_input = None;
            *result = response.result;
        }
        if let crate::model::ContextItem::Tool { attestation, .. } = &mut conversation.contexts[0] {
            *attestation = response.attestation;
        }
        let saved = update(&state, &anchor, &workspace_id, &conversation, &ids).unwrap();
        assert_eq!(saved.contexts, conversation.contexts);
    }

    #[test]
    fn hand_edit_rejects_missing_identity_wrong_tool_and_active_run() {
        let directory = tempfile::tempdir().unwrap();
        let (state, anchor, _, conversation) = edited_card_fixture(directory.path());
        let mut request = edit_request(&conversation);
        request.context_id = "never-existed".into();
        assert!(attest_edited_tool_context(&state, &anchor, request).is_err());
        let mut request = edit_request(&conversation);
        request.tool_name = "read_file".into();
        assert!(attest_edited_tool_context(&state, &anchor, request).is_err());
        let mut request = edit_request(&conversation);
        request.conversation_id = "never-existed".into();
        assert!(attest_edited_tool_context(&state, &anchor, request).is_err());
        let (cancel, _) = state.begin_model_run("edit-run", &conversation.id).unwrap();
        assert!(attest_edited_tool_context(&state, &anchor, edit_request(&conversation)).is_err());
        state.finish_model_run("edit-run", &cancel);
    }

    #[test]
    fn hand_edit_uses_sqlite_and_refuses_subagent_records() {
        let directory = tempfile::tempdir().unwrap();
        let (state, anchor, workspace_id, mut conversation) = edited_card_fixture(directory.path());
        if let crate::model::ContextItem::Tool { subagent, .. } = &mut conversation.contexts[0] {
            *subagent = Some(
                serde_json::from_value(serde_json::json!({
                    "task": "child", "status": "completed", "contexts": [], "updates": []
                }))
                .unwrap(),
            );
        }
        // Deliberately leave DocumentStore stale: the command must read SQLite.
        store(&anchor)
            .unwrap()
            .put_conversation(&workspace_id, &conversation)
            .unwrap();
        assert!(attest_edited_tool_context(&state, &anchor, edit_request(&conversation)).is_err());
    }

    #[test]
    fn hand_edit_images_are_full_metadata_ordered_deletion_only() {
        let directory = tempfile::tempdir().unwrap();
        let (state, anchor, workspace_id, mut conversation) = edited_card_fixture(directory.path());
        let images = (1..=3)
            .map(|n| crate::model::ImageAttachment {
                id: format!("{n:064x}"),
                name: format!("{n}.png"),
                mime: "image/png".into(),
                width: 1,
                height: 1,
                bytes: 20,
                short_id: Some(n),
            })
            .collect::<Vec<_>>();
        if let crate::model::ContextItem::Tool { result, .. } = &mut conversation.contexts[0] {
            result.images = images.clone();
        }
        store(&anchor)
            .unwrap()
            .put_conversation(&workspace_id, &conversation)
            .unwrap();
        for retained in [
            images.clone(),
            vec![images[0].clone(), images[2].clone()],
            vec![],
        ] {
            let mut request = edit_request(&conversation);
            request.images = retained.clone();
            let response = attest_edited_tool_context(&state, &anchor, request).unwrap();
            assert_eq!(response.result.images, retained);
        }
        let mut changed_metadata = images[0].clone();
        changed_metadata.name = "forged.png".into();
        for invalid in [
            vec![images[1].clone(), images[0].clone()],
            vec![images[2].clone(), images[1].clone(), images[0].clone()],
            vec![images[0].clone(), images[0].clone()],
            vec![changed_metadata],
        ] {
            let mut request = edit_request(&conversation);
            request.images = invalid;
            assert!(attest_edited_tool_context(&state, &anchor, request).is_err());
        }
    }

    fn insert_request(conversation: &Conversation) -> AttestInsertedToolContextRequest {
        AttestInsertedToolContextRequest {
            conversation_id: conversation.id.clone(),
            context_id: "placed-card".into(),
            tool_name: "shell".into(),
            input: serde_json::json!({"command": "rm -rf build"})
                .as_object()
                .unwrap()
                .clone(),
            output: "已删除 build".into(),
        }
    }

    #[test]
    fn placed_card_is_written_without_running_and_owns_the_hosts_result_fields() {
        let directory = tempfile::tempdir().unwrap();
        let (state, anchor, workspace_id, mut conversation) = edited_card_fixture(directory.path());
        let response =
            attest_inserted_tool_context(&state, &anchor, insert_request(&conversation)).unwrap();
        // Nothing ran, so nothing the host would have measured is claimed.
        assert!(response.result.success);
        assert_eq!(response.result.output, "已删除 build");
        assert!(response.result.diff.is_none());
        assert!(response.result.images.is_empty());
        assert_eq!(response.result.duration_ms, 0);
        assert_eq!(response.result.executed_at, "");

        let placed: crate::model::ContextItem = serde_json::from_value(serde_json::json!({
            "kind": "tool", "id": "placed-card", "toolName": "shell",
            "input": response.input, "result": response.result,
            "attestation": response.attestation,
            "createdAt": "2026-09-11T00:00:00Z"
        }))
        .unwrap();
        conversation.contexts.push(placed);
        // The expectation is what is on disk now, not what the proposal adds.
        let ids = vec!["edited-card".to_owned()];
        let saved = update(&state, &anchor, &workspace_id, &conversation, &ids).unwrap();
        assert_eq!(saved.contexts, conversation.contexts);
    }

    #[test]
    fn placed_card_refuses_an_existing_id_and_an_active_run_but_serves_an_unsaved_draft() {
        let directory = tempfile::tempdir().unwrap();
        let (state, anchor, _, conversation) = edited_card_fixture(directory.path());
        // An edit that mistyped its id must not quietly become a second card.
        let mut request = insert_request(&conversation);
        request.context_id = "edited-card".into();
        assert!(attest_inserted_tool_context(&state, &anchor, request).is_err());
        // A draft with no row yet still takes a placed call.
        let mut request = insert_request(&conversation);
        request.conversation_id = "conv_never_saved".into();
        assert!(attest_inserted_tool_context(&state, &anchor, request).is_ok());
        let (cancel, _) = state
            .begin_model_run("place-run", &conversation.id)
            .unwrap();
        assert!(
            attest_inserted_tool_context(&state, &anchor, insert_request(&conversation)).is_err()
        );
        state.finish_model_run("place-run", &cancel);
    }

    /// The user may move the level at any time: a write during a run lands at
    /// once and reaches the cell the running turn and its children read, and a
    /// write after it reaches workers that outlived the turn.
    #[test]
    fn a_settings_write_moves_the_level_during_and_between_runs() {
        let directory = tempfile::tempdir().unwrap();
        let (state, anchor, workspace_id, stored) = seeded(directory.path());
        let cell = state
            .live_security_level_for_run(&stored.id, crate::model::SecurityLevel::RequestApproval);
        let (cancel, _) = state.begin_model_run("run-level", &stored.id).unwrap();

        let mut proposal = stored.clone();
        proposal.settings.security_level = crate::model::SecurityLevel::AllowEdits;
        let written = update(&state, &anchor, &workspace_id, &proposal, &[]).unwrap();
        assert_eq!(
            written.settings.security_level,
            crate::model::SecurityLevel::AllowEdits
        );
        assert_eq!(cell.get(), crate::model::SecurityLevel::AllowEdits);

        state.finish_model_run("run-level", &cancel);
        proposal.settings.security_level = crate::model::SecurityLevel::FullAccess;
        let written = update(&state, &anchor, &workspace_id, &proposal, &[]).unwrap();
        assert_eq!(
            written.settings.security_level,
            crate::model::SecurityLevel::FullAccess
        );
        assert_eq!(cell.get(), crate::model::SecurityLevel::FullAccess);
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
            &state
                .existing_conversation_tasks("other-conversation")
                .unwrap()
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
        let document = default_document();
        let workspace = &document.workspaces[0];
        let conversation = &workspace.conversations[0];
        let store = store(&anchor).unwrap();
        store.put_conversation(&workspace.id, conversation).unwrap();
        let _tasks = state.conversation_tasks(&conversation.id);
        assert!(state.document_store.current_snapshot(&anchor).is_err());
        assert!(delete(&state, &anchor, &workspace.id, &conversation.id).is_err());
        assert!(store.conversation(&conversation.id).unwrap().is_none());
        assert!(state
            .existing_conversation_tasks(&conversation.id)
            .is_none());
    }

    #[test]
    fn document_removal_retires_workspace_tasks_but_not_moved_conversations() {
        let state = AppState::default();
        let previous = default_document();
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
        assert!(std::sync::Arc::ptr_eq(
            &moved,
            &state.existing_conversation_tasks(&removed_id).unwrap()
        ));
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
        proposal.settings.include_app_data_path = true;
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
        assert!(returned.settings.include_app_data_path);
        let mut expected = expected_ids.clone();
        expected.extend(["ctx_after_snapshot".to_owned(), "ctx_live".to_owned()]);
        assert_eq!(context_ids(&returned), expected);
        // A full rewrite would have re-filed the streaming row as settled prose
        // and, with it, the run's claim to replace it in place or mark it
        // interrupted. Its status is what tells the two writes apart.
        assert_eq!(store.reconcile_streaming_in(&stored.id).unwrap(), 1);
        // The snapshot carries the new metadata and no body; the body the
        // renderer loads carries the same timeline.
        let snapshot = state.document_store.current_snapshot(&anchor).unwrap();
        let mirrored = snapshot
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.conversations.iter())
            .find(|conversation| conversation.id == stored.id)
            .unwrap();
        assert!(mirrored.contexts.is_empty());
        assert_eq!(mirrored.title, "renamed mid-run");
        let loaded = load(&anchor, &stored.id).unwrap().unwrap();
        assert_eq!(context_ids(&loaded), expected);
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
