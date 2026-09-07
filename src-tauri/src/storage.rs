use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::Write,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

use chrono::Utc;

use crate::{
    catalog::default_document,
    image_attachments::validate_image_list,
    model::{
        AgentDefinition, AgentDefinitionBinding, AgentDefinitionMemory, AgentDefinitionSource,
        AgentModelSelection, AppDocument, ContextItem, Conversation, ConversationPresetSettings,
        ConversationSettings, ForkModelBinding, ToolResult, Workspace, WorkspaceKind,
    },
    orchestration::parse_question,
    state::AppState,
};

/// Current persisted document schema version.
///
/// Documents at any other version are rejected rather than migrated: old data is
/// archived and rebuilt by the startup recovery path, or wiped explicitly via
/// `npm run reset:data`. There is no migration ladder and no compatibility shim;
/// adding one is a bug unless it is an adjacent-version in-place upgrade whose
/// newer schema only adds keys with serde defaults, reviewed as such, and naming
/// its source version as a literal. Fields removed in past schemas must not be
/// resurrected through serde defaults when this version changes.
pub const SCHEMA_VERSION: u32 = 2;
const TEMPORARY_WORKSPACE_ID: &str = "__temporary__";
const MAX_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;
const MAX_WEB_SEARCHES_PER_CALL: u32 = 99_999;
const MAX_AGENT_DEFINITIONS: usize = 256;
const MAX_RETAINED_AGENT_DEFINITION_IDENTITIES: usize = 4096;
const MAX_AGENT_DEFINITION_SOURCE_KEY_CHARS: usize = 256;
/// Limits prevent malformed documents from causing unbounded startup work.
const MAX_MCP_SERVERS: usize = 256;
const MAX_MCP_NAME_CHARS: usize = 64;
const MAX_MCP_TIMEOUT_SECONDS: u32 = 3600;
const MAX_MCP_ARGUMENTS: usize = 128;
const MAX_INSTALLED_SKILLS: usize = 1024;
const MAX_SKILL_FOLDER_CHARS: usize = 128;
const MAX_ENVIRONMENT_TOOLS: usize = 128;
const MAX_ENVIRONMENT_TOOL_NAME_CHARS: usize = 64;
const MAX_ENVIRONMENT_TOOL_ARGUMENTS: usize = 8;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn load_or_initialize(path: &Path, default_workspace: &Path) -> Result<AppDocument, String> {
    if !path.exists() {
        let store = crate::conversation_store::store_for(path)?;
        let has_existing_conversations = store
            .conversation_workspaces()
            .map(|owners| !owners.is_empty())
            .unwrap_or(false);
        let mut document = default_document(default_workspace);
        if has_existing_conversations {
            // A recovery conversation needs an ID that cannot collide with stored history.
            if let Some(conversation) = document
                .workspaces
                .first_mut()
                .and_then(|workspace| workspace.conversations.first_mut())
            {
                conversation.id = format!("conv_recovery_{}", uuid::Uuid::new_v4().simple());
                conversation.title = "数据恢复".into();
                conversation.contexts = vec![ContextItem::System {
                    id: format!("ctx_recovery_{}", uuid::Uuid::new_v4().simple()),
                    content: "数据锚文件缺失；已重建锚，并按对话库里记录的工作区绑定收养现存对话（原工作区不存在时移入临时工作区）。".into(),
                    local_only: true,
                    hook_execution: None,
                    created_at: Utc::now()
                        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                }];
            }
        }
        seed_conversations(&store, &document)?;
        save_unchecked(path, &document)?;
        return read_document(path);
    }
    read_document(path).map_err(|load_error| {
        let diagnostic = match preserve_corrupt_document(path) {
            Ok(()) => "；已另外复制诊断副本",
            Err(_) => "；无法复制诊断副本",
        };
        format!("文档加载失败（{load_error}）；原文件未修改{diagnostic}")
    })
}

/// Seeds the conversation store only for initialization and recovery.
fn seed_conversations(
    store: &crate::conversation_store::ConversationStore,
    document: &AppDocument,
) -> Result<(), String> {
    for workspace in &document.workspaces {
        for conversation in &workspace.conversations {
            store.put_conversation(&workspace.id, conversation)?;
        }
        let ids = workspace
            .conversations
            .iter()
            .map(|conversation| conversation.id.clone())
            .collect::<Vec<_>>();
        store.set_workspace_order(&workspace.id, &ids)?;
    }
    Ok(())
}

/// Loads at startup, rebuilding from a seed document after banking any failed load.
pub fn load_or_recover(path: &Path, default_workspace: &Path) -> Result<AppDocument, String> {
    let load_error = match load_or_initialize(path, default_workspace) {
        Ok(mut document) => {
            // Startup cannot have an in-flight run; mark an unanswered trailing user message.
            let markers = mark_orphaned_trailing_user_contexts(&mut document);
            if !markers.is_empty() {
                match crate::conversation_store::store_for(path) {
                    Ok(store) => {
                        for (conversation_id, marker) in &markers {
                            if let Err(error) = store.upsert_contexts(
                                conversation_id,
                                std::slice::from_ref(marker),
                                crate::conversation_store::ContextStatus::Settled,
                            ) {
                                eprintln!("孤儿消息标记未能落库：{error}");
                            }
                        }
                    }
                    Err(error) => eprintln!("孤儿消息标记未能落库：{error}"),
                }
            }
            return Ok(document);
        }
        Err(error) => error,
    };
    let timestamp = Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
    let banked = path.with_file_name(format!("document.v1.rejected-{timestamp}.json"));
    if let Err(error) = fs::rename(path, &banked) {
        // Rebuilding is unsafe unless the failed anchor can be banked.
        return Err(format!(
            "文档加载失败（{load_error}），且无法封存原文件以重建：{error}"
        ));
    }
    let mut document = default_document(default_workspace);
    let notice = format!(
        "应用数据锚文件无法加载（{load_error}），已封存为 {} 并以初始数据重建。\
         原有对话仍在对话库里，已按其记录的工作区绑定收养（原工作区不存在时移入临时工作区）。",
        banked.display()
    );
    eprintln!("{notice}");
    // Keep historical conversations and add a uniquely identified recovery notice.
    let mut notice_conversation = document
        .workspaces
        .first()
        .and_then(|workspace| workspace.conversations.first())
        .cloned();
    for workspace in &mut document.workspaces {
        workspace.conversations.clear();
    }
    if let Some(conversation) = notice_conversation.as_mut() {
        conversation.id = format!("conv_recovery_{}", uuid::Uuid::new_v4().simple());
        conversation.title = "数据恢复".into();
        conversation.contexts = vec![ContextItem::System {
            id: format!("ctx_recovery_{}", uuid::Uuid::new_v4().simple()),
            content: notice,
            local_only: true,
            hook_execution: None,
            created_at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        }];
        conversation.branches.clear();
        conversation.queued_messages.clear();
        conversation.user_aborted_tasks.clear();
        if let Some(workspace) = document.workspaces.first_mut() {
            workspace.conversations.push(conversation.clone());
        }
    }
    let store = crate::conversation_store::store_for(path)?;
    seed_conversations(&store, &document)?;
    save_unchecked(path, &document)?;
    read_document(path)
}

pub fn read_document(path: &Path) -> Result<AppDocument, String> {
    let file = File::open(path).map_err(|error| format!("无法打开数据文档: {error}"))?;
    if file
        .metadata()
        .map_err(|error| format!("无法读取数据文档元数据: {error}"))?
        .len()
        > MAX_DOCUMENT_BYTES as u64
    {
        return Err(format!(
            "数据文档超过 {} MiB 限制",
            MAX_DOCUMENT_BYTES / 1024 / 1024
        ));
    }
    let mut value: serde_json::Value =
        serde_json::from_reader(file).map_err(|error| format!("数据文档 JSON 无效: {error}"))?;
    let original_schema = value
        .get("schemaVersion")
        .and_then(serde_json::Value::as_u64)
        .and_then(|version| u32::try_from(version).ok())
        .unwrap_or_default();
    if original_schema > SCHEMA_VERSION {
        return Err(format!(
            "数据由更新版本写入（schema {original_schema}），当前仅支持 schema {SCHEMA_VERSION}"
        ));
    }
    // The accepted older format adds only a host-owned SQLite fork-intent table.
    // Its anchor configuration remains unchanged, so preserve it in place.
    if original_schema == 1 {
        value["schemaVersion"] = serde_json::json!(SCHEMA_VERSION);
    } else if original_schema < SCHEMA_VERSION {
        return Err(format!(
            "数据由旧版 schema {original_schema} 写入，历史迁移已在未发布阶段删除，当前仅支持 schema {SCHEMA_VERSION}"
        ));
    }
    // Runs before the anchor is parsed, unconditionally: neither `ReasoningContent`
    // nor `ModelCapability` can still read every value older archives wrote, and a
    // retired capability slug fails the whole document rather than one field.
    migrate_persisted_models(&mut value);
    let mut document = assemble_layout(path, value)?;
    canonicalize_temporary_workspace(&mut document);
    canonicalize_provider_model_ids(&mut document);
    // The document on disk was written in canonical number form, but reading
    // it back is itself a parse, and this crate's float parser is not exact on
    // every literal it accepts. Re-canonicalizing makes the loaded document
    // equal to the one that was saved, which is what the unchanged-card fast
    // path in `validate_tool_results` compares against.
    canonicalize_tool_payload_numbers(&mut document);
    isolate_invalid_loaded_conversations(&mut document);
    validate_shape(&document)?;
    prime_layout_write_cache(path);
    Ok(document)
}

fn canonicalize_temporary_workspace(document: &mut AppDocument) {
    let mut migrated_conversations = Vec::new();
    document.workspaces.retain_mut(|workspace| {
        let retired = workspace.kind == WorkspaceKind::Unsupported
            || (workspace.id.starts_with("__") && workspace.id != TEMPORARY_WORKSPACE_ID);
        if retired {
            migrated_conversations.append(&mut workspace.conversations);
        }
        !retired
    });

    let temporary_index = document
        .workspaces
        .iter()
        .position(|workspace| workspace.id == TEMPORARY_WORKSPACE_ID);

    if temporary_index.is_none() {
        document.workspaces.push(Workspace {
            id: TEMPORARY_WORKSPACE_ID.into(),
            name: "临时工作区".into(),
            kind: WorkspaceKind::Temporary,
            path: String::new(),
            created_at: Utc::now().to_rfc3339(),
            default_conversation_preset_id: String::new(),
            last_conversation_settings: None,
            conversations: Vec::new(),
        });
    }

    let temporary = document
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id == TEMPORARY_WORKSPACE_ID)
        .expect("temporary workspace is inserted above");
    temporary.name = "临时工作区".into();
    temporary.kind = WorkspaceKind::Temporary;
    temporary.path.clear();
    temporary.conversations.append(&mut migrated_conversations);
}

/// Test-only convenience: production code writes conversations through the
/// conversation commands (`crate::conversations`) and the anchor through the
/// document store. This helper does both in one call so a test can assert a
/// full round trip without standing up the command layer.
/// Test-only convenience: production code writes conversations through the
/// conversation commands (`crate::conversations`) and the anchor through the
/// document store. This helper does both in one call so a test can assert a
/// full round trip without standing up the command layer.
#[cfg(test)]
fn save_all(path: &Path, document: &AppDocument) -> Result<(), String> {
    let store = crate::conversation_store::store_for(path)?;
    let live = document
        .workspaces
        .iter()
        .flat_map(|workspace| workspace.conversations.iter())
        .map(|conversation| conversation.id.clone())
        .collect::<HashSet<_>>();
    for (id, _) in store.conversation_workspaces()? {
        if !live.contains(&id) {
            store.delete_conversation(&id)?;
        }
    }
    seed_conversations(&store, document)?;
    save_unchecked(path, document)
}

#[cfg(test)]
pub fn validate_and_save(
    path: &Path,
    previous: &AppDocument,
    document: &AppDocument,
    state: &AppState,
) -> Result<(), String> {
    save_all(path, document)?;
    let store = crate::conversation_store::store_for(path)?;
    // Read back the just-seeded conversations as the authoritative baseline.
    let mut authority = previous.clone();
    for workspace in &mut authority.workspaces {
        workspace.conversations = store.workspace_conversations(&workspace.id)?;
    }
    let PreparedSaveTransition {
        document: canonical, ..
    } = prepare_save_transition(&authority, document, state)?;
    save_unchecked(path, &canonical)?;
    Ok(())
}

pub(crate) struct PreparedSaveTransition {
    pub(crate) document: AppDocument,
    /// Tool cards that could not be attested and were replaced with markers so
    /// the save could proceed. Empty on an ordinary save; the renderer is told
    /// about anything here so the loss is visible rather than silent.
    pub(crate) quarantined: Vec<UnattestedTool>,
}

/// Read-only validation helper for the transition tests below. Production
/// lifecycle code validates a conversation at its own write command
/// (`crate::conversations`) and the configuration domains here; this helper
/// runs both against one proposed document so a test can assert either.
#[cfg(test)]
pub fn validate_save_transition(
    previous: &AppDocument,
    document: &AppDocument,
    state: &AppState,
) -> Result<AppDocument, String> {
    let mut proposal = document.clone();
    for index in 0..proposal.workspaces.len() {
        let workspace_id = proposal.workspaces[index].id.clone();
        let mut conversations = std::mem::take(&mut proposal.workspaces[index].conversations);
        for conversation in &mut conversations {
            validate_incoming_conversation(previous, &workspace_id, conversation, state)?;
        }
        proposal.workspaces[index].conversations = conversations;
    }
    let PreparedSaveTransition {
        document: mut canonical,
        ..
    } = prepare_save_transition(previous, &proposal, state)?;
    // Return the individually validated proposal for this test helper.
    for workspace in &mut canonical.workspaces {
        if let Some(proposed) = proposal
            .workspaces
            .iter()
            .find(|candidate| candidate.id == workspace.id)
        {
            workspace.conversations = proposed.conversations.clone();
        }
    }
    Ok(canonical)
}

pub(crate) fn prepare_save_transition(
    previous: &AppDocument,
    document: &AppDocument,
    state: &AppState,
) -> Result<PreparedSaveTransition, String> {
    let mut canonical = document.clone();
    canonicalize_provider_model_ids(&mut canonical);
    canonicalize_temporary_workspace(&mut canonical);
    canonicalize_tool_payload_numbers(&mut canonical);
    canonicalize_context_timestamps(&mut canonical);
    // Conversation bodies are host-authoritative; retain only renderer-owned configuration.
    adopt_authoritative_conversations(previous, &mut canonical);
    // Validate the renderer's proposed shapes before replacing every
    // renderer-owned revision/epoch field with host-derived authority.
    validate_agent_definitions(&canonical)?;
    canonicalize_renderer_agent_definitions(previous, &mut canonical)?;
    validate_shape(&canonical)?;
    validate_agent_definition_transition(previous, &canonical)?;
    let unattested = validate_tool_results_isolated(previous, &canonical, state);
    let quarantined = quarantine_unattested_tools(&mut canonical, &unattested);
    validate_workspace_authorizations(previous, &canonical, state)?;
    Ok(PreparedSaveTransition {
        document: canonical,
        quarantined,
    })
}

/// Uses host-authoritative conversation bodies to prevent stale renderer snapshots from overwriting output.
fn adopt_authoritative_conversations(previous: &AppDocument, canonical: &mut AppDocument) {
    let authoritative = previous
        .workspaces
        .iter()
        .map(|workspace| (workspace.id.as_str(), &workspace.conversations))
        .collect::<HashMap<_, _>>();
    for workspace in &mut canonical.workspaces {
        workspace.conversations = authoritative
            .get(workspace.id.as_str())
            .map(|conversations| (*conversations).clone())
            .unwrap_or_default();
    }
    drop_retired_enabled_tools(canonical);
}

/// Drops enabled-tool names that no longer exist in the catalog so archived conversations remain writable.
fn drop_retired_enabled_tools(canonical: &mut AppDocument) {
    let tool_names = canonical
        .tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<HashSet<_>>();
    for workspace in &mut canonical.workspaces {
        for conversation in &mut workspace.conversations {
            conversation
                .settings
                .enabled_tools
                .retain(|name| tool_names.contains(name));
        }
    }
}

/// Replaces each unattestable tool card with a visible local marker so the rest
/// of the document can still be written.
///
/// A card reaches this point when its payload no longer matches anything the
/// host attested — the renderer altered it, the process restarted and took the
/// receipt with it, or the receipt aged out of the book. Before, that rejected
/// the entire save, and because the card stayed in the renderer's document
/// every later save failed the same way: one stale card and nothing could be
/// written again, in any conversation.
///
/// The card is not silently deleted. It becomes a `system` context that says
/// what was dropped and why, marked `local_only` so it never enters model
/// input — the user sees a gap they can explain rather than one that just
/// happens. Dropping the tool card is safe for the document's other
/// invariants: branch fork points address user contexts, and nothing else
/// references a tool card by id.
fn quarantine_unattested_tools(
    document: &mut AppDocument,
    unattested: &[UnattestedTool],
) -> Vec<UnattestedTool> {
    if unattested.is_empty() {
        return Vec::new();
    }
    let mut quarantined = Vec::new();
    for entry in unattested {
        let Some(workspace) = document
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == entry.workspace_id)
        else {
            continue;
        };
        let Some(conversation) = workspace
            .conversations
            .iter_mut()
            .find(|conversation| conversation.id == entry.conversation_id)
        else {
            continue;
        };
        let mut replaced = replace_tool_with_marker(&mut conversation.contexts, entry);
        for branch in &mut conversation.branches {
            replaced |= replace_tool_with_marker(&mut branch.contexts, entry);
        }
        if replaced {
            quarantined.push(UnattestedTool {
                workspace_id: entry.workspace_id.clone(),
                conversation_id: entry.conversation_id.clone(),
                context_id: entry.context_id.clone(),
                tool_name: entry.tool_name.clone(),
            });
        }
    }
    quarantined
}

/// Swaps one tool card for its marker, keeping the card's position and id so
/// the timeline reads in order and the renderer's next save carries the marker
/// forward instead of the card.
fn replace_tool_with_marker(contexts: &mut Vec<ContextItem>, entry: &UnattestedTool) -> bool {
    let Some(index) = contexts.iter().position(
        |context| matches!(context, ContextItem::Tool { id, .. } if id == &entry.context_id),
    ) else {
        return false;
    };
    let created_at = match &contexts[index] {
        ContextItem::Tool { created_at, .. } => created_at.clone(),
        _ => Utc::now().to_rfc3339(),
    };
    contexts[index] = ContextItem::System {
        id: entry.context_id.clone(),
        content: format!(
            "工具调用 {} 的结果无法确认来自本次后端执行，已从记录中移除以便继续保存。\
             这通常是因为它在保存前被改动，或应用重启后回执已不在内存中。",
            entry.tool_name
        ),
        local_only: true,
        hook_execution: None,
        created_at,
    };
    true
}

/// Puts every tool payload back into the number form the host attested.
///
/// A tool card is built here, crosses to the renderer as JSON, spends its life
/// as JavaScript objects, and comes back on save. JavaScript has only `f64`,
/// so a provider's `1.0` returns as `1` and an integer past 2^53 returns
/// rounded. Attestation compares serialized payloads, so without this the card
/// the renderer hands back is not the card the host signed — and since the
/// comparison is exact, one such number makes that card permanently
/// unsaveable. Normalizing both sides onto the form that survives the trip
/// makes the comparison meaningful again.
///
/// This runs before validation on purpose: it must be what gets attested and
/// what gets written, or the next save would have to redo it.
fn canonicalize_tool_payload_numbers(document: &mut AppDocument) {
    fn walk(contexts: &mut [ContextItem]) {
        for context in contexts {
            let ContextItem::Tool {
                requested_input,
                input,
                subagent,
                ..
            } = context
            else {
                continue;
            };
            crate::model::canonicalize_object_numbers(input);
            if let Some(requested_input) = requested_input {
                crate::model::canonicalize_object_numbers(requested_input);
            }
            // A child transcript is committed by the outer card's fingerprint,
            // so its payloads have to be canonical too or the outer card stops
            // matching for a reason nothing about it explains.
            if let Some(subagent) = subagent {
                walk(&mut subagent.contexts);
            }
        }
    }

    for workspace in &mut document.workspaces {
        for conversation in &mut workspace.conversations {
            walk(&mut conversation.contexts);
            for branch in &mut conversation.branches {
                walk(&mut branch.contexts);
            }
        }
    }
}

/// Rewrites every persisted model into a shape the current enums can parse.
///
/// Two model vocabularies shrank, and they fail differently on read:
///
/// - `reasoningContent` used to be optional with an `auto` variant that a request
///   resolved by provider family. Both are gone, so an archive that omits the key
///   — or still carries `"auto"` — would otherwise load as plaintext and silently
///   change what Responses-family models put on the wire.
/// - `capabilities` used to carry eight slugs and now carries one. A retired slug
///   is an unknown enum *variant*, not an unknown field, so serde rejects the whole
///   document rather than skipping the entry — every pre-existing profile would be
///   quarantined and rebuilt empty.
///
/// This runs on the raw JSON because the enums can no longer parse the retired
/// values, on every load rather than on a schema-version edge, and rewrites
/// nothing that already holds a surviving value.
fn migrate_persisted_models(value: &mut serde_json::Value) {
    let Some(providers) = value
        .pointer_mut("/assets/apiProviders")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    for provider in providers {
        // Single source of truth for which families return ciphertext.
        let encrypted = provider
            .get("family")
            .cloned()
            .and_then(|family| serde_json::from_value::<crate::model::ProviderFamily>(family).ok())
            .is_some_and(crate::model::ProviderFamily::reasoning_content_takes_effect);
        let Some(models) = provider
            .get_mut("models")
            .and_then(serde_json::Value::as_array_mut)
        else {
            continue;
        };
        for model in models {
            let Some(model) = model.as_object_mut() else {
                continue;
            };
            // Ask serde which slugs still exist rather than naming the survivors
            // here, so a later catalog change cannot leave this filter behind.
            if let Some(capabilities) = model
                .get_mut("capabilities")
                .and_then(serde_json::Value::as_array_mut)
            {
                capabilities.retain(|capability| {
                    serde_json::from_value::<crate::model::ModelCapability>(capability.clone())
                        .is_ok()
                });
            }
            if matches!(
                model.get("reasoningContent").and_then(serde_json::Value::as_str),
                Some("plaintext" | "encrypted")
            ) {
                continue;
            }
            let resolved = if encrypted { "encrypted" } else { "plaintext" };
            model.insert("reasoningContent".into(), serde_json::json!(resolved));
        }
    }
}

fn canonicalize_provider_model_ids(document: &mut AppDocument) {
    document.global_settings.active_provider_id = document
        .global_settings
        .active_provider_id
        .take()
        .map(|id| id.trim().to_owned());
    for provider in &mut document.assets.api_providers {
        provider.id = provider.id.trim().to_owned();
        provider.active_model_id = provider
            .active_model_id
            .take()
            .map(|id| id.trim().to_owned());
        for model in &mut provider.models {
            model.id = model.id.trim().to_owned();
        }
    }
}

fn validate_workspace_authorizations(
    previous: &AppDocument,
    document: &AppDocument,
    state: &AppState,
) -> Result<(), String> {
    let previous_exact = previous
        .workspaces
        .iter()
        .filter(|workspace| workspace.kind == WorkspaceKind::Directory)
        .map(|workspace| workspace.path.as_str())
        .collect::<HashSet<_>>();
    let previous_canonical = previous
        .workspaces
        .iter()
        .filter(|workspace| workspace.kind == WorkspaceKind::Directory)
        .filter_map(|workspace| AppState::workspace_key(Path::new(&workspace.path)))
        .collect::<HashSet<_>>();

    for workspace in &document.workspaces {
        if workspace.kind != WorkspaceKind::Directory {
            continue;
        }
        if previous_exact.contains(workspace.path.as_str()) {
            continue;
        }
        if AppState::workspace_key(Path::new(&workspace.path))
            .is_some_and(|key| previous_canonical.contains(&key))
        {
            continue;
        }
        state
            .require_workspace_authorization(Path::new(&workspace.path))
            .map_err(|error| format!("工作区 {} 未获授权: {error}", workspace.id))?;
    }

    Ok(())
}

// Anchor layout: configuration stays in the JSON anchor while the conversation store owns bodies, membership, and order.

/// Anchor representation of a workspace without conversations.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedWorkspaceShell {
    id: String,
    name: String,
    #[serde(default)]
    kind: WorkspaceKind,
    path: String,
    created_at: String,
    #[serde(default)]
    default_conversation_preset_id: String,
    #[serde(default)]
    last_conversation_settings: Option<ConversationSettings>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedAnchor {
    schema_version: u32,
    global_settings: crate::model::GlobalSettings,
    #[serde(default)]
    assets: crate::model::AssetLibrary,
    #[serde(default)]
    presets: crate::model::PresetLibrary,
    workspaces: Vec<PersistedWorkspaceShell>,
    tools: Vec<crate::model::ToolDescriptor>,
    capabilities: crate::model::CapabilityCatalog,
}

/// Validates the conversation ID used as both store key and filename-safe identifier.
fn validate_conversation_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-')
    {
        return Err(format!(
            "对话 ID 必须是可作文件名的小写 ASCII 字母、数字、下划线或连字符: {id}"
        ));
    }
    Ok(())
}

/// Assembles an in-memory document from the anchor and conversation store.
///
/// Conversations with missing workspace owners move to the temporary workspace; malformed rows are isolated.
fn assemble_layout(path: &Path, value: serde_json::Value) -> Result<AppDocument, String> {
    let anchor: PersistedAnchor = serde_json::from_value(value)
        .map_err(|error| format!("数据文档 JSON 无效: {error}"))?;
    let store = crate::conversation_store::store_for(path)?;

    let mut seen = HashSet::new();
    let mut workspaces = Vec::with_capacity(anchor.workspaces.len());
    for shell in anchor.workspaces {
        let conversations = match store.workspace_conversations(&shell.id) {
            Ok(conversations) => conversations,
            Err(error) => {
                eprintln!("工作区 {} 的对话无法装配（{error}），本次按空列表处理", shell.id);
                Vec::new()
            }
        };
        for conversation in &conversations {
            seen.insert(conversation.id.clone());
        }
        workspaces.push(Workspace {
            id: shell.id,
            name: shell.name,
            kind: shell.kind,
            path: shell.path,
            created_at: shell.created_at,
            default_conversation_preset_id: shell.default_conversation_preset_id,
            last_conversation_settings: shell.last_conversation_settings,
            conversations,
        });
    }

    let mut document = AppDocument {
        schema_version: anchor.schema_version,
        global_settings: anchor.global_settings,
        assets: anchor.assets,
        presets: anchor.presets,
        workspaces,
        tools: anchor.tools,
        capabilities: anchor.capabilities,
    };

    adopt_unowned_conversations(&store, &mut document, &seen);
    Ok(document)
}

/// Adopts store conversations whose workspace owner is absent into the temporary workspace.
fn adopt_unowned_conversations(
    store: &crate::conversation_store::ConversationStore,
    document: &mut AppDocument,
    seen: &HashSet<String>,
) {
    let Ok(owners) = store.conversation_workspaces() else {
        return;
    };
    let mut orphans = owners
        .into_iter()
        .filter(|(id, _)| !seen.contains(id))
        .collect::<Vec<_>>();
    if orphans.is_empty() {
        return;
    }
    orphans.sort();
    canonicalize_temporary_workspace(document);
    for (id, workspace_id) in orphans {
        let Ok(Some(conversation)) = store.conversation(&id) else {
            eprintln!("对话 {id} 的正文无法装配，已跳过");
            continue;
        };
        eprintln!("对话 {id} 不属于任何现存工作区，已按其记录的工作区绑定收养");
        let target_index = document
            .workspaces
            .iter()
            .position(|workspace| workspace.id == workspace_id)
            .or_else(|| {
                document
                    .workspaces
                    .iter()
                    .position(|workspace| workspace.id == TEMPORARY_WORKSPACE_ID)
            });
        if let Some(index) = target_index {
            document.workspaces[index].conversations.push(conversation);
        }
    }
}

/// Normalizes context and queued-message timestamps to RFC3339 UTC milliseconds without changing MAC-covered fields.
fn canonicalize_context_timestamps(document: &mut AppDocument) {
    fn canonical(value: &mut String) {
        if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(value) {
            *value = parsed
                .with_timezone(&Utc)
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        }
    }
    fn canonicalize_contexts(contexts: &mut [ContextItem]) {
        for context in contexts {
            match context {
                ContextItem::System { created_at, .. }
                | ContextItem::User { created_at, .. }
                | ContextItem::Assistant { created_at, .. }
                | ContextItem::Reasoning { created_at, .. }
                | ContextItem::Tool { created_at, .. } => canonical(created_at),
            }
        }
    }
    for workspace in &mut document.workspaces {
        for conversation in &mut workspace.conversations {
            canonicalize_contexts(&mut conversation.contexts);
            for branch in &mut conversation.branches {
                canonicalize_contexts(&mut branch.contexts);
            }
            for message in &mut conversation.queued_messages {
                canonical(&mut message.created_at);
            }
        }
    }
}

/// Marks unanswered trailing user contexts during startup because no run can be in flight.
fn mark_orphaned_trailing_user_contexts(
    document: &mut AppDocument,
) -> Vec<(String, ContextItem)> {
    let mut marked = Vec::new();
    for workspace in &mut document.workspaces {
        for conversation in &mut workspace.conversations {
            if matches!(conversation.contexts.last(), Some(ContextItem::User { .. })) {
                let marker = ContextItem::System {
                    id: format!("ctx_orphan_{}", uuid::Uuid::new_v4().simple()),
                    content: "上一次会话在此中断，上面这条消息尚未得到回答。".into(),
                    local_only: true,
                    hook_execution: None,
                    created_at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                };
                conversation.contexts.push(marker.clone());
                marked.push((conversation.id.clone(), marker));
            }
        }
    }
    marked
}

/// Isolates malformed conversations after assembly while preserving their stored rows.
fn isolate_invalid_loaded_conversations(document: &mut AppDocument) {
    let tool_names_owned = document
        .tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<Vec<_>>();
    let tool_names = tool_names_owned
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let mut failures = Vec::new();
    for (workspace_index, workspace) in document.workspaces.iter().enumerate() {
        for (conversation_index, conversation) in workspace.conversations.iter().enumerate() {
            let result = validate_conversation_shape(conversation, &tool_names).and_then(|()| {
                validate_agent_definition_list(
                    &format!("对话 {}", conversation.id),
                    &conversation.settings.agent_definitions,
                )
            });
            if let Err(error) = result {
                failures.push((
                    workspace_index,
                    conversation_index,
                    conversation.id.clone(),
                    error,
                ));
            }
        }
    }
    for (workspace_index, conversation_index, conversation_id, error) in failures.into_iter().rev()
    {
        document.workspaces[workspace_index]
            .conversations
            .remove(conversation_index);
        eprintln!("对话 {conversation_id} 未通过装载校验（{error}），本次不装配");
    }
}

/// Primes the anchor content-fingerprint cache.
fn prime_layout_write_cache(path: &Path) {
    let mut prints = HashMap::new();
    if let Ok(bytes) = fs::read(path) {
        prints.insert(String::new(), content_fingerprint(&bytes));
    }
    let mut guard = LAYOUT_WRITE_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard
        .get_or_insert_with(HashMap::new)
        .insert(path.to_owned(), prints);
}

/// Per-anchor fingerprints of the last successful write. The process owns the application-data lease.
static LAYOUT_WRITE_CACHE: std::sync::Mutex<Option<HashMap<std::path::PathBuf, HashMap<String, u64>>>> =
    std::sync::Mutex::new(None);

fn content_fingerprint(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// Writes only the configuration anchor; conversation bodies are persisted by `conversation_store`.
pub fn save_unchecked(path: &Path, document: &AppDocument) -> Result<(), String> {
    let anchor = PersistedAnchor {
        schema_version: document.schema_version,
        global_settings: document.global_settings.clone(),
        assets: document.assets.clone(),
        presets: document.presets.clone(),
        workspaces: document
            .workspaces
            .iter()
            .map(|workspace| PersistedWorkspaceShell {
                id: workspace.id.clone(),
                name: workspace.name.clone(),
                kind: workspace.kind,
                path: workspace.path.clone(),
                created_at: workspace.created_at.clone(),
                default_conversation_preset_id: workspace.default_conversation_preset_id.clone(),
                last_conversation_settings: workspace.last_conversation_settings.clone(),
            })
            .collect(),
        tools: document.tools.clone(),
        capabilities: document.capabilities.clone(),
    };
    let anchor_bytes = serde_json::to_vec_pretty(&anchor)
        .map_err(|error| format!("无法序列化数据文档: {error}"))?;
    if anchor_bytes.len() > MAX_DOCUMENT_BYTES {
        return Err(format!(
            "数据文档超过 {} MiB 限制",
            MAX_DOCUMENT_BYTES / 1024 / 1024
        ));
    }
    let anchor_print = content_fingerprint(&anchor_bytes);
    let mut cache_guard = LAYOUT_WRITE_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let cache = cache_guard
        .get_or_insert_with(HashMap::new)
        .entry(path.to_owned())
        .or_default();
    if cache.get("").copied() == Some(anchor_print) && path.exists() {
        return Ok(());
    }
    match atomic_write(path, &anchor_bytes) {
        Ok(()) => {
            cache.insert(String::new(), anchor_print);
            Ok(())
        }
        Err(error) => Err(format!("设置锚写入失败：{error}")),
    }
}

/// Deletes all conversation data for a full reset without affecting banked or diagnostic copies.
pub fn purge_conversation_bodies(path: &Path) -> Result<(), String> {
    if let Some(caches) = LAYOUT_WRITE_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_mut()
    {
        caches.remove(path);
    }
    // Close the store before deleting files: Windows handles block deletion and live connections can recreate WAL frames.
    crate::conversation_store::close_store_for(path);
    let database = crate::conversation_store::database_path(path);
    for suffix in ["", "-wal", "-shm"] {
        let mut candidate = database.as_os_str().to_owned();
        candidate.push(suffix);
        let candidate = std::path::PathBuf::from(candidate);
        if candidate.exists() {
            fs::remove_file(&candidate).map_err(|error| {
                format!("重置时无法删除对话库 {}：{error}", candidate.display())
            })?;
        }
    }
    // Reset the independent token ledger as well.
    crate::token_ledger::close_store_for(path);
    let ledger = crate::token_ledger::database_path(path);
    for suffix in ["", "-wal", "-shm"] {
        let mut candidate = ledger.as_os_str().to_owned();
        candidate.push(suffix);
        let candidate = std::path::PathBuf::from(candidate);
        if candidate.exists() {
            fs::remove_file(&candidate).map_err(|error| {
                format!("重置时无法删除 token 流水账 {}：{error}", candidate.display())
            })?;
        }
    }
    // Remove the obsolete per-conversation directory during a full reset.
    if let Some(parent) = path.parent() {
        let legacy = parent.join("conversations");
        if legacy.is_dir() {
            let _ = fs::remove_dir_all(&legacy);
        }
    }
    Ok(())
}

pub fn validate_shape(document: &AppDocument) -> Result<(), String> {
    if document.schema_version != SCHEMA_VERSION {
        return Err(format!(
            "不支持 schemaVersion {}，当前仅支持 {}",
            document.schema_version, SCHEMA_VERSION
        ));
    }

    let tool_names = unique_nonempty(
        document.tools.iter().map(|tool| tool.name.as_str()),
        "工具名称",
    )?;
    if tool_names.is_empty() {
        return Err("工具目录不能为空".into());
    }
    let provider_ids = unique_trimmed_nonempty(
        document
            .assets.api_providers
            .iter()
            .map(|provider| provider.id.as_str()),
        "API 提供商 ID",
    )?;
    for provider in &document.assets.api_providers {
        // This namespace is reserved because search-provider credentials are keyed only by provider ID.
        if provider
            .id
            .trim()
            .starts_with(crate::web_search::SEARCH_PROVIDER_ID_PREFIX)
        {
            return Err(format!(
                "API 提供商 ID 不能占用搜索提供商保留命名空间：{}",
                provider.id
            ));
        }
        let model_ids = unique_trimmed_nonempty(
            provider.models.iter().map(|model| model.id.as_str()),
            "模型 ID",
        )?;
        for model in &provider.models {
            crate::model::validate_model_id(&model.id).map_err(|error| {
                format!("提供商 {} 的模型 ID 无效: {error}", provider.id)
            })?;
        }
        // Endpoint overrides must satisfy the same URL safety rules as `base_url`.
        for (endpoint, base_url) in &provider.endpoint_base_urls {
            if base_url.trim().is_empty() {
                continue;
            }
            crate::api::validate_base_url(base_url).map_err(|error| {
                format!(
                    "提供商 {} 的 {} 端点地址无效: {error}",
                    provider.id,
                    endpoint.slug()
                )
            })?;
        }
        if let Some(active_model_id) = &provider.active_model_id {
            if active_model_id.trim().is_empty() {
                return Err(format!("提供商 {} 的当前模型 ID 不能为空", provider.id));
            }
            if !model_ids.contains(active_model_id.trim()) {
                return Err(format!(
                    "提供商 {} 的当前模型不存在: {active_model_id}",
                    provider.id
                ));
            }
        }
    }
    validate_agent_definitions(document)?;
    if let Some(active_provider_id) = &document.global_settings.active_provider_id {
        if active_provider_id.trim().is_empty() {
            return Err("当前 API 提供商 ID 不能为空".into());
        }
        if !provider_ids.contains(active_provider_id.trim()) {
            return Err(format!("当前 API 提供商不存在: {active_provider_id}"));
        }
    }
    validate_web_search_assets(&document.assets.web_search)?;
    validate_mcp_servers(&document.assets.mcp_servers)?;
    validate_skill_records(&document.assets.skills)?;
    validate_execution_environments(&document.assets.execution_environments)?;
    validate_environment_tools(&document.global_settings.environment_tools)?;
    let conversation_preset_ids = unique_nonempty(
        document
            .presets.conversation_presets
            .iter()
            .map(|preset| preset.id.as_str()),
        "对话预设 ID",
    )?;
    if conversation_preset_ids.is_empty()
        && !document
            .presets.default_conversation_preset_id
            .is_empty()
    {
        return Err("没有对话预设时，新对话默认预设 ID 必须为空".into());
    }
    if !conversation_preset_ids.is_empty()
        && !conversation_preset_ids.contains(
            document
                .presets.default_conversation_preset_id
                .as_str(),
        )
    {
        return Err(format!(
            "新对话默认预设不存在: {}",
            document.presets.default_conversation_preset_id
        ));
    }
    for preset in &document.presets.conversation_presets {
        validate_conversation_preset_settings(
            &format!("对话预设 {}", preset.id),
            &preset.settings,
            &tool_names,
        )?;
    }

    unique_nonempty(
        document
            .capabilities
            .hooks
            .iter()
            .map(|item| item.id.as_str()),
        "钩子 ID",
    )?;
    unique_nonempty(
        document
            .capabilities
            .skills
            .iter()
            .map(|item| item.id.as_str()),
        "技能 ID",
    )?;
    unique_nonempty(
        document
            .capabilities
            .mcps
            .iter()
            .map(|item| item.id.as_str()),
        "MCP ID",
    )?;

    let mut workspace_ids = HashSet::new();
    let mut conversation_ids = HashSet::new();
    let mut temporary_workspace_count = 0usize;

    for workspace in &document.workspaces {
        insert_runtime_identity_unique(&mut workspace_ids, &workspace.id, "工作区 ID")?;
        match workspace.kind {
            WorkspaceKind::Directory => {
                if workspace.id.starts_with("__") {
                    return Err(format!("保留工作区 {} 不得声明为普通目录", workspace.id));
                }
                if workspace.path.trim().is_empty() {
                    return Err(format!("工作区 {} 的路径为空", workspace.id));
                }
            }
            WorkspaceKind::Temporary => {
                if workspace.id != TEMPORARY_WORKSPACE_ID {
                    return Err(format!(
                        "临时工作区必须使用保留 ID {TEMPORARY_WORKSPACE_ID}"
                    ));
                }
                if !workspace.path.is_empty() {
                    return Err("临时工作区不得设置持久化路径".into());
                }
                temporary_workspace_count += 1;
            }
            WorkspaceKind::Unsupported => {
                return Err(format!("工作区 {} 使用了不支持的类型", workspace.id));
            }
        }

        // Default preset IDs may be dangling; validate length but not catalog existence.
        if workspace.default_conversation_preset_id.len() > 128 {
            return Err(format!("工作区 {} 的默认对话预设 ID 过长", workspace.id));
        }
        if let Some(remembered) = &workspace.last_conversation_settings {
            validate_conversation_settings_shape(
                &format!("工作区 {} 记住的对话设置", workspace.id),
                remembered,
                &tool_names,
            )?;
        }

        for conversation in &workspace.conversations {
            insert_runtime_identity_unique(&mut conversation_ids, &conversation.id, "对话 ID")?;
            validate_conversation_shape(conversation, &tool_names)?;
        }
    }
    if temporary_workspace_count != 1 {
        return Err("文档必须且只能包含一个临时工作区".into());
    }
    Ok(())
}

/// Validates one independent conversation failure domain, including IDs unique within that conversation.
fn validate_conversation_shape(
    conversation: &Conversation,
    tool_names: &HashSet<&str>,
) -> Result<(), String> {
    // Conversation IDs are body filenames and must meet the same safety rule.
    validate_conversation_id(&conversation.id)?;
    let mut context_ids = HashSet::new();
    let mut branch_ids = HashSet::new();
    let mut context_count = 0usize;
    validate_conversation_settings_shape(
        &format!("对话 {}", conversation.id),
        &conversation.settings,
        tool_names,
    )?;

    // Validate only target shape; missing SSH machines are resolved at dispatch time.
    if let Some(target) = &conversation.run_target {
        match target {
            crate::model::RunTarget::Wsl { distro } => {
                crate::run_environment::validate_wsl_distro_name(distro)
                    .map_err(|error| format!("对话 {} 的运行地点无效: {error}", conversation.id))?;
            }
            crate::model::RunTarget::Ssh { machine_id } => {
                if machine_id.trim().is_empty() || machine_id.len() > 128 {
                    return Err(format!(
                        "对话 {} 的运行地点 SSH 机器 ID 无效",
                        conversation.id
                    ));
                }
            }
        }
    }

    if conversation.user_aborted_tasks.len() > 256 {
        return Err(format!(
            "对话 {} 的用户中止任务记录不能超过 256 条",
            conversation.id
        ));
    }
    let mut aborted_task_ids = HashSet::new();
    for task in &conversation.user_aborted_tasks {
        if task.id.trim().is_empty() || task.id.len() > 128 || !aborted_task_ids.insert(task.id.as_str()) {
            return Err(format!("对话 {} 的用户中止任务记录 ID 无效或重复", conversation.id));
        }
        if !matches!(task.source_kind.as_str(), "subagent" | "workflow" | "terminal" | "shell" | "browser") {
            return Err(format!("对话 {} 的用户中止任务类型无效", conversation.id));
        }
        if task.source_identity.trim().is_empty() || task.source_identity.len() > 256 {
            return Err(format!("对话 {} 的用户中止任务来源无效", conversation.id));
        }
        if task.label.chars().count() > 512 || task.detail.chars().count() > 4096 {
            return Err(format!("对话 {} 的用户中止任务文本过长", conversation.id));
        }
        if task.reason != "userAborted" {
            return Err(format!("对话 {} 的用户中止任务原因无效", conversation.id));
        }
        if !task.started_at.is_empty() {
            chrono::DateTime::parse_from_rfc3339(&task.started_at)
                .map_err(|_| format!("对话 {} 的用户中止任务开始时间无效", conversation.id))?;
        }
        chrono::DateTime::parse_from_rfc3339(&task.ended_at)
            .map_err(|_| format!("对话 {} 的用户中止任务结束时间无效", conversation.id))?;
    }

    if conversation.queued_messages.len() > 100 {
        return Err(format!(
            "对话 {} 的排队消息不能超过 100 条",
            conversation.id
        ));
    }
    for message in &conversation.queued_messages {
        insert_nonempty_unique(&mut context_ids, &message.id, "排队消息 ID")?;
        if message.id.len() > 128 {
            return Err(format!("对话 {} 的排队消息 ID 过长", conversation.id));
        }
        if message.content.trim().is_empty() && message.images.is_empty() {
            return Err(format!(
                "对话 {} 的排队消息文字与图片不能同时为空",
                conversation.id
            ));
        }
        validate_image_list(
            &message.images,
            &format!("对话 {} 的排队消息 {}", conversation.id, message.id),
        )?;
        if message.content.chars().count() > 100_000 {
            return Err(format!(
                "对话 {} 的单条排队消息不能超过 100000 个字符",
                conversation.id
            ));
        }
        chrono::DateTime::parse_from_rfc3339(&message.created_at)
            .map_err(|_| format!("对话 {} 的排队消息时间无效", conversation.id))?;
    }

    validate_context_tree(&conversation.contexts, &mut context_ids, &mut context_count)?;

    let mut forkable_user_owners = HashMap::<&str, Option<&str>>::new();
    collect_forkable_user_owners(&conversation.contexts, None, &mut forkable_user_owners);
    let mut branch_groups = HashMap::<&str, (usize, usize)>::new();
    for branch in &conversation.branches {
        insert_nonempty_unique(&mut branch_ids, &branch.id, "分支 ID")?;
        if branch.fork_context_id.trim().is_empty() {
            return Err(format!("对话 {} 的分支点 ID 不能为空", conversation.id));
        }
        if branch.active && !branch.contexts.is_empty() {
            return Err(format!(
                "对话 {} 的活动分支槽 {} 不得保存后缀上下文",
                conversation.id, branch.id
            ));
        }
        collect_forkable_user_owners(
            &branch.contexts,
            Some(branch.fork_context_id.as_str()),
            &mut forkable_user_owners,
        );
        validate_context_tree(&branch.contexts, &mut context_ids, &mut context_count)?;
        let group = branch_groups
            .entry(branch.fork_context_id.as_str())
            .or_insert((0, 0));
        group.0 += 1;
        group.1 += usize::from(branch.active);
    }
    for (&fork_context_id, &(branch_count, active_count)) in &branch_groups {
        if !forkable_user_owners.contains_key(fork_context_id) {
            return Err(format!(
                "对话 {} 的分支点 {} 不是同一主时间线中的普通用户消息",
                conversation.id, fork_context_id
            ));
        }
        if branch_count < 2 || active_count != 1 {
            return Err(format!(
                "对话 {} 的分支点 {} 必须至少有两个分支且恰有一个活动槽",
                conversation.id, fork_context_id
            ));
        }
    }
    let mut reachable_branch_groups = HashSet::<&str>::new();
    loop {
        let count_before = reachable_branch_groups.len();
        for &fork_context_id in branch_groups.keys() {
            let reachable = match forkable_user_owners.get(fork_context_id).copied() {
                Some(None) => true,
                Some(Some(parent_fork_context_id)) => {
                    reachable_branch_groups.contains(parent_fork_context_id)
                }
                None => false,
            };
            if reachable {
                reachable_branch_groups.insert(fork_context_id);
            }
        }
        if reachable_branch_groups.len() == count_before {
            break;
        }
    }
    if let Some(fork_context_id) = branch_groups
        .keys()
        .copied()
        .find(|fork_context_id| !reachable_branch_groups.contains(fork_context_id))
    {
        return Err(format!(
            "对话 {} 的分支点 {} 不在从活动时间线可达的分支树中",
            conversation.id, fork_context_id
        ));
    }
    Ok(())
}

/// Every role list in the document, tagged with a human-readable location for
/// error messages. One list per preset, per conversation, and per captured local
/// preset — each is its own identity ledger.
fn agent_definition_lists(document: &AppDocument) -> Vec<(String, &[AgentDefinition])> {
    let mut lists = Vec::new();
    for preset in &document.presets.conversation_presets {
        lists.push((
            format!("对话预设 {}", preset.id),
            preset.settings.agent_definitions.as_slice(),
        ));
    }
    for workspace in &document.workspaces {
        for conversation in &workspace.conversations {
            lists.push((
                format!("对话 {}", conversation.id),
                conversation.settings.agent_definitions.as_slice(),
            ));
        }
    }
    lists
}

/// Whether any role, anywhere in the document, differs between two snapshots.
///
/// Compares list by list at matching locations, so a preset that gained or lost
/// roles counts as a change even when the totals happen to match.
pub(crate) fn agent_definitions_differ(previous: &AppDocument, next: &AppDocument) -> bool {
    let previous_lists = agent_definition_lists(previous)
        .into_iter()
        .collect::<HashMap<_, _>>();
    let next_lists = agent_definition_lists(next)
        .into_iter()
        .collect::<HashMap<_, _>>();
    previous_lists.len() != next_lists.len()
        || next_lists.iter().any(|(location, definitions)| {
            previous_lists
                .get(location)
                .is_none_or(|committed| committed != definitions)
        })
}

fn validate_agent_definitions(document: &AppDocument) -> Result<(), String> {
    for (location, definitions) in agent_definition_lists(document) {
        validate_agent_definition_list(&location, definitions)?;
    }
    Ok(())
}

/// `document` is needed only to resolve an explicit provider/model pair; the
/// roles being checked come from `definitions`, not from the document.
fn validate_agent_definition_list(
    location: &str,
    definitions: &[AgentDefinition],
) -> Result<(), String> {
    if definitions.len() > MAX_RETAINED_AGENT_DEFINITION_IDENTITIES {
        return Err(format!(
            "{location} 的命名子代理活跃定义与墓碑超过 {MAX_RETAINED_AGENT_DEFINITION_IDENTITIES} 项限制"
        ));
    }
    if definitions
        .iter()
        .filter(|definition| !definition.deleted)
        .count()
        > MAX_AGENT_DEFINITIONS
    {
        return Err(format!(
            "{location} 的活跃命名子代理定义超过 {MAX_AGENT_DEFINITIONS} 项限制"
        ));
    }

    let mut identities = HashSet::<(AgentDefinitionSource, String, String)>::new();
    for definition in definitions {
        crate::model::validate_agent_type_slug(&definition.name)?;
        // Descriptions have no separate length limit, but NUL is prohibited because downstream consumers treat it as binary.
        if definition.description.contains('\0') {
            return Err(format!(
                "命名子代理 {} 的说明不能包含 NUL 字符",
                definition.name
            ));
        }
        match definition.source {
            AgentDefinitionSource::User | AgentDefinitionSource::Managed => {
                if !definition.source_key.is_empty() {
                    return Err(format!(
                        "命名子代理 {} 的 user/managed sourceKey 必须是精确空串",
                        definition.name
                    ));
                }
            }
            AgentDefinitionSource::Project | AgentDefinitionSource::Plugin => {
                if definition.source_key.is_empty()
                    || definition.source_key.trim() != definition.source_key
                    || definition.source_key.chars().count() > MAX_AGENT_DEFINITION_SOURCE_KEY_CHARS
                    || definition.source_key.len() > 512
                    || definition.source_key.chars().any(char::is_control)
                {
                    return Err(format!(
                        "命名子代理 {} 的 project/plugin sourceKey 必须是 1–{} 个无首尾空白、无控制字符的精确字符",
                        definition.name, MAX_AGENT_DEFINITION_SOURCE_KEY_CHARS
                    ));
                }
            }
        }
        if definition.revision == 0 {
            return Err(format!(
                "命名子代理 {} 的 revision 必须是正整数",
                definition.name
            ));
        }
        if definition.memory_epoch == 0 {
            return Err(format!(
                "命名子代理 {} 的 memoryEpoch 必须是正整数",
                definition.name
            ));
        }
        if definition.deleted
            && (definition.source != AgentDefinitionSource::User
                || definition.enabled
                || !definition.description.is_empty()
                || definition.model_selection != AgentModelSelection::Inherit
                || definition.memory != AgentDefinitionMemory::None)
        {
            return Err(format!(
                "命名子代理 {} 的删除墓碑必须是停用的 user 身份，且不含说明与 model/memory 配置",
                definition.name
            ));
        }

        let identity = (
            definition.source,
            definition.source_key.clone(),
            definition.name.clone(),
        );
        if !identities.insert(identity) {
            return Err(format!(
                "命名子代理复合身份重复: {:?}/{}/{}",
                definition.source, definition.source_key, definition.name
            ));
        }

        if !definition.deleted {
            // `Unavailable` is intentionally not checked against the providers:
            // it already records the outcome of that check. Validating it would
            // be checking a value against the very fact it encodes.
            //
            // Only the SHAPE of an explicit pair is checked. Whether the pair
            // currently resolves is not a document-validity question: a role may
            // legitimately name a model the user has not fetched yet — a seeded
            // role bound to a provider that is signed out is exactly this — and
            // rejecting the document would make the whole profile unloadable
            // over a binding the user can still see and fix. Resolution is
            // evaluated at call time instead, by
            // `api::agent_definition_model_is_available`, which hides the role
            // from the model until the pair resolves again.
            if let AgentModelSelection::Explicit {
                provider_id,
                model_id,
            } = &definition.model_selection
            {
                if provider_id.is_empty()
                    || provider_id.trim() != provider_id
                    || model_id.is_empty()
                    || model_id.trim() != model_id
                {
                    return Err(format!(
                        "命名子代理 {} 的显式 providerId/modelId 必须是无首尾空白的精确标识",
                        definition.name
                    ));
                }
            }
        }
    }
    Ok(())
}

fn agent_definition_identity(
    definition: &AgentDefinition,
) -> (AgentDefinitionSource, String, String) {
    (
        definition.source,
        definition.source_key.clone(),
        definition.name.clone(),
    )
}

fn same_user_agent_configuration(previous: &AgentDefinition, next: &AgentDefinition) -> bool {
    // `description` is deliberately absent. It is prose the model reads, not a
    // capability the child holds, and this comparison is what decides whether
    // `revision` advances. A rewording that bumped the revision would make
    // `api::validate_current_agent_definition` refuse every child already bound
    // to the old one — a live run killed by an edit that changed nothing about
    // what that run may do.
    previous.enabled == next.enabled
        && previous.model_selection == next.model_selection
        && previous.memory == next.memory
}

/// Turns the renderer's user-definition draft into host authority.
///
/// The renderer may edit only `source=user, sourceKey=""` definitions. Every
/// revision and memory epoch is derived from the current committed snapshot;
/// project/plugin/managed definitions are immutable through this save path.
/// A deletion becomes a durable hidden tombstone, and recreating the same
/// public name advances its memory epoch so an old partition is never reused.
/// Turns one list of renderer-proposed roles into host authority.
///
/// Roles live in a conversation preset and in each conversation's materialized
/// settings, so this runs once per list rather than once per document.
/// `previous` is the committed list at the SAME location: an identity ledger is
/// per-list, because two presets may each define a role called `reviewer` and
/// they are not the same identity.
fn canonicalize_renderer_agent_definition_list(
    previous_definitions: &[AgentDefinition],
    proposed: Vec<AgentDefinition>,
) -> Result<Vec<AgentDefinition>, String> {
    let previous_by_identity = previous_definitions
        .iter()
        .map(|definition| (agent_definition_identity(definition), definition))
        .collect::<HashMap<_, _>>();

    let proposed_host = proposed
        .iter()
        .filter(|definition| definition.source != AgentDefinitionSource::User)
        .map(|definition| (agent_definition_identity(definition), definition))
        .collect::<HashMap<_, _>>();
    let previous_host = previous_definitions
        .iter()
        .filter(|definition| definition.source != AgentDefinitionSource::User)
        .map(|definition| (agent_definition_identity(definition), definition))
        .collect::<HashMap<_, _>>();
    if proposed_host.len() != previous_host.len()
        || previous_host.iter().any(|(identity, trusted)| {
            proposed_host
                .get(identity)
                .is_none_or(|proposed| *proposed != *trusted)
        })
    {
        return Err("renderer 不能新增、删除或修改 project/plugin/managed 命名 Agent 定义".into());
    }

    let mut canonical = Vec::new();
    let mut proposed_user_names = HashSet::new();
    for mut definition in proposed
        .into_iter()
        .filter(|definition| definition.source == AgentDefinitionSource::User)
    {
        if definition.deleted {
            return Err("renderer 不能直接创建命名 Agent 删除墓碑".into());
        }
        if !proposed_user_names.insert(definition.name.clone()) {
            return Err(format!("user 命名 Agent 身份重复: {}", definition.name));
        }
        let identity = agent_definition_identity(&definition);
        match previous_by_identity.get(&identity).copied() {
            Some(previous) if previous.deleted => {
                definition.revision = previous
                    .revision
                    .checked_add(1)
                    .ok_or_else(|| format!("命名 Agent {} revision 已耗尽", definition.name))?;
                definition.memory_epoch = previous
                    .memory_epoch
                    .checked_add(1)
                    .ok_or_else(|| format!("命名 Agent {} memoryEpoch 已耗尽", definition.name))?;
            }
            Some(previous) => {
                definition.revision = if same_user_agent_configuration(previous, &definition) {
                    previous.revision
                } else {
                    previous
                        .revision
                        .checked_add(1)
                        .ok_or_else(|| format!("命名 Agent {} revision 已耗尽", definition.name))?
                };
                definition.memory_epoch = previous.memory_epoch;
            }
            None => {
                definition.revision = 1;
                definition.memory_epoch = 1;
            }
        }
        definition.deleted = false;
        canonical.push(definition);
    }

    for previous_definition in previous_definitions
        .iter()
        .filter(|definition| definition.source == AgentDefinitionSource::User)
    {
        if proposed_user_names.contains(&previous_definition.name) {
            continue;
        }
        if previous_definition.deleted {
            canonical.push(previous_definition.clone());
            continue;
        }
        let mut tombstone = previous_definition.clone();
        tombstone.enabled = false;
        tombstone.deleted = true;
        tombstone.revision = tombstone
            .revision
            .checked_add(1)
            .ok_or_else(|| format!("命名 Agent {} revision 已耗尽", tombstone.name))?;
        // Tombstones retain identity only, not deleted-role prose.
        tombstone.description = String::new();
        tombstone.model_selection = AgentModelSelection::Inherit;
        tombstone.memory = AgentDefinitionMemory::None;
        canonical.push(tombstone);
    }

    canonical.extend(
        previous_definitions
            .iter()
            .filter(|definition| definition.source != AgentDefinitionSource::User)
            .cloned(),
    );
    if canonical.len() > MAX_RETAINED_AGENT_DEFINITION_IDENTITIES {
        return Err(format!(
            "命名 Agent 活跃定义与删除墓碑合计超过 {MAX_RETAINED_AGENT_DEFINITION_IDENTITIES} 项；请先清理持久身份账本"
        ));
    }
    Ok(canonical)
}

/// Walks every renderer-owned role list in the document: one per conversation
/// preset. A location with no committed predecessor canonicalizes against an
/// empty ledger, which is exactly right for a preset the user just created.
///
/// Conversation role lists are canonicalized by their write command and must not be processed again here.
fn canonicalize_renderer_agent_definitions(
    previous: &AppDocument,
    next: &mut AppDocument,
) -> Result<(), String> {
    let previous_presets = previous
        .presets
        .conversation_presets
        .iter()
        .map(|preset| (preset.id.as_str(), preset.settings.agent_definitions.as_slice()))
        .collect::<HashMap<_, _>>();
    for preset in &mut next.presets.conversation_presets {
        let proposed = std::mem::take(&mut preset.settings.agent_definitions);
        let committed = previous_presets
            .get(preset.id.as_str())
            .copied()
            .unwrap_or_default();
        preset.settings.agent_definitions =
            canonicalize_renderer_agent_definition_list(committed, proposed)?;
    }
    Ok(())
}

/// The identity ledger is per-list, so the transition is checked list by list:
/// `previous` and `next` are the committed and proposed roles at the SAME
/// location. Two presets may each define a role called `reviewer`, and they are
/// separate identities whose revisions must not be compared to each other.
fn validate_agent_definition_transition(
    previous: &AppDocument,
    next: &AppDocument,
) -> Result<(), String> {
    let mut previous_lists = HashMap::new();
    for (location, definitions) in agent_definition_lists(previous) {
        previous_lists.insert(location, definitions);
    }
    for (location, definitions) in agent_definition_lists(next) {
        let committed = previous_lists.get(&location).copied().unwrap_or_default();
        validate_agent_definition_list_transition(committed, definitions)?;
    }
    Ok(())
}

fn validate_agent_definition_list_transition(
    previous_definitions: &[AgentDefinition],
    next_definitions: &[AgentDefinition],
) -> Result<(), String> {
    let previous_by_identity = previous_definitions
        .iter()
        .map(|definition| {
            (
                (
                    definition.source,
                    definition.source_key.as_str(),
                    definition.name.as_str(),
                ),
                definition,
            )
        })
        .collect::<HashMap<_, _>>();

    for definition in next_definitions {
        let identity = (
            definition.source,
            definition.source_key.as_str(),
            definition.name.as_str(),
        );
        let Some(previous_definition) = previous_by_identity.get(&identity).copied() else {
            // A source/name change is intentionally a delete plus a new
            // definition. Persistent memory partitions are not moved here.
            continue;
        };
        if definition.revision < previous_definition.revision {
            return Err(format!(
                "命名子代理 {:?}/{}/{} 的 revision 不能从 {} 回退到 {}",
                definition.source,
                definition.source_key,
                definition.name,
                previous_definition.revision,
                definition.revision
            ));
        }
        if definition.memory_epoch < previous_definition.memory_epoch {
            return Err(format!(
                "命名子代理 {:?}/{}/{} 的 memoryEpoch 不能从 {} 回退到 {}",
                definition.source,
                definition.source_key,
                definition.name,
                previous_definition.memory_epoch,
                definition.memory_epoch
            ));
        }
        if previous_definition.deleted && !definition.deleted {
            if definition.memory_epoch <= previous_definition.memory_epoch {
                return Err(format!(
                    "重新创建命名子代理 {:?}/{}/{} 必须提高 memoryEpoch",
                    definition.source, definition.source_key, definition.name
                ));
            }
        } else if definition.memory_epoch != previous_definition.memory_epoch {
            return Err(format!(
                "命名子代理 {:?}/{}/{} 只有删除后重建才能提高 memoryEpoch",
                definition.source, definition.source_key, definition.name
            ));
        }
        let configuration_changed = definition.enabled != previous_definition.enabled
            || definition.deleted != previous_definition.deleted
            || definition.model_selection != previous_definition.model_selection
            || definition.memory != previous_definition.memory;
        if configuration_changed && definition.revision <= previous_definition.revision {
            return Err(format!(
                "命名子代理 {:?}/{}/{} 的可信配置变更必须提高 revision（当前 {}）",
                definition.source,
                definition.source_key,
                definition.name,
                previous_definition.revision
            ));
        }
    }
    Ok(())
}

/// Validates execution-environment assets.
///
/// Limits prevent malformed documents from causing unbounded startup work.
fn validate_execution_environments(
    assets: &crate::model::ExecutionEnvironmentAssets,
) -> Result<(), String> {
    const MAX_SSH_MACHINES: usize = 64;
    const MAX_ENV_TABLES: usize = 256;
    const MAX_ENV_VARS_PER_TABLE: usize = 128;
    const MAX_ENV_VALUE_CHARS: usize = 8192;
    const MAX_HOST_CHARS: usize = 512;
    const MAX_PATH_FIELD_CHARS: usize = 4096;

    let ids = unique_trimmed_nonempty(
        assets.ssh_machines.iter().map(|machine| machine.id.as_str()),
        "SSH 机器 ID",
    )?;
    if ids.len() > MAX_SSH_MACHINES {
        return Err(format!("SSH 机器不能超过 {MAX_SSH_MACHINES} 台"));
    }
    for machine in &assets.ssh_machines {
        let name = machine.name.trim();
        if name.is_empty() {
            return Err(format!("SSH 机器 {} 的名称不能为空", machine.id));
        }
        if name.chars().count() > 64 {
            return Err(format!("SSH 机器 {} 的名称过长", machine.id));
        }
        let host = machine.host.trim();
        if host.is_empty() {
            return Err(format!("SSH 机器 {name} 的主机地址不能为空"));
        }
        if host.chars().count() > MAX_HOST_CHARS {
            return Err(format!("SSH 机器 {name} 的主机地址过长"));
        }
        // The host is one `ssh` argv argument; whitespace, control characters, and a leading dash are unsafe.
        if host.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(format!("SSH 机器 {name} 的主机地址不能包含空白或控制字符"));
        }
        if host.starts_with('-') {
            return Err(format!("SSH 机器 {name} 的主机地址不能以 - 开头"));
        }
        for (label, value) in [
            ("身份文件路径", &machine.identity_file),
            ("远端工作目录", &machine.remote_cwd),
        ] {
            if value.chars().count() > MAX_PATH_FIELD_CHARS {
                return Err(format!("SSH 机器 {name} 的{label}过长"));
            }
            if value.chars().any(char::is_control) {
                return Err(format!("SSH 机器 {name} 的{label}不能包含控制字符"));
            }
        }
    }
    if assets.env_vars.len() > MAX_ENV_TABLES {
        return Err(format!("运行环境变量表不能超过 {MAX_ENV_TABLES} 份"));
    }
    for (key, table) in &assets.env_vars {
        // Allow dangling SSH environment tables so removed machines do not invalidate the document.
        let valid_key = key == "local"
            || key
                .strip_prefix("wsl:")
                .is_some_and(|distro| {
                    crate::run_environment::validate_wsl_distro_name(distro).is_ok()
                })
            || key
                .strip_prefix("ssh:")
                .is_some_and(|id| !id.trim().is_empty() && id.len() <= 128);
        if !valid_key {
            return Err(format!("运行环境键 {key:?} 不合法"));
        }
        if table.len() > MAX_ENV_VARS_PER_TABLE {
            return Err(format!(
                "运行环境 {key} 的变量不能超过 {MAX_ENV_VARS_PER_TABLE} 条"
            ));
        }
        for (variable, value) in table {
            crate::run_environment::validate_env_var_name(variable)
                .map_err(|error| format!("运行环境 {key}: {error}"))?;
            // Environment variables reach remote command lines or local processes; reject control characters and private harness names.
            if crate::child_environment::is_private_child_environment_name(
                std::ffi::OsStr::new(variable),
            ) {
                return Err(format!("运行环境 {key} 的变量 {variable} 是宿主保留名"));
            }
            // Shell startup variables could execute unapproved scripts before each command.
            if crate::run_environment::is_shell_startup_env_name(variable) {
                return Err(format!(
                    "运行环境 {key} 的变量 {variable} 是 shell 启动保留名，不允许配置"
                ));
            }
            if value.chars().count() > MAX_ENV_VALUE_CHARS {
                return Err(format!("运行环境 {key} 的变量 {variable} 的值过长"));
            }
            if value.chars().any(char::is_control) {
                return Err(format!(
                    "运行环境 {key} 的变量 {variable} 的值不能包含控制字符"
                ));
            }
        }
    }
    Ok(())
}

fn validate_mcp_servers(servers: &[crate::model::McpServerConfig]) -> Result<(), String> {
    let ids = unique_trimmed_nonempty(
        servers.iter().map(|server| server.id.as_str()),
        "MCP 服务器 ID",
    )?;
    if ids.len() > MAX_MCP_SERVERS {
        return Err(format!("MCP 服务器不能超过 {MAX_MCP_SERVERS} 台"));
    }
    let mut names = std::collections::HashSet::new();
    for server in servers {
        let name = server.name.trim();
        if name.is_empty() {
            return Err(format!("MCP 服务器 {} 的名称不能为空", server.id));
        }
        if name.chars().count() > MAX_MCP_NAME_CHARS {
            return Err(format!("MCP 服务器 {} 的名称过长", server.id));
        }
        // Server names prefix exposed tool names and must be unique.
        if !names.insert(name.to_lowercase()) {
            return Err(format!("MCP 服务器名称重复：{name}"));
        }
        match server.transport {
            crate::model::McpTransportKind::Stdio => {
                if server.command.trim().is_empty() {
                    return Err(format!("MCP 服务器 {name} 的启动命令不能为空"));
                }
            }
            crate::model::McpTransportKind::StreamableHttp => {
                if server.url.trim().is_empty() {
                    return Err(format!("MCP 服务器 {name} 的地址不能为空"));
                }
                crate::api::validate_base_url(&server.url)
                    .map_err(|error| format!("MCP 服务器 {name} 的地址无效: {error}"))?;
            }
        }
        if server.timeout_seconds > MAX_MCP_TIMEOUT_SECONDS {
            return Err(format!(
                "MCP 服务器 {name} 的超时不能超过 {MAX_MCP_TIMEOUT_SECONDS} 秒"
            ));
        }
        if server.args.len() > MAX_MCP_ARGUMENTS {
            return Err(format!("MCP 服务器 {name} 的参数过多"));
        }
        for (key, value) in server.env.iter().chain(server.headers.iter()) {
            if key.trim().is_empty() {
                return Err(format!("MCP 服务器 {name} 的环境变量/请求头名不能为空"));
            }
            if key.chars().any(char::is_control) || value.chars().any(char::is_control) {
                return Err(format!(
                    "MCP 服务器 {name} 的环境变量/请求头不能包含控制字符"
                ));
            }
        }
    }
    Ok(())
}

/// Validates skill records. Folder names are case-insensitive on Windows and must be single path components.
fn validate_skill_records(skills: &[crate::model::SkillRecord]) -> Result<(), String> {
    unique_trimmed_nonempty(skills.iter().map(|skill| skill.id.as_str()), "技能记录 ID")?;
    if skills.len() > MAX_INSTALLED_SKILLS {
        return Err(format!("已安装技能不能超过 {MAX_INSTALLED_SKILLS} 个"));
    }
    let mut folders = std::collections::HashSet::new();
    for skill in skills {
        let folder = skill.folder_name.trim();
        if folder.is_empty() {
            return Err(format!("技能 {} 的目录名不能为空", skill.id));
        }
        if folder.chars().count() > MAX_SKILL_FOLDER_CHARS {
            return Err(format!("技能 {} 的目录名过长", skill.id));
        }
        if folder == "."
            || folder == ".."
            || folder.contains('/')
            || folder.contains('\\')
            || folder.chars().any(char::is_control)
        {
            return Err(format!("技能目录名不是一个单层目录：{folder}"));
        }
        if !folders.insert(folder.to_lowercase()) {
            return Err(format!("技能目录名重复：{folder}"));
        }
        if skill.name.trim().is_empty() {
            return Err(format!("技能 {} 的名称不能为空", skill.id));
        }
    }
    Ok(())
}

/// Validates environment-tool definitions. Executables must be bare names because they are used to start processes.
fn validate_environment_tools(
    tools: &[crate::model::EnvironmentToolDefinition],
) -> Result<(), String> {
    if tools.len() > MAX_ENVIRONMENT_TOOLS {
        return Err(format!("环境依赖不能超过 {MAX_ENVIRONMENT_TOOLS} 条"));
    }
    let mut names = std::collections::HashSet::new();
    for tool in tools {
        let name = tool.name.trim();
        if name.is_empty() || name.chars().count() > MAX_ENVIRONMENT_TOOL_NAME_CHARS {
            return Err("环境依赖名称必须是 1–64 个字符".into());
        }
        if !names.insert(name.to_lowercase()) {
            return Err(format!("环境依赖名称重复：{name}"));
        }
        let executable = tool.executable.trim();
        if executable.is_empty() {
            return Err(format!("环境依赖 {name} 的可执行文件名不能为空"));
        }
        if executable.contains('/')
            || executable.contains('\\')
            || executable.chars().any(char::is_control)
        {
            return Err(format!(
                "环境依赖 {name} 的可执行文件名必须是裸名字，不能是路径"
            ));
        }
        if tool.version_args.len() > MAX_ENVIRONMENT_TOOL_ARGUMENTS {
            return Err(format!("环境依赖 {name} 的版本参数过多"));
        }
        for argument in &tool.version_args {
            if argument.chars().any(char::is_control) {
                return Err(format!("环境依赖 {name} 的版本参数不能包含控制字符"));
            }
        }
    }
    Ok(())
}

fn validate_web_search_assets(assets: &crate::model::WebSearchAssets) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for entry in &assets.providers {
        if !seen.insert(entry.kind) {
            return Err(format!("搜索提供商重复：{}", entry.kind.slug()));
        }
        for (label, host) in [
            ("搜索端点", &entry.search_api_host),
            ("抓取端点", &entry.fetch_api_host),
        ] {
            if host.chars().count() > 2_048 {
                return Err(format!("搜索提供商 {} 的{label}过长", entry.kind.slug()));
            }
            if host
                .chars()
                .any(|character| character.is_control() || character.is_whitespace())
            {
                return Err(format!(
                    "搜索提供商 {} 的{label}不能包含空白或控制字符",
                    entry.kind.slug()
                ));
            }
        }
        // Engine names enter the query string, so control characters are request-splitting primitives.
        if entry.engines.len() > 64 {
            return Err(format!("搜索提供商 {} 的引擎过多", entry.kind.slug()));
        }
        for engine in &entry.engines {
            if engine.chars().count() > 128 || engine.chars().any(char::is_control) {
                return Err(format!("搜索提供商 {} 的引擎名无效", entry.kind.slug()));
            }
        }
        // The username is sent in a Basic authentication header.
        if entry.basic_auth_username.chars().count() > 256
            || entry.basic_auth_username.chars().any(char::is_control)
        {
            return Err(format!(
                "搜索提供商 {} 的 Basic Auth 用户名无效",
                entry.kind.slug()
            ));
        }
    }
    // A configured fetch provider must support URL fetching.
    if let Some(kind) = assets.fetch_provider {
        if !kind.supports(crate::model::SearchCapability::FetchUrls) {
            return Err(format!(
                "搜索提供商 {} 不提供网页抓取，不能作为抓取提供商",
                kind.slug()
            ));
        }
    }
    if assets.max_results == 0 || assets.max_results > 50 {
        return Err("搜索结果数必须在 1 到 50 之间".into());
    }
    if assets.exclude_domains.len() > 512 {
        return Err("域名黑名单条目过多".into());
    }
    for rule in &assets.exclude_domains {
        if rule.chars().count() > 512 || rule.chars().any(char::is_control) {
            return Err("域名黑名单规则无效".into());
        }
    }
    if assets.compression.cutoff_limit > 200_000 {
        return Err("搜索结果截断预算过大".into());
    }
    Ok(())
}

/// Validates per-conversation web-search limits. Credentials are read from the OS store only at request time.
fn validate_conversation_web_search(
    label: &str,
    settings: &crate::model::ConversationWebSearchSettings,
) -> Result<(), String> {
    if settings.max_searches_per_call > MAX_WEB_SEARCHES_PER_CALL {
        return Err(format!(
            "{label}的单次联网搜索次数上限必须在 0–{MAX_WEB_SEARCHES_PER_CALL} 之间（0 表示不设限）"
        ));
    }
    Ok(())
}

/// Validates inline tool-description selections.
fn validate_tool_description_selection(label: &str, selection: Option<&str>) -> Result<(), String> {
    match selection {
        Some(id) if id.trim().is_empty() => Err(format!("{label}的工具描述选择不能为空串")),
        Some(id) if id.len() > 256 => Err(format!("{label}的工具描述 ID 过长")),
        _ => Ok(()),
    }
}

/// Validates directly selected capability IDs without requiring a scan-time catalog entry.
fn validate_capability_ids(label: &str, kind: &str, ids: &[String]) -> Result<(), String> {
    let mut seen = HashSet::new();
    for id in ids {
        if id.trim().is_empty() {
            return Err(format!("{label}的{kind} ID 不能为空"));
        }
        if !seen.insert(id.as_str()) {
            return Err(format!("{label}重复选择了{kind}: {id}"));
        }
    }
    Ok(())
}

/**
 * Validates shared conversation settings used by conversations, workspace snapshots, and template presets.
 */
fn validate_conversation_settings_shape(
    label: &str,
    settings: &ConversationSettings,
    tool_names: &HashSet<&str>,
) -> Result<(), String> {
    if settings.system_prompt.len() > 1024 * 1024 {
        return Err(format!("{label}的系统提示词超过 1 MiB 限制"));
    }
    let mut enabled_tools = HashSet::new();
    for enabled in &settings.enabled_tools {
        if !tool_names.contains(enabled.as_str()) {
            return Err(format!("{label}引用了未知工具: {enabled}"));
        }
        if !enabled_tools.insert(enabled.as_str()) {
            return Err(format!("{label}重复启用了工具: {enabled}"));
        }
    }
    validate_tool_description_selection(label, settings.tool_description_file_id.as_deref())?;
    for (kind, ids) in [
        ("钩子", &settings.hook_ids),
        ("技能", &settings.skill_ids),
        ("MCP", &settings.mcp_ids),
    ] {
        validate_capability_ids(label, kind, ids)?;
    }
    // Document validation does not bind external selections to assets; runtime execution fails closed.
    validate_conversation_web_search(label, &settings.web_search)?;
    Ok(())
}

fn validate_conversation_preset_settings(
    label: &str,
    settings: &ConversationPresetSettings,
    tool_names: &HashSet<&str>,
) -> Result<(), String> {
    if settings.system_prompt.len() > 1024 * 1024 {
        return Err(format!("{label}的系统提示词超过 1 MiB 限制"));
    }
    let mut enabled_tools = HashSet::new();
    for enabled in &settings.enabled_tools {
        if !tool_names.contains(enabled.as_str()) {
            return Err(format!("{label}引用了未知工具: {enabled}"));
        }
        if !enabled_tools.insert(enabled.as_str()) {
            return Err(format!("{label}重复启用了工具: {enabled}"));
        }
    }
    validate_tool_description_selection(label, settings.tool_description_file_id.as_deref())?;
    for (kind, ids) in [
        ("钩子", &settings.hook_ids),
        ("技能", &settings.skill_ids),
        ("MCP", &settings.mcp_ids),
    ] {
        validate_capability_ids(label, kind, ids)?;
    }
    // Preset web-search settings use the same numeric bounds as conversation settings.
    validate_conversation_web_search(label, &settings.web_search)?;
    Ok(())
}

fn collect_forkable_user_owners<'a>(
    roots: &'a [ContextItem],
    owner: Option<&'a str>,
    users: &mut HashMap<&'a str, Option<&'a str>>,
) {
    let mut pending = roots.iter().collect::<Vec<_>>();
    while let Some(context) = pending.pop() {
        match context {
            ContextItem::User { id, .. } => {
                users.insert(id.as_str(), owner);
            }
            _ => {}
        }
    }
}

fn validate_context_tree(
    roots: &[ContextItem],
    context_ids: &mut HashSet<String>,
    context_count: &mut usize,
) -> Result<(), String> {
    validate_context_source_metadata(roots)?;
    let mut fork_scopes = Vec::new();
    validate_context_scope(roots, context_ids, context_count, &mut fork_scopes)?;
    while let Some(fork_roots) = fork_scopes.pop() {
        let mut fork_context_ids = HashSet::new();
        validate_context_scope(
            fork_roots,
            &mut fork_context_ids,
            context_count,
            &mut fork_scopes,
        )?;
    }
    Ok(())
}

fn validate_context_source_metadata(roots: &[ContextItem]) -> Result<(), String> {
    let mut scopes = vec![roots];
    while let Some(scope) = scopes.pop() {
        for context in scope {
            match context {
                ContextItem::Assistant { model_turn_id, .. }
                | ContextItem::Reasoning { model_turn_id, .. } => {
                    if let Some(source_id) = model_turn_id.as_deref() {
                        validate_model_turn_id(source_id)?;
                    }
                }
                ContextItem::Tool {
                    model_turn_id,
                    subagent,
                    ..
                } => {
                    if let Some(source_id) = model_turn_id.as_deref() {
                        validate_model_turn_id(source_id)?;
                    }
                    if let Some(subagent) = subagent {
                        scopes.push(&subagent.contexts);
                    }
                }
                ContextItem::System { .. } | ContextItem::User { .. } => {}
            }
        }
    }
    Ok(())
}

fn validate_model_turn_id(turn_id: &str) -> Result<(), String> {
    if turn_id.trim().is_empty() || turn_id.len() > 1024 {
        return Err("modelTurnId 必须是长度不超过 1024 的非空字符串".into());
    }
    Ok(())
}

fn valid_keyed_receipt(value: &str) -> bool {
    crate::model::is_lower_hex_digest(value)
}

/// Optional receipts are either empty or lowercase hexadecimal digests.
fn valid_optional_keyed_receipt(value: &str) -> bool {
    value.is_empty() || valid_keyed_receipt(value)
}

fn validate_subagent_fork_binding(
    context_id: &str,
    binding: &ForkModelBinding,
) -> Result<(), String> {
    for (label, value) in [
        ("providerId", binding.provider_id.as_str()),
        ("modelId", binding.model_id.as_str()),
    ] {
        if value.is_empty()
            || value.trim() != value
            || value.len() > 512
            || value.chars().any(char::is_control)
        {
            return Err(format!(
                "工具上下文 {context_id} 的 conversation fork {label} 不是有效的精确原始标识"
            ));
        }
    }
    if binding.system_prompt_snapshot.is_empty()
        || binding.system_prompt_snapshot.len() > 1024 * 1024
        || binding.system_prompt_snapshot.contains('\0')
        || !valid_optional_keyed_receipt(&binding.system_prompt_receipt)
        || !valid_optional_keyed_receipt(&binding.binding_receipt)
    {
        return Err(format!(
            "工具上下文 {context_id} 的 conversation fork 系统提示快照或绑定回执无效"
        ));
    }
    // Accept either the complete current or complete retired memory-tool set; mixed sets have no valid source.
    let all_current_memory_tools = binding
        .memory_tool_names
        .iter()
        .all(|name| crate::mework_memory::is_memory_tool(name));
    let all_retired_memory_tools = binding.memory_tool_names.iter().all(|name| {
        matches!(
            name.as_str(),
            "memory_list" | "memory_read" | "memory_search" | "memory_upsert" | "memory_delete"
        )
    });
    if binding.memory_tool_names.len() > crate::mework_memory::MEMORY_TOOL_NAMES.len()
        || binding
            .memory_tool_names
            .windows(2)
            .any(|window| window[0].as_str() >= window[1].as_str())
        || !(all_current_memory_tools || all_retired_memory_tools)
    {
        return Err(format!(
            "工具上下文 {context_id} 的 conversation fork 记忆工具集合无效"
        ));
    }
    if binding
        .memory_snapshot_receipt
        .as_deref()
        .is_some_and(|receipt| !valid_keyed_receipt(receipt))
    {
        return Err(format!(
            "工具上下文 {context_id} 的 conversation fork 记忆快照回执无效"
        ));
    }
    Ok(())
}

fn validate_subagent_definition_binding(
    context_id: &str,
    binding: &AgentDefinitionBinding,
) -> Result<(), String> {
    crate::model::validate_agent_type_slug(&binding.name)
        .map_err(|error| format!("工具上下文 {context_id} 的命名 Agent 定义无效: {error}"))?;
    match binding.source {
        AgentDefinitionSource::User | AgentDefinitionSource::Managed
            if !binding.source_key.is_empty() =>
        {
            return Err(format!(
                "工具上下文 {context_id} 的 user/managed Agent sourceKey 必须为空"
            ))
        }
        AgentDefinitionSource::Project | AgentDefinitionSource::Plugin
            if binding.source_key.is_empty()
                || binding.source_key.trim() != binding.source_key
                || binding.source_key.chars().count() > MAX_AGENT_DEFINITION_SOURCE_KEY_CHARS
                || binding.source_key.len() > 512
                || binding.source_key.chars().any(char::is_control) =>
        {
            return Err(format!(
                "工具上下文 {context_id} 的 project/plugin Agent sourceKey 无效"
            ))
        }
        _ => {}
    }
    if binding.revision == 0 {
        return Err(format!(
            "工具上下文 {context_id} 的命名 Agent revision 必须是正整数"
        ));
    }
    if binding.memory_epoch == 0 {
        return Err(format!(
            "工具上下文 {context_id} 的命名 Agent memoryEpoch 必须是正整数"
        ));
    }
    if !valid_optional_keyed_receipt(&binding.configuration_receipt) {
        return Err(format!(
            "工具上下文 {context_id} 的命名 Agent 配置回执格式无效"
        ));
    }
    for (label, value) in [
        ("providerId", binding.provider_id.as_str()),
        ("modelId", binding.model_id.as_str()),
    ] {
        if value.is_empty()
            || value.trim() != value
            || value.chars().count() > 512
            || value.chars().any(char::is_control)
        {
            return Err(format!(
                "工具上下文 {context_id} 的命名 Agent {label} 必须是无首尾空白、无控制字符的精确原始标识"
            ));
        }
    }
    match binding.memory {
        AgentDefinitionMemory::None | AgentDefinitionMemory::User
            if !binding.scope_key.is_empty() =>
        {
            return Err(format!(
                "工具上下文 {context_id} 的 none/user Agent 记忆不得携带工作区作用域键"
            ))
        }
        AgentDefinitionMemory::Project | AgentDefinitionMemory::Local
            if binding.scope_key.is_empty()
                || binding.scope_key.trim() != binding.scope_key
                || binding.scope_key.chars().count() > 512
                || binding.scope_key.chars().any(char::is_control) =>
        {
            return Err(format!(
                "工具上下文 {context_id} 的 project/local Agent 记忆缺少有效作用域键"
            ))
        }
        _ => {}
    }
    Ok(())
}

fn validate_context_scope<'a>(
    roots: &'a [ContextItem],
    context_ids: &mut HashSet<String>,
    context_count: &mut usize,
    fork_scopes: &mut Vec<&'a [ContextItem]>,
) -> Result<(), String> {
    let mut pending = roots.iter().rev().collect::<Vec<_>>();
    while let Some(context) = pending.pop() {
        *context_count += 1;
        if *context_count > 100_000 {
            return Err("上下文数量超过 100000 条限制".into());
        }
        insert_nonempty_unique(context_ids, context.id(), "上下文 ID")?;
        match context {
            ContextItem::User {
                id,
                content,
                images,
                ..
            } => {
                if content.trim().is_empty() && images.is_empty() {
                    return Err(format!("用户上下文 {id} 的文字与图片不能同时为空"));
                }
                validate_image_list(images, &format!("用户上下文 {id}"))?;
            }
            ContextItem::Tool { id, result, .. } => {
                validate_image_list(&result.images, &format!("工具上下文 {id}"))?;
            }
            _ => {}
        }
        match context {
            ContextItem::Reasoning { .. } => {}
            ContextItem::Tool { id, subagent, .. } => {
                if let Some(subagent) = subagent {
                    if !valid_optional_keyed_receipt(&subagent.execution_mode_receipt) {
                        return Err(format!("工具上下文 {id} 的子代理执行模式回执格式无效"));
                    }
                    if subagent.inherits_model_memory != subagent.fork_model_binding.is_some() {
                        return Err(format!(
                            "工具上下文 {id} 的 conversation fork 继承标记与精确绑定不一致"
                        ));
                    }
                    if subagent.agent_definition.is_some()
                        && (subagent.inherits_model_memory || subagent.fork_model_binding.is_some())
                    {
                        return Err(format!(
                            "工具上下文 {id} 的子代理不能同时声明 conversation fork 与命名 Agent 定义"
                        ));
                    }
                    if let Some(binding) = subagent.fork_model_binding.as_ref() {
                        if subagent.name.is_none()
                            || subagent.kind != crate::model::SubagentRunKind::General
                        {
                            return Err(format!(
                                "工具上下文 {id} 的 conversation fork 缺少普通可寻址 Agent 身份"
                            ));
                        }
                        validate_subagent_fork_binding(id, binding)?;
                    }
                    if let Some(binding) = subagent.agent_definition.as_ref() {
                        if subagent.name.is_none() {
                            return Err(format!(
                                "工具上下文 {id} 的命名 Agent 记录缺少可寻址运行时名称"
                            ));
                        }
                        if subagent.kind != crate::model::SubagentRunKind::General {
                            return Err(format!(
                                "工具上下文 {id} 的专用子代理记录不得携带命名 Agent 定义"
                            ));
                        }
                        validate_subagent_definition_binding(id, binding)?;
                    }
                    if subagent.queued_messages.len() > 100 {
                        return Err(format!("工具上下文 {id} 的子代理排队消息超过 100 条限制"));
                    }
                    for queued in &subagent.queued_messages {
                        if queued.content.trim().is_empty() {
                            return Err(format!("工具上下文 {id} 的子代理排队消息不能为空"));
                        }
                        if queued.content.chars().count() > 32 * 1024 {
                            return Err(format!(
                                "工具上下文 {id} 的子代理排队消息超过 32768 字符限制"
                            ));
                        }
                    }
                    // A subagent record is a fork snapshot, not another archive location in the
                    // parent timeline. It may therefore retain the exact IDs it inherited from
                    // that timeline. Keep uniqueness strict inside each fork while sharing the
                    // global count limit. Queue the fork
                    // scope explicitly so a maliciously deep record tree cannot consume the call
                    // stack before the count limit rejects it.
                    fork_scopes.push(&subagent.contexts);
                }
            }
            ContextItem::System { .. }
            | ContextItem::User { .. }
            | ContextItem::Assistant { .. } => {}
        }
    }
    Ok(())
}

fn conversation_context_roots(conversation: &Conversation) -> impl Iterator<Item = &[ContextItem]> {
    std::iter::once(conversation.contexts.as_slice()).chain(
        conversation
            .branches
            .iter()
            .map(|branch| branch.contexts.as_slice()),
    )
}

/// Validates a renderer-proposed conversation synchronously at the command boundary, including tool-card provenance.
pub(crate) fn validate_incoming_conversation(
    document: &AppDocument,
    workspace_id: &str,
    conversation: &mut Conversation,
    state: &AppState,
) -> Result<(), String> {
    let committed = document
        .workspaces
        .iter()
        .flat_map(|workspace| workspace.conversations.iter())
        .find(|candidate| candidate.id == conversation.id)
        .map(|candidate| candidate.settings.agent_definitions.as_slice())
        .unwrap_or_default();
    // Renderer definitions cannot assign trusted source or epoch values.
    let proposed = std::mem::take(&mut conversation.settings.agent_definitions);
    conversation.settings.agent_definitions =
        canonicalize_renderer_agent_definition_list(committed, proposed)?;
    validate_agent_definition_list_transition(committed, &conversation.settings.agent_definitions)?;

    let tool_names = document
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<HashSet<_>>();
    validate_conversation_shape(conversation, &tool_names)?;
    validate_agent_definition_list(
        &format!("对话 {}", conversation.id),
        &conversation.settings.agent_definitions,
    )?;
    let workspace = document
        .workspaces
        .iter()
        .find(|candidate| candidate.id == workspace_id)
        .ok_or_else(|| format!("工作区 {workspace_id} 不存在"))?;
    let previous_entry = document.workspaces.iter().find_map(|workspace| {
        workspace
            .conversations
            .iter()
            .find(|candidate| candidate.id == conversation.id)
            .map(|candidate| (workspace.id.as_str(), candidate))
    });
    let unattested =
        validate_conversation_tool_cards(previous_entry, workspace, conversation, state);
    if let Some(first) = unattested.first() {
        return Err(format!(
            "对话 {} 的工具卡 {}（{}）无法证明来自本应用自己的执行",
            conversation.id, first.context_id, first.tool_name
        ));
    }
    Ok(())
}

/// Reports whether a conversation update changes named-agent definitions and requires durable persistence.
pub(crate) fn conversation_agent_definitions_differ(
    previous: Option<&Conversation>,
    next: &Conversation,
) -> bool {
    previous.map(|conversation| conversation.settings.agent_definitions.as_slice())
        != Some(next.settings.agent_definitions.as_slice())
}

/// Collects unattested tool cards for per-card quarantine rather than rejecting every conversation.
fn validate_tool_results_isolated(
    previous: &AppDocument,
    document: &AppDocument,
    state: &AppState,
) -> Vec<UnattestedTool> {
    let previous_conversations = previous
        .workspaces
        .iter()
        .flat_map(|workspace| {
            workspace.conversations.iter().map(move |conversation| {
                (
                    conversation.id.as_str(),
                    (workspace.id.as_str(), conversation),
                )
            })
        })
        .collect::<HashMap<_, _>>();
    let mut unattested = Vec::new();
    for workspace in &document.workspaces {
        for conversation in &workspace.conversations {
            let previous_entry = previous_conversations
                .get(conversation.id.as_str())
                .copied();
            unattested.extend(validate_conversation_tool_cards(
                previous_entry,
                workspace,
                conversation,
                state,
            ));
        }
    }
    unattested
}

/// Test-only aggregate validation that collects quarantined cards.
#[cfg(test)]
fn validate_tool_results(
    previous: &AppDocument,
    document: &AppDocument,
    state: &AppState,
) -> Result<ToolResultValidation, String> {
    let previous_conversations = previous
        .workspaces
        .iter()
        .flat_map(|workspace| {
            workspace.conversations.iter().map(move |conversation| {
                (
                    conversation.id.as_str(),
                    (workspace.id.as_str(), conversation),
                )
            })
        })
        .collect::<HashMap<_, _>>();
    let mut unattested = Vec::new();
    for workspace in &document.workspaces {
        for conversation in &workspace.conversations {
            let previous_entry = previous_conversations
                .get(conversation.id.as_str())
                .copied();
            unattested.extend(validate_conversation_tool_cards(
                previous_entry,
                workspace,
                conversation,
                state,
            ));
        }
    }
    Ok(ToolResultValidation { unattested })
}

/// Validates tool-card provenance within one conversation against its prior snapshot.
fn validate_conversation_tool_cards(
    previous_entry: Option<(&str, &Conversation)>,
    workspace: &Workspace,
    conversation: &Conversation,
    state: &AppState,
) -> Vec<UnattestedTool> {
    let mut unattested = Vec::new();
    struct PreviousTool<'a> {
        workspace_id: &'a str,
        conversation_id: &'a str,
        tool_name: &'a str,
        requested_input: Option<&'a serde_json::Map<String, serde_json::Value>>,
        input: &'a serde_json::Map<String, serde_json::Value>,
        result: &'a ToolResult,
        subagent: Option<&'a crate::model::SubagentRunRecord>,
    }

    let mut previous_tools = HashMap::<&str, PreviousTool<'_>>::new();
    if let Some((previous_workspace_id, previous_conversation)) = previous_entry {
        for roots in conversation_context_roots(previous_conversation) {
            let mut pending = roots.iter().rev().collect::<Vec<_>>();
            while let Some(context) = pending.pop() {
                if let ContextItem::Tool {
                    id,
                    tool_name,
                    requested_input,
                    input,
                    result,
                    subagent,
                    ..
                } = context
                {
                    previous_tools.insert(
                        id.as_str(),
                        PreviousTool {
                            workspace_id: previous_workspace_id,
                            conversation_id: &previous_conversation.id,
                            tool_name,
                            requested_input: requested_input.as_ref(),
                            input,
                            result,
                            subagent: subagent.as_ref(),
                        },
                    );
                    // The exact serialized child record is committed by the outer subagent
                    // fingerprint. Nested tool cards are not separate renderer-owned timeline
                    // roots and therefore do not consume independent process-local receipts.
                }
            }
        }
    }

    {
        {
            for roots in conversation_context_roots(conversation) {
                let mut pending = roots.iter().rev().collect::<Vec<_>>();
                while let Some(context) = pending.pop() {
                    let ContextItem::Tool {
                        id,
                        tool_name,
                        requested_input,
                        input,
                        result,
                        subagent,
                        attestation,
                        ..
                    } = context
                    else {
                        continue;
                    };
                    // A valid outer subagent receipt binds the complete recursive audit snapshot,
                    // including every nested tool input/result and sidecar. Requiring the same
                    // nested cards to remain in the small process-local receipt LRU would make a
                    // long, otherwise valid child unable to save after it evicted its own first
                    // receipt. Any nested mutation changes `subagent` equality/fingerprint and is
                    // rejected at this outer context.

                    let unchanged = previous_tools.get(id.as_str()).is_some_and(|previous| {
                        previous.workspace_id == workspace.id
                            && previous.conversation_id == conversation.id
                            && previous.tool_name == *tool_name
                            && previous.requested_input == requested_input.as_ref()
                            && previous.input == input
                            && previous.result == result
                            && previous.subagent == subagent.as_ref()
                    });
                    let editable_question =
                        previous_tools.get(id.as_str()).is_some_and(|previous| {
                            if previous.workspace_id != workspace.id
                                || previous.conversation_id != conversation.id
                                || previous.tool_name != "ask_user"
                                || tool_name != "ask_user"
                                || previous.requested_input != requested_input.as_ref()
                                || previous.result != result
                                || !result.success
                                || !input.contains_key("questions")
                                || previous.subagent.is_some()
                                || subagent.is_some()
                            {
                                return false;
                            }
                            let Ok(previous_question) = parse_question(previous.input) else {
                                return false;
                            };
                            let Ok(next_question) = parse_question(input) else {
                                return false;
                            };
                            previous_question.questions.len() == next_question.questions.len()
                        });
                    let image_removal = previous_tools.get(id.as_str()).is_some_and(|previous| {
                        previous.workspace_id == workspace.id
                            && previous.conversation_id == conversation.id
                            && previous.tool_name == *tool_name
                            && previous.requested_input == requested_input.as_ref()
                            && previous.input == input
                            && previous.subagent == subagent.as_ref()
                            && tool_result_only_removes_images(previous.result, result)
                    });
                    if previous_tools
                        .get(id.as_str())
                        .is_some_and(|previous| previous.subagent.is_some() && subagent.is_none())
                    {
                        // A removed subagent sidecar is quarantined per card to avoid rolling back unrelated valid configuration.
                        unattested.push(UnattestedTool {
                            workspace_id: workspace.id.clone(),
                            conversation_id: conversation.id.clone(),
                            context_id: id.clone(),
                            tool_name: tool_name.clone(),
                        });
                        continue;
                    }
                    // The receipt lookup canonicalizes the workspace path and
                    // serializes the full tool payload; only changed contexts
                    // (typically the one just executed) need that attestation.
                    // `ask_user` is a host-owned pause marker, not an external side effect.
                    // The timeline editor may update its valid prompt/options without forging
                    // a new result, but it must preserve both the pending result and question
                    // count so its paired answer remains structurally editable.
                    // The timeline editor may also hide images from an already persisted tool
                    // result. This is intentionally deletion-only: the ordered retained subset
                    // must match byte metadata exactly, while the tool, scope, result text and
                    // sidecars remain immutable.
                    if unchanged || editable_question || image_removal {
                        continue;
                    }
                    // The card's own token is the primary proof and the only
                    // one that survives a restart or a long session: it travels
                    // with the card instead of living in a bounded in-memory
                    // map. The receipt book below stays as the fallback for
                    // cards issued before a token was attached.
                    if state.verify_tool_context(
                        &crate::tool_attestation::AttestationSubject {
                            conversation_id: &conversation.id,
                            context_id: id,
                            tool_name,
                            input,
                            requested_input: requested_input.as_ref(),
                            result,
                            subagent: subagent.as_ref(),
                        },
                        attestation,
                    ) {
                        continue;
                    }
                    let has_receipt = {
                        let exact = match (workspace.kind, subagent.as_ref()) {
                            (WorkspaceKind::Directory, Some(subagent)) => state
                                .has_context_subagent_receipt(
                                    &workspace.path,
                                    &conversation.id,
                                    tool_name,
                                    input,
                                    requested_input.as_ref(),
                                    result,
                                    subagent,
                                ),
                            (_, Some(subagent)) => state
                                .has_context_subagent_receipt_in_any_workspace(
                                    &conversation.id,
                                    tool_name,
                                    input,
                                    requested_input.as_ref(),
                                    result,
                                    subagent,
                                ),
                            (WorkspaceKind::Directory, None) => state.has_context_receipt(
                                &workspace.path,
                                &conversation.id,
                                tool_name,
                                input,
                                requested_input.as_ref(),
                                result,
                            ),
                            (_, None) => state.has_context_receipt_in_any_workspace(
                                &conversation.id,
                                tool_name,
                                input,
                                requested_input.as_ref(),
                                result,
                            ),
                        };
                        exact
                            || state.has_context_receipt_or_image_removal(
                                (workspace.kind == WorkspaceKind::Directory)
                                    .then_some(workspace.path.as_str()),
                                &conversation.id,
                                tool_name,
                                input,
                                requested_input.as_ref(),
                                result,
                                subagent.as_ref(),
                            )
                    };
                    if !has_receipt {
                        // Refusing the whole document here is what turned one
                        // bad card into an unusable app: every later save
                        // re-walked the same card, failed on it again, and took
                        // every unrelated new conversation down with it. The
                        // card is collected instead and stripped below, so the
                        // rest of the save proceeds.
                        unattested.push(UnattestedTool {
                            workspace_id: workspace.id.clone(),
                            conversation_id: conversation.id.clone(),
                            context_id: id.clone(),
                            tool_name: tool_name.clone(),
                        });
                    }
                }
            }
        }
    }
    unattested
}

/// One tool card that could not be attested, addressed well enough to strip it.
#[cfg_attr(test, derive(Debug))]
pub(crate) struct UnattestedTool {
    pub workspace_id: String,
    pub conversation_id: String,
    pub context_id: String,
    pub tool_name: String,
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct ToolResultValidation {
    pub unattested: Vec<UnattestedTool>,
}

#[cfg(test)]
impl ToolResultValidation {
    /// Whether this validation refused every card it saw — the shape the
    /// tampering tests assert. Quarantine changed *how* a forged card is
    /// refused (it is stripped rather than taking the document down with it),
    /// not *whether* it is refused: nothing unattested is ever written.
    fn refused(&self) -> bool {
        !self.unattested.is_empty()
    }

    fn refused_context_ids(&self) -> Vec<&str> {
        self.unattested
            .iter()
            .map(|entry| entry.context_id.as_str())
            .collect()
    }
}

fn tool_result_only_removes_images(previous: &ToolResult, next: &ToolResult) -> bool {
    next.images.len() < previous.images.len()
        && crate::state::tool_result_is_exact_or_image_removal(previous, next)
}

fn unique_nonempty<'a>(
    values: impl Iterator<Item = &'a str>,
    label: &str,
) -> Result<HashSet<&'a str>, String> {
    let mut unique = HashSet::new();
    for value in values {
        if value.trim().is_empty() {
            return Err(format!("{label} 不能为空"));
        }
        if !unique.insert(value) {
            return Err(format!("{label} 重复: {value}"));
        }
    }
    Ok(unique)
}

fn unique_trimmed_nonempty<'a>(
    values: impl Iterator<Item = &'a str>,
    label: &str,
) -> Result<HashSet<&'a str>, String> {
    let mut unique = HashSet::new();
    for value in values {
        let canonical = value.trim();
        if canonical.is_empty() {
            return Err(format!("{label} 不能为空"));
        }
        if !unique.insert(canonical) {
            return Err(format!("{label} 重复: {canonical}"));
        }
    }
    Ok(unique)
}

fn insert_nonempty_unique(
    values: &mut HashSet<String>,
    value: &str,
    label: &str,
) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{label} 不能为空"));
    }
    if !values.insert(value.to_owned()) {
        return Err(format!("{label} 重复: {value}"));
    }
    Ok(())
}

fn insert_runtime_identity_unique(
    values: &mut HashSet<String>,
    value: &str,
    label: &str,
) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{label} 不能为空"));
    }
    if value.trim() != value || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(format!("{label} 必须是无首尾空白且不含控制字符的精确标识"));
    }
    if !values.insert(value.to_owned()) {
        return Err(format!("{label} 重复: {value}"));
    }
    Ok(())
}

fn preserve_corrupt_document(path: &Path) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "数据文档没有父目录".to_owned())?;
    let file_name = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("document");
    let timestamp = Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
    let backup = parent.join(format!("{file_name}.corrupt-{timestamp}.json"));
    fs::copy(path, &backup)
        .map_err(|error| format!("数据文档损坏，且无法保存副本 {}: {error}", backup.display()))?;
    Ok(())
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "写入路径没有父目录".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| format!("无法创建数据目录: {error}"))?;

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("document.json");
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{file_name}.tmp-{}-{sequence}",
        std::process::id()
    ));

    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| format!("无法创建临时数据文档: {error}"))?;
        file.write_all(bytes)
            .map_err(|error| format!("无法写入临时数据文档: {error}"))?;
        file.flush()
            .map_err(|error| format!("无法刷新临时数据文档: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("无法同步临时数据文档: {error}"))?;
        drop(file);
        replace_file(&temporary, path)
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(not(windows))]
fn replace_file(temporary: &Path, destination: &Path) -> Result<(), String> {
    fs::rename(temporary, destination).map_err(|error| format!("无法原子替换数据文档: {error}"))?;
    if let Some(parent) = destination.parent() {
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("无法同步数据目录: {error}"))?;
    }
    Ok(())
}

#[cfg(windows)]
fn replace_file(temporary: &Path, destination: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{ReplaceFileW, REPLACEFILE_WRITE_THROUGH};

    if !destination.exists() {
        match fs::rename(temporary, destination) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => {
                return Err(format!("无法安装数据文档: {error}"));
            }
            Err(_) => {}
        }
    }

    let destination_wide = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let temporary_wide = temporary
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let replaced = unsafe {
        ReplaceFileW(
            destination_wide.as_ptr(),
            temporary_wide.as_ptr(),
            std::ptr::null(),
            REPLACEFILE_WRITE_THROUGH,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if replaced == 0 {
        Err(format!(
            "无法原子替换数据文档: {}",
            std::io::Error::last_os_error()
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        AgentDefinition, AgentDefinitionMemory, AgentModelSelection, ContextItem,
        ConversationBranch, EndpointType, ImageAttachment, ModelProfile, ModelUsage,
        QueuedMessage, ReasoningContent,
        SubagentRunKind, SubagentRunRecord, SubagentRunStatus, ToolExecutionRequest,
        UserAbortedTaskMetrics, UserAbortedTaskRecord,
    };

    /// Builds a document with a role explicitly bound to a provider model.
    fn document_with_bound_role(provider_id: &str, model_id: &str) -> AppDocument {
        let mut document = default_document(Path::new("."));
        if let Some(provider) = document
            .assets
            .api_providers
            .iter_mut()
            .find(|provider| provider.id == provider_id)
        {
            provider.models = vec![test_model(model_id)];
        }
        let mut role = test_agent_definition("mew");
        role.model_selection = AgentModelSelection::Explicit {
            provider_id: provider_id.into(),
            model_id: model_id.into(),
        };
        set_document_agent_definitions(&mut document, vec![role]);
        document
    }

    /// Returns model selection and revision for every role with `name`.
    fn bound_role_states(document: &AppDocument, name: &str) -> Vec<(AgentModelSelection, u64)> {
        agent_definition_lists(document)
            .into_iter()
            .flat_map(|(_, definitions)| {
                definitions
                    .iter()
                    .filter(|definition| definition.name == name)
                    .map(|definition| (definition.model_selection.clone(), definition.revision))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// Disabling a provider leaves its existing role bindings intact; runtime availability is checked separately.
    #[test]
    fn disabling_a_bound_provider_keeps_the_binding_and_still_saves() {
        let previous = document_with_bound_role("openai_responses", "gpt-5");
        let state = AppState::default();
        assert!(validate_save_transition(&previous, &previous, &state).is_ok());

        let mut disabled = previous.clone();
        disabled
            .assets
            .api_providers
            .iter_mut()
            .find(|provider| provider.id == "openai_responses")
            .unwrap()
            .enabled = false;
        // Update the active provider as the UI does when disabling the bound provider.
        disabled.global_settings.active_provider_id = Some("openai_chat".into());

        let canonical = validate_save_transition(&previous, &disabled, &state)
            .expect("停用一家有角色绑在上面的提供商必须仍然能保存");
        let states = bound_role_states(&canonical, "mew");
        assert!(!states.is_empty(), "夹具必须真的种下了角色");
        for (selection, revision) in states {
            assert_eq!(
                selection,
                AgentModelSelection::Explicit {
                    provider_id: "openai_responses".into(),
                    model_id: "gpt-5".into()
                },
                "停用不该改写绑定"
            );
            assert_eq!(revision, 1, "什么都没变，revision 不该动");
        }
    }

    /// Removing a provider row leaves its bindings intact and permits saving.
    #[test]
    fn deleting_the_bound_provider_keeps_the_binding_instead_of_refusing_the_save() {
        let previous = document_with_bound_role("openai_responses", "gpt-5");
        let state = AppState::default();

        let mut removed = previous.clone();
        removed
            .assets
            .api_providers
            .retain(|provider| provider.id != "openai_responses");
        removed.global_settings.active_provider_id = Some("openai_chat".into());

        validate_save_transition(&previous, &removed, &state)
            .expect("删掉一家有角色绑在上面的提供商必须仍然能保存");

        // Assert against the host-canonical document produced at the true save boundary.
        let canonical = prepare_save_transition(&previous, &removed, &state)
            .expect("保存边界必须接受这次删除")
            .document;
        let states = bound_role_states(&canonical, "mew");
        assert!(!states.is_empty(), "预设与对话两处都要被断言到");
        for (selection, revision) in &states {
            assert_eq!(
                *selection,
                AgentModelSelection::Explicit {
                    provider_id: "openai_responses".into(),
                    model_id: "gpt-5".into(),
                },
                "绑定必须原样保留：签出与永久消失在静态数据里分不开"
            );
            assert_eq!(*revision, 1, "用户没改配置，revision 不能涨");
        }
        // The pair the user chose is still readable, so the UI can name it.
        let serialized = serde_json::to_string(&canonical).unwrap();
        assert!(serialized.contains("\"providerId\":\"openai_responses\""));

        // Saving again changes nothing further.
        let again = prepare_save_transition(&canonical, &canonical, &state)
            .unwrap()
            .document;
        assert_eq!(bound_role_states(&again, "mew"), states);
    }

    /// Adopting archived conversations drops retired enabled-tool names so configuration remains writable.
    #[test]
    fn adopting_a_conversation_that_enables_a_retired_tool_drops_the_name_instead_of_refusing() {
        let state = AppState::default();
        let retired = "goal";

        // The host-stored conversation enables a retired tool.
        let mut previous = default_document(Path::new("."));
        let conversation = &mut previous.workspaces[0].conversations[0];
        conversation.settings.enabled_tools.push(retired.into());
        assert!(
            !previous.tools.iter().any(|tool| tool.name == retired),
            "前提：{retired} 已经不在目录里，否则这个测试证明不了什么"
        );

        // Renderer proposals contain no conversations and use the current tool catalog.
        let mut proposal = previous.clone();
        for workspace in &mut proposal.workspaces {
            workspace.conversations = Vec::new();
        }

        let canonical = prepare_save_transition(&previous, &proposal, &state)
            .expect("启用着已退役工具的旧对话必须仍然能保存")
            .document;

        let adopted = &canonical.workspaces[0].conversations[0];
        assert!(
            !adopted.settings.enabled_tools.iter().any(|name| name == retired),
            "退役工具名必须被静默丢弃"
        );
        assert!(
            adopted
                .settings
                .enabled_tools
                .iter()
                .all(|name| canonical.tools.iter().any(|tool| &tool.name == name)),
            "其余启用项必须原样保留"
        );
    }

    /// Conversation writes accept a binding to a removed provider rather than refusing it.
    #[test]
    fn a_conversation_write_keeps_a_stale_binding_instead_of_being_refused() {
        let state = AppState::default();
        let mut committed = document_with_bound_role("openai_responses", "gpt-5");
        committed
            .assets
            .api_providers
            .retain(|provider| provider.id != "openai_responses");
        committed.global_settings.active_provider_id = Some("openai_chat".into());
        committed = prepare_save_transition(&committed.clone(), &committed, &state)
            .unwrap()
            .document;

        // The renderer still proposes the removed provider binding.
        let mut conversation = committed.workspaces[0].conversations[0].clone();
        for definition in &mut conversation.settings.agent_definitions {
            definition.model_selection = AgentModelSelection::Explicit {
                provider_id: "openai_responses".into(),
                model_id: "gpt-5".into(),
            };
        }
        let workspace_id = committed.workspaces[0].id.clone();
        validate_incoming_conversation(&committed, &workspace_id, &mut conversation, &state)
            .expect("一条过期的绑定不该挡住整个对话的写入");
        for definition in &conversation.settings.agent_definitions {
            assert_eq!(
                definition.model_selection,
                AgentModelSelection::Explicit {
                    provider_id: "openai_responses".into(),
                    model_id: "gpt-5".into(),
                }
            );
        }
    }

    /// Writes an archive whose provider models carry `stored` verbatim as their
    /// `reasoningContent` (omitting the key entirely for `None`), then loads it.
    fn load_with_stored_reasoning_content(
        directory: &Path,
        provider_id: &str,
        stored: Option<&str>,
    ) -> ModelProfile {
        let mut document = default_document(Path::new("."));
        let provider = document
            .assets
            .api_providers
            .iter_mut()
            .find(|provider| provider.id == provider_id)
            .expect("种子文档里有这个提供商");
        provider.models = vec![test_model("m")];
        provider.active_model_id = Some("m".into());
        let path = directory.join("document.v1.json");
        save_all(&path, &document).unwrap();

        // Reach past the serializer: the struct can no longer express either of
        // the two shapes this migration exists for.
        let mut anchor: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        for provider in anchor["assets"]["apiProviders"].as_array_mut().unwrap() {
            for model in provider["models"].as_array_mut().unwrap() {
                let model = model.as_object_mut().unwrap();
                match stored {
                    Some(value) => {
                        model.insert("reasoningContent".into(), serde_json::json!(value));
                    }
                    None => {
                        model.remove("reasoningContent");
                    }
                }
            }
        }
        fs::write(&path, serde_json::to_vec_pretty(&anchor).unwrap()).unwrap();

        let loaded = read_document(&path).expect("旧档案必须还能装载");
        // Loading twice must land on the same value: the pre-pass runs on every
        // load, including the one that reads back what it just wrote.
        let reloaded = read_document(&path).unwrap();
        assert_eq!(loaded, reloaded, "迁移不是幂等的");
        loaded
            .assets
            .api_providers
            .iter()
            .find(|provider| provider.id == provider_id)
            .unwrap()
            .models[0]
            .clone()
    }

    /// `reasoningContent` used to be omissible and to have an `auto` variant that
    /// resolved by family at request time. Both are gone, so an archive written
    /// before this change has to be resolved on load or a Responses model
    /// silently stops asking for ciphertext.
    #[test]
    fn an_archive_without_an_explicit_reasoning_form_resolves_by_family_on_load() {
        let directory = tempfile::tempdir().unwrap();
        for (index, stored) in [None, Some("auto")].into_iter().enumerate() {
            let responses = directory.path().join(format!("responses-{index}"));
            std::fs::create_dir_all(&responses).unwrap();
            assert_eq!(
                load_with_stored_reasoning_content(&responses, "openai_responses", stored)
                    .reasoning_content,
                ReasoningContent::Encrypted,
                "Responses 家族的 {stored:?} 必须解析成密文"
            );

            let chat = directory.path().join(format!("chat-{index}"));
            std::fs::create_dir_all(&chat).unwrap();
            assert_eq!(
                load_with_stored_reasoning_content(&chat, "openai_chat", stored).reasoning_content,
                ReasoningContent::Plaintext,
                "Chat 家族的 {stored:?} 必须解析成明文"
            );
        }
    }

    /// An explicit form is the user's own choice and outranks the family default.
    #[test]
    fn an_explicit_reasoning_form_survives_the_load_migration_untouched() {
        let directory = tempfile::tempdir().unwrap();
        let plaintext = directory.path().join("plaintext");
        std::fs::create_dir_all(&plaintext).unwrap();
        assert_eq!(
            load_with_stored_reasoning_content(&plaintext, "openai_responses", Some("plaintext"))
                .reasoning_content,
            ReasoningContent::Plaintext,
            "Responses 家族上的明文是用户的显式选择，迁移不该改写它"
        );

        let encrypted = directory.path().join("encrypted");
        std::fs::create_dir_all(&encrypted).unwrap();
        assert_eq!(
            load_with_stored_reasoning_content(&encrypted, "openai_chat", Some("encrypted"))
                .reasoning_content,
            ReasoningContent::Encrypted,
            "Chat 家族上的密文同理"
        );
    }

    /// The capability catalog shrank to a single slug. A retired slug is an unknown
    /// enum *variant* rather than an unknown field, so serde rejects the entire
    /// anchor instead of skipping the entry — without the load-time prune the app
    /// quarantines the document and rebuilds it empty, and every provider the user
    /// had configured disappears.
    #[test]
    fn an_archive_carrying_retired_capability_slugs_still_loads() {
        let directory = tempfile::tempdir().unwrap();
        let mut document = default_document(Path::new("."));
        let provider = document
            .assets
            .api_providers
            .iter_mut()
            .find(|provider| provider.id == "openai_responses")
            .expect("种子文档里有这个提供商");
        provider.models = vec![test_model("m")];
        provider.active_model_id = Some("m".into());
        let path = directory.path().join("document.v1.json");
        save_all(&path, &document).unwrap();

        // Reach past the serializer: the enum can no longer express the retired slugs.
        let mut anchor: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        for provider in anchor["assets"]["apiProviders"].as_array_mut().unwrap() {
            for model in provider["models"].as_array_mut().unwrap() {
                model.as_object_mut().unwrap().insert(
                    "capabilities".into(),
                    serde_json::json!([
                        "function_call",
                        "image_recognition",
                        "reasoning",
                        "image_generation",
                        "audio_generation",
                        "audio_transcript",
                        "embedding",
                        "rerank"
                    ]),
                );
            }
        }
        fs::write(&path, serde_json::to_vec_pretty(&anchor).unwrap()).unwrap();

        let loaded = read_document(&path).expect("带退役能力槽的旧档案必须还能装载");
        let reloaded = read_document(&path).unwrap();
        assert_eq!(loaded, reloaded, "迁移不是幂等的");
        assert_eq!(
            loaded
                .assets
                .api_providers
                .iter()
                .find(|provider| provider.id == "openai_responses")
                .expect("提供商必须还在")
                .models[0]
                .capabilities,
            std::collections::BTreeSet::from([crate::model::ModelCapability::ImageRecognition]),
            "退役的槽必须被丢掉，只留下还在目录里的视觉输入"
        );
    }

    /// Loading keeps dangling bindings readable instead of rejecting the document.
    #[test]
    fn loading_keeps_a_dangling_binding_instead_of_dropping_the_conversation() {
        let directory = tempfile::tempdir().unwrap();
        let mut document = document_with_bound_role("openai_responses", "gpt-5");
        let conversation_id = document.workspaces[0].conversations[0].id.clone();
        let path = directory.path().join("document.v1.json");
        save_all(&path, &document).unwrap();

        // The stored conversation still contains the binding after its provider is removed.
        document
            .assets
            .api_providers
            .retain(|provider| provider.id != "openai_responses");
        document.global_settings.active_provider_id = Some("openai_chat".into());
        save_all(&path, &document).unwrap();

        let loaded = read_document(&path).expect("悬空绑定不该让文档装不起来");
        assert!(
            loaded.workspaces[0]
                .conversations
                .iter()
                .any(|conversation| conversation.id == conversation_id),
            "对话不该在装载期被丢掉"
        );
        for (selection, _) in bound_role_states(&loaded, "mew") {
            assert_eq!(
                selection,
                AgentModelSelection::Explicit {
                    provider_id: "openai_responses".into(),
                    model_id: "gpt-5".into(),
                },
                "装回这家提供商后角色要能自己恢复，所以两个 id 必须留着"
            );
        }
    }

    /// A role bound to a model the provider has not fetched yet survives a full
    /// save/load cycle and starts resolving once the model row appears. This is
    /// the seeded-Codex-role case: signed out at first launch, working later.
    #[test]
    fn a_binding_to_an_unfetched_model_survives_and_recovers() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.v1.json");
        let mut document = document_with_bound_role("openai_responses", "not-fetched-yet");
        document
            .assets
            .api_providers
            .iter_mut()
            .find(|provider| provider.id == "openai_responses")
            .expect("提供商必须在")
            .models
            .clear();
        save_all(&path, &document).unwrap();

        let loaded = read_document(&path).expect("未拉取的模型不该让文档装不起来");
        for (selection, revision) in bound_role_states(&loaded, "mew") {
            assert_eq!(
                selection,
                AgentModelSelection::Explicit {
                    provider_id: "openai_responses".into(),
                    model_id: "not-fetched-yet".into(),
                }
            );
            assert_eq!(revision, 1, "宿主没改配置，revision 不能涨");
        }
    }

    fn test_model(id: &str) -> ModelProfile {
        ModelProfile {
            id: id.into(),
            name: String::new(),
            group: String::new(),
            context_window: None,
            max_output_tokens: None,
            capabilities: Default::default(),
            reasoning_content: Default::default(),
        }
    }

    fn test_agent_definition(name: &str) -> AgentDefinition {
        AgentDefinition {
            enabled: true,
            deleted: false,
            name: name.into(),
            description: String::new(),
            source: AgentDefinitionSource::User,
            source_key: String::new(),
            revision: 1,
            memory_epoch: 1,
            model_selection: AgentModelSelection::Inherit,
            memory: AgentDefinitionMemory::None,
            effort: None,
            tools: None,
            disallowed_tools: Vec::new(),
            search_provider: None,
        }
    }

    fn set_document_agent_definitions(
        document: &mut AppDocument,
        definitions: Vec<AgentDefinition>,
    ) {
        // Presets are templates; conversations own their settings. Seeding both
        // keeps every identity-ledger list under test populated the same way.
        for preset in &mut document.presets.conversation_presets {
            preset.settings.agent_definitions = definitions.clone();
        }
        for workspace in &mut document.workspaces {
            for conversation in &mut workspace.conversations {
                conversation.settings.agent_definitions = definitions.clone();
            }
        }
    }

    /// Mirrors the seed helper for a targeted edit: applies `edit` to the role
    /// at `index` in EVERY location.
    fn edit_document_agent_definition(
        document: &mut AppDocument,
        index: usize,
        edit: impl Fn(&mut AgentDefinition),
    ) {
        for preset in &mut document.presets.conversation_presets {
            if let Some(definition) = preset.settings.agent_definitions.get_mut(index) {
                edit(definition);
            }
        }
        for workspace in &mut document.workspaces {
            for conversation in &mut workspace.conversations {
                if let Some(definition) =
                    conversation.settings.agent_definitions.get_mut(index)
                {
                    edit(definition);
                }
            }
        }
    }

    fn default_conversation_preset_mut(
        document: &mut AppDocument,
    ) -> &mut ConversationPresetSettings {
        let id = document
            .presets.default_conversation_preset_id
            .clone();
        &mut document
            .presets.conversation_presets
            .iter_mut()
            .find(|preset| preset.id == id)
            .expect("default conversation preset must exist")
            .settings
    }

    fn user_context(id: &str) -> ContextItem {
        ContextItem::User {
            id: id.into(),
            content: format!("message from {id}"),
            images: Vec::new(),
            created_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn context_source_metadata_is_optional_and_does_not_define_timeline_structure() {
        let assistant = ContextItem::Assistant {
            id: "assistant-anchor".into(),
            content: String::new(),
            round: Some(1),
            model_turn_id: Some("turn-one".into()),
            interrupted: false,
            sources: Vec::new(),
            created_at: "2026-07-21T00:00:00Z".into(),
        };
        let tool = ContextItem::Tool {
            id: "local-tool-id".into(),
            tool_name: "read".into(),
            round: Some(1),
            model_turn_id: Some("turn-one".into()),
            requested_input: None,
            input: serde_json::json!({"path":"a.txt"})
                .as_object()
                .unwrap()
                .clone(),
            result: ToolResult {
                success: true,
                output: "ok".into(),
                images: Vec::new(),
                diff: None,
                executed_at: "2026-07-21T00:00:01Z".into(),
                duration_ms: 1,
            },
            subagent: None,
            attestation: String::new(),
            created_at: "2026-07-21T00:00:01Z".into(),
        };

        assert!(validate_context_source_metadata(&[assistant.clone(), tool.clone()]).is_ok());
        assert!(validate_context_source_metadata(&[tool.clone()]).is_ok());

        let mut wrong_tool = tool;
        if let ContextItem::Tool { round, .. } = &mut wrong_tool {
            *round = Some(2);
        }
        assert!(validate_context_source_metadata(&[assistant.clone(), wrong_tool]).is_ok());
        assert!(validate_context_source_metadata(&[assistant.clone(), assistant]).is_ok());

        let invalid = ContextItem::Assistant {
            id: "invalid-source-id".into(),
            content: String::new(),
            round: None,
            model_turn_id: Some("   ".into()),
            interrupted: false,
            sources: Vec::new(),
            created_at: "2026-07-21T00:00:02Z".into(),
        };
        assert!(validate_context_source_metadata(&[invalid])
            .unwrap_err()
            .contains("modelTurnId"));
    }

    fn ask_user_tool_context(context_id: &str) -> ContextItem {
        ContextItem::Tool {
            id: context_id.into(),
            tool_name: "ask_user".into(),
            round: Some(1),
            model_turn_id: None,
            requested_input: None,
            input: serde_json::from_value(serde_json::json!({
                "questions": [{
                    "question": "Continue?",
                    "header": "Choice",
                    "options": [
                        { "label": "Yes", "description": "Continue" },
                        { "label": "No", "description": "Stop" }
                    ],
                    "multiSelect": false
                }]
            }))
            .unwrap(),
            result: ToolResult {
                success: true,
                output: crate::prompt_profile::PromptKey::TaskAskUserPending
                    .builtin_en()
                    .into(),
                images: Vec::new(),
                diff: None,
                executed_at: "2026-01-01T00:02:00Z".into(),
                duration_ms: 0,
            },
            subagent: None,
            attestation: String::new(),
            created_at: "2026-01-01T00:02:00Z".into(),
        }
    }

    fn subagent_record_tool(id: &str, name: &str, contexts: Vec<ContextItem>) -> ContextItem {
        ContextItem::Tool {
            id: id.into(),
            tool_name: "agent_spawn".into(),
            round: Some(1),
            model_turn_id: None,
            requested_input: None,
            input: serde_json::from_value(serde_json::json!({
                "prompt": format!("task for {name}"),
                "name": name
            }))
            .unwrap(),
            result: ToolResult {
                success: true,
                output: format!("子代理 {name} 已派生"),
                images: Vec::new(),
                diff: None,
                executed_at: "2026-01-01T00:02:00Z".into(),
                duration_ms: 1,
            },
            subagent: Some(SubagentRunRecord {
                kind: crate::model::SubagentRunKind::General,
                name: Some(name.into()),
                label: None,
                inherits_model_memory: false,
                fork_model_binding: None,
                agent_definition: None,
                execution_mode_receipt: String::new(),
                task: format!("task for {name}"),
                status: SubagentRunStatus::Completed,
                contexts,
                updates: Vec::new(),
                queued_messages: Vec::new(),
                structured_output: None,
                output_schema: None,
                usage: ModelUsage::default(),
            }),
            attestation: String::new(),
            created_at: "2026-01-01T00:02:00Z".into(),
        }
    }

    fn subagent_nested_tool(id: &str) -> ContextItem {
        ContextItem::Tool {
            id: id.into(),
            tool_name: "read".into(),
            round: Some(1),
            model_turn_id: Some("child-turn".into()),
            requested_input: None,
            input: serde_json::from_value(serde_json::json!({
                "path": "README.md"
            }))
            .unwrap(),
            result: ToolResult {
                success: true,
                output: "trusted child output".into(),
                images: Vec::new(),
                diff: None,
                executed_at: "2026-07-24T00:00:00Z".into(),
                duration_ms: 2,
            },
            subagent: None,
            attestation: String::new(),
            created_at: "2026-07-24T00:00:00Z".into(),
        }
    }

    /// Validates registry shape at save time so unusable servers are rejected before model requests.
    #[test]
    fn validate_shape_bounds_execution_environments() {
        let directory = tempfile::tempdir().unwrap();
        let base = default_document(directory.path());

        let machine = crate::model::SshMachineConfig {
            id: "ssh_a".into(),
            name: "devbox".into(),
            host: "user@devbox.local".into(),
            port: 2222,
            ..Default::default()
        };

        let mut valid = base.clone();
        valid.assets.execution_environments.ssh_machines = vec![machine.clone()];
        valid
            .assets
            .execution_environments
            .env_vars
            .insert("local".into(), [("FOO".to_owned(), "bar".to_owned())].into());
        valid
            .assets
            .execution_environments
            .env_vars
            .insert("wsl:Ubuntu".into(), [("A".to_owned(), "1".to_owned())].into());
        valid
            .assets
            .execution_environments
            .env_vars
            .insert("ssh:ssh_a".into(), Default::default());
        assert!(validate_shape(&valid).is_ok(), "完整配置必须被接受");

        // Dangling SSH environment tables remain valid.
        let mut dangling = base.clone();
        dangling
            .assets
            .execution_environments
            .env_vars
            .insert("ssh:gone".into(), Default::default());
        assert!(validate_shape(&dangling).is_ok());

        let mut bad_host = base.clone();
        bad_host.assets.execution_environments.ssh_machines = vec![crate::model::SshMachineConfig {
            host: "devbox --evil".into(),
            ..machine.clone()
        }];
        assert!(validate_shape(&bad_host).is_err(), "主机地址不能包含空白");

        let mut option_host = base.clone();
        option_host.assets.execution_environments.ssh_machines =
            vec![crate::model::SshMachineConfig {
                host: "-oProxyCommand=calc".into(),
                ..machine.clone()
            }];
        assert!(validate_shape(&option_host).is_err(), "主机地址不能以 - 开头");

        let mut bad_key = base.clone();
        bad_key
            .assets
            .execution_environments
            .env_vars
            .insert("docker:x".into(), Default::default());
        assert!(validate_shape(&bad_key).is_err(), "未知环境键必须被拒绝");

        let mut bad_name = base.clone();
        bad_name
            .assets
            .execution_environments
            .env_vars
            .insert("local".into(), [("1BAD".to_owned(), "x".to_owned())].into());
        assert!(validate_shape(&bad_name).is_err(), "非法变量名必须被拒绝");

        let mut reserved = base.clone();
        reserved.assets.execution_environments.env_vars.insert(
            "local".into(),
            [("MEWORK_BROWSER_DEV_TOKEN".to_owned(), "x".to_owned())].into(),
        );
        assert!(validate_shape(&reserved).is_err(), "harness 私有名必须被拒绝");

        let mut control_value = base.clone();
        control_value
            .assets
            .execution_environments
            .env_vars
            .insert("local".into(), [("FOO".to_owned(), "a
b".to_owned())].into());
        assert!(validate_shape(&control_value).is_err(), "值里的控制字符必须被拒绝");

        // Conversation targets validate shape only: dangling machine IDs pass, invalid distro names do not.
        let mut wsl_target = base.clone();
        if let Some(conversation) = wsl_target
            .workspaces
            .first_mut()
            .and_then(|workspace| workspace.conversations.first_mut())
        {
            conversation.run_target = Some(crate::model::RunTarget::Wsl {
                distro: "Ubuntu".into(),
            });
        }
        assert!(validate_shape(&wsl_target).is_ok());

        let mut bad_distro = base.clone();
        if let Some(conversation) = bad_distro
            .workspaces
            .first_mut()
            .and_then(|workspace| workspace.conversations.first_mut())
        {
            conversation.run_target = Some(crate::model::RunTarget::Wsl {
                distro: "Ubuntu; rm -rf /".into(),
            });
        }
        assert!(validate_shape(&bad_distro).is_err(), "非法发行版名必须被拒绝");

        let mut dangling_machine = base.clone();
        if let Some(conversation) = dangling_machine
            .workspaces
            .first_mut()
            .and_then(|workspace| workspace.conversations.first_mut())
        {
            conversation.run_target = Some(crate::model::RunTarget::Ssh {
                machine_id: "gone".into(),
            });
        }
        assert!(
            validate_shape(&dangling_machine).is_ok(),
            "悬空的机器绑定在持久层放行，由派发时报错"
        );
    }

    #[test]
    fn validate_shape_bounds_the_schema_91_registries() {
        let directory = tempfile::tempdir().unwrap();
        let base = default_document(directory.path());

        let stdio_server = crate::model::McpServerConfig {
            id: "mcp_a".into(),
            name: "Files".into(),
            enabled: true,
            transport: crate::model::McpTransportKind::Stdio,
            command: "npx".into(),
            ..Default::default()
        };

        let mut valid = base.clone();
        valid.assets.mcp_servers = vec![stdio_server.clone()];
        assert!(validate_shape(&valid).is_ok(), "一台配置完整的 stdio 服务器必须被接受");

        let mut no_command = base.clone();
        no_command.assets.mcp_servers = vec![crate::model::McpServerConfig {
            command: String::new(),
            ..stdio_server.clone()
        }];
        assert!(validate_shape(&no_command).is_err(), "stdio 服务器缺少启动命令必须被拒绝");

        let mut no_url = base.clone();
        no_url.assets.mcp_servers = vec![crate::model::McpServerConfig {
            id: "mcp_http".into(),
            transport: crate::model::McpTransportKind::StreamableHttp,
            command: String::new(),
            ..stdio_server.clone()
        }];
        assert!(validate_shape(&no_url).is_err(), "HTTP 服务器缺少地址必须被拒绝");

        // Server names prefix model-visible tools and must be unique.
        let mut duplicate_name = base.clone();
        duplicate_name.assets.mcp_servers = vec![
            stdio_server.clone(),
            crate::model::McpServerConfig {
                id: "mcp_b".into(),
                name: "files".into(),
                ..stdio_server.clone()
            },
        ];
        assert!(validate_shape(&duplicate_name).is_err(), "MCP 服务器重名必须被拒绝");

        // Skill folder names are single path components.
        for folder in ["..", "a/b", "a\\b", ""] {
            let mut escaping = base.clone();
            escaping.assets.skills = vec![crate::model::SkillRecord {
                id: "skill_a".into(),
                name: "Demo".into(),
                folder_name: folder.into(),
                ..Default::default()
            }];
            assert!(
                validate_shape(&escaping).is_err(),
                "技能目录名 {folder:?} 必须被拒绝"
            );
        }

        // Environment-tool executables are bare names used for process startup.
        let mut path_executable = base.clone();
        path_executable.global_settings.environment_tools =
            vec![crate::model::EnvironmentToolDefinition {
                name: "evil".into(),
                executable: "../../bin/sh".into(),
                version_args: Vec::new(),
            }];
        assert!(validate_shape(&path_executable).is_err(), "可执行名不能是一条路径");
    }

    /// Only servers enabled in the document and selected by the conversation are dialed.
    #[test]
    fn only_enabled_and_selected_mcp_servers_are_dialed() {
        let directory = tempfile::tempdir().unwrap();
        let mut document = default_document(directory.path());
        let template = crate::model::McpServerConfig {
            transport: crate::model::McpTransportKind::Stdio,
            command: "node".into(),
            ..Default::default()
        };
        document.assets.mcp_servers = vec![
            crate::model::McpServerConfig {
                id: "on_and_picked".into(),
                name: "Picked".into(),
                enabled: true,
                ..template.clone()
            },
            crate::model::McpServerConfig {
                id: "on_not_picked".into(),
                name: "Unpicked".into(),
                enabled: true,
                ..template.clone()
            },
            crate::model::McpServerConfig {
                id: "off_but_picked".into(),
                name: "Disabled".into(),
                enabled: false,
                ..template.clone()
            },
        ];
        let conversation = &mut document.workspaces[0].conversations[0];
        // Exclude disabled, dangling, and forged selections.
        conversation.settings.mcp_ids = vec![
            "on_and_picked".into(),
            "off_but_picked".into(),
            "never_existed".into(),
        ];
        let conversation = document.workspaces[0].conversations[0].clone();

        let dialed = crate::mcp::selected_runtime_servers(&document, &conversation);

        assert_eq!(
            dialed.iter().map(|server| server.server_id.as_str()).collect::<Vec<_>>(),
            vec!["on_and_picked"]
        );

        // No selection produces no runtime server and skips tool attachment.
        let mut none_selected = conversation.clone();
        none_selected.settings.mcp_ids = Vec::new();
        assert!(crate::mcp::selected_runtime_servers(&document, &none_selected).is_empty());
    }

    /// Only servers enabled in the document and selected by the conversation are dialed.
    #[test]
    fn a_runtime_server_mirrors_only_the_dialable_fields_of_its_config() {
        let config = crate::model::McpServerConfig {
            id: "mcp_files".into(),
            name: "Files".into(),
            description: "本地文件".into(),
            enabled: true,
            transport: crate::model::McpTransportKind::Stdio,
            command: "npx".into(),
            args: vec!["-y".into(), "server-filesystem".into()],
            env: [("TOKEN".to_owned(), "secret".to_owned())].into_iter().collect(),
            url: "https://ignored.example".into(),
            ..Default::default()
        };

        let runtime = crate::mcp::RuntimeMcpServer::from_config(&config);

        assert_eq!(runtime.server_id, "mcp_files");
        assert_eq!(runtime.artifact_id, "mcp_files");
        match &runtime.transport {
            crate::mcp::RuntimeMcpTransport::Stdio { command, args, env, cwd } => {
                assert_eq!(command, "npx");
                assert_eq!(args, &["-y".to_owned(), "server-filesystem".to_owned()]);
                assert_eq!(env.get("TOKEN").map(String::as_str), Some("secret"));
                // Working directories are not configurable to prevent pinning a server to a workspace.
                assert!(cwd.is_none());
            }
            other => panic!("stdio 配置必须映射成 stdio 传输，得到 {other:?}"),
        }

        // Redaction is required because command lines and environment variables can leak through logs and crash reports.
        let debug = format!("{runtime:?}");
        assert!(!debug.contains("secret"), "环境变量不能出现在 Debug 输出里: {debug}");
        assert!(!debug.contains("npx"), "启动命令不能出现在 Debug 输出里: {debug}");

        let http = crate::mcp::RuntimeMcpServer::from_config(&crate::model::McpServerConfig {
            transport: crate::model::McpTransportKind::StreamableHttp,
            url: "https://example.test/mcp".into(),
            headers: [("Authorization".to_owned(), "Bearer x".to_owned())]
                .into_iter()
                .collect(),
            ..config
        });
        match &http.transport {
            crate::mcp::RuntimeMcpTransport::Http { url, headers } => {
                assert_eq!(url, "https://example.test/mcp");
                assert_eq!(headers.len(), 1);
            }
            other => panic!("HTTP 配置必须映射成 HTTP 传输，得到 {other:?}"),
        }
    }

    /// Search-provider IDs share credential identity by provider ID, so the namespace must remain reserved.
    #[test]
    fn a_user_provider_cannot_claim_the_search_provider_namespace() {
        let directory = tempfile::tempdir().unwrap();
        let document = default_document(directory.path());
        for stolen in [
            "search-provider:tavily",
            "search-provider:searxng:basic-auth",
            "search-provider:",
            "search-provider:anything",
        ] {
            let mut colliding = document.clone();
            let mut provider = colliding.assets.api_providers[0].clone();
            provider.id = stolen.to_owned();
            colliding.assets.api_providers.push(provider);
            colliding.global_settings.active_provider_id = None;
            let error = validate_shape(&colliding).unwrap_err();
            assert!(
                error.contains("搜索提供商保留命名空间"),
                "{stolen} 必须因命名空间冲突被拒：{error}"
            );
        }
        // Ordinary provider IDs remain valid.
        let mut ordinary = document;
        let mut extra = ordinary.assets.api_providers[0].clone();
        extra.id = "6f1d0b2c-6c1e-4a2f-9a4a-6c1e4a2f9a4a".into();
        ordinary.assets.api_providers.push(extra);
        validate_shape(&ordinary).expect("an ordinary provider ID still saves");
    }

    #[test]
    fn search_provider_rows_are_unique_per_catalog_kind_with_sane_overrides() {
        use crate::model::{SearchProviderConfig, SearchProviderKind};
        let directory = tempfile::tempdir().unwrap();
        let document = default_document(directory.path());
        let row = |document: &crate::model::AppDocument, kind: SearchProviderKind| {
            document
                .assets
                .web_search
                .providers
                .iter()
                .position(|entry| entry.kind == kind)
                .expect("目录里有这一行")
        };

        // One catalog kind maps to one credential-store identity.
        let mut duplicated = document.clone();
        duplicated
            .assets
            .web_search
            .providers
            .push(SearchProviderConfig::new(SearchProviderKind::Tavily));
        assert_eq!(
            validate_shape(&duplicated).unwrap_err(),
            "搜索提供商重复：tavily"
        );

        // Endpoint hosts enter request URLs; whitespace and control characters are injection vectors.
        let tavily = row(&document, SearchProviderKind::Tavily);
        for refused in ["https://example.com/ a", "https://example.com/\na"] {
            let mut invalid = document.clone();
            invalid.assets.web_search.providers[tavily].search_api_host = refused.to_owned();
            assert_eq!(
                validate_shape(&invalid).unwrap_err(),
                "搜索提供商 tavily 的搜索端点不能包含空白或控制字符"
            );
        }

        // Fetch hosts use the same rule independently.
        let jina = row(&document, SearchProviderKind::Jina);
        let mut invalid_fetch = document.clone();
        invalid_fetch.assets.web_search.providers[jina].fetch_api_host = "h ttps://r.jina.ai".into();
        assert_eq!(
            validate_shape(&invalid_fetch).unwrap_err(),
            "搜索提供商 jina 的抓取端点不能包含空白或控制字符"
        );

        // Engine names enter SearXNG query strings.
        let searxng = row(&document, SearchProviderKind::Searxng);
        let mut invalid_engine = document.clone();
        invalid_engine.assets.web_search.providers[searxng].engines = vec!["goo\ngle".into()];
        assert_eq!(
            validate_shape(&invalid_engine).unwrap_err(),
            "搜索提供商 searxng 的引擎名无效"
        );

        // A fetch provider must support fetching.
        let mut impossible_fetch = document.clone();
        impossible_fetch.assets.web_search.fetch_provider = Some(SearchProviderKind::Tavily);
        assert_eq!(
            validate_shape(&impossible_fetch).unwrap_err(),
            "搜索提供商 tavily 不提供网页抓取，不能作为抓取提供商"
        );

        // The global settings each have an independent bound.
        let mut zero_results = document.clone();
        zero_results.assets.web_search.max_results = 0;
        assert_eq!(
            validate_shape(&zero_results).unwrap_err(),
            "搜索结果数必须在 1 到 50 之间"
        );

        // A valid override confirms the preceding rejection assertions are specific.
        let mut ordinary = document;
        ordinary.assets.web_search.providers[tavily].enabled = true;
        ordinary.assets.web_search.providers[tavily].search_api_host =
            "https://gateway.example/tavily".into();
        ordinary.assets.web_search.providers[searxng].engines = vec!["google".into()];
        ordinary.assets.web_search.providers[searxng].basic_auth_username = "searx".into();
        ordinary.assets.web_search.fetch_provider = Some(SearchProviderKind::Jina);
        ordinary.assets.web_search.max_results = 8;
        ordinary.assets.web_search.exclude_domains = vec!["*://ads.example/*".into()];
        validate_shape(&ordinary).expect("an ordinary provider override still saves");
    }

    #[test]
    fn persisted_workspace_and_conversation_ids_reject_whitespace_aliases() {
        let directory = tempfile::tempdir().unwrap();
        let document = default_document(directory.path());

        for value in [" workspace-id", "workspace-id ", "workspace\nid"] {
            let mut invalid = document.clone();
            invalid.workspaces[0].id = value.to_owned();
            let error = validate_shape(&invalid).unwrap_err();
            assert!(error.contains("工作区 ID"), "{error}");
        }

        for value in [" conversation-id", "conversation-id ", "conversation\nid"] {
            let mut invalid = document.clone();
            invalid.workspaces[0].conversations[0].id = value.to_owned();
            let error = validate_shape(&invalid).unwrap_err();
            assert!(error.contains("对话 ID"), "{error}");
        }
    }

    #[test]
    fn image_only_contexts_and_queue_survive_reload_without_inline_or_present_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let mut document = default_document(directory.path());
        let image = ImageAttachment {
            id: "b".repeat(64),
            name: "missing-sidecar.png".into(),
            mime: "image/png".into(),
            width: 32,
            height: 16,
            bytes: 123,
            short_id: None,
        };
        let conversation = &mut document.workspaces[0].conversations[0];
        conversation.contexts.push(ContextItem::User {
            id: "image-only-user".into(),
            content: String::new(),
            images: vec![image.clone()],
            created_at: "2026-07-24T00:00:00Z".into(),
        });
        conversation.queued_messages.push(QueuedMessage {
            id: "image-only-queued".into(),
            content: String::new(),
            images: vec![image],
            created_at: "2026-07-24T00:00:01Z".into(),
        });
        assert!(validate_shape(&document).is_ok());
        let serialized = serde_json::to_string(&document).unwrap();
        assert!(!serialized.contains("data:image/"));
        assert!(!serialized.contains("iVBOR"));

        let path = directory.path().join("document.json");
        save_all(&path, &document).unwrap();
        let reloaded = read_document(&path).unwrap();
        assert_eq!(reloaded, document);

        let mut forged = document;
        let ContextItem::User { images, .. } = forged.workspaces[0].conversations[0]
            .contexts
            .last_mut()
            .unwrap()
        else {
            unreachable!()
        };
        images[0].width = 0;
        assert!(validate_shape(&forged)
            .unwrap_err()
            .contains("has an invalid image attachment"));
    }

    #[test]
    fn persisted_image_lists_enforce_aggregate_limits_for_every_owner() {
        fn image(index: usize, bytes: u64, width: u32, height: u32) -> ImageAttachment {
            ImageAttachment {
                id: format!("{:064x}", index + 1),
                name: format!("image-{index}.png"),
                mime: "image/png".into(),
                width,
                height,
                bytes,
                short_id: None,
            }
        }

        let directory = tempfile::tempdir().unwrap();
        let exact_limit = (0..4)
            .map(|index| {
                image(
                    index,
                    crate::image_attachments::MAX_IMAGE_ATTACHMENT_BYTES as u64,
                    4096,
                    4096,
                )
            })
            .collect::<Vec<_>>();
        let mut exact = default_document(directory.path());
        exact.workspaces[0].conversations[0]
            .contexts
            .push(ContextItem::User {
                id: "aggregate-user-exact".into(),
                content: String::new(),
                images: exact_limit.clone(),
                created_at: "2026-07-24T00:00:00Z".into(),
            });
        assert!(validate_shape(&exact).is_ok());

        let mut oversized_user = default_document(directory.path());
        oversized_user.workspaces[0].conversations[0]
            .contexts
            .push(ContextItem::User {
                id: "aggregate-user-bytes".into(),
                content: String::new(),
                images: (0..5)
                    .map(|index| {
                        image(
                            index,
                            crate::image_attachments::MAX_IMAGE_ATTACHMENT_BYTES as u64,
                            1,
                            1,
                        )
                    })
                    .collect(),
                created_at: "2026-07-24T00:00:00Z".into(),
            });
        let error = validate_shape(&oversized_user).unwrap_err();
        assert!(error.contains("用户上下文 aggregate-user-bytes"));
        assert!(error.contains("20 MiB"));

        let mut oversized_queue = default_document(directory.path());
        oversized_queue.workspaces[0].conversations[0]
            .queued_messages
            .push(QueuedMessage {
                id: "aggregate-queue-pixels".into(),
                content: String::new(),
                images: (0..5).map(|index| image(index, 1, 4096, 4096)).collect(),
                created_at: "2026-07-24T00:00:00Z".into(),
            });
        let error = validate_shape(&oversized_queue).unwrap_err();
        assert!(error.contains("排队消息 aggregate-queue-pixels"));
        assert!(error.contains("64 MP"));

        let mut oversized_tool = default_document(directory.path());
        let ContextItem::Tool { result, .. } = oversized_tool.workspaces[0].conversations[0]
            .contexts
            .iter_mut()
            .find(|context| matches!(context, ContextItem::Tool { .. }))
            .unwrap()
        else {
            unreachable!()
        };
        result.images = (0..=crate::image_attachments::MAX_REQUEST_IMAGES)
            .map(|index| image(index, 1, 1, 1))
            .collect();
        let error = validate_shape(&oversized_tool).unwrap_err();
        assert!(error.contains("工具上下文"));
        assert!(error.contains("more than 20 image attachments"), "{error}");
    }

    #[test]
    fn atomic_save_round_trips() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state").join("document.v1.json");
        let document = default_document(directory.path());
        let store = crate::conversation_store::store_for(&path).unwrap();
        seed_conversations(&store, &document).unwrap();
        save_all(&path, &document).unwrap();
        assert_eq!(read_document(&path).unwrap(), document);
    }

    #[test]
    fn conversation_preset_references_are_validated_and_settings_stand_alone() {
        let directory = tempfile::tempdir().unwrap();
        let document = default_document(directory.path());

        let mut missing_default = document.clone();
        missing_default
            .presets.default_conversation_preset_id = "missing".into();
        assert!(validate_shape(&missing_default)
            .unwrap_err()
            .contains("新对话默认预设不存在"));

        let mut unknown_tool = document.clone();
        default_conversation_preset_mut(&mut unknown_tool)
            .enabled_tools
            .push("unknown-tool".into());
        assert!(validate_shape(&unknown_tool)
            .unwrap_err()
            .contains("引用了未知工具"));

        // Presets are templates; conversations may legitimately diverge from all of them.
        let mut diverged = document;
        diverged.workspaces[0].conversations[0]
            .settings
            .system_prompt
            .push_str(" changed");
        assert!(validate_shape(&diverged).is_ok());
    }

    #[test]
    fn zero_conversation_presets_use_an_empty_default_reference() {
        let mut document = default_document(Path::new("."));
        document.presets.conversation_presets.clear();
        document
            .presets.default_conversation_preset_id
            .clear();
        assert!(validate_shape(&document).is_ok());

        document.presets.default_conversation_preset_id = "missing".into();
        assert!(validate_shape(&document).unwrap_err().contains("必须为空"));
    }

    #[test]
    fn user_aborted_task_records_round_trip_and_validate() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.json");
        let mut document = default_document(directory.path());
        document.workspaces[0].conversations[0].user_aborted_tasks = vec![
            UserAbortedTaskRecord {
                id: "abort-1".into(),
                source_kind: "shell".into(),
                source_identity: "shell:task-1".into(),
                label: "bash".into(),
                detail: "npm test".into(),
                metrics: UserAbortedTaskMetrics {
                    child_count: None,
                    tokens: None,
                    tool_count: None,
                    elapsed_ms: Some(1000),
                },
                started_at: "2026-08-11T00:00:00Z".into(),
                ended_at: "2026-08-11T00:00:01Z".into(),
                reason: "userAborted".into(),
            },
        ];

        validate_shape(&document).unwrap();
        save_all(&path, &document).unwrap();
        let restored = read_document(&path).unwrap();
        assert_eq!(restored.workspaces[0].conversations[0].user_aborted_tasks.len(), 1);

        let mut invalid = document;
        invalid.workspaces[0].conversations[0].user_aborted_tasks[0].reason = "failed".into();
        assert!(validate_shape(&invalid).unwrap_err().contains("原因无效"));
    }

    #[test]
    fn product_default_document_ships_no_conversations_and_round_trips() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.template-preset.json");
        let document = crate::catalog::product_default_document(directory.path());

        // Product seeds contain no conversations; the renderer creates the first real conversation from the first message.
        assert!(document
            .workspaces
            .iter()
            .all(|workspace| workspace.conversations.is_empty()));

        validate_and_save(&path, &document, &document, &AppState::default()).unwrap();
        let restored = read_document(&path).unwrap();
        assert_eq!(restored.workspaces.len(), document.workspaces.len());
        // Assert deprecated link fields are absent from every serialized conversation.
        let mut seeded = document.clone();
        seeded.workspaces[0].conversations =
            default_document(directory.path()).workspaces[0].conversations.clone();
        let canonical = serde_json::to_value(&seeded).unwrap();
        let serialized = &canonical["workspaces"][0]["conversations"][0];
        assert!(serialized.get("presetId").is_none());
        assert!(serialized.get("localPreset").is_none());
    }

    /// The two shipped presets survive the real save boundary with their role
    /// bindings intact — including the Codex ones, whose models cannot exist
    /// until the user signs in.
    #[test]
    fn product_default_document_ships_two_presets_whose_bindings_survive() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.seed-presets.json");
        let document = crate::catalog::product_default_document(directory.path());

        let presets = &document.presets.conversation_presets;
        assert_eq!(
            presets.iter().map(|preset| preset.id.as_str()).collect::<Vec<_>>(),
            vec![
                crate::catalog::CODEX_PRESET_ID,
                crate::catalog::CLAUDE_CODE_PRESET_ID
            ]
        );
        assert_eq!(
            document.presets.default_conversation_preset_id,
            crate::catalog::CLAUDE_CODE_PRESET_ID
        );

        validate_and_save(&path, &document, &document, &AppState::default()).unwrap();
        let restored = read_document(&path).unwrap();

        let codex_provider = restored
            .assets
            .api_providers
            .iter()
            .find(|provider| provider.family == crate::model::ProviderFamily::OpenaiCodex)
            .expect("内置 Codex 行必须在种子里");
        let claude_provider = restored
            .assets
            .api_providers
            .iter()
            .find(|provider| provider.family == crate::model::ProviderFamily::ClaudeAgent)
            .expect("内置 Claude Agent 行必须在种子里");

        // Signed out, so it has no catalog yet — and that is exactly the case
        // the retained binding has to survive.
        assert!(codex_provider.models.is_empty());
        assert!(!codex_provider.enabled);
        assert!(claude_provider.enabled);
        assert!(
            !claude_provider.models.is_empty(),
            "Claude Agent 的模型是本地内置表，种子阶段就该装好"
        );
        assert!(
            claude_provider.models.iter().all(|model| !model.id.contains("[1m]")),
            "1M 上下文的孪生行不入种子"
        );
        assert_eq!(
            claude_provider.active_model_id.as_deref(),
            claude_provider.models.first().map(|model| model.id.as_str())
        );

        // Everything on except the names the host derives for itself.
        for preset in &restored.presets.conversation_presets {
            assert!(!preset.settings.allow_roleless_subagents);
            assert_eq!(preset.settings.agent_definitions.len(), 3);
            let enabled = &preset.settings.enabled_tools;
            assert!(enabled.iter().all(|name| {
                !crate::mework_memory::is_memory_tool(name)
                    && !crate::agents::is_task_runtime_tool_name(name)
                    && !crate::plan_mode::is_plan_mode_tool_name(name)
                    && name != crate::capabilities::SKILL_TOOL
            }));
            let host_derived = restored
                .tools
                .iter()
                .filter(|tool| {
                    crate::mework_memory::is_memory_tool(&tool.name)
                        || crate::agents::is_task_runtime_tool_name(&tool.name)
                        || crate::plan_mode::is_plan_mode_tool_name(&tool.name)
                        || tool.name == crate::capabilities::SKILL_TOOL
                })
                .count();
            assert_eq!(enabled.len(), restored.tools.len() - host_derived);
        }

        let binding = |preset_id: &str, role: &str| {            restored
                .presets
                .conversation_presets
                .iter()
                .find(|preset| preset.id == preset_id)
                .and_then(|preset| {
                    preset
                        .settings
                        .agent_definitions
                        .iter()
                        .find(|definition| definition.name == role)
                })
                .map(|definition| definition.model_selection.clone())
                .expect("角色必须在")
        };
        assert_eq!(
            binding(crate::catalog::CODEX_PRESET_ID, "sol"),
            AgentModelSelection::Explicit {
                provider_id: codex_provider.id.clone(),
                model_id: "gpt-5.6-sol".into(),
            },
            "未登录的 Codex 绑定必须原样活过一次存取"
        );
        assert_eq!(
            binding(crate::catalog::CLAUDE_CODE_PRESET_ID, "opus"),
            AgentModelSelection::Explicit {
                provider_id: claude_provider.id.clone(),
                model_id: "claude-opus-5".into(),
            }
        );
    }

    #[test]
    fn validate_shape_accepts_reachable_nested_branch_tree() {
        let directory = tempfile::tempdir().unwrap();
        let mut document = default_document(directory.path());
        let conversation = &mut document.workspaces[0].conversations[0];
        let root_fork_context_id = conversation
            .contexts
            .iter()
            .find_map(|context| match context {
                ContextItem::User { id, .. } => Some(id.clone()),
                _ => None,
            })
            .unwrap();
        conversation.branches = vec![
            ConversationBranch {
                id: "root-hidden".into(),
                fork_context_id: root_fork_context_id.clone(),
                active: false,
                contexts: vec![user_context("nested-fork")],
                created_at: "2026-07-20T00:00:00Z".into(),
                updated_at: "2026-07-20T00:00:01Z".into(),
            },
            ConversationBranch {
                id: "root-active".into(),
                fork_context_id: root_fork_context_id,
                active: true,
                contexts: Vec::new(),
                created_at: "2026-07-20T00:00:02Z".into(),
                updated_at: "2026-07-20T00:00:02Z".into(),
            },
            ConversationBranch {
                id: "nested-hidden".into(),
                fork_context_id: "nested-fork".into(),
                active: false,
                contexts: vec![user_context("nested-suffix")],
                created_at: "2026-07-20T00:00:03Z".into(),
                updated_at: "2026-07-20T00:00:03Z".into(),
            },
            ConversationBranch {
                id: "nested-active".into(),
                fork_context_id: "nested-fork".into(),
                active: true,
                contexts: Vec::new(),
                created_at: "2026-07-20T00:00:04Z".into(),
                updated_at: "2026-07-20T00:00:04Z".into(),
            },
        ];

        assert!(validate_shape(&document).is_ok());
    }

    #[test]
    fn validate_shape_rejects_unreachable_branch_cycles() {
        let directory = tempfile::tempdir().unwrap();
        let mut self_cycle = default_document(directory.path());
        self_cycle.workspaces[0].conversations[0].branches = vec![
            ConversationBranch {
                id: "self-hidden".into(),
                fork_context_id: "self-fork".into(),
                active: false,
                contexts: vec![user_context("self-fork")],
                created_at: "2026-07-20T00:00:00Z".into(),
                updated_at: "2026-07-20T00:00:01Z".into(),
            },
            ConversationBranch {
                id: "self-active".into(),
                fork_context_id: "self-fork".into(),
                active: true,
                contexts: Vec::new(),
                created_at: "2026-07-20T00:00:02Z".into(),
                updated_at: "2026-07-20T00:00:02Z".into(),
            },
        ];
        assert!(validate_shape(&self_cycle)
            .unwrap_err()
            .contains("不在从活动时间线可达的分支树中"));

        let mut two_group_cycle = default_document(directory.path());
        two_group_cycle.workspaces[0].conversations[0].branches = vec![
            ConversationBranch {
                id: "cycle-a-hidden".into(),
                fork_context_id: "cycle-a".into(),
                active: false,
                contexts: vec![user_context("cycle-b")],
                created_at: "2026-07-20T00:00:00Z".into(),
                updated_at: "2026-07-20T00:00:01Z".into(),
            },
            ConversationBranch {
                id: "cycle-a-active".into(),
                fork_context_id: "cycle-a".into(),
                active: true,
                contexts: Vec::new(),
                created_at: "2026-07-20T00:00:02Z".into(),
                updated_at: "2026-07-20T00:00:02Z".into(),
            },
            ConversationBranch {
                id: "cycle-b-hidden".into(),
                fork_context_id: "cycle-b".into(),
                active: false,
                contexts: vec![user_context("cycle-a")],
                created_at: "2026-07-20T00:00:03Z".into(),
                updated_at: "2026-07-20T00:00:03Z".into(),
            },
            ConversationBranch {
                id: "cycle-b-active".into(),
                fork_context_id: "cycle-b".into(),
                active: true,
                contexts: Vec::new(),
                created_at: "2026-07-20T00:00:04Z".into(),
                updated_at: "2026-07-20T00:00:04Z".into(),
            },
        ];
        assert!(validate_shape(&two_group_cycle)
            .unwrap_err()
            .contains("不在从活动时间线可达的分支树中"));
    }

    #[test]
    fn branch_suffix_roundtrip_keeps_protected_contexts_in_the_same_trusted_scope() {
        let directory = tempfile::tempdir().unwrap();
        let previous = default_document(directory.path());
        let mut branched = previous.clone();
        let conversation = &mut branched.workspaces[0].conversations[0];
        let fork_index = conversation
            .contexts
            .iter()
            .position(|context| matches!(context, ContextItem::User { .. }))
            .unwrap();
        let fork_context_id = conversation.contexts[fork_index].id().to_owned();
        let old_suffix = conversation.contexts.split_off(fork_index + 1);
        conversation.branches = vec![
            ConversationBranch {
                id: "trusted-old".into(),
                fork_context_id: fork_context_id.clone(),
                active: false,
                contexts: old_suffix.clone(),
                created_at: "2026-07-20T00:00:00Z".into(),
                updated_at: "2026-07-20T00:00:01Z".into(),
            },
            ConversationBranch {
                id: "trusted-new".into(),
                fork_context_id,
                active: true,
                contexts: Vec::new(),
                created_at: "2026-07-20T00:00:02Z".into(),
                updated_at: "2026-07-20T00:00:02Z".into(),
            },
        ];
        let state = AppState::default();
        assert!(validate_save_transition(&previous, &branched, &state).is_ok());

        let mut restored = branched.clone();
        let conversation = &mut restored.workspaces[0].conversations[0];
        conversation.contexts.extend(old_suffix);
        conversation.branches[0].active = true;
        conversation.branches[0].contexts.clear();
        conversation.branches[1].active = false;
        assert!(validate_save_transition(&branched, &restored, &state).is_ok());
    }

    #[test]
    fn validate_shape_accepts_sibling_and_cumulative_fork_snapshots() {
        let directory = tempfile::tempdir().unwrap();
        let mut document = default_document(directory.path());
        let inherited = document.workspaces[0].conversations[0].contexts[1].clone();
        let conversation = &mut document.workspaces[0].conversations[0];
        conversation.contexts.push(subagent_record_tool(
            "fork-a-first",
            "a1",
            vec![inherited.clone()],
        ));
        conversation.contexts.push(subagent_record_tool(
            "fork-b",
            "b1",
            vec![inherited.clone()],
        ));
        conversation.contexts.push(subagent_record_tool(
            "fork-a-later",
            "a1",
            vec![inherited, user_context("a1-new-context")],
        ));

        assert!(validate_shape(&document).is_ok());
    }

    #[test]
    fn validate_shape_walks_deep_fork_scopes_without_recursion() {
        let directory = tempfile::tempdir().unwrap();
        let mut document = default_document(directory.path());
        let mut nested = user_context("deep-fork-leaf");
        for depth in 0..10_000 {
            nested = subagent_record_tool(
                &format!("deep-fork-{depth}"),
                &format!("agent-{depth}"),
                vec![nested],
            );
        }
        document.workspaces[0].conversations[0]
            .contexts
            .push(nested);

        assert!(validate_shape(&document).is_ok());

        // Dismantle the synthetic chain iteratively as well, so this test exercises the
        // validator rather than the recursive drop glue generated for the nested value.
        let mut current = document.workspaces[0].conversations[0]
            .contexts
            .pop()
            .unwrap();
        while let ContextItem::Tool {
            subagent: Some(mut subagent),
            ..
        } = current
        {
            let Some(next) = subagent.contexts.pop() else {
                break;
            };
            current = next;
        }
    }

    #[test]
    fn validate_shape_fork_memory_tool_names_accept_current_and_retired_sets_only() {
        let directory = tempfile::tempdir().unwrap();
        let document_with_fork_tools = |names: &[&str]| {
            let mut document = default_document(directory.path());
            let mut fork = subagent_record_tool(
                "fork-memory",
                "m1",
                vec![user_context("fork-memory-context")],
            );
            let ContextItem::Tool { subagent, .. } = &mut fork else {
                unreachable!("subagent_record_tool 构造的是 Tool 上下文");
            };
            let subagent = subagent.as_mut().unwrap();
            subagent.inherits_model_memory = true;
            subagent.fork_model_binding = Some(ForkModelBinding {
                provider_id: "anthropic".into(),
                model_id: "claude-opus-5".into(),
                memory_language: crate::model::ResolvedLanguage::ZhCn,
                memory_tool_names: names.iter().map(|name| (*name).into()).collect(),
                system_prompt_snapshot: "快照提示词".into(),
                system_prompt_receipt: "a".repeat(64),
                memory_snapshot_receipt: None,
                binding_receipt: "b".repeat(64),
                receipt_version: 1,
            });
            document.workspaces[0].conversations[0].contexts.push(fork);
            document
        };

        // Current memory-tool names are persisted in BTreeSet order.
        let mut current = crate::mework_memory::MEMORY_TOOL_NAMES;
        current.sort_unstable();
        assert!(validate_shape(&document_with_fork_tools(&current)).is_ok());

        // Archived documents can retain the complete retired memory-tool set.
        let retired = [
            "memory_delete",
            "memory_list",
            "memory_read",
            "memory_search",
            "memory_upsert",
        ];
        assert!(validate_shape(&document_with_fork_tools(&retired)).is_ok());

        // Mixed and unknown tool-name sets have no valid source.
        let mixed = ["memory_read", "read_global_memory"];
        assert!(validate_shape(&document_with_fork_tools(&mixed)).is_err());
        assert!(validate_shape(&document_with_fork_tools(&["memory_write"])).is_err());
        let unsorted = ["read_global_memory", "create_global_memory"];
        assert!(validate_shape(&document_with_fork_tools(&unsorted)).is_err());
    }

    /// Empty host-minted receipts are valid optional receipts.
    #[test]
    fn validate_shape_accepts_the_empty_receipts_the_host_actually_mints() {
        let directory = tempfile::tempdir().unwrap();

        let document_with_named_agent = |receipt: &str| {
            let mut document = default_document(directory.path());
            let mut spawned = subagent_record_tool("named-agent", "mew", Vec::new());
            let ContextItem::Tool { subagent, .. } = &mut spawned else {
                unreachable!("subagent_record_tool 构造的是 Tool 上下文");
            };
            subagent.as_mut().unwrap().agent_definition = Some(AgentDefinitionBinding {
                source: AgentDefinitionSource::User,
                source_key: String::new(),
                name: "mew".into(),
                revision: 1,
                memory_epoch: 1,
                provider_id: "deepseek".into(),
                model_id: "deepseek-v4-flash".into(),
                memory: AgentDefinitionMemory::None,
                scope_key: String::new(),
                configuration_receipt: receipt.into(),
                receipt_version: 2,
            });
            document.workspaces[0].conversations[0].contexts.push(spawned);
            document
        };
        assert!(validate_shape(&document_with_named_agent("")).is_ok());
        assert!(validate_shape(&document_with_named_agent(&"a".repeat(64))).is_ok());
        assert!(validate_shape(&document_with_named_agent("not-a-digest"))
            .unwrap_err()
            .contains("命名 Agent 配置回执格式无效"));

        let document_with_fork = |system_prompt_receipt: &str, binding_receipt: &str| {
            let mut document = default_document(directory.path());
            let mut fork = subagent_record_tool("forked-agent", "forked", Vec::new());
            let ContextItem::Tool { subagent, .. } = &mut fork else {
                unreachable!("subagent_record_tool 构造的是 Tool 上下文");
            };
            let subagent = subagent.as_mut().unwrap();
            let mut memory_tool_names = crate::mework_memory::MEMORY_TOOL_NAMES
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>();
            memory_tool_names.sort();
            subagent.inherits_model_memory = true;
            subagent.fork_model_binding = Some(ForkModelBinding {
                provider_id: "deepseek".into(),
                model_id: "deepseek-v4-flash".into(),
                memory_language: crate::model::ResolvedLanguage::ZhCn,
                memory_tool_names,
                system_prompt_snapshot: "快照提示词".into(),
                system_prompt_receipt: system_prompt_receipt.into(),
                memory_snapshot_receipt: None,
                binding_receipt: binding_receipt.into(),
                receipt_version: 1,
            });
            document.workspaces[0].conversations[0].contexts.push(fork);
            document
        };
        assert!(validate_shape(&document_with_fork("", "")).is_ok());
        assert!(validate_shape(&document_with_fork(&"a".repeat(64), &"b".repeat(64))).is_ok());
        assert!(validate_shape(&document_with_fork("not-a-digest", ""))
            .unwrap_err()
            .contains("绑定回执无效"));
    }

    #[test]
    fn validate_shape_enforces_conversation_preset_system_prompt_utf8_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let mut exact_limit = default_document(directory.path());
        default_conversation_preset_mut(&mut exact_limit).system_prompt = "x".repeat(1024 * 1024);
        assert!(validate_shape(&exact_limit).is_ok());

        let mut oversized_ascii = default_document(directory.path());
        default_conversation_preset_mut(&mut oversized_ascii).system_prompt =
            "x".repeat(1024 * 1024 + 1);
        assert!(validate_shape(&oversized_ascii)
            .unwrap_err()
            .contains("1 MiB"));

        let mut oversized_utf8 = default_document(directory.path());
        default_conversation_preset_mut(&mut oversized_utf8).system_prompt =
            "界".repeat(1024 * 1024 / 3 + 1);
        assert!(validate_shape(&oversized_utf8)
            .unwrap_err()
            .contains("1 MiB"));
    }

    #[test]
    fn tool_round_is_optional_for_legacy_data_and_round_trips_when_present() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state").join("document.json");
        let legacy_value = serde_json::to_value(default_document(directory.path())).unwrap();
        assert!(
            legacy_value["workspaces"][0]["conversations"][0]["contexts"][3]
                .get("round")
                .is_none()
        );

        let mut document: AppDocument = serde_json::from_value(legacy_value).unwrap();
        let ContextItem::Tool { round, .. } =
            &mut document.workspaces[0].conversations[0].contexts[3]
        else {
            panic!("seed context must be a tool call");
        };
        assert_eq!(*round, None);
        *round = Some(2);

        save_all(&path, &document).unwrap();
        let restored = read_document(&path).unwrap();
        let ContextItem::Tool { round, .. } = &restored.workspaces[0].conversations[0].contexts[3]
        else {
            panic!("seed context must be a tool call");
        };
        assert_eq!(*round, Some(2));
        assert_eq!(restored.schema_version, SCHEMA_VERSION);
    }

    #[test]
    fn missing_document_is_initialized_once() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state").join("document.json");
        assert!(!path.exists());

        let initialized = load_or_initialize(&path, directory.path()).unwrap();

        assert!(path.exists());
        assert_eq!(read_document(&path).unwrap(), initialized);
    }

    #[test]
    fn malformed_existing_document_is_preserved_and_reported() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.json");
        let original = b"{ definitely-not-json";
        fs::write(&path, original).unwrap();

        let error = load_or_initialize(&path, directory.path()).unwrap_err();

        assert!(error.contains("文档加载失败"));
        assert!(error.contains("原文件未修改"));
        assert_eq!(fs::read(&path).unwrap(), original);
        let backups = fs::read_dir(directory.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("document.corrupt-")
            })
            .collect::<Vec<_>>();
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read(backups[0].path()).unwrap(), original);
    }

    #[test]
    fn future_schema_document_is_preserved_and_reported() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.json");
        let mut value = serde_json::to_value(default_document(directory.path())).unwrap();
        value["schemaVersion"] = serde_json::json!(SCHEMA_VERSION + 1);
        let original = serde_json::to_vec_pretty(&value).unwrap();
        fs::write(&path, &original).unwrap();

        let error = load_or_initialize(&path, directory.path()).unwrap_err();

        assert!(error.contains("更新版本"));
        assert!(error.contains("原文件未修改"));
        assert_eq!(fs::read(&path).unwrap(), original);
    }

    #[test]
    fn outdated_schema_document_is_preserved_and_reported() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.json");
        let mut value = serde_json::to_value(default_document(directory.path())).unwrap();
        // Test an explicitly old schema version. Zero is also what a document
        // with no `schemaVersion` at all reads as, so this covers both.
        value["schemaVersion"] = serde_json::json!(0);
        let original = serde_json::to_vec_pretty(&value).unwrap();
        fs::write(&path, &original).unwrap();

        let error = load_or_initialize(&path, directory.path()).unwrap_err();

        assert!(error.contains("旧版 schema"));
        assert!(error.contains("原文件未修改"));
        assert_eq!(fs::read(&path).unwrap(), original);
    }

    /// The accepted older anchor shape preserves configuration when the
    /// host-owned fork-intent table is added.
    #[test]
    fn released_schema_one_preserves_configuration_for_fork_recovery() {
        assert_eq!(SCHEMA_VERSION, 2, "抬版本时重新判断要不要就地迁移");
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.json");
        let mut document = default_document(directory.path());
        document.assets.web_search.max_results = 42;
        let mut value = serde_json::to_value(&document).unwrap();
        value["schemaVersion"] = serde_json::json!(1);
        let original = serde_json::to_vec_pretty(&value).unwrap();
        fs::write(&path, &original).unwrap();

        let restored = load_or_initialize(&path, directory.path()).unwrap();
        assert_eq!(restored.schema_version, 2);
        assert_eq!(restored.assets.web_search.max_results, 42);
        assert_eq!(fs::read(&path).unwrap(), original);
    }

    /// A rejected conversation restores its full prior workspace membership.
    #[test]
    fn rejected_moved_conversation_returns_to_its_original_workspace() {
        let directory = tempfile::tempdir().unwrap();
        let previous = default_document(directory.path());
        let mut changed = previous.clone();
        let source_index = changed
            .workspaces
            .iter()
            .position(|workspace| workspace.id == "ws_default")
            .unwrap();
        let target_index = changed
            .workspaces
            .iter()
            .position(|workspace| workspace.id == TEMPORARY_WORKSPACE_ID)
            .unwrap();
        let mut moved = changed.workspaces[source_index].conversations.remove(0);
        moved.updated_at = "2099-08-24T00:00:00.000Z".into();
        moved.contexts.push(ContextItem::User {
            id: moved.contexts[0].id().to_owned(),
            content: "duplicate".into(),
            images: Vec::new(),
            created_at: "2099-08-24T00:00:00.000Z".into(),
        });
        changed.workspaces[target_index].conversations.push(moved);

        let prepared =
            prepare_save_transition(&previous, &changed, &AppState::default()).unwrap();

        assert!(prepared.document.workspaces[source_index]
            .conversations
            .iter()
            .any(|conversation| conversation.id == "conv_welcome"));
        assert!(!prepared.document.workspaces[target_index]
            .conversations
            .iter()
            .any(|conversation| conversation.id == "conv_welcome"));
    }

    /// A corrupt conversation row is isolated while the anchor and other conversations load.
    #[test]
    fn a_corrupt_conversation_row_is_skipped_and_the_rest_loads() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.v1.json");
        let mut document = default_document(directory.path());
        let welcome_id = document.workspaces[0].conversations[0].id.clone();
        let mut sibling = document.workspaces[0].conversations[0].clone();
        sibling.id = "conv_sibling".into();
        sibling.contexts.clear();
        document.workspaces[0].conversations.push(sibling);
        save_all(&path, &document).unwrap();

        let store = crate::conversation_store::store_for(&path).unwrap();
        store.corrupt_context_for_test(&welcome_id).unwrap();

        let loaded = read_document(&path).unwrap();
        let ids = loaded.workspaces[0]
            .conversations
            .iter()
            .map(|conversation| conversation.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["conv_sibling"], "坏行只带走自己");
    }

    /// Deleted conversations must not be re-adopted after reload.
    #[test]
    fn a_deleted_conversation_does_not_come_back_after_reload() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.v1.json");
        let document = default_document(directory.path());
        let welcome_id = document.workspaces[0].conversations[0].id.clone();
        save_all(&path, &document).unwrap();

        let store = crate::conversation_store::store_for(&path).unwrap();
        store.delete_conversation(&welcome_id).unwrap();

        let reloaded = read_document(&path).unwrap();
        assert!(reloaded
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.conversations.iter())
            .all(|conversation| conversation.id != welcome_id));
    }

    /// Conversations whose workspace is missing move to the temporary workspace.
    #[test]
    fn a_conversation_bound_to_a_missing_workspace_is_adopted() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.v1.json");
        let document = default_document(directory.path());
        save_all(&path, &document).unwrap();

        let store = crate::conversation_store::store_for(&path).unwrap();
        let mut orphan = document.workspaces[0].conversations[0].clone();
        orphan.id = "conv_orphan".into();
        store.put_conversation("workspace-that-no-longer-exists", &orphan).unwrap();

        let loaded = read_document(&path).unwrap();
        let temporary = loaded
            .workspaces
            .iter()
            .find(|workspace| workspace.kind == WorkspaceKind::Temporary)
            .expect("temporary workspace");
        assert!(temporary
            .conversations
            .iter()
            .any(|conversation| conversation.id == "conv_orphan"));
    }

    /// Startup recovery banks a broken anchor, adopts conversation bodies, and records a notice.
    #[test]
    fn load_or_recover_banks_a_broken_anchor_and_keeps_conversation_bodies() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.v1.json");
        let document = default_document(directory.path());
        let welcome_id = document.workspaces[0].conversations[0].id.clone();
        save_all(&path, &document).unwrap();
        fs::write(&path, b"{ definitely broken").unwrap();

        let recovered = load_or_recover(&path, directory.path()).unwrap();

        assert_eq!(recovered.schema_version, SCHEMA_VERSION);
        // The original anchor is banked.
        assert!(fs::read_dir(directory.path())
            .unwrap()
            .flatten()
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with("document.v1.rejected-")));
        // The stored conversation is adopted without assuming its original workspace.
        assert!(recovered
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.conversations.iter())
            .any(|conversation| conversation.id == welcome_id));
        // The recovery notice is stored on the first conversation.
        let has_notice = recovered.workspaces[0]
            .conversations
            .first()
            .into_iter()
            .flat_map(|conversation| conversation.contexts.iter())
            .any(|context| matches!(
                context,
                ContextItem::System { content, .. } if content.contains("已封存")
            ));
        assert!(has_notice);
    }

    #[test]
    fn current_schema_empty_provider_catalog_is_not_reseeded() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.json");
        let mut document = default_document(directory.path());
        document.assets.api_providers.clear();
        document.global_settings.active_provider_id = None;
        save_all(&path, &document).unwrap();

        let restored = read_document(&path).unwrap();

        assert!(restored.assets.api_providers.is_empty());
        assert!(restored.global_settings.active_provider_id.is_none());
    }

    #[test]
    fn workspace_kinds_require_valid_directory_and_temporary_shapes() {
        let directory = tempfile::tempdir().unwrap();
        let document = default_document(directory.path());
        assert!(validate_shape(&document).is_ok());
        assert!(!document
            .workspaces
            .iter()
            .any(|workspace| workspace.kind == WorkspaceKind::Unsupported));

        let mut invalid_directory = document.clone();
        invalid_directory
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.kind == WorkspaceKind::Directory)
            .unwrap()
            .path
            .clear();
        assert!(validate_shape(&invalid_directory)
            .unwrap_err()
            .contains("路径为空"));

        let mut unsupported = document.clone();
        unsupported.workspaces.push(Workspace {
            id: "unsupported-workspace".into(),
            name: "Unsupported".into(),
            kind: WorkspaceKind::Unsupported,
            path: String::new(),
            created_at: "2026-01-01T00:00:00Z".into(),
            default_conversation_preset_id: String::new(),
            last_conversation_settings: None,
            conversations: Vec::new(),
        });
        assert!(validate_shape(&unsupported)
            .unwrap_err()
            .contains("不支持的类型"));

        let mut missing_temporary = document;
        missing_temporary
            .workspaces
            .retain(|workspace| workspace.kind != WorkspaceKind::Temporary);
        assert!(validate_shape(&missing_temporary)
            .unwrap_err()
            .contains("只能包含一个临时工作区"));
    }

    #[test]
    fn unknown_current_workspace_kind_is_normalized_in_memory() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.json");
        let document = default_document(directory.path());
        save_all(&path, &document).unwrap();

        // Unknown workspace kinds normalize to the temporary workspace with their conversations.
        let store = crate::conversation_store::store_for(&path).unwrap();
        store
            .put_conversation("future-workspace", &document.workspaces[0].conversations[0])
            .unwrap();
        let mut anchor: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        anchor["workspaces"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "id": "future-workspace",
                "name": "Future",
                "kind": "future_kind",
                "path": "",
                "createdAt": "2026-01-01T00:00:00Z"
            }));
        fs::write(&path, serde_json::to_vec_pretty(&anchor).unwrap()).unwrap();

        let normalized = read_document(&path).unwrap();
        assert!(!normalized
            .workspaces
            .iter()
            .any(|workspace| workspace.kind == WorkspaceKind::Unsupported));
        assert!(normalized
            .workspaces
            .iter()
            .find(|workspace| workspace.kind == WorkspaceKind::Temporary)
            .unwrap()
            .conversations
            .iter()
            .any(|conversation| conversation.id == "conv_welcome"));
    }

    #[test]
    fn incomplete_provider_connection_fields_do_not_block_document_saves() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.json");
        let previous = default_document(directory.path());
        let mut changed = previous.clone();
        changed.assets.api_providers[0].name = "   ".into();
        changed.assets.api_providers[0].base_url = "not a URL yet".into();

        assert!(validate_shape(&changed).is_ok());
        validate_and_save(&path, &previous, &changed, &AppState::default()).unwrap();

        let restored = read_document(&path).unwrap();
        assert_eq!(restored.assets.api_providers[0].name, "   ");
        assert_eq!(
            restored.assets.api_providers[0].base_url,
            "not a URL yet"
        );
    }

    /// Endpoint overrides obey the same URL safety rules as the chat base URL.
    #[test]
    fn endpoint_base_url_overrides_go_through_the_same_url_rules() {
        let mut document = default_document(Path::new("."));
        document.assets.api_providers[0].endpoint_base_urls = std::collections::BTreeMap::from([(
            EndpointType::OpenaiImageGeneration,
            "https://images.example.com/v1".to_owned(),
        )]);
        validate_shape(&document).expect("合法的 HTTPS 覆盖地址");

        // An empty override falls back to the chat base URL and needs no URL validation.
        document.assets.api_providers[0]
            .endpoint_base_urls
            .insert(EndpointType::OpenaiTextToSpeech, "   ".to_owned());
        validate_shape(&document).expect("空覆盖等同于未设置");

        document.assets.api_providers[0]
            .endpoint_base_urls
            .insert(EndpointType::OpenaiTextToSpeech, "http://images.example.com".to_owned());
        let error = validate_shape(&document).unwrap_err();
        assert!(
            error.contains("openai_text_to_speech") && error.contains("端点地址无效"),
            "报错要点名是哪个端点，否则四个输入框里看不出改哪个：{error}"
        );
    }

    #[test]
    fn provider_and_model_ids_are_trimmed_for_uniqueness() {
        let mut duplicate_provider = default_document(Path::new("."));
        let existing_provider_id = duplicate_provider.assets.api_providers[0]
            .id
            .clone();
        duplicate_provider.assets.api_providers[1].id =
            format!("  {existing_provider_id}  ");
        let provider_error = validate_shape(&duplicate_provider).unwrap_err();
        assert!(provider_error.contains("API 提供商 ID 重复"));

        let mut duplicate_model = default_document(Path::new("."));
        duplicate_model.assets.api_providers[0].models =
            vec![test_model("same-model"), test_model("  same-model  ")];
        let model_error = validate_shape(&duplicate_model).unwrap_err();
        assert!(model_error.contains("模型 ID 重复"));

        let mut blank_provider = default_document(Path::new("."));
        blank_provider.assets.api_providers[0].id = " \t ".into();
        assert!(validate_shape(&blank_provider)
            .unwrap_err()
            .contains("API 提供商 ID 不能为空"));

        let mut blank_model = default_document(Path::new("."));
        blank_model.assets.api_providers[0].models = vec![test_model(" \t ")];
        assert!(validate_shape(&blank_model)
            .unwrap_err()
            .contains("模型 ID 不能为空"));
    }

    #[test]
    fn chat_model_ids_share_the_exact_memory_owner_byte_contract() {
        let mut document = default_document(Path::new("."));
        document.assets.api_providers[0].active_model_id = None;
        document.assets.api_providers[0].models = vec![
            test_model(&"a".repeat(512)),
            test_model(&"é".repeat(256)),
            test_model("vendor/kimi-k3:Reasoning"),
            test_model("vendor/kimi-k3:reasoning"),
        ];
        assert!(validate_shape(&document).is_ok());

        document.assets.api_providers[0].models = vec![test_model(&"a".repeat(513))];
        assert!(validate_shape(&document)
            .unwrap_err()
            .contains("512 个 UTF-8 字节"));

        document.assets.api_providers[0].models = vec![test_model(&"é".repeat(257))];
        assert!(validate_shape(&document)
            .unwrap_err()
            .contains("512 个 UTF-8 字节"));

        document.assets.api_providers[0].models = vec![test_model("kimi\u{0007}-k3")];
        assert!(validate_shape(&document)
            .unwrap_err()
            .contains("控制字符"));
    }

    #[test]
    fn named_agent_definitions_are_bounded_and_reject_unknown_source_or_memory() {
        let mut oversized = default_document(Path::new("."));
        set_document_agent_definitions(
            &mut oversized,
            (0..=MAX_AGENT_DEFINITIONS)
                .map(|index| {
                    let mut definition = test_agent_definition("reviewer");
                    definition.source = AgentDefinitionSource::Plugin;
                    definition.source_key = format!("user-{index}");
                    definition
                })
                .collect(),
        );
        assert!(validate_shape(&oversized)
            .unwrap_err()
            .contains("超过 256 项"));

        let definition = serde_json::to_value(test_agent_definition("reviewer")).unwrap();
        let mut invalid_source = definition.clone();
        invalid_source["source"] = serde_json::json!("remote");
        assert!(serde_json::from_value::<AgentDefinition>(invalid_source).is_err());
        let mut invalid_memory = definition;
        invalid_memory["memory"] = serde_json::json!("global");
        assert!(serde_json::from_value::<AgentDefinition>(invalid_memory).is_err());
    }

    #[test]
    fn named_agent_definition_revisions_are_monotonic_across_save_transitions() {
        let mut previous = default_document(Path::new("."));
        let mut definition = test_agent_definition("reviewer");
        definition.revision = 5;
        set_document_agent_definitions(&mut previous, vec![definition]);

        assert!(validate_agent_definition_transition(&previous, &previous).is_ok());

        let mut rollback = previous.clone();
        edit_document_agent_definition(&mut rollback, 0, |definition| {
            definition.revision = 4;
        });
        assert!(validate_agent_definition_transition(&previous, &rollback)
            .unwrap_err()
            .contains("不能从 5 回退到 4"));

        let mut stale_change = previous.clone();
        edit_document_agent_definition(&mut stale_change, 0, |definition| {
            definition.memory = AgentDefinitionMemory::Project;
        });
        assert!(
            validate_agent_definition_transition(&previous, &stale_change)
                .unwrap_err()
                .contains("必须提高 revision")
        );

        let mut bumped_change = stale_change;
        edit_document_agent_definition(&mut bumped_change, 0, |definition| {
            definition.revision = 6;
        });
        assert!(validate_agent_definition_transition(&previous, &bumped_change).is_ok());

        let mut stale_memory_change = previous.clone();
        edit_document_agent_definition(&mut stale_memory_change, 0, |definition| {
            definition.memory = AgentDefinitionMemory::User;
        });
        assert!(validate_agent_definition_transition(&previous, &stale_memory_change).is_err());

        let mut renamed = previous.clone();
        edit_document_agent_definition(&mut renamed, 0, |definition| {
            definition.name = "security-reviewer".into();
            definition.revision = 1;
        });
        assert!(
            validate_agent_definition_transition(&previous, &renamed).is_ok(),
            "renames are delete plus create and never imply a partition move"
        );

        let error = validate_save_transition(&previous, &bumped_change, &AppState::default());
        assert!(error.is_ok(), "{error:?}");
    }

    #[test]
    fn renderer_user_definition_delete_and_recreate_uses_a_durable_new_memory_epoch() {
        let state = AppState::default();
        let mut previous = default_document(Path::new("."));
        let mut definition = test_agent_definition("reviewer");
        definition.revision = 9;
        definition.memory_epoch = 4;
        definition.memory = AgentDefinitionMemory::User;
        set_document_agent_definitions(&mut previous, vec![definition.clone()]);

        let mut deletion_draft = previous.clone();
        set_document_agent_definitions(&mut deletion_draft, Vec::new());
        let deleted =
            validate_save_transition(&previous, &deletion_draft, &state).expect("delete draft");
        let tombstone = &deleted.workspaces[0].conversations[0].settings.agent_definitions[0];
        assert!(tombstone.deleted);
        assert!(!tombstone.enabled);
        assert_eq!(tombstone.revision, 10);
        assert_eq!(tombstone.memory_epoch, 4);
        assert_eq!(tombstone.memory, AgentDefinitionMemory::None);
        assert_eq!(tombstone.model_selection, AgentModelSelection::Inherit);

        let mut recreate_draft = deleted.clone();
        let mut provisional = test_agent_definition("reviewer");
        provisional.revision = 1;
        provisional.memory_epoch = 1;
        provisional.memory = AgentDefinitionMemory::User;
        set_document_agent_definitions(&mut recreate_draft, vec![provisional]);
        let recreated_document =
            validate_save_transition(&deleted, &recreate_draft, &state).expect("recreate draft");
        let recreated = &recreated_document.workspaces[0].conversations[0].settings.agent_definitions[0];
        assert!(!recreated.deleted);
        assert_eq!(recreated.revision, 11);
        assert_eq!(recreated.memory_epoch, 5);

        let mut forged_draft = recreated_document.clone();
        forged_draft.workspaces[0].conversations[0].settings.agent_definitions[0].revision = 999;
        forged_draft.workspaces[0].conversations[0].settings.agent_definitions[0].memory_epoch = 999;
        let canonical = validate_save_transition(&recreated_document, &forged_draft, &state)
            .expect("host overwrites renderer revision and epoch");
        assert_eq!(canonical.workspaces[0].conversations[0].settings.agent_definitions[0].revision, 11);
        assert_eq!(
            canonical.workspaces[0].conversations[0].settings.agent_definitions[0].memory_epoch,
            5
        );
    }

    /// Rewording a description preserves its revision because it does not change capabilities.
    #[test]
    fn rewording_a_role_persists_the_text_without_moving_its_revision() {
        let state = AppState::default();
        let mut previous = default_document(Path::new("."));
        let mut definition = test_agent_definition("reviewer");
        definition.revision = 9;
        definition.description = "旧说明".into();
        set_document_agent_definitions(&mut previous, vec![definition.clone()]);

        let mut reworded_draft = previous.clone();
        reworded_draft.workspaces[0].conversations[0]
            .settings
            .agent_definitions[0]
            .description = "  新说明。\n第二行。  ".into();
        let saved =
            validate_save_transition(&previous, &reworded_draft, &state).expect("reword draft");
        let role = &saved.workspaces[0].conversations[0].settings.agent_definitions[0];
        assert_eq!(role.description, "  新说明。\n第二行。  ");
        assert_eq!(role.revision, 9, "改说明不该动版本号");

        // Changing the model is a configuration change and must advance the revision.
        let mut rebound_draft = saved.clone();
        rebound_draft.workspaces[0].conversations[0]
            .settings
            .agent_definitions[0]
            .memory = AgentDefinitionMemory::User;
        let rebound =
            validate_save_transition(&saved, &rebound_draft, &state).expect("rebind draft");
        assert_eq!(
            rebound.workspaces[0].conversations[0].settings.agent_definitions[0].revision,
            10
        );
    }

    /// Tombstones retain identity only and discard deleted-role descriptions.
    #[test]
    fn deleting_a_role_leaves_no_prose_behind_in_its_tombstone() {
        let state = AppState::default();
        let mut previous = default_document(Path::new("."));
        let mut definition = test_agent_definition("reviewer");
        definition.description = "会被一起带走的说明".into();
        set_document_agent_definitions(&mut previous, vec![definition]);

        let mut deletion_draft = previous.clone();
        set_document_agent_definitions(&mut deletion_draft, Vec::new());
        let deleted =
            validate_save_transition(&previous, &deletion_draft, &state).expect("delete draft");
        let tombstone = &deleted.workspaces[0].conversations[0].settings.agent_definitions[0];
        assert!(tombstone.deleted);
        assert_eq!(tombstone.description, "");
    }

    /// Descriptions are unbounded except that NUL is prohibited because downstream consumers treat it as binary.
    #[test]
    fn a_role_description_is_unbounded_in_length_but_never_carries_a_nul() {
        let mut long = default_document(Path::new("."));
        let mut definition = test_agent_definition("reviewer");
        definition.description = "很长的一段说明。\n".repeat(20_000);
        set_document_agent_definitions(&mut long, vec![definition.clone()]);
        assert!(validate_shape(&long).is_ok(), "说明不设长度上限");

        let mut with_nul = default_document(Path::new("."));
        definition.description = "说明\u{0}夹了一个 NUL".into();
        set_document_agent_definitions(&mut with_nul, vec![definition]);
        assert!(validate_shape(&with_nul)
            .unwrap_err()
            .contains("说明不能包含 NUL 字符"));
    }

    #[test]
    fn renderer_cannot_elevate_or_mutate_host_agent_definition_sources() {
        let state = AppState::default();
        let previous = default_document(Path::new("."));
        let mut injected = previous.clone();
        let mut managed = test_agent_definition("managed-reviewer");
        managed.source = AgentDefinitionSource::Managed;
        set_document_agent_definitions(&mut injected, vec![managed]);
        assert!(validate_save_transition(&previous, &injected, &state)
            .unwrap_err()
            .contains("renderer 不能新增"));

        let mut trusted_previous = previous;
        let mut trusted = test_agent_definition("project-reviewer");
        trusted.source = AgentDefinitionSource::Project;
        trusted.source_key = "ws_default".into();
        set_document_agent_definitions(&mut trusted_previous, vec![trusted.clone()]);
        let mut tampered = trusted_previous.clone();
        tampered.workspaces[0].conversations[0]
            .settings
            .agent_definitions[0]
            .effort = Some(crate::model::ReasoningEffort::High);
        tampered.workspaces[0].conversations[0]
            .settings
            .agent_definitions[0]
            .revision += 1;
        assert!(
            validate_save_transition(&trusted_previous, &tampered, &state)
                .unwrap_err()
                .contains("renderer 不能新增、删除或修改")
        );
    }

    #[test]
    fn active_provider_and_model_references_use_and_persist_canonical_ids() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.json");
        let previous = default_document(directory.path());
        let mut changed = previous.clone();
        {
            let provider = &mut changed.assets.api_providers[0];
            provider.id = "  custom-provider  ".into();
            provider.models = vec![test_model("  custom-model  ")];
            provider.active_model_id = Some(" custom-model ".into());
        }
        changed.global_settings.active_provider_id = Some(" custom-provider ".into());

        assert!(validate_shape(&changed).is_ok());
        validate_and_save(&path, &previous, &changed, &AppState::default()).unwrap();

        let restored = read_document(&path).unwrap();
        let provider = &restored.assets.api_providers[0];
        assert_eq!(provider.id, "custom-provider");
        assert_eq!(provider.active_model_id.as_deref(), Some("custom-model"));
        assert_eq!(provider.models[0].id, "custom-model");
        assert_eq!(
            restored.global_settings.active_provider_id.as_deref(),
            Some("custom-provider")
        );
    }

    #[test]
    fn provider_active_model_must_reference_its_own_catalog() {
        let directory = tempfile::tempdir().unwrap();
        let mut document = default_document(directory.path());
        let provider = &mut document.assets.api_providers[0];
        provider.models = vec![test_model("available")];
        provider.active_model_id = Some("missing".into());
        assert!(validate_shape(&document).is_err());

        document.assets.api_providers[0].active_model_id = Some("available".into());
        assert!(validate_shape(&document).is_ok());
    }

    #[test]
    fn tool_results_require_an_execution_receipt() {
        let directory = tempfile::tempdir().unwrap();
        let previous = default_document(directory.path());
        let mut changed = previous.clone();
        let workspace_path = changed.workspaces[0].path.clone();
        let conversation_id = changed.workspaces[0].conversations[0].id.clone();
        let (request, receipt) = {
            let ContextItem::Tool {
                tool_name,
                input,
                result,
                ..
            } = &mut changed.workspaces[0].conversations[0].contexts[3]
            else {
                panic!("seed context must be a tool call");
            };
            result.output = "verified backend output".into();
            result.duration_ms = 42;
            (
                ToolExecutionRequest {
                    conversation_id,
                    workspace_path,
                    tool_name: tool_name.clone(),
                    input: input.clone(),
                },
                result.clone(),
            )
        };

        let state = AppState::default();
        assert!(validate_tool_results(&previous, &changed, &state)
            .unwrap()
            .refused());

        state.record_receipt(&request, &receipt);
        assert!(!validate_tool_results(&previous, &changed, &state).unwrap().refused());
    }

    /// The failure the token exists to end: a card executed in one process and
    /// saved after a restart. The old receipt book lived only in memory, so
    /// every restart stranded anything not yet persisted — permanently, because
    /// the card kept coming back on every later save. The token travels with
    /// the card, so a fresh process with the same key still verifies it.
    #[test]
    fn a_card_attested_before_a_restart_still_saves_afterwards() {
        let directory = tempfile::tempdir().unwrap();
        let app_data = directory.path();
        let previous = default_document(app_data);
        let mut changed = previous.clone();
        let conversation_id = changed.workspaces[0].conversations[0].id.clone();

        // The process that executed the tool attests its card.
        let executing = AppState::default();
        executing.install_attestation_key(app_data).unwrap();
        {
            let ContextItem::Tool {
                id,
                tool_name,
                requested_input,
                input,
                result,
                subagent,
                attestation,
                ..
            } = &mut changed.workspaces[0].conversations[0].contexts[3]
            else {
                panic!("seed context must be a tool call");
            };
            result.output = "produced by the backend before the restart".into();
            *attestation = executing.attest_tool_context(
                &crate::tool_attestation::AttestationSubject {
                    conversation_id: &conversation_id,
                    context_id: id,
                    tool_name,
                    input,
                    requested_input: requested_input.as_ref(),
                    result,
                    subagent: subagent.as_ref(),
                },
            );
        }

        // A completely fresh process: no receipt book, only the durable key.
        let restarted = AppState::default();
        restarted.install_attestation_key(app_data).unwrap();
        assert!(
            !validate_tool_results(&previous, &changed, &restarted)
                .unwrap()
                .refused(),
            "a token issued before the restart must still be accepted after it"
        );
    }

    /// The token is the credential, so a card that arrives without one — or
    /// with one that does not match its contents — is not trusted just because
    /// it looks well-formed.
    #[test]
    fn a_missing_or_forged_token_is_not_accepted() {
        let directory = tempfile::tempdir().unwrap();
        let app_data = directory.path();
        let previous = default_document(app_data);
        let state = AppState::default();
        state.install_attestation_key(app_data).unwrap();
        let conversation_id = previous.workspaces[0].conversations[0].id.clone();

        let attest = |document: &mut AppDocument| {
            let ContextItem::Tool {
                id,
                tool_name,
                requested_input,
                input,
                result,
                subagent,
                attestation,
                ..
            } = &mut document.workspaces[0].conversations[0].contexts[3]
            else {
                panic!("seed context must be a tool call");
            };
            *attestation =
                state.attest_tool_context(&crate::tool_attestation::AttestationSubject {
                    conversation_id: &conversation_id,
                    context_id: id,
                    tool_name,
                    input,
                    requested_input: requested_input.as_ref(),
                    result,
                    subagent: subagent.as_ref(),
                });
        };

        // Edited after attestation: the token no longer describes the card.
        let mut edited = previous.clone();
        attest(&mut edited);
        {
            let ContextItem::Tool { result, .. } =
                &mut edited.workspaces[0].conversations[0].contexts[3]
            else {
                unreachable!()
            };
            result.output = "the renderer rewrote this after the fact".into();
        }
        assert!(validate_tool_results(&previous, &edited, &state)
            .unwrap()
            .refused());

        // Present, well-formed, and simply invented.
        let mut forged = previous.clone();
        {
            let ContextItem::Tool {
                result,
                attestation,
                ..
            } = &mut forged.workspaces[0].conversations[0].contexts[3]
            else {
                unreachable!()
            };
            result.output = "never executed".into();
            *attestation = "0".repeat(64);
        }
        assert!(validate_tool_results(&previous, &forged, &state)
            .unwrap()
            .refused());

        // Absent entirely: absence of a proof is not proof.
        let mut bare = previous.clone();
        {
            let ContextItem::Tool {
                result,
                attestation,
                ..
            } = &mut bare.workspaces[0].conversations[0].contexts[3]
            else {
                unreachable!()
            };
            result.output = "never executed".into();
            attestation.clear();
        }
        assert!(validate_tool_results(&previous, &bare, &state)
            .unwrap()
            .refused());
    }

    /// Synchronously reject renderer-mutated tool cards at the conversation command boundary.
    #[test]
    fn a_renderer_mutated_tool_card_is_refused_at_the_conversation_command() {
        let document = default_document(Path::new("."));
        let mut proposal = document.workspaces[0].conversations[0].clone();
        let stale_id = {
            let ContextItem::Tool { id, result, .. } = &mut proposal.contexts[3] else {
                panic!("seed context must be a tool call");
            };
            result.output = "no receipt will ever match this".into();
            id.clone()
        };

        let state = AppState::default();
        let error = validate_incoming_conversation(
            &document,
            &document.workspaces[0].id,
            &mut proposal,
            &state,
        )
        .expect_err("a mutated card must not be accepted");

        assert!(error.contains(&stale_id), "错误必须指出是哪张卡：{error}");
    }

    /// Unchanged cards take the equality fast path and remain repeatedly writable.
    #[test]
    fn an_unchanged_conversation_passes_the_command_boundary_repeatedly() {
        let document = default_document(Path::new("."));
        let state = AppState::default();
        for _ in 0..3 {
            let mut proposal = document.workspaces[0].conversations[0].clone();
            validate_incoming_conversation(
                &document,
                &document.workspaces[0].id,
                &mut proposal,
                &state,
            )
            .expect("unchanged conversation stays writable");
        }
    }

    #[test]
    fn post_tool_blocked_image_success_cannot_be_used_for_first_save() {
        let directory = tempfile::tempdir().unwrap();
        let previous = default_document(directory.path());
        let mut stale_success_document = previous.clone();
        let mut stale_context =
            stale_success_document.workspaces[0].conversations[0].contexts[3].clone();
        let (request, stale_success) = {
            let ContextItem::Tool {
                id,
                tool_name,
                input,
                result,
                ..
            } = &mut stale_context
            else {
                panic!("seed context must be a tool call");
            };
            *id = "fresh-post-tool-blocked-image".into();
            result.success = true;
            result.output = "captured before PostToolUse".into();
            result.images = vec![ImageAttachment {
                id: "a".repeat(64),
                name: "blocked.png".into(),
                mime: "image/png".into(),
                width: 32,
                height: 16,
                bytes: 123,
                short_id: None,
            }];
            (
                ToolExecutionRequest {
                    conversation_id: stale_success_document.workspaces[0].conversations[0]
                        .id
                        .clone(),
                    workspace_path: stale_success_document.workspaces[0].path.clone(),
                    tool_name: tool_name.clone(),
                    input: input.clone(),
                },
                result.clone(),
            )
        };
        stale_success_document.workspaces[0].conversations[0]
            .contexts
            .push(stale_context);

        let mut blocked = stale_success.clone();
        blocked.success = false;
        blocked.output = "blocked by PostToolUse".into();
        blocked.images.clear();
        let state = AppState::default();
        {
            let _provisional = state.begin_provisional_receipts();
            state.record_receipt(&request, &stale_success);
        }
        state.record_receipt(&request, &blocked);

        assert!(validate_tool_results(&previous, &stale_success_document, &state).unwrap().refused());

        let mut final_document = previous.clone();
        let mut final_context = final_document.workspaces[0].conversations[0].contexts[3].clone();
        let ContextItem::Tool { id, result, .. } = &mut final_context else {
            unreachable!()
        };
        *id = "fresh-post-tool-blocked-final".into();
        *result = blocked;
        final_document.workspaces[0].conversations[0]
            .contexts
            .push(final_context);
        assert!(!validate_tool_results(&previous, &final_document, &state).unwrap().refused());
    }

    #[test]
    fn model_requested_tool_input_requires_an_exact_context_receipt() {
        let directory = tempfile::tempdir().unwrap();
        let previous = default_document(directory.path());
        let mut changed = previous.clone();
        let workspace_path = changed.workspaces[0].path.clone();
        let conversation_id = changed.workspaces[0].conversations[0].id.clone();
        let requested_input: crate::model::JsonObject =
            serde_json::from_value(serde_json::json!({"path":"before-hook.md"})).unwrap();
        let (request, result) = {
            let ContextItem::Tool {
                tool_name,
                requested_input: context_requested_input,
                input,
                result,
                ..
            } = &mut changed.workspaces[0].conversations[0].contexts[3]
            else {
                panic!("seed context must be a tool call");
            };
            *context_requested_input = Some(requested_input.clone());
            (
                ToolExecutionRequest {
                    conversation_id,
                    workspace_path,
                    tool_name: tool_name.clone(),
                    input: input.clone(),
                },
                result.clone(),
            )
        };
        let state = AppState::default();

        state.record_receipt(&request, &result);
        assert!(validate_tool_results(&previous, &changed, &state).unwrap().refused());

        state.record_context_receipt(&request, &result, Some(&requested_input));
        assert!(!validate_tool_results(&previous, &changed, &state).unwrap().refused());

        let mut fresh = previous.clone();
        let mut fresh_context = changed.workspaces[0].conversations[0].contexts[3].clone();
        let ContextItem::Tool { id, .. } = &mut fresh_context else {
            unreachable!()
        };
        *id = "fresh-tool-with-requested-input".into();
        fresh.workspaces[0].conversations[0]
            .contexts
            .push(fresh_context);
        assert!(!validate_tool_results(&previous, &fresh, &state).unwrap().refused());

        let mut dropped_before_first_save = fresh.clone();
        let ContextItem::Tool {
            requested_input, ..
        } = dropped_before_first_save.workspaces[0].conversations[0]
            .contexts
            .last_mut()
            .unwrap()
        else {
            unreachable!()
        };
        *requested_input = None;
        assert!(validate_tool_results(&previous, &dropped_before_first_save, &state).unwrap().refused());

        let mut forged_before_first_save = changed.clone();
        let ContextItem::Tool {
            requested_input, ..
        } = &mut forged_before_first_save.workspaces[0].conversations[0].contexts[3]
        else {
            unreachable!()
        };
        requested_input.as_mut().unwrap().insert(
            "path".into(),
            serde_json::json!("renderer-forged-before-save.md"),
        );
        assert!(validate_tool_results(&previous, &forged_before_first_save, &state).unwrap().refused());

        let mut mutated_after_save = changed.clone();
        let ContextItem::Tool {
            requested_input, ..
        } = &mut mutated_after_save.workspaces[0].conversations[0].contexts[3]
        else {
            unreachable!()
        };
        requested_input.as_mut().unwrap().insert(
            "path".into(),
            serde_json::json!("renderer-forged-after-save.md"),
        );
        assert!(validate_tool_results(&changed, &mutated_after_save, &state).unwrap().refused());
    }

    #[test]
    fn persisted_tool_result_images_allow_only_exact_ordered_subset_removal() {
        fn image(id_byte: char, name: &str, width: u32) -> ImageAttachment {
            ImageAttachment {
                id: id_byte.to_string().repeat(64),
                name: name.into(),
                mime: "image/png".into(),
                width,
                height: 16,
                bytes: 123,
                short_id: None,
            }
        }

        let directory = tempfile::tempdir().unwrap();
        let mut previous = default_document(directory.path());
        let first = image('a', "first.png", 32);
        let second = image('b', "second.png", 48);
        let third = image('c', "third.png", 64);
        let ContextItem::Tool { result, .. } =
            &mut previous.workspaces[0].conversations[0].contexts[3]
        else {
            panic!("seed context must be a tool call");
        };
        result.images = vec![first.clone(), second.clone(), third.clone()];
        let state = AppState::default();

        let mut subset = previous.clone();
        let ContextItem::Tool { result, .. } =
            &mut subset.workspaces[0].conversations[0].contexts[3]
        else {
            unreachable!()
        };
        result.images = vec![first.clone(), third.clone()];
        assert!(!validate_tool_results(&previous, &subset, &state).unwrap().refused());

        let mut empty = previous.clone();
        let ContextItem::Tool { result, .. } =
            &mut empty.workspaces[0].conversations[0].contexts[3]
        else {
            unreachable!()
        };
        result.images.clear();
        assert!(!validate_tool_results(&previous, &empty, &state).unwrap().refused());

        let mut added = previous.clone();
        let ContextItem::Tool { result, .. } =
            &mut added.workspaces[0].conversations[0].contexts[3]
        else {
            unreachable!()
        };
        result.images.push(image('d', "added.png", 80));
        assert!(validate_tool_results(&previous, &added, &state).unwrap().refused());

        let mut changed_metadata = previous.clone();
        let ContextItem::Tool { result, .. } =
            &mut changed_metadata.workspaces[0].conversations[0].contexts[3]
        else {
            unreachable!()
        };
        result.images = vec![first.clone(), third.clone()];
        result.images[0].name = "renamed.png".into();
        assert!(validate_tool_results(&previous, &changed_metadata, &state).unwrap().refused());

        let mut reordered_while_removing = previous.clone();
        let ContextItem::Tool { result, .. } =
            &mut reordered_while_removing.workspaces[0].conversations[0].contexts[3]
        else {
            unreachable!()
        };
        result.images = vec![third.clone(), first.clone()];
        assert!(validate_tool_results(&previous, &reordered_while_removing, &state).unwrap().refused());

        let mut changed_output = subset.clone();
        let ContextItem::Tool { result, .. } =
            &mut changed_output.workspaces[0].conversations[0].contexts[3]
        else {
            unreachable!()
        };
        result.output.push_str(" tampered");
        assert!(validate_tool_results(&previous, &changed_output, &state).unwrap().refused());

        let mut changed_input = subset.clone();
        let ContextItem::Tool { input, .. } =
            &mut changed_input.workspaces[0].conversations[0].contexts[3]
        else {
            unreachable!()
        };
        input.insert("unexpected".into(), serde_json::json!(true));
        assert!(validate_tool_results(&previous, &changed_input, &state).unwrap().refused());

        let mut moved = previous.clone();
        let mut destination = moved.workspaces[0].conversations[0].clone();
        destination.id = "image-removal-destination".into();
        destination.contexts = vec![moved.workspaces[0].conversations[0].contexts.remove(3)];
        let ContextItem::Tool { result, .. } = &mut destination.contexts[0] else {
            unreachable!()
        };
        result.images = vec![second];
        moved.workspaces[0].conversations.push(destination);
        assert!(validate_tool_results(&previous, &moved, &state).unwrap().refused());
    }

    #[test]
    fn receipt_attested_tool_images_can_be_removed_before_the_first_persist() {
        let image = |id_byte: char, name: &str| ImageAttachment {
            id: id_byte.to_string().repeat(64),
            name: name.into(),
            mime: "image/png".into(),
            width: 32,
            height: 16,
            bytes: 123,
            short_id: None,
        };
        let directory = tempfile::tempdir().unwrap();
        let previous = default_document(directory.path());
        let mut next = previous.clone();
        let mut context = previous.workspaces[0].conversations[0].contexts[3].clone();
        let (request, attested_result) = {
            let ContextItem::Tool {
                id,
                tool_name,
                input,
                result,
                ..
            } = &mut context
            else {
                panic!("seed context must be a tool call");
            };
            *id = "fresh-tool-image-result".into();
            result.images = vec![image('a', "first.png"), image('b', "second.png")];
            (
                ToolExecutionRequest {
                    conversation_id: next.workspaces[0].conversations[0].id.clone(),
                    workspace_path: next.workspaces[0].path.clone(),
                    tool_name: tool_name.clone(),
                    input: input.clone(),
                },
                result.clone(),
            )
        };
        let state = AppState::default();
        state.record_receipt(&request, &attested_result);

        let ContextItem::Tool { result, .. } = &mut context else {
            unreachable!()
        };
        result.images.remove(0);
        next.workspaces[0].conversations[0].contexts.push(context);
        assert!(!validate_tool_results(&previous, &next, &state).unwrap().refused());

        let mut forged = next.clone();
        let ContextItem::Tool { result, .. } = forged.workspaces[0].conversations[0]
            .contexts
            .last_mut()
            .unwrap()
        else {
            unreachable!()
        };
        result.images[0].name = "forged.png".into();
        assert!(validate_tool_results(&previous, &forged, &state).unwrap().refused());

        let mut changed_output = next;
        let ContextItem::Tool { result, .. } = changed_output.workspaces[0].conversations[0]
            .contexts
            .last_mut()
            .unwrap()
        else {
            unreachable!()
        };
        result.output.push_str(" forged");
        assert!(validate_tool_results(&previous, &changed_output, &state).unwrap().refused());
    }

    #[test]
    fn ask_user_prompt_edit_preserves_result_without_receipt_and_question_count() {
        let directory = tempfile::tempdir().unwrap();
        let mut previous = default_document(directory.path());
        previous.workspaces[0].conversations[0]
            .contexts
            .push(ask_user_tool_context("editable-question"));

        let mut changed = previous.clone();
        let ContextItem::Tool { input, .. } = changed.workspaces[0].conversations[0]
            .contexts
            .last_mut()
            .unwrap()
        else {
            unreachable!()
        };
        *input = serde_json::from_value(serde_json::json!({
            "questions": [{
                "question": "Which direction should we take?",
                "header": "Direction",
                "options": [
                    { "label": "Simple", "description": "Keep it minimal" },
                    { "label": "Detailed", "description": "Show more context" },
                    { "label": "Custom", "description": "Use another direction" }
                ],
                "multiSelect": false
            }]
        }))
        .unwrap();

        let state = AppState::default();
        assert!(!validate_tool_results(&previous, &changed, &state).unwrap().refused());

        let mut count_changed = changed.clone();
        let ContextItem::Tool { input, .. } = count_changed.workspaces[0].conversations[0]
            .contexts
            .last_mut()
            .unwrap()
        else {
            unreachable!()
        };
        let first_question = input["questions"][0].clone();
        input.insert(
            "questions".into(),
            serde_json::json!([
                first_question,
                {
                    "question": "One more question?",
                    "header": "Extra",
                    "options": [
                        { "label": "Yes", "description": "Ask it" },
                        { "label": "No", "description": "Skip it" }
                    ],
                    "multiSelect": false
                }
            ]),
        );
        assert!(validate_tool_results(&previous, &count_changed, &state).unwrap().refused());

        let mut tampered_result = changed;
        let ContextItem::Tool { result, .. } = tampered_result.workspaces[0].conversations[0]
            .contexts
            .last_mut()
            .unwrap()
        else {
            unreachable!()
        };
        result.duration_ms = 1;
        assert!(validate_tool_results(&previous, &tampered_result, &state).unwrap().refused());

        let mut legacy_shaped = previous.clone();
        let ContextItem::Tool { input, .. } = legacy_shaped.workspaces[0].conversations[0]
            .contexts
            .last_mut()
            .unwrap()
        else {
            unreachable!()
        };
        *input = serde_json::from_value(serde_json::json!({
            "question": "Legacy-shaped edit?",
            "options": ["Yes", "No"]
        }))
        .unwrap();
        assert!(validate_tool_results(&previous, &legacy_shaped, &state).unwrap().refused());
    }

    #[test]
    fn subagent_receipt_commits_the_exact_recursive_record_and_stays_immutable() {
        let directory = tempfile::tempdir().unwrap();
        let previous = default_document(directory.path());
        let mut changed = previous.clone();
        let workspace_path = changed.workspaces[0].path.clone();
        let conversation_id = changed.workspaces[0].conversations[0].id.clone();
        let nested = subagent_nested_tool("nested-research-tool");
        let (nested_request, nested_result) = match &nested {
            ContextItem::Tool {
                tool_name,
                input,
                result,
                ..
            } => (
                ToolExecutionRequest {
                    conversation_id: conversation_id.clone(),
                    workspace_path: workspace_path.clone(),
                    tool_name: tool_name.clone(),
                    input: input.clone(),
                },
                result.clone(),
            ),
            _ => unreachable!(),
        };
        let context = subagent_record_tool("research-run", "researcher", vec![nested]);
        let (outer_request, outer_result, outer_subagent) = match &context {
            ContextItem::Tool {
                tool_name,
                input,
                result,
                subagent: Some(subagent),
                ..
            } => (
                ToolExecutionRequest {
                    conversation_id,
                    workspace_path,
                    tool_name: tool_name.clone(),
                    input: input.clone(),
                },
                result.clone(),
                subagent.clone(),
            ),
            _ => unreachable!(),
        };
        changed.workspaces[0].conversations[0]
            .contexts
            .push(context);

        let exact_outer = AppState::default();
        exact_outer.record_subagent_receipt(&outer_request, &outer_result, &outer_subagent);
        assert!(
            !validate_tool_results(&previous, &changed, &exact_outer).unwrap().refused(),
            "the outer fingerprint commits every nested tool result atomically"
        );
        let mut stripped_before_first_save = changed.clone();
        let ContextItem::Tool { subagent, .. } = stripped_before_first_save.workspaces[0]
            .conversations[0]
            .contexts
            .last_mut()
            .unwrap()
        else {
            unreachable!()
        };
        *subagent = None;
        assert!(
            validate_tool_results(&previous, &stripped_before_first_save, &exact_outer).unwrap().refused(),
            "the exact sidecar receipt must revoke the weaker ordinary receipt"
        );

        let ordinary_outer = AppState::default();
        ordinary_outer.record_receipt(&outer_request, &outer_result);
        ordinary_outer.record_receipt(&nested_request, &nested_result);
        assert!(
            validate_tool_results(&previous, &changed, &ordinary_outer).unwrap().refused(),
            "an ordinary result receipt must not attest a subagent audit snapshot"
        );

        // Once persisted, the exact outer record and all recursively nested
        // tool results survive a later save without relying on process-local
        // receipts.
        let no_live_receipts = AppState::default();
        assert!(!validate_tool_results(&changed, &changed.clone(), &no_live_receipts)
            .unwrap()
            .refused());

        let mut tampered_task = changed.clone();
        let ContextItem::Tool {
            subagent: Some(subagent),
            ..
        } = tampered_task.workspaces[0].conversations[0]
            .contexts
            .last_mut()
            .unwrap()
        else {
            unreachable!()
        };
        subagent.task = "renderer changed the task".into();
        assert!(validate_tool_results(&changed, &tampered_task, &no_live_receipts)
            .unwrap()
            .refused());

        let mut tampered_kind = changed.clone();
        let ContextItem::Tool {
            subagent: Some(subagent),
            ..
        } = tampered_kind.workspaces[0].conversations[0]
            .contexts
            .last_mut()
            .unwrap()
        else {
            unreachable!()
        };
        subagent.kind = SubagentRunKind::WorkflowStep;
        assert!(validate_tool_results(&changed, &tampered_kind, &no_live_receipts)
            .unwrap()
            .refused());

        let mut tampered_contexts = changed.clone();
        let ContextItem::Tool {
            subagent: Some(subagent),
            ..
        } = tampered_contexts.workspaces[0].conversations[0]
            .contexts
            .last_mut()
            .unwrap()
        else {
            unreachable!()
        };
        subagent
            .contexts
            .push(user_context("renderer-added-child-context"));
        assert!(validate_tool_results(&changed, &tampered_contexts, &no_live_receipts)
            .unwrap()
            .refused());

        let mut tampered_nested_result = changed.clone();
        let ContextItem::Tool {
            subagent: Some(subagent),
            ..
        } = tampered_nested_result.workspaces[0].conversations[0]
            .contexts
            .last_mut()
            .unwrap()
        else {
            unreachable!()
        };
        let ContextItem::Tool { result, .. } = &mut subagent.contexts[0] else {
            unreachable!()
        };
        result.output = "renderer changed the child result".into();
        assert!(
            validate_tool_results(&changed, &tampered_nested_result, &no_live_receipts)
                .unwrap()
                .refused()
        );

        let mut tampered_outer_result = changed.clone();
        let ContextItem::Tool { result, .. } = tampered_outer_result.workspaces[0].conversations[0]
            .contexts
            .last_mut()
            .unwrap()
        else {
            unreachable!()
        };
        result.output = "renderer changed the parent result".into();
        assert!(
            validate_tool_results(&changed, &tampered_outer_result, &no_live_receipts)
                .unwrap()
                .refused()
        );

        let mut stripped = changed.clone();
        let ContextItem::Tool { subagent, .. } = stripped.workspaces[0].conversations[0]
            .contexts
            .last_mut()
            .unwrap()
        else {
            unreachable!()
        };
        *subagent = None;
        // Removing a child record refuses the entire card through the ordinary
        // quarantine path: the renderer cannot keep the parent while erasing
        // its audit trail, but one card also cannot force a whole-conversation
        // rollback that resurrects stale provider/Agent references.
        let validation =
            validate_tool_results(&changed, &stripped, &no_live_receipts).unwrap();
        assert_eq!(validation.refused_context_ids(), ["research-run"]);
    }

    #[test]
    fn tool_context_and_receipt_cannot_move_between_conversations() {
        let directory = tempfile::tempdir().unwrap();
        let previous = default_document(directory.path());
        let mut changed = previous.clone();
        let source_conversation_id = changed.workspaces[0].conversations[0].id.clone();
        let moved = changed.workspaces[0].conversations[0].contexts.remove(3);
        let (tool_name, input, result) = match &moved {
            ContextItem::Tool {
                tool_name,
                input,
                result,
                ..
            } => (tool_name.clone(), input.clone(), result.clone()),
            _ => panic!("seed context must be a tool call"),
        };
        let request = ToolExecutionRequest {
            conversation_id: source_conversation_id,
            workspace_path: changed.workspaces[0].path.clone(),
            tool_name,
            input,
        };
        let mut destination = changed.workspaces[0].conversations[0].clone();
        destination.id = "conv_destination".into();
        destination.contexts = vec![moved];
        changed.workspaces[0].conversations.push(destination);
        let state = AppState::default();
        state.record_receipt(&request, &result);

        assert!(validate_tool_results(&previous, &changed, &state).unwrap().refused());
    }

    #[test]
    fn new_workspace_path_requires_backend_authorization() {
        let directory = tempfile::tempdir().unwrap();
        let selected = directory.path().join("selected-workspace");
        fs::create_dir(&selected).unwrap();
        let previous = default_document(directory.path());
        let mut changed = previous.clone();
        let mut workspace = changed.workspaces[0].clone();
        workspace.id = "ws_selected".into();
        workspace.path = selected.to_string_lossy().into_owned();
        workspace.conversations.clear();
        changed.workspaces.push(workspace);

        let state = AppState::default();
        assert!(validate_workspace_authorizations(&previous, &changed, &state).is_err());
        state.authorize_workspace(&selected).unwrap();
        assert!(validate_workspace_authorizations(&previous, &changed, &state).is_ok());
    }
}
