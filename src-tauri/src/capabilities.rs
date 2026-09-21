use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    hash::{DefaultHasher, Hash, Hasher},
    path::{Path, PathBuf},
};

use serde_json::Value;

use crate::{
    mcp::RuntimeMcpServer,
    mcp_config, memory_archive_file,
    model::{
        AddedSkill, AppDocument, CapabilityCatalog, Conversation, HookDefinition, HookEvent,
        McpServerConfig, ResolvedLanguage, ResolvedSkill, ResourceDescriptor, ResourceSource,
        ToolDescriptionEntry, Workspace,
    },
    prompt_profile::{self, PromptKey, PromptProfile},
    prompt_profile_files, skills,
};

const METADATA_READ_LIMIT: u64 = 64 * 1024;
pub(crate) const SKILL_READ_LIMIT: u64 = 256 * 1024;
const RUNTIME_CONTEXT_LIMIT: usize = 1024 * 1024;
/// Maximum metadata length for catalog display. Names, descriptions, authors,
/// versions, and tags share this limit.
const METADATA_VALUE_LIMIT: usize = 240;
/// Maximum trigger text supplied to the model. Trigger conditions must remain intact,
/// so this limit is larger than the catalog display limit.
const SKILL_TRIGGER_LIMIT: usize = 1024;
const CONFIG_DIRECTORY: &str = ".mework";
const LEGACY_CONFIG_DIRECTORY: &str = ".naiword";

/// Skill body file name. Readers accept only this exact name.
pub const SKILL_MANIFEST: &str = "SKILL.md";

/// Tool name for on-demand skill loading.
///
/// `catalog::tool_catalog` provides a localized model-facing descriptor and timeline
/// card, but this tool is derived from `skill_tool_enabled`, not user-selected. The
/// renderer mirror is `SKILL_TOOL_NAME` in `src/lib/taskTools.ts`.
pub const SKILL_TOOL: &str = "skill";

/// Derives `skill` into the enabled tools for this turn.
///
/// Remove it before adding it so stale persisted names cannot re-enable a disabled
/// setting. Expose it only when skills resolved; an empty enum would invite a call
/// guaranteed to fail.
pub fn apply_skill_tool(enabled_tools: &mut Vec<String>, resolved_skills: usize) {
    enabled_tools.retain(|name| name != SKILL_TOOL);
    if resolved_skills > 0 {
        enabled_tools.push(SKILL_TOOL.to_owned());
    }
}

/// Tool name for on-demand MCP tool loading.
///
/// Like [`SKILL_TOOL`] it has a catalog descriptor for its timeline card and
/// its localized prose, but it is never a user selection: it follows
/// `mcp_tool_discovery_enabled` and the presence of at least one withheld tool.
/// The renderer mirror is `TOOL_SEARCH_TOOL_NAME` in `src/lib/taskTools.ts`.
pub const TOOL_SEARCH_TOOL: &str = "tool_search";

/// Derives `tool_search` into the enabled tools for this turn.
///
/// Strip before adding, like [`apply_skill_tool`], so a name left in a
/// conversation's persisted list cannot re-enable a switch the user turned off.
/// Withheld tools are the whole condition: with none of them, the tool has
/// nothing to hand out and every call it invited would fail.
pub fn apply_tool_search_tool(enabled_tools: &mut Vec<String>, deferred_tools: usize) {
    enabled_tools.retain(|name| name != TOOL_SEARCH_TOOL);
    if deferred_tools > 0 {
        enabled_tools.push(TOOL_SEARCH_TOOL.to_owned());
    }
}

/// The kinds of capability a `.mework` directory can hold, for the commands
/// that reveal or delete one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CapabilityKind {
    Skills,
    Mcp,
    Hooks,
    Lsp,
}

/// One place configuration is read from: the user's home or one workspace.
///
/// Everything under a level sits in its `.mework` directory (or the legacy
/// `.naiword` one when that is all there is): `skills/<dir>/SKILL.md`,
/// `mcp.json`, `hooks.json` and `tool-descriptions/*.json`.
#[derive(Clone, Debug)]
pub struct ConfigLevel {
    pub source: ResourceSource,
    pub base: PathBuf,
    /// Set for a workspace level; the descriptors it yields carry it.
    pub workspace_id: Option<String>,
}

impl ConfigLevel {
    /// The global level, `~`; `None` when the platform has no home directory.
    pub fn user() -> Option<Self> {
        dirs::home_dir().map(|home| Self {
            source: ResourceSource::User,
            base: home,
            workspace_id: None,
        })
    }

    /// A workspace's level; `None` for the temporary workspace, which has no
    /// directory of its own.
    pub fn workspace(workspace: &Workspace) -> Option<Self> {
        if workspace.path.trim().is_empty() {
            return None;
        }
        Some(Self {
            source: ResourceSource::Workspace,
            base: PathBuf::from(&workspace.path),
            workspace_id: Some(workspace.id.clone()),
        })
    }

    /// The directory or file holding one kind of capability at this level.
    pub fn path_for(&self, kind: CapabilityKind) -> PathBuf {
        preferred_config_path(&self.base, Path::new(kind.relative_path()))
    }

    /// The `.mework` directory itself (never the legacy fallback: creating
    /// anything goes to the current name).
    pub fn config_directory(&self) -> PathBuf {
        self.base.join(CONFIG_DIRECTORY)
    }
}

impl CapabilityKind {
    /// Where one kind lives relative to a level's config directory. Writers use
    /// it against [`ConfigLevel::config_directory`] rather than
    /// [`ConfigLevel::path_for`], which resolves the legacy fallback.
    pub(crate) fn relative_path(self) -> &'static str {
        match self {
            CapabilityKind::Skills => "skills",
            CapabilityKind::Mcp => "mcp.json",
            CapabilityKind::Hooks => "hooks.json",
            CapabilityKind::Lsp => "lsp.json",
        }
    }
}

/// Where one kind of capability lives under `base`, for callers that hold a
/// directory rather than a [`ConfigLevel`] — the language-server resolver reads
/// a workspace path straight off a tool request.
pub fn config_path_for(base: &Path, kind: CapabilityKind) -> PathBuf {
    preferred_config_path(base, Path::new(kind.relative_path()))
}

/// The spellings of one kind's file relative to a base directory, preferred
/// first, for a caller that has to test them on a machine whose filesystem it
/// cannot join paths on — the same fallback [`config_path_for`] applies here.
pub(crate) fn relative_config_paths(kind: CapabilityKind) -> [String; 2] {
    [
        format!("{CONFIG_DIRECTORY}/{}", kind.relative_path()),
        format!("{LEGACY_CONFIG_DIRECTORY}/{}", kind.relative_path()),
    ]
}

/// The global level plus every workspace of the document — what the catalog
/// shows, so a preset can select from any of them.
pub fn all_levels(document: &AppDocument) -> Vec<ConfigLevel> {
    ConfigLevel::user()
        .into_iter()
        .chain(
            document
                .workspaces
                .iter()
                .filter_map(ConfigLevel::workspace),
        )
        .collect()
}

/// The global level plus the level of the workspace owning `conversation` —
/// what a run of that conversation may use. A conversation nobody owns (which
/// the trusted request path rejects before getting here) sees the global level
/// only.
pub fn levels_for_conversation(
    document: &AppDocument,
    conversation: &Conversation,
) -> Vec<ConfigLevel> {
    let workspace = document.workspaces.iter().find(|workspace| {
        workspace
            .conversations
            .iter()
            .any(|candidate| candidate.id == conversation.id)
    });
    ConfigLevel::user()
        .into_iter()
        .chain(workspace.and_then(ConfigLevel::workspace))
        .collect()
}

/// Discovers the current capability catalog: skills, MCP servers and hooks
/// from `~/.mework` and every workspace's `.mework`, plus the tool-description
/// files and the two built-in prompt profiles. `app_data` locates the editable
/// copies of the built-in profiles, which are catalog entries like any other file.
pub fn discover(document: &AppDocument, app_data: &Path) -> CapabilityCatalog {
    CapabilityCatalog {
        tool_description_files: discover_tool_description_files(document, app_data),
        ..discover_levels(
            &all_levels(document),
            document.global_settings.resolved_app_language,
        )
        .catalog
    }
}

/// What a scan of some levels found: the catalog rows, and the executable
/// definitions behind the hook and MCP rows that are available.
pub(crate) struct DiscoveredCapabilities {
    pub catalog: CapabilityCatalog,
    pub hooks: HashMap<String, HookDefinition>,
    pub mcp_servers: HashMap<String, McpServerConfig>,
}

/// Scans `levels` in order. The same location reached through two levels (a
/// workspace that is the home directory) is listed once, under the first.
pub(crate) fn discover_levels(
    levels: &[ConfigLevel],
    language: ResolvedLanguage,
) -> DiscoveredCapabilities {
    let mut seen: HashSet<String> = HashSet::new();
    let mut skills_out = Vec::new();
    let mut mcps = Vec::new();
    let mut lsps = Vec::new();
    let mut hooks = Vec::new();
    let mut hook_definitions = HashMap::new();
    let mut mcp_servers = HashMap::new();
    for level in levels {
        for descriptor in skills::discover_in_root(
            &level.path_for(CapabilityKind::Skills),
            level.source,
            level.workspace_id.as_deref(),
        ) {
            if seen.insert(descriptor.location.clone()) {
                skills_out.push(descriptor);
            }
        }
        for entry in mcp_config::read_file(
            &level.path_for(CapabilityKind::Mcp),
            level.source,
            level.workspace_id.as_deref(),
        ) {
            if seen.insert(entry.descriptor.location.clone()) {
                if let Some(config) = entry.config {
                    mcp_servers.insert(entry.descriptor.id.clone(), config);
                }
                mcps.push(entry.descriptor);
            }
        }
        for entry in read_hooks_file(
            &level.path_for(CapabilityKind::Hooks),
            level.source,
            level.workspace_id.as_deref(),
            language,
        ) {
            if seen.insert(entry.descriptor.location.clone()) {
                hook_definitions.insert(entry.descriptor.id.clone(), entry.definition);
                hooks.push(entry.descriptor);
            }
        }
        for entry in crate::lsp_config::read_file(
            &level.path_for(CapabilityKind::Lsp),
            level.source,
            level.workspace_id.as_deref(),
        ) {
            if seen.insert(entry.descriptor.location.clone()) {
                lsps.push(entry.descriptor);
            }
        }
    }
    // The built-in presets are not a level: they have no file and no workspace,
    // and they are listed last because any `lsp.json` entry of the same name
    // replaces them.
    for entry in crate::lsp_config::builtin_entries(language) {
        if seen.insert(entry.descriptor.location.clone())
            && !lsps
                .iter()
                .any(|existing| existing.name == entry.descriptor.name)
        {
            lsps.push(entry.descriptor);
        }
    }
    skills_out.sort_by(resource_sort);
    mcps.sort_by(resource_sort);
    hooks.sort_by(resource_sort);
    DiscoveredCapabilities {
        catalog: CapabilityCatalog {
            hooks,
            skills: skills_out,
            mcps,
            lsps,
            tool_description_files: Vec::new(),
        },
        hooks: hook_definitions,
        mcp_servers,
    }
}

/// Scans tool-description files separately because [`resolve_prompt_profile`]
/// needs only these files and must not require a skills root.
///
/// The two built-in profiles come first and are always present: the English
/// one is the default every conversation renders with until it selects
/// something else, and neither has a location a user could delete.
fn discover_tool_description_files(
    document: &AppDocument,
    app_data: &Path,
) -> Vec<ResourceDescriptor> {
    let mut files: Vec<ResourceDescriptor> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for level in all_levels(document) {
        scan_tool_description_root(
            &preferred_config_path(&level.base, Path::new("tool-descriptions")),
            level.source,
            level.workspace_id.as_deref(),
            &mut files,
            &mut seen,
        );
    }

    files.sort_by(resource_sort);
    let mut catalog = builtin_prompt_profile_descriptors(app_data);
    catalog.extend(files);
    catalog
}

/// The two built-in profiles as catalog entries. `location` stays a `builtin:`
/// pseudo-location so the renderer can tell them from the files a user added;
/// the path their texts are edited at goes into the description instead.
fn builtin_prompt_profile_descriptors(app_data: &Path) -> Vec<ResourceDescriptor> {
    [
        PromptProfile::builtin_english(),
        PromptProfile::builtin_chinese(),
    ]
    .into_iter()
    .map(|profile| ResourceDescriptor {
        id: profile.id.clone(),
        name: profile.name.clone(),
        description: {
            let path = prompt_profile_files::builtin_profile_path(app_data, profile.language)
                .to_string_lossy()
                .into_owned();
            match profile.language {
                ResolvedLanguage::EnUs => {
                    format!("Built-in English prompts and tool descriptions — editable at {path}")
                }
                ResolvedLanguage::ZhCn => {
                    format!("内置中文提示词与工具描述——可在 {path} 编辑")
                }
            }
        },
        location: format!(
            "builtin:{}",
            match profile.language {
                ResolvedLanguage::EnUs => "en-US",
                ResolvedLanguage::ZhCn => "zh-CN",
            }
        ),
        source: ResourceSource::Builtin,
        available: true,
        workspace_id: None,
    })
    .collect()
}

/// Runtime context for this turn: the system-prompt addendum, the skills
/// supplied to the `skill` tool, the skills that arrive as their own system
/// message, and the MCP servers to dial.
///
/// `skill_tool_enabled` decides the FORM the selected skills take — bodies when
/// it is off, a name-and-trigger listing when it is on — and the conversation's
/// tool lock decides the ROUTE. Skills the opening prompt was built with stay in
/// `addendum`; anything selected after that goes in `added_skills`, because the
/// prompt the earlier rounds were answered against cannot be rewritten under
/// them. MCP and hooks always enter `addendum`.
#[derive(Debug)]
pub struct RuntimeContext {
    pub addendum: String,
    /// Non-empty only when the skill tool is enabled — every selected skill,
    /// wherever its listing went, so the tool can serve any of them. Otherwise
    /// skill bodies are in `addendum` or `added_skills`; using both outputs
    /// would duplicate them.
    pub skills: Vec<ResolvedSkill>,
    /// Skills selected after the opening prompt, already rendered for delivery
    /// as system messages.
    pub added_skills: Vec<AddedSkill>,
    /// The selected, available servers, in selection order. Host-owned: the
    /// renderer's `mcp_ids` only ever pick from what discovery read off disk.
    pub mcp_servers: Vec<RuntimeMcpServer>,
    /// The selected hooks, in selection order, with their executable commands.
    /// Never from the renderer or the persisted document: only from the files.
    pub hooks: Vec<HookDefinition>,
}

/// Resolve the persisted conversation's selected capability presets into model instructions.
/// Renderer-provided resource text is never trusted: every descriptor is rediscovered from the
/// current filesystem/configuration and skill bodies are read with strict size limits.
///
/// Discovery is scoped to the global level and the conversation's own
/// workspace, so a run can never read another project's skill or launch its
/// MCP server. `profile` supplies the wording of the MCP and hook sections;
/// skill bodies are user-authored and enter verbatim.
pub fn runtime_context(
    document: &AppDocument,
    conversation: &Conversation,
    profile: &PromptProfile,
) -> Result<RuntimeContext, String> {
    let discovered = discover_levels(
        &levels_for_conversation(document, conversation),
        document.global_settings.resolved_app_language,
    );
    runtime_context_from_discovery(conversation, &discovered, profile)
}

/// A selected skill or MCP id that discovery no longer finds is skipped, not
/// fatal: the folder was deleted, the entry left the file, or the id predates
/// file discovery, and a conversation whose tool lock still holds that id could
/// otherwise never run again. An id discovery does find but cannot use fails
/// closed, with the reason, because the row is on screen to be fixed. Hooks
/// are always fatal when missing: a guard that silently stops running is worse
/// than a run that does not start.
fn runtime_context_from_discovery(
    conversation: &Conversation,
    discovered: &DiscoveredCapabilities,
    profile: &PromptProfile,
) -> Result<RuntimeContext, String> {
    let catalog = &discovered.catalog;
    let mut selected_hook_ids = Vec::new();
    let mut selected_skill_ids = Vec::new();
    let mut selected_mcp_ids = Vec::new();

    extend_unique(&mut selected_hook_ids, &conversation.settings.hook_ids);
    extend_unique(&mut selected_skill_ids, &conversation.settings.skill_ids);
    extend_unique(&mut selected_mcp_ids, &conversation.settings.mcp_ids);

    let via_tool = conversation.settings.skill_tool_enabled;
    /* Which skills the system prompt is allowed to carry. A conversation with
       no pin has not opened its prompt yet, so everything selected belongs to
       it — which is also what a conversation predating the pin gets, and what
       it was already doing. */
    let prompt_skill_ids = conversation
        .settings
        .tool_lock
        .as_ref()
        .and_then(|lock| lock.prompt_skill_ids.clone());
    let in_opening_prompt = |resource_id: &str| match &prompt_skill_ids {
        None => true,
        Some(pinned) => pinned.iter().any(|id| id == resource_id),
    };
    let mut sections = Vec::new();
    let mut skills = Vec::new();
    let mut added_skills = Vec::new();
    let mut listing_rows = Vec::new();
    for resource_id in selected_skill_ids {
        let Some(descriptor) = catalog
            .skills
            .iter()
            .find(|resource| resource.id == resource_id)
        else {
            eprintln!("Skipping skill {resource_id}: it is not in ~/.mework/skills or this workspace's .mework/skills any more");
            continue;
        };
        if !descriptor.available {
            return Err(format!(
                "Skill \"{}\" cannot be used: {}",
                descriptor.name, descriptor.description
            ));
        }
        let parsed = skill_document_from_source(&read_skill_body(descriptor)?);
        let opening = in_opening_prompt(&resource_id);
        if via_tool {
            // The model selects skills by directory name, so every name must
            // identify exactly one skill. Duplicate names make one skill
            // unreachable, wherever its listing was written.
            let name = skills::directory_name_of(descriptor);
            if skills
                .iter()
                .any(|other: &ResolvedSkill| other.name == name)
            {
                return Err(format!(
                    "This conversation selected two skills whose directories are both named \"{name}\". On-demand loading selects skills by directory name, so one would be unreachable; rename one directory or select only one."
                ));
            }
            if opening {
                if !parsed.trigger.trim().is_empty() {
                    listing_rows.push(profile.render(
                        PromptKey::SkillListingRow,
                        &[("name", &name), ("trigger", &parsed.trigger)],
                    ));
                }
            } else {
                added_skills.push(AddedSkill {
                    resource_id: resource_id.clone(),
                    content: profile.render(
                        PromptKey::SystemSkillAddedTrigger,
                        &[("name", &name), ("trigger", &parsed.trigger)],
                    ),
                });
            }
            skills.push(ResolvedSkill {
                name,
                trigger: parsed.trigger,
                body: parsed.body,
                directory: skill_directory(descriptor),
            });
        } else if !parsed.body.is_empty() {
            if opening {
                // Include only the body. The heading and frontmatter are registration
                // metadata, while `---` below provides structure between skill bodies.
                sections.push(parsed.body);
            } else {
                added_skills.push(AddedSkill {
                    resource_id: resource_id.clone(),
                    content: profile.render(
                        PromptKey::SystemSkillAddedBody,
                        &[("name", &descriptor.name), ("body", &parsed.body)],
                    ),
                });
            }
        }
    }
    /* On-demand loading puts the catalog in the prompt rather than in the
       tool's schema: a schema is re-declared on every request, so a listing
       there would change the tool set the moment another skill was selected. */
    if !listing_rows.is_empty() {
        sections.push(format!(
            "{}\n{}",
            profile.text(PromptKey::SkillListingHeading),
            listing_rows.join("\n")
        ));
    }

    let mut mcp_servers = Vec::new();
    let mut server_rows = Vec::new();
    for resource_id in selected_mcp_ids {
        let Some(descriptor) = catalog
            .mcps
            .iter()
            .find(|resource| resource.id == resource_id)
        else {
            eprintln!("Skipping MCP server {resource_id}: it is not in ~/.mework/mcp.json or this workspace's .mework/mcp.json any more");
            continue;
        };
        let Some(config) = discovered
            .mcp_servers
            .get(&descriptor.id)
            .filter(|_| descriptor.available)
        else {
            return Err(format!(
                "MCP server \"{}\" cannot be used: {}",
                descriptor.name, descriptor.description
            ));
        };
        mcp_servers.push(RuntimeMcpServer::from_config(config));
        // A server without a description carries the English default from
        // discovery; the model-facing row says it in the profile's words.
        let description = if descriptor.description
            == PromptKey::SystemMcpServerDefaultDescription.builtin_en()
        {
            profile.text(PromptKey::SystemMcpServerDefaultDescription)
        } else {
            descriptor.description.as_str()
        };
        server_rows.push(profile.render(
            PromptKey::SystemCapabilityRow,
            &[("name", &descriptor.name), ("description", description)],
        ));
    }
    if !server_rows.is_empty() {
        sections.push(profile.render(
            PromptKey::SystemMcpSection,
            &[("servers", &server_rows.join("\n"))],
        ));
    }

    let mut hooks = Vec::new();
    if !selected_hook_ids.is_empty() {
        let selected = selected_hook_ids
            .iter()
            .map(|resource_id| {
                catalog
                    .hooks
                    .iter()
                    .find(|resource| resource.id == *resource_id && resource.available)
                    .ok_or_else(|| {
                        format!(
                            "Hook {resource_id} is unavailable in the current workspace. Check hooks.json and scan again."
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        // The catalog description is UI text; the model-facing row is rendered
        // from the hook's event and matcher in the profile's words.
        let rows = selected
            .iter()
            .map(|resource| {
                let definition = discovered.hooks.get(&resource.id);
                hooks.extend(definition.cloned());
                let description = definition
                    .map(|definition| hook_description(profile, definition))
                    .unwrap_or_else(|| resource.description.clone());
                profile.render(
                    PromptKey::SystemCapabilityRow,
                    &[("name", &resource.name), ("description", &description)],
                )
            })
            .collect::<Vec<_>>();
        let hook_names = profile.join_list(selected.iter().map(|resource| resource.name.as_str()));
        sections.push(profile.render(
            PromptKey::SystemHooksSection,
            &[("hook_names", &hook_names), ("hooks", &rows.join("\n"))],
        ));
    }

    let addendum = sections.join("\n\n---\n\n");
    // Every output is independently measured. Tool-mode bodies and the messages
    // a later skill arrives in do not enter the system prompt, but they still
    // reside in the request and share its size limit.
    let skill_bytes = skills.iter().map(|skill| skill.body.len()).sum::<usize>();
    let added_bytes = added_skills
        .iter()
        .map(|skill| skill.content.len())
        .sum::<usize>();
    if addendum.len() > RUNTIME_CONTEXT_LIMIT
        || skill_bytes > RUNTIME_CONTEXT_LIMIT
        || added_bytes > RUNTIME_CONTEXT_LIMIT
    {
        return Err(
            "The runtime context for enabled skills and hooks exceeds the 1 MiB limit.".into(),
        );
    }
    Ok(RuntimeContext {
        addendum,
        skills,
        added_skills,
        mcp_servers,
        hooks,
    })
}

/// Skill directory: the parent of its body file.
///
/// Skills can include `scripts/` and `references/` addressed relative to `SKILL.md`;
/// providing the directory lets the model resolve those references.
fn skill_directory(descriptor: &ResourceDescriptor) -> String {
    Path::new(&descriptor.location)
        .parent()
        .map(|parent| parent.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn resource_sort(left: &ResourceDescriptor, right: &ResourceDescriptor) -> std::cmp::Ordering {
    left.name
        .to_lowercase()
        .cmp(&right.name.to_lowercase())
        .then_with(|| left.location.cmp(&right.location))
}

#[derive(Clone)]
struct HookEntry {
    descriptor: ResourceDescriptor,
    definition: HookDefinition,
}

/// Where a hook sits inside its `hooks.json` — the file, and the position
/// [`read_hooks_file`] read the handler from.
///
/// A hook has no identity of its own on disk: the id in its descriptor is a hash
/// over this address, which is why the address has to be recovered from the
/// descriptor rather than looked up.
pub(crate) struct HookAddress {
    pub path: PathBuf,
    pub event: String,
    pub group_index: usize,
    pub handler_index: usize,
}

/// Reads a hook descriptor's `location` back into the address it was built from.
///
/// The format is the one [`read_hooks_file`] writes: the config path, then the
/// JSON pointer `#/hooks/<event>/<group>/hooks/<handler>`. Anything else is not
/// an address this application produced, so it resolves to nothing rather than
/// to a guess.
pub(crate) fn parse_hook_location(location: &str) -> Option<HookAddress> {
    let (path, pointer) = location.rsplit_once("#/hooks/")?;
    let mut parts = pointer.split('/');
    let event = parts.next()?;
    let group_index = parts.next()?.parse().ok()?;
    if parts.next()? != "hooks" {
        return None;
    }
    let handler_index = parts.next()?.parse().ok()?;
    if parts.next().is_some() || event.is_empty() {
        return None;
    }
    Some(HookAddress {
        path: PathBuf::from(path),
        event: event.to_owned(),
        group_index,
        handler_index,
    })
}

/// Drops one handler from a `hooks.json`, leaving every other entry and every
/// other field of the file as it was.
///
/// The remaining handlers in the same group move up, and because a hook's id is
/// a hash over its position, they come back from the next scan under new ids —
/// exactly as they would had the user deleted the line by hand. Conversations
/// that had selected one of them show it as a dangling selection, which is the
/// state that already exists for a hand-edited file.
///
/// An emptied group is left in place rather than pruned: the file is the user's,
/// and a group with no handlers reads as nothing while keeping the matcher they
/// wrote.
pub(crate) fn remove_hook_from_file(address: &HookAddress) -> Result<(), String> {
    let display = address.path.display();
    let text = fs::read_to_string(&address.path)
        .map_err(|error| format!("无法读取 {display}：{error}"))?;
    // Parse first, so a malformed file is reported as such rather than cut into,
    // and so the address is checked against what the file actually holds.
    let document: Value = serde_json::from_str(&text)
        .map_err(|error| format!("{display} 不是合法的 JSON：{error}"))?;
    document
        .get("hooks")
        .and_then(Value::as_object)
        .and_then(|hooks| hooks.get(&address.event))
        .and_then(Value::as_array)
        .and_then(|groups| groups.get(address.group_index))
        .and_then(Value::as_object)
        .and_then(|group| group.get("hooks"))
        .and_then(Value::as_array)
        .filter(|handlers| address.handler_index < handlers.len())
        .ok_or_else(|| format!("{display} 里已经没有这个钩子了"))?;
    // The edit is textual: this is the user's file, and re-serializing it would
    // reorder every object key and reflow every line around the one deletion.
    let edited = crate::json_edit::remove_array_element(
        &text,
        &[
            crate::json_edit::Step::Key("hooks"),
            crate::json_edit::Step::Key(&address.event),
            crate::json_edit::Step::Index(address.group_index),
            crate::json_edit::Step::Key("hooks"),
        ],
        address.handler_index,
    )
    .map_err(|error| format!("无法从 {display} 里删除这个钩子：{error}"))?;
    write_config_file(&address.path, &edited)
}

/// Replaces a config file through a sibling temporary, so an interrupted write
/// leaves the original rather than a half file.
pub(crate) fn write_config_file(path: &Path, contents: &str) -> Result<(), String> {
    let display = path.display();
    let parent = path
        .parent()
        .ok_or_else(|| format!("{display} 没有可写入的目录"))?;
    let temporary = parent.join(format!(".config.{}.tmp", uuid::Uuid::new_v4().simple()));
    fs::write(&temporary, contents).map_err(|error| format!("无法写入 {display}：{error}"))?;
    match fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            Err(format!("无法替换 {display}：{error}"))
        }
    }
}

/// Every hook the catalog would list, paired with the file it came from. The
/// catalog itself keeps only the descriptors, and deleting one needs the address.
pub(crate) fn hook_addresses(document: &AppDocument) -> Vec<(ResourceDescriptor, HookAddress)> {
    discover_levels(
        &all_levels(document),
        document.global_settings.resolved_app_language,
    )
    .catalog
    .hooks
    .into_iter()
    .filter_map(|descriptor| {
        let address = parse_hook_location(&descriptor.location)?;
        Some((descriptor, address))
    })
    .collect()
}

/// Deletes a discovered skill's folder, whichever level it was read from.
///
/// The skill is looked up in a fresh scan rather than trusted from the
/// renderer, and `skills::delete_directory` only ever removes a direct child of
/// one of the `skills/` roots that scan read.
pub(crate) fn delete_skill(document: &AppDocument, skill_id: &str) -> Result<(), String> {
    let levels = all_levels(document);
    let descriptor = discover_levels(&levels, document.global_settings.resolved_app_language)
        .catalog
        .skills
        .into_iter()
        .find(|descriptor| descriptor.id == skill_id)
        .ok_or_else(|| format!("技能 {skill_id} 不存在"))?;
    let roots = levels
        .iter()
        .map(|level| level.path_for(CapabilityKind::Skills))
        .collect::<Vec<_>>();
    skills::delete_directory(&roots, &descriptor)
}

/// Removes a discovered MCP server from the `mcp.json` it was read from.
pub(crate) fn delete_mcp_server(document: &AppDocument, server_id: &str) -> Result<(), String> {
    let descriptor = discover_levels(
        &all_levels(document),
        document.global_settings.resolved_app_language,
    )
    .catalog
    .mcps
    .into_iter()
    .find(|descriptor| descriptor.id == server_id)
    .ok_or_else(|| format!("MCP 服务器 {server_id} 不存在"))?;
    let (path, name) = mcp_config::parse_location(&descriptor.location)
        .ok_or_else(|| format!("MCP 服务器 {server_id} 的位置无法解析"))?;
    mcp_config::remove_server_from_file(&path, &name)
}

/// The launch configuration of one discovered, available MCP server, for a
/// probe. Read off disk on every call: the renderer names a server, never
/// describes one.
pub(crate) fn mcp_server_config(
    document: &AppDocument,
    server_id: &str,
) -> Result<McpServerConfig, String> {
    let discovered = discover_levels(
        &all_levels(document),
        document.global_settings.resolved_app_language,
    );
    if let Some(config) = discovered.mcp_servers.get(server_id) {
        return Ok(config.clone());
    }
    match discovered
        .catalog
        .mcps
        .iter()
        .find(|descriptor| descriptor.id == server_id)
    {
        Some(descriptor) => Err(format!(
            "MCP 服务器 {} 无法使用：{}",
            descriptor.name, descriptor.description
        )),
        None => Err(format!("MCP 服务器 {server_id} 不存在")),
    }
}

/// The on-disk place one kind of capability lives at one level, created on
/// demand so the file manager has somewhere to open: the `skills/` directory,
/// or the `.mework` directory holding `mcp.json` / `hooks.json` (the file
/// itself when it already exists).
pub(crate) fn capability_location_to_reveal(
    document: &AppDocument,
    kind: CapabilityKind,
    workspace_id: Option<&str>,
) -> Result<PathBuf, String> {
    let level = match workspace_id {
        Some(workspace_id) => document
            .workspaces
            .iter()
            .find(|workspace| workspace.id == workspace_id)
            .and_then(ConfigLevel::workspace)
            .ok_or_else(|| "这个工作区没有可打开的目录".to_owned())?,
        None => ConfigLevel::user().ok_or_else(|| "无法确定用户主目录".to_owned())?,
    };
    let existing = level.path_for(kind);
    if existing.exists() {
        return Ok(existing);
    }
    let directory = match kind {
        CapabilityKind::Skills => level.config_directory().join("skills"),
        CapabilityKind::Mcp | CapabilityKind::Hooks | CapabilityKind::Lsp => {
            level.config_directory()
        }
    };
    fs::create_dir_all(&directory)
        .map_err(|error| format!("无法创建 {}：{error}", directory.display()))?;
    Ok(directory)
}

fn read_hooks_file(
    path: &Path,
    source: ResourceSource,
    workspace_id: Option<&str>,
    language: ResolvedLanguage,
) -> Vec<HookEntry> {
    let labels = PromptProfile::builtin_for_language(language);
    let Ok(file) = File::open(path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_reader::<_, Value>(file) else {
        return Vec::new();
    };
    let Some(root) = value.as_object() else {
        return Vec::new();
    };
    let Some(hooks) = root.get("hooks").and_then(Value::as_object) else {
        return Vec::new();
    };
    let config_location = path.to_string_lossy();
    let scope = if source == ResourceSource::Workspace {
        "workspace"
    } else {
        "user"
    };
    let mut entries = Vec::new();
    for (event_name, groups) in hooks {
        let Some(event) = parse_hook_event(event_name) else {
            continue;
        };
        let Some(groups) = groups.as_array() else {
            continue;
        };
        for (group_index, group) in groups.iter().filter_map(Value::as_object).enumerate() {
            let matcher = group
                .get("matcher")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty() && *value != "*")
                .map(str::to_owned);
            if matcher
                .as_deref()
                .is_some_and(|value| regex::Regex::new(value).is_err())
            {
                continue;
            }
            let Some(handlers) = group.get("hooks").and_then(Value::as_array) else {
                continue;
            };
            for (handler_index, handler) in handlers.iter().filter_map(Value::as_object).enumerate()
            {
                if handler.get("type").and_then(Value::as_str) != Some("command") {
                    continue;
                }
                let Some(command) = handler
                    .get("command")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty() && value.len() <= 64 * 1024)
                    .map(str::to_owned)
                else {
                    continue;
                };
                if handler.get("asyncRewake").and_then(Value::as_bool) == Some(true) {
                    continue;
                }
                if handler.get("async").and_then(Value::as_bool) == Some(true)
                    && event != HookEvent::InstructionsLoaded
                {
                    continue;
                }
                let timeout_seconds = handler.get("timeout").and_then(Value::as_u64).unwrap_or(30);
                if !(1..=600).contains(&timeout_seconds) {
                    continue;
                }
                let command_windows = handler
                    .get("commandWindows")
                    .or_else(|| handler.get("command_windows"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty() && value.len() <= 64 * 1024)
                    .map(str::to_owned);
                let status_message = handler
                    .get("statusMessage")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(|value| truncate_chars(value, 240));
                let name = handler
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(|value| truncate_chars(value, 240))
                    .or_else(|| status_message.clone())
                    .unwrap_or_else(|| format!("{} #{}", event_name, handler_index + 1));
                let pointer = format!("#/hooks/{event_name}/{group_index}/hooks/{handler_index}");
                let location = format!("{config_location}{pointer}");
                let key = format!("{event_name}:{group_index}:{handler_index}");
                let id = stable_id(&format!("hook_{scope}"), &key, &location);
                let definition = HookDefinition {
                    id: id.clone(),
                    name: name.clone(),
                    event,
                    matcher: matcher.clone(),
                    command,
                    command_windows,
                    status_message,
                    enabled: true,
                    timeout_ms: timeout_seconds * 1_000,
                };
                entries.push(HookEntry {
                    descriptor: ResourceDescriptor {
                        id,
                        name,
                        description: hook_description(&labels, &definition),
                        location,
                        source,
                        available: true,
                        workspace_id: workspace_id.map(str::to_owned),
                    },
                    definition,
                });
            }
        }
    }
    entries
}

fn parse_hook_event(value: &str) -> Option<HookEvent> {
    match value {
        "SessionStart" => Some(HookEvent::SessionStart),
        "InstructionsLoaded" => Some(HookEvent::InstructionsLoaded),
        "UserPromptSubmit" => Some(HookEvent::UserPromptSubmit),
        "PreToolUse" => Some(HookEvent::PreToolUse),
        "PermissionRequest" => Some(HookEvent::PermissionRequest),
        "PostToolUse" => Some(HookEvent::PostToolUse),
        "Stop" => Some(HookEvent::Stop),
        _ => None,
    }
}

/// The profile key naming a hook event.
pub fn hook_event_key(event: HookEvent) -> PromptKey {
    match event {
        HookEvent::SessionStart => PromptKey::SystemHookEventSessionStart,
        HookEvent::InstructionsLoaded => PromptKey::SystemHookEventInstructionsLoaded,
        HookEvent::UserPromptSubmit => PromptKey::SystemHookEventUserPromptSubmit,
        HookEvent::PreToolUse => PromptKey::SystemHookEventPreToolUse,
        HookEvent::PermissionRequest => PromptKey::SystemHookEventPermissionRequest,
        HookEvent::PostToolUse => PromptKey::SystemHookEventPostToolUse,
        HookEvent::Stop => PromptKey::SystemHookEventStop,
    }
}

/// The model-facing description of a hook: its event, plus its matcher when it
/// has one, in the profile's words.
fn hook_description(profile: &PromptProfile, definition: &HookDefinition) -> String {
    let mut description = profile.text(hook_event_key(definition.event)).to_owned();
    if let Some(matcher) = definition.matcher.as_deref() {
        description
            .push_str(&profile.render(PromptKey::SystemHookMatcherDetail, &[("matcher", matcher)]));
    }
    description
}

fn extend_unique(target: &mut Vec<String>, values: &[String]) {
    for value in values {
        if !target.iter().any(|existing| existing == value) {
            target.push(value.clone());
        }
    }
}

/// Reads a skill body. Symlinks and reparse points are rejected: following one would
/// delegate the catalog snapshot's trust boundary to the link owner.
fn read_skill_body(descriptor: &ResourceDescriptor) -> Result<String, String> {
    let path = Path::new(&descriptor.location);
    let bytes = memory_archive_file::read_bounded_nofollow_labeled(
        path,
        SKILL_READ_LIMIT as usize,
        "skill",
    )
    .map_err(|error| format!("Could not read skill {}: {error}", descriptor.name))?;
    String::from_utf8(bytes).map_err(|_| {
        format!(
            "Skill {}'s SKILL.md is not valid UTF-8 text",
            descriptor.name
        )
    })
}

/// Metadata for `SKILL.md`. Frontmatter takes precedence; missing fields are inferred
/// from the first heading and paragraph in the body.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SkillMetadata {
    pub name: String,
    pub description: String,
    pub author: String,
    pub version: String,
    pub tags: Vec<String>,
}

/// Parses a `SKILL.md` body.
///
/// Installation and catalog display share this parser so skill names cannot diverge.
/// The model-facing parser uses the same scan with [`SKILL_TRIGGER_LIMIT`].
///
/// This is intentionally not a YAML parser: it accepts only flat `key: value` pairs;
/// tags may be `[a, b]` or `a, b`, and unknown keys are ignored.
pub fn skill_metadata_from_source(source: &str, fallback_name: &str) -> SkillMetadata {
    let lines = source.lines().collect::<Vec<_>>();
    let parsed = scan_skill_frontmatter(&lines);
    let capped = |value: String| truncate_chars(&value, METADATA_VALUE_LIMIT);

    SkillMetadata {
        name: parsed
            .name
            .or_else(|| inferred_skill_name(&lines, parsed.body_start))
            .map(capped)
            .unwrap_or_else(|| fallback_name.to_owned()),
        description: parsed
            .description
            .or_else(|| inferred_skill_description(&lines, parsed.body_start))
            .map(capped)
            .unwrap_or_default(),
        author: parsed.author.map(capped).unwrap_or_default(),
        version: parsed.version.map(capped).unwrap_or_default(),
        tags: parsed.tags,
    }
}

/// The two model-facing parts of a `SKILL.md`.
///
/// They are a second view of the text used by [`SkillMetadata`]. Catalog metadata and
/// model context have different length limits and frontmatter requirements.
pub struct SkillDocument {
    /// Trigger text: frontmatter `description`, optionally followed by
    /// `{description} - {when_to_use}`. If both fields are absent, infer the first
    /// body paragraph using the catalog rule.
    pub trigger: String,
    /// The body with frontmatter removed.
    ///
    /// Frontmatter indexes installation and catalog display, not model instructions;
    /// `name:`, `version:`, and `tags:` do not belong in model context.
    pub body: String,
}

/// Parses `SKILL.md` using the model-facing view. See [`SkillDocument`].
pub fn skill_document_from_source(source: &str) -> SkillDocument {
    let lines = source.lines().collect::<Vec<_>>();
    let parsed = scan_skill_frontmatter(&lines);
    let description = parsed
        .description
        .or_else(|| inferred_skill_description(&lines, parsed.body_start))
        .unwrap_or_default();
    // `when_to_use` supplements `description`; it replaces it only when the
    // description is absent.
    let trigger = match parsed.when_to_use {
        Some(when) if !description.is_empty() => format!("{description} - {when}"),
        Some(when) => when,
        None => description,
    };
    SkillDocument {
        trigger: truncate_chars(trigger.trim(), SKILL_TRIGGER_LIMIT),
        body: lines[parsed.body_start..].join("\n").trim().to_owned(),
    }
}

/// Untruncated frontmatter values and the body start line.
///
/// Do not truncate during scanning: metadata and trigger consumers have different
/// limits, and scan-time truncation would impose the stricter limit on both.
#[derive(Default)]
struct SkillFrontmatter {
    name: Option<String>,
    description: Option<String>,
    when_to_use: Option<String>,
    author: Option<String>,
    version: Option<String>,
    /// Tags are consumed only by catalog display, so truncate them here.
    tags: Vec<String>,
    body_start: usize,
}

fn scan_skill_frontmatter(lines: &[&str]) -> SkillFrontmatter {
    let mut parsed = SkillFrontmatter::default();
    if !lines.first().is_some_and(|line| line.trim() == "---") {
        return parsed;
    }
    let Some(frontmatter_end) = lines
        .iter()
        .enumerate()
        .skip(1)
        .find_map(|(index, line)| (line.trim() == "---").then_some(index))
    else {
        return parsed;
    };
    for line in &lines[1..frontmatter_end] {
        if let Some(value) = line.strip_prefix("name:") {
            parsed.name = raw_metadata_value(value);
        } else if let Some(value) = line.strip_prefix("description:") {
            parsed.description = raw_metadata_value(value);
        } else if let Some(value) = line.strip_prefix("when_to_use:") {
            parsed.when_to_use = raw_metadata_value(value);
        } else if let Some(value) = line.strip_prefix("author:") {
            parsed.author = raw_metadata_value(value);
        } else if let Some(value) = line.strip_prefix("version:") {
            parsed.version = raw_metadata_value(value);
        } else if let Some(value) = line.strip_prefix("tags:") {
            parsed.tags = parse_metadata_tags(value);
        }
    }
    parsed.body_start = frontmatter_end + 1;
    parsed
}

fn inferred_skill_name(lines: &[&str], body_start: usize) -> Option<String> {
    lines[body_start..]
        .iter()
        .find_map(|line| line.trim().strip_prefix("# "))
        .and_then(raw_metadata_value)
}

fn inferred_skill_description(lines: &[&str], body_start: usize) -> Option<String> {
    lines[body_start..]
        .iter()
        .map(|line| line.trim())
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
}

fn parse_metadata_tags(value: &str) -> Vec<String> {
    let trimmed = value.trim().trim_start_matches('[').trim_end_matches(']');
    trimmed
        .split(',')
        .filter_map(nonempty_metadata_value)
        .take(16)
        .collect()
}

/// Trim whitespace and paired quotes without truncating.
fn raw_metadata_value(value: &str) -> Option<String> {
    let value = value.trim().trim_matches(['\'', '"']);
    (!value.is_empty()).then(|| value.to_owned())
}

fn nonempty_metadata_value(value: &str) -> Option<String> {
    raw_metadata_value(value).map(|value| truncate_chars(&value, METADATA_VALUE_LIMIT))
}

fn truncate_chars(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let result = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        format!("{result}…")
    } else {
        result
    }
}

fn preferred_config_path(base: &Path, relative: &Path) -> PathBuf {
    let primary = base.join(CONFIG_DIRECTORY).join(relative);
    if primary.exists() {
        primary
    } else {
        base.join(LEGACY_CONFIG_DIRECTORY).join(relative)
    }
}

/// Parsed tool-description file (prompt profile): display name, per-tool
/// entries and prompt overrides.
///
/// Unknown fields do not appear here because the application never writes these files;
/// their on-disk source remains authoritative.
pub struct ToolDescriptionDocument {
    pub name: String,
    pub entries: Vec<ToolDescriptionEntry>,
    /// `prompts` overrides keyed by injection point; unknown ids are dropped.
    pub prompts: HashMap<PromptKey, String>,
}

const TOOL_DESCRIPTION_FILE_LABEL: &str = "工具描述";
const MAX_TOOL_DESCRIPTION_NAME_CHARS: usize = 120;

fn tool_description_scope(source: ResourceSource) -> &'static str {
    match source {
        ResourceSource::User => "tooldesc_user",
        ResourceSource::Workspace => "tooldesc_workspace",
        ResourceSource::Builtin => "tooldesc_builtin",
    }
}

/// Parses a tool-description JSON file into entries. Accept either a top-level array
/// or `{"tools": [...]}`; fill missing fields with empty strings, drop entries with
/// both fields empty, and retain only the first occurrence of each name.
///
/// A name is reserved only once an entry actually carries an override. The
/// authoring scaffold ships a blank row per tool, so reserving names before the
/// blank check would let those rows shadow every populated row a user appends
/// after them — the file would parse, be selectable, and change nothing.
///
/// The editable built-in profiles use the same `tools` shape, so they parse
/// through this function rather than a second one that could drift from it.
pub(crate) fn parse_tool_description_entries(value: &Value) -> Vec<ToolDescriptionEntry> {
    let items = value
        .get("tools")
        .and_then(Value::as_array)
        .or_else(|| value.as_array());
    let Some(items) = items else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for item in items {
        let Some(record) = item.as_object() else {
            continue;
        };
        let tool_name = record
            .get("toolName")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned();
        if tool_name.is_empty() {
            continue;
        }
        let schema_notes = record
            .get("schemaNotes")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let usage_guidance = record
            .get("usageGuidance")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if schema_notes.trim().is_empty() && usage_guidance.trim().is_empty() {
            continue;
        }
        if !seen.insert(tool_name.clone()) {
            continue;
        }
        entries.push(ToolDescriptionEntry {
            tool_name,
            schema_notes,
            usage_guidance,
        });
    }
    entries
}

/// Reads a tool-description file. Symlinks and reparse points are rejected so an
/// entry that can be selected is also eligible for the matching no-follow save path.
fn read_tool_description_document(
    path: &Path,
    maximum_bytes: usize,
) -> Result<ToolDescriptionDocument, String> {
    let bytes = memory_archive_file::read_bounded_nofollow_labeled(
        path,
        maximum_bytes,
        TOOL_DESCRIPTION_FILE_LABEL,
    )?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| format!("{TOOL_DESCRIPTION_FILE_LABEL}文件不是有效的 JSON"))?;
    let fallback_name = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "tool-descriptions".to_owned());
    // The display name comes from content, while `stable_id` hashes the location.
    // Renaming content preserves selections; moving a file intentionally changes its ID.
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| truncate_chars(name, MAX_TOOL_DESCRIPTION_NAME_CHARS))
        .unwrap_or(fallback_name);
    let entries = parse_tool_description_entries(&value);
    let prompts = prompt_profile::parse_prompt_overrides(&value);
    Ok(ToolDescriptionDocument {
        name,
        entries,
        prompts,
    })
}

/// Whether a `tools[]` entry can reach anything at run time.
///
/// A built-in tool is addressed by its exact catalog name; an MCP tool by the
/// `mcp__server__tool` name the model sees. Anything else — a retired name, a
/// display label, the wrong case — parses fine and then matches nothing, so the
/// catalog entry says so rather than counting it as an override that works.
fn tool_description_entry_is_addressable(tool_name: &str) -> bool {
    let name = tool_name.trim();
    PromptKey::for_tool_description(name).is_some() || name.starts_with("mcp__")
}

fn tool_description_descriptor(
    path: &Path,
    source: ResourceSource,
    workspace_id: Option<&str>,
    location: String,
) -> ResourceDescriptor {
    let document = read_tool_description_document(path, METADATA_READ_LIMIT as usize).ok();
    let fallback_name = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "tool-descriptions".to_owned());
    let (tool_count, unmatched, prompt_count) = document
        .as_ref()
        .map(|document| {
            let unmatched = document
                .entries
                .iter()
                .filter(|entry| !tool_description_entry_is_addressable(&entry.tool_name))
                .count();
            (document.entries.len(), unmatched, document.prompts.len())
        })
        .unwrap_or((0, 0, 0));
    ResourceDescriptor {
        // Hash the file stem, not the display name, so editing the name preserves its ID.
        id: stable_id(tool_description_scope(source), &fallback_name, &location),
        name: document
            .as_ref()
            .map(|document| document.name.clone())
            .unwrap_or(fallback_name),
        description: if tool_count == 0 && prompt_count == 0 {
            "未解析出可用条目".to_owned()
        } else if unmatched > 0 {
            format!(
                "{tool_count} 个工具描述（{unmatched} 个工具名无法匹配，不会生效） · {prompt_count} 条提示词覆盖"
            )
        } else {
            format!("{tool_count} 个工具描述 · {prompt_count} 条提示词覆盖")
        },
        location,
        source,
        available: tool_count > 0 || prompt_count > 0,
        workspace_id: workspace_id.map(str::to_owned),
    }
}

/// Scans all `*.json` files in a `tool-descriptions/` directory. Each file is one
/// complete tool-description table, parallel to a skill directory or MCP entry.
fn scan_tool_description_root(
    root: &Path,
    source: ResourceSource,
    workspace_id: Option<&str>,
    output: &mut Vec<ResourceDescriptor>,
    seen_locations: &mut HashSet<String>,
) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // Use `symlink_metadata`, not `is_file`: the latter follows links and would
        // list an entry that the write path must later reject.
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.is_file()
            || !path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
        {
            continue;
        }
        let location = path.to_string_lossy().into_owned();
        if !seen_locations.insert(location.clone()) {
            continue;
        }
        output.push(tool_description_descriptor(
            &path,
            source,
            workspace_id,
            location,
        ));
    }
}

/// Resolves the conversation's selected tool-description file into the prompt
/// profile the run renders with.
///
/// No selection, the built-in English id, a dangling id, or an unreadable file
/// all resolve to the built-in English profile — the one that is always
/// present. A selected file declares no language of its own, so it follows the
/// application language, which also picks the built-in that fills in whatever
/// the file does not override.
///
/// Both built-ins render from their editable copy under `app_data`, so a user
/// who changed a text there sees it in the next run without a rebuild.
pub fn resolve_prompt_profile(
    document: &AppDocument,
    conversation: &Conversation,
    app_data: &Path,
) -> PromptProfile {
    let app_language = document.global_settings.resolved_app_language;
    let Some(selected) = conversation
        .settings
        .tool_description_file_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    else {
        return prompt_profile_files::load_builtin_profile(app_data, ResolvedLanguage::EnUs);
    };
    if let Some(builtin) = PromptProfile::builtin_for_id(selected) {
        return prompt_profile_files::load_builtin_profile(app_data, builtin.language);
    }
    discover_tool_description_files(document, app_data)
        .iter()
        .find(|descriptor| descriptor.id == selected)
        .and_then(|descriptor| {
            read_tool_description_document(
                Path::new(&descriptor.location),
                METADATA_READ_LIMIT as usize,
            )
            .ok()
            .map(|file| {
                PromptProfile::from_file(
                    descriptor.id.clone(),
                    file.name,
                    app_language,
                    file.prompts,
                    file.entries,
                )
            })
        })
        .unwrap_or_else(|| {
            prompt_profile_files::load_builtin_profile(app_data, ResolvedLanguage::EnUs)
        })
}

/*
 * Tool-description files are read-only. The application does not create roots,
 * resolve IDs to write paths, write, or delete files; it discovers, selects, and
 * rereads them from `.mework/tool-descriptions/` for trusted requests.
 */

/// Windows paths are case-insensitive and treat `\` and `/` interchangeably. Fold
/// both degrees of freedom before hashing so one resource retains its ID; preserve
/// the original location string for display.
pub(crate) fn normalized_location_for_id(location: &str) -> String {
    location.replace('\\', "/").to_lowercase()
}

/// Hashes a location exactly as supplied. Callers choose raw locations when matching
/// legacy IDs and normalized locations when minting current IDs.
pub(crate) fn location_id_hash(location: &str) -> u32 {
    let mut hasher = DefaultHasher::new();
    location.hash(&mut hasher);
    hasher.finish() as u32
}

/// The id of a discovered resource: a prefix naming its kind and level, a slug
/// of its name for legibility, and a hash of where it was read from. Skills,
/// MCP servers, hooks and tool-description files all mint theirs here, so an
/// entry keeps its id across rescans as long as it stays where it is.
pub(crate) fn stable_id(prefix: &str, name: &str, location: &str) -> String {
    let slug = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_owned();
    let slug = if slug.is_empty() { "resource" } else { &slug };
    format!(
        "{prefix}_{slug}_{:08x}",
        location_id_hash(&normalized_location_for_id(location))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scan result assembled by hand: the catalog rows plus the hook
    /// definitions behind them. MCP launch configurations are filled in by the
    /// tests that dial.
    fn discovered(
        catalog: CapabilityCatalog,
        hooks: HashMap<String, HookDefinition>,
    ) -> DiscoveredCapabilities {
        DiscoveredCapabilities {
            catalog,
            hooks,
            mcp_servers: HashMap::new(),
        }
    }

    /// Windows can spell one path with different casing and separators; IDs must
    /// collapse both variations so references remain stable.
    #[test]
    fn stable_id_collapses_windows_path_casing_and_separators() {
        let base = stable_id("skill_user", "Demo", "C:/Users/dev/skills/demo/SKILL.md");
        for variant in [
            "c:/users/dev/skills/demo/skill.md",
            "C:\\Users\\dev\\skills\\demo\\SKILL.md",
            "c:\\USERS\\Dev\\Skills\\Demo\\Skill.MD",
        ] {
            assert_eq!(stable_id("skill_user", "Demo", variant), base);
        }
        // Distinct locations must still produce distinct IDs.
        assert_ne!(
            stable_id("skill_user", "Demo", "C:/Users/dev/skills/other/SKILL.md"),
            base
        );
        // Legacy-ID recognition requires a mixed-case raw hash to differ from the
        // normalized hash.
        assert_ne!(
            location_id_hash("C:\\Users\\dev\\skills\\demo\\SKILL.md"),
            location_id_hash(&normalized_location_for_id(
                "C:\\Users\\dev\\skills\\demo\\SKILL.md"
            ))
        );
    }

    #[test]
    fn metadata_prefers_frontmatter() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("SKILL.md");
        fs::write(
            &path,
            "---\nname: Example Skill\ndescription: Does useful things\n---\n# Ignored\nBody",
        )
        .unwrap();
        let source = fs::read_to_string(&path).unwrap();
        let metadata = skill_metadata_from_source(&source, "fallback");
        assert_eq!(metadata.name, "Example Skill");
        assert_eq!(metadata.description, "Does useful things");
    }

    /// Prompt profiles resolve from the selected resource and retain a file's tool entries.
    #[test]
    fn selected_prompt_profile_file_preserves_entries_and_overrides() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join(".mework").join("tool-descriptions");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("strict.json"),
            r#"{
              "name": "strict",
              "prompts": {"system.mcp_section": "Custom MCP section: {servers}"},
              "tools": [
                {"toolName": "ls", "schemaNotes": "Paths must be absolute.", "usageGuidance": ""},
                {"toolName": "read", "schemaNotes": "", "usageGuidance": "Search before reading."},
                {"toolName": "", "schemaNotes": "An empty name is dropped.", "usageGuidance": ""},
                {"toolName": "write", "schemaNotes": "  ", "usageGuidance": "  "}
              ]
            }"#,
        )
        .unwrap();

        let mut document = crate::catalog::default_document();
        for workspace in &mut document.workspaces {
            workspace.path = directory.path().to_string_lossy().into_owned();
        }
        let app_data = directory.path().join("app-data");
        let catalog = discover(&document, &app_data);
        assert_eq!(catalog.tool_description_files.len(), 3);
        assert_eq!(
            catalog.tool_description_files[0].id,
            prompt_profile::BUILTIN_EN_US_ID
        );
        assert_eq!(
            catalog.tool_description_files[1].id,
            prompt_profile::BUILTIN_ZH_CN_ID
        );
        // A built-in keeps its `builtin:` location and says where its texts are
        // edited, because that path is the only way to reach them.
        for (index, language) in [ResolvedLanguage::EnUs, ResolvedLanguage::ZhCn]
            .into_iter()
            .enumerate()
        {
            assert!(catalog.tool_description_files[index].description.contains(
                &prompt_profile_files::builtin_profile_path(&app_data, language)
                    .to_string_lossy()
                    .into_owned()
            ));
        }
        assert_eq!(catalog.tool_description_files[2].name, "strict");
        let found = &catalog.tool_description_files[2];
        assert_eq!(found.description, "2 个工具描述 · 1 条提示词覆盖");
        assert!(found.available);

        document.workspaces[0].conversations[0]
            .settings
            .tool_description_file_id = Some(found.id.clone());
        let profile = resolve_prompt_profile(
            &document,
            &document.workspaces[0].conversations[0],
            &app_data,
        );
        assert_eq!(profile.id, found.id);
        // A file declares no language of its own, so it takes the application
        // language — which is also the built-in that fills the keys it omits.
        assert_eq!(
            document.global_settings.resolved_app_language,
            ResolvedLanguage::ZhCn
        );
        assert_eq!(profile.language, ResolvedLanguage::ZhCn);
        assert_eq!(profile.tools.len(), 2);
        assert_eq!(profile.tools[0].tool_name, "ls");
        assert_eq!(profile.tools[0].schema_notes, "Paths must be absolute.");
        assert_eq!(profile.tools[1].tool_name, "read");
        assert_eq!(profile.tools[1].usage_guidance, "Search before reading.");
        assert_eq!(
            profile.text(PromptKey::SystemMcpSection),
            "Custom MCP section: {servers}"
        );
        assert_eq!(
            profile.text(PromptKey::SystemHooksSection),
            PromptProfile::builtin_chinese().text(PromptKey::SystemHooksSection)
        );

        // Switching the application language switches the fill-in base with it;
        // the file's own overrides are untouched.
        document.global_settings.resolved_app_language = ResolvedLanguage::EnUs;
        let profile = resolve_prompt_profile(
            &document,
            &document.workspaces[0].conversations[0],
            &app_data,
        );
        assert_eq!(profile.language, ResolvedLanguage::EnUs);
        assert_eq!(
            profile.text(PromptKey::SystemMcpSection),
            "Custom MCP section: {servers}"
        );
        assert_eq!(
            profile.text(PromptKey::SystemHooksSection),
            PromptKey::SystemHooksSection.builtin_en()
        );
    }

    #[test]
    fn prompt_profile_defaults_to_english_and_resolves_builtins_or_dangling_ids() {
        let directory = tempfile::tempdir().unwrap();
        let app_data = directory.path().join("app-data");
        let mut document = crate::catalog::default_document();
        let resolve = |document: &AppDocument| {
            resolve_prompt_profile(
                document,
                &document.workspaces[0].conversations[0],
                &app_data,
            )
        };
        let profile = resolve(&document);
        assert_eq!(profile.id, prompt_profile::BUILTIN_EN_US_ID);
        assert_eq!(profile.language, ResolvedLanguage::EnUs);

        document.workspaces[0].conversations[0]
            .settings
            .tool_description_file_id = Some(prompt_profile::BUILTIN_ZH_CN_ID.to_owned());
        let profile = resolve(&document);
        assert_eq!(profile.id, prompt_profile::BUILTIN_ZH_CN_ID);
        assert_eq!(profile.language, ResolvedLanguage::ZhCn);

        document.workspaces[0].conversations[0]
            .settings
            .tool_description_file_id = Some("missing-profile".to_owned());
        let profile = resolve(&document);
        assert_eq!(profile.id, prompt_profile::BUILTIN_EN_US_ID);
        assert_eq!(profile.language, ResolvedLanguage::EnUs);

        // A built-in id renders from the editable copy on disk: the id and the
        // language stay the built-in's, the texts are the file's.
        let path = prompt_profile_files::builtin_profile_path(&app_data, ResolvedLanguage::ZhCn);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"name":"edited","prompts":{"system.capability_row":"ROW {name} {description}"},"tools":[]}"#,
        )
        .unwrap();
        document.workspaces[0].conversations[0]
            .settings
            .tool_description_file_id = Some(prompt_profile::BUILTIN_ZH_CN_ID.to_owned());
        let profile = resolve(&document);
        assert_eq!(profile.id, prompt_profile::BUILTIN_ZH_CN_ID);
        assert_eq!(profile.language, ResolvedLanguage::ZhCn);
        assert_eq!(
            profile.text(PromptKey::SystemCapabilityRow),
            "ROW {name} {description}"
        );
    }

    /// A document with no user or workspace skills must discover no skills and must
    /// not fall back to bundled entries.
    #[test]
    fn discovery_ships_no_builtin_skills() {
        let directory = tempfile::tempdir().unwrap();
        let mut document = crate::catalog::default_document();
        for workspace in &mut document.workspaces {
            workspace.path = directory.path().to_string_lossy().into_owned();
        }
        let conversation = &document.workspaces[0].conversations[0];

        let catalog = discover(&document, &directory.path().join("app-data"));
        assert!(catalog
            .skills
            .iter()
            .all(|skill| skill.source != ResourceSource::Builtin));

        let hook_definitions = HashMap::new();
        let profile = PromptProfile::builtin_english();
        let addendum = runtime_context_from_discovery(
            conversation,
            &discovered(catalog.clone(), hook_definitions.clone()),
            &profile,
        )
        .unwrap()
        .addendum;
        assert!(!addendum.contains("capability installation guide"));
    }

    #[test]
    fn selected_workspace_skill_mcp_and_hook_are_assembled_in_profile_words() {
        let directory = tempfile::tempdir().unwrap();
        let skill_path = directory.path().join("SKILL.md");
        fs::write(&skill_path, "# Probe skill\n\nMEWORK_SKILL_BODY_E2E").unwrap();
        let mut document = crate::catalog::default_document();
        document.workspaces[0].conversations[0].settings.skill_ids = vec!["skill-probe".into()];
        document.workspaces[0].conversations[0].settings.mcp_ids = vec!["mcp-probe".into()];
        document.workspaces[0].conversations[0].settings.hook_ids = vec!["hook-probe".into()];
        let catalog = CapabilityCatalog {
            lsps: Vec::new(),
            hooks: vec![ResourceDescriptor {
                id: "hook-probe".into(),
                name: "Probe Hook".into(),
                description: "display-only hook description".into(),
                location: "~/.naiword/hooks.json#/hooks/probe".into(),
                source: ResourceSource::User,
                available: true,
                workspace_id: None,
            }],
            skills: vec![ResourceDescriptor {
                id: "skill-probe".into(),
                name: "Probe Skill".into(),
                description: "test skill".into(),
                location: skill_path.to_string_lossy().into_owned(),
                source: ResourceSource::Workspace,
                available: true,
                workspace_id: None,
            }],
            mcps: vec![ResourceDescriptor {
                id: "mcp-probe".into(),
                name: "Probe MCP".into(),
                description: "stdio test server".into(),
                location: "~/.naiword/mcp.json#Probe MCP".into(),
                source: ResourceSource::User,
                available: true,
                workspace_id: None,
            }],
            tool_description_files: Vec::new(),
        };
        let hook_definitions = HashMap::from([(
            "hook-probe".to_owned(),
            HookDefinition {
                id: "hook-probe".into(),
                name: "Probe Hook".into(),
                event: HookEvent::PreToolUse,
                matcher: None,
                command: "npm test".into(),
                command_windows: None,
                status_message: None,
                enabled: true,
                timeout_ms: 30_000,
            },
        )]);
        let conversation = &document.workspaces[0].conversations[0];
        let english = PromptProfile::builtin_english();
        let scan = || DiscoveredCapabilities {
            mcp_servers: HashMap::from([(
                "mcp-probe".to_owned(),
                McpServerConfig {
                    id: "mcp-probe".into(),
                    name: "Probe MCP".into(),
                    description: "stdio test server".into(),
                    command: "node".into(),
                    ..Default::default()
                },
            )]),
            ..discovered(catalog.clone(), hook_definitions.clone())
        };

        let context = runtime_context_from_discovery(conversation, &scan(), &english).unwrap();
        let addendum = context.addendum;

        let skill_position = addendum.find("MEWORK_SKILL_BODY_E2E").unwrap();
        let mcp_position = addendum.find("## Selected MCP servers").unwrap();
        let hook_position = addendum.find("## Lifecycle hooks").unwrap();
        assert!(skill_position < mcp_position && mcp_position < hook_position);
        assert!(addendum.contains("- Probe MCP: stdio test server"));
        assert!(addendum.contains("- Probe Hook: Before a tool runs"));
        assert!(addendum
            .contains("Their tools can be called only when the host exposed them to this turn"));
        assert!(!addendum.contains("npm test"));
        // The same scan that wrote the section is what gets dialed.
        assert_eq!(context.mcp_servers.len(), 1);
        assert_eq!(context.mcp_servers[0].server_id, "mcp-probe");

        let chinese = PromptProfile::builtin_chinese();
        let chinese_addendum = runtime_context_from_discovery(conversation, &scan(), &chinese)
            .unwrap()
            .addendum;
        assert!(chinese_addendum.contains("## 已选择的 MCP Server"));
        assert!(chinese_addendum.contains("## 生命周期钩子"));
        assert!(chinese_addendum.contains("- Probe Hook：工具执行前"));
    }

    /// Include only the skill body.
    ///
    /// The body supplies its own heading; frontmatter is consumed by installation and
    /// catalog display, not by model instructions.
    #[test]
    fn a_skill_body_reaches_the_prompt_without_its_heading_or_frontmatter() {
        let directory = tempfile::tempdir().unwrap();
        let skill_path = directory.path().join(SKILL_MANIFEST);
        fs::write(
            &skill_path,
            "---\nname: Probe Skill\ndescription: Use when probing\nversion: 1.2.3\ntags: [a, b]\n---\n\n# Probe skill\n\nMEWORK_SKILL_BODY_E2E\n",
        )
        .unwrap();
        let (document, catalog) = skill_only_fixture(&skill_path);
        let conversation = &document.workspaces[0].conversations[0];

        let hook_definitions = HashMap::new();
        let profile = PromptProfile::builtin_english();
        let context = runtime_context_from_discovery(
            conversation,
            &discovered(catalog.clone(), hook_definitions.clone()),
            &profile,
        )
        .unwrap();

        assert_eq!(context.addendum, "# Probe skill\n\nMEWORK_SKILL_BODY_E2E");
        // With the skill tool disabled, this output must be empty to avoid duplicating bodies.
        assert!(context.skills.is_empty());
    }

    /// With the tool enabled, skill bodies leave the system prompt while the
    /// name-and-trigger listing takes their place there and the directory
    /// reaches the tool output.
    #[test]
    fn the_skill_tool_takes_the_body_out_of_the_prompt_and_carries_trigger_and_directory() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("probe-skill");
        fs::create_dir_all(&directory).unwrap();
        let skill_path = directory.join(SKILL_MANIFEST);
        fs::write(
            &skill_path,
            "---\nname: Probe Skill\ndescription: Use when probing\nwhen_to_use: the user says probe\n---\n\nMEWORK_SKILL_BODY_E2E\n",
        )
        .unwrap();
        let (mut document, catalog) = skill_only_fixture(&skill_path);
        document.workspaces[0].conversations[0]
            .settings
            .skill_tool_enabled = true;
        let conversation = &document.workspaces[0].conversations[0];

        let hook_definitions = HashMap::new();
        let profile = PromptProfile::builtin_english();
        let context = runtime_context_from_discovery(
            conversation,
            &discovered(catalog.clone(), hook_definitions.clone()),
            &profile,
        )
        .unwrap();

        // The listing lives in the prompt rather than in the tool's schema, so
        // selecting another skill later never redeclares the tool.
        assert_eq!(
            context.addendum,
            "Available skills:\n- probe-skill: Use when probing - the user says probe"
        );
        // Nothing arrived after the opening prompt, so nothing is delivered as
        // its own message.
        assert!(context.added_skills.is_empty());
        assert!(!context.addendum.contains("MEWORK_SKILL_BODY_E2E"));
        assert_eq!(context.skills.len(), 1);
        let skill = &context.skills[0];
        // The directory is the name the model addresses, as in Claude Code;
        // the frontmatter name is the label the catalog shows.
        assert_eq!(skill.name, "probe-skill");
        // `when_to_use` supplements the description rather than replacing it.
        assert_eq!(skill.trigger, "Use when probing - the user says probe");
        assert_eq!(skill.body, "MEWORK_SKILL_BODY_E2E");
        assert_eq!(skill.directory, directory.to_string_lossy());
    }

    /// A conversation whose prompt is already open delivers a newly selected
    /// skill as its own message instead of rewriting that prompt — in both
    /// delivery modes, and in the form that mode calls for.
    #[test]
    fn a_skill_selected_after_the_opening_prompt_arrives_as_its_own_message() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("probe-skill");
        fs::create_dir_all(&directory).unwrap();
        let skill_path = directory.join(SKILL_MANIFEST);
        fs::write(
            &skill_path,
            "---\nname: Probe Skill\ndescription: Use when probing\n---\n\nMEWORK_SKILL_BODY_E2E\n",
        )
        .unwrap();
        let (mut document, catalog) = skill_only_fixture(&skill_path);
        // The first run opened this prompt with no skill at all, which is what
        // makes the one selected since an addition rather than part of it.
        document.workspaces[0].conversations[0].settings.tool_lock =
            Some(crate::model::ConversationToolLock {
                prompt_skill_ids: Some(Vec::new()),
                ..Default::default()
            });
        let profile = PromptProfile::builtin_english();
        let scan = || discovered(catalog.clone(), HashMap::new());

        let inline = runtime_context_from_discovery(
            &document.workspaces[0].conversations[0],
            &scan(),
            &profile,
        )
        .unwrap();
        // The prompt is untouched; the body arrives in the message instead.
        assert_eq!(inline.addendum, "");
        assert_eq!(inline.added_skills.len(), 1);
        assert_eq!(inline.added_skills[0].resource_id, "skill-probe");
        assert!(inline.added_skills[0]
            .content
            .contains("MEWORK_SKILL_BODY_E2E"));

        document.workspaces[0].conversations[0]
            .settings
            .skill_tool_enabled = true;
        let on_demand = runtime_context_from_discovery(
            &document.workspaces[0].conversations[0],
            &scan(),
            &profile,
        )
        .unwrap();
        assert_eq!(on_demand.addendum, "");
        assert_eq!(on_demand.added_skills.len(), 1);
        // On demand, only the trigger travels; the body stays behind the tool,
        // which still has to be able to serve it.
        assert!(!on_demand.added_skills[0]
            .content
            .contains("MEWORK_SKILL_BODY_E2E"));
        assert!(on_demand.added_skills[0]
            .content
            .contains("Use when probing"));
        assert_eq!(on_demand.skills.len(), 1);
        assert_eq!(on_demand.skills[0].body, "MEWORK_SKILL_BODY_E2E");
    }

    /// A conversation selecting one skill and a catalog containing only that skill.
    fn skill_only_fixture(skill_path: &Path) -> (AppDocument, CapabilityCatalog) {
        let mut document = crate::catalog::default_document();
        document.workspaces[0].conversations[0].settings.skill_ids = vec!["skill-probe".into()];
        let catalog = CapabilityCatalog {
            lsps: Vec::new(),
            hooks: Vec::new(),
            skills: vec![ResourceDescriptor {
                id: "skill-probe".into(),
                name: "Probe Skill".into(),
                description: "test skill".into(),
                location: skill_path.to_string_lossy().into_owned(),
                source: ResourceSource::Workspace,
                available: true,
                workspace_id: None,
            }],
            mcps: Vec::new(),
            tool_description_files: Vec::new(),
        };
        (document, catalog)
    }

    /// Two selected skills with the same directory name — one global, one in the
    /// workspace — must fail in tool mode because one enum value cannot select
    /// two skills. Prompt mode does not select by name and permits duplicates.
    #[test]
    fn two_selected_skills_sharing_a_name_are_refused_only_in_tool_mode() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory
            .path()
            .join("global")
            .join("deploy")
            .join(SKILL_MANIFEST);
        let second = directory
            .path()
            .join("project")
            .join("deploy")
            .join(SKILL_MANIFEST);
        for (path, marker) in [(&first, "BODY-A"), (&second, "BODY-B")] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(
                path,
                format!("---\nname: Deploy ({marker})\n---\n\n{marker}\n"),
            )
            .unwrap();
        }
        let mut document = crate::catalog::default_document();
        document.workspaces[0].conversations[0].settings.skill_ids =
            vec!["skill-a".into(), "skill-b".into()];
        let descriptor = |id: &str, path: &Path| ResourceDescriptor {
            id: id.into(),
            name: "Probe Skill".into(),
            description: "test skill".into(),
            location: path.to_string_lossy().into_owned(),
            source: ResourceSource::User,
            available: true,
            workspace_id: None,
        };
        let catalog = CapabilityCatalog {
            hooks: Vec::new(),
            skills: vec![
                descriptor("skill-a", &first),
                descriptor("skill-b", &second),
            ],
            mcps: Vec::new(),
            lsps: Vec::new(),
            tool_description_files: Vec::new(),
        };

        // Prompt mode permits duplicate names because both bodies reach the model.
        let hook_definitions = HashMap::new();
        let profile = PromptProfile::builtin_english();
        let concatenated = runtime_context_from_discovery(
            &document.workspaces[0].conversations[0],
            &discovered(catalog.clone(), hook_definitions.clone()),
            &profile,
        )
        .unwrap()
        .addendum;
        assert!(concatenated.contains("BODY-A") && concatenated.contains("BODY-B"));

        document.workspaces[0].conversations[0]
            .settings
            .skill_tool_enabled = true;
        let error = runtime_context_from_discovery(
            &document.workspaces[0].conversations[0],
            &discovered(catalog.clone(), hook_definitions.clone()),
            &profile,
        )
        .expect_err("duplicate skill directory names cannot be selected in tool mode");

        assert!(error.contains("\"deploy\""), "{error}");
    }

    /// A selected id the scan no longer finds is skipped: the folder was deleted,
    /// the entry left the file, or the id predates file discovery, and a tool
    /// lock may keep such an id selected forever. An entry the scan does find
    /// but cannot use fails closed with its reason, and a missing hook always does.
    #[test]
    fn dangling_skill_and_mcp_ids_are_skipped_but_unusable_and_missing_hook_ids_fail() {
        let directory = tempfile::tempdir().unwrap();
        let skill_path = directory.path().join("ok").join(SKILL_MANIFEST);
        fs::create_dir_all(skill_path.parent().unwrap()).unwrap();
        fs::write(&skill_path, "# Ok\n\nBODY-OK\n").unwrap();
        let mut document = crate::catalog::default_document();
        let settings = &mut document.workspaces[0].conversations[0].settings;
        settings.skill_ids = vec!["skill_gone".into(), "skill_ok".into()];
        settings.mcp_ids = vec!["mcp_server_legacy".into(), "mcp_ok".into()];
        let descriptor =
            |id: &str, name: &str, location: &str, available: bool| ResourceDescriptor {
                id: id.into(),
                name: name.into(),
                description: if available {
                    "fine".into()
                } else {
                    "Missing environment variables: TOKEN".into()
                },
                location: location.into(),
                source: ResourceSource::User,
                available,
                workspace_id: None,
            };
        let catalog = CapabilityCatalog {
            lsps: Vec::new(),
            hooks: Vec::new(),
            skills: vec![descriptor(
                "skill_ok",
                "Ok",
                &skill_path.to_string_lossy(),
                true,
            )],
            mcps: vec![
                descriptor("mcp_ok", "Ok server", "x#/mcpServers/ok", true),
                descriptor("mcp_broken", "Broken", "x#/mcpServers/broken", false),
            ],
            tool_description_files: Vec::new(),
        };
        let scan = DiscoveredCapabilities {
            mcp_servers: HashMap::from([(
                "mcp_ok".to_owned(),
                McpServerConfig {
                    id: "mcp_ok".into(),
                    name: "Ok server".into(),
                    command: "node".into(),
                    ..Default::default()
                },
            )]),
            ..discovered(catalog, HashMap::new())
        };
        let profile = PromptProfile::builtin_english();

        let context = runtime_context_from_discovery(
            &document.workspaces[0].conversations[0],
            &scan,
            &profile,
        )
        .unwrap();
        assert!(context.addendum.contains("BODY-OK"));
        assert!(context.addendum.contains("- Ok server: fine"));
        assert_eq!(context.mcp_servers.len(), 1);

        let settings = &mut document.workspaces[0].conversations[0].settings;
        settings.mcp_ids.push("mcp_broken".into());
        let error = runtime_context_from_discovery(
            &document.workspaces[0].conversations[0],
            &scan,
            &profile,
        )
        .unwrap_err();
        assert!(
            error.contains("Broken") && error.contains("TOKEN"),
            "{error}"
        );

        let settings = &mut document.workspaces[0].conversations[0].settings;
        settings.mcp_ids.pop();
        settings.hook_ids = vec!["hook_gone".into()];
        let error = runtime_context_from_discovery(
            &document.workspaces[0].conversations[0],
            &scan,
            &profile,
        )
        .unwrap_err();
        assert!(error.contains("hook_gone"), "{error}");
    }

    /// One workspace's `.mework` yields all three kinds, each tagged with the
    /// workspace, and a run of a conversation in another workspace sees none
    /// of them.
    #[test]
    fn a_workspace_level_yields_skills_servers_and_hooks_scoped_to_that_workspace() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join(".mework");
        fs::create_dir_all(config.join("skills").join("review")).unwrap();
        fs::write(
            config.join("skills").join("review").join(SKILL_MANIFEST),
            "---\nname: Review\ndescription: Review code\n---\n\nREVIEW-BODY\n",
        )
        .unwrap();
        fs::write(
            config.join("mcp.json"),
            r#"{"mcpServers":{"docs":{"command":"node","args":["docs.js"]},"legacy":{"type":"sse","url":"http://127.0.0.1:1/"}}}"#,
        )
        .unwrap();
        fs::write(
            config.join("hooks.json"),
            r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","name":"Tests","command":"npm test"}]}]}}"#,
        )
        .unwrap();
        let mut document = crate::catalog::default_document();
        document.workspaces[0].path = directory.path().to_string_lossy().into_owned();
        document.workspaces[0].id = "ws_project".into();
        let other = tempfile::tempdir().unwrap();
        let mut elsewhere = document.workspaces[0].clone();
        elsewhere.id = "ws_other".into();
        elsewhere.path = other.path().to_string_lossy().into_owned();
        elsewhere.conversations[0].id = "conv_other".into();
        document.workspaces.push(elsewhere);

        let level = ConfigLevel::workspace(&document.workspaces[0]).unwrap();
        let scan = discover_levels(std::slice::from_ref(&level), ResolvedLanguage::EnUs);
        assert_eq!(scan.catalog.skills.len(), 1);
        assert_eq!(scan.catalog.skills[0].name, "Review");
        assert!(scan.catalog.skills[0]
            .id
            .starts_with("skill_workspace_review_"));
        assert_eq!(scan.catalog.mcps.len(), 2);
        let docs = scan
            .catalog
            .mcps
            .iter()
            .find(|row| row.name == "docs")
            .unwrap();
        assert!(docs.available && scan.mcp_servers.contains_key(&docs.id));
        let legacy = scan
            .catalog
            .mcps
            .iter()
            .find(|row| row.name == "legacy")
            .unwrap();
        assert!(!legacy.available && !scan.mcp_servers.contains_key(&legacy.id));
        assert_eq!(scan.catalog.hooks.len(), 1);
        for row in scan
            .catalog
            .skills
            .iter()
            .chain(&scan.catalog.mcps)
            .chain(&scan.catalog.hooks)
        {
            assert_eq!(row.workspace_id.as_deref(), Some("ws_project"));
            assert_eq!(row.source, ResourceSource::Workspace);
        }

        // The conversation in the other workspace only gets the global level.
        let other_conversation = &document.workspaces.last().unwrap().conversations[0];
        assert_eq!(other_conversation.id, "conv_other");
        let levels = levels_for_conversation(&document, other_conversation);
        assert!(levels
            .iter()
            .all(|level| level.workspace_id.as_deref() != Some("ws_project")));
        let levels = levels_for_conversation(&document, &document.workspaces[0].conversations[0]);
        assert!(levels
            .iter()
            .any(|level| level.workspace_id.as_deref() == Some("ws_project")));
    }

    /// Deleting through the catalog reaches the folder or the file entry the
    /// scan read, and nothing else.
    #[test]
    fn discovered_skills_and_servers_can_be_deleted_where_they_were_read() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join(".mework");
        fs::create_dir_all(config.join("skills").join("gone")).unwrap();
        fs::write(
            config.join("skills").join("gone").join(SKILL_MANIFEST),
            "# Gone\n",
        )
        .unwrap();
        fs::write(
            config.join("mcp.json"),
            r#"{"mcpServers":{"keep":{"command":"node"},"drop":{"command":"node"}}}"#,
        )
        .unwrap();
        let mut document = crate::catalog::default_document();
        document.workspaces[0].path = directory.path().to_string_lossy().into_owned();
        let level = ConfigLevel::workspace(&document.workspaces[0]).unwrap();
        let scan = discover_levels(std::slice::from_ref(&level), ResolvedLanguage::EnUs);
        let skill_id = scan.catalog.skills[0].id.clone();
        let drop_id = scan
            .catalog
            .mcps
            .iter()
            .find(|row| row.name == "drop")
            .unwrap()
            .id
            .clone();

        delete_skill(&document, &skill_id).unwrap();
        assert!(!config.join("skills").join("gone").exists());
        assert!(delete_skill(&document, &skill_id).is_err());

        delete_mcp_server(&document, &drop_id).unwrap();
        let remaining = discover_levels(std::slice::from_ref(&level), ResolvedLanguage::EnUs);
        assert_eq!(remaining.catalog.mcps.len(), 1);
        assert_eq!(remaining.catalog.mcps[0].name, "keep");
        assert!(mcp_server_config(&document, &remaining.catalog.mcps[0].id).is_ok());
        assert!(mcp_server_config(&document, &drop_id).is_err());
    }

    /// Derive the tool from resolved skills, not merely from the enabled setting.
    #[test]
    fn the_skill_tool_is_derived_only_when_a_skill_actually_resolved() {
        let mut enabled = vec!["read".to_owned()];
        apply_skill_tool(&mut enabled, 2);
        assert_eq!(enabled, vec!["read".to_owned(), SKILL_TOOL.to_owned()]);

        // Remove stale persisted names so old data cannot re-enable the setting.
        apply_skill_tool(&mut enabled, 0);
        assert_eq!(enabled, vec!["read".to_owned()]);
    }

    #[test]
    fn hook_file_discovers_only_valid_external_definitions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("hooks.json");
        fs::write(
            &path,
            r#"{
              "hooks": {
                "Stop": [{
                  "hooks": [{
                    "type": "command",
                    "name": "结束后测试",
                    "command": "npm test",
                    "timeout": 120
                  }]
                }],
                "Unknown": [{"hooks":[{"type":"command","command":"echo no"}]}],
                "PreToolUse": [{"hooks":[{"type":"command","command":"  "}]}]
              }
            }"#,
        )
        .unwrap();

        let entries = read_hooks_file(&path, ResourceSource::User, None, ResolvedLanguage::EnUs);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].descriptor.name, "结束后测试");
        assert_eq!(entries[0].descriptor.description, "Before the turn stops");
        assert_eq!(entries[0].definition.command, "npm test");
        assert_eq!(entries[0].definition.timeout_ms, 120_000);
        let visible = serde_json::to_string(&entries[0].descriptor).unwrap();
        assert!(!visible.contains("npm test"));
    }

    #[test]
    fn instructions_loaded_is_discovered_as_forced_async_observability() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("hooks.json");
        fs::write(
            &path,
            r#"{
              "hooks": {
                "InstructionsLoaded": [{
                  "matcher": "^(session_start|include)$",
                  "hooks": [
                    {"type":"command","name":"Observe instructions","command":"observe","async":true},
                    {"type":"command","name":"Must not rewake","command":"rewake","asyncRewake":true}
                  ]
                }]
              }
            }"#,
        )
        .unwrap();

        let entries = read_hooks_file(&path, ResourceSource::User, None, ResolvedLanguage::EnUs);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].definition.event, HookEvent::InstructionsLoaded);
        assert_eq!(
            entries[0].definition.matcher.as_deref(),
            Some("^(session_start|include)$")
        );
        assert_eq!(entries[0].definition.command, "observe");
    }

    #[test]
    fn selected_workspace_hook_resolves_to_external_command() {
        let directory = tempfile::tempdir().unwrap();
        let config_dir = directory.path().join(".naiword");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join("hooks.json"),
            r#"{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","name":"Lint","command":"npm run lint"}]}]}}"#,
        )
        .unwrap();
        let mut document = crate::catalog::default_document();
        for workspace in &mut document.workspaces {
            workspace.path = directory.path().to_string_lossy().into_owned();
        }
        let discovered = discover(&document, &directory.path().join("app-data"));
        let hook_id = discovered
            .hooks
            .iter()
            .find(|hook| hook.source == ResourceSource::Workspace)
            .unwrap()
            .id
            .clone();
        document.workspaces[0].conversations[0].settings.hook_ids = vec![hook_id];

        let hooks = runtime_context(
            &document,
            &document.workspaces[0].conversations[0],
            &PromptProfile::builtin_english(),
        )
        .unwrap()
        .hooks;

        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0].event, HookEvent::UserPromptSubmit);
        assert_eq!(hooks[0].command, "npm run lint");
        assert!(hooks[0].enabled);
    }

    /// Two events; the first has two groups and one of them two handlers. Plus a
    /// top-level field that has nothing to do with hooks.
    fn hooks_fixture() -> Value {
        serde_json::json!({
            "version": 1,
            "hooks": {
                "Stop": [
                    {
                        "hooks": [
                            {"type": "command", "name": "stop-a", "command": "echo a"},
                            {"type": "command", "name": "stop-b", "command": "echo b"}
                        ]
                    },
                    {
                        "matcher": "Bash",
                        "hooks": [{"type": "command", "name": "bash-a", "command": "echo bash"}]
                    }
                ],
                "UserPromptSubmit": [
                    {"hooks": [{"type": "command", "name": "prompt-a", "command": "echo prompt"}]}
                ]
            }
        })
    }

    /// Every handler name in the file, sorted, so an assertion can state exactly
    /// which handlers a rewrite left behind.
    fn handler_names(document: &Value) -> Vec<String> {
        let mut names = Vec::new();
        if let Some(events) = document.get("hooks").and_then(Value::as_object) {
            for groups in events.values() {
                if let Some(groups) = groups.as_array() {
                    for group in groups {
                        if let Some(handlers) = group.get("hooks").and_then(Value::as_array) {
                            for handler in handlers {
                                if let Some(name) = handler.get("name").and_then(Value::as_str) {
                                    names.push(name.to_owned());
                                }
                            }
                        }
                    }
                }
            }
        }
        names.sort();
        names
    }

    fn write_hooks_fixture(directory: &tempfile::TempDir) -> PathBuf {
        let path = directory.path().join("hooks.json");
        fs::write(
            &path,
            serde_json::to_string_pretty(&hooks_fixture()).unwrap(),
        )
        .unwrap();
        path
    }

    #[test]
    fn hook_locations_parse_back_into_the_file_and_position_they_came_from() {
        let address =
            parse_hook_location(r"C:\Users\dev\.mework\hooks.json#/hooks/PostToolUse/2/hooks/1")
                .expect("这是 read_hooks_file 会写出的格式");
        assert_eq!(
            address.path,
            PathBuf::from(r"C:\Users\dev\.mework\hooks.json")
        );
        assert_eq!(address.event, "PostToolUse");
        assert_eq!(address.group_index, 2);
        assert_eq!(address.handler_index, 1);

        // The path is whatever the scan spelled, so a POSIX one round-trips too.
        let address = parse_hook_location("/home/dev/.mework/hooks.json#/hooks/Stop/0/hooks/0")
            .expect("斜杠路径同样是本应用写出的格式");
        assert_eq!(address.path, PathBuf::from("/home/dev/.mework/hooks.json"));
        assert_eq!(address.event, "Stop");
        assert_eq!(address.group_index, 0);
        assert_eq!(address.handler_index, 0);
    }

    #[test]
    fn anything_that_is_not_a_hook_location_resolves_to_nothing() {
        for location in [
            // An ordinary file entry, with no pointer at all.
            r"C:\Users\dev\.mework\hooks.json",
            // A pointer of some other shape.
            r"C:\Users\dev\.mework\hooks.json#/skills/0/hooks/1",
            // Both indices must be numbers.
            r"C:\Users\dev\.mework\hooks.json#/hooks/Stop/zero/hooks/1",
            r"C:\Users\dev\.mework\hooks.json#/hooks/Stop/0/hooks/one",
            // The segment between the two indices is always `hooks`.
            r"C:\Users\dev\.mework\hooks.json#/hooks/Stop/0/handlers/1",
            // Nothing may follow the handler index, and nothing may be missing.
            r"C:\Users\dev\.mework\hooks.json#/hooks/Stop/0/hooks/1/extra",
            r"C:\Users\dev\.mework\hooks.json#/hooks/Stop/0/hooks",
            // An event name is required.
            r"C:\Users\dev\.mework\hooks.json#/hooks//0/hooks/1",
        ] {
            assert!(
                parse_hook_location(location).is_none(),
                "{location} 不是本应用写出的地址，不该被解析成某个位置"
            );
        }
    }

    #[test]
    fn removing_one_handler_leaves_every_other_entry_and_field_alone() {
        let directory = tempfile::tempdir().unwrap();
        let path = write_hooks_fixture(&directory);
        let original = hooks_fixture();
        assert_eq!(handler_names(&original).len(), 4, "夹具里应有四个处理器");

        remove_hook_from_file(&HookAddress {
            path: path.clone(),
            event: "Stop".into(),
            group_index: 0,
            handler_index: 0,
        })
        .expect("删掉一个存在的处理器应当成功");

        // The expected file is the original with that one handler edited out, so
        // this compares everything: every other handler, every other group, and
        // every field that is not a hook.
        let mut expected = original.clone();
        expected["hooks"]["Stop"][0]["hooks"]
            .as_array_mut()
            .unwrap()
            .remove(0);
        let written: Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).expect("文件仍是合法 JSON");
        assert_eq!(written, expected, "除被删的那条外，文件其余部分必须原样");
        assert_eq!(written["version"], 1);
        assert_eq!(written["hooks"]["Stop"][1]["matcher"], "Bash");
        assert_eq!(handler_names(&written), ["bash-a", "prompt-a", "stop-b"]);
    }

    #[test]
    fn deleting_a_groups_last_handler_keeps_the_group_and_a_valid_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = write_hooks_fixture(&directory);

        remove_hook_from_file(&HookAddress {
            path: path.clone(),
            event: "UserPromptSubmit".into(),
            group_index: 0,
            handler_index: 0,
        })
        .expect("删掉组里最后一个处理器应当成功");

        let text = fs::read_to_string(&path).unwrap();
        let written: Value = serde_json::from_str(&text).expect("删除后文件仍是合法 JSON");
        assert_eq!(
            written["hooks"]["UserPromptSubmit"][0]["hooks"],
            serde_json::json!([]),
            "空组要留在文件里，而不是被剪掉"
        );
        assert_eq!(
            written["hooks"]["UserPromptSubmit"]
                .as_array()
                .unwrap()
                .len(),
            1,
            "组本身仍在"
        );
        assert_eq!(handler_names(&written), ["bash-a", "stop-a", "stop-b"]);
    }

    #[test]
    fn an_out_of_range_position_errors_without_touching_the_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = write_hooks_fixture(&directory);
        let before = fs::read_to_string(&path).unwrap();

        for address in [
            // One past the handlers of a group that exists.
            HookAddress {
                path: path.clone(),
                event: "Stop".into(),
                group_index: 0,
                handler_index: 2,
            },
            // A group that does not exist.
            HookAddress {
                path: path.clone(),
                event: "Stop".into(),
                group_index: 9,
                handler_index: 0,
            },
            // An event that does not exist.
            HookAddress {
                path: path.clone(),
                event: "SessionStart".into(),
                group_index: 0,
                handler_index: 0,
            },
        ] {
            assert!(
                remove_hook_from_file(&address).is_err(),
                "越界必须报错，而不是写坏文件"
            );
            assert_eq!(
                fs::read_to_string(&path).unwrap(),
                before,
                "报错时文件不能被改动"
            );
        }
    }

    #[test]
    fn a_discovered_hook_can_be_located_and_deleted_end_to_end() {
        let directory = tempfile::tempdir().unwrap();
        let path = write_hooks_fixture(&directory);

        let entries = read_hooks_file(&path, ResourceSource::User, None, ResolvedLanguage::EnUs);
        assert_eq!(entries.len(), 4, "夹具里四个可执行的处理器");
        let target = entries
            .iter()
            .find(|entry| entry.descriptor.name == "stop-a")
            .expect("夹具里应有 stop-a");
        let address = parse_hook_location(&target.descriptor.location)
            .expect("描述符的 location 必须能解析回地址");
        assert_eq!(address.path, path, "解析回来的路径必须是这个文件");
        assert_eq!(
            (
                address.event.as_str(),
                address.group_index,
                address.handler_index
            ),
            ("Stop", 0, 0)
        );

        remove_hook_from_file(&address).expect("删掉刚发现的钩子应当成功");

        let remaining = read_hooks_file(&path, ResourceSource::User, None, ResolvedLanguage::EnUs);
        assert_eq!(remaining.len(), entries.len() - 1);
        let mut names = remaining
            .iter()
            .map(|entry| entry.descriptor.name.as_str())
            .collect::<Vec<_>>();
        names.sort_unstable();
        assert_eq!(names, ["bash-a", "prompt-a", "stop-b"]);
    }
}
