use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    hash::{DefaultHasher, Hash, Hasher},
    path::{Path, PathBuf},
};

use serde_json::Value;

use crate::{
    memory_archive_file,
    model::{
        AppDocument, CapabilityCatalog, Conversation, HookDefinition, HookEvent, ResolvedLanguage,
        ResolvedSkill, ResourceDescriptor, ResourceSource, ToolDescriptionEntry,
    },
    prompt_profile::{self, PromptKey, PromptProfile},
};

const METADATA_READ_LIMIT: u64 = 64 * 1024;
const SKILL_READ_LIMIT: u64 = 256 * 1024;
const RUNTIME_CONTEXT_LIMIT: usize = 1024 * 1024;
/// Maximum metadata length for catalog display. Names, descriptions, authors,
/// versions, and tags share this limit.
const METADATA_VALUE_LIMIT: usize = 240;
/// Maximum trigger text supplied to the model. Trigger conditions must remain intact,
/// so this limit is larger than the catalog display limit.
const SKILL_TRIGGER_LIMIT: usize = 1024;
const CONFIG_DIRECTORY: &str = ".mework";
const LEGACY_CONFIG_DIRECTORY: &str = ".naiword";

/// Skill body file name. Installation normalizes it to uppercase; readers accept
/// only this name.
pub const SKILL_MANIFEST: &str = "SKILL.md";

/// Tool name for on-demand skill loading.
///
/// `catalog::tool_catalog` provides a localized model-facing descriptor and timeline
/// card, but this tool is derived from `skill_tool_enabled`, not user-selected. The
/// renderer mirror is `src/lib/skillTool.ts`.
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

/// Discovers the current capability catalog.
///
/// Skills and MCP servers are projections of the in-app registry, while hooks and
/// tool-description files are still discovered from disk because they have no
/// corresponding management page.
pub fn discover(document: &AppDocument, skills_root: &Path) -> CapabilityCatalog {
    CapabilityCatalog {
        hooks: discover_hook_entries(document)
            .into_iter()
            .map(|entry| entry.descriptor)
            .collect(),
        skills: installed_skill_descriptors(document, skills_root),
        mcps: mcp_server_descriptors(document),
        tool_description_files: discover_tool_description_files(document),
    }
}

/// Scans tool-description files separately because [`resolve_prompt_profile`]
/// needs only these files and must not require a skills root.
///
/// The two built-in profiles come first and are always present: the English
/// one is the default every conversation renders with until it selects
/// something else, and neither has a location a user could delete.
fn discover_tool_description_files(document: &AppDocument) -> Vec<ResourceDescriptor> {
    let mut files: Vec<ResourceDescriptor> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    if let Some(home) = dirs::home_dir() {
        scan_tool_description_root(
            &preferred_config_path(&home, Path::new("tool-descriptions")),
            ResourceSource::User,
            &mut files,
            &mut seen,
        );
    }

    for workspace in &document.workspaces {
        if workspace.path.trim().is_empty() {
            continue;
        }
        scan_tool_description_root(
            &preferred_config_path(
                &PathBuf::from(&workspace.path),
                Path::new("tool-descriptions"),
            ),
            ResourceSource::Workspace,
            &mut files,
            &mut seen,
        );
    }

    files.sort_by(resource_sort);
    let mut catalog = builtin_prompt_profile_descriptors();
    catalog.extend(files);
    catalog
}

/// The two compiled-in profiles as catalog entries. `location` is a
/// `builtin:` pseudo-location so the renderer can tell them from files.
fn builtin_prompt_profile_descriptors() -> Vec<ResourceDescriptor> {
    [
        PromptProfile::builtin_english(),
        PromptProfile::builtin_chinese(),
    ]
    .into_iter()
    .map(|profile| ResourceDescriptor {
        id: profile.id.clone(),
        name: profile.name.clone(),
        description: match profile.language {
            ResolvedLanguage::EnUs => "Built-in English prompts and tool descriptions".to_owned(),
            ResolvedLanguage::ZhCn => "内置中文提示词与工具描述".to_owned(),
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
    })
    .collect()
}

/// Catalog projection of installed skills.
///
/// Disabled skills remain listed as `available: false` so the preset editor can show
/// why a selection is inactive instead of silently removing it.
fn installed_skill_descriptors(
    document: &AppDocument,
    skills_root: &Path,
) -> Vec<ResourceDescriptor> {
    let mut descriptors = document
        .assets
        .skills
        .iter()
        .map(|skill| {
            let manifest = skills_root.join(&skill.folder_name).join(SKILL_MANIFEST);
            // Treat symlinked bodies as unavailable, following the same no-follow
            // policy as tool-description scanning.
            let installed =
                matches!(fs::symlink_metadata(&manifest), Ok(metadata) if metadata.is_file());
            ResourceDescriptor {
                id: skill.id.clone(),
                name: skill.name.clone(),
                description: skill.description.clone(),
                location: manifest.to_string_lossy().into_owned(),
                source: ResourceSource::User,
                available: skill.enabled && installed,
            }
        })
        .collect::<Vec<_>>();
    descriptors.sort_by(resource_sort);
    descriptors
}

/// Catalog projection of MCP servers. `location` is the server's dial target and is
/// display-only.
///
/// A server without a description gets the English default here; the system
/// prompt section re-derives the row from the profile
/// (`system.mcp_server_default_description`), so this text is UI-only.
fn mcp_server_descriptors(document: &AppDocument) -> Vec<ResourceDescriptor> {
    let mut descriptors = document
        .assets
        .mcp_servers
        .iter()
        .map(|server| ResourceDescriptor {
            id: server.id.clone(),
            name: server.name.clone(),
            description: if server.description.trim().is_empty() {
                PromptKey::SystemMcpServerDefaultDescription
                    .builtin_en()
                    .to_owned()
            } else {
                truncate_chars(server.description.trim(), 240)
            },
            location: match server.transport {
                crate::model::McpTransportKind::Stdio => {
                    if server.args.is_empty() {
                        server.command.clone()
                    } else {
                        format!("{} {}", server.command, server.args.join(" "))
                    }
                }
                crate::model::McpTransportKind::StreamableHttp => server.url.clone(),
            },
            source: ResourceSource::User,
            available: server.enabled,
        })
        .collect::<Vec<_>>();
    descriptors.sort_by(resource_sort);
    descriptors
}

/// Runtime context for this turn: the system-prompt addendum and skills supplied to
/// the `skill` tool.
///
/// These are mutually exclusive outputs for the same selected skills. When
/// `skill_tool_enabled` is off, bodies enter `addendum`; when on, only names and
/// triggers enter the tool schema and bodies are loaded on demand. MCP and hooks
/// always enter `addendum`.
#[derive(Debug)]
pub struct RuntimeContext {
    pub addendum: String,
    /// Non-empty only when the skill tool is enabled. Otherwise skill bodies are in
    /// `addendum`; using both outputs would duplicate them.
    pub skills: Vec<ResolvedSkill>,
}

/// Resolve the persisted conversation's selected capability presets into model instructions.
/// Renderer-provided resource text is never trusted: every descriptor is rediscovered from the
/// current filesystem/configuration and skill bodies are read with strict size limits.
///
/// `profile` supplies the wording of the MCP and hook sections; skill bodies
/// are user-authored and enter verbatim.
pub fn runtime_context(
    document: &AppDocument,
    conversation: &Conversation,
    skills_root: &Path,
    profile: &PromptProfile,
) -> Result<RuntimeContext, String> {
    let catalog = discover(document, skills_root);
    let hooks = discover_hook_entries(document)
        .into_iter()
        .map(|entry| (entry.descriptor.id, entry.definition))
        .collect::<HashMap<_, _>>();
    runtime_context_from_catalog(conversation, &catalog, &hooks, profile)
}

fn runtime_context_from_catalog(
    conversation: &Conversation,
    catalog: &CapabilityCatalog,
    hook_definitions: &HashMap<String, HookDefinition>,
    profile: &PromptProfile,
) -> Result<RuntimeContext, String> {
    let mut selected_hook_ids = Vec::new();
    let mut selected_skill_ids = Vec::new();
    let mut selected_mcp_ids = Vec::new();

    extend_unique(&mut selected_hook_ids, &conversation.settings.hook_ids);
    extend_unique(&mut selected_skill_ids, &conversation.settings.skill_ids);
    extend_unique(&mut selected_mcp_ids, &conversation.settings.mcp_ids);

    let via_tool = conversation.settings.skill_tool_enabled;
    let mut sections = Vec::new();
    let mut skills = Vec::new();
    for resource_id in selected_skill_ids {
        let descriptor = catalog
            .skills
            .iter()
            .find(|resource| resource.id == resource_id && resource.available)
            .ok_or_else(|| {
                format!(
                    "Skill {resource_id} is currently unavailable. Confirm it is installed and enabled in the Skills settings page."
                )
            })?;
        let parsed = skill_document_from_source(&read_skill_body(descriptor)?);
        if via_tool {
            // The model selects skills by name, so every name must identify exactly
            // one skill. Duplicate enum values make one skill unreachable.
            if skills
                .iter()
                .any(|other: &ResolvedSkill| other.name == descriptor.name)
            {
                return Err(format!(
                    "This conversation selected two skills named \"{}\". On-demand loading selects skills by name, so one would be unreachable; change a name in its SKILL.md or select only one.",
                    descriptor.name
                ));
            }
            skills.push(ResolvedSkill {
                name: descriptor.name.clone(),
                trigger: parsed.trigger,
                body: parsed.body,
                directory: skill_directory(descriptor),
            });
        } else if !parsed.body.is_empty() {
            // Include only the body. The heading and frontmatter are registration
            // metadata, while `---` below provides structure between skill bodies.
            sections.push(parsed.body);
        }
    }

    if !selected_mcp_ids.is_empty() {
        let mut servers = Vec::new();
        for resource_id in selected_mcp_ids {
            let descriptor = catalog
                .mcps
                .iter()
                .find(|resource| resource.id == resource_id && resource.available)
                .ok_or_else(|| {
                    format!(
                        "MCP server {resource_id} is currently unavailable. Confirm it is enabled in the MCP settings page."
                    )
                })?;
            // A server without a description carries the English default from
            // discovery; the model-facing row says it in the profile's words.
            let description = if descriptor.description
                == PromptKey::SystemMcpServerDefaultDescription.builtin_en()
            {
                profile.text(PromptKey::SystemMcpServerDefaultDescription)
            } else {
                descriptor.description.as_str()
            };
            servers.push(profile.render(
                PromptKey::SystemCapabilityRow,
                &[("name", &descriptor.name), ("description", description)],
            ));
        }
        sections.push(profile.render(
            PromptKey::SystemMcpSection,
            &[("servers", &servers.join("\n"))],
        ));
    }

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
                            "Hook {resource_id} is currently unavailable. Check hooks.json and scan again."
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        // The catalog description is UI text; the model-facing row is rendered
        // from the hook's event and matcher in the profile's words.
        let hooks = selected
            .iter()
            .map(|resource| {
                let description = hook_definitions
                    .get(&resource.id)
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
            &[("hook_names", &hook_names), ("hooks", &hooks.join("\n"))],
        ));
    }

    let addendum = sections.join("\n\n---\n\n");
    // Both outputs are independently measured. Tool-mode bodies do not enter the
    // system prompt, but they still reside in the request and share its size limit.
    let skill_bytes = skills.iter().map(|skill| skill.body.len()).sum::<usize>();
    if addendum.len() > RUNTIME_CONTEXT_LIMIT || skill_bytes > RUNTIME_CONTEXT_LIMIT {
        return Err(
            "The runtime context for enabled skills and hooks exceeds the 1 MiB limit.".into(),
        );
    }
    Ok(RuntimeContext { addendum, skills })
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

/// Resolve the conversation's selected hook resource IDs against current external hook files.
/// Executable commands never come from the renderer or the persisted application document.
/// Selection order is preserved and duplicates are dropped.
pub fn resolve_hooks(
    document: &AppDocument,
    conversation: &Conversation,
) -> Result<Vec<HookDefinition>, String> {
    if conversation.settings.hook_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut selected_resource_ids = Vec::new();
    extend_unique(&mut selected_resource_ids, &conversation.settings.hook_ids);
    let workspace_path = document
        .workspaces
        .iter()
        .find(|workspace| {
            workspace
                .conversations
                .iter()
                .any(|candidate| candidate.id == conversation.id)
        })
        .map(|workspace| workspace.path.as_str())
        .unwrap_or_default();
    let entries = discover_hook_entries_for_workspace(
        workspace_path,
        document.global_settings.resolved_app_language,
    );
    selected_resource_ids
        .iter()
        .map(|resource_id| {
            entries
                .iter()
                .find(|entry| entry.descriptor.id == *resource_id)
                .map(|entry| entry.definition.clone())
                .ok_or_else(|| {
                    format!(
                        "Hook {resource_id} is unavailable in the current workspace. Check hooks.json and scan again."
                    )
                })
        })
        .collect()
}

#[derive(Clone)]
struct HookEntry {
    descriptor: ResourceDescriptor,
    definition: HookDefinition,
}

/// Hook entries from the user file and every workspace file. Descriptor
/// descriptions (event label, matcher) follow the application language: they
/// are UI text; the model-facing row is rendered from the run's profile.
fn discover_hook_entries(document: &AppDocument) -> Vec<HookEntry> {
    let language = document.global_settings.resolved_app_language;
    let mut entries = discover_hook_entries_for_workspace("", language);
    let mut seen_locations = entries
        .iter()
        .map(|entry| entry.descriptor.location.clone())
        .collect::<HashSet<_>>();
    for workspace in &document.workspaces {
        if workspace.path.trim().is_empty() {
            continue;
        }
        for entry in read_hooks_file(
            &preferred_config_path(&PathBuf::from(&workspace.path), Path::new("hooks.json")),
            ResourceSource::Workspace,
            language,
        ) {
            if seen_locations.insert(entry.descriptor.location.clone()) {
                entries.push(entry);
            }
        }
    }
    entries.sort_by(|left, right| {
        left.descriptor
            .name
            .to_lowercase()
            .cmp(&right.descriptor.name.to_lowercase())
            .then_with(|| left.descriptor.location.cmp(&right.descriptor.location))
    });
    entries
}

fn discover_hook_entries_for_workspace(
    workspace_path: &str,
    language: ResolvedLanguage,
) -> Vec<HookEntry> {
    let mut entries = dirs::home_dir()
        .map(|home| {
            read_hooks_file(
                &preferred_config_path(&home, Path::new("hooks.json")),
                ResourceSource::User,
                language,
            )
        })
        .unwrap_or_default();
    if !workspace_path.trim().is_empty() {
        entries.extend(read_hooks_file(
            &preferred_config_path(&PathBuf::from(workspace_path), Path::new("hooks.json")),
            ResourceSource::Workspace,
            language,
        ));
    }
    entries
}

fn read_hooks_file(path: &Path, source: ResourceSource, language: ResolvedLanguage) -> Vec<HookEntry> {
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
fn parse_tool_description_entries(value: &Value) -> Vec<ToolDescriptionEntry> {
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
    }
}

/// Scans all `*.json` files in a `tool-descriptions/` directory. Each file is one
/// complete tool-description table, parallel to a skill directory or MCP entry.
fn scan_tool_description_root(
    root: &Path,
    source: ResourceSource,
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
        output.push(tool_description_descriptor(&path, source, location));
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
pub fn resolve_prompt_profile(
    document: &AppDocument,
    conversation: &Conversation,
) -> PromptProfile {
    let app_language = document.global_settings.resolved_app_language;
    let Some(selected) = conversation
        .settings
        .tool_description_file_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    else {
        return PromptProfile::builtin_english();
    };
    if let Some(builtin) = PromptProfile::builtin_for_id(selected) {
        return builtin;
    }
    discover_tool_description_files(document)
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
        .unwrap_or_else(PromptProfile::builtin_english)
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

fn stable_id(prefix: &str, name: &str, location: &str) -> String {
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

        let mut document = crate::catalog::default_document(directory.path());
        for workspace in &mut document.workspaces {
            workspace.path = directory.path().to_string_lossy().into_owned();
        }
        let catalog = discover(&document, &directory.path().join("skills"));
        assert_eq!(catalog.tool_description_files.len(), 3);
        assert_eq!(
            catalog.tool_description_files[0].id,
            prompt_profile::BUILTIN_EN_US_ID
        );
        assert_eq!(
            catalog.tool_description_files[1].id,
            prompt_profile::BUILTIN_ZH_CN_ID
        );
        assert_eq!(catalog.tool_description_files[2].name, "strict");
        let found = &catalog.tool_description_files[2];
        assert_eq!(found.description, "2 个工具描述 · 1 条提示词覆盖");
        assert!(found.available);

        document.workspaces[0].conversations[0]
            .settings
            .tool_description_file_id = Some(found.id.clone());
        let profile = resolve_prompt_profile(&document, &document.workspaces[0].conversations[0]);
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
        let profile = resolve_prompt_profile(&document, &document.workspaces[0].conversations[0]);
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
        let mut document = crate::catalog::default_document(directory.path());
        let profile = resolve_prompt_profile(&document, &document.workspaces[0].conversations[0]);
        assert_eq!(profile.id, prompt_profile::BUILTIN_EN_US_ID);
        assert_eq!(profile.language, ResolvedLanguage::EnUs);

        document.workspaces[0].conversations[0]
            .settings
            .tool_description_file_id = Some(prompt_profile::BUILTIN_ZH_CN_ID.to_owned());
        let profile = resolve_prompt_profile(&document, &document.workspaces[0].conversations[0]);
        assert_eq!(profile.id, prompt_profile::BUILTIN_ZH_CN_ID);
        assert_eq!(profile.language, ResolvedLanguage::ZhCn);

        document.workspaces[0].conversations[0]
            .settings
            .tool_description_file_id = Some("missing-profile".to_owned());
        let profile = resolve_prompt_profile(&document, &document.workspaces[0].conversations[0]);
        assert_eq!(profile.id, prompt_profile::BUILTIN_EN_US_ID);
        assert_eq!(profile.language, ResolvedLanguage::EnUs);
    }

    /// A document with no user or workspace skills must discover no skills and must
    /// not fall back to bundled entries.
    #[test]
    fn discovery_ships_no_builtin_skills() {
        let directory = tempfile::tempdir().unwrap();
        let mut document = crate::catalog::default_document(directory.path());
        for workspace in &mut document.workspaces {
            workspace.path = directory.path().to_string_lossy().into_owned();
        }
        let conversation = &document.workspaces[0].conversations[0];

        let catalog = discover(&document, &directory.path().join("skills"));
        assert!(catalog
            .skills
            .iter()
            .all(|skill| skill.source != ResourceSource::Builtin));

        let hook_definitions = HashMap::new();
        let profile = PromptProfile::builtin_english();
        let addendum =
            runtime_context_from_catalog(conversation, &catalog, &hook_definitions, &profile)
                .unwrap()
                .addendum;
        assert!(!addendum.contains("capability installation guide"));
    }

    #[test]
    fn selected_workspace_skill_mcp_and_hook_are_assembled_in_profile_words() {
        let directory = tempfile::tempdir().unwrap();
        let skill_path = directory.path().join("SKILL.md");
        fs::write(&skill_path, "# Probe skill\n\nMEWORK_SKILL_BODY_E2E").unwrap();
        let mut document = crate::catalog::default_document(directory.path());
        document.workspaces[0].conversations[0].settings.skill_ids = vec!["skill-probe".into()];
        document.workspaces[0].conversations[0].settings.mcp_ids = vec!["mcp-probe".into()];
        document.workspaces[0].conversations[0].settings.hook_ids = vec!["hook-probe".into()];
        let catalog = CapabilityCatalog {
            hooks: vec![ResourceDescriptor {
                id: "hook-probe".into(),
                name: "Probe Hook".into(),
                description: "display-only hook description".into(),
                location: "~/.naiword/hooks.json#/hooks/probe".into(),
                source: ResourceSource::User,
                available: true,
            }],
            skills: vec![ResourceDescriptor {
                id: "skill-probe".into(),
                name: "Probe Skill".into(),
                description: "test skill".into(),
                location: skill_path.to_string_lossy().into_owned(),
                source: ResourceSource::Workspace,
                available: true,
            }],
            mcps: vec![ResourceDescriptor {
                id: "mcp-probe".into(),
                name: "Probe MCP".into(),
                description: "stdio test server".into(),
                location: "~/.naiword/mcp.json#Probe MCP".into(),
                source: ResourceSource::User,
                available: true,
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

        let addendum =
            runtime_context_from_catalog(conversation, &catalog, &hook_definitions, &english)
                .unwrap()
                .addendum;

        let skill_position = addendum.find("MEWORK_SKILL_BODY_E2E").unwrap();
        let mcp_position = addendum.find("## Selected MCP servers").unwrap();
        let hook_position = addendum.find("## Lifecycle hooks").unwrap();
        assert!(skill_position < mcp_position && mcp_position < hook_position);
        assert!(addendum.contains("- Probe MCP: stdio test server"));
        assert!(addendum.contains("- Probe Hook: Before a tool runs"));
        assert!(addendum
            .contains("Their tools can be called only when the host exposed them to this turn"));
        assert!(!addendum.contains("npm test"));

        let chinese = PromptProfile::builtin_chinese();
        let chinese_addendum =
            runtime_context_from_catalog(conversation, &catalog, &hook_definitions, &chinese)
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
        let (document, catalog) = skill_only_fixture(directory.path(), &skill_path);
        let conversation = &document.workspaces[0].conversations[0];

        let hook_definitions = HashMap::new();
        let profile = PromptProfile::builtin_english();
        let context =
            runtime_context_from_catalog(conversation, &catalog, &hook_definitions, &profile)
                .unwrap();

        assert_eq!(context.addendum, "# Probe skill\n\nMEWORK_SKILL_BODY_E2E");
        // With the skill tool disabled, this output must be empty to avoid duplicating bodies.
        assert!(context.skills.is_empty());
    }

    /// With the tool enabled, skill bodies leave the system prompt while trigger text
    /// and the directory enter the tool output.
    #[test]
    fn the_skill_tool_takes_the_body_out_of_the_prompt_and_carries_trigger_and_directory() {
        let directory = tempfile::tempdir().unwrap();
        let skill_path = directory.path().join(SKILL_MANIFEST);
        fs::write(
            &skill_path,
            "---\nname: Probe Skill\ndescription: Use when probing\nwhen_to_use: the user says probe\n---\n\nMEWORK_SKILL_BODY_E2E\n",
        )
        .unwrap();
        let (mut document, catalog) = skill_only_fixture(directory.path(), &skill_path);
        document.workspaces[0].conversations[0]
            .settings
            .skill_tool_enabled = true;
        let conversation = &document.workspaces[0].conversations[0];

        let hook_definitions = HashMap::new();
        let profile = PromptProfile::builtin_english();
        let context =
            runtime_context_from_catalog(conversation, &catalog, &hook_definitions, &profile)
                .unwrap();

        assert_eq!(context.addendum, "");
        assert_eq!(context.skills.len(), 1);
        let skill = &context.skills[0];
        assert_eq!(skill.name, "Probe Skill");
        // `when_to_use` supplements the description rather than replacing it.
        assert_eq!(skill.trigger, "Use when probing - the user says probe");
        assert_eq!(skill.body, "MEWORK_SKILL_BODY_E2E");
        assert_eq!(skill.directory, directory.path().to_string_lossy());
    }

    /// A conversation selecting one skill and a catalog containing only that skill.
    fn skill_only_fixture(workspace: &Path, skill_path: &Path) -> (AppDocument, CapabilityCatalog) {
        let mut document = crate::catalog::default_document(workspace);
        document.workspaces[0].conversations[0].settings.skill_ids = vec!["skill-probe".into()];
        let catalog = CapabilityCatalog {
            hooks: Vec::new(),
            skills: vec![ResourceDescriptor {
                id: "skill-probe".into(),
                name: "Probe Skill".into(),
                description: "test skill".into(),
                location: skill_path.to_string_lossy().into_owned(),
                source: ResourceSource::Workspace,
                available: true,
            }],
            mcps: Vec::new(),
            tool_description_files: Vec::new(),
        };
        (document, catalog)
    }

    /// Duplicate selected skill names must fail in tool mode because one name cannot
    /// select two skills. Prompt mode does not select by name and permits duplicates.
    #[test]
    fn two_selected_skills_sharing_a_name_are_refused_only_in_tool_mode() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("a").join(SKILL_MANIFEST);
        let second = directory.path().join("b").join(SKILL_MANIFEST);
        for (path, marker) in [(&first, "BODY-A"), (&second, "BODY-B")] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, format!("---\nname: Probe Skill\n---\n\n{marker}\n")).unwrap();
        }
        let mut document = crate::catalog::default_document(directory.path());
        document.workspaces[0].conversations[0].settings.skill_ids =
            vec!["skill-a".into(), "skill-b".into()];
        let descriptor = |id: &str, path: &Path| ResourceDescriptor {
            id: id.into(),
            name: "Probe Skill".into(),
            description: "test skill".into(),
            location: path.to_string_lossy().into_owned(),
            source: ResourceSource::User,
            available: true,
        };
        let catalog = CapabilityCatalog {
            hooks: Vec::new(),
            skills: vec![
                descriptor("skill-a", &first),
                descriptor("skill-b", &second),
            ],
            mcps: Vec::new(),
            tool_description_files: Vec::new(),
        };

        // Prompt mode permits duplicate names because both bodies reach the model.
        let hook_definitions = HashMap::new();
        let profile = PromptProfile::builtin_english();
        let concatenated = runtime_context_from_catalog(
            &document.workspaces[0].conversations[0],
            &catalog,
            &hook_definitions,
            &profile,
        )
        .unwrap()
        .addendum;
        assert!(concatenated.contains("BODY-A") && concatenated.contains("BODY-B"));

        document.workspaces[0].conversations[0]
            .settings
            .skill_tool_enabled = true;
        let error = runtime_context_from_catalog(
            &document.workspaces[0].conversations[0],
            &catalog,
            &hook_definitions,
            &profile,
        )
        .expect_err("duplicate skill names cannot be selected in tool mode");

        assert!(error.contains("Probe Skill"), "{error}");
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

        let entries = read_hooks_file(&path, ResourceSource::User, ResolvedLanguage::EnUs);

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

        let entries = read_hooks_file(&path, ResourceSource::User, ResolvedLanguage::EnUs);

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
        let mut document = crate::catalog::default_document(directory.path());
        let discovered = discover(&document, &directory.path().join("skills"));
        let hook_id = discovered
            .hooks
            .iter()
            .find(|hook| hook.source == ResourceSource::Workspace)
            .unwrap()
            .id
            .clone();
        document.workspaces[0].conversations[0].settings.hook_ids = vec![hook_id];

        let hooks = resolve_hooks(&document, &document.workspaces[0].conversations[0]).unwrap();

        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0].event, HookEvent::UserPromptSubmit);
        assert_eq!(hooks[0].command, "npm run lint");
        assert!(hooks[0].enabled);
    }
}
