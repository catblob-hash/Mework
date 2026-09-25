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
    file_attachments::validate_file_list,
    image_attachments::validate_image_list,
    model::{
        AgentDefinition, AgentDefinitionBinding, AgentDefinitionMemory, AgentDefinitionSource,
        AgentModelSelection, AppDocument, ContextItem, Conversation, ConversationPresetSettings,
        ConversationSettings, ForkModelBinding, ToolResult, Workspace, WorkspaceKind,
        MAX_SEARCH_CUTOFF_LIMIT, MAX_SEARCH_MAX_RESULTS,
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
pub const SCHEMA_VERSION: u32 = 3;
const TEMPORARY_WORKSPACE_ID: &str = "__temporary__";
const MAX_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;
const MAX_WEB_SEARCHES_PER_CALL: u32 = 99_999;
const MAX_AGENT_DEFINITIONS: usize = 256;
const MAX_RETAINED_AGENT_DEFINITION_IDENTITIES: usize = 4096;
const MAX_AGENT_DEFINITION_SOURCE_KEY_CHARS: usize = 256;
/// Limits prevent malformed documents from causing unbounded startup work.
const MAX_ENVIRONMENT_TOOLS: usize = 128;
const MAX_ENVIRONMENT_TOOL_NAME_CHARS: usize = 64;
const MAX_ENVIRONMENT_TOOL_ARGUMENTS: usize = 8;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn load_or_initialize(path: &Path) -> Result<AppDocument, String> {
    if !path.exists() {
        let store = crate::conversation_store::store_for(path)?;
        let has_existing_conversations = store
            .conversation_workspaces()
            .map(|owners| !owners.is_empty())
            .unwrap_or(false);
        let mut document = default_document();
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
        seed_builtin_capabilities(path, &mut document);
        seed_local_shell(&mut document);
        seed_conversations(&store, &document)?;
        seed_preset_templates(&store, &document)?;
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

/// Writes the built-in capability files and points the shipped presets at them.
///
/// The ids can only be minted here. `stable_id` hashes the absolute path of the
/// file it names, so neither seed document can carry them as literals — the
/// renderer seed ships empty lists and this fills them in on the machine that
/// owns the paths.
fn seed_builtin_capabilities(path: &Path, document: &mut AppDocument) {
    let Some(app_data) = path.parent() else {
        return;
    };
    let selection = crate::capability_seed::seed_builtin_capabilities(app_data);
    apply_builtin_capability_selection(document, &selection);
}

/// Selects the seeded capabilities in every shipped preset.
///
/// Hooks are deliberately absent: a hook id is a hash of its position in
/// `hooks.json`, so any edit to that file renumbers the rest, and a selected
/// hook id that no longer resolves fails every run of the conversation closed.
/// The built-in hooks are shipped to be deleted freely, which is incompatible
/// with being selected. Skills and MCP servers dangle harmlessly by comparison —
/// discovery skips an id it cannot find and the run continues.
fn apply_builtin_capability_selection(
    document: &mut AppDocument,
    selection: &crate::capability_seed::BuiltinCapabilitySelection,
) {
    if selection.skill_ids.is_empty() && selection.mcp_ids.is_empty() {
        return;
    }
    for preset in &mut document.presets.conversation_presets {
        if crate::catalog::seeded_template_id(&preset.id).is_empty() {
            continue;
        }
        preset.settings.skill_ids = selection.skill_ids.clone();
        preset.settings.mcp_ids = selection.mcp_ids.clone();
    }
}

/// Gives the shipped presets this machine's most preferred shell and no other.
///
/// The seed lists every backend's command tool, because which of them this
/// machine has is a question only a probe can answer, and the first launch is
/// the first moment there is a machine to ask. Looking a few names up on
/// `PATH` is cheap enough to do inline, and it does not wait for
/// [`crate::machine_shells::install`], which the startup path runs later.
fn seed_local_shell(document: &mut AppDocument) {
    let local = crate::machine_shells::probe_local();
    apply_seeded_shell(document, local.os, &local.backends());
}

/// Narrows every shipped preset's shell command tools to the first backend in
/// `os`'s priority order that `available` has. A machine where the probe found
/// none still gets its OS's first: the tool list hides a shell the machine
/// lacks, and the user's own install can supply it later.
fn apply_seeded_shell(
    document: &mut AppDocument,
    os: crate::shell_backend::MachineOs,
    available: &[crate::shell_backend::ShellBackend],
) {
    let chosen = crate::shell_backend::preferred_backend(os, available)
        .or_else(|| crate::shell_backend::backends_for(os).first().copied());
    for preset in &mut document.presets.conversation_presets {
        if crate::catalog::seeded_template_id(&preset.id).is_empty() {
            continue;
        }
        preset.settings.enabled_tools.retain(|name| {
            match crate::shell_backend::ShellBackend::of_tool(name) {
                Some(backend) => Some(backend) == chosen,
                None => true,
            }
        });
    }
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

/// The system prompt a shipped preset opens with, keyed by preset id.
///
/// A conversation template is the only channel a preset has for carrying prompt
/// text of its own: `ConversationPresetSettings` has no system-prompt field, and
/// the prompt profile can only override the host's own fixed keys. A
/// `ContextItem::System` row with `local_only: false` is what
/// `aisdk::step::system_prompt_parts` lifts into the request's dynamic system
/// half, so this really does travel as a system prompt rather than as history.
///
/// Each text describes how that CLI works rather than what this host already
/// says for itself; the host's own sections are injected separately and the two
/// are deduplicated only on a byte-identical match.
fn seeded_preset_prompt(preset_id: &str) -> Option<&'static str> {
    match preset_id {
        crate::catalog::CODEX_PRESET_ID => Some(
            "你在 Mework 里以 Codex 的工作方式做事。\n\
             \n\
             - 先勘察再动手：改一处之前先看清它的调用点与相邻代码，让新代码读起来像它周围的代码。\n\
             - 说完成之前先验证：跑过的命令和它的输出才算证据；跑不通就直说跑不通，不要用「应该没问题」收尾。\n\
             - 一次只推进一件事，改动保持可回滚。边界清楚、能独立验证的子任务派给子代理并行做；需要整体判断的留在主线。\n\
             - 删除、覆盖、推送、对外发布这类难以撤回的动作，先确认再执行；上一次的批准不延续到下一次。\n\
             - 交回结论时先给结论，再给证据和仍未解决的问题；引用文件给完整路径。",
        ),
        crate::catalog::CLAUDE_CODE_PRESET_ID => Some(
            "你在 Mework 里以 Claude Code 的工作方式做事。\n\
             \n\
             - 先读后写：动手前把相关文件、调用点和既有约定读清楚，写出的代码在注释密度、命名和惯用法上与周围一致。\n\
             - 交付即验收：声称做完、修好或跑通之前，必须真的执行过验证命令并看过输出。测试红了就把输出贴出来，跳过的步骤要讲明。\n\
             - 按用户实际提出的范围交付，不擅自收窄或放大。真有问题先用一两句说清顾虑，再把活干完。\n\
             - 边界清楚、可独立验证的子任务派给子代理并行做；需要推理与交叉验证的留在主线。\n\
             - 删除、覆盖、推送、对外发布这类难以撤回的动作，先确认再执行；上一次的批准不延续到下一次。",
        ),
        _ => None,
    }
}

/// Writes the body behind each shipped preset's `template_id`, once.
///
/// Idempotent by row existence, and that guard is load-bearing:
/// `put_template` replaces a template's whole body, and this runs again on the
/// "anchor missing but conversation store alive" recovery path — writing
/// unconditionally would silently discard a body the user had edited.
fn seed_preset_templates(
    store: &crate::conversation_store::ConversationStore,
    document: &AppDocument,
) -> Result<(), String> {
    let existing = store
        .templates()?
        .into_iter()
        .map(|template| template.id)
        .collect::<std::collections::HashSet<_>>();
    for preset in &document.presets.conversation_presets {
        if preset.template_id.is_empty() || existing.contains(&preset.template_id) {
            continue;
        }
        let Some(prompt) = seeded_preset_prompt(&preset.id) else {
            continue;
        };
        // The name stays empty like every other template: nothing displays it,
        // and the preset page addresses this row through `template_id` alone.
        store.put_template(
            &preset.template_id,
            "",
            &[ContextItem::System {
                id: format!("ctx_{}", uuid::Uuid::new_v4().simple()),
                content: prompt.into(),
                // Never `true`: a local-only system row is a lifecycle
                // diagnostic and `system_prompt_parts` drops it, so the prompt
                // would be seeded and then never sent.
                local_only: false,
                hook_execution: None,
                created_at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            }],
        )?;
    }
    Ok(())
}

/// Loads at startup, rebuilding from a seed document after banking any failed load.
pub fn load_or_recover(path: &Path) -> Result<AppDocument, String> {
    let load_error = match load_or_initialize(path) {
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
    let mut document = default_document();
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
    seed_builtin_capabilities(path, &mut document);
    seed_local_shell(&mut document);
    let store = crate::conversation_store::store_for(path)?;
    seed_conversations(&store, &document)?;
    seed_preset_templates(&store, &document)?;
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
    // The accepted older formats differ only by additions: schema 1 lacks a
    // host-owned SQLite fork-intent table, and schema 2 lacks the model
    // `promptCache` key, which `migrate_persisted_models` fills with its
    // default below. Their anchor configuration remains unchanged, so preserve
    // it in place.
    if original_schema == 1 || original_schema == 2 {
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
    // The two role shaping numbers are result shaping, not capability
    // selection: a document written past the ceiling is pulled back onto it
    // rather than failed, and the run applies the clamped answer.
    canonicalize_agent_definition_shaping(&mut document);
    isolate_invalid_loaded_conversations(&mut document);
    canonicalize_draft_conversation(&mut document);
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
            machine: None,
            additional_workspaces: Vec::new(),
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
        document: canonical,
        ..
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
    // The role shaping numbers are result shaping, not capability selection, so
    // a proposal past the ceiling is pulled back onto it rather than failed.
    // Runs after `adopt_authoritative_conversations`, which is what decides
    // which conversation settings survive this transition at all.
    canonicalize_agent_definition_shaping(&mut canonical);
    canonicalize_draft_conversation(&mut canonical);
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

/// Keeps a stored new-task draft from failing the document it rides in.
///
/// A draft is disposable, unlike a conversation: retired tool names leave it the
/// way they leave conversations, and a draft that still does not validate is
/// discarded rather than making the whole document unloadable or unsaveable.
fn canonicalize_draft_conversation(document: &mut AppDocument) {
    let tool_names = document
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<HashSet<_>>();
    let Some(draft) = document.global_settings.draft_conversation.as_mut() else {
        return;
    };
    let mut seen = HashSet::new();
    draft
        .settings
        .enabled_tools
        .retain(|name| tool_names.contains(name.as_str()) && seen.insert(name.clone()));
    let invalid = draft.preset_id.len() > 128
        || validate_conversation_settings_shape("", &draft.settings, &tool_names).is_err();
    if invalid {
        document.global_settings.draft_conversation = None;
    }
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
/// - `familySettings` used to accept `claude_executable`, the path to the user's
///   own Claude Code. Mework now ships that executable, and the retired key is an
///   unknown variant in a map *key*, which fails the document just as hard.
///
/// A third key, `promptCache`, is newer than schema 2 and defaults on: an archive
/// that omits it, or carries a non-boolean, is made concrete here so the saved
/// document always spells the attribute out.
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
        // `claude_executable` is retired: Mework ships the Claude Code build the
        // `claude_agent` family drives, so there is no path for the user to name.
        // A retired setting is an unknown enum *variant* in the key position of
        // `BTreeMap<FamilySetting, String>`, so serde rejects the whole document
        // rather than skipping the entry — every archive that ever showed that
        // field would be quarantined. Ask serde which keys still exist rather
        // than naming the survivors here.
        if let Some(settings) = provider
            .get_mut("familySettings")
            .and_then(serde_json::Value::as_object_mut)
        {
            settings.retain(|name, _| {
                serde_json::from_value::<crate::model::FamilySetting>(serde_json::Value::String(
                    name.clone(),
                ))
                .is_ok()
            });
        }
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
            if !matches!(
                model
                    .get("reasoningContent")
                    .and_then(serde_json::Value::as_str),
                Some("plaintext" | "encrypted")
            ) {
                let resolved = if encrypted { "encrypted" } else { "plaintext" };
                model.insert("reasoningContent".into(), serde_json::json!(resolved));
            }
            if !model
                .get("promptCache")
                .is_some_and(serde_json::Value::is_boolean)
            {
                model.insert("promptCache".into(), serde_json::json!(true));
            }
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

/// Pulls every role's two result-shaping numbers back onto their ceilings.
///
/// They are result shaping rather than capability selection, so a document
/// written past the ceiling is corrected in place rather than failed: the run
/// applies the clamped answer, and the next save persists it. Roles appear both
/// in conversation presets and in each conversation's materialized settings, so
/// both surfaces are walked.
fn canonicalize_agent_definition_shaping(document: &mut AppDocument) {
    let clamp = |definition: &mut AgentDefinition| {
        definition.max_results = definition.max_results.min(MAX_SEARCH_MAX_RESULTS);
        definition.compression_cutoff =
            definition.compression_cutoff.min(MAX_SEARCH_CUTOFF_LIMIT);
    };
    for preset in &mut document.presets.conversation_presets {
        for definition in &mut preset.settings.agent_definitions {
            clamp(definition);
        }
    }
    for workspace in &mut document.workspaces {
        for conversation in &mut workspace.conversations {
            for definition in &mut conversation.settings.agent_definitions {
                clamp(definition);
            }
        }
    }
}

fn validate_workspace_authorizations(
    previous: &AppDocument,
    document: &AppDocument,
    state: &AppState,
) -> Result<(), String> {
    // A workspace's identity is its machine and its path: one machine's
    // `/srv/app` is not another's, and a remote path spelled like a local one
    // must not be read as the local grant.
    //
    // Every directory a previous project held counts as held — its workspace 1
    // and its further workspaces alike — so reordering a project's workspaces,
    // or moving one from one project to another, is not read as a new grant.
    let previous_directories = previous
        .workspaces
        .iter()
        .filter(|workspace| workspace.kind == WorkspaceKind::Directory)
        .flat_map(project_directories)
        .collect::<Vec<_>>();
    let previous_exact = previous_directories
        .iter()
        .map(|(machine, path)| (crate::run_environment::env_key(*machine), *path))
        .collect::<HashSet<_>>();
    // Only local directories have a canonical form this host can compute, and
    // only a proposal that misses `previous_exact` ever needs one.
    let previous_canonical = LazyCanonicalKeys::new(
        previous_directories
            .iter()
            .filter(|(machine, _)| machine.is_none())
            .map(|(_, path)| *path)
            .collect(),
    );
    let previous_by_id = previous
        .workspaces
        .iter()
        .map(|workspace| (workspace.id.as_str(), workspace))
        .collect::<HashMap<_, _>>();
    let previous_machine_by_id = previous
        .workspaces
        .iter()
        .filter(|workspace| workspace.kind == WorkspaceKind::Directory)
        .filter_map(|workspace| {
            workspace
                .machine
                .as_ref()
                .map(|machine| (workspace.id.as_str(), machine))
        })
        .collect::<HashMap<_, _>>();

    for workspace in &document.workspaces {
        if workspace.kind != WorkspaceKind::Directory {
            continue;
        }
        authorize_project_directory(
            workspace.machine.as_ref(),
            &workspace.path,
            &previous_exact,
            &previous_canonical,
            state,
        )
        .map_err(|error| {
            // The same workspace stood on another machine a moment ago and
            // now claims to be local at the same spelling: a load path
            // dropped `machine`, not a user re-picking a directory. Say so.
            // The local check that just failed would otherwise explain it
            // as a POSIX path not being "absolute" on Windows.
            match (
                &workspace.machine,
                previous_machine_by_id.get(workspace.id.as_str()),
            ) {
                (None, Some(machine)) => format!(
                    "工作区 {} 位于另一台机器上（{}），但这次提交没有带 machine 字段，宿主不能把 {} 当作本机目录: {error}",
                    workspace.id,
                    crate::run_environment::env_key(Some(machine)),
                    workspace.path
                ),
                _ => format!("工作区 {} 未获授权: {error}", workspace.id),
            }
        })?;
        for (offset, member) in workspace.additional_workspaces.iter().enumerate() {
            let position = offset + 2;
            authorize_project_directory(
                member.machine.as_ref(),
                &member.path,
                &previous_exact,
                &previous_canonical,
                state,
            )
            .map_err(|error| {
                // The same rule as workspace 1: a member that was on another
                // machine at this spelling and now claims to be local lost its
                // `machine` on the way here.
                let dropped_machine = member
                    .machine
                    .is_none()
                    .then(|| previous_by_id.get(workspace.id.as_str()))
                    .flatten()
                    .and_then(|held| {
                        held.additional_workspaces.iter().find_map(|existing| {
                            (existing.path == member.path)
                                .then_some(existing.machine.as_ref())
                                .flatten()
                        })
                    });
                match dropped_machine {
                    Some(machine) => format!(
                        "项目 {} 的工作区 {position}（{}）位于另一台机器上（{}），但这次提交没有带 machine 字段，宿主不能把它当作本机目录: {error}",
                        workspace.id,
                        member.path,
                        crate::run_environment::env_key(Some(machine))
                    ),
                    None => format!(
                        "项目 {} 的工作区 {position}（{}）未获授权: {error}",
                        workspace.id, member.path
                    ),
                }
            })?;
        }
        reject_duplicate_project_workspaces(workspace)?;
    }

    validate_additional_directory_authorizations(previous, document, state)?;

    Ok(())
}

/// A project's directories in workspace order — workspace 1, then its further
/// workspaces — each as the machine it is on and the path recorded there.
fn project_directories(workspace: &Workspace) -> Vec<(Option<&crate::model::RunTarget>, &str)> {
    std::iter::once((workspace.machine.as_ref(), workspace.path.as_str()))
        .chain(
            workspace
                .additional_workspaces
                .iter()
                .map(|member| (member.machine.as_ref(), member.path.as_str())),
        )
        .collect()
}

/// The canonical keys of some held local directories, resolved on first use.
///
/// Resolving a path touches it, and every save walks every project and every
/// conversation, not only the one being edited. On macOS a touch under Desktop,
/// Documents, Downloads, iCloud Drive or a removable or network volume raises a
/// privacy prompt (again after every rebuild of an ad-hoc signed build), and a
/// disconnected network share can stall the save. An unchanged directory is
/// always re-proposed at the spelling it was held at, so the exact-text check
/// answers it without the filesystem; the keys are needed only when a proposal
/// misses that check, and then they are the same set an eager build would give.
///
/// The caller passes local paths only: a directory on another machine has no
/// canonical form on this one.
struct LazyCanonicalKeys<'a> {
    paths: Vec<&'a str>,
    keys: std::cell::OnceCell<HashSet<String>>,
}

impl<'a> LazyCanonicalKeys<'a> {
    fn new(paths: Vec<&'a str>) -> Self {
        Self {
            paths,
            keys: std::cell::OnceCell::new(),
        }
    }

    fn contains(&self, key: &str) -> bool {
        self.keys
            .get_or_init(|| {
                self.paths
                    .iter()
                    .filter_map(|path| AppState::workspace_key(Path::new(path)))
                    .collect()
            })
            .contains(key)
    }

    /// Whether the held paths have been resolved yet — what the tests check to
    /// show an exact match never reached the filesystem.
    #[cfg(test)]
    fn is_resolved(&self) -> bool {
        self.keys.get().is_some()
    }
}

/// Whether one directory may stand in a project: the previous document already
/// held it, or a picker of the host's own returned it in this session.
///
/// A local directory is compared by canonical key as well as by literal text, so
/// re-proposing one spelled differently is not read as a new grant. A directory
/// on another machine has no canonical form this host can compute, so its grant
/// is the machine plus the exact text the remote browser returned — the same
/// rule attached workspaces follow. The error is the picker check's own; the
/// caller says which workspace it was about.
///
/// The checks run cheapest first: the literal text, which needs no filesystem,
/// then the canonical key, which resolves the proposed path and, only if that
/// succeeds, the held ones.
fn authorize_project_directory(
    machine: Option<&crate::model::RunTarget>,
    path: &str,
    held_exact: &HashSet<(String, &str)>,
    held_canonical: &LazyCanonicalKeys<'_>,
    state: &AppState,
) -> Result<(), String> {
    let machine_key = crate::run_environment::env_key(machine);
    if held_exact.contains(&(machine_key.clone(), path)) {
        return Ok(());
    }
    if machine.is_some() {
        return state.require_remote_workspace_authorization(&machine_key, path);
    }
    if AppState::workspace_key(Path::new(path)).is_some_and(|key| held_canonical.contains(&key)) {
        return Ok(());
    }
    state.require_workspace_authorization(Path::new(path))
}

/// Refuses a project that lists one directory twice, workspace 1 included.
///
/// Two numbers for one directory would give the model two addresses for the
/// same files and make "which workspace is this" ambiguous in every path it is
/// shown. Local directories are compared by canonical key when the directory
/// resolves — two spellings of one checkout are one workspace — and by exact
/// text otherwise; a remote directory can only be compared by the machine and
/// the exact text, since only that machine can resolve its own paths.
///
/// A project with fewer than two local directories is not resolved at all: one
/// canonical key has nothing to collide with, so the answer is known without
/// touching the filesystem — which, run on every save for every project, would
/// otherwise reach into each one (see [`LazyCanonicalKeys`] for what that costs
/// on macOS).
fn reject_duplicate_project_workspaces(workspace: &Workspace) -> Result<(), String> {
    let directories = project_directories(workspace);
    let compare_canonical = directories
        .iter()
        .filter(|(machine, _)| machine.is_none())
        .count()
        > 1;
    let mut seen_exact = HashSet::new();
    let mut seen_canonical = HashSet::new();
    for (offset, (machine, path)) in directories.into_iter().enumerate() {
        let duplicate_text =
            !seen_exact.insert((crate::run_environment::env_key(machine), path.to_owned()));
        let duplicate_directory = compare_canonical
            && machine.is_none()
            && AppState::workspace_key(Path::new(path))
                .is_some_and(|key| !seen_canonical.insert(key));
        if duplicate_text || duplicate_directory {
            return Err(format!(
                "项目 {} 的工作区 {} 与前面的工作区是同一个目录: {path}",
                workspace.id,
                offset + 1
            ));
        }
    }
    Ok(())
}

/// Applies the workspace rule to the extra directories every conversation in the
/// document may work in.
fn validate_additional_directory_authorizations(
    previous: &AppDocument,
    document: &AppDocument,
    state: &AppState,
) -> Result<(), String> {
    let mut previous_by_id: HashMap<&str, &Conversation> = HashMap::new();
    for workspace in &previous.workspaces {
        for conversation in &workspace.conversations {
            previous_by_id.insert(conversation.id.as_str(), conversation);
        }
    }
    for workspace in &document.workspaces {
        for conversation in &workspace.conversations {
            validate_additional_directories(
                conversation,
                previous_by_id.get(conversation.id.as_str()).copied(),
                state,
            )?;
            validate_worktree_records(
                conversation,
                previous_by_id.get(conversation.id.as_str()).copied(),
                state,
            )?;
        }
    }
    Ok(())
}

/// The number of workspaces one conversation may attach. It exists so a
/// malfunctioning renderer cannot grow the list without bound; the composer's
/// own chip row stops being readable long before this, and the number is also
/// the address space the model is given, which has to stay readable at a glance.
pub(crate) const MAX_ADDITIONAL_DIRECTORIES: usize = 32;

/// The most workspaces one project may hold, workspace 1 included (so fifteen
/// further ones). Every conversation in the project addresses all of them
/// before its own attached workspaces, so the bound keeps the shared prefix of
/// every conversation's numbered list short enough to read at a glance.
pub(crate) const MAX_PROJECT_WORKSPACES: usize = 16;

/// Holds one conversation's attached workspaces to the workspace rule.
///
/// They widen that conversation's reach exactly as its own workspace does, so
/// they are held to the same standard: an entry the renderer proposes is
/// accepted only if the conversation already had it, or if a picker of the
/// host's own returned it in this session — the native dialog for a directory
/// on this machine, the remote browser for one on another.
///
/// A local entry is compared by canonical key as well as by literal text, so
/// re-proposing a directory spelled differently is not read as a new grant. A
/// remote entry has no canonical form the host can compute — only that machine
/// can resolve its own paths — so its identity is the machine and the exact
/// text the browser returned.
pub(crate) fn validate_additional_directories(
    conversation: &Conversation,
    previous: Option<&Conversation>,
    state: &AppState,
) -> Result<(), String> {
    let proposed = conversation.effective_attached_workspaces();
    if proposed.len() > MAX_ADDITIONAL_DIRECTORIES {
        return Err(format!(
            "对话 {} 的工作区超过 {MAX_ADDITIONAL_DIRECTORIES} 个",
            conversation.id
        ));
    }
    let held = previous
        .map(Conversation::effective_attached_workspaces)
        .unwrap_or_default();
    // Resolved only for a local entry the exact comparison below misses: this
    // runs for every conversation on every save.
    let held_keys = LazyCanonicalKeys::new(
        held.iter()
            .filter(|workspace| workspace.machine.is_none())
            .map(|workspace| workspace.path.as_str())
            .collect(),
    );
    for workspace in &proposed {
        if held.iter().any(|existing| existing == workspace) {
            continue;
        }
        let Some(machine) = &workspace.machine else {
            if AppState::workspace_key(Path::new(&workspace.path))
                .is_some_and(|key| held_keys.contains(&key))
            {
                continue;
            }
            state
                .require_workspace_authorization(Path::new(&workspace.path))
                .map_err(|error| {
                    let dropped_machine = held.iter().find_map(|existing| {
                        (existing.path == workspace.path)
                            .then_some(existing.machine.as_ref())
                            .flatten()
                    });
                    match dropped_machine {
                        Some(machine) => format!(
                            "对话 {} 的工作区 {} 位于另一台机器上（{}），但这次提交没有带 machine 字段，宿主不能把它当作本机目录: {error}",
                            conversation.id,
                            workspace.path,
                            crate::run_environment::env_key(Some(machine))
                        ),
                        None => format!("对话 {} 的工作区未获授权: {error}", conversation.id),
                    }
                })?;
            continue;
        };
        state
            .require_remote_workspace_authorization(
                &crate::run_environment::env_key(Some(machine)),
                &workspace.path,
            )
            .map_err(|error| format!("对话 {} 的工作区未获授权: {error}", conversation.id))?;
    }
    Ok(())
}

/// Refuses a worktree record the host did not make.
///
/// A record is where the conversation's tools run in place of the workspace it
/// names, so writing one is as good as granting a directory: a record whose
/// path is anything but a worktree `create_conversation_worktree` checked out
/// in this process — which authorizes it, on its machine — is refused, unless
/// the conversation already held that exact record.
pub(crate) fn validate_worktree_records(
    conversation: &Conversation,
    previous: Option<&Conversation>,
    state: &AppState,
) -> Result<(), String> {
    if conversation.worktrees.len() > MAX_PROJECT_WORKSPACES {
        return Err(format!(
            "对话 {} 的隔离工作树超过 {MAX_PROJECT_WORKSPACES} 个",
            conversation.id
        ));
    }
    let held = previous
        .map(|previous| previous.worktrees.as_slice())
        .unwrap_or_default();
    for worktree in &conversation.worktrees {
        if held.contains(worktree) {
            continue;
        }
        let machine = worktree
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.machine.as_ref());
        let authorized = match machine {
            Some(machine) => state.require_remote_workspace_authorization(
                &crate::run_environment::env_key(Some(machine)),
                &worktree.path,
            ),
            None => state.require_workspace_authorization(Path::new(&worktree.path)),
        };
        authorized.map_err(|error| {
            format!(
                "对话 {} 的隔离工作树 {} 不是本应用建立的: {error}",
                conversation.id, worktree.path
            )
        })?;
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
    /// Machine the directory lives on; absent is the host machine, which is what
    /// every anchor written before workspaces could be remote means.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    machine: Option<crate::model::RunTarget>,
    /// The project's workspaces after workspace 1. Absent in every anchor
    /// written before projects could hold more than one directory. This shell
    /// is rebuilt field by field in both directions, so a field left out here
    /// would be silently dropped on the next save.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    additional_workspaces: Vec<crate::model::AttachedWorkspace>,
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
    let anchor: PersistedAnchor =
        serde_json::from_value(value).map_err(|error| format!("数据文档 JSON 无效: {error}"))?;
    let store = crate::conversation_store::store_for(path)?;

    let mut seen = HashSet::new();
    let mut workspaces = Vec::with_capacity(anchor.workspaces.len());
    for shell in anchor.workspaces {
        let conversations = match store.workspace_conversations(&shell.id) {
            Ok(conversations) => conversations,
            Err(error) => {
                eprintln!(
                    "工作区 {} 的对话无法装配（{error}），本次按空列表处理",
                    shell.id
                );
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
            machine: shell.machine,
            additional_workspaces: shell.additional_workspaces,
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

/// Re-reads every conversation body from the store into an already-assembled
/// document, in place.
///
/// The in-memory snapshot's bodies stop at the last conversation *command*: a
/// run writes its rows straight to the store from the thread that owns the
/// model stream and deliberately never commits a whole-document snapshot for
/// them (`docs/persistence.md`, 「下一次运行读哪一份」). So the snapshot's idea
/// of a conversation ends at the user message that started the round, and any
/// path that hands a whole document *back to the renderer* has to re-read the
/// bodies — otherwise it answers with a conversation that is one round short,
/// the question without its answer.
///
/// A store that will not open, or a workspace that will not read, keeps the
/// snapshot's own bodies: a stale body beats no body, the same posture
/// `stored_conversation` takes when the authoritative read fails.
pub fn refresh_conversation_bodies(path: &Path, document: &mut AppDocument) {
    let store = match crate::conversation_store::store_for(path) {
        Ok(store) => store,
        Err(error) => {
            eprintln!("对话库无法打开（{error}），本次沿用内存快照的对话正文");
            return;
        }
    };
    let mut seen = HashSet::new();
    for workspace in &mut document.workspaces {
        match store.workspace_conversations(&workspace.id) {
            Ok(conversations) => workspace.conversations = conversations,
            Err(error) => eprintln!(
                "工作区 {} 的对话正文无法重读（{error}），本次沿用内存快照",
                workspace.id
            ),
        }
        for conversation in &workspace.conversations {
            seen.insert(conversation.id.clone());
        }
    }
    adopt_unowned_conversations(&store, document, &seen);
    // The same shaping `read_document` applies to bodies it assembles. Skipping
    // it would hand the renderer contexts that no longer compare equal to the
    // ones it last saw, which is what the unchanged-card fast path relies on.
    canonicalize_tool_payload_numbers(document);
    canonicalize_agent_definition_shaping(document);
    isolate_invalid_loaded_conversations(document);
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
fn mark_orphaned_trailing_user_contexts(document: &mut AppDocument) -> Vec<(String, ContextItem)> {
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
static LAYOUT_WRITE_CACHE: std::sync::Mutex<
    Option<HashMap<std::path::PathBuf, HashMap<String, u64>>>,
> = std::sync::Mutex::new(None);

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
                machine: workspace.machine.clone(),
                additional_workspaces: workspace.additional_workspaces.clone(),
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
                format!(
                    "重置时无法删除 token 流水账 {}：{error}",
                    candidate.display()
                )
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
            .assets
            .api_providers
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
            crate::model::validate_model_id(&model.id)
                .map_err(|error| format!("提供商 {} 的模型 ID 无效: {error}", provider.id))?;
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
    validate_execution_environments(&document.assets.execution_environments)?;
    validate_environment_tools(&document.global_settings.environment_tools)?;
    let conversation_preset_ids = unique_nonempty(
        document
            .presets
            .conversation_presets
            .iter()
            .map(|preset| preset.id.as_str()),
        "对话预设 ID",
    )?;
    if conversation_preset_ids.is_empty()
        && !document.presets.default_conversation_preset_id.is_empty()
    {
        return Err("没有对话预设时，新对话默认预设 ID 必须为空".into());
    }
    if !conversation_preset_ids.is_empty()
        && !conversation_preset_ids
            .contains(document.presets.default_conversation_preset_id.as_str())
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
    // Checked like a workspace's remembered settings: a draft has not run, so
    // its roles meet the full check when it becomes a conversation.
    if let Some(draft) = &document.global_settings.draft_conversation {
        validate_conversation_settings_shape("未发送的新任务的对话设置", &draft.settings, &tool_names)?;
        if draft.preset_id.len() > 128 {
            return Err("未发送的新任务的预设 ID 过长".into());
        }
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
                validate_project_member_shape(workspace)?;
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
                // The temporary project is one scratch directory per
                // conversation; there is no shared root for a second workspace
                // to sit beside.
                if !workspace.additional_workspaces.is_empty() {
                    return Err("临时工作区不得设置额外工作区".into());
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

/// The shape a directory project's further workspaces must have: a bounded
/// list of non-blank paths. Whether each entry was granted, and whether two of
/// them name the same directory, needs the previous document and the
/// filesystem, so [`validate_workspace_authorizations`] decides that.
fn validate_project_member_shape(workspace: &Workspace) -> Result<(), String> {
    if workspace.additional_workspaces.len() + 1 > MAX_PROJECT_WORKSPACES {
        return Err(format!(
            "项目 {} 的工作区超过 {MAX_PROJECT_WORKSPACES} 个",
            workspace.id
        ));
    }
    for (offset, member) in workspace.additional_workspaces.iter().enumerate() {
        if member.path.trim().is_empty() {
            return Err(format!(
                "项目 {} 的工作区 {} 路径为空",
                workspace.id,
                offset + 2
            ));
        }
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

    // Applied preset IDs may be dangling; validate length but not catalog existence.
    if conversation.preset_id.len() > 128 {
        return Err(format!("对话 {} 的预设 ID 过长", conversation.id));
    }

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
        if task.id.trim().is_empty()
            || task.id.len() > 128
            || !aborted_task_ids.insert(task.id.as_str())
        {
            return Err(format!(
                "对话 {} 的用户中止任务记录 ID 无效或重复",
                conversation.id
            ));
        }
        if !matches!(
            task.source_kind.as_str(),
            "subagent" | "workflow" | "terminal" | "shell" | "browser"
        ) {
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
        if message.content.trim().is_empty()
            && message.images.is_empty()
            && message.files.is_empty()
        {
            return Err(format!(
                "对话 {} 的排队消息文字、图片与文件不能同时为空",
                conversation.id
            ));
        }
        validate_image_list(
            &message.images,
            &format!("对话 {} 的排队消息 {}", conversation.id, message.id),
        )?;
        validate_file_list(
            &message.files,
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
        crate::model::validate_agent_type_name(&definition.name)?;
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
            // A tombstone the host minted itself rides back in every later
            // proposal: the renderer mirrors the conversation body it was
            // handed, and only the roles page filters tombstones out of what it
            // writes. Echoing one back is not a renderer-authored deletion, so
            // it is dropped here and the committed tombstone below is what
            // reaches the ledger — the renderer's bytes are never honoured.
            // Inventing a tombstone the host never issued stays refused: that
            // would hide a live identity and force its memory epoch forward.
            if previous_by_identity
                .get(&agent_definition_identity(&definition))
                .is_some_and(|previous| previous.deleted)
            {
                continue;
            }
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
        // The two shaping numbers are result shaping, not capability selection:
        // an out-of-range proposal is pulled to the ceiling rather than failed,
        // the same reading the run itself would have given a clamped setting.
        definition.max_results = definition.max_results.min(MAX_SEARCH_MAX_RESULTS);
        definition.compression_cutoff = definition.compression_cutoff.min(MAX_SEARCH_CUTOFF_LIMIT);
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
        .map(|preset| {
            (
                preset.id.as_str(),
                preset.settings.agent_definitions.as_slice(),
            )
        })
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
    // One table per workspace rather than per machine, so the bound leaves room
    // for many projects of up to sixteen workspaces each.
    const MAX_ENV_TABLES: usize = 1024;
    const MAX_ENV_VARS_PER_TABLE: usize = 128;
    const MAX_ENV_VALUE_CHARS: usize = 8192;
    const MAX_HOST_CHARS: usize = 512;
    const MAX_PATH_FIELD_CHARS: usize = 4096;

    let ids = unique_trimmed_nonempty(
        assets
            .ssh_machines
            .iter()
            .map(|machine| machine.id.as_str()),
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
        for (label, value) in [("身份文件路径", &machine.identity_file)] {
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
        let valid_machine_key = |machine: &str| {
            machine == "local"
                || machine.strip_prefix("wsl:").is_some_and(|distro| {
                    crate::run_environment::validate_wsl_distro_name(distro).is_ok()
                })
                || machine
                    .strip_prefix("ssh:")
                    .is_some_and(|id| !id.trim().is_empty() && id.len() <= 128)
        };
        // A table belongs to a workspace: `<machine key>|<path>`, see
        // `run_environment::workspace_env_key`. A bare machine key is a table
        // from before variables moved onto workspaces; the renderer spreads it
        // over that machine's workspaces on load, and until it saves, the key is
        // tolerated but never read. Tables of removed workspaces may dangle.
        let valid_key = match key.split_once('|') {
            Some((machine, path)) => {
                valid_machine_key(machine)
                    && !path.trim().is_empty()
                    && path.chars().count() <= MAX_PATH_FIELD_CHARS
                    && !path.chars().any(char::is_control)
            }
            None => valid_machine_key(key),
        };
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
            if crate::child_environment::is_private_child_environment_name(std::ffi::OsStr::new(
                variable,
            )) {
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
    // A WSL distribution runs POSIX shells only, so its agent shell must be one.
    if assets.wsl_agent_shells.len() > 256 {
        return Err("WSL 发行版的代理 shell 设置不能超过 256 条".into());
    }
    for (distro, backend) in &assets.wsl_agent_shells {
        crate::run_environment::validate_wsl_distro_name(distro)?;
        if !crate::shell_backend::is_registered(crate::shell_backend::MachineOs::Wsl, *backend) {
            return Err(format!(
                "WSL 发行版 {distro} 的代理 shell 不能是 {}",
                backend.display_name()
            ));
        }
    }
    Ok(())
}

/// Saving an MCP server does not judge whether it can be dialled.
///
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
    validate_search_result_shaping(label, settings.max_results, settings.compression_cutoff)?;
    for (rules, name) in [
        (&settings.include_domains, "白名单"),
        (&settings.exclude_domains, "黑名单"),
    ] {
        if rules.len() > crate::model::MAX_SEARCH_DOMAIN_RULES {
            return Err(format!("{label}的域名{name}条目过多"));
        }
        for rule in rules.iter() {
            if rule.chars().count() > 512 || rule.chars().any(char::is_control) {
                return Err(format!("{label}的域名{name}规则无效"));
            }
        }
    }
    Ok(())
}

/// Validates the two result-shaping numbers a conversation or a role carries.
///
/// 0 is inside both ranges rather than below them: it is the answer "no limit",
/// and refusing it would make the one setting that turns a limit off illegal to
/// save.
fn validate_search_result_shaping(
    label: &str,
    max_results: u32,
    compression_cutoff: u32,
) -> Result<(), String> {
    if max_results > crate::model::MAX_SEARCH_MAX_RESULTS {
        return Err(format!(
            "{label}的搜索结果数必须在 0–{} 之间（0 表示不设限）",
            crate::model::MAX_SEARCH_MAX_RESULTS
        ));
    }
    if compression_cutoff > crate::model::MAX_SEARCH_CUTOFF_LIMIT {
        return Err(format!(
            "{label}的搜索结果截断预算必须在 0–{} 之间（0 表示不压缩）",
            crate::model::MAX_SEARCH_CUTOFF_LIMIT
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
/// A conversation's or a preset's sandbox. Its lists reach every machine's
/// agent with every command, so they are bounded.
fn validate_sandbox_settings(
    owner: &str,
    sandbox: &crate::model::SandboxSettings,
) -> Result<(), String> {
    const MAX_SANDBOX_ENTRIES: usize = 256;
    const MAX_PATH_FIELD_CHARS: usize = 4096;
    for (label, entries) in [
        ("沙箱网络白名单", &sandbox.network.allow),
        ("沙箱网络黑名单", &sandbox.network.deny),
        ("沙箱可写目录", &sandbox.writable),
        ("沙箱禁读路径", &sandbox.deny_read),
    ] {
        if entries.len() > MAX_SANDBOX_ENTRIES {
            return Err(format!("{owner}的{label}不能超过 {MAX_SANDBOX_ENTRIES} 条"));
        }
        for entry in entries {
            let entry = entry.trim();
            if entry.is_empty() || entry.chars().count() > MAX_PATH_FIELD_CHARS {
                return Err(format!("{owner}的{label}的条目不能为空或过长"));
            }
            if entry.chars().any(char::is_control) {
                return Err(format!("{owner}的{label}的条目不能包含控制字符"));
            }
        }
    }
    for (label, entries) in [("沙箱可写目录", &sandbox.writable), ("沙箱禁读路径", &sandbox.deny_read)] {
        for entry in entries {
            let entry = entry.trim();
            let absolute = entry.starts_with('/')
                || entry.starts_with('~')
                || (entry.len() >= 3 && entry.as_bytes()[1] == b':' && entry.as_bytes()[0].is_ascii_alphabetic());
            if !absolute {
                return Err(format!("{owner}的{label} {entry} 必须是绝对路径或以 ~ 开头"));
            }
        }
    }
    for pattern in sandbox.network.allow.iter().chain(&sandbox.network.deny) {
        if pattern.trim().chars().any(|c| c.is_whitespace() || c == '/') {
            return Err(format!(
                "{owner}的沙箱网络规则 {pattern} 应是主机名（可带 :端口），不能是网址"
            ));
        }
    }
    Ok(())
}

fn validate_conversation_settings_shape(
    label: &str,
    settings: &ConversationSettings,
    tool_names: &HashSet<&str>,
) -> Result<(), String> {
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
    validate_sandbox_settings(label, &settings.sandbox)?;
    // Renderer state the host never reads, bounded so it cannot grow without limit.
    if settings.remembered_tool_families.len() > 8
        || settings
            .remembered_tool_families
            .values()
            .any(|rows| rows.len() > 64 || rows.iter().any(|row| row.chars().count() > 128))
    {
        return Err(format!("{label}记住的工具组过多或过长"));
    }
    Ok(())
}

fn validate_conversation_preset_settings(
    label: &str,
    settings: &ConversationPresetSettings,
    tool_names: &HashSet<&str>,
) -> Result<(), String> {
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
    validate_sandbox_settings(label, &settings.sandbox)?;
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

/// Checks a body about to be written back to a conversation template, as a
/// context tree standing on its own.
///
/// A template body belongs to no conversation, so the id ledger and the row
/// count start empty and are thrown away: this is the shape check the template
/// write path (`update_conversation_template`) runs, not a document migration.
/// What it stops is a renderer-edited body that would store and then fail to
/// load for every reader of it.
pub(crate) fn validate_template_contexts(contexts: &[ContextItem]) -> Result<(), String> {
    let mut context_ids = HashSet::new();
    let mut context_count = 0usize;
    validate_context_tree(contexts, &mut context_ids, &mut context_count)
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
    crate::model::validate_agent_type_name(&binding.name)
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
                files,
                ..
            } => {
                if content.trim().is_empty() && images.is_empty() && files.is_empty() {
                    return Err(format!("用户上下文 {id} 的文字、图片与文件不能同时为空"));
                }
                validate_image_list(images, &format!("用户上下文 {id}"))?;
                validate_file_list(files, &format!("用户上下文 {id}"))?;
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
    // An extra working directory widens this conversation's filesystem boundary,
    // so the per-conversation write path enforces the same picker rule that
    // `validate_workspace_authorizations` applies to a whole-document save.
    validate_additional_directories(
        conversation,
        previous_entry.map(|(_, previous)| previous),
        state,
    )?;
    validate_worktree_records(
        conversation,
        previous_entry.map(|(_, previous)| previous),
        state,
    )?;
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

/// Where a save looks up the receipts of a directory project's conversation,
/// and so where they have to be recorded: workspace 1 as registered, and the
/// conversation's worktree of it when that is on this machine. A forged
/// worktree record does not widen this — the save refuses records the host did
/// not make (`validate_worktree_records`).
///
/// A workspace 1 on another machine has only its registered root here, though
/// its calls run from a host-side anchor directory: whoever records a receipt
/// there has to record it under this root too (`execute_tool`).
pub(crate) fn tool_receipt_roots<'a>(
    workspace: &'a Workspace,
    conversation: &'a Conversation,
) -> Vec<&'a str> {
    let primary = crate::model::AttachedWorkspace {
        machine: workspace.machine.clone(),
        path: workspace.path.clone(),
    };
    std::iter::once(workspace.path.as_str())
        .chain(
            conversation
                .worktree_for(1, &primary)
                .filter(|_| workspace.machine.is_none())
                .map(|worktree| worktree.path.as_str()),
        )
        .collect()
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

    let execution_roots = tool_receipt_roots(workspace, conversation);

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
                            (WorkspaceKind::Directory, Some(subagent)) => {
                                execution_roots.iter().any(|root| {
                                    state.has_context_subagent_receipt(
                                        root,
                                        &conversation.id,
                                        tool_name,
                                        input,
                                        requested_input.as_ref(),
                                        result,
                                        subagent,
                                    )
                                })
                            }
                            (_, Some(subagent)) => state
                                .has_context_subagent_receipt_in_any_workspace(
                                    &conversation.id,
                                    tool_name,
                                    input,
                                    requested_input.as_ref(),
                                    result,
                                    subagent,
                                ),
                            (WorkspaceKind::Directory, None) => {
                                execution_roots.iter().any(|root| {
                                    state.has_context_receipt(
                                        root,
                                        &conversation.id,
                                        tool_name,
                                        input,
                                        requested_input.as_ref(),
                                        result,
                                    )
                                })
                            }
                            (_, None) => state.has_context_receipt_in_any_workspace(
                                &conversation.id,
                                tool_name,
                                input,
                                requested_input.as_ref(),
                                result,
                            ),
                        };
                        exact
                            || if workspace.kind == WorkspaceKind::Directory {
                                execution_roots.iter().any(|root| {
                                    state.has_context_receipt_or_image_removal(
                                        Some(root),
                                        &conversation.id,
                                        tool_name,
                                        input,
                                        requested_input.as_ref(),
                                        result,
                                        subagent.as_ref(),
                                    )
                                })
                            } else {
                                state.has_context_receipt_or_image_removal(
                                    None,
                                    &conversation.id,
                                    tool_name,
                                    input,
                                    requested_input.as_ref(),
                                    result,
                                    subagent.as_ref(),
                                )
                            }
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
        ConversationBranch, EndpointType, ImageAttachment, ModelProfile, ModelUsage, QueuedMessage,
        ReasoningContent, SubagentRunKind, SubagentRunRecord, SubagentRunStatus,
        ToolExecutionRequest, UserAbortedTaskMetrics, UserAbortedTaskRecord,
    };

    /// Builds a document with a role explicitly bound to a provider model.
    fn document_with_bound_role(provider_id: &str, model_id: &str) -> AppDocument {
        let mut document = default_document();
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
        let mut previous = default_document();
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
            !adopted
                .settings
                .enabled_tools
                .iter()
                .any(|name| name == retired),
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

    /// The unsent new task's settings round-trip through a save; a retired tool
    /// leaves them, and a draft that still does not validate is dropped rather
    /// than refusing the whole document.
    #[test]
    fn a_stored_draft_sheds_retired_tools_and_never_blocks_the_save() {
        let state = AppState::default();
        let retired = "goal";
        let previous = default_document();
        let mut settings = previous.workspaces[0].conversations[0].settings.clone();
        let kept = settings.enabled_tools.clone();
        settings.enabled_tools.push(retired.into());

        let mut proposal = previous.clone();
        proposal.global_settings.draft_conversation =
            Some(crate::model::DraftConversationSnapshot {
                settings: settings.clone(),
                preset_id: "preset_codex".into(),
            });
        let canonical = prepare_save_transition(&previous, &proposal, &state)
            .expect("带着已退役工具的草稿不能挡住保存")
            .document;
        let draft = canonical
            .global_settings
            .draft_conversation
            .expect("草稿自己的设置必须保留");
        assert_eq!(draft.settings.enabled_tools, kept, "只丢退役工具名");
        assert_eq!(draft.preset_id, "preset_codex");

        let mut unusable = proposal.clone();
        unusable.global_settings.draft_conversation =
            Some(crate::model::DraftConversationSnapshot {
                settings,
                preset_id: "p".repeat(129),
            });
        let canonical = prepare_save_transition(&previous, &unusable, &state)
            .expect("校验不过的草稿直接丢掉，文档照常保存")
            .document;
        assert!(canonical.global_settings.draft_conversation.is_none());
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
        load_with_stored_model_key(
            directory,
            provider_id,
            "reasoningContent",
            stored.map(|value| serde_json::json!(value)),
            SCHEMA_VERSION,
        )
    }

    /// Writes an archive at `schema` whose provider models carry `stored`
    /// verbatim under `key` (omitting the key entirely for `None`), then loads it.
    fn load_with_stored_model_key(
        directory: &Path,
        provider_id: &str,
        key: &str,
        stored: Option<serde_json::Value>,
        schema: u32,
    ) -> ModelProfile {
        let mut document = default_document();
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
        anchor["schemaVersion"] = serde_json::json!(schema);
        for provider in anchor["assets"]["apiProviders"].as_array_mut().unwrap() {
            for model in provider["models"].as_array_mut().unwrap() {
                let model = model.as_object_mut().unwrap();
                match &stored {
                    Some(value) => {
                        model.insert(key.into(), value.clone());
                    }
                    None => {
                        model.remove(key);
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

    /// `claude_executable` was where the user named their own Claude Code
    /// install. Mework ships that executable now, so the setting is gone — and a
    /// retired setting sits in the *key* position of a `BTreeMap<FamilySetting,
    /// String>`, where an unknown variant fails the whole document. An archive
    /// that carries one must still load, with the field dropped.
    #[test]
    fn an_archive_carrying_the_retired_claude_executable_setting_still_loads() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.v1.json");
        save_all(&path, &default_document()).unwrap();

        // Reach past the serializer twice over: the enum can no longer express
        // this key, and the built-in Claude Agent row is seeded by the renderer
        // rather than by `default_document`.
        let mut anchor: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let providers = anchor["assets"]["apiProviders"].as_array_mut().unwrap();
        providers.push(serde_json::json!({
            "id": "claude-agent-legacy",
            "name": "Claude Agent",
            "family": "claude_agent",
            "baseUrl": "",
            "familySettings": { "claude_executable": "C:\\Users\\me\\.local\\bin\\claude.exe" },
        }));
        // A surviving setting on another family must not be swept up with it.
        providers.push(serde_json::json!({
            "id": "bedrock-legacy",
            "name": "Bedrock",
            "family": "bedrock",
            "baseUrl": "",
            "familySettings": { "region": "us-east-1", "claude_executable": "/nonsense" },
        }));
        fs::write(&path, serde_json::to_vec_pretty(&anchor).unwrap()).unwrap();

        let loaded = read_document(&path).expect("旧档案必须还能装载");
        let reloaded = read_document(&path).unwrap();
        assert_eq!(loaded, reloaded, "迁移不是幂等的");
        let find = |id: &str| {
            loaded
                .assets
                .api_providers
                .iter()
                .find(|provider| provider.id == id)
                .unwrap_or_else(|| panic!("{id} 那一行必须还在"))
        };
        assert!(
            find("claude-agent-legacy").family_settings.is_empty(),
            "{:?}",
            find("claude-agent-legacy").family_settings
        );
        assert_eq!(
            find("bedrock-legacy")
                .family_settings
                .get(&crate::model::FamilySetting::Region)
                .map(String::as_str),
            Some("us-east-1"),
            "只有退役的键该被丢掉"
        );
        assert_eq!(
            find("bedrock-legacy").family_settings.len(),
            1,
            "退役的键在别的家族上也要丢掉"
        );
    }

    /// `promptCache` is newer than schema 2. A schema-2 archive that never had
    /// the key, or a hand-edited one carrying a non-boolean, loads as enabled —
    /// the default Claude Code applies — while an explicit `false` is the user's
    /// choice and survives. The family plays no part: a boolean has no
    /// family-derived default.
    #[test]
    fn an_archive_without_a_prompt_cache_flag_loads_as_enabled() {
        let directory = tempfile::tempdir().unwrap();
        let cases: [(&str, Option<serde_json::Value>, u32, bool); 5] = [
            ("missing-2", None, 2, true),
            ("missing-3", None, SCHEMA_VERSION, true),
            (
                "string",
                Some(serde_json::json!("yes")),
                SCHEMA_VERSION,
                true,
            ),
            ("false", Some(serde_json::json!(false)), 2, false),
            ("true", Some(serde_json::json!(true)), SCHEMA_VERSION, true),
        ];
        for (name, stored, schema, expected) in cases {
            for provider_id in ["anthropic_messages", "openai_chat"] {
                let case = directory.path().join(format!("{name}-{provider_id}"));
                std::fs::create_dir_all(&case).unwrap();
                let model = load_with_stored_model_key(
                    &case,
                    provider_id,
                    "promptCache",
                    stored.clone(),
                    schema,
                );
                assert_eq!(model.prompt_cache, expected, "{name} on {provider_id}");
            }
        }
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
        let mut document = default_document();
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
            prompt_cache: true,
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
            fetch_provider: None,
            max_results: crate::model::DEFAULT_SEARCH_MAX_RESULTS,
            compression_cutoff: crate::model::DEFAULT_SEARCH_CUTOFF_LIMIT,
            domain_filter: None,
            include_domains: Vec::new(),
            exclude_domains: Vec::new(),
            template_id: None,
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
                if let Some(definition) = conversation.settings.agent_definitions.get_mut(index) {
                    edit(definition);
                }
            }
        }
    }

    fn default_conversation_preset_mut(
        document: &mut AppDocument,
    ) -> &mut ConversationPresetSettings {
        let id = document.presets.default_conversation_preset_id.clone();
        &mut document
            .presets
            .conversation_presets
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
            files: Vec::new(),
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
            provider_call_id: None,
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
            provider_call_id: None,
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
            provider_call_id: None,
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
            provider_call_id: None,
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
        let base = default_document();

        let machine = crate::model::SshMachineConfig {
            id: "ssh_a".into(),
            name: "devbox".into(),
            host: "user@devbox.local".into(),
            port: 2222,
            ..Default::default()
        };

        let mut valid = base.clone();
        valid.assets.execution_environments.ssh_machines = vec![machine.clone()];
        valid.assets.execution_environments.env_vars.insert(
            "local|C:\\work\\app".into(),
            [("FOO".to_owned(), "bar".to_owned())].into(),
        );
        valid.assets.execution_environments.env_vars.insert(
            "wsl:Ubuntu|/home/dev/app".into(),
            [("A".to_owned(), "1".to_owned())].into(),
        );
        valid
            .assets
            .execution_environments
            .env_vars
            .insert("ssh:ssh_a|~/app".into(), Default::default());
        assert!(validate_shape(&valid).is_ok(), "完整配置必须被接受");

        // Dangling SSH environment tables remain valid, and so do tables from
        // before variables belonged to workspaces.
        let mut dangling = base.clone();
        for key in ["ssh:gone|/srv/app", "ssh:gone", "local", "wsl:Ubuntu"] {
            dangling
                .assets
                .execution_environments
                .env_vars
                .insert(key.into(), Default::default());
        }
        assert!(validate_shape(&dangling).is_ok());

        for key in ["local|", "local|  ", "docker:x|/srv", "wsl:bad;name|/srv", "local|a\nb"] {
            let mut bad_workspace_key = base.clone();
            bad_workspace_key
                .assets
                .execution_environments
                .env_vars
                .insert(key.into(), Default::default());
            assert!(
                validate_shape(&bad_workspace_key).is_err(),
                "工作区环境键 {key:?} 必须被拒绝"
            );
        }

        let mut bad_host = base.clone();
        bad_host.assets.execution_environments.ssh_machines =
            vec![crate::model::SshMachineConfig {
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
        assert!(
            validate_shape(&option_host).is_err(),
            "主机地址不能以 - 开头"
        );

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
        assert!(
            validate_shape(&reserved).is_err(),
            "harness 私有名必须被拒绝"
        );

        let mut control_value = base.clone();
        control_value.assets.execution_environments.env_vars.insert(
            "local".into(),
            [(
                "FOO".to_owned(),
                "a
b"
                .to_owned(),
            )]
            .into(),
        );
        assert!(
            validate_shape(&control_value).is_err(),
            "值里的控制字符必须被拒绝"
        );

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
        assert!(
            validate_shape(&bad_distro).is_err(),
            "非法发行版名必须被拒绝"
        );

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
    fn validate_shape_bounds_environment_tools() {
        let base = default_document();

        // Environment-tool executables are bare names used for process startup.
        let mut path_executable = base.clone();
        path_executable.global_settings.environment_tools =
            vec![crate::model::EnvironmentToolDefinition {
                name: "evil".into(),
                executable: "../../bin/sh".into(),
                version_args: Vec::new(),
            }];
        assert!(
            validate_shape(&path_executable).is_err(),
            "可执行名不能是一条路径"
        );
    }

    /// A runtime server carries only what dialing needs from a parsed `mcp.json` entry.
    #[test]
    fn a_runtime_server_mirrors_only_the_dialable_fields_of_its_config() {
        let config = crate::model::McpServerConfig {
            id: "mcp_files".into(),
            name: "Files".into(),
            description: "本地文件".into(),
            transport: crate::model::McpTransportKind::Stdio,
            command: "npx".into(),
            args: vec!["-y".into(), "server-filesystem".into()],
            env: [("TOKEN".to_owned(), "secret".to_owned())]
                .into_iter()
                .collect(),
            cwd: std::env::temp_dir().to_string_lossy().into_owned(),
            env_passthrough: vec!["GITHUB_TOKEN".into()],
            url: "https://ignored.example".into(),
            ..Default::default()
        };

        let runtime = crate::mcp::RuntimeMcpServer::from_config(&config);

        assert_eq!(runtime.server_id, "mcp_files");
        assert_eq!(runtime.artifact_id, "mcp_files");
        match &runtime.transport {
            crate::mcp::RuntimeMcpTransport::Stdio {
                command,
                args,
                env,
                cwd,
                env_passthrough,
            } => {
                assert_eq!(command, "npx");
                assert_eq!(args, &["-y".to_owned(), "server-filesystem".to_owned()]);
                assert_eq!(env.get("TOKEN").map(String::as_str), Some("secret"));
                assert_eq!(cwd.as_deref(), Some(config.cwd.as_str()));
                assert_eq!(env_passthrough, &config.env_passthrough);
            }
            other => panic!("stdio 配置必须映射成 stdio 传输，得到 {other:?}"),
        }

        // Redaction is required because command lines and environment variables can leak through logs and crash reports.
        let debug = format!("{runtime:?}");
        assert!(
            !debug.contains("secret"),
            "环境变量不能出现在 Debug 输出里: {debug}"
        );
        assert!(
            !debug.contains("npx"),
            "启动命令不能出现在 Debug 输出里: {debug}"
        );

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
        let document = default_document();
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
        let document = default_document();
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
        invalid_fetch.assets.web_search.providers[jina].fetch_api_host =
            "h ttps://r.jina.ai".into();
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

        // A valid override confirms the preceding rejection assertions are specific.
        let mut ordinary = document;
        ordinary.assets.web_search.providers[tavily].enabled = true;
        ordinary.assets.web_search.providers[tavily].search_api_host =
            "https://gateway.example/tavily".into();
        ordinary.assets.web_search.providers[searxng].engines = vec!["google".into()];
        ordinary.assets.web_search.providers[searxng].basic_auth_username = "searx".into();
        validate_shape(&ordinary).expect("an ordinary provider override still saves");
    }

    /// Result shaping belongs to the conversation, so its bounds are checked
    /// there — including that 0 saves, since 0 is the one value that turns each
    /// limit off.
    #[test]
    fn conversation_result_shaping_accepts_zero_and_rejects_only_the_ceiling() {
        let document = default_document();
        let shaped = |max_results, compression_cutoff| {
            let mut candidate = document.clone();
            for preset in &mut candidate.presets.conversation_presets {
                preset.settings.web_search.max_results = max_results;
                preset.settings.web_search.compression_cutoff = compression_cutoff;
            }
            candidate
        };

        validate_shape(&shaped(0, 0)).expect("0 is the no-limit answer on both");
        assert!(validate_shape(&shaped(
            crate::model::MAX_SEARCH_MAX_RESULTS + 1,
            crate::model::DEFAULT_SEARCH_CUTOFF_LIMIT
        ))
        .is_err());
        assert!(validate_shape(&shaped(
            crate::model::DEFAULT_SEARCH_MAX_RESULTS,
            crate::model::MAX_SEARCH_CUTOFF_LIMIT + 1
        ))
        .is_err());
    }

    #[test]
    fn persisted_workspace_and_conversation_ids_reject_whitespace_aliases() {
        let document = default_document();

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
        let mut document = default_document();
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
            files: Vec::new(),
            created_at: "2026-07-24T00:00:00Z".into(),
        });
        conversation.queued_messages.push(QueuedMessage {
            id: "image-only-queued".into(),
            content: String::new(),
            images: vec![image],
            files: Vec::new(),
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
    fn file_only_messages_persist_and_invalid_file_lists_are_refused() {
        let directory = tempfile::tempdir().unwrap();
        let file = crate::model::FileAttachment {
            id: "c".repeat(64),
            name: "notes.md".into(),
            format: crate::model::FileAttachmentFormat::Text,
            bytes: 12,
            tokens: 3,
            pages: None,
        };
        let mut document = default_document();
        let conversation = &mut document.workspaces[0].conversations[0];
        conversation.contexts.push(ContextItem::User {
            id: "file-only-user".into(),
            content: String::new(),
            images: Vec::new(),
            files: vec![file.clone()],
            created_at: "2026-07-24T00:00:00Z".into(),
        });
        conversation.queued_messages.push(QueuedMessage {
            id: "file-only-queued".into(),
            content: String::new(),
            images: Vec::new(),
            files: vec![file.clone()],
            created_at: "2026-07-24T00:00:01Z".into(),
        });
        assert!(validate_shape(&document).is_ok());
        let path = directory.path().join("document.json");
        save_all(&path, &document).unwrap();
        assert_eq!(read_document(&path).unwrap(), document);

        let mut duplicated = document.clone();
        let ContextItem::User { files, .. } = duplicated.workspaces[0].conversations[0]
            .contexts
            .last_mut()
            .unwrap()
        else {
            unreachable!()
        };
        files.push(file.clone());
        let error = validate_shape(&duplicated).unwrap_err();
        assert!(error.contains("用户上下文 file-only-user"), "{error}");

        let mut forged_queue = document;
        forged_queue.workspaces[0].conversations[0].queued_messages[0].files[0].bytes = 0;
        let error = validate_shape(&forged_queue).unwrap_err();
        assert!(error.contains("排队消息 file-only-queued"), "{error}");
        assert!(error.contains("invalid file attachment"), "{error}");

        let mut empty = default_document();
        empty.workspaces[0].conversations[0]
            .queued_messages
            .push(QueuedMessage {
                id: "empty-queued".into(),
                content: "  ".into(),
                images: Vec::new(),
                files: Vec::new(),
                created_at: "2026-07-24T00:00:01Z".into(),
            });
        assert!(validate_shape(&empty).unwrap_err().contains("不能同时为空"));
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
        let mut exact = default_document();
        exact.workspaces[0].conversations[0]
            .contexts
            .push(ContextItem::User {
                id: "aggregate-user-exact".into(),
                content: String::new(),
                images: exact_limit.clone(),
                files: Vec::new(),
                created_at: "2026-07-24T00:00:00Z".into(),
            });
        assert!(validate_shape(&exact).is_ok());

        let mut oversized_user = default_document();
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
                files: Vec::new(),
                created_at: "2026-07-24T00:00:00Z".into(),
            });
        let error = validate_shape(&oversized_user).unwrap_err();
        assert!(error.contains("用户上下文 aggregate-user-bytes"));
        assert!(error.contains("20 MiB"));

        let mut oversized_queue = default_document();
        oversized_queue.workspaces[0].conversations[0]
            .queued_messages
            .push(QueuedMessage {
                id: "aggregate-queue-pixels".into(),
                content: String::new(),
                images: (0..5).map(|index| image(index, 1, 4096, 4096)).collect(),
                files: Vec::new(),
                created_at: "2026-07-24T00:00:00Z".into(),
            });
        let error = validate_shape(&oversized_queue).unwrap_err();
        assert!(error.contains("排队消息 aggregate-queue-pixels"));
        assert!(error.contains("64 MP"));

        let mut oversized_tool = default_document();
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
        let document = default_document();
        let store = crate::conversation_store::store_for(&path).unwrap();
        seed_conversations(&store, &document).unwrap();
        save_all(&path, &document).unwrap();
        assert_eq!(read_document(&path).unwrap(), document);
    }

    #[test]
    fn a_row_written_after_the_last_command_reaches_a_refreshed_document() {
        // A run writes its rows straight to the conversation store and nothing
        // commits a whole-document snapshot for them, so the snapshot's idea of
        // a conversation stops at the user message that opened the round. A
        // renderer handed that document reloads a conversation whose answer is
        // missing — and goes on running from it.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state").join("document.v1.json");
        let document = default_document();
        let store = crate::conversation_store::store_for(&path).unwrap();
        seed_conversations(&store, &document).unwrap();
        save_all(&path, &document).unwrap();

        let conversation_id = document.workspaces[0].conversations[0].id.clone();
        let answer = ContextItem::Assistant {
            id: "ctx-settled-answer".into(),
            content: "回合的最后一条回答".into(),
            round: Some(1),
            model_turn_id: None,
            interrupted: false,
            sources: Vec::new(),
            created_at: "2026-09-19T02:00:00.000Z".into(),
        };
        store
            .upsert_contexts(
                &conversation_id,
                std::slice::from_ref(&answer),
                crate::conversation_store::ContextStatus::Settled,
            )
            .unwrap();

        let mut snapshot = document.clone();
        assert!(!snapshot.workspaces[0].conversations[0]
            .contexts
            .iter()
            .any(|context| context.id() == answer.id()));

        refresh_conversation_bodies(&path, &mut snapshot);
        assert_eq!(
            snapshot.workspaces[0].conversations[0]
                .contexts
                .last()
                .map(ContextItem::id),
            Some(answer.id())
        );
        // Only the bodies are re-read. Settings reach the renderer from the
        // snapshot, which is the one place they are ever edited.
        assert_eq!(snapshot.presets, document.presets);
        assert_eq!(snapshot.global_settings, document.global_settings);
    }

    #[test]
    fn refreshed_bodies_follow_the_store_even_when_it_is_shorter() {
        // The store is authoritative, not longer: a user who deleted the tail
        // of a conversation must not have it handed back by the next reload.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state").join("document.v1.json");
        let document = default_document();
        let store = crate::conversation_store::store_for(&path).unwrap();
        seed_conversations(&store, &document).unwrap();
        save_all(&path, &document).unwrap();

        let conversation_id = document.workspaces[0].conversations[0].id.clone();
        let mut snapshot = document.clone();
        snapshot.workspaces[0].conversations[0]
            .contexts
            .push(ContextItem::Assistant {
                id: "ctx-deleted-answer".into(),
                content: "用户已经删掉的一条".into(),
                round: Some(1),
                model_turn_id: None,
                interrupted: false,
                sources: Vec::new(),
                created_at: "2026-09-19T02:00:00.000Z".into(),
            });

        refresh_conversation_bodies(&path, &mut snapshot);
        assert_eq!(
            snapshot.workspaces[0].conversations[0].contexts,
            store
                .conversation(&conversation_id)
                .unwrap()
                .unwrap()
                .contexts
        );
        assert!(!snapshot.workspaces[0].conversations[0]
            .contexts
            .iter()
            .any(|context| context.id() == "ctx-deleted-answer"));
    }

    #[test]
    fn conversation_preset_references_are_validated_and_settings_stand_alone() {
        let document = default_document();

        let mut missing_default = document.clone();
        missing_default.presets.default_conversation_preset_id = "missing".into();
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
            .enabled_tools
            .pop();
        assert!(validate_shape(&diverged).is_ok());
    }

    #[test]
    fn zero_conversation_presets_use_an_empty_default_reference() {
        let mut document = default_document();
        document.presets.conversation_presets.clear();
        document.presets.default_conversation_preset_id.clear();
        assert!(validate_shape(&document).is_ok());

        document.presets.default_conversation_preset_id = "missing".into();
        assert!(validate_shape(&document).unwrap_err().contains("必须为空"));
    }

    #[test]
    fn user_aborted_task_records_round_trip_and_validate() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.json");
        let mut document = default_document();
        document.workspaces[0].conversations[0].user_aborted_tasks = vec![UserAbortedTaskRecord {
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
        }];

        validate_shape(&document).unwrap();
        save_all(&path, &document).unwrap();
        let restored = read_document(&path).unwrap();
        assert_eq!(
            restored.workspaces[0].conversations[0]
                .user_aborted_tasks
                .len(),
            1
        );

        let mut invalid = document;
        invalid.workspaces[0].conversations[0].user_aborted_tasks[0].reason = "failed".into();
        assert!(validate_shape(&invalid).unwrap_err().contains("原因无效"));
    }

    #[test]
    fn product_default_document_ships_no_conversations_and_round_trips() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.template-preset.json");
        let document = crate::catalog::product_default_document();

        // Product seeds contain no conversations; the renderer creates the first real conversation from the first message.
        assert!(document
            .workspaces
            .iter()
            .all(|workspace| workspace.conversations.is_empty()));

        validate_and_save(&path, &document, &document, &AppState::default()).unwrap();
        let restored = read_document(&path).unwrap();
        assert_eq!(restored.workspaces.len(), document.workspaces.len());
        // Assert the deprecated `localPreset` field is absent from every serialized conversation.
        let mut seeded = document.clone();
        seeded.workspaces[0].conversations = default_document().workspaces[0].conversations.clone();
        let canonical = serde_json::to_value(&seeded).unwrap();
        let serialized = &canonical["workspaces"][0]["conversations"][0];
        assert!(serialized.get("localPreset").is_none());
    }

    /// The first launch keeps one shell in each shipped preset: the first of
    /// the machine's OS order that its probe found. Any other preset is the
    /// user's and is left alone.
    #[test]
    fn first_launch_keeps_the_machines_most_preferred_shell_in_shipped_presets() {
        use crate::shell_backend::{MachineOs, ShellBackend};
        let shells = |document: &AppDocument, preset_id: &str| -> Vec<String> {
            document
                .presets
                .conversation_presets
                .iter()
                .find(|preset| preset.id == preset_id)
                .unwrap()
                .settings
                .enabled_tools
                .iter()
                .filter(|name| ShellBackend::of_tool(name).is_some())
                .cloned()
                .collect()
        };
        let product = crate::catalog::product_default_document();
        let mut custom = product.presets.conversation_presets[0].clone();
        custom.id = "preset_custom".into();

        let mut mac = product.clone();
        mac.presets.conversation_presets.push(custom);
        let before = mac.presets.conversation_presets[0].settings.enabled_tools.len();
        apply_seeded_shell(&mut mac, MachineOs::Macos, &[ShellBackend::Sh, ShellBackend::Bash]);
        for preset_id in [
            crate::catalog::CODEX_PRESET_ID,
            crate::catalog::CLAUDE_CODE_PRESET_ID,
        ] {
            assert_eq!(shells(&mac, preset_id), vec!["bash"]);
        }
        let seeded = shells(&product, crate::catalog::CODEX_PRESET_ID);
        assert_eq!(seeded.len(), 4, "the seed lists every backend's command tool");
        assert_eq!(shells(&mac, "preset_custom"), seeded);
        // Only shell tools leave the list.
        assert_eq!(
            mac.presets.conversation_presets[0].settings.enabled_tools.len(),
            before - seeded.len() + 1
        );

        let mut windows = product.clone();
        apply_seeded_shell(
            &mut windows,
            MachineOs::Windows,
            &[ShellBackend::Bash, ShellBackend::PowerShell],
        );
        assert_eq!(shells(&windows, crate::catalog::CLAUDE_CODE_PRESET_ID), vec!["powershell"]);

        let mut linux = product.clone();
        apply_seeded_shell(&mut linux, MachineOs::Linux, &[ShellBackend::Zsh, ShellBackend::Sh]);
        assert_eq!(shells(&linux, crate::catalog::CLAUDE_CODE_PRESET_ID), vec!["zsh"]);

        // A probe that found nothing still leaves the OS's first shell.
        let mut bare = product;
        apply_seeded_shell(&mut bare, MachineOs::Macos, &[]);
        assert_eq!(shells(&bare, crate::catalog::CLAUDE_CODE_PRESET_ID), vec!["zsh"]);
    }

    /// Each shipped preset opens with a system prompt, and it is seeded as a
    /// template body in the conversation store rather than carried in the
    /// document — `ConversationPresetSettings` has no prompt field, and a
    /// template `System` row is what reaches the request's system half.
    ///
    /// Driven through `seed_preset_templates` rather than `load_or_initialize`,
    /// because `hydrate_test_settings` replaces the shipped presets in test
    /// builds; the product document is the input the real first launch uses.
    #[test]
    fn seeding_writes_one_system_prompt_behind_each_shipped_preset() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.v1.json");
        let document = crate::catalog::product_default_document();
        let store = crate::conversation_store::store_for(&path).unwrap();
        seed_preset_templates(&store, &document).unwrap();

        for preset_id in [
            crate::catalog::CODEX_PRESET_ID,
            crate::catalog::CLAUDE_CODE_PRESET_ID,
        ] {
            let template_id = crate::catalog::seeded_template_id(preset_id);
            assert!(!template_id.is_empty(), "{preset_id} 应当绑定出厂模板");
            let contexts = store.template_contexts(template_id).unwrap();
            assert_eq!(contexts.len(), 1, "{preset_id} 的模板应当只有一条系统提示");
            match &contexts[0] {
                ContextItem::System {
                    content, local_only, ..
                } => {
                    assert!(!content.trim().is_empty());
                    assert!(
                        !local_only,
                        "local_only 的系统行是生命周期诊断，永远不会上线"
                    );
                }
                other => panic!("{preset_id} 的模板正文不是系统行：{other:?}"),
            }
        }

        // The two prompts describe two different CLIs, so they must not be the
        // same text.
        let codex = store
            .template_contexts(crate::catalog::seeded_template_id(
                crate::catalog::CODEX_PRESET_ID,
            ))
            .unwrap();
        let claude = store
            .template_contexts(crate::catalog::seeded_template_id(
                crate::catalog::CLAUDE_CODE_PRESET_ID,
            ))
            .unwrap();
        assert_ne!(
            serde_json::to_value(&codex[0])
                .unwrap()
                .get("content")
                .cloned(),
            serde_json::to_value(&claude[0])
                .unwrap()
                .get("content")
                .cloned()
        );
    }

    /// Seeding runs again whenever the anchor is missing, including the recovery
    /// path over a live conversation store. `put_template` replaces a body
    /// wholesale, so the existence guard is the only thing standing between a
    /// rebuild and a body the user has since edited.
    #[test]
    fn re_seeding_keeps_a_template_body_the_user_edited() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.v1.json");
        let document = crate::catalog::product_default_document();
        let store = crate::conversation_store::store_for(&path).unwrap();
        seed_preset_templates(&store, &document).unwrap();

        let template_id = crate::catalog::seeded_template_id(crate::catalog::CLAUDE_CODE_PRESET_ID);
        store
            .put_template(
                template_id,
                "",
                &[ContextItem::System {
                    id: "ctx_edited".into(),
                    content: "用户改过的提示词".into(),
                    local_only: false,
                    hook_execution: None,
                    created_at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                }],
            )
            .unwrap();

        seed_preset_templates(&store, &document).unwrap();

        let contexts = store.template_contexts(template_id).unwrap();
        assert_eq!(contexts.len(), 1);
        match &contexts[0] {
            ContextItem::System { content, .. } => {
                assert_eq!(content, "用户改过的提示词", "重新播种不得洗掉用户的正文");
            }
            other => panic!("模板正文不是系统行：{other:?}"),
        }
    }

    /// The seeded capability ids reach both shipped presets and no other, and
    /// hooks stay unselected — a dangling hook id fails every run closed, and
    /// the built-in hooks exist to be deletable.
    #[test]
    fn seeded_capability_ids_select_skills_and_servers_but_never_hooks() {
        let mut document = crate::catalog::product_default_document();
        apply_builtin_capability_selection(
            &mut document,
            &crate::capability_seed::BuiltinCapabilitySelection {
                skill_ids: vec!["skill_user_demo_0000000a".into()],
                mcp_ids: vec!["mcp_user_demo_0000000b".into()],
            },
        );
        for preset in &document.presets.conversation_presets {
            assert_eq!(preset.settings.skill_ids, vec!["skill_user_demo_0000000a"]);
            assert_eq!(preset.settings.mcp_ids, vec!["mcp_user_demo_0000000b"]);
            assert!(preset.settings.hook_ids.is_empty());
        }
    }

    /// A platform with no home directory, or a seeding failure, leaves the
    /// selection empty. That must start with no built-ins selected rather than
    /// with ids nothing can resolve.
    #[test]
    fn an_empty_selection_leaves_the_presets_selecting_nothing() {
        let mut document = crate::catalog::product_default_document();
        apply_builtin_capability_selection(
            &mut document,
            &crate::capability_seed::BuiltinCapabilitySelection::default(),
        );
        for preset in &document.presets.conversation_presets {
            assert!(preset.settings.skill_ids.is_empty());
            assert!(preset.settings.mcp_ids.is_empty());
            assert!(preset.settings.hook_ids.is_empty());
        }
    }

    /// The two shipped presets survive the real save boundary with their role
    /// bindings intact — including the Codex ones, whose models cannot exist
    /// until the user signs in.
    #[test]
    fn product_default_document_ships_two_presets_whose_bindings_survive() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.seed-presets.json");
        let document = crate::catalog::product_default_document();

        let presets = &document.presets.conversation_presets;
        assert_eq!(
            presets
                .iter()
                .map(|preset| preset.id.as_str())
                .collect::<Vec<_>>(),
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
            claude_provider
                .models
                .iter()
                .all(|model| !model.id.contains("[1m]")),
            "1M 上下文的孪生行不入种子"
        );
        assert_eq!(
            claude_provider.active_model_id.as_deref(),
            claude_provider
                .models
                .first()
                .map(|model| model.id.as_str())
        );

        // Everything on except the names the host derives for itself. The
        // preview tools and `workflow` are on too.
        for preset in &restored.presets.conversation_presets {
            assert!(!preset.settings.allow_roleless_subagents);
            assert_eq!(preset.settings.agent_definitions.len(), 3);
            let withheld = |name: &str| {
                crate::mework_memory::is_memory_tool(name)
                    || crate::agents::is_task_runtime_tool_name(name)
                    || crate::plan_mode::is_plan_mode_tool_name(name)
                    || name == crate::capabilities::SKILL_TOOL
                    || name == crate::capabilities::TOOL_SEARCH_TOOL
            };
            let enabled = &preset.settings.enabled_tools;
            assert!(enabled.iter().all(|name| !withheld(name)));
            let withheld_count = restored
                .tools
                .iter()
                .filter(|tool| withheld(&tool.name))
                .count();
            assert_eq!(enabled.len(), restored.tools.len() - withheld_count);
            assert!(enabled.iter().any(|name| name == "preview_start"));
            assert!(enabled
                .iter()
                .any(|name| name == crate::workflow::WORKFLOW_TOOL));
            // The memory tools come from the two switches, which are on.
            assert!(preset.settings.global_memory_enabled);
            assert!(preset.settings.project_memory_enabled);
            // Every role follows the preset's list rather than keeping its own.
            assert!(preset
                .settings
                .agent_definitions
                .iter()
                .all(|role| role.tools.is_none()));

            // Both capability surfaces load on demand rather than inlining
            // every body and schema into the system prompt.
            assert!(preset.settings.skill_tool_enabled);
            assert!(preset.settings.mcp_tool_discovery_enabled);
            // A dangling hook id fails every run of the conversation closed,
            // and the built-in hooks are meant to be deletable, so no shipped
            // preset may name one.
            assert!(preset.settings.hook_ids.is_empty());
            // Both legs are native on both presets. Nothing is borrowed from a
            // catalog provider by default, and nothing resolves to a backend
            // chosen somewhere else: on a family that folds retrieval into its
            // one search tool, native fetch simply grants no second web tool,
            // which is that family's own shape.
            assert_eq!(
                preset.settings.web_search.provider,
                crate::model::SearchProviderSelection::Native
            );
            assert_eq!(
                preset.settings.web_search.fetch_provider,
                crate::model::FetchProviderSelection::Native
            );
            // A shipped domain list would be this application deciding what the
            // web may say, so the filter is off and both lists are empty.
            assert_eq!(
                preset.settings.web_search.domain_filter,
                crate::model::SearchDomainFilterMode::Off
            );
            assert!(preset.settings.web_search.include_domains.is_empty());
            assert!(preset.settings.web_search.exclude_domains.is_empty());
            // The body is seeded into the conversation store, not the document;
            // the id survives the save boundary either way.
            assert_eq!(
                preset.template_id,
                crate::catalog::seeded_template_id(&preset.id)
            );
        }

        let binding = |preset_id: &str, role: &str| {
            restored
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
        let mut document = default_document();
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
        let mut self_cycle = default_document();
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

        let mut two_group_cycle = default_document();
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
        let previous = default_document();
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
        let mut document = default_document();
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
        let mut document = default_document();
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
        let document_with_fork_tools = |names: &[&str]| {
            let mut document = default_document();
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
        let document_with_named_agent = |receipt: &str| {
            let mut document = default_document();
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
            document.workspaces[0].conversations[0]
                .contexts
                .push(spawned);
            document
        };
        assert!(validate_shape(&document_with_named_agent("")).is_ok());
        assert!(validate_shape(&document_with_named_agent(&"a".repeat(64))).is_ok());
        assert!(validate_shape(&document_with_named_agent("not-a-digest"))
            .unwrap_err()
            .contains("命名 Agent 配置回执格式无效"));

        let document_with_fork = |system_prompt_receipt: &str, binding_receipt: &str| {
            let mut document = default_document();
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
    fn tool_round_is_optional_for_legacy_data_and_round_trips_when_present() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state").join("document.json");
        let legacy_value = serde_json::to_value(default_document()).unwrap();
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

        let initialized = load_or_initialize(&path).unwrap();

        assert!(path.exists());
        assert_eq!(read_document(&path).unwrap(), initialized);
    }

    #[test]
    fn malformed_existing_document_is_preserved_and_reported() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.json");
        let original = b"{ definitely-not-json";
        fs::write(&path, original).unwrap();

        let error = load_or_initialize(&path).unwrap_err();

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
        let mut value = serde_json::to_value(default_document()).unwrap();
        value["schemaVersion"] = serde_json::json!(SCHEMA_VERSION + 1);
        let original = serde_json::to_vec_pretty(&value).unwrap();
        fs::write(&path, &original).unwrap();

        let error = load_or_initialize(&path).unwrap_err();

        assert!(error.contains("更新版本"));
        assert!(error.contains("原文件未修改"));
        assert_eq!(fs::read(&path).unwrap(), original);
    }

    #[test]
    fn outdated_schema_document_is_preserved_and_reported() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.json");
        let mut value = serde_json::to_value(default_document()).unwrap();
        // Test an explicitly old schema version. Zero is also what a document
        // with no `schemaVersion` at all reads as, so this covers both.
        value["schemaVersion"] = serde_json::json!(0);
        let original = serde_json::to_vec_pretty(&value).unwrap();
        fs::write(&path, &original).unwrap();

        let error = load_or_initialize(&path).unwrap_err();

        assert!(error.contains("旧版 schema"));
        assert!(error.contains("原文件未修改"));
        assert_eq!(fs::read(&path).unwrap(), original);
    }

    /// The accepted older anchor shapes preserve configuration: schema 1 gains
    /// only the host-owned fork-intent table, schema 2 only the model
    /// `promptCache` key with its default.
    #[test]
    fn released_older_schemas_preserve_configuration_in_place() {
        assert_eq!(SCHEMA_VERSION, 3, "抬版本时重新判断要不要就地迁移");
        for older in [1, 2] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("document.json");
            let mut document = default_document();
            for preset in &mut document.presets.conversation_presets {
                preset.settings.web_search.max_results = 42;
            }
            let mut value = serde_json::to_value(&document).unwrap();
            value["schemaVersion"] = serde_json::json!(older);
            let original = serde_json::to_vec_pretty(&value).unwrap();
            fs::write(&path, &original).unwrap();

            let restored = load_or_initialize(&path).unwrap();
            assert_eq!(restored.schema_version, 3, "schema {older}");
            for preset in &restored.presets.conversation_presets {
                assert_eq!(preset.settings.web_search.max_results, 42, "schema {older}");
            }
            assert_eq!(fs::read(&path).unwrap(), original, "schema {older}");
        }
    }

    /// A rejected conversation restores its full prior workspace membership.
    #[test]
    fn rejected_moved_conversation_returns_to_its_original_workspace() {
        let previous = default_document();
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
            files: Vec::new(),
            created_at: "2099-08-24T00:00:00.000Z".into(),
        });
        changed.workspaces[target_index].conversations.push(moved);

        let prepared = prepare_save_transition(&previous, &changed, &AppState::default()).unwrap();

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
        let mut document = default_document();
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
        let document = default_document();
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
        let document = default_document();
        save_all(&path, &document).unwrap();

        let store = crate::conversation_store::store_for(&path).unwrap();
        let mut orphan = document.workspaces[0].conversations[0].clone();
        orphan.id = "conv_orphan".into();
        store
            .put_conversation("workspace-that-no-longer-exists", &orphan)
            .unwrap();

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
        let document = default_document();
        let welcome_id = document.workspaces[0].conversations[0].id.clone();
        save_all(&path, &document).unwrap();
        fs::write(&path, b"{ definitely broken").unwrap();

        let recovered = load_or_recover(&path).unwrap();

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
            .any(|context| {
                matches!(
                    context,
                    ContextItem::System { content, .. } if content.contains("已封存")
                )
            });
        assert!(has_notice);
    }

    #[test]
    fn current_schema_empty_provider_catalog_is_not_reseeded() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.json");
        let mut document = default_document();
        document.assets.api_providers.clear();
        document.global_settings.active_provider_id = None;
        save_all(&path, &document).unwrap();

        let restored = read_document(&path).unwrap();

        assert!(restored.assets.api_providers.is_empty());
        assert!(restored.global_settings.active_provider_id.is_none());
    }

    #[test]
    fn workspace_kinds_require_valid_directory_and_temporary_shapes() {
        let document = default_document();
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
            machine: None,
            additional_workspaces: Vec::new(),
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
        let document = default_document();
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
        let previous = default_document();
        let mut changed = previous.clone();
        changed.assets.api_providers[0].name = "   ".into();
        changed.assets.api_providers[0].base_url = "not a URL yet".into();

        assert!(validate_shape(&changed).is_ok());
        validate_and_save(&path, &previous, &changed, &AppState::default()).unwrap();

        let restored = read_document(&path).unwrap();
        assert_eq!(restored.assets.api_providers[0].name, "   ");
        assert_eq!(restored.assets.api_providers[0].base_url, "not a URL yet");
    }

    /// Endpoint overrides obey the same URL safety rules as the chat base URL.
    #[test]
    fn endpoint_base_url_overrides_go_through_the_same_url_rules() {
        let mut document = default_document();
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

        document.assets.api_providers[0].endpoint_base_urls.insert(
            EndpointType::OpenaiTextToSpeech,
            "http://images.example.com".to_owned(),
        );
        let error = validate_shape(&document).unwrap_err();
        assert!(
            error.contains("openai_text_to_speech") && error.contains("端点地址无效"),
            "报错要点名是哪个端点，否则四个输入框里看不出改哪个：{error}"
        );
    }

    #[test]
    fn provider_and_model_ids_are_trimmed_for_uniqueness() {
        let mut duplicate_provider = default_document();
        let existing_provider_id = duplicate_provider.assets.api_providers[0].id.clone();
        duplicate_provider.assets.api_providers[1].id = format!("  {existing_provider_id}  ");
        let provider_error = validate_shape(&duplicate_provider).unwrap_err();
        assert!(provider_error.contains("API 提供商 ID 重复"));

        let mut duplicate_model = default_document();
        duplicate_model.assets.api_providers[0].models =
            vec![test_model("same-model"), test_model("  same-model  ")];
        let model_error = validate_shape(&duplicate_model).unwrap_err();
        assert!(model_error.contains("模型 ID 重复"));

        let mut blank_provider = default_document();
        blank_provider.assets.api_providers[0].id = " \t ".into();
        assert!(validate_shape(&blank_provider)
            .unwrap_err()
            .contains("API 提供商 ID 不能为空"));

        let mut blank_model = default_document();
        blank_model.assets.api_providers[0].models = vec![test_model(" \t ")];
        assert!(validate_shape(&blank_model)
            .unwrap_err()
            .contains("模型 ID 不能为空"));
    }

    #[test]
    fn chat_model_ids_share_the_exact_memory_owner_byte_contract() {
        let mut document = default_document();
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
        assert!(validate_shape(&document).unwrap_err().contains("控制字符"));
    }

    #[test]
    fn named_agent_definitions_are_bounded_and_reject_unknown_source_or_memory() {
        let mut oversized = default_document();
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
        let mut previous = default_document();
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
        let mut previous = default_document();
        let mut definition = test_agent_definition("reviewer");
        definition.revision = 9;
        definition.memory_epoch = 4;
        definition.memory = AgentDefinitionMemory::User;
        set_document_agent_definitions(&mut previous, vec![definition.clone()]);

        let mut deletion_draft = previous.clone();
        set_document_agent_definitions(&mut deletion_draft, Vec::new());
        let deleted =
            validate_save_transition(&previous, &deletion_draft, &state).expect("delete draft");
        let tombstone = &deleted.workspaces[0].conversations[0]
            .settings
            .agent_definitions[0];
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
        let recreated = &recreated_document.workspaces[0].conversations[0]
            .settings
            .agent_definitions[0];
        assert!(!recreated.deleted);
        assert_eq!(recreated.revision, 11);
        assert_eq!(recreated.memory_epoch, 5);

        let mut forged_draft = recreated_document.clone();
        forged_draft.workspaces[0].conversations[0]
            .settings
            .agent_definitions[0]
            .revision = 999;
        forged_draft.workspaces[0].conversations[0]
            .settings
            .agent_definitions[0]
            .memory_epoch = 999;
        let canonical = validate_save_transition(&recreated_document, &forged_draft, &state)
            .expect("host overwrites renderer revision and epoch");
        assert_eq!(
            canonical.workspaces[0].conversations[0]
                .settings
                .agent_definitions[0]
                .revision,
            11
        );
        assert_eq!(
            canonical.workspaces[0].conversations[0]
                .settings
                .agent_definitions[0]
                .memory_epoch,
            5
        );
    }

    /// Rewording a description preserves its revision because it does not change capabilities.
    #[test]
    fn rewording_a_role_persists_the_text_without_moving_its_revision() {
        let state = AppState::default();
        let mut previous = default_document();
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
        let role = &saved.workspaces[0].conversations[0]
            .settings
            .agent_definitions[0];
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
            rebound.workspaces[0].conversations[0]
                .settings
                .agent_definitions[0]
                .revision,
            10
        );
    }

    /// A tombstone is host bookkeeping the renderer only mirrors: it rides back
    /// in every later settings write, because only the roles page filters
    /// tombstones out of what it sends. Echoing one must therefore stay
    /// writable — refusing it would brick every later edit of that conversation
    /// while the roles page went on working.
    #[test]
    fn echoing_a_committed_tombstone_keeps_the_conversation_writable() {
        let mut active = test_agent_definition("reviewer");
        active.revision = 3;
        active.memory_epoch = 2;
        let committed =
            canonicalize_renderer_agent_definition_list(std::slice::from_ref(&active), Vec::new())
                .expect("omitting a role mints its tombstone");
        assert_eq!(committed.len(), 1);
        assert!(committed[0].deleted);

        // What the settings pane sends on the next edit of any other field: the
        // body the host just handed back, tombstone included.
        let echoed = canonicalize_renderer_agent_definition_list(&committed, committed.clone())
            .expect("a settings edit after a role deletion must still be accepted");
        assert_eq!(
            echoed, committed,
            "the committed tombstone is carried forward verbatim"
        );

        // A tombstone the host never issued stays refused: it would hide an
        // identity and force its memory epoch forward.
        let mut invented = test_agent_definition("smuggled");
        invented.deleted = true;
        invented.enabled = false;
        let error =
            canonicalize_renderer_agent_definition_list(&[], vec![invented]).expect_err("invented");
        assert!(error.contains("删除墓碑"), "{error}");

        // So does one aimed at an identity that is still live: the renderer
        // deletes a role by leaving it out, never by marking it deleted.
        let mut forged = active.clone();
        forged.deleted = true;
        forged.enabled = false;
        let error = canonicalize_renderer_agent_definition_list(
            std::slice::from_ref(&active),
            vec![forged],
        )
        .expect_err("forged deletion of a live role");
        assert!(error.contains("删除墓碑"), "{error}");
    }

    /// The two shaping numbers are result shaping rather than capability
    /// selection, so a proposal past the ceiling is pulled back onto it rather
    /// than failed — the run applies the clamped answer, and the next save
    /// persists it.
    #[test]
    fn a_role_shaping_past_the_ceiling_is_clamped_rather_than_rejected() {
        let mut document = default_document();
        let mut definition = test_agent_definition("reviewer");
        definition.max_results = MAX_SEARCH_MAX_RESULTS + 7;
        definition.compression_cutoff = MAX_SEARCH_CUTOFF_LIMIT * 2;
        set_document_agent_definitions(&mut document, vec![definition]);

        let canonicalized = prepare_save_transition(&document, &document, &AppState::default())
            .expect("shaping above the ceiling is clamped, not refused");
        let clamped = &canonicalized.document.workspaces[0].conversations[0]
            .settings
            .agent_definitions[0];
        assert_eq!(clamped.max_results, MAX_SEARCH_MAX_RESULTS);
        assert_eq!(clamped.compression_cutoff, MAX_SEARCH_CUTOFF_LIMIT);
    }

    /// Tombstones retain identity only and discard deleted-role descriptions.
    #[test]
    fn deleting_a_role_leaves_no_prose_behind_in_its_tombstone() {
        let state = AppState::default();
        let mut previous = default_document();
        let mut definition = test_agent_definition("reviewer");
        definition.description = "会被一起带走的说明".into();
        set_document_agent_definitions(&mut previous, vec![definition]);

        let mut deletion_draft = previous.clone();
        set_document_agent_definitions(&mut deletion_draft, Vec::new());
        let deleted =
            validate_save_transition(&previous, &deletion_draft, &state).expect("delete draft");
        let tombstone = &deleted.workspaces[0].conversations[0]
            .settings
            .agent_definitions[0];
        assert!(tombstone.deleted);
        assert_eq!(tombstone.description, "");
    }

    /// Descriptions are unbounded except that NUL is prohibited because downstream consumers treat it as binary.
    #[test]
    fn a_role_description_is_unbounded_in_length_but_never_carries_a_nul() {
        let mut long = default_document();
        let mut definition = test_agent_definition("reviewer");
        definition.description = "很长的一段说明。\n".repeat(20_000);
        set_document_agent_definitions(&mut long, vec![definition.clone()]);
        assert!(validate_shape(&long).is_ok(), "说明不设长度上限");

        let mut with_nul = default_document();
        definition.description = "说明\u{0}夹了一个 NUL".into();
        set_document_agent_definitions(&mut with_nul, vec![definition]);
        assert!(validate_shape(&with_nul)
            .unwrap_err()
            .contains("说明不能包含 NUL 字符"));
    }

    #[test]
    fn renderer_cannot_elevate_or_mutate_host_agent_definition_sources() {
        let state = AppState::default();
        let previous = default_document();
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
        let previous = default_document();
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
        let mut document = default_document();
        let provider = &mut document.assets.api_providers[0];
        provider.models = vec![test_model("available")];
        provider.active_model_id = Some("missing".into());
        assert!(validate_shape(&document).is_err());

        document.assets.api_providers[0].active_model_id = Some("available".into());
        assert!(validate_shape(&document).is_ok());
    }

    #[test]
    fn tool_results_require_an_execution_receipt() {
        let previous = default_document();
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
        assert!(!validate_tool_results(&previous, &changed, &state)
            .unwrap()
            .refused());
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
        let previous = default_document();
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
            *attestation =
                executing.attest_tool_context(&crate::tool_attestation::AttestationSubject {
                    conversation_id: &conversation_id,
                    context_id: id,
                    tool_name,
                    input,
                    requested_input: requested_input.as_ref(),
                    result,
                    subagent: subagent.as_ref(),
                });
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
        let previous = default_document();
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
        let document = default_document();
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
        let document = default_document();
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
        let previous = default_document();
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

        assert!(
            validate_tool_results(&previous, &stale_success_document, &state)
                .unwrap()
                .refused()
        );

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
        assert!(!validate_tool_results(&previous, &final_document, &state)
            .unwrap()
            .refused());
    }

    /// A conversation in a worktree of its first workspace runs its tools there,
    /// and the receipt of a card run by hand is recorded there too; the card
    /// has to save.
    #[test]
    fn a_card_run_in_the_conversations_worktree_is_attested_by_its_receipt() {
        let root = tempfile::tempdir().unwrap();
        let worktree = root.path().join(".mework/worktrees/conversations/a1");
        fs::create_dir_all(&worktree).unwrap();
        let mut previous = default_document();
        previous.workspaces[0].kind = WorkspaceKind::Directory;
        previous.workspaces[0].path = root.path().to_string_lossy().into_owned();
        previous.workspaces[0].conversations[0].worktrees = vec![crate::model::ConversationWorktree {
            path: worktree.to_string_lossy().into_owned(),
            branch: "mework/conv/a1".into(),
            base_oid: "abc1234".into(),
            base_branch: None,
            workspace: None,
        }];
        let mut changed = previous.clone();
        let mut card = changed.workspaces[0].conversations[0].contexts[3].clone();
        let ContextItem::Tool {
            id,
            tool_name,
            input,
            result,
            ..
        } = &mut card
        else {
            panic!("seed context must be a tool call");
        };
        *id = "fresh-card-in-worktree".into();
        let request = ToolExecutionRequest {
            conversation_id: changed.workspaces[0].conversations[0].id.clone(),
            // `execute_tool` runs the call in the conversation's effective directory.
            workspace_path: worktree.to_string_lossy().into_owned(),
            tool_name: tool_name.clone(),
            input: input.clone(),
        };
        let result = result.clone();
        changed.workspaces[0].conversations[0].contexts.push(card);
        let state = AppState::default();
        assert!(validate_tool_results(&previous, &changed, &state)
            .unwrap()
            .refused());
        state.record_receipt(&request, &result);
        assert!(!validate_tool_results(&previous, &changed, &state)
            .unwrap()
            .refused());
    }

    /// A card run by hand in a conversation whose first workspace is on another
    /// machine runs from a host-side anchor directory, while the save knows
    /// only the registered remote root. A receipt under the anchor alone gets
    /// the card quarantined on its first save; `execute_tool` records it under
    /// `tool_receipt_roots` as well.
    #[test]
    fn a_card_run_by_hand_in_a_remote_workspace_is_attested_under_its_root() {
        let anchor = tempfile::tempdir().unwrap();
        let mut previous = default_document();
        previous.workspaces[0].kind = WorkspaceKind::Directory;
        previous.workspaces[0].path = "/srv/app".into();
        previous.workspaces[0].machine = Some(crate::model::RunTarget::Ssh {
            machine_id: "m1".into(),
        });
        let mut changed = previous.clone();
        let mut card = changed.workspaces[0].conversations[0].contexts[3].clone();
        let ContextItem::Tool {
            id,
            tool_name,
            input,
            result,
            ..
        } = &mut card
        else {
            panic!("seed context must be a tool call");
        };
        *id = "fresh-card-in-remote-workspace".into();
        let ran = ToolExecutionRequest {
            conversation_id: changed.workspaces[0].conversations[0].id.clone(),
            workspace_path: anchor.path().to_string_lossy().into_owned(),
            tool_name: tool_name.clone(),
            input: input.clone(),
        };
        let result = result.clone();
        changed.workspaces[0].conversations[0].contexts.push(card);

        let state = AppState::default();
        state.record_receipt(&ran, &result);
        assert_eq!(
            validate_tool_results(&previous, &changed, &state)
                .unwrap()
                .refused_context_ids(),
            ["fresh-card-in-remote-workspace"]
        );

        let roots = tool_receipt_roots(&changed.workspaces[0], &changed.workspaces[0].conversations[0]);
        assert_eq!(roots, ["/srv/app"]);
        let recorded = ToolExecutionRequest {
            workspace_path: roots[0].to_owned(),
            ..ran
        };
        state.record_receipt(&recorded, &result);
        assert!(!validate_tool_results(&previous, &changed, &state)
            .unwrap()
            .refused());
    }

    #[test]
    fn model_requested_tool_input_requires_an_exact_context_receipt() {
        let previous = default_document();
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
        assert!(validate_tool_results(&previous, &changed, &state)
            .unwrap()
            .refused());

        state.record_context_receipt(&request, &result, Some(&requested_input));
        assert!(!validate_tool_results(&previous, &changed, &state)
            .unwrap()
            .refused());

        let mut fresh = previous.clone();
        let mut fresh_context = changed.workspaces[0].conversations[0].contexts[3].clone();
        let ContextItem::Tool { id, .. } = &mut fresh_context else {
            unreachable!()
        };
        *id = "fresh-tool-with-requested-input".into();
        fresh.workspaces[0].conversations[0]
            .contexts
            .push(fresh_context);
        assert!(!validate_tool_results(&previous, &fresh, &state)
            .unwrap()
            .refused());

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
        assert!(
            validate_tool_results(&previous, &dropped_before_first_save, &state)
                .unwrap()
                .refused()
        );

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
        assert!(
            validate_tool_results(&previous, &forged_before_first_save, &state)
                .unwrap()
                .refused()
        );

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
        assert!(validate_tool_results(&changed, &mutated_after_save, &state)
            .unwrap()
            .refused());
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

        let mut previous = default_document();
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
        assert!(!validate_tool_results(&previous, &subset, &state)
            .unwrap()
            .refused());

        let mut empty = previous.clone();
        let ContextItem::Tool { result, .. } =
            &mut empty.workspaces[0].conversations[0].contexts[3]
        else {
            unreachable!()
        };
        result.images.clear();
        assert!(!validate_tool_results(&previous, &empty, &state)
            .unwrap()
            .refused());

        let mut added = previous.clone();
        let ContextItem::Tool { result, .. } =
            &mut added.workspaces[0].conversations[0].contexts[3]
        else {
            unreachable!()
        };
        result.images.push(image('d', "added.png", 80));
        assert!(validate_tool_results(&previous, &added, &state)
            .unwrap()
            .refused());

        let mut changed_metadata = previous.clone();
        let ContextItem::Tool { result, .. } =
            &mut changed_metadata.workspaces[0].conversations[0].contexts[3]
        else {
            unreachable!()
        };
        result.images = vec![first.clone(), third.clone()];
        result.images[0].name = "renamed.png".into();
        assert!(validate_tool_results(&previous, &changed_metadata, &state)
            .unwrap()
            .refused());

        let mut reordered_while_removing = previous.clone();
        let ContextItem::Tool { result, .. } =
            &mut reordered_while_removing.workspaces[0].conversations[0].contexts[3]
        else {
            unreachable!()
        };
        result.images = vec![third.clone(), first.clone()];
        assert!(
            validate_tool_results(&previous, &reordered_while_removing, &state)
                .unwrap()
                .refused()
        );

        let mut changed_output = subset.clone();
        let ContextItem::Tool { result, .. } =
            &mut changed_output.workspaces[0].conversations[0].contexts[3]
        else {
            unreachable!()
        };
        result.output.push_str(" tampered");
        assert!(validate_tool_results(&previous, &changed_output, &state)
            .unwrap()
            .refused());

        let mut changed_input = subset.clone();
        let ContextItem::Tool { input, .. } =
            &mut changed_input.workspaces[0].conversations[0].contexts[3]
        else {
            unreachable!()
        };
        input.insert("unexpected".into(), serde_json::json!(true));
        assert!(validate_tool_results(&previous, &changed_input, &state)
            .unwrap()
            .refused());

        let mut moved = previous.clone();
        let mut destination = moved.workspaces[0].conversations[0].clone();
        destination.id = "image-removal-destination".into();
        destination.contexts = vec![moved.workspaces[0].conversations[0].contexts.remove(3)];
        let ContextItem::Tool { result, .. } = &mut destination.contexts[0] else {
            unreachable!()
        };
        result.images = vec![second];
        moved.workspaces[0].conversations.push(destination);
        assert!(validate_tool_results(&previous, &moved, &state)
            .unwrap()
            .refused());
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
        let previous = default_document();
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
        assert!(!validate_tool_results(&previous, &next, &state)
            .unwrap()
            .refused());

        let mut forged = next.clone();
        let ContextItem::Tool { result, .. } = forged.workspaces[0].conversations[0]
            .contexts
            .last_mut()
            .unwrap()
        else {
            unreachable!()
        };
        result.images[0].name = "forged.png".into();
        assert!(validate_tool_results(&previous, &forged, &state)
            .unwrap()
            .refused());

        let mut changed_output = next;
        let ContextItem::Tool { result, .. } = changed_output.workspaces[0].conversations[0]
            .contexts
            .last_mut()
            .unwrap()
        else {
            unreachable!()
        };
        result.output.push_str(" forged");
        assert!(validate_tool_results(&previous, &changed_output, &state)
            .unwrap()
            .refused());
    }

    #[test]
    fn ask_user_prompt_edit_preserves_result_without_receipt_and_question_count() {
        let mut previous = default_document();
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
        assert!(!validate_tool_results(&previous, &changed, &state)
            .unwrap()
            .refused());

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
        assert!(validate_tool_results(&previous, &count_changed, &state)
            .unwrap()
            .refused());

        let mut tampered_result = changed;
        let ContextItem::Tool { result, .. } = tampered_result.workspaces[0].conversations[0]
            .contexts
            .last_mut()
            .unwrap()
        else {
            unreachable!()
        };
        result.duration_ms = 1;
        assert!(validate_tool_results(&previous, &tampered_result, &state)
            .unwrap()
            .refused());

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
        assert!(validate_tool_results(&previous, &legacy_shaped, &state)
            .unwrap()
            .refused());
    }

    #[test]
    fn subagent_receipt_commits_the_exact_recursive_record_and_stays_immutable() {
        let previous = default_document();
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
            !validate_tool_results(&previous, &changed, &exact_outer)
                .unwrap()
                .refused(),
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
            validate_tool_results(&previous, &stripped_before_first_save, &exact_outer)
                .unwrap()
                .refused(),
            "the exact sidecar receipt must revoke the weaker ordinary receipt"
        );

        let ordinary_outer = AppState::default();
        ordinary_outer.record_receipt(&outer_request, &outer_result);
        ordinary_outer.record_receipt(&nested_request, &nested_result);
        assert!(
            validate_tool_results(&previous, &changed, &ordinary_outer)
                .unwrap()
                .refused(),
            "an ordinary result receipt must not attest a subagent audit snapshot"
        );

        // Once persisted, the exact outer record and all recursively nested
        // tool results survive a later save without relying on process-local
        // receipts.
        let no_live_receipts = AppState::default();
        assert!(
            !validate_tool_results(&changed, &changed.clone(), &no_live_receipts)
                .unwrap()
                .refused()
        );

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
        assert!(
            validate_tool_results(&changed, &tampered_task, &no_live_receipts)
                .unwrap()
                .refused()
        );

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
        assert!(
            validate_tool_results(&changed, &tampered_kind, &no_live_receipts)
                .unwrap()
                .refused()
        );

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
        assert!(
            validate_tool_results(&changed, &tampered_contexts, &no_live_receipts)
                .unwrap()
                .refused()
        );

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
        let validation = validate_tool_results(&changed, &stripped, &no_live_receipts).unwrap();
        assert_eq!(validation.refused_context_ids(), ["research-run"]);
    }

    #[test]
    fn tool_context_and_receipt_cannot_move_between_conversations() {
        let previous = default_document();
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

        assert!(validate_tool_results(&previous, &changed, &state)
            .unwrap()
            .refused());
    }

    #[test]
    fn new_workspace_path_requires_backend_authorization() {
        let directory = tempfile::tempdir().unwrap();
        let selected = directory.path().join("selected-workspace");
        fs::create_dir(&selected).unwrap();
        let previous = default_document();
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

    #[test]
    fn a_new_extra_working_directory_requires_backend_authorization() {
        let directory = tempfile::tempdir().unwrap();
        let picked = directory.path().join("shared-library");
        fs::create_dir(&picked).unwrap();
        let previous = default_document();
        let mut changed = previous.clone();
        changed.workspaces[0].conversations[0].additional_directories =
            vec![picked.to_string_lossy().into_owned()];

        let state = AppState::default();
        assert!(validate_workspace_authorizations(&previous, &changed, &state).is_err());
        state.authorize_workspace(&picked).unwrap();
        assert!(validate_workspace_authorizations(&previous, &changed, &state).is_ok());

        // Once held, the same directory survives a save in a session that never
        // opened the picker — reloading must not revoke a grant.
        let fresh = AppState::default();
        assert!(validate_workspace_authorizations(&changed, &changed, &fresh).is_ok());
        // But another conversation cannot borrow it.
        let mut borrowed = changed.clone();
        let mut sibling = borrowed.workspaces[0].conversations[0].clone();
        sibling.id = "conv_sibling".into();
        borrowed.workspaces[0].conversations.push(sibling);
        assert!(validate_workspace_authorizations(&changed, &borrowed, &fresh).is_err());
    }

    /// A worktree record is where a conversation's tools run, so only one the
    /// host made — local or on another machine — may be saved, besides one the
    /// conversation already held.
    #[test]
    fn only_worktrees_the_host_made_may_be_saved() {
        let directory = tempfile::tempdir().unwrap();
        let made = directory.path().join("made");
        fs::create_dir(&made).unwrap();
        let forged = directory.path().join("forged");
        fs::create_dir(&forged).unwrap();
        let state = AppState::default();
        state.authorize_workspace(&made).unwrap();
        state.authorize_remote_workspace("ssh:m1", "C:/repo/.mework/worktrees/conversations/a1");
        let record = |path: &str, machine: Option<crate::model::RunTarget>| {
            crate::model::ConversationWorktree {
                path: path.into(),
                branch: "mework/conv/a1".into(),
                base_oid: "abc1234".into(),
                base_branch: Some("main".into()),
                workspace: Some(crate::model::AttachedWorkspace {
                    machine,
                    path: "/repo".into(),
                }),
            }
        };
        let ssh = || Some(crate::model::RunTarget::Ssh { machine_id: "m1".into() });
        let mut conversation = default_document().workspaces[0].conversations[0].clone();
        conversation.worktrees = vec![
            record(&made.to_string_lossy(), None),
            record("C:/repo/.mework/worktrees/conversations/a1", ssh()),
        ];
        validate_worktree_records(&conversation, None, &state).unwrap();

        let mut local_forgery = conversation.clone();
        local_forgery.worktrees = vec![record(&forged.to_string_lossy(), None)];
        assert!(validate_worktree_records(&local_forgery, None, &state).is_err());
        let mut remote_forgery = conversation.clone();
        remote_forgery.worktrees = vec![record("C:/Windows", ssh())];
        assert!(validate_worktree_records(&remote_forgery, None, &state).is_err());
        // A record the conversation already held survives a fresh process.
        let fresh = AppState::default();
        validate_worktree_records(&local_forgery, Some(&local_forgery), &fresh).unwrap();
    }

    #[test]
    fn attached_workspaces_are_bounded_and_survive_an_unchanged_save() {
        let directory = tempfile::tempdir().unwrap();
        let picked = directory.path().join("shared-library");
        fs::create_dir(&picked).unwrap();
        let state = AppState::default();
        state.authorize_workspace(&picked).unwrap();

        let mut conversation = default_document().workspaces[0].conversations[0].clone();
        conversation.attached_workspaces = vec![
            crate::model::AttachedWorkspace {
                machine: None,
                path: picked.to_string_lossy().into_owned(),
            };
            MAX_ADDITIONAL_DIRECTORIES + 1
        ];
        let error = validate_additional_directories(&conversation, None, &state)
            .expect_err("an unbounded list must be refused");
        assert!(error.contains("工作区超过"), "{error}");

        conversation.attached_workspaces.truncate(1);
        assert!(validate_additional_directories(&conversation, None, &state).is_ok());
    }

    /// A directory on another machine has no canonical form this host can
    /// compute, so its grant is the machine plus the exact text the remote
    /// browser returned — and nothing else may stand in for it.
    #[test]
    fn a_remote_workspace_needs_a_grant_from_the_remote_browser() {
        let state = AppState::default();
        let machine = crate::model::RunTarget::Ssh {
            machine_id: "m1".into(),
        };
        let mut conversation = default_document().workspaces[0].conversations[0].clone();
        conversation.attached_workspaces = vec![crate::model::AttachedWorkspace {
            machine: Some(machine.clone()),
            path: "/srv/app".into(),
        }];

        assert!(validate_additional_directories(&conversation, None, &state).is_err());
        // A grant for the same path on this machine is not a grant for that one.
        state.authorize_remote_workspace("local", "/srv/app");
        assert!(validate_additional_directories(&conversation, None, &state).is_err());
        state.authorize_remote_workspace("ssh:m1", "/srv/app");
        assert!(validate_additional_directories(&conversation, None, &state).is_ok());

        // Once held it survives a session that never opened the browser.
        let fresh = AppState::default();
        assert!(
            validate_additional_directories(&conversation, Some(&conversation), &fresh).is_ok()
        );
    }

    /// The workspace library follows the same rule as a conversation's attached
    /// list: a top-level workspace on another machine is authorized by the
    /// remote browser's grant, keyed by machine and exact path, and neither a
    /// local grant for the same spelling nor a previous workspace at that path
    /// on another machine stands in for it.
    #[test]
    fn a_remote_primary_workspace_needs_the_remote_grant() {
        let previous = default_document();
        let mut changed = previous.clone();
        let mut workspace = changed.workspaces[0].clone();
        workspace.id = "ws_remote".into();
        workspace.path = "/srv/app".into();
        workspace.machine = Some(crate::model::RunTarget::Ssh {
            machine_id: "m1".into(),
        });
        workspace.conversations.clear();
        changed.workspaces.push(workspace);

        let state = AppState::default();
        assert!(validate_workspace_authorizations(&previous, &changed, &state).is_err());
        state.authorize_remote_workspace("wsl:Ubuntu", "/srv/app");
        assert!(validate_workspace_authorizations(&previous, &changed, &state).is_err());
        state.authorize_remote_workspace("ssh:m1", "/srv/app");
        assert!(validate_workspace_authorizations(&previous, &changed, &state).is_ok());

        // A workspace the previous document already held keeps its standing
        // without any grant in this session — by machine and path together.
        let fresh = AppState::default();
        assert!(validate_workspace_authorizations(&changed, &changed, &fresh).is_ok());
        let mut relocated = changed.clone();
        relocated.workspaces.last_mut().unwrap().machine = Some(crate::model::RunTarget::Wsl {
            distro: "Ubuntu".into(),
        });
        assert!(validate_workspace_authorizations(&changed, &relocated, &fresh).is_err());
    }

    /// A load path that rebuilds a remote workspace without its machine has to
    /// be told apart from a user picking a new local directory. The local check
    /// used to answer "Workspace path must be absolute" — true of a POSIX path
    /// on Windows and silent about where the field went.
    #[test]
    fn a_remote_workspace_that_lost_its_machine_is_named_as_such() {
        let mut previous = default_document();
        let mut workspace = previous.workspaces[0].clone();
        workspace.id = "ws_remote".into();
        workspace.path = "/home/dev/app".into();
        workspace.machine = Some(crate::model::RunTarget::Ssh {
            machine_id: "m1".into(),
        });
        workspace.conversations.clear();
        previous.workspaces.push(workspace);

        let mut changed = previous.clone();
        changed.workspaces.last_mut().unwrap().machine = None;

        let error = validate_workspace_authorizations(&previous, &changed, &AppState::default())
            .expect_err("a remote path with no machine is not a local workspace");
        assert!(error.contains("ws_remote"), "{error}");
        assert!(error.contains("ssh:m1"), "{error}");
        assert!(error.contains("machine 字段"), "{error}");

        // The same rule for a conversation's attached list.
        let mut held = default_document().workspaces[0].conversations[0].clone();
        held.attached_workspaces = vec![crate::model::AttachedWorkspace {
            machine: Some(crate::model::RunTarget::Ssh {
                machine_id: "m1".into(),
            }),
            path: "/srv/app".into(),
        }];
        let mut proposed = held.clone();
        proposed.attached_workspaces[0].machine = None;
        let error = validate_additional_directories(&proposed, Some(&held), &AppState::default())
            .expect_err("a remote path with no machine is not a local workspace");
        assert!(error.contains("/srv/app"), "{error}");
        assert!(error.contains("ssh:m1"), "{error}");
    }

    fn host_member(path: &Path) -> crate::model::AttachedWorkspace {
        crate::model::AttachedWorkspace {
            machine: None,
            path: path.to_string_lossy().into_owned(),
        }
    }

    fn ssh_member(path: &str) -> crate::model::AttachedWorkspace {
        crate::model::AttachedWorkspace {
            machine: Some(crate::model::RunTarget::Ssh {
                machine_id: "m1".into(),
            }),
            path: path.into(),
        }
    }

    /// The anchor rebuilds each project field by field in both directions, so
    /// a field it forgets is dropped on the next save without any error.
    #[test]
    fn project_members_survive_the_anchor_round_trip() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state").join("document.v1.json");
        let mut document = default_document();
        document.workspaces[0].additional_workspaces =
            vec![host_member(directory.path()), ssh_member("~/services")];
        let store = crate::conversation_store::store_for(&path).unwrap();
        seed_conversations(&store, &document).unwrap();
        save_all(&path, &document).unwrap();

        let loaded = read_document(&path).unwrap();
        assert_eq!(
            loaded.workspaces[0].additional_workspaces,
            document.workspaces[0].additional_workspaces
        );
        assert_eq!(loaded, document);

        // An anchor written before projects had members reads as none, and a
        // project without members writes no key at all.
        let anchor = fs::read_to_string(&path).unwrap();
        assert_eq!(
            anchor.matches("additionalWorkspaces").count(),
            1,
            "{anchor}"
        );
    }

    #[test]
    fn a_new_project_member_requires_a_picker_grant() {
        let directory = tempfile::tempdir().unwrap();
        let picked = directory.path().join("shared-library");
        fs::create_dir(&picked).unwrap();
        let previous = default_document();
        let mut changed = previous.clone();
        changed.workspaces[0].additional_workspaces = vec![host_member(&picked)];

        let state = AppState::default();
        let error = validate_workspace_authorizations(&previous, &changed, &state)
            .expect_err("an unpicked directory must not join a project");
        assert!(error.contains("工作区 2"), "{error}");
        state.authorize_workspace(&picked).unwrap();
        assert!(validate_workspace_authorizations(&previous, &changed, &state).is_ok());

        // A member on another machine needs the remote browser's grant for that
        // machine and that exact text; a local grant at the same spelling or a
        // grant on another machine does not stand in for it.
        let mut remote = previous.clone();
        remote.workspaces[0].additional_workspaces = vec![ssh_member("/srv/app")];
        let state = AppState::default();
        assert!(validate_workspace_authorizations(&previous, &remote, &state).is_err());
        state.authorize_remote_workspace("local", "/srv/app");
        state.authorize_remote_workspace("wsl:Ubuntu", "/srv/app");
        assert!(validate_workspace_authorizations(&previous, &remote, &state).is_err());
        state.authorize_remote_workspace("ssh:m1", "/srv/app");
        assert!(validate_workspace_authorizations(&previous, &remote, &state).is_ok());
    }

    /// Reloading must not revoke a grant, and a directory the previous document
    /// held anywhere — as some project's workspace 1 or as a member — keeps its
    /// standing wherever it moves among the projects.
    #[test]
    fn a_previously_held_directory_needs_no_new_grant_as_a_member() {
        let directory = tempfile::tempdir().unwrap();
        let picked = directory.path().join("shared-library");
        fs::create_dir(&picked).unwrap();
        let fresh = AppState::default();

        let mut held = default_document();
        held.workspaces[0].additional_workspaces =
            vec![host_member(&picked), ssh_member("/srv/app")];
        assert!(validate_workspace_authorizations(&held, &held, &fresh).is_ok());

        // Reordering members is not a new grant.
        let mut reordered = held.clone();
        reordered.workspaces[0].additional_workspaces.reverse();
        assert!(validate_workspace_authorizations(&held, &reordered, &fresh).is_ok());

        // Another spelling of the same local directory is the same grant.
        let mut respelled = held.clone();
        respelled.workspaces[0].additional_workspaces[0] = host_member(&picked.join("."));
        assert!(validate_workspace_authorizations(&held, &respelled, &fresh).is_ok());

        // A member promoted to its own project's workspace 1 keeps its standing.
        let mut promoted = held.clone();
        let mut project = promoted.workspaces[0].clone();
        project.id = "ws_promoted".into();
        project.path = picked.to_string_lossy().into_owned();
        project.additional_workspaces.clear();
        project.conversations.clear();
        promoted.workspaces.push(project);
        assert!(validate_workspace_authorizations(&held, &promoted, &fresh).is_ok());

        // And a project's workspace 1 may join another project as a member.
        let mut primary_only = promoted.clone();
        primary_only.workspaces[0].additional_workspaces.clear();
        let mut joined = primary_only.clone();
        joined.workspaces[0].additional_workspaces = vec![host_member(&picked)];
        assert!(validate_workspace_authorizations(&primary_only, &joined, &fresh).is_ok());

        // The same text on another machine is not what was held.
        let mut relocated = held.clone();
        relocated.workspaces[0].additional_workspaces[1].machine =
            Some(crate::model::RunTarget::Wsl {
                distro: "Ubuntu".into(),
            });
        assert!(validate_workspace_authorizations(&held, &relocated, &fresh).is_err());

        // A remote member that lost its machine is named as such.
        let mut dropped = held.clone();
        dropped.workspaces[0].additional_workspaces[1].machine = None;
        let error = validate_workspace_authorizations(&held, &dropped, &fresh)
            .expect_err("a remote path with no machine is not a local directory");
        assert!(error.contains("ssh:m1"), "{error}");
        assert!(error.contains("machine 字段"), "{error}");
    }

    #[test]
    fn a_project_cannot_list_one_directory_twice() {
        let directory = tempfile::tempdir().unwrap();
        let picked = directory.path().join("shared-library");
        fs::create_dir(&picked).unwrap();
        let root = directory.path().join("app");
        fs::create_dir(&root).unwrap();
        let state = AppState::default();
        state.authorize_workspace(&picked).unwrap();
        state.authorize_workspace(&root).unwrap();
        state.authorize_remote_workspace("ssh:m1", "/srv/app");
        state.authorize_remote_workspace("wsl:Ubuntu", "/srv/app");
        let previous = default_document();

        let mut twice = previous.clone();
        twice.workspaces[0].additional_workspaces =
            vec![host_member(&picked), host_member(&picked)];
        let error = validate_workspace_authorizations(&previous, &twice, &state)
            .expect_err("the same directory twice is refused");
        assert!(error.contains("工作区 3"), "{error}");

        // Workspace 1 counts, and two spellings of one local directory are one.
        let mut shadowing = previous.clone();
        shadowing.workspaces[0].path = root.to_string_lossy().into_owned();
        shadowing.workspaces[0].additional_workspaces = vec![host_member(&root.join("."))];
        let error = validate_workspace_authorizations(&previous, &shadowing, &state)
            .expect_err("a member that is workspace 1 again is refused");
        assert!(error.contains("工作区 2"), "{error}");

        // Remote directories compare by machine and exact text.
        let mut remote_twice = previous.clone();
        remote_twice.workspaces[0].additional_workspaces =
            vec![ssh_member("/srv/app"), ssh_member("/srv/app")];
        assert!(validate_workspace_authorizations(&previous, &remote_twice, &state).is_err());
        let mut two_machines = previous.clone();
        two_machines.workspaces[0].additional_workspaces = vec![
            ssh_member("/srv/app"),
            crate::model::AttachedWorkspace {
                machine: Some(crate::model::RunTarget::Wsl {
                    distro: "Ubuntu".into(),
                }),
                path: "/srv/app".into(),
            },
            host_member(&picked),
        ];
        assert!(validate_workspace_authorizations(&previous, &two_machines, &state).is_ok());
    }

    /// Every save re-checks every project's directories, so one proposed at the
    /// spelling it was held at must pass without resolving anything: on macOS
    /// resolving a path under Documents or on a network volume raises a privacy
    /// prompt or stalls. A miss still resolves the held set, and then accepts or
    /// refuses exactly what the eager check did.
    #[test]
    fn held_directories_are_resolved_only_when_the_exact_text_misses() {
        let directory = tempfile::tempdir().unwrap();
        let picked = directory.path().join("shared-library");
        fs::create_dir(&picked).unwrap();
        let other = directory.path().join("other");
        fs::create_dir(&other).unwrap();
        let picked_text = picked.to_string_lossy().into_owned();
        let respelled = picked.join(".").to_string_lossy().into_owned();
        let other_text = other.to_string_lossy().into_owned();
        let held_exact =
            HashSet::from([(crate::run_environment::env_key(None), picked_text.as_str())]);
        let remote = crate::model::RunTarget::Ssh {
            machine_id: "m1".into(),
        };
        let fresh = AppState::default();

        let held = LazyCanonicalKeys::new(vec![picked_text.as_str()]);
        assert!(
            authorize_project_directory(None, &picked_text, &held_exact, &held, &fresh).is_ok()
        );
        assert!(!held.is_resolved());
        // A directory on another machine never consults local keys, even at a
        // spelling that resolves here to what is held.
        assert!(authorize_project_directory(
            Some(&remote),
            &picked_text,
            &held_exact,
            &held,
            &fresh
        )
        .is_err());
        assert!(!held.is_resolved());
        // Another spelling of the held directory is the same grant.
        assert!(authorize_project_directory(None, &respelled, &held_exact, &held, &fresh).is_ok());
        assert!(held.is_resolved());

        // A directory nobody held is refused until a picker returns it.
        let held = LazyCanonicalKeys::new(vec![picked_text.as_str()]);
        assert!(
            authorize_project_directory(None, &other_text, &held_exact, &held, &fresh).is_err()
        );
        assert!(held.is_resolved());
        fresh.authorize_workspace(&other).unwrap();
        assert!(authorize_project_directory(None, &other_text, &held_exact, &held, &fresh).is_ok());
    }

    /// A held directory that has gone away — an unplugged drive, a network
    /// share that is not mounted — is still re-proposed at the same text, and
    /// that text alone keeps its standing, as a project's workspace and as a
    /// conversation's attached one. Proposed as new, it is refused as before.
    #[test]
    fn an_unchanged_save_keeps_held_directories_that_no_longer_resolve() {
        let directory = tempfile::tempdir().unwrap();
        let gone = directory
            .path()
            .join("unmounted-share")
            .to_string_lossy()
            .into_owned();
        let mut held = default_document();
        let mut project = held.workspaces[0].clone();
        project.id = "ws_gone".into();
        project.path = gone.clone();
        project.additional_workspaces = vec![host_member(Path::new(&gone).join("docs").as_path())];
        project.conversations.clear();
        held.workspaces.push(project);
        held.workspaces[0].conversations[0].attached_workspaces =
            vec![host_member(Path::new(&gone))];

        let fresh = AppState::default();
        assert!(validate_workspace_authorizations(&held, &held, &fresh).is_ok());
        assert!(validate_workspace_authorizations(&default_document(), &held, &fresh).is_err());
        let mut attached_only = default_document();
        attached_only.workspaces[0].conversations[0].attached_workspaces =
            vec![host_member(Path::new(&gone))];
        assert!(
            validate_workspace_authorizations(&default_document(), &attached_only, &fresh).is_err()
        );
    }

    #[test]
    fn a_project_holds_a_bounded_number_of_workspaces() {
        let mut document = default_document();
        document.workspaces[0].additional_workspaces = (0..MAX_PROJECT_WORKSPACES - 1)
            .map(|index| ssh_member(&format!("/srv/app-{index}")))
            .collect();
        assert!(validate_shape(&document).is_ok());

        document.workspaces[0]
            .additional_workspaces
            .push(ssh_member("/srv/one-too-many"));
        let error = validate_shape(&document).expect_err("an unbounded project is refused");
        assert!(
            error.contains(&format!("超过 {MAX_PROJECT_WORKSPACES} 个")),
            "{error}"
        );

        let mut blank = default_document();
        blank.workspaces[0].additional_workspaces = vec![ssh_member("  ")];
        assert!(validate_shape(&blank).unwrap_err().contains("路径为空"));
    }

    /// The temporary project is one scratch directory per conversation, so it
    /// has no root for a second workspace to sit beside — and the save-time
    /// canonicalization that clears its path must not quietly clear these.
    #[test]
    fn the_temporary_project_has_no_members() {
        let previous = default_document();
        let mut changed = previous.clone();
        changed
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.kind == WorkspaceKind::Temporary)
            .unwrap()
            .additional_workspaces = vec![ssh_member("/srv/app")];

        let error = validate_shape(&changed).expect_err("a temporary project has no members");
        assert!(error.contains("额外工作区"), "{error}");
        let state = AppState::default();
        state.authorize_remote_workspace("ssh:m1", "/srv/app");
        assert!(prepare_save_transition(&previous, &changed, &state).is_err());
    }
}
