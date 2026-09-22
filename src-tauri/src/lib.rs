#![cfg_attr(test, allow(dead_code))]

mod agents;
mod api;
// The AI SDK sidecar implements the upstream protocol in `aisdk-service/`.
// This layer owns the NDJSON protocol, lifecycle, and event translation.
mod aisdk;
mod app_exit;
mod app_tray;
mod app_update;
mod approval;
mod browser;
#[cfg(all(feature = "browser-dev", not(test)))]
mod browser_dev;
#[cfg(any(test, feature = "browser-dev"))]
mod browser_dev_fixture;
#[cfg(any(test, feature = "browser-dev"))]
#[cfg_attr(not(feature = "browser-dev"), allow(dead_code))]
mod browser_dev_lifecycle;
mod browser_file_preview;
#[allow(dead_code)]
mod browser_profile_data;
mod browser_renderer_mount;
mod browser_webview_lifecycle;
mod browser_window_region;
mod builtin_schemas;
mod cancel;
mod capabilities;
mod capability_seed;
mod catalog;
mod child_environment;
mod chromium_capability;
mod codex_oauth;
mod compaction;
mod console_text;
mod conversation_fork;
mod conversation_store;
mod conversation_template;
mod conversations;
mod decision_model;
mod decision_tools;
mod document_store;
mod environment_prompt;
mod environment_tools;
mod external_open;
mod favicon;
mod file_read_state;
mod fork_requests;
mod git;
mod hooks;
mod http_util;
mod image_attachments;
mod instance_activation;
mod json_edit;
mod kernel_shadow;
/// The `lsp` tool: nine code-navigation operations, ported from Claude Code.
mod lsp;
/// `lsp.json` plus the built-in language-server presets.
mod lsp_config;
/// Language-server processes and the LSP base protocol.
mod lsp_servers;
mod mcp;
mod mcp_config;
mod memory_archive_file;
mod memory_locations;
mod mework_memory;
mod model;
mod model_discovery;
mod model_registry;
mod operation_coordinator;
mod orchestration;
mod path_guard;
mod plan_mode;
mod powershell_host;
/// Resolves a `.mework/launch.json` entry into a running dev server.
mod preview;
/// Reads and validates `.mework/launch.json`. Nothing consumes the whole surface
/// yet — the `preview_*` model tools are a later stage — so the unused public
/// items are allowed rather than deleted and re-ported.
#[allow(dead_code)]
mod preview_launch_config;
#[allow(dead_code)]
mod preview_servers;
mod project_import_trust;
mod project_memory;
mod prompt_profile;
mod prompt_profile_files;
mod push_events;
mod remote_directory;
mod remote_files;
mod remote_lsp;
mod reveal_path;
mod run_environment;
mod run_stream;
mod security;
mod shell_snapshot;
mod shell_task_store;
mod shell_tasks;
mod skills;
mod state;
mod storage;
mod subagent_prompt;
mod subagent_schema;
mod terminal;
mod terminal_lifecycle;
mod token_ledger;
mod token_statistics;
mod tool_attestation;
mod tool_executor;
mod tool_prompt;
mod web_search;
mod wire_history;
mod wire_ledger;
mod workflow;
mod workflow_store;
mod workspace_dirs;
mod workspace_files;
mod workspace_lookup;
mod workspace_set;

#[cfg(not(test))]
include!("../app_commands.rs");

#[cfg(not(test))]
macro_rules! app_invoke_handler {
    ($($command:ident),* $(,)?) => {
        tauri::generate_handler![$($command),*]
    };
}

#[cfg(not(test))]
fn dispatch_app_invoke(
    handler: impl Fn(tauri::ipc::Invoke<tauri::Wry>) -> bool,
    invoke: tauri::ipc::Invoke<tauri::Wry>,
) -> bool {
    handler(invoke)
}

use std::path::{Path, PathBuf};

#[cfg(not(test))]
use std::{collections::HashSet, sync::Arc, time::Duration};

#[cfg(not(test))]
use base64::Engine as _;
#[cfg(not(test))]
use model::{
    ApiKeyStatus, ApiProvider, AppDocument, CapabilityCatalog, ContextItem, Conversation,
    ConversationWorktree, ModelProfile, ModelStreamEvent, RunModelRequest, RunModelResponse,
    SecurityLevel, ToolCategory, ToolDescriptor, ToolExecutionRequest, ToolExecutionResponse,
    Workspace, WorkspaceKind,
};
#[cfg(not(test))]
use operation_coordinator::WorkspaceKey;
#[cfg(not(test))]
use state::AppState;
#[cfg(not(test))]
use tauri::{ipc::Channel, webview::PageLoadEvent, AppHandle, Manager, State, Webview};
#[cfg(not(test))]
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
#[cfg(not(test))]
use workspace_lookup::GitTarget;

#[cfg(not(test))]
const APP_IDENTIFIER: &str = "com.mework.app";
#[cfg(not(test))]
const LEGACY_APP_IDENTIFIER: &str = "com.naiword.agentstudio";

#[cfg(not(test))]
fn is_memory_tool_name(name: &str) -> bool {
    mework_memory::is_memory_tool(name)
}

#[cfg(not(test))]
fn trusted_memory_document(
    app: &AppHandle,
    state: &State<'_, AppState>,
) -> Result<Arc<AppDocument>, String> {
    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.document_store.read(&document_path(app)?)
}

#[cfg(not(test))]
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct MeworkMemoryFileSummary {
    name: String,
    path: String,
    description: Option<String>,
}

#[cfg(not(test))]
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct MeworkMemoryTierOverview {
    tier: String,
    available: bool,
    root_path: Option<String>,
    instructions: Option<MeworkMemoryFileSummary>,
    index: Option<MeworkMemoryFileSummary>,
    documents: Vec<MeworkMemoryFileSummary>,
}

#[cfg(not(test))]
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct MeworkMemoryOverview {
    global: MeworkMemoryTierOverview,
    project: MeworkMemoryTierOverview,
}

#[cfg(not(test))]
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct MeworkMemoryFile {
    tier: String,
    name: String,
    path: String,
    content: String,
}

#[cfg(not(test))]
fn mework_memory_workspace_root(
    app: &AppHandle,
    state: &State<'_, AppState>,
    workspace_id: Option<&str>,
) -> Result<Option<PathBuf>, String> {
    let Some(workspace_id) = workspace_id.map(str::trim).filter(|id| !id.is_empty()) else {
        return Ok(None);
    };
    let document = trusted_memory_document(app, state)?;
    let workspace = document
        .workspaces
        .iter()
        .find(|workspace| workspace.id == workspace_id)
        .ok_or_else(|| "找不到指定工作区，无法解析项目记忆目录".to_owned())?;
    match workspace.kind {
        WorkspaceKind::Directory if !workspace.path.trim().is_empty() => {
            Ok(Some(PathBuf::from(&workspace.path)))
        }
        WorkspaceKind::Directory | WorkspaceKind::Temporary | WorkspaceKind::Unsupported => {
            Ok(None)
        }
    }
}

#[cfg(not(test))]
fn mework_memory_roots(
    app: &AppHandle,
    state: &State<'_, AppState>,
    workspace_id: Option<&str>,
) -> Result<mework_memory::MemoryRoots, String> {
    let workspace = mework_memory_workspace_root(app, state, workspace_id)?;
    Ok(mework_memory::MemoryRoots::resolve(
        dirs::home_dir().as_deref(),
        workspace.as_deref(),
    ))
}

#[cfg(not(test))]
fn mework_memory_tier(raw: &str) -> Result<mework_memory::MemoryTier, String> {
    match raw {
        "global" => Ok(mework_memory::MemoryTier::Global),
        "project" => Ok(mework_memory::MemoryTier::Project),
        _ => Err("记忆层级必须是 global 或 project".into()),
    }
}

#[cfg(not(test))]
fn mework_memory_root<'a>(
    roots: &'a mework_memory::MemoryRoots,
    tier: mework_memory::MemoryTier,
) -> Result<&'a mework_memory::MemoryRoot, String> {
    match tier {
        mework_memory::MemoryTier::Global => roots
            .global
            .as_ref()
            .ok_or_else(|| "全局记忆不可用：宿主没有解析到用户主目录".to_owned()),
        mework_memory::MemoryTier::Project => roots
            .project
            .as_ref()
            .ok_or_else(|| "项目记忆不可用：当前工作区没有可用的磁盘目录".to_owned()),
    }
}

#[cfg(not(test))]
fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(not(test))]
fn mework_memory_tier_overview(
    tier: mework_memory::MemoryTier,
    root: Option<&mework_memory::MemoryRoot>,
) -> MeworkMemoryTierOverview {
    let Some(root) = root else {
        return MeworkMemoryTierOverview {
            tier: tier.as_str().to_owned(),
            available: false,
            root_path: None,
            instructions: None,
            index: None,
            documents: Vec::new(),
        };
    };
    let instructions_path = root.instructions_path();
    let index_path = root.index_path();
    let descriptions = mework_memory::read_index(root)
        .into_iter()
        .map(|entry| (entry.name, entry.description))
        .collect::<std::collections::HashMap<_, _>>();
    let mut names = std::fs::read_dir(root.memory_dir())
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            if !entry.file_type().ok()?.is_file() {
                return None;
            }
            let raw = entry.file_name().to_string_lossy().into_owned();
            let normalized = mework_memory::normalize_document_name(&raw).ok()?;
            (normalized == raw).then_some(normalized)
        })
        .collect::<Vec<_>>();
    names.sort_by_key(|name| name.to_lowercase());
    names.dedup();
    let documents = names
        .into_iter()
        .map(|name| MeworkMemoryFileSummary {
            path: display_path(&root.memory_dir().join(&name)),
            description: descriptions.get(&name).cloned(),
            name,
        })
        .collect();
    MeworkMemoryTierOverview {
        tier: tier.as_str().to_owned(),
        available: true,
        root_path: instructions_path.parent().map(display_path),
        instructions: Some(MeworkMemoryFileSummary {
            name: mework_memory::INSTRUCTIONS_NAME.to_owned(),
            path: display_path(&instructions_path),
            description: None,
        }),
        index: Some(MeworkMemoryFileSummary {
            name: mework_memory::INDEX_NAME.to_owned(),
            path: display_path(&index_path),
            description: None,
        }),
        documents,
    }
}

#[cfg(not(test))]
#[tauri::command]
fn mework_memory_overview(
    app: AppHandle,
    state: State<'_, AppState>,
    workspace_id: Option<String>,
) -> Result<MeworkMemoryOverview, String> {
    let roots = mework_memory_roots(&app, &state, workspace_id.as_deref())?;
    Ok(MeworkMemoryOverview {
        global: mework_memory_tier_overview(
            mework_memory::MemoryTier::Global,
            roots.global.as_ref(),
        ),
        project: mework_memory_tier_overview(
            mework_memory::MemoryTier::Project,
            roots.project.as_ref(),
        ),
    })
}

#[cfg(not(test))]
fn mework_memory_named_path(
    root: &mework_memory::MemoryRoot,
    name: &str,
) -> Result<(String, PathBuf), String> {
    match name {
        mework_memory::INSTRUCTIONS_NAME => Ok((name.to_owned(), root.instructions_path())),
        mework_memory::INDEX_NAME => Ok((name.to_owned(), root.index_path())),
        _ => {
            let normalized = mework_memory::normalize_document_name(name)?;
            Ok((normalized.clone(), root.memory_dir().join(normalized)))
        }
    }
}

#[cfg(not(test))]
#[tauri::command]
fn mework_memory_read_file(
    app: AppHandle,
    state: State<'_, AppState>,
    workspace_id: Option<String>,
    tier: String,
    name: String,
) -> Result<MeworkMemoryFile, String> {
    let tier = mework_memory_tier(&tier)?;
    let roots = mework_memory_roots(&app, &state, workspace_id.as_deref())?;
    let root = mework_memory_root(&roots, tier)?;
    let (name, path) = mework_memory_named_path(root, &name)?;
    let maximum = if name == mework_memory::INSTRUCTIONS_NAME {
        128 * 1024
    } else if name == mework_memory::INDEX_NAME {
        64 * 1024
    } else {
        mework_memory::MAX_DOCUMENT_BYTES
    };
    let bytes = memory_archive_file::read_bounded_nofollow(&path, maximum)
        .map_err(|_| format!("无法读取记忆文件 {name}"))?;
    let content =
        String::from_utf8(bytes).map_err(|_| format!("记忆文件 {name} 不是有效的 UTF-8 文本"))?;
    Ok(MeworkMemoryFile {
        tier: tier.as_str().to_owned(),
        name,
        path: display_path(&path),
        content,
    })
}

#[cfg(not(test))]
#[tauri::command]
fn mework_memory_write_file(
    app: AppHandle,
    state: State<'_, AppState>,
    workspace_id: Option<String>,
    tier: String,
    name: String,
    content: String,
) -> Result<MeworkMemoryFile, String> {
    let tier = mework_memory_tier(&tier)?;
    let roots = mework_memory_roots(&app, &state, workspace_id.as_deref())?;
    let root = mework_memory_root(&roots, tier)?;
    let (name, path) = mework_memory_named_path(root, &name)?;
    let maximum = if name == mework_memory::INSTRUCTIONS_NAME {
        128 * 1024
    } else if name == mework_memory::INDEX_NAME {
        64 * 1024
    } else {
        mework_memory::MAX_DOCUMENT_BYTES
    };
    if content.len() > maximum {
        return Err(format!("记忆文件超过 {maximum} 字节上限"));
    }
    if content.contains('\0') {
        return Err("记忆文件不能包含空字符".into());
    }
    if name == mework_memory::INSTRUCTIONS_NAME {
        std::fs::create_dir_all(
            path.parent()
                .ok_or_else(|| "记忆指令文件没有父目录".to_owned())?,
        )
        .map_err(|_| "无法创建记忆目录".to_owned())?;
        memory_archive_file::write_all_nofollow(&path, content.as_bytes(), maximum)
            .map_err(|_| format!("无法写入记忆文件 {name}"))?;
    } else if name == mework_memory::INDEX_NAME {
        std::fs::create_dir_all(root.memory_dir()).map_err(|_| "无法创建记忆目录".to_owned())?;
        memory_archive_file::write_all_nofollow(&path, content.as_bytes(), maximum)
            .map_err(|_| format!("无法写入记忆文件 {name}"))?;
    } else {
        let existing = mework_memory::read_document(root, &name)?;
        if existing.content.is_empty() {
            return Err("空的主题记忆请先在外部编辑器中写入内容，再从设置中编辑".into());
        }
        let description = mework_memory::read_index(root)
            .into_iter()
            .find(|entry| entry.name == name)
            .map(|entry| entry.description)
            .filter(|description| !description.trim().is_empty())
            .unwrap_or_else(|| name.trim_end_matches(".md").to_owned());
        mework_memory::edit_document(root, &name, &existing.content, &content, &description)?;
    }
    Ok(MeworkMemoryFile {
        tier: tier.as_str().to_owned(),
        name,
        path: display_path(&path),
        content,
    })
}

#[cfg(not(test))]
#[tauri::command]
fn mework_memory_delete_document(
    app: AppHandle,
    state: State<'_, AppState>,
    workspace_id: Option<String>,
    tier: String,
    name: String,
) -> Result<(), String> {
    let tier = mework_memory_tier(&tier)?;
    let roots = mework_memory_roots(&app, &state, workspace_id.as_deref())?;
    mework_memory::delete_document(mework_memory_root(&roots, tier)?, &name)
}

#[cfg(not(test))]
fn current_project_import_workspace_root(
    app: &AppHandle,
    state: &State<'_, AppState>,
    workspace_id: &str,
) -> Result<Option<PathBuf>, String> {
    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let document = state.document_store.read(&document_path(app)?)?;
    let Some(workspace) = document
        .workspaces
        .iter()
        .find(|workspace| workspace.id == workspace_id)
    else {
        // Orphaned decisions remain listable/revocable by their safe metadata.
        return Ok(None);
    };
    match workspace.kind {
        WorkspaceKind::Directory => {
            Ok((!workspace.path.trim().is_empty()).then(|| PathBuf::from(workspace.path.clone())))
        }
        // Temporary workspace roots are conversation-specific. Their records
        // are still visible and revocable, but cannot be labelled active from
        // only a workspace ID.
        WorkspaceKind::Temporary | WorkspaceKind::Unsupported => Ok(None),
    }
}

#[cfg(not(test))]
#[tauri::command]
fn project_memory_list_import_trust(
    app: AppHandle,
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<Vec<project_import_trust::ProjectImportTrustSummary>, String> {
    let workspace_root = current_project_import_workspace_root(&app, &state, &workspace_id)?;
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法解析应用数据目录: {error}"))?;
    let store = project_import_trust::EncryptedProjectImportTrustStore::open(app_data);
    let _guard = state
        .project_import_trust_lock
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    project_import_trust::list_project_import_trust(
        &store,
        &workspace_id,
        workspace_root.as_deref(),
    )
}

#[cfg(not(test))]
#[tauri::command]
fn project_memory_revoke_import_trust(
    app: AppHandle,
    state: State<'_, AppState>,
    workspace_id: String,
    record_id: String,
) -> Result<(), String> {
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法解析应用数据目录: {error}"))?;
    let store = project_import_trust::EncryptedProjectImportTrustStore::open(app_data);
    let _guard = state
        .project_import_trust_lock
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    project_import_trust::revoke_project_import_trust(&store, &workspace_id, &record_id)
}

#[cfg(not(test))]
#[tauri::command]
fn load_document(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<std::sync::Arc<AppDocument>, String> {
    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = document_path(&app)?;
    let mut document = (*state.document_store.read(&path)?).clone();
    // The snapshot the store serves carries the conversation bodies it had at
    // the last conversation *command* — the user message that opened the round.
    // Everything the run itself wrote went straight to the conversation store,
    // so a renderer that reloads mid-session (a page refresh, or the authority
    // fetch after a rejected save) would otherwise be handed a conversation
    // whose last answer is missing, and would go on running from that body.
    storage::refresh_conversation_bodies(&path, &mut document);
    let document = std::sync::Arc::new(document);
    let app_data = path
        .parent()
        .ok_or_else(|| "数据文档没有应用数据父目录".to_owned())?;
    if let Err(error) = workspace_dirs::reconcile_temporary_workspaces(app_data, &document) {
        eprintln!("加载文档时同步临时工作区失败，将在下次保存或加载时重试：{error}");
    }
    Ok(document)
}

/// Copies a conversation's history through `throughContextId` into a branch.
///
/// The source contexts are read from the host's own committed document, never
/// from the renderer, so the returned copy is history the host previously
/// validated. Every id is regenerated and every tool result is re-attested
/// against `targetConversationId`; see [`conversation_fork`] for why that is
/// the honest way to satisfy the receipt boundary rather than a way around it.
///
/// The caller persists the returned contexts as part of an ordinary
/// `save_document`. Receipts issued here are consumed by that save.
#[cfg(not(test))]
#[tauri::command]
fn fork_conversation_contexts(
    app: AppHandle,
    state: State<'_, AppState>,
    workspace_id: String,
    source_conversation_id: String,
    target_conversation_id: String,
    through_context_id: String,
) -> Result<Vec<crate::model::ContextItem>, String> {
    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = document_path(&app)?;
    let document = state.document_store.read(&path)?;
    let workspace = document
        .workspaces
        .iter()
        .find(|candidate| candidate.id == workspace_id)
        .ok_or_else(|| format!("工作区 {workspace_id} 不存在"))?;
    // The renderer flushes the newly created branch before calling us, because
    // we copy from the committed document. So an existing-but-empty target is
    // the expected state, not a duplicate-creation attempt.
    conversation_fork::validate_fork_target(
        workspace
            .conversations
            .iter()
            .find(|candidate| candidate.id == target_conversation_id),
        &target_conversation_id,
    )?;
    let source = workspace
        .conversations
        .iter()
        .find(|candidate| candidate.id == source_conversation_id)
        .ok_or_else(|| format!("源对话 {source_conversation_id} 不存在"))?;
    // Use the same authoritative source as model runs. The fork point is a user
    // message, so earlier contexts have already been committed to the snapshot.
    let source_contexts =
        authoritative_contexts(stored_conversation(&path, &source_conversation_id), source);

    conversation_fork::fork_contexts(
        &state,
        &conversation_fork::ForkRequest {
            workspace_path: &workspace.path,
            target_conversation_id: &target_conversation_id,
            through_context_id: &through_context_id,
        },
        &source_contexts,
        |prefix| format!("{prefix}_{}", uuid::Uuid::new_v4().simple()),
    )
}

/// Lists saved conversation templates without their bodies.
#[cfg(not(test))]
#[tauri::command]
fn list_conversation_templates(
    app: AppHandle,
) -> Result<Vec<crate::conversation_store::ConversationTemplateSummary>, String> {
    let path = document_path(&app)?;
    conversations::store(&path)?.templates()
}

/// Instantiates a template for a conversation, returning contexts the caller
/// persists through an ordinary conversation update.
///
/// Every id is rewritten and every tool result re-attested for the target, so
/// the returned body is persistable where a renderer-side copy would be refused.
/// Receipts issued here are consumed by that update.
#[cfg(not(test))]
#[tauri::command]
fn apply_conversation_template(
    app: AppHandle,
    state: State<'_, AppState>,
    workspace_id: String,
    template_id: String,
    target_conversation_id: String,
) -> Result<Vec<crate::model::ContextItem>, String> {
    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = document_path(&app)?;
    let document = state.document_store.read(&path)?;
    let workspace = document
        .workspaces
        .iter()
        .find(|candidate| candidate.id == workspace_id)
        .ok_or_else(|| format!("工作区 {workspace_id} 不存在"))?;
    let store = conversations::store(&path)?;
    let contexts = store.template_contexts(&template_id)?;
    conversation_template::instantiate(&state, &workspace.path, &target_conversation_id, &contexts)
}

/// Reads a template's body for display, exactly as it was stored.
///
/// The opposite of [`apply_conversation_template`] in every way that matters:
/// nothing is rewritten, nothing is re-attested, and the ids and receipts that
/// come back belong to the conversation the template was captured from. That is
/// precisely why this is safe to hand the renderer — a body whose receipts name
/// another conversation is refused by `validate_conversation_tool_cards`, so the
/// preview cannot become history no matter what the renderer does with it. It is
/// to be read and discarded.
///
/// An unknown id is an empty body rather than an error, matching every other
/// reader of a template id: a dangling id everywhere means "no template".
#[cfg(not(test))]
#[tauri::command]
fn preview_conversation_template(
    app: AppHandle,
    template_id: String,
) -> Result<Vec<crate::model::ContextItem>, String> {
    let path = document_path(&app)?;
    conversations::store(&path)?.template_contexts(&template_id)
}

/// Writes a renderer-edited body onto a template, creating it if that id has no
/// row yet.
///
/// A preset binds at most one template, by id, and that id is the renderer's to
/// choose, so the first write onto a freshly bound id is an ordinary edit rather
/// than an error. An unknown id reads as an empty body everywhere, which is the
/// whole of the evidence there is no row; the row created here carries an empty
/// name, because nothing renders template names any more.
///
/// This is the one door a renderer has into a template body, and it is load
/// bearing: applying a template runs its contexts through
/// [`conversation_template::instantiate`], which re-attests every tool card
/// unconditionally for the target conversation. Anything that reaches a template
/// body therefore becomes, on the next Apply, a card this application attests to
/// having run itself. Nothing downstream will catch a forgery, so the check has
/// to happen here.
///
/// The check is [`conversation_template::normalize_tool_cards`], not a refusal
/// of everything edited: the host keeps the evidence it alone can vouch for. A
/// tool card the template already had keeps its stored `success`, `diff`,
/// `executedAt`, `durationMs`, `round`, `modelTurnId`, `subagent` and
/// `createdAt`, and takes the submitted tool name, input, output text and
/// images; a card the template never had is minted text-only. That is
/// exactly the power the timeline editor already has through
/// `conversations::attest_edited_tool_context` and
/// `conversations::attest_inserted_tool_context`, and no stronger. The
/// deliberate forgeries — swapping a tool name, adding or reordering images,
/// editing a card that carries a subagent record — are refused. Prose contexts —
/// system, user, assistant, reasoning — carry no attestation and no claim about
/// what this application did, so they are the renderer's to edit, insert, delete
/// and reorder freely.
#[cfg(not(test))]
#[tauri::command]
fn update_conversation_template(
    app: AppHandle,
    state: State<'_, AppState>,
    template_id: String,
    contexts: Vec<crate::model::ContextItem>,
) -> Result<crate::conversation_store::ConversationTemplateSummary, String> {
    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = document_path(&app)?;
    let store = conversations::store(&path)?;
    // An unknown id reads as an empty body, and that empty read is what tells a
    // row that has never been written from one that merely lost its body.
    let stored = store.template_contexts(&template_id)?;
    if contexts.is_empty() {
        return Err("对话模板至少要保留一条消息".into());
    }
    let contexts = conversation_template::normalize_tool_cards(&stored, contexts)?;
    crate::storage::validate_template_contexts(&contexts)?;
    if stored.is_empty() {
        // A new row goes through `put_template` because it is the only writer
        // that can create one. It also writes the name, which is empty on
        // purpose: a template's name is no longer anything a renderer shows.
        store.put_template(&template_id, "", &contexts)
    } else {
        store.put_template_contexts(&template_id, &contexts)
    }
}

/// Deletes a template. Citing ids are left to dangle, which everywhere resolves
/// as "no template" rather than as an error.
#[cfg(not(test))]
#[tauri::command]
fn delete_conversation_template(
    app: AppHandle,
    state: State<'_, AppState>,
    template_id: String,
) -> Result<(), String> {
    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = document_path(&app)?;
    conversations::store(&path)?.delete_template(&template_id)
}

/// Creates a conversation. The host is the sole writer of conversation bodies,
/// so creation is an explicit command rather than a whole-document save.
#[cfg(not(test))]
#[tauri::command]
fn create_conversation(
    app: AppHandle,
    state: State<'_, AppState>,
    workspace_id: String,
    conversation: Conversation,
) -> Result<Conversation, String> {
    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = document_path(&app)?;
    conversations::create(&state, &path, &workspace_id, &conversation)
}

#[cfg(not(test))]
#[tauri::command]
fn delete_conversation(
    app: AppHandle,
    state: State<'_, AppState>,
    workspace_id: String,
    conversation_id: String,
) -> Result<(), String> {
    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = document_path(&app)?;
    conversations::delete(&state, &path, &workspace_id, &conversation_id)?;
    // A fork card whose source is gone can no longer be performed.
    fork_requests::retract_requests_for_conversation(&state, &conversation_id);
    Ok(())
}

/// Updates a conversation. `expected_context_ids` identifies the main-timeline
/// contexts seen by the renderer. Body changes are accepted only when it matches
/// the host and no turn is running; metadata is always accepted. Returns the
/// host-authoritative conversation body.
#[cfg(not(test))]
#[tauri::command]
fn update_conversation(
    app: AppHandle,
    state: State<'_, AppState>,
    workspace_id: String,
    conversation: Conversation,
    expected_context_ids: Vec<String>,
) -> Result<Conversation, String> {
    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = document_path(&app)?;
    conversations::update(
        &state,
        &path,
        &workspace_id,
        &conversation,
        &expected_context_ids,
    )
}

#[cfg(not(test))]
#[tauri::command]
fn reorder_conversations(
    app: AppHandle,
    state: State<'_, AppState>,
    workspace_id: String,
    conversation_ids: Vec<String>,
) -> Result<(), String> {
    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = document_path(&app)?;
    conversations::reorder(&state, &path, &workspace_id, &conversation_ids)
}

#[cfg(not(test))]
#[tauri::command]
fn attest_edited_tool_context(
    app: AppHandle,
    state: State<'_, AppState>,
    request: conversations::AttestEditedToolContextRequest,
) -> Result<conversations::AttestEditedToolContextResponse, String> {
    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = document_path(&app)?;
    conversations::attest_edited_tool_context(&state, &path, request)
}

#[cfg(not(test))]
#[tauri::command]
fn attest_inserted_tool_context(
    app: AppHandle,
    state: State<'_, AppState>,
    request: conversations::AttestInsertedToolContextRequest,
) -> Result<conversations::AttestEditedToolContextResponse, String> {
    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = document_path(&app)?;
    conversations::attest_inserted_tool_context(&state, &path, request)
}

/// Loads an authoritative conversation body after turn settlement.
#[cfg(not(test))]
#[tauri::command]
fn load_conversation(
    app: AppHandle,
    state: State<'_, AppState>,
    conversation_id: String,
) -> Result<Option<Conversation>, String> {
    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = document_path(&app)?;
    conversations::load(&path, &conversation_id)
}

/// Loads the plan document the plan panel renders. `None` means the
/// conversation has never had one written.
#[cfg(not(test))]
#[tauri::command]
fn load_conversation_plan(
    app: AppHandle,
    state: State<'_, AppState>,
    conversation_id: String,
) -> Result<Option<model::ConversationPlan>, String> {
    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = document_path(&app)?;
    conversations::store(&path)?.conversation_plan(&conversation_id)
}

/// Lists the conversation's recorded timeline history, oldest first. Each entry is
/// one moment the trunk changed: a settled backend request, or a change the
/// renderer committed.
#[cfg(not(test))]
#[tauri::command]
fn list_timeline_events(
    app: AppHandle,
    state: State<'_, AppState>,
    conversation_id: String,
) -> Result<Vec<conversation_store::TimelineEvent>, String> {
    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = document_path(&app)?;
    conversations::store(&path)?.timeline_events(&conversation_id)
}

/// Rebuilds the trunk as it stood once `seq` applied.
#[cfg(not(test))]
#[tauri::command]
fn load_timeline_snapshot(
    app: AppHandle,
    state: State<'_, AppState>,
    conversation_id: String,
    seq: i64,
) -> Result<Vec<model::ContextItem>, String> {
    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = document_path(&app)?;
    conversations::store(&path)?.timeline_snapshot(&conversation_id, seq)
}

/// Lists one ledger's recorded outgoing requests, oldest first. One entry is one
/// payload this host handed to the model runner — a round, a retry of a round, or
/// a host-minted native web-tool call.
///
/// `owners` picks the ledger: absent is the conversation's own trunk, and a list
/// of child-agent names is those agents'.
#[cfg(not(test))]
#[tauri::command]
fn list_wire_requests(
    app: AppHandle,
    state: State<'_, AppState>,
    conversation_id: String,
    owners: Option<Vec<String>>,
) -> Result<Vec<conversation_store::WireRequestSummary>, String> {
    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = document_path(&app)?;
    conversations::store(&path)?.wire_requests(&conversation_id, owners.as_deref())
}

/// Loads one recorded request: its envelope and every part it carried, in wire
/// order.
#[cfg(not(test))]
#[tauri::command]
fn load_wire_request(
    app: AppHandle,
    state: State<'_, AppState>,
    conversation_id: String,
    seq: i64,
) -> Result<Option<conversation_store::WireRequestDetail>, String> {
    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = document_path(&app)?;
    conversations::store(&path)?.wire_request(&conversation_id, seq)
}

/// Returns built-in gateway usage statistics. Activity is aggregated from the
/// conversation database and token usage from the ledger, both in UTC-hour bins.
///
/// Runs in `spawn_blocking` because historical SQLite scans must not block the
/// synchronous invoke handler.
#[cfg(not(test))]
#[tauri::command]
async fn token_usage_statistics(
    app: AppHandle,
) -> Result<crate::token_statistics::UsageStatistics, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let path = document_path(&app)?;
        Ok(crate::token_statistics::collect(&path))
    })
    .await
    .map_err(|error| format!("读取使用统计的后台任务失败: {error}"))?
}

/// Backfills historical usage. Events carry derived idempotency keys; the host
/// drops events beyond the cutoff and `token_ledger` enforces the entry limit.
#[cfg(not(test))]
#[tauri::command]
async fn backfill_token_usage(
    app: AppHandle,
    events: Vec<crate::token_ledger::UsageEvent>,
) -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let path = document_path(&app)?;
        crate::token_statistics::backfill(&path, &events)
    })
    .await
    .map_err(|error| format!("补历史用量的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn save_document(
    app: AppHandle,
    state: State<'_, AppState>,
    document: AppDocument,
    durable: Option<bool>,
) -> Result<(), String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        save_document_blocking(app, state, document, durable.unwrap_or(false))
    })
    .await
    .map_err(|error| format!("保存文档后台任务失败: {error}"))?
}

/// Durability barrier: resolves after every commit so far is on disk. The
/// desktop flow relies on the exit hook; tests and the browser-dev harness use
/// this to assert on-disk state deterministically.
#[cfg(not(test))]
#[tauri::command]
async fn flush_document_saves(state: State<'_, AppState>) -> Result<(), String> {
    let store = state.document_store.clone();
    tauri::async_runtime::spawn_blocking(move || store.flush(std::time::Duration::from_secs(30)))
        .await
        .map_err(|error| format!("等待文档落盘的后台任务失败: {error}"))?
}

/// The renderer's single long-lived push-event subscription (see
/// `push_events`): called once per renderer document load, latest wins.
#[cfg(not(test))]
#[tauri::command]
fn subscribe_app_events(
    state: State<'_, AppState>,
    on_event: Channel<push_events::AppPushEvent>,
) -> Result<(), String> {
    state.push_events.subscribe(on_event);
    Ok(())
}

/// Bridges document-store background write transitions onto the push-event
/// hub so a failing disk reaches the renderer immediately instead of on its
/// next save. Installed once per process by both runtime setups.
#[cfg(not(test))]
fn install_background_write_failure_reporting(state: &AppState) {
    let hub = state.push_events.clone();
    state
        .document_store
        .set_write_failure_observer(Box::new(move |failure| {
            hub.publish(match failure {
                Some(message) => push_events::AppPushEvent::DocumentWriteFailure { message },
                None => push_events::AppPushEvent::DocumentWriteRecovered,
            });
        }));
    // A shell command starts because the model ran a tool, never because the
    // renderer asked for one, so the same hub is the only way its row can reach
    // the task sidebar while it is still running.
    state.shell_tasks.attach_events(state.push_events.clone());
    // Same argument for a dev server, plus one the shell rows do not have: a
    // server also *ends* without the renderer asking — `preview_stop`, a crash,
    // a supervisor noticing the process is gone — and the preview page showing
    // it has to come down with it.
    let preview_hub = state.push_events.clone();
    state
        .preview_servers
        .set_change_listener(std::sync::Arc::new(move |stopped: &[String]| {
            preview_hub.publish(push_events::AppPushEvent::PreviewServersChanged {
                stopped: stopped.to_vec(),
            });
        }));
}

#[cfg(not(test))]
fn save_document_blocking(
    app: AppHandle,
    state: AppState,
    document: AppDocument,
    durable: bool,
) -> Result<(), String> {
    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = document_path(&app)?;
    let previous = state.document_store.read(&path)?;
    let app_data = path
        .parent()
        .ok_or_else(|| "数据文档没有应用数据父目录".to_owned())?;
    let retained = document
        .assets
        .api_providers
        .iter()
        .map(|provider| provider.id.trim())
        .collect::<HashSet<_>>();
    // Credential bindings are hashed and cannot be enumerated after a provider
    // row disappears, so retain IDs of deleted providers.
    let removed = previous
        .assets
        .api_providers
        .iter()
        .filter(|provider| !retained.contains(provider.id.trim()))
        .map(|provider| provider.id.trim().to_owned())
        .collect::<Vec<_>>();
    // Renderer payloads contain no conversation bodies. Determine lost workspace
    // bindings from `previous`; comparing payload conversations would mark every
    // save as invalid and make the exclusive fence permanently true.
    let workspace_lifecycle_requires_exclusive =
        terminal_lifecycle::requires_exclusive_workspace_lifecycle_fence(&previous, &document);
    let invalidated_terminal_conversations =
        terminal_lifecycle::workspace_rebound_conversations(&previous, &document);
    // This non-blocking reservation is taken while document authority is
    // locked, so workspace-bound operations cannot cross a transition that
    // invalidates an existing owner. Purely adding a conversation to an
    // unchanged workspace has no existing owner to invalidate and deliberately
    // stays concurrent with unrelated runs.
    let _workspace_lifecycle_operation = workspace_lifecycle_requires_exclusive
        .then(|| state.begin_mutation())
        .transpose()
        .map_err(|_| {
            "仍有模型、工具、终端命令或 Agent 管理操作正在进行；工作区变更已取消，请稍后重试"
                .to_owned()
        })?;

    // The returned canonical document is what gets committed below, so the
    // persisted bytes always match what was validated here.
    let storage::PreparedSaveTransition {
        document: canonical,
        quarantined,
    } = storage::prepare_save_transition(&previous, &document, &state)?;
    // Every save takes the named-agent write fence (even when the renderer
    // believes definitions are unchanged), so no concurrent named-agent
    // hook/tool/provider read can cross the document publication boundary.
    let definition_authority = match state.definition_authority_lock.write() {
        Ok(authority) => authority,
        Err(_) => return Err("命名 Agent 定义授权锁已损坏；文档尚未保存".to_owned()),
    };
    // Roles live in each preset and conversation, so "did any role change" is
    // a question about the whole document rather than one list.
    let definitions_changed = crate::storage::agent_definitions_differ(&previous, &canonical);
    // The canonical document was fully validated above; commit swaps the in-memory
    // authority synchronously and persists (serialize + fsync) on the background
    // writer so the renderer's save await no longer pays the disk pipeline. An
    // error here means an EARLIER background write failed — this commit is
    // applied and queued regardless, and the error is surfaced at the end so
    // the renderer retries persistence.
    let deferred_write_error = match state.document_store.commit(&path, canonical.clone()) {
        Ok(()) => None,
        Err(error) => {
            // `commit` may report an older deferred writer failure after
            // installing this snapshot, but lease/path failures happen before
            // that swap. Confirm which case occurred before treating this
            // snapshot as the published one.
            let committed = state.document_store.read(&path).map_err(|read_error| {
                format!("文档提交失败且无法确认内存快照：{error}；{read_error}")
            })?;
            if committed.as_ref() != &canonical {
                return Err(format!("文档提交失败且新快照未成为内存 authority：{error}"));
            }
            Some(error)
        }
    };
    let durability_error = if definitions_changed || durable {
        // Definition identity is always security authority. Selected workflow
        // transitions (such as queued-message promotion) also request this
        // barrier so no model run starts from a snapshot that exists only in
        // process memory. A successful flush supersedes an older deferred
        // writer error because this exact canonical snapshot is already the
        // in-memory authority and every commit through it is now on disk.
        state
            .document_store
            .flush(std::time::Duration::from_secs(30))
            .err()
            .map(|error| {
                if definitions_changed {
                    format!(
                        "命名 Agent 定义已成为当前内存 authority，但未能安全写入磁盘；\
                         本次保存已拒绝完成并会在后台重试：{error}"
                    )
                } else {
                    format!(
                        "文档已成为当前内存 authority，但耐久保存未完成；\
                         本次工作流不会继续：{error}"
                    )
                }
            })
    } else {
        None
    };
    drop(definition_authority);
    // The tray menu is host-rendered chrome, so it follows the language the
    // renderer just mirrored into the document.
    if previous.global_settings.resolved_app_language
        != canonical.global_settings.resolved_app_language
    {
        app_tray::apply_language(&app, canonical.global_settings.resolved_app_language);
    }
    // The snapshot that dropped these cards is now authoritative, so say so.
    // Publishing before the commit would announce a loss that a later failure
    // could still roll back.
    if !quarantined.is_empty() {
        state
            .push_events
            .publish(push_events::AppPushEvent::ToolContextsQuarantined {
                contexts: quarantined
                    .into_iter()
                    .filter_map(|entry| {
                        let replacement = canonical
                            .workspaces
                            .iter()
                            .find(|workspace| workspace.id == entry.workspace_id)?
                            .conversations
                            .iter()
                            .find(|conversation| conversation.id == entry.conversation_id)
                            .and_then(|conversation| {
                                conversation
                                    .contexts
                                    .iter()
                                    .chain(
                                        conversation
                                            .branches
                                            .iter()
                                            .flat_map(|branch| branch.contexts.iter()),
                                    )
                                    .find(|context| context.id() == entry.context_id)
                            })?
                            .clone();
                        Some(push_events::QuarantinedToolContext {
                            workspace_id: entry.workspace_id,
                            conversation_id: entry.conversation_id,
                            context_id: entry.context_id,
                            tool_name: entry.tool_name,
                            replacement,
                        })
                    })
                    .collect(),
            });
    }
    // The committed snapshot is now authoritative in memory, but its disk
    // write is intentionally asynchronous. Move removed attachment references
    // only into the reversible quarantine: an older on-disk snapshot or an
    // already-running model/tool request can still restore the bytes by ID.
    // Template bodies live outside the document and are pinned explicitly; a
    // reclaim that cannot read them is skipped rather than run half-informed.
    match conversations::template_image_ids(&path) {
        Err(error) => {
            eprintln!("文档已保存，但无法读取模板图片引用，本次不回收图片附件：{error}");
        }
        Ok(pinned) => {
            if let Err(error) = image_attachments::ImageAttachmentStore::new(app_data)
                .reconcile_transition(&previous, &canonical, &pinned)
            {
                eprintln!("文档已保存，但图片附件隔离回收将在下次保存或启动时重试：{error}");
            }
        }
    }
    state.retire_removed_conversation_tasks(&previous, &canonical);
    // Workflow records belong to their conversation; delete their directories
    // with removed conversations. Startup cleanup handles failed deletions.
    workflow_store::remove_removed_conversation_runs(app_data, &previous, &canonical);

    // Terminal shells retain the cwd and workspace coordinator key captured when they were
    // launched. Invalidate only owners whose persisted workspace binding changed or disappeared;
    // otherwise a moved conversation could continue writing in its former workspace after this
    // lifecycle fence is released.
    state.terminals.close_conversations(
        invalidated_terminal_conversations
            .iter()
            .map(String::as_str),
    );
    state.mcp_sessions.evict_conversations(
        invalidated_terminal_conversations
            .iter()
            .map(String::as_str),
    );
    // Defense in depth for malformed legacy documents: no process-local terminal may outlive its
    // owning conversation even if a future binding projection fails to classify that removal.
    state.terminals.close_missing(
        canonical
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.conversations.iter())
            .map(|conversation| conversation.id.as_str()),
    );
    // Finished shell commands are retained so the task sidebar can answer "did that build pass".
    // A conversation that no longer exists is the one thing that makes that question meaningless.
    state.shell_tasks.try_forget_missing(
        canonical
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.conversations.iter())
            .map(|conversation| conversation.id.as_str()),
    )?;
    // The same event retires the shell session — its snapshot file and its
    // remembered directory — for a conversation that no longer exists.
    state.forget_shell_sessions(
        canonical
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.conversations.iter())
            .map(|conversation| conversation.id.as_str()),
    );
    // And what its model had read: the file write guards' record is as
    // process-local as the shell session and goes with the conversation.
    state.file_read_state.retain_conversations(
        &canonical
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.conversations.iter())
            .map(|conversation| conversation.id.clone())
            .collect(),
    );

    let temporary_workspace_result =
        workspace_dirs::reconcile_temporary_workspaces(app_data, &canonical);
    for provider_id in removed {
        if let Err(error) = api::delete_api_key(&provider_id) {
            eprintln!("提供商已删除，但清理其 API Key 失败（{provider_id}）：{error}");
        }
        // A Codex row also owns an encrypted token file; the master key above is
        // gone, so the file is garbage either way, but do not leave it behind.
        if let Err(error) = codex_oauth::host().remove(&provider_id) {
            eprintln!("提供商已删除，但清理其 Codex 登录状态失败（{provider_id}）：{error}");
        }
    }
    // Do not remove search-provider keys on save: disabling a catalog entry does
    // not change its credential identity. Remove keys only explicitly or on reset.
    if durable {
        if let Err(error) = temporary_workspace_result {
            eprintln!("文档已耐久保存，但同步临时工作区失败；下次加载或保存会重试：{error}");
        }
    } else {
        temporary_workspace_result.map_err(|error| {
            format!("文档已保存，但同步临时工作区失败；下次加载或保存会重试：{error}")
        })?;
    }
    if (definitions_changed || durable) && durability_error.is_none() {
        if let Some(error) = deferred_write_error {
            eprintln!("耐久保存已恢复先前的后台写入错误：{error}");
        }
        return Ok(());
    }
    match (durability_error, deferred_write_error) {
        (Some(definition_error), Some(deferred_error)) => {
            Err(format!("{definition_error}；{deferred_error}"))
        }
        (Some(error), None) | (None, Some(error)) => Err(error),
        (None, None) => Ok(()),
    }
}
#[cfg(not(test))]
#[tauri::command]
fn reset_document(app: AppHandle, state: State<'_, AppState>) -> Result<AppDocument, String> {
    // Acquire the operation fence before storage_lock. Every competing operation uses a
    // non-blocking gate, so reset can then retain exclusivity through process termination,
    // runtime cleanup, document commit and directory reconciliation without a TOCTOU window.
    let _operation = state
        .begin_mutation()
        .map_err(|_| "仍有模型、工具、终端命令或 Agent 管理操作正在进行；重置已取消")?;
    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = document_path(&app)?;
    let previous = storage::read_document(&path).ok();
    let app_data = path
        .parent()
        .ok_or_else(|| "数据文档没有应用数据父目录".to_owned())?;
    state.terminals.close_all();
    // Reset replaces every workspace record, so the worktrees these servers are
    // keyed under are about to stop existing as far as the document is concerned.
    state.preview_servers.stop_all();
    // Language servers are keyed by the roots they index, and reset is about to
    // replace those roots too. Their diagnostics ledger goes with them: it names
    // conversations that will not exist after this.
    state.lsp_servers.stop_all();
    state.mcp_sessions.clear();
    // Reset must remove only bodies known to the loaded baseline and wait for old
    // writers to drain; otherwise old writes can restore data after the seed.
    state
        .document_store
        .flush(std::time::Duration::from_secs(30))?;
    storage::purge_conversation_bodies(&path)?;
    let mut document = catalog::default_document();
    document.capabilities = capabilities::discover(&document, app_data);
    // Reset is the recovery path: commit through the store so the in-memory
    // authority is replaced too, and block until the default document is
    // durably on disk before reporting success.
    let _definition_authority = state
        .definition_authority_lock
        .write()
        .map_err(|_| "命名 Agent 定义授权锁已损坏；重置尚未提交".to_owned())?;
    state.document_store.commit(&path, document.clone())?;
    state
        .document_store
        .flush(std::time::Duration::from_secs(30))?;
    state.clear_receipts();
    app_tray::apply_language(&app, document.global_settings.resolved_app_language);
    let image_attachment_result =
        image_attachments::ImageAttachmentStore::new(app_data).purge_all();
    let temporary_workspace_result =
        workspace_dirs::reconcile_temporary_workspaces(app_data, &document);
    if let Some(previous) = previous {
        let retained = document
            .assets
            .api_providers
            .iter()
            .map(|provider| provider.id.as_str())
            .collect::<HashSet<_>>();
        for provider in previous
            .assets
            .api_providers
            .iter()
            .filter(|provider| !retained.contains(provider.id.as_str()))
        {
            if let Err(error) = api::delete_api_key(&provider.id) {
                eprintln!("重置文档后清理 API Key 失败（{}）：{error}", provider.id);
            }
            if let Err(error) = codex_oauth::host().remove(&provider.id) {
                eprintln!(
                    "重置文档后清理 Codex 登录状态失败（{}）：{error}",
                    provider.id
                );
            }
        }
        // Reset every credential slot for every catalog search provider; keys are
        // not stored in the document.
        for kind in crate::model::SearchProviderKind::CATALOG.iter().copied() {
            for slot in web_search::CredentialSlot::ALL.iter().copied() {
                // Only SearXNG has a Basic Auth slot; rejection for other
                // providers is expected.
                if slot == web_search::CredentialSlot::BasicAuthPassword
                    && kind != crate::model::SearchProviderKind::Searxng
                {
                    continue;
                }
                if let Err(error) = web_search::delete_provider_api_key(kind.slug(), slot.slug()) {
                    eprintln!(
                        "重置文档后清理搜索提供商凭据失败（{}/{}）：{error}",
                        kind.slug(),
                        slot.slug()
                    );
                }
            }
        }
    }
    temporary_workspace_result
        .map_err(|error| format!("文档已重置，但清理临时工作区失败；下次加载会重试：{error}"))?;
    image_attachment_result.map_err(|error| format!("文档已重置，但清理图片附件失败：{error}"))?;
    Ok(document)
}

#[cfg(not(test))]
#[tauri::command]
fn discover_capabilities(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<CapabilityCatalog, String> {
    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = document_path(&app)?;
    let app_data = path
        .parent()
        .ok_or_else(|| "数据文档没有应用数据父目录".to_owned())?
        .to_path_buf();
    let document = state.document_store.read(&path)?;
    Ok(capabilities::discover(&document, &app_data))
}

/// Deletes one hook from the `hooks.json` it was discovered in.
///
/// The catalog is not rewritten here. Hook ids are hashes over a hook's position
/// in its file, so what the rest of the file now means is whatever the next scan
/// says it means; the caller rescans.
#[cfg(not(test))]
#[tauri::command]
fn delete_hook(app: AppHandle, state: State<'_, AppState>, hook_id: String) -> Result<(), String> {
    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = document_path(&app)?;
    let document = state.document_store.read(&path)?;
    let (descriptor, address) = capabilities::hook_addresses(&document)
        .into_iter()
        .find(|(descriptor, _)| descriptor.id == hook_id)
        .ok_or_else(|| format!("钩子 {hook_id} 不存在"))?;
    if descriptor.source == crate::model::ResourceSource::Builtin {
        return Err("内置条目不能删除".into());
    }
    capabilities::remove_hook_from_file(&address)
}

/*
 * 工具描述文件没有任何写盘 IPC：应用只在 `discover_capabilities` 里发现它们、
 * 在对话/预设里保存被选中的资源 ID、并在构建可信请求时由
 * `capabilities::resolve_tool_descriptions` 从磁盘重读正文。文件内容由用户在
 * 应用外维护，因此这里既没有 read/save/delete 命令，也没有对应的渲染层类型。
 */

/// Result of one MCP connectivity probe.
///
/// The probe performs a real connection, handshake, and resource listing before
/// disconnecting. It must not reuse a conversation-owned session-pool connection.
#[cfg(not(test))]
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct McpProbeReport {
    ok: bool,
    /// Whether the user cancelled this probe. A cancelled probe is not a failed
    /// connection, so the catalog row must not present it as one.
    cancelled: bool,
    /// Negotiated MCP protocol version; empty on probe failure.
    protocol_version: String,
    /// Remote `serverInfo.name` and `serverInfo.version`; empty on probe failure.
    server_name: String,
    server_version: String,
    tools: Vec<McpProbeTool>,
    prompts: Vec<McpProbePrompt>,
    resources: Vec<McpProbeResource>,
    /// Child-process stderr, including failure details.
    logs: Vec<String>,
    error: String,
}

#[cfg(not(test))]
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct McpProbeTool {
    /// Remote tool name, without the model-visible prefix.
    name: String,
    title: String,
    description: String,
    /// Whether the remote server requires manual confirmation for each call.
    requires_user_interaction: bool,
    input_schema: serde_json::Value,
}

#[cfg(not(test))]
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct McpProbePrompt {
    name: String,
    title: String,
    description: String,
    arguments: Vec<McpProbePromptArgument>,
}

#[cfg(not(test))]
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct McpProbePromptArgument {
    name: String,
    description: String,
    required: bool,
}

#[cfg(not(test))]
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct McpProbeResource {
    uri: String,
    name: String,
    title: String,
    description: String,
    mime_type: String,
    size: u64,
}

/// Probes a discovered MCP server for a single dial: the server is looked up
/// in a fresh scan of `mcp.json` by id, so the renderer names a server and
/// never describes one — executable configuration only ever comes off disk.
///
/// `probe_id` names this probe so `mcp_cancel_probe` can reach it. The renderer
/// mints it; the host only requires that it not collide with a live probe.
#[cfg(not(test))]
#[tauri::command]
async fn mcp_probe_server(
    app: AppHandle,
    state: State<'_, AppState>,
    server_id: String,
    probe_id: String,
) -> Result<McpProbeReport, String> {
    let server = {
        let _guard = state
            .storage_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let path = document_path(&app)?;
        let document = state.document_store.read(&path)?;
        capabilities::mcp_server_config(&document, &server_id)?
    };
    tauri::async_runtime::spawn_blocking(move || {
        let runtime = mcp::RuntimeMcpServer::from_config(&server);
        // Use production timeout and schema limits so probe results match runs.
        let outcome = match mcp::probe_server_for_settings(&runtime, &probe_id) {
            Ok(outcome) => outcome,
            Err((error, logs)) => {
                return McpProbeReport {
                    ok: false,
                    // A cancelled probe reached no verdict about the server.
                    cancelled: error.kind == mcp::McpErrorKind::Cancelled,
                    protocol_version: String::new(),
                    server_name: String::new(),
                    server_version: String::new(),
                    tools: Vec::new(),
                    prompts: Vec::new(),
                    resources: Vec::new(),
                    logs,
                    error: error.to_string(),
                };
            }
        };
        McpProbeReport {
            ok: true,
            cancelled: false,
            protocol_version: outcome.protocol_version,
            server_name: outcome.server_name,
            server_version: outcome.server_version,
            tools: outcome
                .tools
                .into_iter()
                .map(|binding| McpProbeTool {
                    name: binding.remote_name,
                    title: binding.title,
                    description: binding.description,
                    requires_user_interaction: binding.requires_user_interaction,
                    input_schema: binding.input_schema,
                })
                .collect(),
            prompts: outcome
                .prompts
                .into_iter()
                .map(|prompt| McpProbePrompt {
                    name: prompt.name,
                    title: prompt.title,
                    description: prompt.description,
                    arguments: prompt
                        .arguments
                        .into_iter()
                        .map(|argument| McpProbePromptArgument {
                            name: argument.name,
                            description: argument.description,
                            required: argument.required,
                        })
                        .collect(),
                })
                .collect(),
            resources: outcome
                .resources
                .into_iter()
                .map(|resource| McpProbeResource {
                    uri: resource.uri,
                    name: resource.name,
                    title: resource.title,
                    description: resource.description,
                    mime_type: resource.mime_type,
                    size: resource.size,
                })
                .collect(),
            logs: outcome.diagnostics,
            error: String::new(),
        }
    })
    .await
    .map_err(|error| format!("MCP 探测后台任务失败: {error}"))
}

/// Cancels the MCP probe named by `probe_id`.
///
/// Idempotent: cancelling an unknown probe succeeds, and a cancel that arrives
/// before the probe registers is remembered so the probe starts already
/// cancelled. Refusals (a malformed id) are returned so the renderer can show
/// them instead of leaving a button that silently does nothing.
#[cfg(not(test))]
#[tauri::command]
fn mcp_cancel_probe(probe_id: String) -> Result<(), String> {
    mcp::cancel_probe(&probe_id).map_err(|error| error.to_string())
}

/// Deletes a discovered skill's folder — under `~/.mework/skills` or a
/// workspace's `.mework/skills`, whichever the scan found it in.
///
/// Skills, MCP servers and hooks are all files the user owns, so each delete is
/// a host command that rewrites or removes the file, and the caller rescans.
#[cfg(not(test))]
#[tauri::command]
fn delete_skill(
    app: AppHandle,
    state: State<'_, AppState>,
    skill_id: String,
) -> Result<(), String> {
    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = document_path(&app)?;
    let document = state.document_store.read(&path)?;
    capabilities::delete_skill(&document, &skill_id)
}

/// Removes a discovered MCP server from the `mcp.json` it was read from.
#[cfg(not(test))]
#[tauri::command]
fn delete_mcp_server(
    app: AppHandle,
    state: State<'_, AppState>,
    server_id: String,
) -> Result<(), String> {
    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = document_path(&app)?;
    let document = state.document_store.read(&path)?;
    capabilities::delete_mcp_server(&document, &server_id)
}

/// Opens the place one kind of capability is configured at — the global
/// `~/.mework` or one workspace's `.mework` — in the OS file manager, creating
/// the directory when it does not exist yet so a first-time user lands
/// somewhere rather than nowhere.
#[cfg(not(test))]
#[tauri::command]
async fn reveal_capability_location(
    app: AppHandle,
    state: State<'_, AppState>,
    kind: capabilities::CapabilityKind,
    workspace_id: Option<String>,
) -> Result<(), String> {
    let target = {
        let _guard = state
            .storage_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let path = document_path(&app)?;
        let document = state.document_store.read(&path)?;
        capabilities::capability_location_to_reveal(&document, kind, workspace_id.as_deref())?
    };
    tauri::async_runtime::spawn_blocking(move || {
        reveal_path::reveal(&reveal_path::resolve_reveal_path(
            &target.to_string_lossy(),
            None,
        )?)
    })
    .await
    .map_err(|error| format!("打开配置位置的后台任务失败: {error}"))?
}

/// Enumerates installed WSL distributions for the run-location selector.
/// Results are never persisted because the list is machine state; missing WSL,
/// enumeration failure, and timeouts all return an empty list.
#[cfg(not(test))]
#[tauri::command]
async fn list_wsl_distros() -> Result<Vec<run_environment::WslDistro>, String> {
    tauri::async_runtime::spawn_blocking(run_environment::list_wsl_distros)
        .await
        .map_err(|error| format!("枚举 WSL 发行版的后台任务失败: {error}"))
}

/// Probes environment dependencies from built-in presets and persisted custom
/// entries. Executable names must not come from parameters because they select
/// processes to start.
#[cfg(not(test))]
#[tauri::command]
async fn environment_tool_snapshots(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<environment_tools::EnvironmentToolSnapshot>, String> {
    let tools = environment_tool_definitions(&app, &state)?;
    tauri::async_runtime::spawn_blocking(move || environment_tools::probe(&tools))
        .await
        .map_err(|error| format!("探测环境依赖的后台任务失败: {error}"))
}

/// Opens the directory of an environment dependency in the system file manager.
/// The parameter must be a known executable name; the host resolves its path.
#[cfg(not(test))]
#[tauri::command]
async fn reveal_environment_tool(
    app: AppHandle,
    state: State<'_, AppState>,
    executable: String,
) -> Result<(), String> {
    let tools = environment_tool_definitions(&app, &state)?;
    if !environment_tools::is_known_executable(&executable, &tools) {
        return Err(format!("{executable} 不是一个已登记的环境依赖"));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let resolved = environment_tools::resolve_on_path(&executable)
            .ok_or_else(|| format!("PATH 上没有找到 {executable}"))?;
        let directory = resolved
            .parent()
            .ok_or_else(|| "可执行文件没有所在目录".to_owned())?;
        reveal_directory(directory)
    })
    .await
    .map_err(|error| format!("打开目录的后台任务失败: {error}"))?
}

/// Opens an external URL in the operating system's default browser.
///
/// All external UI and Markdown links use this command because the application
/// WebView does not navigate across sites. Host-side validation protects this
/// boundary because model and remote-catalog data can reach the parameter.
#[cfg(not(test))]
#[tauri::command]
async fn open_external_url(url: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || external_open::open_in_default_browser(&url))
        .await
        .map_err(|error| format!("打开外部链接的后台任务失败: {error}"))?
}

/// Shows a path in the operating system's file manager.
///
/// Paths detected in model replies reach this command, so `reveal_path`
/// validates the parameter and proves the target exists. A file is selected in
/// its containing folder rather than opened, which keeps the command from
/// starting a program.
#[cfg(not(test))]
#[tauri::command]
async fn reveal_path_in_file_manager(path: String, base_dir: Option<String>) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        reveal_path::reveal(&reveal_path::resolve_reveal_path(
            &path,
            base_dir.as_deref(),
        )?)
    })
    .await
    .map_err(|error| format!("打开文件位置的后台任务失败: {error}"))?
}

/// Directory holding the running executable: where the NSIS uninstaller would sit and where a
/// portable copy lives.
#[cfg(not(test))]
fn executable_dir() -> Result<PathBuf, String> {
    let exe =
        std::env::current_exe().map_err(|error| format!("无法定位当前可执行文件: {error}"))?;
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "当前可执行文件没有所在目录".to_owned())
}

/// The running version and how it was installed. No network; the Updates page shows this before
/// any check happens.
#[cfg(not(test))]
#[tauri::command]
fn app_version_info(app: AppHandle) -> Result<app_update::AppVersionInfo, String> {
    let version = app.package_info().version.to_string();
    Ok(app_update::version_info(&version, &executable_dir()?))
}

/// Asks GitHub for the latest release and compares it with the running version.
#[cfg(not(test))]
#[tauri::command]
async fn check_app_update(app: AppHandle) -> Result<app_update::UpdateCheck, String> {
    let version = app.package_info().version.to_string();
    let flavor = app_update::detect_flavor(&executable_dir()?);
    tauri::async_runtime::spawn_blocking(move || app_update::check_for_update(&version, flavor))
        .await
        .map_err(|error| format!("检查更新的后台任务失败: {error}"))?
}

/// Downloads the release asset the check selected, streaming progress on `on_progress`.
///
/// The renderer passes the asset back rather than an id because the check result is not kept
/// host-side; `app_update::validate_asset` re-checks the name, size, and host before any byte
/// is written, so the renderer cannot turn this into a download from elsewhere. Installer
/// downloads land in the app's local-data `updates` directory; a portable archive goes to the
/// user's Downloads folder because they will unpack it by hand.
#[cfg(not(test))]
#[tauri::command]
async fn download_app_update(
    app: AppHandle,
    state: State<'_, AppState>,
    asset: app_update::ReleaseAsset,
    checksums_asset: Option<app_update::ReleaseAsset>,
    on_progress: Channel<app_update::DownloadEvent>,
) -> Result<app_update::DownloadedUpdate, String> {
    let flavor = app_update::detect_flavor(&executable_dir()?);
    let updates_dir = app
        .path()
        .app_local_data_dir()
        .map_err(|error| format!("无法解析应用本地数据目录: {error}"))?
        .join("updates");
    let destination_dir = match flavor {
        app_update::InstallFlavor::Installer => updates_dir,
        app_update::InstallFlavor::Portable => dirs::download_dir().unwrap_or(updates_dir),
    };
    let request = app_update::DownloadRequest {
        asset,
        checksums_asset,
        flavor,
        destination_dir,
        current_version: app.package_info().version.to_string(),
    };
    let session = state.app_update.clone();
    let cancel = session.begin_download()?;
    let result = tauri::async_runtime::spawn_blocking(move || {
        app_update::download_update(request, &cancel, |event| {
            let _ = on_progress.send(event);
        })
    })
    .await
    .map_err(|error| format!("下载更新的后台任务失败: {error}"))
    .and_then(|result| result);
    session.finish_download(result.as_ref().ok().cloned());
    result
}

#[cfg(not(test))]
#[tauri::command]
fn cancel_app_update_download(state: State<'_, AppState>) -> Result<(), String> {
    state.app_update.cancel_download();
    Ok(())
}

/// Hands the downloaded file over: the installer flavor starts it and exits the app so the
/// installer can replace the binaries; the portable flavor shows the archive in the file
/// manager. Only the file this process downloaded is accepted.
#[cfg(not(test))]
#[tauri::command]
async fn install_app_update(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> Result<app_update::InstallOutcome, String> {
    // Authorization re-hashes the file, so it runs off the async runtime.
    let session = state.app_update.clone();
    let authorized = tauri::async_runtime::spawn_blocking(move || session.authorize_install(&path))
        .await
        .map_err(|error| format!("核对更新文件的后台任务失败: {error}"))??;
    let flavor = authorized.downloaded.flavor;
    match flavor {
        app_update::InstallFlavor::Installer => {
            // The authorization keeps the verified file open; hand it over instead of rebuilding
            // a launch target from the bare path, which is what the swap window relied on.
            tauri::async_runtime::spawn_blocking(move || app_update::launch_installer(authorized))
                .await
                .map_err(|error| format!("启动安装程序的后台任务失败: {error}"))??;
            // `RunEvent::Exit` runs `finalize_app_shutdown`: documents flush, the sidecar stops.
            app.exit(0);
            Ok(app_update::InstallOutcome {
                action: app_update::InstallAction::InstallerLaunched,
            })
        }
        app_update::InstallFlavor::Portable => {
            let target = PathBuf::from(&authorized.downloaded.path);
            drop(authorized);
            tauri::async_runtime::spawn_blocking(move || app_update::reveal_file(&target))
                .await
                .map_err(|error| format!("打开下载目录的后台任务失败: {error}"))??;
            Ok(app_update::InstallOutcome {
                action: app_update::InstallAction::Revealed,
            })
        }
    }
}

/// Opens an external link received from a WebView callback.
///
/// WebView callbacks run on the UI thread, while `ShellExecuteW` may block. Spawn
/// the operation and ignore its result; this fallback has no caller to receive it.
#[cfg(not(test))]
fn open_external_url_in_background(url: String) {
    if let Err(error) = std::thread::Builder::new()
        .name("mework-external-link".to_owned())
        .spawn(move || {
            if let Err(reason) = external_open::open_in_default_browser(&url) {
                eprintln!("打开外部链接失败：{reason}");
            }
        })
    {
        eprintln!("无法派发打开外部链接的线程：{error}");
    }
}

/// What the main window does with a navigation it is about to commit.
#[derive(Debug, PartialEq, Eq)]
enum MainWindowNavigation {
    /// Commit the navigation in the window.
    LoadInWindow,
    /// Refuse the navigation and hand the address to the system browser.
    OpenExternally(String),
}

/// Decides whether a main-window navigation belongs to the application or to the
/// user's default browser.
///
/// The window's own frontend is never an external link, and it has to be
/// recognized *before* the external test runs. In a development build the
/// frontend is served from loopback HTTP at `frontend_origin`, which is an
/// address the system browser accepts perfectly well — classifying it as
/// external hands the first navigation of every development run to the system
/// browser and leaves the window empty. A release build serves the frontend from
/// the custom protocol, has no development origin, and so treats loopback HTTP
/// as what it then is: somebody else's server.
///
/// The comparison is exact, not "any loopback address on that port". This window
/// is the trusted surface; a different local server that merely shares the port
/// is not this frontend, and loading it here would replace the application's own
/// UI rather than open it where a foreign page belongs.
fn classify_main_window_navigation(
    url: &url::Url,
    frontend_origin: Option<&str>,
) -> MainWindowNavigation {
    if frontend_origin.is_some_and(|origin| url.origin().ascii_serialization() == origin) {
        return MainWindowNavigation::LoadInWindow;
    }
    // Intercept only external HTTP(S) URLs the system accepts. Internal origins
    // and schemes must pass through, including initial-page navigation.
    match external_open::parse_external_url(url.as_str()) {
        Ok(external) => MainWindowNavigation::OpenExternally(external.into()),
        Err(_) => MainWindowNavigation::LoadInWindow,
    }
}

#[cfg(test)]
mod main_window_navigation_tests {
    use super::{classify_main_window_navigation, MainWindowNavigation};
    use url::Url;

    const DEV_ORIGIN: Option<&str> = Some("http://127.0.0.1:1420");

    fn classify(raw: &str, frontend_origin: Option<&str>) -> MainWindowNavigation {
        classify_main_window_navigation(&Url::parse(raw).unwrap(), frontend_origin)
    }

    /// The regression this guard exists for: the development server's own origin
    /// is an ordinary loopback HTTP address, so an external-link test that does
    /// not know about it refuses the window's first navigation and opens the
    /// application's own page in the system browser.
    #[test]
    fn the_development_frontend_loads_in_the_window_rather_than_the_system_browser() {
        for raw in [
            "http://127.0.0.1:1420/",
            "http://127.0.0.1:1420/index.html?x=1",
            "http://127.0.0.1:1420/nested/page#fragment",
        ] {
            assert_eq!(
                classify(raw, DEV_ORIGIN),
                MainWindowNavigation::LoadInWindow,
                "{raw} is this window's own frontend"
            );
        }
    }

    /// The origin is whatever the development server actually took, not a fixed
    /// address, and only that exact origin counts.
    #[test]
    fn only_the_exact_frontend_origin_loads_in_the_window() {
        assert_eq!(
            classify("http://127.0.0.1:53117/", Some("http://127.0.0.1:53117")),
            MainWindowNavigation::LoadInWindow
        );
        for other in [
            // A port this run never served.
            "http://127.0.0.1:1421/",
            // Another loopback address that merely shares the port. Loading it
            // here would replace the trusted UI with a different server's page.
            "http://127.0.0.2:1420/",
            "http://localhost:1420/",
            // Same authority, different scheme.
            "https://127.0.0.1:1420/",
        ] {
            assert_eq!(
                classify(other, DEV_ORIGIN),
                MainWindowNavigation::OpenExternally(other.into()),
                "{other} does not name this window's own frontend"
            );
        }
    }

    /// Without a development server there is no loopback frontend to protect, so
    /// a local address is the user's own server and belongs in their browser.
    #[test]
    fn a_release_build_sends_loopback_addresses_to_the_system_browser() {
        assert_eq!(
            classify("http://127.0.0.1:1420/", None),
            MainWindowNavigation::OpenExternally("http://127.0.0.1:1420/".into())
        );
    }

    #[test]
    fn remote_links_go_to_the_system_browser() {
        assert_eq!(
            classify("https://example.com/docs", DEV_ORIGIN),
            MainWindowNavigation::OpenExternally("https://example.com/docs".into())
        );
    }

    /// The production frontend and every other internal surface must commit in
    /// the window. These are the addresses a release build actually navigates to.
    #[test]
    fn internal_origins_and_schemes_load_in_the_window() {
        for raw in [
            "tauri://localhost/",
            "http://tauri.localhost/index.html",
            "http://asset.localhost/icon.png",
            "http://ipc.localhost/",
            "about:blank",
        ] {
            for frontend_origin in [DEV_ORIGIN, None] {
                assert_eq!(
                    classify(raw, frontend_origin),
                    MainWindowNavigation::LoadInWindow,
                    "{raw} is internal"
                );
            }
        }
    }
}

#[cfg(not(test))]
fn environment_tool_definitions(
    app: &AppHandle,
    state: &State<'_, AppState>,
) -> Result<Vec<model::EnvironmentToolDefinition>, String> {
    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = document_path(app)?;
    let document = state.document_store.read(&path)?;
    Ok(document.global_settings.environment_tools.clone())
}

#[cfg(all(not(test), windows))]
fn reveal_directory(directory: &std::path::Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // explorer.exe returns nonzero when the directory is already open; only
    // process-start success matters.
    std::process::Command::new("explorer.exe")
        .arg(directory)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("无法打开目录: {error}"))
}

#[cfg(all(not(test), not(windows)))]
fn reveal_directory(directory: &std::path::Path) -> Result<(), String> {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(opener)
        .arg(directory)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("无法打开目录: {error}"))
}

#[cfg(not(test))]
#[tauri::command]
async fn execute_tool(
    app: AppHandle,
    state: State<'_, AppState>,
    request: ToolExecutionRequest,
    approval_nonce: Option<String>,
) -> Result<ToolExecutionResponse, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        execute_tool_blocking(app, state, request, approval_nonce)
    })
    .await
    .map_err(|error| format!("工具执行后台任务失败: {error}"))?
}

#[cfg(not(test))]
fn execute_tool_blocking(
    app: AppHandle,
    state: AppState,
    mut request: ToolExecutionRequest,
    approval_nonce: Option<String>,
) -> Result<ToolExecutionResponse, String> {
    let _operation = state.begin_operation()?;
    let policy = trusted_conversation_policy(&app, &state, &request.conversation_id, "工具请求")?;
    request.workspace_path = policy.workspace_path.clone();
    if !policy.enabled_tools.contains(&request.tool_name) {
        return Err(format!("当前对话未启用工具 {}", request.tool_name));
    }
    let descriptor = policy
        .tools
        .iter()
        .find(|tool| tool.name == request.tool_name)
        .ok_or_else(|| format!("未知工具: {}", request.tool_name))?;
    if descriptor.category == ToolCategory::Memory {
        return Err(format!(
            "长期记忆工具 {} 只能在模型运行循环中由宿主注入当前模型身份，不能手工执行",
            request.tool_name
        ));
    }
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法解析应用数据目录: {error}"))?;
    let decision = security::classify_in_workspaces(
        policy.security_level,
        &policy.workspaces,
        Path::new(&request.workspace_path),
        app_data.as_path(),
        &policy.additional_directories,
        &request,
    )?;
    if decision.requires_approval {
        let nonce = approval_nonce
            .as_deref()
            .ok_or_else(|| "这个工具操作必须先取得单次批准".to_owned())?;
        state.consume_tool_approval(nonce, &request, &policy.run_environment.fingerprint())?;
    }
    Ok(tool_executor::execute_with_scope_and_attachments(
        request,
        &state,
        decision.scope,
        Some(&app_data),
        &policy.workspaces,
        &policy.prompt_profile,
    ))
}

#[cfg(not(test))]
#[tauri::command]
async fn request_tool_approval(
    app: AppHandle,
    state: State<'_, AppState>,
    mut request: ToolExecutionRequest,
) -> Result<approval::ToolApprovalGrant, String> {
    let policy =
        trusted_conversation_policy(&app, state.inner(), &request.conversation_id, "工具请求")?;
    request.workspace_path = policy.workspace_path.clone();
    if !policy.enabled_tools.contains(&request.tool_name) {
        return Err(format!("当前对话未启用工具 {}", request.tool_name));
    }
    let descriptor = policy
        .tools
        .iter()
        .find(|tool| tool.name == request.tool_name)
        .ok_or_else(|| format!("未知工具: {}", request.tool_name))?;
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法解析应用数据目录: {error}"))?;
    let decision = security::classify_in_workspaces(
        policy.security_level,
        &policy.workspaces,
        Path::new(&request.workspace_path),
        app_data.as_path(),
        &policy.additional_directories,
        &request,
    )?;
    if !decision.requires_approval {
        return Ok(approval::ToolApprovalGrant::not_required());
    }
    if !decision.mandatory_prompt
        && state.tool_prompts().is_always_allowed(
            &request.conversation_id,
            &request.tool_name,
            decision.risk_level,
        )
    {
        return state.issue_tool_approval(&request, &policy.run_environment.fingerprint());
    }

    // No card is drawn here: this call has no run to stream through, so the
    // renderer is told which prompt to draw and answers with
    // `resolve_tool_prompt`. The classified request stays host-side until
    // then, so the arguments that get a nonce are the ones that were judged.
    let prompt_id = state
        .tool_prompts()
        .open_manual(&request, decision.risk_level)?;
    Ok(approval::ToolApprovalGrant::pending(
        tool_prompt::PendingToolPrompt {
            prompt_id,
            tool_name: request.tool_name.clone(),
            kind: tool_prompt::PromptKind::Tool,
            label: descriptor.label.clone(),
            summary: tool_prompt::summarize_tool_input(
                &request.tool_name,
                &api::public_tool_input(&request.tool_name, &request.input),
            ),
            risk_level: decision.risk_level.label_zh().to_owned(),
            reason: decision.reason.clone(),
            requester: None,
            // Renderer-initiated calls have no subagent origin.
            source_agent: None,
            source_call_id: None,
            allow_always_offered: !decision.mandatory_prompt
                && !tool_prompt::never_blanket_allowed(&request.tool_name),
            mandatory: decision.mandatory_prompt,
        },
    ))
}

/// Answers one approval card. Both prompt shapes land here: a model prompt
/// wakes its blocked worker, and a manual prompt mints the nonce for the
/// request that was classified when the card was raised.
///
/// `feedback` is the prose the plan cards collect. It is trimmed and capped
/// here because it is renderer-supplied text that ends up in a tool result the
/// model reads.
#[cfg(not(test))]
#[tauri::command]
fn resolve_tool_prompt(
    app: AppHandle,
    state: State<'_, AppState>,
    prompt_id: String,
    decision: tool_prompt::ToolPromptDecision,
    feedback: Option<String>,
) -> Result<approval::ToolApprovalGrant, String> {
    let feedback = tool_prompt::sanitize_prompt_feedback(feedback);
    match state
        .tool_prompts()
        .resolve(&prompt_id, decision, feedback)?
    {
        tool_prompt::PromptResolution::Model => Ok(approval::ToolApprovalGrant::not_required()),
        tool_prompt::PromptResolution::Manual { request, decision } => {
            if !decision.allows() {
                return Err("用户拒绝了这次工具执行".into());
            }
            // Fingerprint the trusted environment at approval time. Nonce
            // consumption re-resolves it, invalidating approval after a target or
            // environment-variable change.
            let policy = trusted_conversation_policy(
                &app,
                state.inner(),
                &request.conversation_id,
                "工具请求",
            )?;
            state.issue_tool_approval(&request, &policy.run_environment.fingerprint())
        }
    }
}

#[cfg(not(test))]
#[tauri::command]
async fn pick_workspace_directory(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<String>, String> {
    let dialog_app = app.clone();
    let selected = tauri::async_runtime::spawn_blocking(move || {
        dialog_app
            .dialog()
            .file()
            .set_title("选择工作区文件夹")
            .blocking_pick_folder()
    })
    .await
    .map_err(|error| format!("打开系统目录选择器失败: {error}"))?;
    let Some(selected) = selected else {
        return Ok(None);
    };
    let path = selected
        .into_path()
        .map_err(|error| format!("系统目录选择结果无效: {error}"))?;
    state.authorize_workspace(&path).map(Some)
}

/// Reads one level of a machine that is not this one, for the remote workspace
/// browser.
///
/// Read-only and unprivileged by construction: the command is fixed, the only
/// thing the renderer supplies is the directory being looked at, and looking
/// grants nothing — [`authorize_remote_workspace`] is what records a grant.
#[cfg(not(test))]
#[tauri::command]
async fn list_remote_directory(
    app: AppHandle,
    state: State<'_, AppState>,
    machine: model::RunTarget,
    path: String,
) -> Result<remote_directory::RemoteDirectoryListing, String> {
    let assets = execution_environment_assets(&app, state.inner())?;
    tauri::async_runtime::spawn_blocking(move || {
        remote_directory::list_directory(&assets, &machine, &path)
    })
    .await
    .map_err(|error| format!("读取远端目录失败: {error}"))?
}

/// Confirms a directory on another machine and records it as granted this
/// session, returning the path that machine's shell resolved.
///
/// The remote half of `pick_workspace_directory`: the renderer may name a
/// workspace in a saved document only when a picker of the host's own returned
/// it, and this is the picker for a machine with no native dialog.
#[cfg(not(test))]
#[tauri::command]
async fn authorize_remote_workspace(
    app: AppHandle,
    state: State<'_, AppState>,
    machine: model::RunTarget,
    path: String,
) -> Result<String, String> {
    let assets = execution_environment_assets(&app, state.inner())?;
    let machine_key = run_environment::env_key(Some(&machine));
    let resolved = {
        let machine = machine.clone();
        tauri::async_runtime::spawn_blocking(move || {
            remote_directory::resolve_directory(&assets, &machine, &path)
        })
        .await
        .map_err(|error| format!("解析远端目录失败: {error}"))??
    };
    state.authorize_remote_workspace(&machine_key, &resolved);
    Ok(resolved)
}

/// The machine catalog, read from the persisted document under its lock.
///
/// Read here rather than taken from the renderer for the same reason the run
/// environment is: an endpoint the renderer could assert is an endpoint it could
/// invent.
#[cfg(not(test))]
fn execution_environment_assets(
    app: &AppHandle,
    state: &AppState,
) -> Result<model::ExecutionEnvironmentAssets, String> {
    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = document_path(app)?;
    Ok(state
        .document_store
        .read(&path)?
        .assets
        .execution_environments
        .clone())
}

/// The card's verbatim display fields (requester, label) go through the same
/// bidi/zero-width neutralization as its summary. There is one
/// implementation — `tool_prompt::escape_display_text` — and this module keeps
/// the guarantee under test from the approval side.
#[cfg(test)]
mod approval_prompt_tests {
    use crate::tool_prompt::escape_display_text as escape_approval_display_text;

    /// A right-to-left override or an isolate could otherwise make a displayed
    /// requester or label read as the reverse of what it is.
    #[test]
    fn display_text_escaping_neutralizes_invisible_direction_controls() {
        let dangerous = "safe\u{202e}spoof\u{2066}tail";
        let escaped = escape_approval_display_text(dangerous);

        assert!(!escaped.contains('\u{202e}'));
        assert!(!escaped.contains('\u{2066}'));
        assert!(escaped.contains("\\u{202E}"));
        assert!(escaped.contains("\\u{2066}"));
        // Only the invisible controls change; the visible text is untouched.
        assert!(escaped.starts_with("safe"));
        assert!(escaped.ends_with("tail"));
    }

    /// `list_pending_tool_prompts` entries must be flat, matching push-channel
    /// and run-stream cards. The renderer has one card renderer for all three
    /// projections.
    #[test]
    fn a_pending_prompt_entry_is_flat_on_the_wire() {
        let entry = crate::PendingToolPromptEntry {
            conversation_id: "conv-1".into(),
            prompt: crate::tool_prompt::PendingToolPrompt {
                prompt_id: "prompt-1".into(),
                tool_name: "write".into(),
                kind: crate::tool_prompt::PromptKind::Tool,
                label: "写入文件".into(),
                summary: "notes.md".into(),
                risk_level: "中".into(),
                reason: "请求批准模式要求确认所有写入操作".into(),
                requester: Some("ws1".into()),
                source_agent: Some("ws1".into()),
                source_call_id: Some("call-7".into()),
                allow_always_offered: true,
                mandatory: false,
            },
        };
        let wire = serde_json::to_value(&entry).unwrap();
        assert_eq!(wire["conversationId"], "conv-1");
        assert_eq!(wire["promptId"], "prompt-1");
        assert_eq!(wire["toolName"], "write");
        assert_eq!(wire["sourceAgent"], "ws1");
        assert_eq!(wire["sourceCallId"], "call-7");
        assert_eq!(wire["allowAlwaysOffered"], true);
        assert!(wire.get("prompt").is_none(), "卡片不得再套一层：{wire}");
    }
}

#[cfg(not(test))]
#[tauri::command]
async fn save_search_api_key(
    state: State<'_, AppState>,
    provider_kind: String,
    slot: String,
    api_key: String,
) -> Result<ApiKeyStatus, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = state
            .storage_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let api_key = zeroize::Zeroizing::new(api_key);
        web_search::save_provider_api_key(&provider_kind, &slot, &api_key)
    })
    .await
    .map_err(|error| format!("保存搜索提供商 API Key 的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn get_search_key_status(
    state: State<'_, AppState>,
    provider_kind: String,
    slot: String,
) -> Result<ApiKeyStatus, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = state
            .storage_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        web_search::get_provider_key_status(&provider_kind, &slot)
    })
    .await
    .map_err(|error| format!("读取搜索提供商 API Key 状态的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn reveal_search_api_key(
    state: State<'_, AppState>,
    provider_kind: String,
    slot: String,
) -> Result<String, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = state
            .storage_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        web_search::reveal_provider_api_key(&provider_kind, &slot)
    })
    .await
    .map_err(|error| format!("读取搜索提供商 API Key 的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn delete_search_api_key(
    state: State<'_, AppState>,
    provider_kind: String,
    slot: String,
) -> Result<ApiKeyStatus, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = state
            .storage_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        web_search::delete_provider_api_key(&provider_kind, &slot)
    })
    .await
    .map_err(|error| format!("删除搜索提供商 API Key 的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn save_decision_api_key(
    state: State<'_, AppState>,
    provider_kind: String,
    api_key: String,
) -> Result<ApiKeyStatus, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = state
            .storage_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let api_key = zeroize::Zeroizing::new(api_key);
        decision_model::save_provider_api_key(&provider_kind, &api_key)
    })
    .await
    .map_err(|error| format!("保存决策模型提供商 API Key 的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn get_decision_key_status(
    state: State<'_, AppState>,
    provider_kind: String,
) -> Result<ApiKeyStatus, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = state
            .storage_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        decision_model::get_provider_key_status(&provider_kind)
    })
    .await
    .map_err(|error| format!("读取决策模型提供商 API Key 状态的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn reveal_decision_api_key(
    state: State<'_, AppState>,
    provider_kind: String,
) -> Result<String, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = state
            .storage_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        decision_model::reveal_provider_api_key(&provider_kind)
    })
    .await
    .map_err(|error| format!("读取决策模型提供商 API Key 的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn delete_decision_api_key(
    state: State<'_, AppState>,
    provider_kind: String,
) -> Result<ApiKeyStatus, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = state
            .storage_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        decision_model::delete_provider_api_key(&provider_kind)
    })
    .await
    .map_err(|error| format!("删除决策模型提供商 API Key 的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn save_api_key(
    app: AppHandle,
    state: State<'_, AppState>,
    provider: ApiProvider,
    api_key: String,
) -> Result<ApiKeyStatus, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = state
            .storage_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let document = state.document_store.read(&document_path(&app)?)?;
        // Providers must already be persisted because their ID hashes the
        // credential binding; otherwise the encrypted entry would be orphaned.
        let stored = document
            .assets
            .api_providers
            .iter()
            .find(|stored| stored.id == provider.id)
            .ok_or_else(|| format!("API 提供商 {} 尚未保存", provider.id))?;
        api::refuse_api_key_command(stored.family, api::ApiKeyCommand::Save)?;
        api::save_api_key(&stored.id, &api_key)
    })
    .await
    .map_err(|error| format!("保存 API Key 的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn reveal_api_key(
    app: AppHandle,
    state: State<'_, AppState>,
    provider: ApiProvider,
) -> Result<String, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = state
            .storage_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let document = state.document_store.read(&document_path(&app)?)?;
        let stored = document
            .assets
            .api_providers
            .iter()
            .find(|stored| stored.id == provider.id)
            .ok_or_else(|| format!("API 提供商 {} 尚未保存", provider.id))?;
        api::refuse_api_key_command(stored.family, api::ApiKeyCommand::Reveal)?;
        api::reveal_api_key(&stored.id)
    })
    .await
    .map_err(|error| format!("读取 API Key 的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn delete_api_key(
    app: AppHandle,
    state: State<'_, AppState>,
    provider: ApiProvider,
) -> Result<ApiKeyStatus, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = state
            .storage_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let document = state.document_store.read(&document_path(&app)?)?;
        let stored = document
            .assets
            .api_providers
            .iter()
            .find(|stored| stored.id == provider.id)
            .ok_or_else(|| format!("API 提供商 {} 尚未保存", provider.id))?;
        api::refuse_api_key_command(stored.family, api::ApiKeyCommand::Delete)?;
        api::delete_api_key(&stored.id)
    })
    .await
    .map_err(|error| format!("删除 API Key 的后台任务失败: {error}"))?
}

/// The persisted Codex row for an OAuth command. Destination and credential
/// identity come from the saved document, never from renderer values, and a
/// non-Codex row is refused before any token or listener is touched.
#[cfg(not(test))]
fn trusted_codex_provider(
    app: &AppHandle,
    state: &State<'_, AppState>,
    requested: &ApiProvider,
) -> Result<ApiProvider, String> {
    let provider = trusted_provider(app, state, requested)?;
    if provider.family != crate::model::ProviderFamily::OpenaiCodex {
        return Err(format!(
            "提供商 {} 不是 OpenAI Codex，不能用 ChatGPT 登录",
            provider.name
        ));
    }
    Ok(provider)
}

/// Run the browser sign-in for the built-in Codex row. Resolves when the
/// loopback callback completed, the user cancelled, or the ten-minute wait
/// expired; the renderer meanwhile keeps its own "signing in" state.
#[cfg(not(test))]
#[tauri::command]
async fn codex_oauth_sign_in(
    app: AppHandle,
    state: State<'_, AppState>,
    provider: ApiProvider,
) -> Result<codex_oauth::CodexOauthStatus, String> {
    let provider = trusted_codex_provider(&app, &state, &provider)?;
    tauri::async_runtime::spawn_blocking(move || {
        codex_oauth::host().sign_in(&provider.id, &|url: &str| {
            external_open::open_in_default_browser(url)
        })
    })
    .await
    .map_err(|error| format!("ChatGPT 登录的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn codex_oauth_cancel_sign_in(
    app: AppHandle,
    state: State<'_, AppState>,
    provider: ApiProvider,
) -> Result<(), String> {
    let provider = trusted_codex_provider(&app, &state, &provider)?;
    codex_oauth::host().cancel_sign_in(&provider.id)
}

#[cfg(not(test))]
#[tauri::command]
async fn codex_oauth_status(
    app: AppHandle,
    state: State<'_, AppState>,
    provider: ApiProvider,
) -> Result<codex_oauth::CodexOauthStatus, String> {
    let provider = trusted_codex_provider(&app, &state, &provider)?;
    tauri::async_runtime::spawn_blocking(move || codex_oauth::host().status(&provider.id))
        .await
        .map_err(|error| format!("读取 ChatGPT 登录状态的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn codex_oauth_sign_out(
    app: AppHandle,
    state: State<'_, AppState>,
    provider: ApiProvider,
) -> Result<codex_oauth::CodexOauthStatus, String> {
    let provider = trusted_codex_provider(&app, &state, &provider)?;
    tauri::async_runtime::spawn_blocking(move || codex_oauth::host().sign_out(&provider.id))
        .await
        .map_err(|error| format!("退出 ChatGPT 登录的后台任务失败: {error}"))?
}

/// The persisted Claude Agent row for a login command. Same rule as the Codex
/// pair: the family comes from the saved document, so a renderer row cannot make
/// an arbitrary provider claim a Claude Code login. The executable is no longer
/// part of that question — the host resolves the one it ships — but the check
/// still keeps the two commands answering only for a genuine Claude Agent row.
#[cfg(not(test))]
fn trusted_claude_agent_provider(
    app: &AppHandle,
    state: &State<'_, AppState>,
    requested: &ApiProvider,
) -> Result<ApiProvider, String> {
    let provider = trusted_provider(app, state, requested)?;
    if provider.family != crate::model::ProviderFamily::ClaudeAgent {
        return Err(format!(
            "提供商 {} 不是 Claude Agent，没有本机 Claude Code 登录",
            provider.name
        ));
    }
    Ok(provider)
}

#[cfg(not(test))]
#[tauri::command]
async fn claude_agent_login_status(
    app: AppHandle,
    state: State<'_, AppState>,
    provider: ApiProvider,
) -> Result<aisdk::agent::ClaudeAgentLoginStatus, String> {
    let provider = trusted_claude_agent_provider(&app, &state, &provider)?;
    // The executable is Mework's own, so the row contributes nothing but the
    // family check above; the login it reports is still the user's.
    drop(provider);
    tauri::async_runtime::spawn_blocking(aisdk::agent::login_status)
        .await
        .map_err(|error| format!("读取 Claude Code 登录状态的后台任务失败: {error}"))?
}

/// Open a terminal running `claude auth login`. Resolves as soon as the terminal
/// starts: the exchange happens in the CLI, and the renderer learns the outcome
/// by re-reading the status.
#[cfg(not(test))]
#[tauri::command]
async fn claude_agent_open_login(
    app: AppHandle,
    state: State<'_, AppState>,
    provider: ApiProvider,
) -> Result<(), String> {
    let provider = trusted_claude_agent_provider(&app, &state, &provider)?;
    drop(provider);
    tauri::async_runtime::spawn_blocking(aisdk::agent::open_login)
        .await
        .map_err(|error| format!("打开 Claude Code 登录的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn fetch_models(
    app: AppHandle,
    state: State<'_, AppState>,
    provider: ApiProvider,
) -> Result<Vec<ModelProfile>, String> {
    let provider = trusted_provider(&app, &state, &provider)?;
    tauri::async_runtime::spawn_blocking(move || api::fetch_models(&provider))
        .await
        .map_err(|error| format!("获取模型列表的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn run_model(
    app: AppHandle,
    state: State<'_, AppState>,
    request: RunModelRequest,
    request_id: String,
    on_event: Channel<ModelStreamEvent>,
    fork_prompt_context_id: Option<String>,
) -> Result<RunModelResponse, String> {
    if request_id.is_empty() || request_id.len() > 128 || request_id.chars().any(char::is_control) {
        return Err("模型运行 ID 无效".into());
    }
    let fork_store = conversations::store(&document_path(&app)?)?;
    let operation = state.begin_operation()?;
    let (cancellation, steer_inbox) =
        state.begin_model_run(&request_id, &request.conversation_id)?;
    // Recheck task wake-ups on every exit path. A task that settles during
    // registration must be delivered either by the active round or this recheck.
    let wake_conversation_id = request.conversation_id.clone();
    let mut request = match trusted_run_request(&app, &state, &request_id, request) {
        Ok(request) => request,
        Err(error) => {
            state.finish_model_run(&request_id, &cancellation);
            state.recheck_task_wake(&wake_conversation_id);
            return Err(error);
        }
    };
    request = match attach_trusted_project_memory(&app, state.inner().clone(), request).await {
        Ok(request) => request,
        Err(error) => {
            state.finish_model_run(&request_id, &cancellation);
            state.recheck_task_wake(&wake_conversation_id);
            return Err(error);
        }
    };
    request.steer_mailbox = agents::AgentMailboxHandle(Some(steer_inbox));
    // Carry this run's cancellation flag after trusted reconstruction because
    // serde-skipped fields are rebuilt by value. Settlement and sync legs must
    // observe this flag, not a conversation-level current-run lookup.
    request.run_cancel = crate::cancel::CancelSignal::from_flag(cancellation.clone());
    if let Err(error) = confirm_run_hooks(&app, &request).await {
        let cleanup = state.finish_model_run_checked(&request_id, &cancellation);
        state.recheck_task_wake(&wake_conversation_id);
        return Err(match cleanup {
            Ok(()) => error,
            Err(cleanup_error) => {
                format!("{error}；此外无法确认模型运行回合清理：{cleanup_error}")
            }
        });
    }
    // Native dialogs do not observe cancellation. Exit immediately after they
    // close if the user stopped the run while one was open.
    if cancellation.load(std::sync::atomic::Ordering::Acquire) {
        let cleanup = state.finish_model_run_checked(&request_id, &cancellation);
        state.recheck_task_wake(&wake_conversation_id);
        let error = "模型运行已停止".to_owned();
        return Err(match cleanup {
            Ok(()) => error,
            Err(cleanup_error) => {
                format!("{error}；此外无法确认模型运行回合清理：{cleanup_error}")
            }
        });
    }
    // The host owns runs from here: buffer events in the hub and deliver them to
    // the current subscriber when possible. A reloaded renderer reattaches and
    // replays the buffer.
    let attach_request = serde_json::to_value(&request)
        .map_err(|error| format!("无法序列化运行请求副本: {error}"))?;
    let acceptance = fork_store.accept_fork_start(
        &request.conversation_id,
        fork_prompt_context_id.as_deref(),
        || {
            let initial_subscriber = on_event.clone();
            state.run_streams().begin(
                &request.conversation_id,
                &request_id,
                attach_request,
                Box::new(move |event| {
                    initial_subscriber
                        .send(event)
                        .map_err(|error| format!("模型流通道已关闭: {error}"))
                }),
            );
        },
    );
    if let Err(error) = acceptance {
        state
            .run_streams()
            .discard(&request.conversation_id, &request_id);
        state.finish_model_run(&request_id, &cancellation);
        state.recheck_task_wake(&wake_conversation_id);
        return Err(error);
    }
    let settle_conversation_id = request.conversation_id.clone();
    let state = state.inner().clone();
    // Task workers publish through the conversation-level surface. It publishes
    // only while a run is unsettled and must never fail when subscribers disconnect.
    // Each new run replaces the registration with current safety and workspace data.
    //
    // Keep approval behavior in `api::session_task_approval`, the shared command
    // and test implementation; this function is excluded from test builds.

    // The level this run executes under, from here on. Plan approval and a
    // renderer settings write both move it; every gate after this point reads
    // the cell rather than the snapshot `trusted_run_request` took.
    let live_security_level =
        state.live_security_level_for_run(&request.conversation_id, request.security_level);
    request.live_security_level = Some(Arc::clone(&live_security_level));
    let surface_approve = api::session_task_approval(
        &state,
        request.conversation_id.clone(),
        Arc::clone(&live_security_level),
        request.app_data_path.clone(),
        request.additional_directories.clone(),
        request.workspaces.clone(),
    );
    let surface_sink: Arc<api::OwnedModelEventSink> = {
        let sink_state = state.clone();
        let sink_conversation_id = request.conversation_id.clone();
        let sink_run_scope = request.request_id.clone();
        Arc::new(move |event: ModelStreamEvent| {
            let event =
                api::stamp_announced_context_id(event, &sink_conversation_id, &sink_run_scope);
            sink_state
                .run_streams()
                .publish_live(&sink_conversation_id, event);
            Ok(())
        })
    };
    state.register_task_surface(
        &request.conversation_id,
        Arc::new(crate::state::TaskSurface {
            sink: surface_sink,
            approve: Arc::clone(&surface_approve),
        }),
    );
    // Pass this run's cancellation flag to the approval callback. It must not
    // resolve cancellation by looking up the conversation's current run.
    let run_approve: Arc<api::OwnedDangerousToolApproval> = {
        let surface_approve = Arc::clone(&surface_approve);
        let approve_cancellation = cancellation.clone();
        Arc::new(move |request, descriptor, requester| {
            surface_approve(request, descriptor, requester, Some(&approve_cancellation))
        })
    };
    let worker_state = state.clone();
    let worker_cancellation = cancellation.clone();
    let sink_conversation_id = request.conversation_id.clone();
    let sink_run_scope = request.request_id.clone();
    let sink_state = state.clone();
    let worker_result = tauri::async_runtime::spawn_blocking(move || {
        let _operation = operation;
        let event_sink = move |event: ModelStreamEvent| {
            if worker_cancellation.load(std::sync::atomic::Ordering::Acquire) {
                return Err("模型运行已停止".into());
            }
            // Stream parsers announce a call without knowing which
            // conversation they are streaming into, but the timeline id has to
            // be derived from it. Stamping the id here — the one place every
            // announcement passes through with the conversation in scope —
            // means the renderer's streaming row and the host's persisted card
            // carry the same id, instead of each side inventing one and
            // disagreeing at save time.
            let event =
                api::stamp_announced_context_id(event, &sink_conversation_id, &sink_run_scope);
            // Hub delivery failure means a subscriber disconnected; cancellation
            // is the only sink error that ends a turn.
            sink_state
                .run_streams()
                .publish(&sink_conversation_id, event);
            Ok(())
        };
        // The approval card rides past the cancellation check above rather
        // than the sink: a card must still be answerable once the sink starts
        // refusing, and the sink refuses as soon as the run is cancelling —
        // which is exactly when the outstanding card needs to be retracted.
        api::run_model(request, &worker_state, &event_sink, &*run_approve)
    })
    .await;
    let worker_result = match worker_result {
        Ok(result) => result,
        Err(error) => Err(format!("模型运行后台任务失败: {error}")),
    };
    let cleanup_result = state.finish_model_run_checked(&request_id, &cancellation);
    let outcome = match (worker_result, cleanup_result) {
        (Ok(response), Ok(())) => Ok(response),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(cleanup_error)) => Err(cleanup_error),
        (Err(error), Err(cleanup_error)) => Err(format!(
            "{error}；此外无法确认模型运行回合清理：{cleanup_error}"
        )),
    };
    // Settle in the hub and broadcast `RunConcluded`. The invoking renderer and
    // reattached renderers consume the same idempotent settlement.
    state.run_streams().settle(
        &settle_conversation_id,
        &request_id,
        match &outcome {
            Ok(response) => crate::run_stream::RunSettlement::Completed(Box::new(response.clone())),
            Err(error) => crate::run_stream::RunSettlement::Failed(error.clone()),
        },
    );
    // Recheck after settlement: tasks completing after the final-round decision
    // cannot be delivered by that round. Early exits run the same recheck.
    state.recheck_task_wake(&settle_conversation_id);
    outcome
}

#[cfg(not(test))]
#[tauri::command]
fn cancel_model_run(state: State<'_, AppState>, request_id: String) -> Result<bool, String> {
    state.cancel_model_run(&request_id)
}

/// Cancels the current run by conversation identity. This remains valid after a
/// renderer reload and is idempotent.
#[cfg(not(test))]
#[tauri::command]
fn cancel_conversation_run(
    state: State<'_, AppState>,
    conversation_id: String,
) -> Result<bool, String> {
    state.cancel_conversation_model_run(&conversation_id)
}

/// IPC result for `attach_model_run`: `running` has replayed and attached the
/// stream, `finished` returns claimed settlement, and `none` has no resumable run.
#[derive(serde::Serialize)]
#[serde(
    rename_all = "camelCase",
    tag = "status",
    rename_all_fields = "camelCase"
)]
enum AttachRunResult {
    Running {
        request_id: String,
        request: serde_json::Value,
        dropped_events: u64,
    },
    Finished {
        request_id: String,
        request: serde_json::Value,
        settlement: RunSettlementPayload,
    },
    None,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RunSettlementPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    response: Option<Box<model::RunModelResponse>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[cfg(not(test))]
fn settlement_payload(settlement: run_stream::RunSettlement) -> RunSettlementPayload {
    match settlement {
        run_stream::RunSettlement::Completed(response) => RunSettlementPayload {
            response: Some(response),
            error: None,
        },
        run_stream::RunSettlement::Failed(error) => RunSettlementPayload {
            response: None,
            error: Some(error),
        },
    }
}

/// Takes over a conversation run stream by replaying buffered events and
/// installing `on_event` as the current subscriber. Finished runs return settlement.
#[cfg(not(test))]
#[tauri::command]
fn attach_model_run(
    state: State<'_, AppState>,
    conversation_id: String,
    on_event: Channel<ModelStreamEvent>,
) -> Result<AttachRunResult, String> {
    let outcome = state.run_streams().attach(
        &conversation_id,
        Box::new(move |event| {
            on_event
                .send(event)
                .map_err(|error| format!("模型流通道已关闭: {error}"))
        }),
    );
    Ok(match outcome {
        run_stream::AttachOutcome::Running {
            request_id,
            request,
            dropped_events,
        } => AttachRunResult::Running {
            request_id,
            request: *request,
            dropped_events,
        },
        run_stream::AttachOutcome::Finished {
            request_id,
            request,
            settlement,
        } => AttachRunResult::Finished {
            request_id,
            request: *request,
            settlement: settlement_payload(settlement),
        },
        run_stream::AttachOutcome::NotFound => AttachRunResult::None,
    })
}

/// Claims settlement announced by `RunConcluded`; only reattached runs need this
/// because the invoking renderer receives the same result from its promise.
#[cfg(not(test))]
#[tauri::command]
fn take_run_settlement(
    state: State<'_, AppState>,
    conversation_id: String,
) -> Result<Option<RunSettlementPayload>, String> {
    Ok(state
        .run_streams()
        .take_settlement(&conversation_id)
        .map(|(_, settlement)| settlement_payload(settlement)))
}

/// Stops a task, identified by subagent name, `workflow:<runId>`, or `shell:<id>`.
/// Tasks outlive model runs. A stopped task settles like any other terminal
/// result: it folds into the timeline and wakes an idle conversation, and its
/// result says the user closed it.
#[cfg(not(test))]
#[tauri::command]
fn stop_conversation_task(
    state: State<'_, AppState>,
    conversation_id: String,
    task: String,
) -> Result<bool, String> {
    if let Some(shell_task_id) = task.strip_prefix("shell:") {
        return Ok(state
            .shell_tasks
            .request_stop(&conversation_id, shell_task_id));
    }
    let Some(tasks) = state.existing_conversation_tasks(&conversation_id) else {
        return Ok(false);
    };
    let entry = tasks
        .pool
        .find(&task)
        .or_else(|| tasks.pool.find_by_label(&task));
    match entry {
        Some(shared) => {
            shared.stop_task(crate::agents::StopOrigin::User);
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Restores level-triggered task wake-ups. `TaskSettled` is an edge event and
/// renderer reloads lose in-memory accounting, so scan deliverable results in
/// conversations without active runs.
#[cfg(not(test))]
#[tauri::command]
fn list_wake_pending_conversations(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    Ok(state.wake_pending_conversations())
}

/// One approval card that is still waiting for an answer, addressed to the
/// conversation it belongs to.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingToolPromptEntry {
    pub conversation_id: String,
    #[serde(flatten)]
    pub prompt: tool_prompt::PendingToolPrompt,
}

/// Re-lists every unanswered approval card so a renderer that just started (or
/// just reloaded) can draw them again.
///
/// Cards raised inside a live run come back with that run's `attach` replay.
/// Cards raised by a background task do not: an S11 task can outlive every open
/// run stream, and the push event that first announced its card may already be
/// delivered. Re-listing prevents a worker from waiting behind an invisible card
/// until the 30-minute timeout.
#[cfg(not(test))]
#[tauri::command]
fn list_pending_tool_prompts(
    state: State<'_, AppState>,
) -> Result<Vec<PendingToolPromptEntry>, String> {
    Ok(state
        .tool_prompts()
        .all_pending_cards()
        .into_iter()
        .map(|(conversation_id, prompt)| PendingToolPromptEntry {
            conversation_id,
            prompt,
        })
        .collect())
}

/// Re-lists every open fork request so a renderer that just started (or just
/// reloaded) can draw the tray again. The push event that first announced a
/// card is long delivered by then, and the request does not die with any run.
#[cfg(not(test))]
#[tauri::command]
fn list_pending_fork_requests(
    state: State<'_, AppState>,
) -> Result<Vec<fork_requests::PendingForkRequest>, String> {
    Ok(state.fork_requests().pending_cards())
}

#[cfg(not(test))]
#[tauri::command]
fn list_pending_fork_starts(
    app: AppHandle,
) -> Result<Vec<conversation_store::PendingForkStart>, String> {
    conversations::store(&document_path(&app)?)?.pending_fork_starts()
}

/// Every answered fork request a conversation raised, oldest first, for the
/// task bar. The model never sees these: they are not tasks of the run.
#[cfg(not(test))]
#[tauri::command]
fn list_fork_decisions(
    app: AppHandle,
    conversation_id: String,
) -> Result<Vec<fork_requests::ForkDecisionRecord>, String> {
    conversations::store(&document_path(&app)?)?.fork_decisions(&conversation_id)
}

/// Answers one fork card. Approval creates the child conversation and returns
/// it; the `forkResolved` push event goes out on both answers so every surface
/// takes the card down, and it — not this return value — is what starts the
/// child's run and carries the decision the task bar records.
#[cfg(not(test))]
#[tauri::command]
fn resolve_fork_request(
    app: AppHandle,
    state: State<'_, AppState>,
    fork_id: String,
    approved: bool,
) -> Result<Option<Conversation>, String> {
    let path = document_path(&app)?;
    fork_requests::resolve_fork_request(&state, &path, &fork_id, approved)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ResumableRunRow {
    conversation_id: String,
    request_id: String,
    running: bool,
}

/// Lists runs with live streams or unclaimed settlement so the renderer can
/// restore its streaming interface after startup or document loading.
#[cfg(not(test))]
#[tauri::command]
fn list_resumable_runs(state: State<'_, AppState>) -> Result<Vec<ResumableRunRow>, String> {
    Ok(state
        .run_streams()
        .list()
        .into_iter()
        .map(|run| ResumableRunRow {
            conversation_id: run.conversation_id,
            request_id: run.request_id,
            running: run.running,
        })
        .collect())
}

#[cfg(not(test))]
#[tauri::command]
fn steer_model_run(
    state: State<'_, AppState>,
    request_id: String,
    message_id: String,
    content: String,
    images: Option<Vec<model::ImageAttachment>>,
    created_at: String,
) -> Result<(), String> {
    state.steer_model_run(
        &request_id,
        message_id,
        content,
        images.unwrap_or_default(),
        created_at,
    )
}

#[cfg(not(test))]
#[tauri::command]
fn workflow_step_control(
    state: State<'_, AppState>,
    request_id: String,
    run_id: String,
    step_index: usize,
    action: String,
) -> Result<(), String> {
    if request_id.is_empty() || request_id.len() > 128 || request_id.chars().any(char::is_control) {
        return Err("模型运行 ID 无效".into());
    }
    if run_id.is_empty() || run_id.len() > 128 || run_id.chars().any(char::is_control) {
        return Err("工作流运行 ID 无效".into());
    }
    state.workflow_step_control(&request_id, &run_id, step_index, &action)
}

/// Fetches the complete record for an externalized workflow step on demand.
/// Timeline `workflow_step` contexts contain only previews and fingerprints;
/// missing files are a valid result when their conversation or record was removed.
#[cfg(not(test))]
#[tauri::command]
fn workflow_step_record(
    app: AppHandle,
    conversation_id: String,
    run_id: String,
    step_index: u32,
) -> Result<Option<serde_json::Value>, String> {
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法解析应用数据目录: {error}"))?;
    workflow_store::read_step_record(&app_data, &conversation_id, &run_id, step_index)
}

#[cfg(not(test))]
const BROWSER_RENDERER_MOUNT_ERROR: &str = "浏览器页面写入权限无效或已失效，请等待界面恢复后重试";
#[cfg(not(test))]
const BROWSER_RENDERER_MOUNT_MAIN_LABEL: &str = "main";
#[cfg(not(test))]
const BROWSER_RENDERER_MOUNT_CHALLENGE_EVENT: &str = "mework:browser-renderer-mount-challenge";
#[cfg(not(test))]
const BROWSER_RENDERER_MOUNT_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(12);
#[cfg(not(test))]
const BROWSER_RENDERER_MOUNT_WATCHDOG_POLL: Duration = Duration::from_secs(4);
/// How long a launch that found the app-data lease taken keeps trying to either
/// wake the holder or take over its lease before reporting the conflict.
#[cfg(not(test))]
const PROCESS_LEASE_HANDOVER_WAIT: Duration = Duration::from_secs(5);

#[cfg(not(test))]
fn browser_renderer_mount_error(
    _error: browser_renderer_mount::BrowserRendererMountError,
) -> String {
    BROWSER_RENDERER_MOUNT_ERROR.to_owned()
}

#[cfg(not(test))]
fn require_main_browser_renderer(webview: &Webview) -> Result<(), String> {
    if webview.label() == BROWSER_RENDERER_MOUNT_MAIN_LABEL {
        Ok(())
    } else {
        Err(BROWSER_RENDERER_MOUNT_ERROR.to_owned())
    }
}

#[cfg(not(test))]
fn with_validated_browser_renderer_mutation<T>(
    state: &AppState,
    renderer_mount_id: &str,
    renderer_mount_generation: u64,
    mutation: impl FnOnce() -> T,
) -> Result<T, String> {
    state
        .browser_renderer_mounts
        .with_validated_mutation(renderer_mount_id, renderer_mount_generation, mutation)
        .map_err(browser_renderer_mount_error)
}

#[cfg(not(test))]
fn hide_invalidated_browser_renderer_presentations(
    mounts: &browser_renderer_mount::BrowserRendererMountRegistry,
    browser: &browser::BrowserRuntime,
) -> Result<(), String> {
    // `latest_generation` may be zero before the first renderer lease. Zero is
    // still useful: it hides an unowned legacy/orphan presentation without
    // granting authority to any renderer.
    let hide_through_generation = mounts
        .latest_generation()
        .map_err(browser_renderer_mount_error)?;
    browser.fail_safe_hide_renderer_presentations_through(hide_through_generation)?;

    while let Some(action) = mounts
        .pending_presentation_orphan_action()
        .map_err(browser_renderer_mount_error)?
    {
        if action.hide_through_generation > hide_through_generation {
            break;
        }
        mounts
            .acknowledge_presentation_orphan_action(action.hide_through_generation)
            .map_err(browser_renderer_mount_error)?;
    }
    Ok(())
}

/// Re-runs the presentation fail-safe for an invalidation that landed while a
/// renderer-origin mutation authorized by the invalidated mount was still
/// running.
///
/// The registry lock is no longer held across those mutations (it cannot be —
/// they wait on the main thread, which also takes that lock), so a mutation may
/// bind a native presentation just after the invalidating fail-safe ran. This
/// closes that window once the mutation settles.
#[cfg(not(test))]
fn settle_browser_renderer_presentation_fence(
    mounts: &browser_renderer_mount::BrowserRendererMountRegistry,
    browser: &browser::BrowserRuntime,
) {
    match mounts.take_settled_presentation_fence() {
        Ok(false) => {}
        Ok(true) => {
            if let Err(error) = hide_invalidated_browser_renderer_presentations(mounts, browser) {
                eprintln!(
                    "浏览器 renderer mount 失效后的延迟 presentation fail-safe 失败：{error}"
                );
                // The fence is still owed. Put it back so the watchdog retries
                // instead of leaving an orphaned presentation visible.
                if mounts.mark_presentation_fence_dirty().is_err() {
                    eprintln!("无法重新登记待执行的浏览器 presentation fail-safe");
                }
            }
        }
        Err(_) => eprintln!("无法读取浏览器 renderer mount presentation fail-safe 状态"),
    }
}

/// Non-blocking variant of [`hide_invalidated_browser_renderer_presentations`].
///
/// `Ok(false)` means the browser lifecycle mutex was busy and the caller must
/// have the fail-safe retried on a thread that is allowed to block.
#[cfg(not(test))]
fn try_hide_invalidated_browser_renderer_presentations(
    mounts: &browser_renderer_mount::BrowserRendererMountRegistry,
    browser: &browser::BrowserRuntime,
) -> Result<bool, String> {
    let hide_through_generation = mounts
        .latest_generation()
        .map_err(browser_renderer_mount_error)?;
    if browser
        .try_fail_safe_hide_renderer_presentations_through(hide_through_generation)?
        .is_none()
    {
        return Ok(false);
    }

    while let Some(action) = mounts
        .pending_presentation_orphan_action()
        .map_err(browser_renderer_mount_error)?
    {
        if action.hide_through_generation > hide_through_generation {
            break;
        }
        mounts
            .acknowledge_presentation_orphan_action(action.hide_through_generation)
            .map_err(browser_renderer_mount_error)?;
    }
    Ok(true)
}

#[cfg(not(test))]
fn begin_main_browser_renderer_document_load(app: &AppHandle) {
    let state = app.state::<AppState>();
    let challenge = match state.browser_renderer_mounts.begin_main_document_load() {
        Ok(challenge) => challenge,
        Err(_) => {
            eprintln!("主界面开始加载时无法撤销旧浏览器 renderer mount");
            return;
        }
    };
    // This hook runs on the main thread, so the fail-safe must not wait for the
    // browser lifecycle mutex: an in-flight native mutation holds it while
    // waiting on this very thread. A deferred run is still guaranteed, so the
    // challenge stays valid; only an outright failure keeps the new document
    // read-only until a later full document load.
    match try_hide_invalidated_browser_renderer_presentations(
        state.browser_renderer_mounts.as_ref(),
        &state.browser,
    ) {
        Ok(true) => {}
        Ok(false) => {
            if state
                .browser_renderer_mounts
                .mark_presentation_fence_dirty()
                .is_err()
            {
                let _ = state
                    .browser_renderer_mounts
                    .abort_main_document_load(&challenge);
                eprintln!("主界面开始加载时无法登记待执行的浏览器 presentation fail-safe");
            }
        }
        Err(error) => {
            // Never inject a challenge after presentation cleanup failed. The new
            // document remains read-only until a later full document load.
            let _ = state
                .browser_renderer_mounts
                .abort_main_document_load(&challenge);
            eprintln!("主界面开始加载时浏览器 presentation fail-safe 失败：{error}");
        }
    }
}

#[cfg(not(test))]
fn finish_main_browser_renderer_document_load(webview: &Webview) {
    let state = webview.app_handle().state::<AppState>();
    let challenge = match state.browser_renderer_mounts.current_bootstrap_challenge() {
        Ok(Some(challenge)) => challenge,
        Ok(None) => return,
        Err(_) => {
            eprintln!("主界面加载完成时无法读取浏览器 renderer mount challenge");
            return;
        }
    };
    let challenge_literal = match serde_json::to_string(&challenge) {
        Ok(value) => value,
        Err(_) => return,
    };
    let script = format!(
        "window.__MEWORK_BROWSER_RENDERER_MOUNT_CHALLENGE__={challenge_literal};\
         window.dispatchEvent(new Event({event}));",
        event = serde_json::to_string(BROWSER_RENDERER_MOUNT_CHALLENGE_EVENT)
            .expect("fixed renderer mount event name must serialize")
    );
    if webview.eval(&script).is_err() {
        eprintln!("主界面加载完成后无法注入浏览器 renderer mount challenge");
    }
}

#[cfg(not(test))]
fn start_browser_renderer_mount_watchdog(state: &AppState) -> Result<(), String> {
    let mounts = Arc::downgrade(&state.browser_renderer_mounts);
    let browser = state.browser.clone();
    std::thread::Builder::new()
        .name("mework-browser-renderer-mount-watchdog".to_owned())
        .spawn(move || loop {
            std::thread::sleep(BROWSER_RENDERER_MOUNT_WATCHDOG_POLL);
            let Some(mounts) = mounts.upgrade() else {
                break;
            };
            match mounts.invalidate_stale_mount(BROWSER_RENDERER_MOUNT_HEARTBEAT_TIMEOUT) {
                Ok(true) => {
                    if let Err(error) =
                        hide_invalidated_browser_renderer_presentations(&mounts, &browser)
                    {
                        eprintln!(
                            "浏览器 renderer mount 心跳失效后的 presentation fail-safe 失败：{error}"
                        );
                    }
                }
                Ok(false) => {}
                Err(_) => {
                    eprintln!("浏览器 renderer mount 心跳监视器无法读取租约状态");
                    break;
                }
            }
            // Backstop for any invalidation that raced an in-flight mutation.
            // `browser_open` settles its own fence immediately; this covers the
            // rest within one poll.
            settle_browser_renderer_presentation_fence(&mounts, &browser);
        })
        .map(|_| ())
        .map_err(|error| format!("无法启动浏览器 renderer mount 心跳监视器: {error}"))
}

#[cfg(not(test))]
fn image_attachment_store(
    app: &AppHandle,
) -> Result<image_attachments::ImageAttachmentStore, String> {
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法解析应用数据目录: {error}"))?;
    Ok(image_attachments::ImageAttachmentStore::new(&app_data))
}

#[cfg(not(test))]
fn reconcile_image_attachments_on_startup(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = document_path(app)?;
    let app_data = path
        .parent()
        .ok_or_else(|| "数据文档没有应用数据父目录".to_owned())?;
    let image_store = image_attachments::ImageAttachmentStore::new(app_data);
    // Reconcile stale pending bodies before loading or starting any run; otherwise
    // newly created streaming rows could be misclassified as interrupted.
    match conversation_store::store_for(&path) {
        Ok(store) => match store.reconcile_streaming() {
            Ok(0) => {}
            Ok(count) => eprintln!("已把 {count} 条上次会话中断的正文标记为中断片段"),
            Err(error) => eprintln!("未定稿正文回收失败：{error}"),
        },
        Err(error) => eprintln!("对话库不可用：{error}"),
    }
    if let Err(error) = state.shell_tasks.install_store(app_data) {
        use tauri_plugin_dialog::DialogExt;
        app.dialog()
            .message(format!("命令历史加载失败；原始记录未清空。\nShell history could not be loaded; existing records were not cleared.\n\n{error}"))
            .title("Mework — 历史恢复失败 / History recovery failed")
            .kind(tauri_plugin_dialog::MessageDialogKind::Error)
            .blocking_show();
        return Err(error);
    }
    state
        .document_store
        .initialize_with_startup_reconciliation(&path, |document| {
            state.shell_tasks.try_forget_missing(
                document
                    .workspaces
                    .iter()
                    .flat_map(|workspace| workspace.conversations.iter())
                    .map(|conversation| conversation.id.as_str()),
            )?;
            // This is the pass that physically deletes, so template bodies —
            // the references no document carries — must be in hand before it
            // runs. Unreadable means skip, not "nothing is referenced".
            match conversations::template_image_ids(&path) {
                Err(error) => {
                    eprintln!("启动时无法读取模板图片引用，本次不回收图片附件：{error}");
                }
                Ok(pinned) => {
                    if let Err(error) = image_store.reconcile_startup(document, &pinned) {
                        // Attachment cleanup is recoverable; startup continues and the
                        // reconciliation retries on the next launch.
                        eprintln!("启动时未能回收图片附件，将在下次启动重试：{error}");
                    }
                }
            }
            // Normal deletion occurs in the save transaction; remove only
            // orphaned workflow directories left by an interrupted deletion.
            let reaped = workflow_store::reap_conversation_orphans(app_data, document);
            if reaped > 0 {
                eprintln!("已清理 {reaped} 个孤儿工作流运行目录");
            }
            // Claim manifests left in `running` state as interrupted. Deliver
            // their notices at the conversation's next run boundary; manifest
            // state preserves both claim and delivery across another crash.
            let interrupted = workflow_store::sweep_interrupted_runs(app_data, document);
            if !interrupted.is_empty() {
                eprintln!(
                    "发现 {} 个因应用退出而中断的工作流运行，已排队恢复通知",
                    interrupted.len()
                );
                state.seed_workflow_restart_notices(interrupted);
            }
            Ok(())
        })?;
    Ok(())
}

#[cfg(not(test))]
#[tauri::command]
fn image_attachment_upload(
    app: AppHandle,
    name: String,
    data: String,
) -> Result<model::ImageAttachment, String> {
    let max_encoded = image_attachments::MAX_IMAGE_ATTACHMENT_BYTES
        .saturating_mul(4)
        .div_ceil(3)
        .saturating_add(4);
    if data.len() > max_encoded {
        return Err(format!(
            "图片 base64 超过 {} MiB 原始字节限制",
            image_attachments::MAX_IMAGE_ATTACHMENT_BYTES / 1024 / 1024
        ));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.as_bytes())
        .map_err(|error| format!("图片 base64 无效: {error}"))?;
    image_attachment_store(&app)?.import(&name, &bytes)
}

#[cfg(not(test))]
#[tauri::command]
fn image_attachment_data(app: AppHandle, image_id: String) -> Result<String, String> {
    image_attachment_store(&app)?.data_url_by_id(&image_id)
}

/// Site icon for one web-search source, as a `data:` URL.
///
/// The renderer cannot fetch it itself — the CSP allows `img-src 'self' data:`
/// and no outbound `connect-src` — so the host does it. Returning `None` rather
/// than an error is the point: a site without a usable icon is ordinary, and the
/// card draws a lettered placeholder instead.
///
/// The renderer passes a source URL rather than a bare host so it cannot name a
/// fetch target of its own; only the host part is used, and only for a public
/// name.
#[cfg(not(test))]
#[tauri::command]
async fn web_source_icon(app: AppHandle, url: String) -> Result<Option<String>, String> {
    let Some(host) = favicon::icon_host(&url) else {
        return Ok(None);
    };
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法解析应用数据目录：{error}"))?;
    // Blocking network and disk work; keep it off the async runtime's threads.
    tauri::async_runtime::spawn_blocking(move || favicon::site_icon(&app_data, &host))
        .await
        .map_err(|error| format!("站点图标查询失败：{error}"))
}

#[cfg(not(test))]
#[tauri::command]
fn browser_register_renderer_mount(
    webview: Webview,
    state: State<'_, AppState>,
    challenge: String,
) -> Result<browser_renderer_mount::BrowserRendererMountLease, String> {
    require_main_browser_renderer(&webview)?;
    state
        .browser_renderer_mounts
        .register_bootstrapped_mount(&challenge)
        .map_err(browser_renderer_mount_error)
}

#[cfg(not(test))]
#[tauri::command]
fn browser_renderer_mount_heartbeat(
    webview: Webview,
    state: State<'_, AppState>,
    mount_id: String,
    generation: u64,
) -> Result<(), String> {
    require_main_browser_renderer(&webview)?;
    state
        .browser_renderer_mounts
        .heartbeat(&mount_id, generation)
        .map_err(browser_renderer_mount_error)
}

#[cfg(not(test))]
#[tauri::command]
async fn browser_open(
    state: State<'_, AppState>,
    session_id: String,
    url: Option<String>,
    lifecycle_epoch: u64,
    renderer_mount_id: String,
    renderer_mount_generation: u64,
) -> Result<browser::BrowserStatus, String> {
    let runtime = state.browser.clone();
    let mounts = state.browser_renderer_mounts.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let opened = mounts
            .with_validated_mutation(&renderer_mount_id, renderer_mount_generation, || {
                runtime.show_with_renderer_mount_intent(
                    &session_id,
                    url.as_deref(),
                    lifecycle_epoch,
                    renderer_mount_generation,
                )
            })
            .map_err(browser_renderer_mount_error);
        // Opening is the only renderer-origin mutation that binds a native
        // presentation to a mount generation, so it must not leave a visible
        // page behind if its mount was invalidated while the page was opening.
        settle_browser_renderer_presentation_fence(&mounts, &runtime);
        opened?
    })
    .await
    .map_err(|error| format!("打开内置浏览器后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn browser_status(
    state: State<'_, AppState>,
    session_id: String,
    renderer_mount_id: String,
    renderer_mount_generation: u64,
) -> Result<browser::BrowserStatus, String> {
    // This is the most frequent browser command. Reading a status touches the
    // native page (url/size/scale), so it must not run on the main thread: a
    // synchronous command would spend main-thread time on every poll and would
    // queue behind whatever native work is in flight.
    let runtime = state.browser.clone();
    let mounts = state.browser_renderer_mounts.clone();
    tauri::async_runtime::spawn_blocking(move || {
        mounts
            .with_validated_mutation(&renderer_mount_id, renderer_mount_generation, || {
                runtime.status(&session_id)
            })
            .map_err(browser_renderer_mount_error)
    })
    .await
    .map_err(|error| format!("读取内置浏览器状态后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn browser_set_panel_bounds(
    state: State<'_, AppState>,
    session_id: String,
    bounds: browser::BrowserPanelBounds,
    lifecycle_epoch: u64,
    renderer_mount_id: String,
    renderer_mount_generation: u64,
) -> Result<browser::BrowserStatus, String> {
    let runtime = state.browser.clone();
    let mounts = state.browser_renderer_mounts.clone();
    tauri::async_runtime::spawn_blocking(move || {
        mounts
            .with_validated_mutation(&renderer_mount_id, renderer_mount_generation, || {
                runtime.set_panel_bounds_with_renderer_mount_intent(
                    &session_id,
                    bounds,
                    lifecycle_epoch,
                    renderer_mount_generation,
                )
            })
            .map_err(browser_renderer_mount_error)?
    })
    .await
    .map_err(|error| format!("同步浏览器侧边栏布局后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn browser_navigate(
    state: State<'_, AppState>,
    session_id: String,
    url: String,
    renderer_mount_id: String,
    renderer_mount_generation: u64,
) -> Result<browser::BrowserStatus, String> {
    let runtime = state.browser.clone();
    let mounts = state.browser_renderer_mounts.clone();
    tauri::async_runtime::spawn_blocking(move || {
        mounts
            .with_validated_mutation(&renderer_mount_id, renderer_mount_generation, || {
                runtime.session(&session_id)?.navigate_as_user(&url)
            })
            .map_err(browser_renderer_mount_error)?
    })
    .await
    .map_err(|error| format!("浏览器导航后台任务失败: {error}"))?
}

/// Returns the visible page as a base64 PNG so the pane can draw on it.
///
/// This is a trusted-chrome action, so it claims the page for the user the way the toolbar's
/// screenshot entry does rather than taking Agent control.
#[cfg(not(test))]
#[tauri::command]
async fn browser_capture_page(
    state: State<'_, AppState>,
    session_id: String,
    renderer_mount_id: String,
    renderer_mount_generation: u64,
) -> Result<Option<browser::BrowserPageCapture>, String> {
    let runtime = state.browser.clone();
    let mounts = state.browser_renderer_mounts.clone();
    tauri::async_runtime::spawn_blocking(move || {
        mounts
            .with_validated_mutation(&renderer_mount_id, renderer_mount_generation, || {
                let session = runtime.session(&session_id)?;
                session.with_user_control(|| session.capture_page())
            })
            .map_err(browser_renderer_mount_error)?
    })
    .await
    .map_err(|error| format!("浏览器页面截图后台任务失败: {error}"))?
}

/// Drains the element the user picked in the built-in browser, if one is waiting.
///
/// `Ok(None)` is the ordinary answer: the pane's poll asks on every tick and a click is rare. The
/// payload is carried here rather than on `browser_status` because two independent pollers read
/// that status every 700 ms, and a cropped PNG has no business riding either of them.
#[cfg(not(test))]
#[tauri::command]
async fn browser_take_selected_element(
    state: State<'_, AppState>,
    session_id: String,
    renderer_mount_id: String,
    renderer_mount_generation: u64,
) -> Result<Option<browser::SelectedElement>, String> {
    let runtime = state.browser.clone();
    let mounts = state.browser_renderer_mounts.clone();
    tauri::async_runtime::spawn_blocking(move || {
        mounts
            .with_validated_mutation(&renderer_mount_id, renderer_mount_generation, || {
                let session = runtime.session(&session_id)?;
                session.with_user_control(|| session.take_selected_element())
            })
            .map_err(browser_renderer_mount_error)?
    })
    .await
    .map_err(|error| format!("浏览器取回所选元素后台任务失败: {error}"))?
}

/// Opens the system file picker and displays the chosen file in the built-in browser.
///
/// There is deliberately no `path` parameter: the picker *is* the authorization, and a
/// renderer-supplied path would make that a lie. `None` means the user cancelled.
#[cfg(not(test))]
#[tauri::command]
async fn browser_open_local_file(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    target: Option<GitTarget>,
    renderer_mount_id: String,
    renderer_mount_generation: u64,
) -> Result<Option<browser::BrowserStatus>, String> {
    // The reference opens this picker with no owner window at all, and that is what makes it work
    // in a host that has none — the browser-dev backend clears them. An owner is still used when
    // there is one, because an ownerless dialog is one Windows may leave behind the application
    // window, which is indistinguishable from a dead menu item.
    let parent = app.get_webview_window(BROWSER_RENDERER_MOUNT_MAIN_LABEL);
    // Where the picker opens, mirroring the reference's `defaultPath`. A workspace that will not
    // resolve is not a failure here: the dialog just starts wherever the system last left it.
    let starting_directory = match target {
        Some(target) => {
            let state = state.inner().clone();
            let resolve_app = app.clone();
            tauri::async_runtime::spawn_blocking(move || {
                trusted_target_workspace_operation(
                    &resolve_app,
                    &state,
                    &target,
                    "本地文件预览",
                    TrustedWorkspaceAccess::Shared,
                    None,
                )
                .ok()
                .map(|operation| operation.workspace_path)
            })
            .await
            .ok()
            .flatten()
        }
        None => None,
    };
    let Some(path) = pick_previewable_file(&app, parent.as_ref(), starting_directory).await? else {
        return Ok(None);
    };
    preview_local_file_in_browser(
        app,
        session_id,
        path,
        renderer_mount_id,
        renderer_mount_generation,
    )
    .await
}

/// Runs the system file picker and waits for its answer without blocking a runtime worker.
///
/// The plugin's blocking variant discards the error from its own main-thread dispatch and then
/// waits on a rendezvous channel nobody will ever send to, so a host that cannot raise a dialog
/// hangs the command forever rather than reporting it. Owning the channel here turns that case
/// into a dropped sender, which is a sentence the user can read.
#[cfg(not(test))]
async fn pick_previewable_file(
    app: &AppHandle,
    parent: Option<&tauri::WebviewWindow>,
    starting_directory: Option<std::path::PathBuf>,
) -> Result<Option<std::path::PathBuf>, String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let mut builder = app
        .dialog()
        .file()
        .set_title("选择要预览的文件")
        .add_filter("Previewable", &browser_file_preview::PREVIEWABLE_EXTENSIONS);
    if let Some(parent) = parent {
        builder = builder.set_parent(parent);
    }
    if let Some(directory) = starting_directory {
        builder = builder.set_directory(directory);
    }
    builder.pick_file(move |selected| {
        let _ = sender.send(selected);
    });
    let Some(selected) = receiver
        .await
        .map_err(|_| "此宿主无法打开系统文件选择器".to_owned())?
    else {
        return Ok(None);
    };
    selected
        .into_path()
        .map(Some)
        .map_err(|error| format!("系统文件选择结果无效: {error}"))
}

#[cfg(not(test))]
async fn preview_local_file_in_browser(
    app: AppHandle,
    session_id: String,
    path: std::path::PathBuf,
    renderer_mount_id: String,
    renderer_mount_generation: u64,
) -> Result<Option<browser::BrowserStatus>, String> {
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法解析应用数据目录: {error}"))?;
    let (browser, mounts) = {
        let state = app.state::<AppState>();
        (state.browser.clone(), state.browser_renderer_mounts.clone())
    };
    tauri::async_runtime::spawn_blocking(move || {
        mounts
            .with_validated_mutation(&renderer_mount_id, renderer_mount_generation, || {
                browser
                    .session(&session_id)?
                    .preview_local_file(&app_data, &path)
            })
            .map_err(browser_renderer_mount_error)?
    })
    .await
    .map_err(|error| format!("本地文件预览后台任务失败: {error}"))?
    .map(Some)
}

/// Publishes the Closed lifecycle intent inside the caller's renderer-mount critical section.
///
/// Publishing synchronously is what keeps a newer Open from slipping between mount validation
/// and the blocking task that performs native teardown.
#[cfg(not(test))]
fn begin_browser_close(
    browser: &browser::BrowserRuntime,
    session_id: &str,
    lifecycle_epoch: u64,
) -> Result<(), browser::BrowserCloseDisposition> {
    browser
        .with_close_intent_fence(session_id, lifecycle_epoch, || Ok::<(), String>(()))
        .map_err(|error| browser::BrowserCloseDisposition::rejected_lifecycle(&error))
}

#[cfg(not(test))]
#[tauri::command]
async fn browser_close(
    state: State<'_, AppState>,
    session_id: String,
    lifecycle_epoch: u64,
    renderer_mount_id: String,
    renderer_mount_generation: u64,
) -> Result<browser::BrowserCloseDisposition, String> {
    let browser = state.browser.clone();
    if let Err(disposition) = with_validated_browser_renderer_mutation(
        state.inner(),
        &renderer_mount_id,
        renderer_mount_generation,
        || begin_browser_close(&browser, &session_id, lifecycle_epoch),
    )? {
        return Ok(disposition);
    }
    let task_browser = browser.clone();
    Ok(tauri::async_runtime::spawn_blocking(move || {
        task_browser.close_after_accepted_intent(&session_id, lifecycle_epoch)
    })
    .await
    .unwrap_or_else(|_| browser::BrowserCloseDisposition::internal_failure()))
}

#[cfg(not(test))]
#[tauri::command]
async fn browser_action(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    action: String,
    value: Option<serde_json::Value>,
    lifecycle_epoch: Option<u64>,
    renderer_mount_id: String,
    renderer_mount_generation: u64,
) -> Result<browser::BrowserStatus, String> {
    let browser = state.browser.clone();
    if action == "close" {
        let lifecycle_epoch = lifecycle_epoch
            .ok_or_else(|| "关闭内置浏览器需要可信 UI 签发的生命周期 epoch".to_owned())?;
        with_validated_browser_renderer_mutation(
            state.inner(),
            &renderer_mount_id,
            renderer_mount_generation,
            || begin_browser_close(&browser, &session_id, lifecycle_epoch),
        )?
        .map_err(|disposition| {
            disposition
                .message
                .unwrap_or_else(|| "内置浏览器关闭请求被拒绝".to_owned())
        })?;
        let disposition = tauri::async_runtime::spawn_blocking(move || {
            browser.close_after_accepted_intent(&session_id, lifecycle_epoch)
        })
        .await
        .unwrap_or_else(|_| browser::BrowserCloseDisposition::internal_failure());
        return if disposition.cleanup_complete {
            Ok(browser::BrowserStatus::default())
        } else {
            Err(disposition
                .message
                .unwrap_or_else(|| "内置浏览器关闭清理尚未完成".to_owned()))
        };
    }
    if action == "hide" {
        let lifecycle_epoch = lifecycle_epoch
            .ok_or_else(|| "收起内置浏览器需要可信 UI 签发的生命周期 epoch".to_owned())?;
        let mounts = state.browser_renderer_mounts.clone();
        return tauri::async_runtime::spawn_blocking(move || {
            mounts
                .with_validated_mutation(&renderer_mount_id, renderer_mount_generation, || {
                    browser.hide_with_intent(&session_id, lifecycle_epoch)
                })
                .map_err(browser_renderer_mount_error)?
        })
        .await
        .map_err(|error| format!("浏览器操作后台任务失败: {error}"))?;
    }
    if lifecycle_epoch.is_some() {
        return Err("浏览器生命周期 epoch 只能用于打开、收起、关闭或布局操作".to_owned());
    }
    let mounts = state.browser_renderer_mounts.clone();
    tauri::async_runtime::spawn_blocking(move || {
        mounts
            .with_validated_mutation(
                &renderer_mount_id,
                renderer_mount_generation,
                || {
                    let runtime = browser.session(&session_id)?;
                    if matches!(action.as_str(), "back" | "forward" | "reload") {
                        return browser.navigate_history_as_user(&session_id, &action);
                    }
                    let takes_user_control = matches!(
                        action.as_str(),
                        "stop"
                            | "devtools"
                            | "screenshot"
                            | "zoom_in"
                            | "zoom_out"
                            | "zoom_reset"
                            | "clear_data"
                            | "find"
                            | "print"
                            | "select_element"
                            | "zoom"
                            | "viewport"
                            | "take_control"
                            | "handoff_agent"
                    );
                    let execute = || -> Result<browser::BrowserStatus, String> {
                        match action.as_str() {
                            "stop" => return runtime.stop(),
                            "devtools" => {
                                runtime.devtools(true)?;
                            }
                            "screenshot" => {
                                let directory = app
                                    .path()
                                    .app_data_dir()
                                    .map_err(|error| {
                                        format!("无法解析应用数据目录: {error}")
                                    })?
                                    .join("browser-screenshots");
                                std::fs::create_dir_all(&directory).map_err(|error| {
                                    format!("无法创建浏览器截图目录: {error}")
                                })?;
                                let filename = format!(
                                    "browser-{}-{}.png",
                                    chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ"),
                                    uuid::Uuid::new_v4().simple()
                                );
                                runtime.screenshot(&directory.join(filename), false)?;
                            }
                            "zoom_in" => {
                                let factor = (runtime.status().zoom + 0.1).min(5.0);
                                runtime.set_zoom(factor)?;
                            }
                            "zoom_out" => {
                                let factor = (runtime.status().zoom - 0.1).max(0.25);
                                runtime.set_zoom(factor)?;
                            }
                            "zoom_reset" => {
                                runtime.set_zoom(1.0)?;
                            }
                            "clear_data" => {
                                runtime.clear_data()?;
                            }
                            "find" => {
                                let query = value
                                    .as_ref()
                                    .and_then(serde_json::Value::as_str)
                                    .ok_or_else(|| {
                                        "browser_action find 需要字符串 value".to_owned()
                                    })?;
                                runtime.find_text(query)?;
                            }
                            "print" => {
                                runtime.print_page()?;
                            }
                            "select_element" => {
                                let armed = value
                                    .as_ref()
                                    .and_then(serde_json::Value::as_bool)
                                    .ok_or_else(|| {
                                        "browser_action select_element 需要布尔 value".to_owned()
                                    })?;
                                runtime.arm_element_picker(armed)?;
                            }
                            "occlude" => {
                                let occluded = value
                                    .as_ref()
                                    .and_then(serde_json::Value::as_bool)
                                    .ok_or_else(|| {
                                        "browser_action occlude 需要布尔 value".to_owned()
                                    })?;
                                return runtime.set_occluded(occluded);
                            }
                            "project" => {
                                let projected = value
                                    .as_ref()
                                    .and_then(serde_json::Value::as_bool)
                                    .ok_or_else(|| {
                                        "browser_action project 需要布尔 value".to_owned()
                                    })?;
                                return runtime.set_projected(projected);
                            }
                            "theme" => {
                                let theme = value
                                    .as_ref()
                                    .and_then(serde_json::Value::as_str)
                                    .ok_or_else(|| {
                                        "browser_action theme 需要 day 或 night 字符串 value"
                                            .to_owned()
                                    })?;
                                return runtime.set_ui_theme(theme);
                            }
                            "locale" => {
                                let language = value
                                    .as_ref()
                                    .and_then(serde_json::Value::as_str)
                                    .ok_or_else(|| {
                                        "browser_action locale 需要 zh 或 en 字符串 value"
                                            .to_owned()
                                    })?;
                                return runtime.set_ui_language(language);
                            }
                            "take_control" => {
                                return Ok(runtime.take_user_control());
                            }
                            "handoff_agent" => {
                                return Ok(runtime.handoff_to_agent());
                            }
                            "suspend" => {
                                return browser.suspend(&session_id);
                            }
                            "zoom" => {
                                let factor = value
                                    .as_ref()
                                    .and_then(serde_json::Value::as_f64)
                                    .ok_or_else(|| {
                                        "browser_action zoom 需要数字 value".to_owned()
                                    })?;
                                runtime.set_zoom(factor)?;
                            }
                            "viewport" => {
                                // The pane's "Responsive" entry sends null, which clears the
                                // emulation; the source has no desktop preset.
                                let size = browser::parse_browser_viewport_action(
                                    value.as_ref(),
                                )?;
                                return runtime.set_emulated_viewport(size);
                            }
                            _ => return Err(format!("未知浏览器操作: {action}")),
                        }
                        Ok(runtime.status())
                    };
                    if takes_user_control {
                        runtime.with_user_control(execute)
                    } else {
                        execute()
                    }
                },
            )
            .map_err(browser_renderer_mount_error)?
    })
    .await
    .map_err(|error| format!("浏览器操作后台任务失败: {error}"))?
}

#[cfg(not(test))]
fn trusted_git_workspace_operation(
    app: &AppHandle,
    state: &AppState,
    target: &GitTarget,
    request_label: &str,
    access: TrustedWorkspaceAccess,
) -> Result<TrustedWorkspaceOperation, String> {
    trusted_target_workspace_operation(app, state, target, request_label, access, None)
}

/// Entry point for Git/GitHub writes. In addition to
/// `trusted_git_workspace_operation`, it rejects writes while a model run writes
/// the same checkout.
///
/// Evaluate the conflict before lease acquisition under the same storage lock as
/// resolution so the coordinator cannot mask the actual conflict.
#[cfg(not(test))]
fn trusted_git_write_operation(
    app: &AppHandle,
    state: &AppState,
    target: &GitTarget,
    request_label: &str,
    surface: &'static str,
) -> Result<TrustedWorkspaceOperation, String> {
    trusted_target_workspace_operation(
        app,
        state,
        target,
        request_label,
        TrustedWorkspaceAccess::Exclusive,
        Some(surface),
    )
}

/// Whether a Git write conflicts with a model run writing the same checkout.
///
/// Conversation addressing checks that conversation. Workspace addressing checks
/// all conversations running at the workspace root because it has no conversation.
#[cfg(not(test))]
fn reject_git_write_during_model_run(
    state: &AppState,
    target: &GitTarget,
    root_conversation_ids: &[String],
    surface: &str,
) -> Result<(), String> {
    let blocked = match target {
        GitTarget::Conversation { conversation_id } => {
            state.conversation_model_run_active(conversation_id)
        }
        GitTarget::Workspace { .. } => root_conversation_ids
            .iter()
            .any(|conversation_id| state.conversation_model_run_active(conversation_id)),
    };
    if blocked {
        return Err(format!(
            "模型或 Agent 正在运行，{surface} 写操作已锁定；请等本轮结束后重试"
        ));
    }
    Ok(())
}

#[cfg(not(test))]
#[tauri::command]
async fn get_git_workspace_snapshot(
    app: AppHandle,
    state: State<'_, AppState>,
    target: GitTarget,
) -> Result<Option<git::GitWorkspaceSnapshot>, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let operation = trusted_git_workspace_operation(
            &app,
            &state,
            &target,
            "Git 状态请求",
            TrustedWorkspaceAccess::Shared,
        )?;
        git::workspace_snapshot(&operation.workspace_path)
    })
    .await
    .map_err(|error| format!("读取 Git 状态的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn get_git_workspace_summary(
    app: AppHandle,
    state: State<'_, AppState>,
    target: GitTarget,
    known_revision: Option<String>,
) -> Result<git::GitWorkspaceSummaryResult, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let operation = trusted_git_workspace_operation(
            &app,
            &state,
            &target,
            "Git 摘要请求",
            TrustedWorkspaceAccess::Shared,
        )?;
        git::workspace_summary(&operation.workspace_path, known_revision)
    })
    .await
    .map_err(|error| format!("读取 Git 摘要的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn get_git_change_page(
    app: AppHandle,
    state: State<'_, AppState>,
    target: GitTarget,
    request: git::GitChangePageRequest,
) -> Result<git::GitChangePageResult, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let operation = trusted_git_workspace_operation(
            &app,
            &state,
            &target,
            "Git 变更页请求",
            TrustedWorkspaceAccess::Shared,
        )?;
        git::change_page(&operation.workspace_path, request)
    })
    .await
    .map_err(|error| format!("读取 Git 变更页的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn get_git_diff(
    app: AppHandle,
    state: State<'_, AppState>,
    target: GitTarget,
    request: git::GitDiffRequest,
) -> Result<git::GitDiffResponse, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let operation = trusted_git_workspace_operation(
            &app,
            &state,
            &target,
            "Git 差异请求",
            TrustedWorkspaceAccess::Shared,
        )?;
        git::diff(&operation.workspace_path, request)
    })
    .await
    .map_err(|error| format!("读取 Git 差异的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn list_workspace_directory(
    app: AppHandle,
    state: State<'_, AppState>,
    target: GitTarget,
    relative_path: String,
) -> Result<workspace_files::WorkspaceDirectoryListing, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let operation = trusted_target_workspace_operation(
            &app,
            &state,
            &target,
            "文件浏览",
            TrustedWorkspaceAccess::Shared,
            None,
        )?;
        workspace_files::list_directory(&operation.workspace_path, &relative_path)
    })
    .await
    .map_err(|error| format!("读取工作区文件的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn read_workspace_file(
    app: AppHandle,
    state: State<'_, AppState>,
    target: GitTarget,
    relative_path: String,
) -> Result<workspace_files::WorkspaceFileContent, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let operation = trusted_target_workspace_operation(
            &app,
            &state,
            &target,
            "文件浏览",
            TrustedWorkspaceAccess::Shared,
            None,
        )?;
        workspace_files::read_file(&operation.workspace_path, &relative_path)
    })
    .await
    .map_err(|error| format!("读取工作区文件的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn read_workspace_file_bytes(
    app: AppHandle,
    state: State<'_, AppState>,
    target: GitTarget,
    relative_path: String,
) -> Result<workspace_files::WorkspaceFileBytes, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let operation = trusted_target_workspace_operation(
            &app,
            &state,
            &target,
            "文件浏览",
            TrustedWorkspaceAccess::Shared,
            None,
        )?;
        workspace_files::read_file_bytes(&operation.workspace_path, &relative_path)
    })
    .await
    .map_err(|error| format!("读取工作区文件的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn search_workspace_files(
    app: AppHandle,
    state: State<'_, AppState>,
    target: GitTarget,
    query: String,
    limit: usize,
) -> Result<workspace_files::WorkspaceSearchResults, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let operation = trusted_target_workspace_operation(
            &app,
            &state,
            &target,
            "文件浏览",
            TrustedWorkspaceAccess::Shared,
            None,
        )?;
        workspace_files::search_files(&operation.workspace_path, &query, limit)
    })
    .await
    .map_err(|error| format!("读取工作区文件的后台任务失败: {error}"))?
}

/// A dev server owned by one chat, for the conflict messages that say so. A
/// workspace-addressed request has no chat, and the registry treats an ownerless
/// server as anybody's.
#[cfg(not(test))]
fn preview_session_id(target: &GitTarget) -> Option<String> {
    match target {
        GitTarget::Conversation { conversation_id } => Some(conversation_id.clone()),
        GitTarget::Workspace { .. } => None,
    }
}

/// Everything `.mework/launch.json` configures, and what is wrong with it.
///
/// The preview commands deliberately take no renderer-mount lease. That fence
/// exists to stop a native child WebView outliving the renderer document that
/// asked for it; a dev server is an OS process keyed by worktree that is supposed
/// to survive a renderer reload, and binding it to a mount generation would kill
/// it on every reload — the opposite of what the list is for. They do take the
/// workspace-trust guard, because launch.json is workspace data and spawning a
/// process out of it is privileged.
#[cfg(not(test))]
#[tauri::command]
async fn preview_list_configurations(
    app: AppHandle,
    state: State<'_, AppState>,
    target: GitTarget,
) -> Result<preview::PreviewConfigurationList, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let operation = trusted_target_workspace_operation(
            &app,
            &state,
            &target,
            "预览服务器配置",
            TrustedWorkspaceAccess::Shared,
            None,
        )?;
        Ok(preview::configurations(&operation.workspace_path))
    })
    .await
    .map_err(|error| format!("读取预览服务器配置的后台任务失败: {error}"))?
}

/// The dev servers running for this workspace, whichever chat started them.
#[cfg(not(test))]
#[tauri::command]
async fn preview_list_servers(
    app: AppHandle,
    state: State<'_, AppState>,
    target: GitTarget,
) -> Result<Vec<preview_servers::PreviewServerSnapshot>, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let operation = trusted_target_workspace_operation(
            &app,
            &state,
            &target,
            "预览服务器列表",
            TrustedWorkspaceAccess::Shared,
            None,
        )?;
        Ok(state
            .preview_servers
            .servers_for_worktree(&operation.workspace_path))
    })
    .await
    .map_err(|error| format!("读取预览服务器列表的后台任务失败: {error}"))?
}

/// Starts the configured server `name` addresses, or returns the one already
/// answering it.
///
/// Shared rather than exclusive workspace access: a dev server reads the checkout
/// and serves it, it does not write it, and demanding exclusivity would refuse
/// every start made while a model run holds the same workspace — which is exactly
/// when a preview is wanted.
#[cfg(not(test))]
#[tauri::command]
async fn preview_start_server(
    app: AppHandle,
    state: State<'_, AppState>,
    target: GitTarget,
    name: Option<String>,
) -> Result<preview::PreviewStartOutcome, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let operation = trusted_target_workspace_operation(
            &app,
            &state,
            &target,
            "启动预览服务器",
            TrustedWorkspaceAccess::Shared,
            None,
        )?;
        preview::start(
            &state.preview_servers,
            &operation.workspace_path,
            name.as_deref(),
            preview_session_id(&target).as_deref(),
        )
    })
    .await
    .map_err(|error| format!("启动预览服务器的后台任务失败: {error}"))?
}

/// Stops one dev server and forgets it, buffered output included.
///
/// Addressed by server id alone, like `stop_shell_task`. A stop must keep working
/// after the workspace record it was started from has been deleted, and resolving
/// a target first is exactly what would make an orphaned server unstoppable until
/// the application exits.
#[cfg(not(test))]
#[tauri::command]
async fn preview_stop_server(
    state: State<'_, AppState>,
    server_id: String,
) -> Result<bool, String> {
    let registry = state.preview_servers.clone();
    tauri::async_runtime::spawn_blocking(move || registry.stop(&server_id))
        .await
        .map_err(|error| format!("停止预览服务器的后台任务失败: {error}"))
}

/// One dev server's buffered output, filtered the way `preview_logs` filters it.
#[cfg(not(test))]
#[tauri::command]
fn preview_server_logs(
    state: State<'_, AppState>,
    server_id: String,
    errors_only: Option<bool>,
    search: Option<String>,
    lines: Option<u32>,
) -> String {
    preview::logs(
        &state.preview_servers,
        &server_id,
        &preview_servers::PreviewLogQuery {
            errors_only: errors_only.unwrap_or(false),
            search,
            lines,
        },
    )
}

/// Records whether this project wants its previews verified automatically.
///
/// The only preview command that writes, so it takes the workspace exclusively.
/// `false` for every reason the write did not happen — a project with no
/// launch.json has nowhere to keep the preference.
#[cfg(not(test))]
#[tauri::command]
async fn preview_set_auto_verify(
    app: AppHandle,
    state: State<'_, AppState>,
    target: GitTarget,
    enabled: bool,
) -> Result<bool, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let operation = trusted_target_workspace_operation(
            &app,
            &state,
            &target,
            "预览自动验证设置",
            TrustedWorkspaceAccess::Exclusive,
            None,
        )?;
        Ok(preview::set_auto_verify(&operation.workspace_path, enabled))
    })
    .await
    .map_err(|error| format!("写入预览自动验证设置的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn get_git_branches(
    app: AppHandle,
    state: State<'_, AppState>,
    target: GitTarget,
) -> Result<git::GitBranchesResult, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let operation = trusted_git_workspace_operation(
            &app,
            &state,
            &target,
            "Git 分支请求",
            TrustedWorkspaceAccess::Shared,
        )?;
        git::branches(&operation.workspace_path)
    })
    .await
    .map_err(|error| format!("读取 Git 分支的后台任务失败: {error}"))?
}

/// The root directory of a conversation workspace and its registered isolated
/// worktree. Unlike `TrustedWorkspaceOperation::workspace_path`, this excludes
/// the isolated worktree because create and release operate on the parent repo.
#[cfg(not(test))]
fn conversation_workspace_root(
    app: &AppHandle,
    state: &AppState,
    conversation_id: &str,
    request_label: &str,
) -> Result<(String, Option<ConversationWorktree>), String> {
    let document = state.document_store.read(&document_path(app)?)?;
    let (workspace, conversation) =
        trusted_workspace_and_conversation(&document, conversation_id, request_label)?;
    if workspace.kind != WorkspaceKind::Directory {
        return Err("只有目录工作区才能使用隔离工作树".into());
    }
    if workspace.path.trim().is_empty() {
        return Err(format!("工作区 {} 的路径为空", workspace.id));
    }
    Ok((workspace.path.clone(), conversation.worktree.clone()))
}

/// Creates an isolated worktree for a conversation and returns its record.
/// The caller persists the conversation pointer because document state and the
/// worktree's filesystem existence are separate facts.
#[cfg(not(test))]
#[tauri::command]
async fn create_conversation_worktree(
    app: AppHandle,
    state: State<'_, AppState>,
    conversation_id: String,
    from_branch: Option<String>,
) -> Result<ConversationWorktree, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        // Creating a worktree requires an exclusive lease within its workspace.
        //
        // Create and release require conversation addressing: an isolated
        // worktree belongs to a conversation and cannot exist without one.
        let _operation = trusted_workspace_operation(
            &app,
            &state,
            &conversation_id,
            "隔离工作树创建请求",
            TrustedWorkspaceAccess::Exclusive,
        )?;
        let (root, existing) =
            conversation_workspace_root(&app, &state, &conversation_id, "隔离工作树创建请求")?;
        if let Some(worktree) = existing {
            if Path::new(&worktree.path).is_dir() {
                // Reuse an existing worktree. Re-creation only fails because
                // its branch already exists and provides no useful signal.
                return Ok(worktree);
            }
        }
        let worktree = git::create_conversation_worktree(
            Path::new(&root),
            &conversation_id,
            from_branch
                .as_deref()
                .filter(|value| !value.trim().is_empty()),
        )?;
        Ok(ConversationWorktree {
            path: worktree.path.to_string_lossy().into_owned(),
            branch: worktree.branch,
            base_oid: worktree.base_oid,
        })
    })
    .await
    .map_err(|error| format!("创建隔离工作树的后台任务失败: {error}"))?
}

/// Releases a conversation's isolated worktree.
///
/// `true` means the worktree and branch were removed. `false` preserves uncommitted
/// changes or extra commits on disk; in both cases the caller clears the pointer.
#[cfg(not(test))]
#[tauri::command]
async fn release_conversation_worktree(
    app: AppHandle,
    state: State<'_, AppState>,
    conversation_id: String,
) -> Result<bool, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = trusted_workspace_operation(
            &app,
            &state,
            &conversation_id,
            "隔离工作树释放请求",
            TrustedWorkspaceAccess::Exclusive,
        )?;
        let (root, existing) =
            conversation_workspace_root(&app, &state, &conversation_id, "隔离工作树释放请求")?;
        // No registered worktree means there is nothing to release; this is not
        // an error.
        let Some(worktree) = existing else {
            return Ok(true);
        };
        git::release_conversation_worktree(Path::new(&root), &worktree)
    })
    .await
    .map_err(|error| format!("释放隔离工作树的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn get_git_history(
    app: AppHandle,
    state: State<'_, AppState>,
    target: GitTarget,
    request: git::GitHistoryRequest,
) -> Result<git::GitHistoryPage, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let operation = trusted_git_workspace_operation(
            &app,
            &state,
            &target,
            "Git 历史请求",
            TrustedWorkspaceAccess::Shared,
        )?;
        git::history(&operation.workspace_path, request)
    })
    .await
    .map_err(|error| format!("读取 Git 历史的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn prepare_git_discard(
    app: AppHandle,
    state: State<'_, AppState>,
    target: GitTarget,
    paths: Vec<String>,
    include_untracked: bool,
) -> Result<git::GitDiscardPreparation, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let operation = trusted_git_workspace_operation(
            &app,
            &state,
            &target,
            "Git 丢弃确认请求",
            TrustedWorkspaceAccess::Shared,
        )?;
        git::prepare_discard(&operation.workspace_path, &paths, include_untracked)
    })
    .await
    .map_err(|error| format!("准备 Git 丢弃确认的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn prepare_git_stage_all(
    app: AppHandle,
    state: State<'_, AppState>,
    target: GitTarget,
) -> Result<git::GitStageAllPreparation, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let operation = trusted_git_workspace_operation(
            &app,
            &state,
            &target,
            "Git 全部暂存确认请求",
            TrustedWorkspaceAccess::Shared,
        )?;
        git::prepare_stage_all(&operation.workspace_path)
    })
    .await
    .map_err(|error| format!("准备 Git 全部暂存确认的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn prepare_git_commit(
    app: AppHandle,
    state: State<'_, AppState>,
    target: GitTarget,
    message: String,
) -> Result<git::GitCommitPreparation, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let operation = trusted_git_workspace_operation(
            &app,
            &state,
            &target,
            "Git 提交确认请求",
            TrustedWorkspaceAccess::Shared,
        )?;
        git::prepare_commit(&operation.workspace_path, &message)
    })
    .await
    .map_err(|error| format!("准备 Git 提交确认的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn execute_git_action(
    app: AppHandle,
    state: State<'_, AppState>,
    target: GitTarget,
    action: git::GitAction,
) -> Result<git::GitActionResult, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        // Resolve trust and acquire the workspace writer while holding the
        // storage lock. A model/tool start racing this action therefore loses
        // atomically at the same coordinator rather than slipping through a
        // check/start gap.
        let operation = trusted_git_write_operation(&app, &state, &target, "Git 操作请求", "Git")?;
        git::execute_action_with_policy(
            &operation.workspace_path,
            action,
            operation.git_network_policy,
        )
    })
    .await
    .map_err(|error| format!("执行 Git 操作的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn get_github_repository(
    app: AppHandle,
    state: State<'_, AppState>,
    target: GitTarget,
) -> Result<Option<git::GithubRepository>, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let operation = trusted_git_workspace_operation(
            &app,
            &state,
            &target,
            "GitHub 仓库请求",
            TrustedWorkspaceAccess::Shared,
        )?;
        git::github_repository(&operation.workspace_path)
    })
    .await
    .map_err(|error| format!("读取 GitHub 仓库的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn get_github_pull_requests(
    app: AppHandle,
    state: State<'_, AppState>,
    target: GitTarget,
    page: u32,
    page_size: u16,
) -> Result<git::GithubPullRequestList, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let operation = trusted_git_workspace_operation(
            &app,
            &state,
            &target,
            "GitHub PR 列表请求",
            TrustedWorkspaceAccess::Shared,
        )?;
        git::github_pull_requests(&operation.workspace_path, page, page_size)
    })
    .await
    .map_err(|error| format!("读取 GitHub PR 列表的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn get_github_pull_request_detail(
    app: AppHandle,
    state: State<'_, AppState>,
    target: GitTarget,
    number: u64,
) -> Result<git::GithubPullRequestDetail, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let operation = trusted_git_workspace_operation(
            &app,
            &state,
            &target,
            "GitHub PR 详情请求",
            TrustedWorkspaceAccess::Shared,
        )?;
        git::github_pull_request_detail(&operation.workspace_path, number)
    })
    .await
    .map_err(|error| format!("读取 GitHub PR 详情的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn get_github_pull_request_readiness(
    app: AppHandle,
    state: State<'_, AppState>,
    target: GitTarget,
    number: u64,
) -> Result<git::GithubPullRequestReadiness, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let operation = trusted_git_workspace_operation(
            &app,
            &state,
            &target,
            "GitHub PR readiness 请求",
            TrustedWorkspaceAccess::Shared,
        )?;
        git::github_pull_request_readiness(&operation.workspace_path, number)
    })
    .await
    .map_err(|error| format!("读取 GitHub PR readiness 的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn get_github_pull_request_diff(
    app: AppHandle,
    state: State<'_, AppState>,
    target: GitTarget,
    number: u64,
    path: Option<String>,
) -> Result<git::GithubPullRequestDiff, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let operation = trusted_git_workspace_operation(
            &app,
            &state,
            &target,
            "GitHub PR 差异请求",
            TrustedWorkspaceAccess::Shared,
        )?;
        git::github_pull_request_diff(&operation.workspace_path, number, path)
    })
    .await
    .map_err(|error| format!("读取 GitHub PR 差异的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn get_github_pull_request_review_threads(
    app: AppHandle,
    state: State<'_, AppState>,
    target: GitTarget,
    request: git::GithubReviewThreadsRequest,
) -> Result<git::GithubReviewThreadsResult, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let operation = trusted_git_workspace_operation(
            &app,
            &state,
            &target,
            "GitHub PR 审阅线程请求",
            TrustedWorkspaceAccess::Shared,
        )?;
        git::github_pull_request_review_threads(&operation.workspace_path, request)
    })
    .await
    .map_err(|error| format!("读取 GitHub PR 审阅线程的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn get_github_pull_request_review_thread_comments(
    app: AppHandle,
    state: State<'_, AppState>,
    target: GitTarget,
    request: git::GithubReviewThreadCommentsRequest,
) -> Result<git::GithubReviewThreadCommentsResult, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let operation = trusted_git_workspace_operation(
            &app,
            &state,
            &target,
            "GitHub PR 审阅线程评论请求",
            TrustedWorkspaceAccess::Shared,
        )?;
        git::github_pull_request_review_thread_comments(&operation.workspace_path, request)
    })
    .await
    .map_err(|error| format!("读取 GitHub PR 审阅线程评论的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
async fn execute_github_action(
    app: AppHandle,
    state: State<'_, AppState>,
    target: GitTarget,
    action: git::GithubAction,
) -> Result<git::GithubActionResult, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let operation =
            trusted_git_write_operation(&app, &state, &target, "GitHub 操作请求", "GitHub")?;
        git::execute_github_action(&operation.workspace_path, action)
    })
    .await
    .map_err(|error| format!("执行 GitHub 操作的后台任务失败: {error}"))?
}

#[cfg(not(test))]
#[tauri::command]
fn open_terminal(
    app: AppHandle,
    state: State<'_, AppState>,
    conversation_id: String,
    terminal_id: String,
    cols: u16,
    rows: u16,
    on_event: Channel<terminal::TerminalEvent>,
) -> Result<terminal::TerminalOpenResponse, String> {
    // A conversation whose workspace is on another machine gets a shell on that
    // machine. There is no host checkout under it, so no workspace lease is
    // taken and the command lease is a no-op: the mutex those guard exists for
    // Git operations on this filesystem.
    if let Some(launch) = remote_terminal_launch(&app, state.inner(), &conversation_id)? {
        return state.terminals.open(
            &conversation_id,
            &terminal_id,
            launch,
            cols,
            rows,
            on_event,
            Box::new(()),
            Arc::new(|| Ok(Box::new(()) as terminal::TerminalCommandLease)),
        );
    }
    let operation = trusted_workspace_operation(
        &app,
        state.inner(),
        &conversation_id,
        "终端请求",
        TrustedWorkspaceAccess::Shared,
    )?;
    let launch = terminal::TerminalLaunch::host(&operation.workspace_path)?;
    let workspace_key = operation.workspace_key;
    let startup_lease = operation.lease;
    let operation_gate = state.operation_gate();
    let lease_conversation_id = conversation_id.clone();
    let command_lease_factory: terminal::TerminalCommandLeaseFactory = Arc::new(move || {
        operation_gate
            .begin_workspace_operation(workspace_key.clone(), Some(lease_conversation_id.clone()))
            .map(|operation| Box::new(operation) as terminal::TerminalCommandLease)
    });
    state.terminals.open(
        &conversation_id,
        &terminal_id,
        launch,
        cols,
        rows,
        on_event,
        startup_lease,
        command_lease_factory,
    )
}

/// The terminal launch for a conversation whose workspace is on another
/// machine, or `None` when the workspace is on this one.
#[cfg(not(test))]
fn remote_terminal_launch(
    app: &AppHandle,
    state: &AppState,
    conversation_id: &str,
) -> Result<Option<terminal::TerminalLaunch>, String> {
    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let document = state.document_store.read(&document_path(app)?)?;
    let (workspace, conversation) =
        trusted_workspace_and_conversation(&document, conversation_id, "终端请求")?;
    let Some(machine) = workspace
        .machine
        .as_ref()
        .filter(|_| workspace.kind == WorkspaceKind::Directory)
    else {
        return Ok(None);
    };
    let runner = crate::run_environment::resolve_shell_runner(
        &document.assets.execution_environments,
        Some(machine),
    )?;
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法解析应用数据目录: {error}"))?;
    let anchor = effective_workspace_path(&app_data, workspace, conversation)?;
    terminal::TerminalLaunch::remote(&runner, &workspace.path, Path::new(&anchor)).map(Some)
}

#[cfg(not(test))]
#[tauri::command]
fn write_terminal(
    state: State<'_, AppState>,
    conversation_id: String,
    terminal_id: String,
    session_id: String,
    data: String,
) -> Result<(), String> {
    state
        .terminals
        .write(&conversation_id, &terminal_id, &session_id, &data)
}

#[cfg(not(test))]
#[tauri::command]
fn resize_terminal(
    state: State<'_, AppState>,
    conversation_id: String,
    terminal_id: String,
    session_id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    state
        .terminals
        .resize(&conversation_id, &terminal_id, &session_id, cols, rows)
}

#[cfg(not(test))]
#[tauri::command]
fn detach_terminal(
    state: State<'_, AppState>,
    conversation_id: String,
    terminal_id: String,
    session_id: String,
) -> bool {
    state
        .terminals
        .detach(&conversation_id, &terminal_id, &session_id)
}

#[cfg(not(test))]
#[tauri::command]
fn close_terminal(
    state: State<'_, AppState>,
    conversation_id: String,
    terminal_id: String,
) -> bool {
    state.terminals.close(&conversation_id, &terminal_id)
}

/// Asks one running `bash` / `powershell` tool call to stop. Returns false when
/// the command already finished — the row is about to disappear on its own — or
/// when it belongs to another conversation.
#[cfg(not(test))]
#[tauri::command]
fn stop_shell_task(
    state: State<'_, AppState>,
    conversation_id: String,
    shell_task_id: String,
) -> bool {
    state
        .shell_tasks
        .request_stop(&conversation_id, &shell_task_id)
}

/// Every shell command this conversation is running right now. Push events carry
/// the live changes; this is how the renderer recovers the set it missed while it
/// had no subscription — a browser-dev reconnect, or a document reload mid-run.
#[cfg(not(test))]
#[tauri::command]
fn list_shell_tasks(
    state: State<'_, AppState>,
    conversation_id: String,
) -> Vec<shell_tasks::ShellTaskSnapshot> {
    state.shell_tasks.task_snapshots(&conversation_id)
}

/// Starts watching one shell command's live output.
///
/// Returns everything retained so far and installs the channel the rest arrives on, both inside
/// one registry critical section — reading the buffer and subscribing as two calls would drop
/// whatever the command printed in between. A command that has already finished answers with its
/// buffer and `live: false`.
#[cfg(not(test))]
#[tauri::command]
fn open_shell_task_output(
    state: State<'_, AppState>,
    conversation_id: String,
    shell_task_id: String,
    on_event: Channel<shell_tasks::ShellOutputEvent>,
) -> Result<shell_tasks::ShellTaskOutputHandle, String> {
    state
        .shell_tasks
        .subscribe_output(&conversation_id, &shell_task_id, on_event)
        .ok_or_else(|| "该命令已不在任务列表中".to_owned())
}

/// Stops watching. False means the command was already gone, which is not an error: the page that
/// asked has simply outlived its task.
#[cfg(not(test))]
#[tauri::command]
fn detach_shell_task_output(
    state: State<'_, AppState>,
    conversation_id: String,
    shell_task_id: String,
    subscription_id: Option<u64>,
) -> bool {
    state
        .shell_tasks
        .detach_output(&conversation_id, &shell_task_id, subscription_id)
}

#[cfg(not(test))]
async fn attach_trusted_project_memory(
    app: &AppHandle,
    state: AppState,
    request: RunModelRequest,
) -> Result<RunModelRequest, String> {
    if request.subagent_depth > 0 || !Path::new(&request.workspace_path).is_dir() {
        return Ok(request);
    }
    let dialog_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let workspace_kind = revalidate_project_memory_workspace(&dialog_app, &state, &request)?;
        let _trust_guard = state
            .project_import_trust_lock
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut options = project_memory::ProjectMemoryOptions::new(&request.workspace_path);
        memory_locations::MemoryLocationRoots::for_host()
            .apply_to_project_memory_options(&mut options);
        if workspace_kind != WorkspaceKind::Directory {
            options.ancestor_floor = Some(PathBuf::from(&request.workspace_path));
        }
        let store = project_import_trust::EncryptedProjectImportTrustStore::open(Path::new(
            &request.app_data_path,
        ));
        let resolution = project_import_trust::resolve_project_memory_imports(
            &store,
            &request.workspace_id,
            &options,
            |candidates| confirm_project_memory_imports(&dialog_app, candidates),
        )?;
        let mut request = request;
        if let Some(context_id) = request.project_memory_context_id.take() {
            request
                .ephemeral_contexts
                .retain(|context| context.id() != context_id);
        }
        let sources = project_memory::startup_sources(&resolution.report);
        let prompt =
            project_memory::render_project_memory_prompt(&sources, &request.prompt_profile);
        request.project_memory_context_id =
            push_ephemeral_user_context(&mut request, "project_memory", prompt);
        if !resolution.report.diagnostics.is_empty() {
            eprintln!(
                "{}",
                project_memory::summarize_project_memory_diagnostics(&resolution.report)
            );
        }
        Ok(request)
    })
    .await
    .map_err(|error| format!("项目记忆信任检查后台任务失败: {error}"))?
}

#[cfg(not(test))]
fn revalidate_project_memory_workspace(
    app: &AppHandle,
    state: &AppState,
    request: &RunModelRequest,
) -> Result<WorkspaceKind, String> {
    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let document = state.document_store.read(&document_path(app)?)?;
    let (workspace, conversation) =
        trusted_workspace_and_conversation(&document, &request.conversation_id, "项目记忆请求")?;
    if workspace.id != request.workspace_id {
        return Err("项目记忆工作区身份在运行前已变化；请重试".into());
    }
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法解析应用数据目录: {error}"))?;
    let current = effective_workspace_path(&app_data, workspace, conversation)?;
    let current = std::fs::canonicalize(current)
        .map_err(|_| "项目记忆工作区在运行前已不可访问；请重试".to_owned())?;
    let requested = std::fs::canonicalize(&request.workspace_path)
        .map_err(|_| "项目记忆工作区在运行前已不可访问；请重试".to_owned())?;
    let same = if cfg!(windows) {
        current
            .to_string_lossy()
            .eq_ignore_ascii_case(&requested.to_string_lossy())
    } else {
        current == requested
    };
    if !same {
        return Err("项目记忆工作区路径在运行前已变化；请重试".into());
    }
    Ok(workspace.kind)
}

#[cfg(not(test))]
fn confirm_project_memory_imports(
    app: &AppHandle,
    candidates: &[project_import_trust::ProjectImportCandidate],
) -> Result<bool, String> {
    if candidates.is_empty() {
        return Ok(false);
    }
    let mut details = String::new();
    for candidate in candidates {
        let kind = match candidate.target_kind {
            project_import_trust::ProjectImportTargetKind::File => "文件",
            project_import_trust::ProjectImportTargetKind::Directory => {
                "目录（授权将覆盖该目录的全部后代）"
            }
        };
        details.push_str(&format!("\n- {kind}：{}", candidate.approval_display));
    }
    let prompt = format!(
        "此项目的 MEWORK.md 请求读取工作区之外的项目记忆。\n\
这些标识已经相对于工作区或卷脱敏，仅显示在本机原生确认框中，不会发送给模型、\
renderer 或普通日志。允许后，文件内容会作为不受信任的用户上下文发送给当前选择的\
模型提供商；授权只绑定当前工作区 ID、当前 canonical 工作区根和下列 canonical \
目标，并会在每次读取前重新校验。\n\n目标：{details}\n\n\
选择“始终允许”会持久授权这些目标；选择“保持禁用”会持久拒绝，后续运行不会反复询问。\
两种决定都可在记忆设置中撤销。"
    );
    if prompt.chars().count() > 12_000 {
        return Err("外部项目记忆目标说明超过原生确认框安全长度；请减少导入后重试".into());
    }
    Ok(app
        .dialog()
        .message(prompt)
        .title("确认外部项目记忆")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "始终允许此工作区".into(),
            "保持禁用".into(),
        ))
        .blocking_show())
}

#[cfg(not(test))]
async fn confirm_run_hooks(app: &AppHandle, request: &RunModelRequest) -> Result<(), String> {
    if request.effective_security_level() == SecurityLevel::FullAccess {
        return Ok(());
    }
    let hooks = request
        .active_hooks
        .iter()
        .filter(|hook| hook.enabled)
        .collect::<Vec<_>>();
    if hooks.is_empty() {
        return Ok(());
    }
    let mut details = String::new();
    for hook in hooks {
        details.push_str(&format!(
            "\n[{}] {}{}\n{}{}\n",
            hooks::event_label(hook.event),
            hook.name,
            hook.matcher
                .as_deref()
                .map(|matcher| format!(" · matcher={matcher}"))
                .unwrap_or_default(),
            hook.command,
            hook.command_windows
                .as_deref()
                .map(|command| format!("\nWindows: {command}"))
                .unwrap_or_default()
        ));
    }
    if details.chars().count() > 12_000 {
        return Err("钩子命令总长度超过原生确认框的 12000 字符限制，请拆分预设".into());
    }
    let prompt = format!(
        "本轮将按生命周期在以下工作区执行钩子命令。模型不能修改这些命令；请核对后仅批准本轮。执行诊断只保存在本地；只有钩子明确返回 additionalContext（或 SessionStart/UserPromptSubmit 成功输出纯文本）时，该部分才会作为系统上下文发送给模型。\n\n工作区：{}\n{}",
        request.workspace_path, details
    );
    let dialog_app = app.clone();
    let confirmed = tauri::async_runtime::spawn_blocking(move || {
        dialog_app
            .dialog()
            .message(prompt)
            .title("确认本轮生命周期钩子")
            .kind(MessageDialogKind::Warning)
            .buttons(MessageDialogButtons::OkCancelCustom(
                "允许本轮".into(),
                "取消".into(),
            ))
            .blocking_show()
    })
    .await
    .map_err(|error| format!("打开钩子确认框失败: {error}"))?;
    if confirmed {
        Ok(())
    } else {
        Err("用户取消了本轮生命周期钩子".into())
    }
}

/// Composes the host-owned environment, runtime capability and application data
/// directory sections, without an empty leading separator.
///
/// The environment block leads: where the run happens frames everything the
/// capability sections go on to describe.
fn assemble_system_prompt(
    profile: &prompt_profile::PromptProfile,
    environment: &str,
    runtime_addendum: &str,
    app_data_path: Option<&str>,
) -> String {
    let mut prompt = String::new();
    append_system_prompt_section(&mut prompt, environment);
    if !runtime_addendum.is_empty() {
        append_system_prompt_section(&mut prompt, runtime_addendum);
    }
    if let Some(path) = app_data_path {
        append_system_prompt_section(
            &mut prompt,
            &profile.render(
                prompt_profile::PromptKey::SystemAppDataDir,
                &[("path", path)],
            ),
        );
    }
    prompt
}

fn append_system_prompt_section(prompt: &mut String, section: &str) {
    if section.trim().is_empty() {
        return;
    }
    if !prompt.trim().is_empty() {
        prompt.push_str("\n\n---\n\n");
    }
    prompt.push_str(section);
}

#[cfg(not(test))]
fn push_ephemeral_user_context(
    request: &mut RunModelRequest,
    label: &str,
    content: String,
) -> Option<String> {
    if content.trim().is_empty() {
        return None;
    }
    let id = format!("ctx_ephemeral_{label}_{}", uuid::Uuid::new_v4().simple());
    request.ephemeral_contexts.push(ContextItem::User {
        id: id.clone(),
        content,
        images: Vec::new(),
        created_at: chrono::Utc::now().to_rfc3339(),
    });
    Some(id)
}

/// Applies the profile's per-tool usage guidance to produce the model-visible
/// catalog. Labels follow the profile's language.
///
/// Only `usage_guidance` is applied here, because only it belongs to this layer.
/// A tool's own description — what it is and where its boundaries lie — is the
/// root of its JSON Schema, and `schema_notes` overrides that through the
/// registry: [`prompt_profile::PromptProfile::from_file`] folds it onto the
/// tool's description key, so it *replaces* the built-in wording instead of
/// arriving beside it. Schemas otherwise, permissions, and execution remain
/// unchanged.
///
/// MCP tools are not here. They are discovered per run and appended later, so
/// their override is applied in `api::attach_mcp_tools`.
fn tool_catalog_with_descriptions(
    profile: &prompt_profile::PromptProfile,
) -> Vec<model::ToolDescriptor> {
    let mut tools = catalog::tool_catalog_for_language(profile.language);
    let entries = &profile.tools;
    if entries.is_empty() {
        return tools;
    }
    for tool in &mut tools {
        let Some(entry) = entries
            .iter()
            .find(|entry| entry.tool_name.trim() == tool.name)
        else {
            continue;
        };
        if !entry.usage_guidance.trim().is_empty() {
            tool.description = entry.usage_guidance.clone();
        }
    }
    tools
}

#[cfg(test)]
mod prompt_tests {
    use super::{assemble_system_prompt, tool_catalog_with_descriptions};
    use crate::model::{ContextItem, ToolDescriptionEntry, ToolResult};
    use crate::prompt_profile::PromptProfile;
    use serde_json::{json, Map};

    fn profile_with(
        entries: Vec<ToolDescriptionEntry>,
        language: crate::model::ResolvedLanguage,
    ) -> PromptProfile {
        PromptProfile::from_file(
            "test".into(),
            "Test".into(),
            language,
            Default::default(),
            entries,
        )
    }

    #[test]
    fn system_prompt_joins_environment_capabilities_and_app_data_in_order() {
        let profile = PromptProfile::builtin_english();
        assert_eq!(
            assemble_system_prompt(
                &profile,
                "# Environment",
                "SKILL\n---\nMCP\n---\nHOOK",
                Some("C:/app-data")
            ),
            "# Environment\n\n---\n\nSKILL\n---\nMCP\n---\nHOOK\n\n---\n\nApplication data directory: C:/app-data"
        );
        assert_eq!(
            assemble_system_prompt(&profile, "", "SKILL", None),
            "SKILL"
        );
        assert_eq!(
            assemble_system_prompt(&profile, "", "", Some("C:/app-data")),
            "Application data directory: C:/app-data"
        );
        assert_eq!(
            assemble_system_prompt(&profile, "# Environment", "", None),
            "# Environment"
        );
    }

    #[test]
    fn empty_host_sections_contribute_nothing() {
        for profile in [
            PromptProfile::builtin_english(),
            PromptProfile::builtin_chinese(),
        ] {
            assert_eq!(assemble_system_prompt(&profile, "", "", None), "");
            assert_eq!(assemble_system_prompt(&profile, " ", "  \n ", None), "");
            assert_eq!(
                assemble_system_prompt(&profile, "", "ADDENDUM", None),
                "ADDENDUM"
            );
        }
        assert_eq!(
            assemble_system_prompt(
                &PromptProfile::builtin_chinese(),
                "",
                "",
                Some("C:/app-data")
            ),
            "应用数据目录：C:/app-data"
        );
    }

    fn state_tool(
        id: &str,
        tool_name: &str,
        input: serde_json::Value,
        success: bool,
    ) -> ContextItem {
        state_tool_with_output(id, tool_name, input, "ok".into(), success)
    }

    fn state_tool_with_output(
        id: &str,
        tool_name: &str,
        input: serde_json::Value,
        output: String,
        success: bool,
    ) -> ContextItem {
        ContextItem::Tool {
            id: id.into(),
            tool_name: tool_name.into(),
            round: Some(1),
            model_turn_id: Some(format!("state-turn-{id}")),
            provider_call_id: None,
            requested_input: None,
            input: input.as_object().cloned().unwrap_or_else(Map::new),
            result: ToolResult {
                success,
                output,
                images: Vec::new(),
                diff: None,
                executed_at: "2026-07-15T00:00:00Z".into(),
                duration_ms: 1,
            },
            subagent: None,
            attestation: String::new(),
            created_at: "2026-07-15T00:00:00Z".into(),
        }
    }

    fn task_create(id: &str, task_id: &str) -> ContextItem {
        state_tool_with_output(
            id,
            "TaskCreate",
            json!({"subject":format!("Task {task_id}"),"description":"details"}),
            json!({"task":{"id":task_id,"subject":format!("Task {task_id}")}}).to_string(),
            true,
        )
    }

    fn task_update(id: &str, task_id: &str, status: Option<&str>) -> ContextItem {
        state_tool_with_output(
            id,
            "TaskUpdate",
            status.map_or_else(
                || json!({"taskId":task_id,"subject":"Renamed task"}),
                |status| json!({"taskId":task_id,"status":status}),
            ),
            json!({
                "success":true,
                "taskId":task_id,
                "updatedFields":if status.is_some() { json!(["status"]) } else { json!(["subject"]) }
            })
            .to_string(),
            true,
        )
    }

    /// The model-visible description of `name` under `profile`: the root of the
    /// schema it would actually receive.
    fn wire_description(profile: &PromptProfile, name: &str) -> String {
        let tools = tool_catalog_with_descriptions(profile);
        let descriptor = tools.iter().find(|tool| tool.name == name).unwrap();
        crate::aisdk::tools::tool_schema(descriptor, profile, &Default::default())["description"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    /// `schema_notes` names the slot that holds "what this tool is", which is the
    /// root of its schema. Overriding it has to *replace* the built-in wording —
    /// arriving beside it would leave the model reading both.
    #[test]
    fn trusted_tool_descriptions_replace_the_model_visible_description() {
        let canonical = crate::catalog::tool_catalog()
            .into_iter()
            .find(|tool| tool.name == "read")
            .unwrap();
        let entries = [ToolDescriptionEntry {
            tool_name: " read ".into(),
            schema_notes: "CUSTOM MODEL DESCRIPTION".into(),
            usage_guidance: String::new(),
        }];
        let profile = profile_with(entries.to_vec(), crate::model::ResolvedLanguage::ZhCn);
        let tools = tool_catalog_with_descriptions(&profile);
        let overridden = tools.iter().find(|tool| tool.name == "read").unwrap();

        assert_eq!(
            wire_description(&profile, "read"),
            "CUSTOM MODEL DESCRIPTION"
        );
        // The built-in wording is gone, not merely preceded by the override.
        assert!(!wire_description(&profile, "read")
            .contains(&wire_description(&PromptProfile::builtin_english(), "read")));
        // The guidance column stays empty: this entry set none.
        assert_eq!(overridden.description, "");
        assert_eq!(overridden.label, canonical.label);
        assert_eq!(overridden.category, canonical.category);
        assert_eq!(overridden.dangerous, canonical.dangerous);
        assert_eq!(overridden.parameters, canonical.parameters);
        // Only the named tool moves.
        assert_eq!(
            wire_description(&profile, "write"),
            wire_description(&PromptProfile::builtin_chinese(), "write")
        );
    }

    #[test]
    fn tool_description_layers_replace_and_append_independently() {
        let entries = [
            ToolDescriptionEntry {
                tool_name: "read".into(),
                schema_notes: String::new(),
                usage_guidance: "USAGE ONLY".into(),
            },
            ToolDescriptionEntry {
                tool_name: "write".into(),
                schema_notes: "SCHEMA ONLY".into(),
                usage_guidance: "AND USAGE".into(),
            },
        ];
        let profile = profile_with(entries.to_vec(), crate::model::ResolvedLanguage::ZhCn);
        let tools = tool_catalog_with_descriptions(&profile);
        let description_of = |name: &str| {
            tools
                .iter()
                .find(|tool| tool.name == name)
                .unwrap()
                .description
                .clone()
        };

        // Guidance alone fills the recommendation layer and leaves the schema's
        // account of what the tool is exactly where it was.
        assert_eq!(description_of("read"), "USAGE ONLY");
        assert_eq!(
            wire_description(&profile, "read"),
            wire_description(&PromptProfile::builtin_chinese(), "read")
        );
        // The two halves land on their own carriers rather than being concatenated
        // into one: notes are what the tool is, guidance is how to use it.
        assert_eq!(wire_description(&profile, "write"), "SCHEMA ONLY");
        assert_eq!(description_of("write"), "AND USAGE");
        // A profile with no entries at all leaves both layers alone.
        let untouched = tool_catalog_with_descriptions(&PromptProfile::builtin_chinese());
        assert_eq!(
            untouched
                .iter()
                .find(|tool| tool.name == "read")
                .unwrap()
                .description,
            ""
        );
    }

    /// The defect this whole slot exists for: selecting a different profile has
    /// to change what the model is told the tools are. Both built-ins ship with
    /// no `tools[]` entries, so if the description did not come from the registry
    /// the two would be byte-identical and switching would do nothing.
    #[test]
    fn switching_between_the_builtin_profiles_changes_every_tool_description() {
        let english = PromptProfile::builtin_english();
        let chinese = PromptProfile::builtin_chinese();
        assert!(english.tools.is_empty() && chinese.tools.is_empty());
        for tool in crate::catalog::tool_catalog() {
            let en = wire_description(&english, &tool.name);
            let zh = wire_description(&chinese, &tool.name);
            assert!(!en.is_empty(), "{} has no English description", tool.name);
            assert_ne!(en, zh, "{} reads the same under both built-ins", tool.name);
            assert!(
                zh.chars()
                    .any(|character| ('\u{4E00}'..='\u{9FFF}').contains(&character)),
                "{} is not translated in the built-in Chinese profile",
                tool.name
            );
        }
    }

    #[test]
    fn trusted_tool_catalog_uses_the_resolved_description_language_before_overrides() {
        let english = tool_catalog_with_descriptions(&PromptProfile::builtin_english());
        let read = english.iter().find(|tool| tool.name == "read").unwrap();
        // Usage-guidance seeds remain empty in every language; locale changes
        // affect only UI labels.
        assert_eq!(read.description, "");
        assert_eq!(read.label, "Read file");

        let entries = [ToolDescriptionEntry {
            tool_name: "read".into(),
            schema_notes: "CUSTOM ENGLISH DEFAULT".into(),
            usage_guidance: String::new(),
        }];
        let profile = profile_with(entries.to_vec(), crate::model::ResolvedLanguage::EnUs);
        assert_eq!(wire_description(&profile, "read"), "CUSTOM ENGLISH DEFAULT");
        // A file overriding one tool leaves the rest on its language's built-in.
        assert_eq!(
            wire_description(&profile, "write"),
            wire_description(&PromptProfile::builtin_english(), "write")
        );
    }
}

/// Returns conversation bodies for runs and forks. The conversation database is
/// authoritative; the in-memory document snapshot is a fallback.
///
/// Commands write SQLite then refresh the snapshot, but streaming writes occur
/// directly through `api::ConversationSink` and cannot refresh it per increment.
/// Wake runs therefore must read the database to include their preceding task
/// dispatches. Settings remain in the snapshot because only commands modify them.
fn authoritative_contexts(
    stored: Option<crate::model::Conversation>,
    snapshot: &crate::model::Conversation,
) -> Vec<crate::model::ContextItem> {
    match stored {
        // Prefer the database because it is authoritative, not because it is
        // longer; choosing the longer body would restore deleted contexts.
        Some(stored) if stored.id == snapshot.id => stored.contexts,
        _ => snapshot.contexts.clone(),
    }
}

/// Falls back to the in-memory snapshot when the database read fails: a stale
/// body is preferable to making the entire run fail.
#[cfg(not(test))]
fn stored_conversation(path: &Path, conversation_id: &str) -> Option<Conversation> {
    match conversations::load(path, conversation_id) {
        Ok(found) => found,
        Err(error) => {
            eprintln!("对话 {conversation_id} 的权威正文读取失败，本次改用内存快照：{error}");
            None
        }
    }
}

#[cfg(test)]
mod authoritative_contexts_tests {
    use super::authoritative_contexts;
    use crate::model::{ContextItem, Conversation};

    fn conversation(id: &str, contexts: Vec<ContextItem>) -> Conversation {
        Conversation {
            id: id.into(),
            title: "t".into(),
            created_at: "2026-08-26T00:00:00.000Z".into(),
            updated_at: "2026-08-26T00:00:00.000Z".into(),
            settings: serde_json::from_value(serde_json::json!({
                "enabledTools": [],
            }))
            .expect("settings"),
            contexts,
            queued_messages: Vec::new(),
            branches: Vec::new(),
            user_aborted_tasks: Vec::new(),
            worktree: None,
            run_target: None,
            additional_directories: Vec::new(),
            parent_conversation_id: None,
            preset_id: String::new(),
            template_id: String::new(),
            attached_workspaces: Vec::new(),
        }
    }

    fn user(id: &str) -> ContextItem {
        ContextItem::User {
            id: id.into(),
            content: "hi".into(),
            images: Vec::new(),
            created_at: "2026-08-26T00:00:01.000Z".into(),
        }
    }

    fn ids(contexts: &[ContextItem]) -> Vec<String> {
        contexts
            .iter()
            .map(|context| context.id().to_owned())
            .collect()
    }

    /// Ensures the database's extra turn reaches a wake run's model request.
    #[test]
    fn the_store_wins_when_the_snapshot_is_a_round_behind() {
        let stored = conversation("c1", vec![user("a"), user("b"), user("c")]);
        let snapshot = conversation("c1", vec![user("a")]);
        assert_eq!(
            ids(&authoritative_contexts(Some(stored), &snapshot)),
            ["a", "b", "c"]
        );
    }

    /// Database authority, rather than body length, preserves deletions and
    /// full replacements.
    #[test]
    fn the_store_wins_even_when_it_is_shorter() {
        let stored = conversation("c1", vec![user("a")]);
        let snapshot = conversation("c1", vec![user("a"), user("b")]);
        assert_eq!(ids(&authoritative_contexts(Some(stored), &snapshot)), ["a"]);
    }

    #[test]
    fn a_missing_row_falls_back_to_the_snapshot() {
        let snapshot = conversation("c1", vec![user("a"), user("b")]);
        assert_eq!(ids(&authoritative_contexts(None, &snapshot)), ["a", "b"]);
    }

    /// A database result for another conversation must not replace the snapshot.
    #[test]
    fn a_row_for_another_conversation_never_substitutes() {
        let stored = conversation("other", vec![user("x")]);
        let snapshot = conversation("c1", vec![user("a")]);
        assert_eq!(ids(&authoritative_contexts(Some(stored), &snapshot)), ["a"]);
    }
}

/// Renders a resolved workspace set as the `# Environment` block states it.
///
/// The block and the selector read the same list, so the numbers a model is
/// shown are the numbers [`workspace_set::WorkspaceSet::select`] resolves. The
/// machine is carried as a named variant rather than a rendered phrase because
/// the wording belongs to the prompt profile, which is what translates it.
#[cfg(not(test))]
fn environment_workspaces(
    workspaces: &workspace_set::WorkspaceSet,
) -> Vec<environment_prompt::EnvironmentWorkspace> {
    workspaces
        .entries()
        .iter()
        .map(|workspace| environment_prompt::EnvironmentWorkspace {
            number: workspace.index,
            path: workspace.root.clone(),
            machine: environment_machine(workspace),
        })
        .collect()
}

fn environment_machine(
    workspace: &workspace_set::ResolvedWorkspace,
) -> environment_prompt::EnvironmentMachine {
    match &workspace.machine {
        None => environment_prompt::EnvironmentMachine::Host,
        Some(model::RunTarget::Wsl { distro }) => {
            environment_prompt::EnvironmentMachine::Wsl(distro.clone())
        }
        // The catalog name when the machine is still registered; the host
        // address is not a substitute, because a machine the user renamed
        // is the one they recognize by its name.
        Some(model::RunTarget::Ssh { .. }) => {
            environment_prompt::EnvironmentMachine::Ssh(workspace.machine_label.clone())
        }
    }
}

/// The `# Environment` facts for one run.
///
/// A primary workspace on this machine is read from disk. One on another
/// machine is stated from the record alone: `request.workspace_path` is then
/// only the host-side anchor, and nothing about it — its Git status least of
/// all — describes where the model is working.
#[cfg(not(test))]
fn environment_facts(
    request: &RunModelRequest,
    conversation: &Conversation,
) -> environment_prompt::EnvironmentFacts {
    let workspaces = environment_workspaces(&request.workspaces);
    match request.workspaces.primary() {
        Some(primary) if !primary.is_local() => environment_prompt::EnvironmentFacts::collect_remote(
            &primary.root,
            environment_machine(primary),
            workspaces,
        ),
        _ => environment_prompt::EnvironmentFacts::collect(
            Path::new(&request.workspace_path),
            // `effective_workspace_path` returns the worktree path verbatim
            // when it used one, so equality is the honest test: a recorded
            // worktree whose directory is gone fell back to the workspace
            // root, and the block must not claim otherwise.
            conversation
                .worktree
                .as_ref()
                .is_some_and(|worktree| worktree.path == request.workspace_path),
            workspaces,
        ),
    }
}

#[cfg(not(test))]
fn trusted_run_request(
    app: &AppHandle,
    state: &State<'_, AppState>,
    request_id: &str,
    mut request: RunModelRequest,
) -> Result<RunModelRequest, String> {
    // The run ID comes only from `run_model`'s parameter. Accepting one in the
    // request body would let callers claim another run's turn.
    request.request_id = request_id.to_owned();
    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let anchor = document_path(app)?;
    let document = state.document_store.read(&anchor)?;
    let provider = document
        .assets
        .api_providers
        .iter()
        .find(|provider| provider.id == request.provider.id)
        .cloned()
        .ok_or_else(|| format!("API 提供商 {} 尚未保存", request.provider.id))?;
    if !provider.enabled {
        return Err(format!("API 提供商 {} 已停用", provider.name));
    }
    if provider.family != request.provider.family
        || provider.base_url.trim_end_matches('/')
            != request.provider.base_url.trim_end_matches('/')
    {
        return Err("提供商协议或 Base URL 与后端已保存配置不一致；请等待设置保存后重试".into());
    }
    let (workspace, conversation) =
        trusted_workspace_and_conversation(&document, &request.conversation_id, "模型请求")?;
    // The provider and model must match the persisted provider's selected model;
    // renderer request bodies cannot expand this authority.
    let model = {
        let active_model_id = provider
            .active_model_id
            .as_deref()
            .ok_or_else(|| format!("API 提供商 {} 尚未选择模型", provider.name))?;
        if request.model.id != active_model_id {
            return Err(format!(
                "请求模型 {} 与提供商已保存的当前模型 {active_model_id} 不一致；请等待设置保存后重试",
                request.model.id
            ));
        }
        provider
            .models
            .iter()
            .find(|model| model.id == active_model_id)
            .cloned()
            .ok_or_else(|| format!("当前模型 {active_model_id} 不在已保存的提供商配置中"))?
    };
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法解析应用数据目录: {error}"))?;
    // The prompt profile decides every host-authored text in this run: the
    // conversation's selected file, or the built-in English profile.
    let profile = capabilities::resolve_prompt_profile(&document, conversation, &app_data);
    let runtime = capabilities::runtime_context(&document, conversation, &profile)?;
    let app_data_path = app_data.to_string_lossy().into_owned();
    let workspace_path = effective_workspace_path(&app_data, workspace, conversation)?;
    let tools = tool_catalog_with_descriptions(&profile);
    let available_tool_names = tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<HashSet<_>>();

    request.provider = provider;
    // The effective web-search view combines conversation behavior with the
    // persisted external-service catalog and selection. The family decides
    // whether the conversation's own model can fetch a page for itself, which is
    // what a `Native` fetch selection is resolved against.
    request.web_search = model::WebSearchSettings::effective(
        &conversation.settings.web_search,
        &document.assets.web_search,
        crate::web_search::family_supports_native_fetch(
            crate::aisdk::protocol::Family::for_format(request.provider.family),
        ),
    );
    request.model = model;
    request.workspace_id = workspace.id.clone();
    request.workspace_path = workspace_path;
    // Extra directories widen this run's filesystem boundary, so they are taken
    // from the persisted conversation rather than from the request the renderer
    // sent. Every entry was authorized through the host's directory picker, and
    // `storage::validate_workspace_authorizations` re-checks that on each save.
    //
    // Resolve shell run locations from the persisted `run_target`. A deleted SSH
    // machine must fail explicitly rather than silently running locally.
    request.run_environment = crate::run_environment::resolve_shell_runner(
        &document.assets.execution_environments,
        conversation.run_target.as_ref(),
    )?;
    // The numbered workspaces this run addresses. Resolved here, from the same
    // atomic read of the document, so the machine catalog a call dispatches to
    // cannot change between this snapshot and the calls the turn makes.
    request.workspaces = crate::workspace_set::WorkspaceSet::resolve(
        &document.assets.execution_environments,
        &primary_workspace(workspace, &request.workspace_path),
        &conversation.effective_attached_workspaces(),
    )?;
    // The local path guard trusts every workspace on this machine. A workspace on
    // another machine is not a path in this filesystem, so it never widens the
    // local boundary — the tool that reaches it goes through that machine's shell.
    request.additional_directories = request.workspaces.local_roots();
    request.ephemeral_contexts.clear();
    request.enabled_tools = conversation
        .settings
        .enabled_tools
        .iter()
        .filter(|name| available_tool_names.contains(name.as_str()))
        .cloned()
        .collect();
    // Dynamic conversation settings and preset selection are trusted from the same atomic
    // persisted snapshot as provider/model/workspace policy, rather than renderer-supplied data.
    // Host-rendered sections are stable for the run; timeline system cards are
    // appended separately by the wire layer as dynamic context.
    request.assembled_system_prompt = assemble_system_prompt(
        &profile,
        &environment_prompt::environment_section(
            &profile,
            &environment_facts(&request, conversation),
        ),
        &runtime.addendum,
        conversation
            .settings
            .include_app_data_path
            .then_some(app_data_path.as_str()),
    );
    request.prompt_profile = std::sync::Arc::new(profile);
    // A nonempty `runtime_context` means skill bodies were withheld from the
    // system prompt and must be loaded through the `skill` tool.
    request.skills = runtime.skills;
    // Skills selected after this conversation's prompt was opened. `run_model`
    // turns each into one system message, once.
    request.added_skills = runtime.added_skills;

    // The two memory tiers are per-conversation switches, hydrated here from
    // the same trusted persisted snapshot as provider/model/workspace policy.
    // The renderer never asserts them and the model never sees them as
    // arguments.
    request.global_memory_enabled = conversation.settings.global_memory_enabled;
    request.project_memory_enabled = conversation.settings.project_memory_enabled;
    request.memory_context_id = None;
    request.project_memory_context_id = None;
    request.agent_definition_binding = None;
    request.inherits_parent_model_memory = false;
    request.fork_model_binding = None;
    request.subagent_execution_mode_receipt = None;
    let mut reserved_subagent_names = api::subagent_names_in_contexts(&conversation.contexts);
    for branch in &conversation.branches {
        reserved_subagent_names.extend(api::subagent_names_in_contexts(&branch.contexts));
    }
    request.subagent_reserved_names = reserved_subagent_names.into_iter().collect();
    request.subagent_reserved_names.sort();
    request.memory_run_id = Some(request_id.to_owned());
    request.context_load_actor_name = None;

    // Memory tools never come from the conversation's enabled-tool list; the
    // switches derive them. Strip all six first so a stale persisted name can
    // never survive a tier the user turned off, then re-add exactly the three
    // that each enabled tier owns.
    request
        .enabled_tools
        .retain(|name| !is_memory_tool_name(name));
    if request.memory_enabled() {
        // Both tiers come from host-trusted directories: the platform home for
        // global memory and this run's own workspace for project memory. A
        // tier with no directory simply contributes nothing, and a tier the
        // conversation left off is never read at all.
        let roots = mework_memory::MemoryRoots::resolve_enabled(
            dirs::home_dir().as_deref(),
            (!request.workspace_path.trim().is_empty())
                .then(|| Path::new(request.workspace_path.as_str())),
            request.memory_tier_access(),
        );
        if let Some(prompt) = roots.render_context(&request.prompt_profile) {
            request.memory_context_id =
                push_ephemeral_user_context(&mut request, "model_memory", prompt);
        }
        for tier in [
            mework_memory::MemoryTier::Global,
            mework_memory::MemoryTier::Project,
        ] {
            if !request.memory_tier_access().allows(tier) {
                continue;
            }
            request.enabled_tools.extend(
                mework_memory::tool_names_for_tier(tier)
                    .iter()
                    .map(|name| (*name).to_owned()),
            );
        }
    }
    // The two task-runtime tools follow the same rule as memory: never taken
    // from the persisted list, always re-derived. See
    // `agents::apply_task_runtime_tools` for the strip-then-derive rule itself.
    agents::apply_task_runtime_tools(&mut request.enabled_tools);
    // Same rule for the two web tools, driven by the conversation's single
    // web-access switch rather than by two checkboxes. Which of the pair this
    // grants depends on the backend that just resolved above, so it has to run
    // after `request.web_search`.
    request.web_search_enabled = conversation.settings.web_search_enabled;
    crate::web_search::apply_web_tools(
        &mut request.enabled_tools,
        request.web_search_enabled,
        &request.web_search,
    );
    // Same rule for the plan tools, except that there is nothing to derive
    // here: their availability follows the level in force at each step, so
    // `aisdk::tools::enabled_tools` derives them per step and this list only
    // has to stop carrying a stale name into a conversation whose level moved.
    request
        .enabled_tools
        .retain(|name| !plan_mode::is_plan_mode_tool_name(name));
    // Same rule again for `skill`, and it must run after the filter above:
    // `skill` IS in the catalog, so a stale persisted name would otherwise
    // survive `available_tool_names` into a conversation that turned the switch
    // back off.
    capabilities::apply_skill_tool(&mut request.enabled_tools, request.skills.len());
    // `tool_search` follows the same strip-then-derive rule, but the deriving
    // half cannot happen here: it is conditioned on tools whose names only
    // exist once the MCP servers answer, which is `api::attach_mcp_tools`. So
    // this half only guarantees a stale persisted name cannot survive into a
    // run that has nothing to withhold.
    request
        .enabled_tools
        .retain(|name| name != capabilities::TOOL_SEARCH_TOOL);
    // Whether this run withholds MCP tool schemas. Floored by the lock the same
    // way skill delivery is: once MCP tools have gone out one way, the other
    // way would either redeclare what the transcript holds or point at a tool
    // the earlier rounds never had.
    request.mcp_tool_discovery = match conversation.settings.tool_lock.as_ref() {
        Some(lock) if !lock.mcp_ids.is_empty() => lock.mcp_tool_discovery,
        _ => conversation.settings.mcp_tool_discovery_enabled,
    };
    // The file write guards are unconditional, so nothing is read from the
    // settings for them. What a run still resolves here is the read record it
    // consults: a top-level run's is the conversation's own scope.
    request.file_guard = model::FileGuard {
        scope: String::new(),
        parent_scope: None,
    };
    request.reasoning_effort = conversation.settings.reasoning_effort;
    request.security_level = conversation.settings.security_level;
    // Conversation history is provider input, not presentation data. Bind the
    // run to the same committed snapshot that supplied provider, model,
    // workspace and approval policy. A compromised renderer may display or
    // propose edits, but it cannot inject an unsaved/forged tool success
    // receipt directly into the next provider request.
    //
    // Read bodies from the conversation database because streaming persistence
    // bypasses command-layer snapshot refreshes; wake runs have no user turn to
    // advance the snapshot.
    request.contexts = authoritative_contexts(
        stored_conversation(&anchor, &request.conversation_id),
        conversation,
    );
    request.app_data_path = app_data_path;
    // The servers to dial were resolved with the skills and hooks above, from
    // the same workspace-scoped scan of `mcp.json`.
    request.mcp_servers = runtime.mcp_servers;
    request.mcp_bindings.clear();
    request.active_hooks = runtime.hooks;
    // Tool descriptors are executable policy, not presentation data. Start from the canonical
    // catalog and apply only the separately persisted model-facing description override; never
    // trust a renderer-supplied descriptor that could relabel PowerShell or change its schema.
    request.tools = tools;
    Ok(request)
}

/// Replaces a renderer-supplied provider with its persisted version.
///
/// `fetch_models` relies on this trust boundary: request destination, protocol,
/// and credential identity come only from the host snapshot. `enabled` is
/// intentionally not checked because this is a configuration action;
/// conversation requests enforce enabled status separately.
#[cfg(not(test))]
fn trusted_provider(
    app: &AppHandle,
    state: &State<'_, AppState>,
    requested: &ApiProvider,
) -> Result<ApiProvider, String> {
    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let document = state.document_store.read(&document_path(app)?)?;
    let stored = document
        .assets
        .api_providers
        .iter()
        .find(|provider| provider.id == requested.id)
        .ok_or_else(|| format!("API 提供商 {} 尚未保存", requested.id))?;
    if stored.family != requested.family
        || stored.base_url.trim_end_matches('/') != requested.base_url.trim_end_matches('/')
    {
        return Err("提供商协议或 Base URL 与后端已保存配置不一致；请等待设置保存后重试".into());
    }
    Ok(stored.clone())
}

#[cfg(not(test))]
struct TrustedConversationPolicy {
    workspace_path: String,
    /// Extra directories this conversation was granted. They widen the
    /// filesystem boundary exactly as the workspace does, so a manually executed
    /// tool card is classified against the same set a model run is.
    additional_directories: Vec<String>,
    security_level: SecurityLevel,
    enabled_tools: Vec<String>,
    tools: Vec<ToolDescriptor>,
    /// Trusted shell environment resolved from the persisted `run_target`; it is
    /// never supplied in the renderer request body.
    run_environment: crate::run_environment::ShellRunner,
    /// The numbered workspaces this conversation addresses, resolved from the
    /// same persisted record a model run reads. A manually executed tool card
    /// carries a `workspace` argument like any other call, and it must select
    /// from exactly the set the model was offered.
    workspaces: crate::workspace_set::WorkspaceSet,
    /// The conversation's selected profile. A manually executed tool card writes
    /// its result into the same conversation a model run would, so it has to be
    /// worded by the same profile; resolving it here keeps the IPC path from
    /// silently falling back to the built-in English wording.
    prompt_profile: prompt_profile::PromptProfile,
}

#[cfg(not(test))]
#[derive(Clone, Copy)]
enum TrustedWorkspaceAccess {
    Shared,
    Exclusive,
}

#[cfg(not(test))]
struct TrustedWorkspaceOperation {
    git_network_policy: git::GitNetworkPolicy,
    workspace_path: PathBuf,
    workspace_key: WorkspaceKey,
    lease: Box<dyn Send>,
}

#[cfg(not(test))]
fn trusted_workspace_operation(
    app: &AppHandle,
    state: &AppState,
    requested_conversation: &str,
    request_label: &str,
    access: TrustedWorkspaceAccess,
) -> Result<TrustedWorkspaceOperation, String> {
    trusted_target_workspace_operation(
        app,
        state,
        &GitTarget::Conversation {
            conversation_id: requested_conversation.to_owned(),
        },
        request_label,
        access,
        None,
    )
}

fn git_network_policy_for_target(
    target: &workspace_lookup::ResolvedGitTarget<'_>,
) -> git::GitNetworkPolicy {
    match target {
        workspace_lookup::ResolvedGitTarget::Conversation { conversation, .. }
            if conversation.settings.security_level == crate::model::SecurityLevel::FullAccess =>
        {
            git::GitNetworkPolicy::FullAccess
        }
        _ => git::GitNetworkPolicy::Restricted,
    }
}

#[cfg(test)]
#[test]
fn git_network_policy_uses_persisted_conversation_and_restricts_workspace_targets() {
    let mut document = catalog::default_document();
    let workspace = &mut document.workspaces[0];
    workspace.conversations[0].settings.security_level = crate::model::SecurityLevel::FullAccess;
    let conversation_target = workspace_lookup::ResolvedGitTarget::Conversation {
        workspace,
        conversation: &workspace.conversations[0],
    };
    assert_eq!(
        git_network_policy_for_target(&conversation_target),
        git::GitNetworkPolicy::FullAccess
    );
    assert_eq!(
        git_network_policy_for_target(&workspace_lookup::ResolvedGitTarget::Workspace {
            workspace
        }),
        git::GitNetworkPolicy::Restricted
    );
    for level in [
        crate::model::SecurityLevel::Plan,
        crate::model::SecurityLevel::RequestApproval,
        crate::model::SecurityLevel::AllowEdits,
    ] {
        workspace.conversations[0].settings.security_level = level;
        assert_eq!(
            git_network_policy_for_target(&workspace_lookup::ResolvedGitTarget::Conversation {
                workspace,
                conversation: &workspace.conversations[0],
            }),
            git::GitNetworkPolicy::Restricted
        );
    }
}

#[cfg(not(test))]
fn trusted_target_workspace_operation(
    app: &AppHandle,
    state: &AppState,
    target: &GitTarget,
    request_label: &str,
    access: TrustedWorkspaceAccess,
    write_surface: Option<&str>,
) -> Result<TrustedWorkspaceOperation, String> {
    // The storage lock binds the persisted conversation/workspace mapping to
    // the coordinator acquisition. A concurrent save/reset can therefore only
    // happen wholly before this operation or after its lease has been stored.
    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let document = state.document_store.read(&document_path(app)?)?;
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法解析应用数据目录: {error}"))?;
    let resolved_target = workspace_lookup::resolve_git_target(&document, target, request_label)?;
    // Git, worktrees and the file pane act on a checkout in this filesystem. A
    // workspace on another machine has none here, and resolving its path
    // locally would act on whatever sits at the same spelling on this host.
    let target_workspace = match &resolved_target {
        workspace_lookup::ResolvedGitTarget::Conversation { workspace, .. }
        | workspace_lookup::ResolvedGitTarget::Workspace { workspace } => *workspace,
    };
    if let Some(machine) = &target_workspace.machine {
        return Err(format!(
            "{request_label}的工作区在另一台机器上（{}），本机的 Git 与文件操作无法作用于它",
            workspace_machine_label(&document.assets.execution_environments, machine)
        ));
    }
    let git_network_policy = git_network_policy_for_target(&resolved_target);
    // Workspace-addressed writes have no conversation to inspect, so they collect the
    // conversations running against the same root checkout; isolated-worktree
    // conversations use another directory and are excluded.
    let (workspace_path, attribution, root_conversation_ids) = match resolved_target {
        workspace_lookup::ResolvedGitTarget::Conversation {
            workspace,
            conversation,
        } => (
            effective_workspace_path(&app_data, workspace, conversation)?,
            Some(conversation.id.clone()),
            Vec::new(),
        ),
        workspace_lookup::ResolvedGitTarget::Workspace { workspace } => (
            workspace.path.clone(),
            None,
            workspace
                .conversations
                .iter()
                .filter(|conversation| !conversation_runs_in_its_own_worktree(conversation))
                .map(|conversation| conversation.id.clone())
                .collect(),
        ),
    };
    let workspace_path = PathBuf::from(workspace_path);
    // Evaluate before leasing under the storage lock to avoid missing a run that
    // will write next and to preserve the specific conflict reason.
    if let Some(surface) = write_surface {
        reject_git_write_during_model_run(state, target, &root_conversation_ids, surface)?;
    }
    let canonical_workspace = std::fs::canonicalize(&workspace_path).map_err(|error| {
        format!(
            "{request_label}的工作区路径不存在或无法访问（{}）: {error}",
            workspace_path.display()
        )
    })?;
    let workspace_key = WorkspaceKey::new(canonical_workspace);
    let operation_gate = state.operation_gate();
    let lease: Box<dyn Send> = match access {
        TrustedWorkspaceAccess::Shared => {
            Box::new(operation_gate.begin_workspace_operation(workspace_key.clone(), attribution)?)
        }
        TrustedWorkspaceAccess::Exclusive => {
            Box::new(operation_gate.begin_workspace_mutation(workspace_key.clone(), attribution)?)
        }
    };
    Ok(TrustedWorkspaceOperation {
        git_network_policy,
        workspace_path,
        workspace_key,
        lease,
    })
}

/// The name a message shows for a machine: the distribution, or the SSH
/// machine's catalog name — its id when the catalog no longer has it.
#[cfg(not(test))]
fn workspace_machine_label(
    assets: &model::ExecutionEnvironmentAssets,
    machine: &model::RunTarget,
) -> String {
    match machine {
        model::RunTarget::Wsl { distro } => format!("WSL: {distro}"),
        model::RunTarget::Ssh { machine_id } => format!(
            "SSH: {}",
            assets
                .ssh_machines
                .iter()
                .find(|candidate| candidate.id == *machine_id)
                .map(|candidate| candidate.name.as_str())
                .unwrap_or(machine_id)
        ),
    }
}

/// Match `effective_workspace_path` exactly: a conversation runs in its worktree
/// only when it is registered and the directory exists; otherwise it runs at root.
#[cfg(not(test))]
fn conversation_runs_in_its_own_worktree(conversation: &Conversation) -> bool {
    conversation
        .worktree
        .as_ref()
        .is_some_and(|worktree| Path::new(&worktree.path).is_dir())
}

#[cfg(not(test))]
fn trusted_conversation_policy(
    app: &AppHandle,
    state: &AppState,
    requested_conversation: &str,
    request_label: &str,
) -> Result<TrustedConversationPolicy, String> {
    let _guard = state
        .storage_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let document = state.document_store.read(&document_path(app)?)?;
    trusted_conversation_policy_from_document(app, &document, requested_conversation, request_label)
}

#[cfg(not(test))]
fn trusted_conversation_policy_from_document(
    app: &AppHandle,
    document: &AppDocument,
    requested_conversation: &str,
    request_label: &str,
) -> Result<TrustedConversationPolicy, String> {
    let (workspace, conversation) =
        trusted_workspace_and_conversation(document, requested_conversation, request_label)?;
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法解析应用数据目录: {error}"))?;
    let workspace_path = effective_workspace_path(&app_data, workspace, conversation)?;
    let prompt_profile = capabilities::resolve_prompt_profile(document, conversation, &app_data);
    let tools = tool_catalog_with_descriptions(&prompt_profile);
    let available_tool_names = tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<HashSet<_>>();
    let enabled_tools = conversation
        .settings
        .enabled_tools
        .iter()
        .filter(|name| available_tool_names.contains(name.as_str()))
        .cloned()
        .collect();
    let run_environment = crate::run_environment::resolve_shell_runner(
        &document.assets.execution_environments,
        conversation.run_target.as_ref(),
    )?;
    let workspaces = crate::workspace_set::WorkspaceSet::resolve(
        &document.assets.execution_environments,
        &primary_workspace(workspace, &workspace_path),
        &conversation.effective_attached_workspaces(),
    )?;
    Ok(TrustedConversationPolicy {
        workspace_path,
        additional_directories: workspaces.local_roots(),
        security_level: conversation.settings.security_level,
        enabled_tools,
        tools,
        run_environment,
        workspaces,
        prompt_profile,
    })
}

#[cfg(not(test))]
fn trusted_workspace_and_conversation<'a>(
    document: &'a AppDocument,
    conversation_id: &str,
    request_label: &str,
) -> Result<(&'a Workspace, &'a Conversation), String> {
    workspace_lookup::find_workspace_for_conversation(document, conversation_id, request_label)
}

#[cfg(not(test))]
fn effective_workspace_path(
    app_data: &Path,
    workspace: &Workspace,
    conversation: &Conversation,
) -> Result<String, String> {
    match workspace.kind {
        WorkspaceKind::Directory => {
            if workspace.path.trim().is_empty() {
                return Err(format!("工作区 {} 的路径为空", workspace.id));
            }
            // A directory on another machine is not a path in this filesystem.
            // The host still needs a local directory for its own subsystems —
            // preview, LSP, image staging, the shell's cwd file — so such a
            // conversation anchors in its own scratch directory, and the
            // remote root reaches the tools through the workspace set instead.
            // A recorded worktree cannot exist for it: worktrees are Git
            // checkouts the host makes on this machine.
            if workspace.machine.is_some() {
                return workspace_dirs::ensure_remote_workspace_anchor(app_data, &conversation.id)
                    .map(|path| path.to_string_lossy().into_owned());
            }
            // An isolated worktree is this conversation's trusted directory; all
            // of its tool calls must resolve there.
            //
            // Fall back to the workspace root when a registered directory no
            // longer exists; stale records must not make conversations unusable.
            if let Some(worktree) = &conversation.worktree {
                let path = Path::new(&worktree.path);
                if path.is_dir() {
                    return Ok(worktree.path.clone());
                }
            }
            Ok(workspace.path.clone())
        }
        WorkspaceKind::Temporary => {
            if workspace.id != "__temporary__" {
                return Err("临时工作区对话不在保留临时工作区中".into());
            }
            workspace_dirs::ensure_temporary_workspace(app_data, &conversation.id)
                .map(|path| path.to_string_lossy().into_owned())
        }
        WorkspaceKind::Unsupported => Err(format!("工作区 {} 的类型不受支持", workspace.id)),
    }
}

/// The primary workspace as the numbered set records it: entry 1, on the
/// machine the workspace is registered on.
///
/// `anchor` is what [`effective_workspace_path`] returned. For a workspace on
/// this machine it is the directory the calls act in — the worktree when the
/// conversation has one — and it is entry 1 verbatim. For a workspace on
/// another machine it is only the host-side scratch directory, and entry 1 is
/// the recorded remote root instead.
#[cfg(not(test))]
fn primary_workspace(workspace: &Workspace, anchor: &str) -> model::AttachedWorkspace {
    match &workspace.machine {
        Some(machine) if workspace.kind == WorkspaceKind::Directory => model::AttachedWorkspace {
            machine: Some(machine.clone()),
            path: workspace.path.clone(),
        },
        _ => model::AttachedWorkspace {
            machine: None,
            path: anchor.to_owned(),
        },
    }
}

#[cfg(not(test))]
fn document_path(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|directory| directory.join("document.v1.json"))
        .map_err(|error| format!("无法解析应用数据目录: {error}"))
}

#[cfg(not(test))]
fn migrate_legacy_app_data(app: &AppHandle) -> Result<(), String> {
    let current = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法解析 Mework 应用数据目录: {error}"))?;
    if current.file_name().and_then(|name| name.to_str()) != Some(APP_IDENTIFIER)
        || current.join("document.v1.json").exists()
    {
        return Ok(());
    }
    let Some(parent) = current.parent() else {
        return Ok(());
    };
    let legacy = parent.join(LEGACY_APP_IDENTIFIER);
    if !legacy.is_dir() {
        return Ok(());
    }

    std::fs::create_dir_all(&current)
        .map_err(|error| format!("无法创建 Mework 应用数据目录: {error}"))?;
    migrate_legacy_app_data_directory(&legacy, &current)?;
    Ok(())
}

const LEGACY_DOCUMENT_FILE: &str = "document.v1.json";
const LEGACY_AUTHORITY_FILE: &str = ".document.v1.json.instance.lock";
const LEGACY_MIGRATION_STAGE_PREFIX: &str = ".mework-legacy-migration-";

#[derive(Clone, Debug, Eq, PartialEq)]
enum LegacyManifestKind {
    Directory,
    File { bytes: u64, sha256: [u8; 32] },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LegacyManifestEntry {
    relative_path: PathBuf,
    kind: LegacyManifestKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LegacyMigrationHookPoint {
    AfterStaging,
    BeforeTargetReplace,
}

struct LegacyMigrationStage {
    path: PathBuf,
}

impl LegacyMigrationStage {
    fn create(target: &Path) -> Result<Self, String> {
        for _ in 0..8 {
            let path = target.join(format!(
                "{LEGACY_MIGRATION_STAGE_PREFIX}{}.staging",
                uuid::Uuid::new_v4().simple()
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(format!(
                        "无法创建唯一旧版应用数据迁移暂存目录 {}: {error}",
                        path.display()
                    ));
                }
            }
        }
        Err("无法分配唯一旧版应用数据迁移暂存目录".to_owned())
    }
}

impl Drop for LegacyMigrationStage {
    fn drop(&mut self) {
        let Ok(metadata) = std::fs::symlink_metadata(&self.path) else {
            return;
        };
        if metadata.is_dir() && !legacy_metadata_is_link(&metadata) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

struct LegacyPreparedFile {
    path: PathBuf,
}

impl LegacyPreparedFile {
    fn publish(mut self, destination: &Path) -> Result<(), String> {
        replace_legacy_file(&self.path, destination)?;
        self.path.clear();
        sync_legacy_parent(destination)?;
        Ok(())
    }
}

impl Drop for LegacyPreparedFile {
    fn drop(&mut self) {
        if !self.path.as_os_str().is_empty() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn migrate_legacy_app_data_directory(source: &Path, target: &Path) -> Result<(), String> {
    migrate_legacy_app_data_directory_with_hook(source, target, |_, _| Ok(()))
}

fn migrate_legacy_app_data_directory_with_hook<F>(
    source: &Path,
    target: &Path,
    mut hook: F,
) -> Result<(), String>
where
    F: FnMut(LegacyMigrationHookPoint, Option<&Path>) -> Result<(), String>,
{
    std::fs::create_dir_all(target)
        .map_err(|error| format!("无法创建 Mework 应用数据迁移目录: {error}"))?;
    ensure_legacy_directory(target, Path::new(""))?;
    let target_document = target.join(LEGACY_DOCUMENT_FILE);
    match std::fs::symlink_metadata(&target_document) {
        Ok(metadata) if metadata.is_file() && !legacy_metadata_is_link(&metadata) => return Ok(()),
        Ok(_) => {
            return Err(
                "Mework 目标 document.v1.json 不是普通文件；为避免覆盖数据，迁移已停止".to_owned(),
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "无法检查 Mework 目标 document.v1.json，迁移已停止: {error}"
            ));
        }
    }

    let source_before = legacy_tree_manifest(source)?;
    let document_relative = Path::new(LEGACY_DOCUMENT_FILE);
    let Some(document_entry) = source_before
        .iter()
        .find(|entry| entry.relative_path == document_relative)
    else {
        return Ok(());
    };
    if !matches!(document_entry.kind, LegacyManifestKind::File { .. }) {
        return Err("旧版应用数据的 document.v1.json 不是普通文件，拒绝迁移".to_owned());
    }

    let stage = LegacyMigrationStage::create(target)?;
    materialize_legacy_manifest(source, &stage.path, &source_before).map_err(|error| {
        format!("旧版应用数据在复制时发生变化或无法完整读取；请完全退出旧版应用后重试：{error}")
    })?;
    hook(LegacyMigrationHookPoint::AfterStaging, None)?;

    let source_after = legacy_tree_manifest(source).map_err(|error| {
        format!("检测到旧版应用可能仍在写入应用数据；请完全退出旧版应用后重新启动 Mework：{error}")
    })?;
    if source_before != source_after {
        return Err(
            "检测到旧版应用仍在写入应用数据；请完全退出旧版应用后重新启动 Mework".to_owned(),
        );
    }

    // Directories and non-authoritative files are published first. Every file
    // reaches the target through a same-directory temporary file plus atomic
    // replacement, so a failed attempt can leave only complete files. The
    // document is the commit marker and is published strictly last.
    for entry in source_before.iter().filter(|entry| {
        entry.relative_path != document_relative
            && !legacy_path_is_reserved_authority(&entry.relative_path)
    }) {
        if matches!(entry.kind, LegacyManifestKind::Directory) {
            ensure_legacy_directory(target, &entry.relative_path)?;
        }
    }
    for entry in source_before.iter().filter(|entry| {
        entry.relative_path != document_relative
            && !legacy_path_is_reserved_authority(&entry.relative_path)
            && matches!(entry.kind, LegacyManifestKind::File { .. })
    }) {
        publish_legacy_manifest_file(&stage.path, target, entry, &mut hook)?;
    }

    match std::fs::symlink_metadata(&target_document) {
        Ok(_) => {
            return Err(
                "Mework 目标文档在旧版迁移期间意外出现；为避免覆盖新数据，迁移已停止".to_owned(),
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "无法确认 Mework 目标文档仍为空缺；迁移已停止: {error}"
            ));
        }
    }
    publish_legacy_manifest_file(&stage.path, target, document_entry, &mut hook)?;
    Ok(())
}

fn materialize_legacy_manifest(
    source: &Path,
    stage: &Path,
    manifest: &[LegacyManifestEntry],
) -> Result<(), String> {
    for entry in manifest {
        match &entry.kind {
            LegacyManifestKind::Directory => {
                ensure_legacy_directory(stage, &entry.relative_path)?;
            }
            LegacyManifestKind::File { .. } => {
                let source_path = source.join(&entry.relative_path);
                let destination = stage.join(&entry.relative_path);
                ensure_legacy_directory(
                    stage,
                    entry
                        .relative_path
                        .parent()
                        .unwrap_or_else(|| Path::new("")),
                )?;
                prepare_legacy_file(&source_path, &destination, entry)?.publish(&destination)?;
            }
        }
    }
    Ok(())
}

fn publish_legacy_manifest_file<F>(
    stage: &Path,
    target: &Path,
    entry: &LegacyManifestEntry,
    hook: &mut F,
) -> Result<(), String>
where
    F: FnMut(LegacyMigrationHookPoint, Option<&Path>) -> Result<(), String>,
{
    let source = stage.join(&entry.relative_path);
    let destination = target.join(&entry.relative_path);
    ensure_legacy_directory(
        target,
        entry
            .relative_path
            .parent()
            .unwrap_or_else(|| Path::new("")),
    )?;
    let prepared = prepare_legacy_file(&source, &destination, entry)?;
    hook(
        LegacyMigrationHookPoint::BeforeTargetReplace,
        Some(&entry.relative_path),
    )?;
    ensure_legacy_destination_replaceable(&destination)?;
    prepared.publish(&destination)
}

fn prepare_legacy_file(
    source: &Path,
    destination: &Path,
    expected: &LegacyManifestEntry,
) -> Result<LegacyPreparedFile, String> {
    use sha2::Digest as _;
    use std::io::{Read as _, Write as _};

    let LegacyManifestKind::File {
        bytes: expected_bytes,
        sha256: expected_sha256,
    } = &expected.kind
    else {
        return Err("迁移清单把目录误当成文件".to_owned());
    };
    let parent = destination
        .parent()
        .ok_or_else(|| format!("迁移目标没有父目录: {}", destination.display()))?;
    let file_name = destination
        .file_name()
        .ok_or_else(|| format!("迁移目标没有文件名: {}", destination.display()))?;
    let mut temporary_name = std::ffi::OsString::from(".");
    temporary_name.push(file_name);
    temporary_name.push(format!(".legacy-{}.tmp", uuid::Uuid::new_v4().simple()));
    let temporary = parent.join(temporary_name);
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| {
            format!(
                "无法创建旧版应用数据原子迁移临时文件 {}: {error}",
                temporary.display()
            )
        })?;
    let guard = LegacyPreparedFile {
        path: temporary.clone(),
    };
    let mut input = open_legacy_source_file(source)?;
    let metadata = input
        .metadata()
        .map_err(|error| format!("无法检查旧版应用数据文件 {}: {error}", source.display()))?;
    if legacy_metadata_is_link(&metadata) || !metadata.is_file() {
        return Err(format!(
            "旧版应用数据路径不是普通文件: {}",
            source.display()
        ));
    }

    let mut hasher = sha2::Sha256::new();
    let mut copied = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = input
            .read(&mut buffer)
            .map_err(|error| format!("无法读取旧版应用数据文件 {}: {error}", source.display()))?;
        if read == 0 {
            break;
        }
        output.write_all(&buffer[..read]).map_err(|error| {
            format!(
                "无法写入旧版应用数据迁移临时文件 {}: {error}",
                temporary.display()
            )
        })?;
        hasher.update(&buffer[..read]);
        copied = copied
            .checked_add(read as u64)
            .ok_or_else(|| "旧版应用数据文件长度溢出".to_owned())?;
    }
    output.sync_all().map_err(|error| {
        format!(
            "无法持久化旧版应用数据迁移临时文件 {}: {error}",
            temporary.display()
        )
    })?;
    drop(output);

    let actual_sha256: [u8; 32] = hasher.finalize().into();
    if copied != *expected_bytes || actual_sha256 != *expected_sha256 {
        return Err(format!(
            "旧版应用数据文件在迁移期间发生变化: {}",
            expected.relative_path.display()
        ));
    }
    Ok(guard)
}

fn legacy_tree_manifest(root: &Path) -> Result<Vec<LegacyManifestEntry>, String> {
    let root_metadata = std::fs::symlink_metadata(root)
        .map_err(|error| format!("无法检查旧版应用数据目录 {}: {error}", root.display()))?;
    if legacy_metadata_is_link(&root_metadata) || !root_metadata.is_dir() {
        return Err(format!(
            "旧版应用数据根路径不是普通目录: {}",
            root.display()
        ));
    }
    let mut entries = Vec::new();
    collect_legacy_manifest(root, Path::new(""), &mut entries)?;
    entries.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(entries)
}

fn collect_legacy_manifest(
    root: &Path,
    relative_directory: &Path,
    entries: &mut Vec<LegacyManifestEntry>,
) -> Result<(), String> {
    let directory = root.join(relative_directory);
    let mut children = std::fs::read_dir(&directory)
        .map_err(|error| format!("无法读取旧版应用数据目录 {}: {error}", directory.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("无法读取旧版应用数据条目: {error}"))?;
    children.sort_by_key(std::fs::DirEntry::file_name);

    for child in children {
        let relative_path = relative_directory.join(child.file_name());
        let path = child.path();
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| format!("无法检查旧版应用数据路径 {}: {error}", path.display()))?;
        if legacy_metadata_is_link(&metadata) {
            return Err(format!(
                "旧版应用数据包含链接或重解析点，无法安全迁移: {}",
                path.display()
            ));
        }
        if metadata.is_dir() {
            entries.push(LegacyManifestEntry {
                relative_path: relative_path.clone(),
                kind: LegacyManifestKind::Directory,
            });
            collect_legacy_manifest(root, &relative_path, entries)?;
        } else if metadata.is_file() {
            let (bytes, sha256) = hash_legacy_file(&path)?;
            entries.push(LegacyManifestEntry {
                relative_path,
                kind: LegacyManifestKind::File { bytes, sha256 },
            });
        } else {
            return Err(format!(
                "旧版应用数据包含非普通文件，无法安全迁移: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn hash_legacy_file(path: &Path) -> Result<(u64, [u8; 32]), String> {
    use sha2::Digest as _;
    use std::io::Read as _;

    let mut file = open_legacy_source_file(path)?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("无法检查旧版应用数据文件 {}: {error}", path.display()))?;
    if legacy_metadata_is_link(&metadata) || !metadata.is_file() {
        return Err(format!("旧版应用数据路径不是普通文件: {}", path.display()));
    }
    let mut hasher = sha2::Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("无法读取旧版应用数据文件 {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        bytes = bytes
            .checked_add(read as u64)
            .ok_or_else(|| "旧版应用数据文件长度溢出".to_owned())?;
    }
    Ok((bytes, hasher.finalize().into()))
}

fn open_legacy_source_file(path: &Path) -> Result<std::fs::File, String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

        // Keep a final-component path swap from silently redirecting the copy
        // through a symlink, junction or another reparse point. The metadata
        // check on the returned handle below then rejects the reparse object.
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    options
        .open(path)
        .map_err(|error| format!("无法安全打开旧版应用数据文件 {}: {error}", path.display()))
}

fn ensure_legacy_directory(root: &Path, relative: &Path) -> Result<(), String> {
    let mut current = root.to_path_buf();
    match std::fs::symlink_metadata(&current) {
        Ok(metadata) if metadata.is_dir() && !legacy_metadata_is_link(&metadata) => {}
        Ok(_) => {
            return Err(format!("旧版迁移目录不是普通目录: {}", current.display()));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(&current)
                .map_err(|error| format!("无法创建旧版迁移目录 {}: {error}", current.display()))?;
        }
        Err(error) => {
            return Err(format!(
                "无法检查旧版迁移目录 {}: {error}",
                current.display()
            ));
        }
    }

    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            return Err(format!(
                "旧版迁移路径不是安全相对路径: {}",
                relative.display()
            ));
        };
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.is_dir() && !legacy_metadata_is_link(&metadata) => {}
            Ok(_) => {
                return Err(format!(
                    "旧版迁移目录与现有非目录冲突: {}",
                    current.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&current).map_err(|error| {
                    format!("无法创建旧版迁移目录 {}: {error}", current.display())
                })?;
            }
            Err(error) => {
                return Err(format!(
                    "无法检查旧版迁移目录 {}: {error}",
                    current.display()
                ));
            }
        }
    }
    Ok(())
}

fn ensure_legacy_destination_replaceable(destination: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(destination) {
        Ok(metadata) if metadata.is_file() && !legacy_metadata_is_link(&metadata) => Ok(()),
        Ok(_) => Err(format!(
            "旧版迁移目标与现有非普通文件冲突: {}",
            destination.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "无法检查旧版迁移目标 {}: {error}",
            destination.display()
        )),
    }
}

fn legacy_path_is_reserved_authority(relative: &Path) -> bool {
    relative == Path::new(LEGACY_AUTHORITY_FILE)
        || relative.starts_with(Path::new(LEGACY_AUTHORITY_FILE))
}

fn legacy_metadata_is_link(metadata: &std::fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn sync_legacy_parent(destination: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        let parent = destination
            .parent()
            .ok_or_else(|| format!("迁移目标没有父目录: {}", destination.display()))?;
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("无法持久化旧版应用数据迁移目录: {error}"))?;
    }
    #[cfg(not(unix))]
    {
        let _ = destination;
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace_legacy_file(temporary: &Path, destination: &Path) -> Result<(), String> {
    std::fs::rename(temporary, destination).map_err(|error| {
        format!(
            "无法原子发布旧版应用数据文件 {}: {error}",
            destination.display()
        )
    })
}

#[cfg(windows)]
fn replace_legacy_file(temporary: &Path, destination: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source = temporary
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let target = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(format!(
            "无法原子发布旧版应用数据文件 {}: {}",
            destination.display(),
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod legacy_migration_tests {
    use super::{
        migrate_legacy_app_data_directory, migrate_legacy_app_data_directory_with_hook,
        LegacyMigrationHookPoint, LEGACY_AUTHORITY_FILE, LEGACY_DOCUMENT_FILE,
        LEGACY_MIGRATION_STAGE_PREFIX,
    };
    use std::path::Path;

    fn write(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, bytes).unwrap();
    }

    fn migration_artifacts(root: &Path) -> Vec<std::path::PathBuf> {
        walkdir::WalkDir::new(root)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains(LEGACY_MIGRATION_STAGE_PREFIX)
                    || entry.file_name().to_string_lossy().contains(".legacy-")
            })
            .map(walkdir::DirEntry::into_path)
            .collect()
    }

    #[test]
    fn legacy_migration_publishes_verified_document_last_and_preserves_authority_file() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("legacy");
        let target = directory.path().join("current");
        write(&source.join("nested/a.bin"), b"alpha");
        write(&source.join("b.bin"), b"beta");
        write(&source.join(LEGACY_DOCUMENT_FILE), b"legacy-document");
        write(&source.join(LEGACY_AUTHORITY_FILE), b"legacy-lock");
        write(&target.join(LEGACY_AUTHORITY_FILE), b"current-lock");

        migrate_legacy_app_data_directory(&source, &target).unwrap();

        assert_eq!(
            std::fs::read(target.join("nested/a.bin")).unwrap(),
            b"alpha"
        );
        assert_eq!(std::fs::read(target.join("b.bin")).unwrap(), b"beta");
        assert_eq!(
            std::fs::read(target.join(LEGACY_DOCUMENT_FILE)).unwrap(),
            b"legacy-document"
        );
        assert_eq!(
            std::fs::read(target.join(LEGACY_AUTHORITY_FILE)).unwrap(),
            b"current-lock"
        );
        assert!(migration_artifacts(&target).is_empty());
    }

    #[test]
    fn legacy_migration_rejects_a_source_that_changes_after_staging() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("legacy");
        let target = directory.path().join("current");
        write(&source.join("data.bin"), b"before");
        write(&source.join(LEGACY_DOCUMENT_FILE), b"legacy-document");

        let error =
            migrate_legacy_app_data_directory_with_hook(&source, &target, |point, _relative| {
                if point == LegacyMigrationHookPoint::AfterStaging {
                    write(&source.join("data.bin"), b"after");
                }
                Ok(())
            })
            .unwrap_err();

        assert!(error.contains("旧版应用仍在写入"), "{error}");
        assert!(!target.join("data.bin").exists());
        assert!(!target.join(LEGACY_DOCUMENT_FILE).exists());
        assert!(migration_artifacts(&target).is_empty());
    }

    #[test]
    fn failed_atomic_publish_keeps_complete_target_and_retry_finishes() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("legacy");
        let target = directory.path().join("current");
        write(&source.join("a.bin"), b"alpha");
        write(&source.join("b.bin"), b"beta");
        write(&source.join(LEGACY_DOCUMENT_FILE), b"legacy-document");
        write(&target.join("b.bin"), b"complete-old-target");

        let error =
            migrate_legacy_app_data_directory_with_hook(&source, &target, |point, relative| {
                if point == LegacyMigrationHookPoint::BeforeTargetReplace
                    && relative == Some(Path::new("b.bin"))
                {
                    return Err("injected publish failure".to_owned());
                }
                Ok(())
            })
            .unwrap_err();

        assert!(error.contains("injected publish failure"), "{error}");
        assert_eq!(std::fs::read(target.join("a.bin")).unwrap(), b"alpha");
        assert_eq!(
            std::fs::read(target.join("b.bin")).unwrap(),
            b"complete-old-target"
        );
        assert!(!target.join(LEGACY_DOCUMENT_FILE).exists());
        assert!(migration_artifacts(&target).is_empty());

        migrate_legacy_app_data_directory(&source, &target).unwrap();
        assert_eq!(std::fs::read(target.join("b.bin")).unwrap(), b"beta");
        assert_eq!(
            std::fs::read(target.join(LEGACY_DOCUMENT_FILE)).unwrap(),
            b"legacy-document"
        );
        assert!(migration_artifacts(&target).is_empty());
    }
}

/// Runs a bounded pre-exit barrier away from Tauri's main event loop, then exits.
///
/// The caller must synchronously invoke `prevent_exit` before calling this helper. The barrier runs
/// on a worker while Tauri's main message pump is still alive, which is what a barrier waiting on
/// native WebView callbacks needs. It returns the effective process exit code, so a failed release
/// can replace a code the caller intended to mean "controlled exit" with a failure code.
///
/// While the drain runs this process stops answering second-launch activations: a launch that
/// arrives now must wait for the lease and start on its own rather than wake a process that is
/// leaving. An abandoned drain resumes them.
#[cfg(not(test))]
pub(crate) fn request_deferred_exit_with_barrier(
    app_handle: AppHandle,
    coordinator: app_exit::AppExitCoordinator,
    exit_code: i32,
    barrier: impl FnOnce(&AppHandle, i32) -> i32 + Send + 'static,
) {
    if !coordinator.try_begin_draining() {
        return;
    }
    if let Some(listener) = app_handle.try_state::<instance_activation::ActivationListener>() {
        listener.pause();
    }

    let spawn_failure_coordinator = coordinator.clone();
    let spawn_failure_app = app_handle.clone();
    let spawn_result = std::thread::Builder::new()
        .name("mework-deferred-exit".to_owned())
        .spawn(move || {
            let drained = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let result = coordinator.finish_draining(|| {
                    let state = app_handle.state::<AppState>();
                    state.prose_journals.flush_for_exit()?;
                    state.shell_tasks.flush()?;
                    state
                        .document_store
                        .flush(std::time::Duration::from_secs(30))?;
                    let anchor = document_path(&app_handle)?;
                    conversation_store::store_for(&anchor)?.reconcile_streaming()?;
                    Ok(barrier(&app_handle, exit_code))
                });
                match result {
                    Ok(effective_exit_code) => app_handle.exit(effective_exit_code),
                    Err(error) => {
                        use tauri_plugin_dialog::DialogExt;
                        eprintln!("退出持久化失败，已保留应用：{error}");
                        resume_instance_activation(&app_handle);
                        // The quit may have come from the tray with the window
                        // hidden; the retained application must be visible.
                        app_tray::show_main_window(&app_handle, BROWSER_RENDERER_MOUNT_MAIN_LABEL);
                        app_handle
                            .dialog()
                            .message(format!(
                                "退出前保存失败，应用已保留。请解决存储问题后再次关闭。\n\n{error}"
                            ))
                            .title("Mework — 保存失败 / Save failed")
                            .kind(tauri_plugin_dialog::MessageDialogKind::Error)
                            .blocking_show();
                    }
                }
            }));

            if drained.is_err() {
                coordinator.abort_draining();
                resume_instance_activation(&app_handle);
                eprintln!("退出屏障异常，已取消本次退出");
            }
        });

    if let Err(error) = spawn_result {
        spawn_failure_coordinator.abort_draining();
        resume_instance_activation(&spawn_failure_app);
        eprintln!("无法启动退出屏障线程，已保留应用：{error}");
    }
}

#[cfg(not(test))]
fn resume_instance_activation(app_handle: &AppHandle) {
    if let Some(listener) = app_handle.try_state::<instance_activation::ActivationListener>() {
        if let Err(error) = listener.resume() {
            eprintln!("实例激活监听未能恢复，重复启动将只提示已在运行：{error}");
        }
    }
}

#[cfg(not(test))]
fn finalize_app_shutdown(app_handle: &AppHandle, coordinator: &app_exit::AppExitCoordinator) {
    if !coordinator.begin_cleanup() {
        return;
    }
    let state = app_handle.state::<AppState>();
    if let Err(error) = state
        .document_store
        .flush(std::time::Duration::from_secs(30))
    {
        eprintln!("退出前落盘文档失败：{error}");
    }
    state.browser.shutdown_all();
    state.terminals.close_all();
    // A dev server is a detached child in its own job object, so nothing else in
    // this teardown reaches it. Leaving one behind would hold its port against the
    // next launch.
    state.preview_servers.stop_all();
    // Same reasoning, and one more: a language server holds an index lock on the
    // project it was started in, so one left behind makes the next launch's
    // server fail to start rather than merely leak.
    state.lsp_servers.stop_all();
    // The AI SDK sidecar is a resident Node process. Windows Job Objects only
    // cover forced parent termination, so normal exit must explicitly stop it.
    crate::aisdk::process::shutdown_sidecar();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
#[cfg(not(test))]
pub fn run() {
    // Before any thread exists: a development launcher hands over the PATH the
    // application should run with, which is not the toolchain-first PATH cargo
    // needed to build it.
    child_environment::restore_dev_application_path();
    let exit_coordinator = app_exit::AppExitCoordinator::default();
    let tray_exit_coordinator = exit_coordinator.clone();
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .on_page_load(|webview, payload| {
            if webview.label() != BROWSER_RENDERER_MOUNT_MAIN_LABEL {
                return;
            }
            match payload.event() {
                PageLoadEvent::Started => {
                    begin_main_browser_renderer_document_load(webview.app_handle())
                }
                PageLoadEvent::Finished => finish_main_browser_renderer_document_load(webview),
            }
        })
        .setup(move |app| {
            if app
                .get_webview_window(BROWSER_RENDERER_MOUNT_MAIN_LABEL)
                .is_some()
            {
                return Err(std::io::Error::other(
                    "主 WebView 在应用数据 authority 之前被创建，拒绝启动",
                )
                .into());
            }
            let path = document_path(app.handle()).map_err(std::io::Error::other)?;
            // A taken lease normally means a live Mework whose window was closed
            // into the tray: hand it this launch and leave quietly. A holder that
            // is still starting or already leaving is given a few seconds to
            // either answer or release the lease.
            let store = app.state::<AppState>().document_store.clone();
            match instance_activation::negotiate_launch(&path, PROCESS_LEASE_HANDOVER_WAIT, || {
                store.acquire_process_authority(&path)
            }) {
                instance_activation::LaunchHandover::Acquired => {}
                instance_activation::LaunchHandover::ResidentSignaled => std::process::exit(0),
                instance_activation::LaunchHandover::Unresolved(error) => {
                    // Report the conflict with a native dialog and exit cleanly
                    // before window creation.
                    use tauri_plugin_dialog::DialogExt;
                    let mut message = error.to_string();
                    if error == document_store::ProcessAuthorityError::Contended {
                        message.push_str(
                            "\n\nMework 可能仍在系统托盘中运行：请从托盘图标打开窗口，或选择「关闭 Mework」后再启动。",
                        );
                    }
                    app.dialog()
                        .message(message)
                        .title("无法启动 Mework")
                        .kind(tauri_plugin_dialog::MessageDialogKind::Warning)
                        .blocking_show();
                    std::process::exit(1);
                }
            }
            migrate_legacy_app_data(app.handle()).map_err(std::io::Error::other)?;
            // Installed before anything can execute a tool, so every card this
            // process issues is signed with the durable key rather than the
            // ephemeral placeholder.
            if let Some(app_data) = path.parent() {
                app.state::<AppState>()
                    .install_attestation_key(app_data)
                    .map_err(std::io::Error::other)?;
                // The two built-in prompt profiles live on disk as editable
                // files; a damaged one is reported, not fatal, because the
                // compiled copy still serves every run.
                if let Err(error) = prompt_profile_files::materialize_builtin_profiles(app_data) {
                    eprintln!("内置提示词档案未能落盘：{error}");
                }
                browser_file_preview::sweep_orphans(app_data);
            }
            reconcile_image_attachments_on_startup(app.handle()).map_err(std::io::Error::other)?;
            install_background_write_failure_reporting(app.state::<AppState>().inner());
            app.state::<AppState>()
                .browser
                .attach_app(app.handle().clone())
                .map_err(std::io::Error::other)?;
            let state = app.state::<AppState>();
            start_browser_renderer_mount_watchdog(state.inner()).map_err(std::io::Error::other)?;
            let main_window = app
                .config()
                .app
                .windows
                .iter()
                .find(|window| window.label == BROWSER_RENDERER_MOUNT_MAIN_LABEL)
                .cloned()
                .ok_or_else(|| std::io::Error::other("缺少 main WebView 配置"))?;
            if main_window.create {
                return Err(std::io::Error::other(
                    "main WebView 配置必须设置 create=false，确保 authority 先于 renderer",
                )
                .into());
            }
            state
                .document_store
                .mark_ipc_ready()
                .map_err(std::io::Error::other)?;
            // In a development build the trusted frontend is served over loopback
            // on whatever port the development server was able to take. Record it
            // before the WebView exists: both the page WebView's reservation and
            // the main window's own-origin test below read it, and a navigation
            // can arrive as soon as the window is built. A release build loads the
            // frontend from the custom protocol and records nothing.
            if tauri::is_dev() {
                if let Some(dev_url) = app.config().build.dev_url.as_ref() {
                    browser::install_app_dev_server(dev_url);
                }
            }
            tauri::WebviewWindowBuilder::from_config(app.handle(), &main_window)
                .map_err(std::io::Error::other)?
                .on_navigation(|url| {
                    match classify_main_window_navigation(url, browser::app_dev_server_origin()) {
                        MainWindowNavigation::LoadInWindow => true,
                        MainWindowNavigation::OpenExternally(target) => {
                            open_external_url_in_background(target);
                            false
                        }
                    }
                })
                .on_new_window(|url, _features| {
                    // `target="_blank"` and `window.open` reach this fallback
                    // after the renderer's normal `preventDefault` interception.
                    open_external_url_in_background(url.into());
                    tauri::webview::NewWindowResponse::Deny
                })
                .build()
                .map_err(std::io::Error::other)?;
            // The process outlives its window from here on: closing the window
            // hides it and the tray is the way back in or out. Without a tray
            // the close handler keeps quitting, so a failure here is only logged.
            let language = state
                .document_store
                .current_snapshot(&path)
                .map(|document| document.global_settings.resolved_app_language)
                .unwrap_or_default();
            let quit_coordinator = tray_exit_coordinator.clone();
            if let Err(error) = app_tray::install(
                app.handle(),
                BROWSER_RENDERER_MOUNT_MAIN_LABEL,
                language,
                move |app_handle| {
                    request_deferred_exit_with_barrier(
                        app_handle.clone(),
                        quit_coordinator.clone(),
                        0,
                        |_, code| code,
                    );
                },
            ) {
                eprintln!("系统托盘不可用，关闭窗口将直接退出应用：{error}");
            }
            // A second launch while this instance sits in the tray asks for the
            // window instead of failing on the app-data lease.
            let activation_app = app.handle().clone();
            match instance_activation::listen(&path, move || {
                let app_handle = activation_app.clone();
                if let Err(error) = activation_app.run_on_main_thread(move || {
                    app_tray::show_main_window(&app_handle, BROWSER_RENDERER_MOUNT_MAIN_LABEL);
                }) {
                    eprintln!("无法调度实例激活：{error}");
                }
            }) {
                Ok(listener) => {
                    app.manage(listener);
                }
                Err(error) => eprintln!("实例激活监听不可用，重复启动将只提示已在运行：{error}"),
            }
            Ok(())
        })
        .invoke_handler(move |invoke: tauri::ipc::Invoke<tauri::Wry>| {
            let ready = invoke
                .message
                .webview_ref()
                .state::<AppState>()
                .document_store
                .is_ipc_ready();
            if ready {
                dispatch_app_invoke(mework_app_commands!(app_invoke_handler), invoke)
            } else {
                invoke
                    .resolver
                    .reject("Mework 应用数据 authority 尚未就绪，拒绝 IPC");
                true
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building Mework");
    app.run(move |app_handle, event| {
        match event {
            tauri::RunEvent::WindowEvent {
                label,
                event: tauri::WindowEvent::CloseRequested { api, .. },
                ..
            } if label == BROWSER_RENDERER_MOUNT_MAIN_LABEL && !exit_coordinator.is_ready() => {
                // Retain the actual window as well as the process on a failed save.
                api.prevent_close();
                if app_tray::is_installed(app_handle) {
                    // With a tray the window is only a view on the running
                    // process; quitting is the tray menu's job.
                    app_tray::hide_main_window(app_handle, BROWSER_RENDERER_MOUNT_MAIN_LABEL);
                } else {
                    request_deferred_exit_with_barrier(
                        app_handle.clone(),
                        exit_coordinator.clone(),
                        0,
                        |_, code| code,
                    );
                }
            }
            tauri::RunEvent::ExitRequested { api, code, .. } if !exit_coordinator.is_ready() => {
                api.prevent_exit();
                request_deferred_exit_with_barrier(
                    app_handle.clone(),
                    exit_coordinator.clone(),
                    code.unwrap_or(0),
                    |_, code| code,
                );
            }
            tauri::RunEvent::Exit => finalize_app_shutdown(app_handle, &exit_coordinator),
            _ => {}
        }
    });
}

#[cfg(all(feature = "browser-dev", not(test)))]
pub fn run_browser_dev() -> i32 {
    // Same handoff as `run`, and for the same reason: `cargo run` builds and
    // executes under one environment, so only the application can split them.
    child_environment::restore_dev_application_path();
    browser_dev::run()
}
