use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

pub type JsonObject = Map<String, Value>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AppDocument {
    pub schema_version: u32,
    pub global_settings: GlobalSettings,
    /// User-managed assets. API keys are stored separately in the OS credential
    /// store and never appear here.
    #[serde(default)]
    pub assets: AssetLibrary,
    /// Flat conversation presets contain system prompts, tool allowlists, inline
    /// descriptions, and selected skill, MCP, and hook resource IDs.
    #[serde(default)]
    pub presets: PresetLibrary,
    pub workspaces: Vec<Workspace>,
    pub tools: Vec<ToolDescriptor>,
    pub capabilities: CapabilityCatalog,
}

/// User-managed assets. API keys are kept exclusively in the OS credential store.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AssetLibrary {
    #[serde(default)]
    pub api_providers: Vec<ApiProvider>,
    /// Search provider configuration. Search behavior belongs to
    /// [`ConversationWebSearchSettings`].
    #[serde(default)]
    pub web_search: WebSearchAssets,
    /// In-app MCP registry; the authoritative source of MCP servers. Read-only
    /// discovery from `~/.mework/mcp.json` was removed and must not return as a
    /// fallback source.
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
    /// App-data skill index; the authoritative source of installed skills.
    /// Read-only discovery from `~/.mework/skills` was removed and must not
    /// return as a fallback source.
    #[serde(default)]
    pub skills: Vec<SkillRecord>,
    /// SSH machine catalog and environment-specific variables. Conversations own
    /// their selected execution environment.
    #[serde(default)]
    pub execution_environments: ExecutionEnvironmentAssets,
}

/// Execution-environment assets.
///
/// WSL distributions are enumerated at runtime rather than persisted, because
/// installation and renaming would make a stored list stale. Environment
/// variables are plaintext launch configuration, not rotatable secrets.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionEnvironmentAssets {
    #[serde(default)]
    pub ssh_machines: Vec<SshMachineConfig>,
    /// Environment key to variables. Dangling keys are allowed so deleting a
    /// machine does not invalidate the document.
    #[serde(default)]
    pub env_vars: BTreeMap<String, BTreeMap<String, String>>,
}

/// A user-registered SSH execution machine.
///
/// Authentication material is not persisted. OpenSSH resolves identities and
/// agents at connection time; BatchMode fails explicitly when none are usable.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SshMachineConfig {
    pub id: String,
    pub name: String,
    pub host: String,
    /// Zero uses the default SSH port, 22.
    #[serde(default)]
    pub port: u16,
    /// Empty delegates identity selection to OpenSSH defaults.
    #[serde(default)]
    pub identity_file: String,
    /// Empty uses the remote user's home directory. A leading `~` is supported.
    #[serde(default)]
    pub remote_cwd: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

/// MCP server transport. Only stdio and Streamable HTTP are supported.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpTransportKind {
    #[default]
    Stdio,
    StreamableHttp,
}

/// A user-configured MCP server.
///
/// Command lines, URLs, headers, and environment variables are launch
/// configuration rather than rotatable secrets; runtime Debug output redacts them.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpServerConfig {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Globally enabled servers enter the capability catalog.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub transport: McpTransportKind,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Optional stdio package-registry mirror. Explicit user environment values
    /// take precedence over this convenience setting.
    #[serde(default)]
    pub registry_url: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Zero uses the host default timeout.
    #[serde(default)]
    pub timeout_seconds: u32,
    #[serde(default)]
    pub long_running: bool,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub provider_url: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// An empty list leaves every server tool available. Exclusions let newly
    /// exposed server tools remain available by default.
    #[serde(default)]
    pub disabled_tools: Vec<String>,
    #[serde(default)]
    pub disabled_auto_approve_tools: Vec<String>,
    #[serde(default)]
    pub sort_order: u32,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

/// Installation source for a skill.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SkillSource {
    #[default]
    LocalDirectory,
    Zip,
    SystemScan,
    /// Installed from an online skill registry; its content is copied into app data.
    Remote,
}

/// An installed skill.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SkillRecord {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Application-data directory name, unique case-insensitively.
    pub folder_name: String,
    #[serde(default)]
    pub source: SkillSource,
    #[serde(default)]
    pub source_location: String,
    /// Registry page used only for a "view source" link; empty for local skills.
    #[serde(default)]
    pub source_url: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub content_hash: String,
    /// Globally enabled; a conversation preset selects a second layer.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub installed_at: String,
    #[serde(default)]
    pub updated_at: String,
}

/// Conversation preset container.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PresetLibrary {
    #[serde(default)]
    pub conversation_presets: Vec<ConversationPreset>,
    /// New conversations link to this preset; empty when the implicit blank
    /// default is active.
    #[serde(default)]
    pub default_conversation_preset_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GlobalSettings {
    /// Missing values belong to records that predate this field and retain the old Chinese UI.
    #[serde(default)]
    pub app_language: AppLanguage,
    /// What `app_language` currently resolves to, mirrored here by the renderer.
    ///
    /// This resolved value determines both host-rendered UI text (hook descriptors)
    /// and the language a *user-written* prompt profile is treated as being in:
    /// such a file declares none of its own, and the built-in that fills the keys it
    /// omits follows from this. Selecting one of the two built-in profiles — or
    /// selecting nothing, which is the English built-in — pins the language to
    /// that profile instead. Only the renderer can resolve `auto` — the host
    /// links no OS-locale crate — so it writes the resolved value here and the
    /// backend reads this field rather than re-deriving it.
    #[serde(default)]
    pub resolved_app_language: ResolvedLanguage,
    /// Missing values belong to records that predate this field and retain the old day theme.
    #[serde(default)]
    pub theme: ThemePreference,
    #[serde(default)]
    pub last_reasoning_effort: ReasoningEffort,
    /// The globally selected default Composer model.
    #[serde(default)]
    pub active_provider_id: Option<String>,
    /// Renderer-owned appearance preferences must round-trip unchanged.
    #[serde(default)]
    pub appearance: AppearancePreferences,
    /// User-modified shortcuts. The command catalog is renderer-owned.
    #[serde(default)]
    pub shortcuts: BTreeMap<String, ShortcutPreference>,
    /// User-added environment dependencies; built-ins are code constants.
    #[serde(default)]
    pub environment_tools: Vec<EnvironmentToolDefinition>,
}

/// Persisted preference for one shortcut.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ShortcutPreference {
    #[serde(default)]
    pub binding: Vec<String>,
    #[serde(default)]
    pub enabled: bool,
}

/// An executable to detect on PATH. This version detects but does not install it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentToolDefinition {
    pub name: String,
    pub executable: String,
    #[serde(default)]
    pub version_args: Vec<String>,
}

/// Appearance preferences.
///
/// Defaults must match `src/seed.ts` so both sides normalize a document identically.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AppearancePreferences {
    #[serde(default)]
    pub theme_color: String,
    #[serde(default = "default_zoom")]
    pub zoom: f64,
    #[serde(default)]
    pub ui_font_family: String,
    #[serde(default)]
    pub mono_font_family: String,
    #[serde(default = "default_message_font_size")]
    pub message_font_size: u32,
    #[serde(default)]
    pub serif_messages: bool,
    #[serde(default)]
    pub wide_messages: bool,
    #[serde(default = "default_send_shortcut")]
    pub send_shortcut: Vec<String>,
    #[serde(default = "default_newline_shortcut")]
    pub newline_shortcut: Vec<String>,
    #[serde(default)]
    pub spell_check: bool,
    #[serde(default)]
    pub render_user_markdown: bool,
    /// Disabled by default because message deletion is reversible.
    #[serde(default)]
    pub confirm_message_delete: bool,
    #[serde(default = "default_true")]
    pub collapse_reasoning: bool,
    #[serde(default)]
    pub code_block_collapsible: bool,
    #[serde(default)]
    pub code_block_wrappable: bool,
    #[serde(default = "default_true")]
    pub single_dollar_math: bool,
    #[serde(default)]
    pub custom_css: String,
}

fn default_zoom() -> f64 {
    1.0
}

fn default_message_font_size() -> u32 {
    14
}

fn default_send_shortcut() -> Vec<String> {
    vec!["Enter".to_owned()]
}

fn default_newline_shortcut() -> Vec<String> {
    vec!["Shift".to_owned(), "Enter".to_owned()]
}

impl Default for AppearancePreferences {
    fn default() -> Self {
        Self {
            theme_color: String::new(),
            zoom: default_zoom(),
            ui_font_family: String::new(),
            mono_font_family: String::new(),
            message_font_size: default_message_font_size(),
            serif_messages: false,
            wide_messages: false,
            send_shortcut: default_send_shortcut(),
            newline_shortcut: default_newline_shortcut(),
            spell_check: false,
            render_user_markdown: false,
            confirm_message_delete: false,
            collapse_reasoning: true,
            code_block_collapsible: false,
            code_block_wrappable: false,
            single_dollar_math: true,
            custom_css: String::new(),
        }
    }
}

pub const MAX_AGENT_TYPE_CHARS: usize = 64;

/// Validate the only named-agent identity accepted from model-facing tool
/// input. Resolution to a persisted definition remains a host responsibility.
pub fn validate_agent_type_slug(name: &str) -> Result<(), String> {
    if name.is_empty() || name.chars().count() > MAX_AGENT_TYPE_CHARS {
        return Err(format!("agent_type 必须是 1–{MAX_AGENT_TYPE_CHARS} 个字符"));
    }
    let first = name.chars().next().expect("non-empty agent type");
    if !first.is_ascii_lowercase() {
        return Err("agent_type 必须以小写字母开头".into());
    }
    if !name.chars().all(|character| {
        character.is_ascii_lowercase()
            || character.is_ascii_digit()
            || character == '_'
            || character == '-'
    }) {
        return Err("agent_type 只能包含小写字母、数字、_ 和 -".into());
    }
    Ok(())
}

/// Valid persisted model ID shape.
///
/// Model IDs are opaque provider-defined strings. Preserve case, punctuation,
/// slashes, colons, and non-ASCII characters; validation only rejects empty,
/// oversized, and control-character values.
pub fn validate_model_id(value: &str) -> Result<(), String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("模型 ID 不能为空".into());
    }
    if trimmed.len() > MAX_MODEL_ID_BYTES {
        return Err(format!("模型 ID 超过 {MAX_MODEL_ID_BYTES} 个 UTF-8 字节"));
    }
    if trimmed.chars().any(char::is_control) {
        return Err("模型 ID 不能包含控制字符".into());
    }
    Ok(())
}

pub const MAX_MODEL_ID_BYTES: usize = 512;

/// Length of a lowercase hexadecimal SHA-256 digest.
pub const LOWER_HEX_DIGEST_LEN: usize = 64;

/// Whether a string is a lowercase hexadecimal SHA-256 digest.
pub fn is_lower_hex_digest(value: &str) -> bool {
    value.len() == LOWER_HEX_DIGEST_LEN
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Rewrites every number into a form that is stable across a JSON round trip
/// through JavaScript, so a value compared before and after that trip is the
/// same value.
///
/// Tool cards are built here, handed to the renderer over IPC, held as
/// JavaScript objects, and handed back on save. `serde_json` preserves the
/// literal a provider wrote (`1.0` stays `1.0`, integers stay exact past
/// 2^53); JavaScript has only `f64`, so the same card comes back as `1` and
/// large integers come back rounded. Anything that compares tool payloads
/// across that boundary — attestation above all — must compare this form
/// rather than the original bytes, or a provider writing `3.0` where the
/// schema says `number` silently makes its card unsaveable forever.
///
/// Applying this twice changes nothing, and applying it to a payload that
/// already made the round trip changes nothing, which is the property
/// attestation depends on. It is deliberately lossy in the same way
/// JavaScript is lossy: integers beyond 2^53 collapse onto the `f64` they
/// would become. It is *not* a promise to reproduce JavaScript's exact digits
/// — `serde_json::Number` cannot even hold `1e20` in positional form — only a
/// promise that both sides converge on one value after one trip.
pub fn canonicalize_json_numbers(value: &mut Value) {
    match value {
        Value::Number(number) => {
            if let Some(canonical) = canonical_json_number(number) {
                *number = canonical;
            }
        }
        Value::Array(items) => {
            for item in items {
                canonicalize_json_numbers(item);
            }
        }
        Value::Object(entries) => {
            for (_, entry) in entries.iter_mut() {
                canonicalize_json_numbers(entry);
            }
        }
        Value::Null | Value::Bool(_) | Value::String(_) => {}
    }
}

/// The round-trip-stable form of one number, or `None` when it already is
/// that form.
fn canonical_json_number(number: &serde_json::Number) -> Option<serde_json::Number> {
    // Integers up to 2^53 survive f64 exactly and cross unchanged. Larger
    // ones are re-derived from the f64 JavaScript would hold. The magnitude
    // decides this, not a round-trip cast: casting `i64::MAX` to f64 and back
    // saturates to `i64::MAX` again and would wrongly look lossless.
    if let Some(value) = number.as_i64() {
        if value.unsigned_abs() <= MAX_EXACT_JSON_INTEGER {
            return None;
        }
        return rounded_integer(value as f64);
    }
    if let Some(value) = number.as_u64() {
        if value <= MAX_EXACT_JSON_INTEGER {
            return None;
        }
        return rounded_integer(value as f64);
    }
    let value = number.as_f64()?;
    // JavaScript has no -0 in JSON output and prints an integral f64 without a
    // fractional part; serde_json keeps `-0.0` and `1.0`. Collapse both.
    if value == 0.0 {
        return (number.to_string() != "0").then(|| 0.into());
    }
    // An integral f64 within the exact range becomes the integer JavaScript
    // would hand back, so `1.0` and `9007199254740992.0` stop being distinct
    // from `1` and `9007199254740992`.
    if value.fract() == 0.0 && value.abs() <= MAX_EXACT_JSON_INTEGER as f64 {
        return Some((value as i64).into());
    }
    None
}

/// The integer an out-of-range integer literal rounds to once JavaScript has
/// held it. `Number::from_f64` would render an integral value with a trailing
/// `.0`, which JavaScript hands straight back as a bare integer — so the two
/// sides would trade `9007199254740992.0` and `9007199254740992` forever.
/// Values too large for an `i64` keep the float form, which both sides agree
/// on because JavaScript also prints them in exponent notation.
fn rounded_integer(value: f64) -> Option<serde_json::Number> {
    if value.abs() <= MAX_EXACT_JSON_INTEGER as f64 {
        return Some((value as i64).into());
    }
    serde_json::Number::from_f64(value)
}

/// 2^53: the largest integer an f64 represents exactly, and so the largest one
/// that crosses the renderer boundary unchanged.
const MAX_EXACT_JSON_INTEGER: u64 = 9_007_199_254_740_992;

/// [`canonicalize_json_numbers`] for a tool payload object.
pub fn canonicalize_object_numbers(object: &mut JsonObject) {
    for (_, entry) in object.iter_mut() {
        canonicalize_json_numbers(entry);
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AgentDefinitionSource {
    User,
    Project,
    Plugin,
    Managed,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentDefinitionMemory {
    #[default]
    None,
    User,
    Project,
    Local,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentModelSelection {
    #[default]
    Inherit,
    Explicit {
        #[serde(rename = "providerId")]
        provider_id: String,
        #[serde(rename = "modelId")]
        model_id: String,
    },
    /// A binding recorded as broken by an older build, which used to rewrite a
    /// dangling `Explicit` pair into this and discard both IDs.
    ///
    /// NOTHING WRITES THIS ANY MORE. An `Explicit` pair is now kept verbatim
    /// however long it fails to resolve, because "the provider is signed out"
    /// and "the model row is gone forever" are indistinguishable at rest, and
    /// only one of them justifies destroying the user's choice. Resolution is
    /// asked at call time instead, so a role recovers by itself once its model
    /// is fetched again. The variant survives so archives written before that
    /// change still deserialize.
    ///
    /// A role in this state is NOT callable: it is absent from the listing the
    /// model sees, and naming it fails with its own wording rather than the
    /// unknown-name text, because "you configured this and it broke" is a
    /// different fact from "no such role" and only the former is actionable.
    /// The role itself stays in the document so the user can see and fix it.
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentDefinition {
    /// Host-owned. A user-authored role lives inside a conversation preset, so
    /// the preset is already its on/off switch and the renderer always writes
    /// `true`; a trusted project/plugin/managed source may still ship a disabled
    /// definition, and that one keeps shadowing a user role of the same name.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Host-retained identity tombstone. Renderer saves cannot author this
    /// bit; deletion keeps the identity epoch durable so delete/re-add never
    /// reconnects to an older named-agent memory partition.
    #[serde(default)]
    pub deleted: bool,
    /// Trusted type slug selected by `agent_spawn.agent_type`.
    pub name: String,
    /// What this role is FOR, in the user's own words, rendered into the
    /// model-facing description of whichever of `agent_spawn` / `workflow`
    /// carries it (`builtin_schemas::append_role_descriptions`). Empty means
    /// "say nothing", and the role's line disappears from that block entirely.
    ///
    /// This model-facing description complements the machine-readable constraint:
    /// the `enum` says which names are legal, and this says what each one is for.
    ///
    /// NOT capability-bearing, and that is what keeps it out of three places on
    /// purpose. It is absent from the frozen `AgentDefinitionBindingV1`
    /// projection, so it cannot move execution-mode payload bytes. It is absent
    /// from `storage::same_user_agent_configuration`, so rewording a role does
    /// not advance `revision` — and therefore does not make
    /// `api::validate_current_agent_definition` refuse every child already
    /// bound to it. And it is absent from the renderer's own stricter
    /// `sameUserAgentConfiguration` for the same reason.
    #[serde(default)]
    pub description: String,
    pub source: AgentDefinitionSource,
    /// Stable host-origin key (for example a workspace, plugin, or policy ID).
    pub source_key: String,
    /// Monotonic positive source revision used to reject stale definitions.
    pub revision: u64,
    /// Host-owned identity epoch. Ordinary edits retain it; a definition
    /// recreated after deletion receives the next value.
    #[serde(default = "default_agent_definition_epoch")]
    pub memory_epoch: u64,
    #[serde(default)]
    pub model_selection: AgentModelSelection,
    #[serde(default)]
    pub memory: AgentDefinitionMemory,
    /// Reasoning effort for this definition's runs; `None` inherits the parent's.
    ///
    /// CAPABILITY-BEARING, like `tools` and `disallowed_tools` below, so it
    /// joins the identity payload: an offline edit must not be able to widen a
    /// definition while a receipt still verifies.
    ///
    /// It carries no `skip_serializing_if`. That would round-trip an old record
    /// byte-identically, but it makes "absent" and "at the default"
    /// indistinguishable, so stripping the key from the persisted document would
    /// silently restore the PERMISSIVE default — which is the substitution these
    /// payloads exist to detect. `Option`/`Vec` defaults handle legacy records
    /// instead: a definition written before this field existed simply lacks the
    /// key and reads as "no override".
    #[serde(default)]
    pub effort: Option<ReasoningEffort>,
    /// Exact tool allowlist. `None` means "whatever the child would otherwise
    /// get" — for a named child that is the parent conversation's enabled set
    /// minus `api::SUBAGENT_DISABLED_TOOL_NAMES`.
    ///
    /// `Some` is an independent SELECTION out of the trusted tool catalogue, not
    /// an intersection with the caller's set: a role may grant a catalogue tool
    /// the calling conversation had switched off. It still cannot name anything
    /// on `SUBAGENT_DISABLED_TOOL_NAMES` — that
    /// list is re-applied to the catalogue side and stays the absolute floor —
    /// and it neither grants nor revokes a host-derived name
    /// (`api::host_derived_child_tool`), because those follow switches and task
    /// producers rather than lists.
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    /// Names removed from this definition's runs, applied after `tools`.
    #[serde(default)]
    pub disallowed_tools: Vec<String>,
    /// Which search backend this role's `web_search` uses. `None` follows the
    /// calling conversation's own selection, which is why this is an `Option`
    /// around an enum that already has a `Native` default — "follow the
    /// conversation" and "explicitly native" are different answers, and a role
    /// on a model whose family cannot run provider-executed search needs to be
    /// able to say the first one.
    ///
    /// Only the search leg. `web_fetch` has no per-conversation choice either:
    /// `WebSearchSettings::effective` always resolves its provider from the
    /// global assets, so there is nothing here for a role to override.
    ///
    /// Capability-bearing, and carries no `skip_serializing_if`, for the same
    /// reason spelled out on `effort` above.
    #[serde(default)]
    pub search_provider: Option<SearchProviderSelection>,
}

/// Persisted host resolution for one trusted named-agent definition.
///
/// The model only selects the public definition name. Mework records the
/// exact source revision and the exact provider/model chosen for the initial
/// spawn so reload and follow-up turns can fail closed instead of silently
/// rebinding to a different definition or inherited model. `model_id` remains
/// raw and case-sensitive; this record never contains a hash or synthetic
/// owner ID.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentDefinitionBinding {
    pub source: AgentDefinitionSource,
    pub source_key: String,
    pub name: String,
    pub revision: u64,
    pub memory_epoch: u64,
    pub provider_id: String,
    pub model_id: String,
    pub memory: AgentDefinitionMemory,
    pub scope_key: String,
    /// Keyed receipt over the complete trusted definition/model binding.
    /// This detects same-revision offline substitution without persisting a
    /// plain prompt digest.
    #[serde(default)]
    pub configuration_receipt: String,
    /// Which payload shape `configuration_receipt` was signed over.
    ///
    /// A record written before versioning existed simply lacks the key and
    /// defaults to 1. That absence IS the version marker, and no MIGRATION ever
    /// rewrites a persisted record to stamp a version onto it: a migration
    /// would have to re-render the payload and re-sign, which is precisely what
    /// would invalidate the receipts the stamp was meant to preserve.
    ///
    /// Ordinary serialization is a separate matter and does write the key.
    /// There is deliberately no `skip_serializing_if` here, so every save emits
    /// whatever value the in-memory record already carries — which is the value
    /// its receipt was signed at, because
    /// `api::initial_agent_definition_binding` stamps and signs from the same
    /// constant. Verification recomputes at the recorded version through
    /// `api::agent_definition_receipt_payload_at`, so a stamped record and a
    /// legacy keyless one both verify against their own bytes.
    #[serde(default = "default_receipt_version")]
    pub receipt_version: u8,
}

/// Receipts predating explicit versioning are v1.
pub fn default_receipt_version() -> u8 {
    1
}

/// Persisted receipt for the exact provider/model and parent-memory snapshot
/// selected when a conversation fork was created.
///
/// Provider and model IDs remain raw, case-sensitive host values. The optional
/// receipt is a keyed authenticity check over the exact rendered auto-memory
/// prompt; it is not model identity and contains neither the prompt nor a plain
/// content digest.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ForkModelBinding {
    pub provider_id: String,
    pub model_id: String,
    pub memory_language: ResolvedLanguage,
    /// Exact sorted subset of the parent's enabled main-memory tools. The
    /// child may regain precisely these capabilities on reload, never a
    /// boolean-derived expansion to the complete tool family.
    #[serde(default)]
    pub memory_tool_names: Vec<String>,
    /// Exact host-generated child prompt used at fork creation. It is already
    /// user-owned document policy, not model-authored memory; the keyed
    /// receipt prevents offline same-record substitution on reload.
    #[serde(default)]
    pub system_prompt_snapshot: String,
    #[serde(default)]
    pub system_prompt_receipt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_snapshot_receipt: Option<String>,
    /// Domain-separated HMAC over every field above. Content receipts protect
    /// their individual snapshots; this receipt protects the exact raw
    /// provider/model identity and the complete capability binding.
    #[serde(default)]
    pub binding_receipt: String,
    /// Which payload shape `binding_receipt` was signed over. Absence means v1.
    ///
    /// No MIGRATION rewrites a persisted record to stamp this — re-rendering
    /// and re-signing is exactly what would invalidate the receipt. Ordinary
    /// serialization does write the key on every save (there is no
    /// `skip_serializing_if`), carrying forward the value the record already
    /// holds, which is the value its receipt was signed at.
    /// `api::fork_model_binding_receipt_payload` dispatches verification to
    /// that recorded version and hard-errors on a version this build cannot
    /// render, so a v2 record can never be silently checked as v1.
    ///
    /// There is deliberately no `prompt_version` companion:
    /// `system_prompt_receipt` is signed over `system_prompt_snapshot` and
    /// verified against that same stored snapshot, never recomputed, so the
    /// snapshot is self-authenticating and a version would cost bytes for
    /// nothing. `memory_snapshot_receipt` needs none either: it is a content
    /// receipt over one opaque host-rendered string, with no field set that
    /// could grow, and its whole purpose is to REJECT a changed render.
    #[serde(default = "default_receipt_version")]
    pub receipt_version: u8,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum AppLanguage {
    #[serde(rename = "auto")]
    Auto,
    #[default]
    #[serde(rename = "zh-CN")]
    ZhCn,
    #[serde(rename = "en-US")]
    EnUs,
}

/// An application language with `auto` already resolved.
///
/// Used both for the mirrored UI language and for the language in which a
/// tool-description set is authored. A conversation's tool descriptions follow
/// the application language, which a selected tool-description file cannot override.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum ResolvedLanguage {
    #[default]
    #[serde(rename = "zh-CN")]
    ZhCn,
    #[serde(rename = "en-US")]
    EnUs,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum ThemePreference {
    #[default]
    #[serde(rename = "day")]
    Day,
    #[serde(rename = "night")]
    Night,
    #[serde(rename = "system")]
    System,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ApiProvider {
    pub id: String,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub family: ProviderFamily,
    pub base_url: String,
    /// Family-specific identity fields, such as Bedrock `region`, Vertex
    /// `project`/`location`, and Azure `api_version`. They are provider-factory
    /// inputs, not URL components; `normalized_base_url` forbids query strings.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub family_settings: BTreeMap<FamilySetting, String>,
    /// Base URL overrides for non-chat endpoints. This cannot be derived because
    /// chat and image endpoints may use unrelated path prefixes.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub endpoint_base_urls: BTreeMap<EndpointType, String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
    #[serde(default)]
    pub models: Vec<ModelProfile>,
    #[serde(default)]
    pub active_model_id: Option<String>,
}


/// Fixed search-provider catalog. `kind` is both entry identity and credential
/// store key. The ten entries mirror Cherry Studio's catalog: identical IDs,
/// display names, capabilities, and default endpoints. These are host-side
/// `searchKeywords`/`fetchUrls` providers, distinct from a model's own
/// server-side search tool. Adding an entry must update this enum, the
/// TypeScript mirror `src/lib/searchProviders.ts`, and the documentation
/// together.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum SearchProviderKind {
    Zhipu,
    Tavily,
    Searxng,
    Exa,
    ExaMcp,
    Bocha,
    Querit,
    Fetch,
    Jina,
    Firecrawl,
}

/// An operation the host can perform through a search provider.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum SearchCapability {
    SearchKeywords,
    FetchUrls,
}

/// Built-in declaration for one catalog capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchCapabilitySpec {
    /// Empty means the capability requires no endpoint, as with local `fetch`.
    pub default_api_host: &'static str,
    /// Whether the capability requires an API key.
    pub requires_api_key: bool,
}

impl SearchCapabilitySpec {
    /// Whether the capability requires a usable HTTP(S) endpoint.
    pub fn requires_api_host(&self) -> bool {
        !self.default_api_host.is_empty()
    }
}

/// Canonical catalog. Keep each entry on one line: the cross-language guard
/// compares this table line by line with its TypeScript mirror.
pub const SEARCH_PROVIDER_CATALOG: &[SearchProviderCatalogEntry] = &[
    SearchProviderCatalogEntry { kind: SearchProviderKind::Zhipu, slug: "zhipu", label: "Zhipu", search: Some(SearchCapabilitySpec { default_api_host: "https://open.bigmodel.cn/api/paas/v4/web_search", requires_api_key: true }), fetch: None },
    SearchProviderCatalogEntry { kind: SearchProviderKind::Tavily, slug: "tavily", label: "Tavily", search: Some(SearchCapabilitySpec { default_api_host: "https://api.tavily.com", requires_api_key: true }), fetch: None },
    SearchProviderCatalogEntry { kind: SearchProviderKind::Searxng, slug: "searxng", label: "Searxng", search: Some(SearchCapabilitySpec { default_api_host: "http://localhost:8080", requires_api_key: false }), fetch: None },
    SearchProviderCatalogEntry { kind: SearchProviderKind::Exa, slug: "exa", label: "Exa", search: Some(SearchCapabilitySpec { default_api_host: "https://api.exa.ai", requires_api_key: true }), fetch: None },
    SearchProviderCatalogEntry { kind: SearchProviderKind::ExaMcp, slug: "exa-mcp", label: "ExaMCP", search: Some(SearchCapabilitySpec { default_api_host: "https://mcp.exa.ai/mcp", requires_api_key: false }), fetch: None },
    SearchProviderCatalogEntry { kind: SearchProviderKind::Bocha, slug: "bocha", label: "Bocha", search: Some(SearchCapabilitySpec { default_api_host: "https://api.bochaai.com", requires_api_key: true }), fetch: None },
    SearchProviderCatalogEntry { kind: SearchProviderKind::Querit, slug: "querit", label: "Querit", search: Some(SearchCapabilitySpec { default_api_host: "https://api.querit.ai", requires_api_key: true }), fetch: Some(SearchCapabilitySpec { default_api_host: "https://api.querit.ai", requires_api_key: true }) },
    SearchProviderCatalogEntry { kind: SearchProviderKind::Fetch, slug: "fetch", label: "fetch", search: None, fetch: Some(SearchCapabilitySpec { default_api_host: "", requires_api_key: false }) },
    SearchProviderCatalogEntry { kind: SearchProviderKind::Jina, slug: "jina", label: "Jina", search: Some(SearchCapabilitySpec { default_api_host: "https://s.jina.ai", requires_api_key: true }), fetch: Some(SearchCapabilitySpec { default_api_host: "https://r.jina.ai", requires_api_key: false }) },
    SearchProviderCatalogEntry { kind: SearchProviderKind::Firecrawl, slug: "firecrawl", label: "Firecrawl", search: Some(SearchCapabilitySpec { default_api_host: "https://api.firecrawl.dev", requires_api_key: false }), fetch: Some(SearchCapabilitySpec { default_api_host: "https://api.firecrawl.dev", requires_api_key: false }) },
];

#[derive(Clone, Copy, Debug)]
pub struct SearchProviderCatalogEntry {
    pub kind: SearchProviderKind,
    pub slug: &'static str,
    pub label: &'static str,
    pub search: Option<SearchCapabilitySpec>,
    pub fetch: Option<SearchCapabilitySpec>,
}

impl SearchProviderKind {
    pub const CATALOG: &'static [Self] = &[
        Self::Zhipu,
        Self::Tavily,
        Self::Searxng,
        Self::Exa,
        Self::ExaMcp,
        Self::Bocha,
        Self::Querit,
        Self::Fetch,
        Self::Jina,
        Self::Firecrawl,
    ];

    fn entry(self) -> &'static SearchProviderCatalogEntry {
        SEARCH_PROVIDER_CATALOG
            .iter()
            .find(|entry| entry.kind == self)
            .expect("every kind has a catalog row")
    }

    /// Catalog identity used by credentials, documents, and the frontend.
    pub fn slug(self) -> &'static str {
        self.entry().slug
    }

    pub fn label(self) -> &'static str {
        self.entry().label
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        SEARCH_PROVIDER_CATALOG
            .iter()
            .find(|entry| entry.slug == slug)
            .map(|entry| entry.kind)
    }

    /// Built-in declaration for this capability, or `None` if unsupported.
    pub fn capability(self, capability: SearchCapability) -> Option<&'static SearchCapabilitySpec> {
        let entry = self.entry();
        match capability {
            SearchCapability::SearchKeywords => entry.search.as_ref(),
            SearchCapability::FetchUrls => entry.fetch.as_ref(),
        }
    }

    pub fn supports(self, capability: SearchCapability) -> bool {
        self.capability(capability).is_some()
    }

    /// Empty means this capability needs no endpoint, as with `fetch`.
    pub fn default_api_host(self, capability: SearchCapability) -> &'static str {
        self.capability(capability)
            .map(|spec| spec.default_api_host)
            .unwrap_or("")
    }
}

/// Search-result compression method.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SearchCompressionMethod {
    /// Return provider content unchanged.
    None,
    /// Truncate each result to a token budget.
    #[default]
    Cutoff,
}

/// Search-result compression configuration. `cutoff_limit` is the total
/// per-call token budget, shared across results.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SearchCompression {
    #[serde(default)]
    pub method: SearchCompressionMethod,
    #[serde(default = "default_search_cutoff_limit")]
    pub cutoff_limit: u32,
}

/// Default per-call search-result token budget.
pub const DEFAULT_SEARCH_CUTOFF_LIMIT: u32 = 2_000;
/// Default maximum search-result count.
pub const DEFAULT_SEARCH_MAX_RESULTS: u32 = 5;
/// Maximum input count per call.
pub const MAX_SEARCH_INPUTS: usize = 20;

fn default_search_cutoff_limit() -> u32 {
    DEFAULT_SEARCH_CUTOFF_LIMIT
}

fn default_search_max_results() -> u32 {
    DEFAULT_SEARCH_MAX_RESULTS
}

impl Default for SearchCompression {
    fn default() -> Self {
        Self {
            method: SearchCompressionMethod::default(),
            cutoff_limit: DEFAULT_SEARCH_CUTOFF_LIMIT,
        }
    }
}

/// Search-provider assets and global execution settings. Providers must come
/// from [`SearchProviderKind::CATALOG`] at most once; API keys stay in the OS
/// credential store.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WebSearchAssets {
    #[serde(default)]
    pub providers: Vec<SearchProviderConfig>,
    /// Global provider for `web_fetch`. `None` makes the tool return a
    /// recoverable error; fetching has no provider-native fallback.
    #[serde(default)]
    pub fetch_provider: Option<SearchProviderKind>,
    /// Maximum results returned by one search.
    #[serde(default = "default_search_max_results")]
    pub max_results: u32,
    /// Domain exclusion patterns. Invalid patterns are ignored rather than
    /// failing the search.
    #[serde(default)]
    pub exclude_domains: Vec<String>,
    #[serde(default)]
    pub compression: SearchCompression,
}

impl Default for WebSearchAssets {
    fn default() -> Self {
        Self {
            providers: Vec::new(),
            fetch_provider: None,
            max_results: DEFAULT_SEARCH_MAX_RESULTS,
            exclude_domains: Vec::new(),
            compression: SearchCompression::default(),
        }
    }
}

impl WebSearchAssets {
    pub fn find(&self, kind: SearchProviderKind) -> Option<&SearchProviderConfig> {
        self.providers.iter().find(|entry| entry.kind == kind)
    }

    /// Runtime settings passed unchanged to the execution layer.
    pub fn execution(&self) -> SearchExecutionConfig {
        SearchExecutionConfig {
            max_results: self.max_results.max(1),
            exclude_domains: self.exclude_domains.clone(),
            compression: self.compression.clone(),
        }
    }
}

/// User configuration for a catalog search provider. `kind` is the entry
/// identity; there is no separate ID.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SearchProviderConfig {
    pub kind: SearchProviderKind,
    #[serde(default)]
    pub enabled: bool,
    /// Empty uses the catalog `searchKeywords` endpoint. Search and fetch may
    /// use separate hosts.
    #[serde(default)]
    pub search_api_host: String,
    /// Empty uses the catalog `fetchUrls` endpoint.
    #[serde(default)]
    pub fetch_api_host: String,
    /// Searxng engines. When empty, read `/config` and select enabled `general`
    /// and `web` engines.
    #[serde(default)]
    pub engines: Vec<String>,
    /// Searxng Basic Auth username. Its password is kept in the OS credential store.
    #[serde(default)]
    pub basic_auth_username: String,
}

impl SearchProviderConfig {
    pub fn new(kind: SearchProviderKind) -> Self {
        Self {
            kind,
            enabled: false,
            search_api_host: String::new(),
            fetch_api_host: String::new(),
            engines: Vec::new(),
            basic_auth_username: String::new(),
        }
    }

    pub fn enabled(kind: SearchProviderKind) -> Self {
        Self {
            enabled: true,
            ..Self::new(kind)
        }
    }

    /// Effective endpoint for a capability. Empty means none is required.
    pub fn effective_api_host(&self, capability: SearchCapability) -> &str {
        let override_host = match capability {
            SearchCapability::SearchKeywords => self.search_api_host.trim(),
            SearchCapability::FetchUrls => self.fetch_api_host.trim(),
        };
        if override_host.is_empty() {
            self.kind.default_api_host(capability)
        } else {
            override_host
        }
    }

    pub fn effective_engines(&self) -> Vec<String> {
        self.engines
            .iter()
            .map(|engine| engine.trim().to_owned())
            .filter(|engine| !engine.is_empty())
            .collect()
    }
}

/// Search backend selected by a conversation. Unavailable selections are saved
/// and reported as recoverable runtime errors.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SearchProviderSelection {
    /// Provider-native web search for this conversation's own model.
    #[default]
    Native,
    Explicit {
        #[serde(rename = "providerKind")]
        provider_kind: SearchProviderKind,
    },
    /// A missing or disabled explicit entry is persisted without its old identifier.
    Unavailable,
}

/// Conversation web-search behavior. Presets hold the same shape as a template
/// copied into new conversations. Tool enablement is the feature switch.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConversationWebSearchSettings {
    /// Maximum provider-native searches per `web_search` call. Zero is unlimited
    /// and applies only to the `Native` backend.
    #[serde(default)]
    pub max_searches_per_call: u32,
    /// Selected search backend for this conversation.
    #[serde(default)]
    pub provider: SearchProviderSelection,
}

/// Effective, host-built web-search view for one run. It is not persisted and
/// is skipped in `RunModelRequest`, so the renderer cannot assert it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WebSearchSettings {
    /// Provider-native search limit per `Native` call. Zero is unlimited.
    #[serde(default)]
    pub max_searches_per_call: u32,
    /// Resolved backend. `None` means search is unavailable with no implicit fallback.
    #[serde(default)]
    pub backend: Option<SearchBackend>,
    /// Resolved fetch provider. `None` makes `web_fetch` unavailable.
    #[serde(default)]
    pub fetch: Option<ResolvedSearchProvider>,
    /// Execution options used only by catalog-provider backends.
    #[serde(default)]
    pub execution: SearchExecutionConfig,
}

/// Runtime options for one search execution.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SearchExecutionConfig {
    pub max_results: u32,
    #[serde(default)]
    pub exclude_domains: Vec<String>,
    #[serde(default)]
    pub compression: SearchCompression,
}

impl Default for SearchExecutionConfig {
    fn default() -> Self {
        Self {
            max_results: DEFAULT_SEARCH_MAX_RESULTS,
            exclude_domains: Vec::new(),
            compression: SearchCompression::default(),
        }
    }
}

/// Resolved search backend for one run.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SearchBackend {
    /// The conversation's provider and model. Unsupported protocol families
    /// return a recoverable `web_search` error.
    Native,
    /// A catalog search provider called by the host.
    Provider(ResolvedSearchProvider),
}

/// Resolved provider with concrete endpoint and instance settings. Credentials
/// are read from the OS credential store only when a request is sent.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedSearchProvider {
    pub kind: SearchProviderKind,
    /// Endpoint for this capability. Empty means none is required.
    pub api_host: String,
    /// Used only by Searxng.
    #[serde(default)]
    pub engines: Vec<String>,
    /// Used only by Searxng.
    #[serde(default)]
    pub basic_auth_username: String,
}

impl WebSearchSettings {
    /// Combine persisted conversation behavior with assets into an effective view.
    /// Native always resolves. Missing or disabled explicit entries leave the
    /// corresponding backend absent for a recoverable tool error.
    pub fn effective(
        conversation: &ConversationWebSearchSettings,
        assets: &WebSearchAssets,
    ) -> Self {
        let resolve = |kind: SearchProviderKind, capability: SearchCapability| {
            assets
                .find(kind)
                .filter(|entry| entry.enabled && entry.kind.supports(capability))
                .map(|entry| ResolvedSearchProvider {
                    kind: entry.kind,
                    api_host: entry.effective_api_host(capability).to_owned(),
                    engines: entry.effective_engines(),
                    basic_auth_username: entry.basic_auth_username.trim().to_owned(),
                })
        };
        let backend = match &conversation.provider {
            SearchProviderSelection::Native => Some(SearchBackend::Native),
            SearchProviderSelection::Explicit { provider_kind } => {
                resolve(*provider_kind, SearchCapability::SearchKeywords)
                    .map(SearchBackend::Provider)
            }
            SearchProviderSelection::Unavailable => None,
        };
        Self {
            max_searches_per_call: conversation.max_searches_per_call,
            backend,
            fetch: assets
                .fetch_provider
                .and_then(|kind| resolve(kind, SearchCapability::FetchUrls)),
            execution: assets.execution(),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyStatus {
    pub configured: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_length: Option<usize>,
}

/// Provider adapter family. A family selects an AI SDK provider factory, not a
/// vendor; compatible endpoints can share one family.
///
/// `OpenaiCodex` is the one vendor-shaped exception: the ChatGPT-subscription
/// Codex backend speaks Responses but authenticates with an OAuth session the
/// host owns (`codex_oauth`), so it cannot be expressed as "a base URL plus a key".
///
/// `ClaudeAgent` is not an HTTP dialect at all: the sidecar drives the locally
/// installed Claude Code executable through the official Claude Agent SDK, and
/// the CLI makes the Anthropic Messages calls itself. Its key is optional (an
/// empty slot defers to the CLI's own login), its base URL is optional (it maps
/// to `ANTHROPIC_BASE_URL`), and the executable path is a family setting.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ProviderFamily {
    OpenaiResponses,
    OpenaiCodex,
    OpenaiChat,
    Anthropic,
    ClaudeAgent,
    Google,
    Xai,
    Azure,
    Bedrock,
    Vertex,
    OpenaiCompatible,
}

/// Family-specific identity fields. This closed enum prevents misspelled keys
/// from silently doing nothing.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum FamilySetting {
    /// AWS region, which determines the Bedrock endpoint host.
    Region,
    /// GCP project ID used in Vertex model paths.
    Project,
    /// GCP region used in Vertex endpoint hosts and model paths.
    Location,
    /// Optional Azure OpenAI `api-version`; an empty value uses the AI SDK default.
    ApiVersion,
    /// Optional path to the native Claude Code executable for the `ClaudeAgent`
    /// family; an empty value lets the host look in the standard install
    /// locations (`~/.local/bin`, then `PATH`).
    ClaudeExecutable,
}

impl FamilySetting {
    pub fn slug(self) -> &'static str {
        match self {
            Self::Region => "region",
            Self::Project => "project",
            Self::Location => "location",
            Self::ApiVersion => "api_version",
            Self::ClaudeExecutable => "claude_executable",
        }
    }

    /// Sidecar wire name. Keep this aligned with `aisdk-service/src/providers.ts`.
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Region => "region",
            Self::Project => "project",
            Self::Location => "location",
            Self::ApiVersion => "apiVersion",
            Self::ClaudeExecutable => "claudeExecutable",
        }
    }
}

impl ProviderFamily {
    /// All families, for exhaustive tests. Production matches are exhaustive at
    /// compile time; tests need an iterable catalog.
    #[cfg(test)]
    pub const CATALOG: &'static [Self] = &[
        Self::OpenaiResponses,
        Self::OpenaiCodex,
        Self::OpenaiChat,
        Self::Anthropic,
        Self::ClaudeAgent,
        Self::Google,
        Self::Xai,
        Self::Azure,
        Self::Bedrock,
        Self::Vertex,
        Self::OpenaiCompatible,
    ];

    /// Chat endpoint type for this family. This exhaustive match requires new
    /// families to declare one. Vocabulary only: every model is a chat model, so
    /// nothing at runtime picks an endpoint by family any more.
    #[cfg(test)]
    pub fn chat_endpoint(self) -> EndpointType {
        match self {
            Self::OpenaiResponses | Self::OpenaiCodex => EndpointType::OpenaiResponses,
            // XAI and the generic compatibility layer use `/chat/completions`.
            Self::OpenaiChat | Self::Xai | Self::OpenaiCompatible => {
                EndpointType::OpenaiChatCompletions
            }
            // The CLI speaks Messages upstream; the endpoint type is a model
            // vocabulary, not a host-issued request path.
            Self::Anthropic | Self::ClaudeAgent => EndpointType::AnthropicMessages,
            Self::Google | Self::Vertex => EndpointType::GoogleGenerative,
            Self::Azure => EndpointType::AzureOpenai,
            Self::Bedrock => EndpointType::BedrockConverse,
        }
    }

    /// Required identity fields. Missing fields cause recoverable, named runtime
    /// errors rather than unrelated upstream failures.
    pub fn required_settings(self) -> &'static [FamilySetting] {
        match self {
            Self::Bedrock => &[FamilySetting::Region],
            Self::Vertex => &[FamilySetting::Project, FamilySetting::Location],
            Self::OpenaiResponses
            | Self::OpenaiCodex
            | Self::OpenaiChat
            | Self::Anthropic
            // The executable is discovered when the setting is empty.
            | Self::ClaudeAgent
            | Self::Google
            | Self::Xai
            // Azure `api_version` is optional because the AI SDK has a default.
            | Self::Azure
            | Self::OpenaiCompatible => &[],
        }
    }

    /// Whether this family's chat base URL has a host-known default so `base_url`
    /// may be left empty. Vertex and Bedrock derive theirs from identity fields;
    /// Codex has one fixed backend (`codex_oauth::CODEX_DEFAULT_BASE_URL`) and a
    /// non-empty value exists only to point at a loopback test double. Claude
    /// Agent leaves the address to the CLI unless the user overrides it.
    pub fn derives_base_url(self) -> bool {
        matches!(
            self,
            Self::Vertex | Self::Bedrock | Self::OpenaiCodex | Self::ClaudeAgent
        )
    }

    /// Whether this family has an effective `ReasoningContent` request option.
    /// Keep this projection aligned with the sidecar's provider-options mapping.
    pub fn reasoning_content_takes_effect(self) -> bool {
        matches!(self, Self::OpenaiResponses | Self::OpenaiCodex | Self::Azure)
    }

    /// Whether this family's dialect places prompt-cache breakpoints, so the
    /// model's `prompt_cache` attribute reaches the wire. Only the Messages
    /// protocol takes explicit `cache_control` markers; Bedrock's Converse
    /// cache points are a different wire and are not driven by this attribute,
    /// and Claude Agent caches inside the CLI on its own.
    pub fn prompt_cache_takes_effect(self) -> bool {
        matches!(self, Self::Anthropic)
    }

    /// Identity fields this family recognizes, including optional fields.
    pub fn known_settings(self) -> &'static [FamilySetting] {
        match self {
            Self::Bedrock => &[FamilySetting::Region],
            Self::Vertex => &[FamilySetting::Project, FamilySetting::Location],
            Self::Azure => &[FamilySetting::ApiVersion],
            Self::ClaudeAgent => &[FamilySetting::ClaudeExecutable],
            Self::OpenaiResponses
            | Self::OpenaiCodex
            | Self::OpenaiChat
            | Self::Anthropic
            | Self::Google
            | Self::Xai
            | Self::OpenaiCompatible => &[],
        }
    }
}

/// Provider endpoint shape. Endpoint identity, rather than vendor identity,
/// covers shared OpenAI-compatible image and audio paths.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EndpointType {
    OpenaiChatCompletions,
    OpenaiResponses,
    AnthropicMessages,
    GoogleGenerative,
    AzureOpenai,
    BedrockConverse,
    OpenaiImageGeneration,
    OpenaiImageEdit,
    OpenaiTextToSpeech,
    OpenaiAudioTranscription,
}

impl EndpointType {
    /// Catalog order must match the TypeScript mirror. It exists only for
    /// vocabulary and cross-language tests.
    #[cfg(test)]
    pub const CATALOG: &'static [Self] = &[
        Self::OpenaiChatCompletions,
        Self::OpenaiResponses,
        Self::AnthropicMessages,
        Self::GoogleGenerative,
        Self::AzureOpenai,
        Self::BedrockConverse,
        Self::OpenaiImageGeneration,
        Self::OpenaiImageEdit,
        Self::OpenaiTextToSpeech,
        Self::OpenaiAudioTranscription,
    ];

    /// Endpoints that can carry a chat request. Vocabulary only; kept so the
    /// catalog test can still prove it is a proper subset of `CATALOG`.
    #[cfg(test)]
    pub const CHAT: &'static [Self] = &[
        Self::OpenaiChatCompletions,
        Self::OpenaiResponses,
        Self::AnthropicMessages,
        Self::GoogleGenerative,
        Self::AzureOpenai,
        Self::BedrockConverse,
    ];

    pub fn slug(self) -> &'static str {
        match self {
            Self::OpenaiChatCompletions => "openai_chat_completions",
            Self::OpenaiResponses => "openai_responses",
            Self::AnthropicMessages => "anthropic_messages",
            Self::GoogleGenerative => "google_generative",
            Self::AzureOpenai => "azure_openai",
            Self::BedrockConverse => "bedrock_converse",
            Self::OpenaiImageGeneration => "openai_image_generation",
            Self::OpenaiImageEdit => "openai_image_edit",
            Self::OpenaiTextToSpeech => "openai_text_to_speech",
            Self::OpenaiAudioTranscription => "openai_audio_transcription",
        }
    }

    #[cfg(test)]
    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::CATALOG.iter().copied().find(|entry| entry.slug() == slug)
    }
}

/// Explicit model capabilities. They are inferred once for unknown models and
/// then persisted; runtime decisions never re-guess them.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ModelCapability {
    ImageRecognition,
}

impl ModelCapability {
    /// Catalog and chip-rendering order, matched by TypeScript tests.
    #[cfg(test)]
    pub const CATALOG: &'static [Self] = &[Self::ImageRecognition];

    /// This declaration is read only by cross-language tests and must agree with
    /// serde's `snake_case` persistence form.
    #[cfg(test)]
    pub fn slug(self) -> &'static str {
        match self {
            Self::ImageRecognition => "image_recognition",
        }
    }

    #[cfg(test)]
    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::CATALOG.iter().copied().find(|entry| entry.slug() == slug)
    }
}

/// Form in which a model returns reasoning.
///
/// This is a model attribute, not a protocol attribute: compatible Responses
/// endpoints can return either plaintext or encrypted reasoning. Only families
/// with a request-side control currently consume it.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningContent {
    /// Readable reasoning text with no replayable ciphertext.
    #[default]
    Plaintext,
    /// Opaque encrypted reasoning that survives tool turns only by replay.
    Encrypted,
}

impl ReasoningContent {
    /// Selector order, matched by TypeScript tests.
    #[cfg(test)]
    pub const CATALOG: &'static [Self] = &[Self::Plaintext, Self::Encrypted];

    #[cfg(test)]
    pub fn slug(self) -> &'static str {
        match self {
            Self::Plaintext => "plaintext",
            Self::Encrypted => "encrypted",
        }
    }

    #[cfg(test)]
    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::CATALOG.iter().copied().find(|entry| entry.slug() == slug)
    }
}

/// Effective body form of a reasoning card after `ReasoningContent` is
/// resolved. Plaintext cards are editable; encrypted cards are delete-only
/// because their body never reaches local storage.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningForm {
    Plaintext,
    Encrypted,
}

impl From<ReasoningContent> for ReasoningForm {
    fn from(content: ReasoningContent) -> Self {
        match content {
            ReasoningContent::Plaintext => Self::Plaintext,
            ReasoningContent::Encrypted => Self::Encrypted,
        }
    }
}

/// Provider-owned reasoning payload replayed verbatim on later turns.
///
/// Holds the AI SDK reasoning parts (`{ "text", "providerOptions" }`) the
/// producing provider handed back for one card: an Anthropic `signature` or
/// `redactedData`, or a Responses `itemId` plus `reasoningEncryptedContent`.
/// The card's visible `content` is presentation; these parts are the bytes the
/// provider signed, so editing the card cannot invalidate a signature. Absent
/// for cards written before this field existed and for providers that return
/// nothing replayable, in which case history projects the plain card text.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ReasoningReplay {
    /// Model id that produced the payload. Anthropic signatures are bound to
    /// the model, so a switched conversation must not replay them.
    pub model: String,
    /// AI SDK reasoning parts in provider order. Each is
    /// `{ "text": string, "providerOptions": object }`; the host never reads
    /// inside `providerOptions`.
    pub parts: Vec<Value>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    #[default]
    Disabled,
    #[serde(alias = "minimal")]
    Low,
    Medium,
    High,
    Xhigh,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SecurityLevel {
    Plan,
    #[default]
    RequestApproval,
    AllowEdits,
    FullAccess,
}

impl SecurityLevel {
    /// Every level, so exhaustiveness can be asserted at run time where the
    /// compiler cannot (serde names, the `LiveSecurityLevel` byte mapping).
    #[cfg(test)]
    pub const ALL: [SecurityLevel; 4] = [
        SecurityLevel::Plan,
        SecurityLevel::RequestApproval,
        SecurityLevel::AllowEdits,
        SecurityLevel::FullAccess,
    ];

    const fn as_byte(self) -> u8 {
        match self {
            SecurityLevel::Plan => 0,
            SecurityLevel::RequestApproval => 1,
            SecurityLevel::AllowEdits => 2,
            SecurityLevel::FullAccess => 3,
        }
    }

    const fn from_byte(byte: u8) -> SecurityLevel {
        match byte {
            0 => SecurityLevel::Plan,
            2 => SecurityLevel::AllowEdits,
            3 => SecurityLevel::FullAccess,
            // Only `as_byte` writes the cell, so this is the `RequestApproval`
            // arm; an impossible byte falls back to the strictest prompting
            // level rather than widening authority.
            _ => SecurityLevel::RequestApproval,
        }
    }
}

/// The level a live run is executing under right now, as opposed to the level
/// it started with. Plan approval switches the level mid-turn, so every gate
/// that runs after the switch must read this cell rather than the snapshot the
/// run began with.
pub struct LiveSecurityLevel(std::sync::atomic::AtomicU8);

impl LiveSecurityLevel {
    pub fn new(level: SecurityLevel) -> Self {
        Self(std::sync::atomic::AtomicU8::new(level.as_byte()))
    }

    pub fn get(&self) -> SecurityLevel {
        SecurityLevel::from_byte(self.0.load(std::sync::atomic::Ordering::Acquire))
    }

    pub fn set(&self, level: SecurityLevel) {
        self.0
            .store(level.as_byte(), std::sync::atomic::Ordering::Release);
    }
}

impl std::fmt::Debug for LiveSecurityLevel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("LiveSecurityLevel")
            .field(&self.get())
            .finish()
    }
}

/// `RunModelRequest` derives `PartialEq`; compare the level the cell holds,
/// because the atomic itself is not comparable.
impl PartialEq for LiveSecurityLevel {
    fn eq(&self, other: &Self) -> bool {
        self.get() == other.get()
    }
}

/// Where a plan document stands with the user.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    /// Written by the model, not yet presented for approval.
    #[default]
    Draft,
    Approved,
    Rejected,
}

impl PlanStatus {
    /// The SQLite column is a text CHECK constraint, so the stored spelling is
    /// part of the schema rather than an implementation detail of serde.
    pub fn as_str(self) -> &'static str {
        match self {
            PlanStatus::Draft => "draft",
            PlanStatus::Approved => "approved",
            PlanStatus::Rejected => "rejected",
        }
    }

    pub fn from_str(value: &str) -> Option<PlanStatus> {
        match value {
            "draft" => Some(PlanStatus::Draft),
            "approved" => Some(PlanStatus::Approved),
            "rejected" => Some(PlanStatus::Rejected),
            _ => None,
        }
    }
}

/// The plan document one conversation is working from: the markdown the model
/// writes with the `plan` tool and the user reads before approving
/// implementation. At most one per conversation — a write replaces the whole
/// document, so history lives in the timeline rather than here.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConversationPlan {
    pub conversation_id: String,
    pub markdown: String,
    pub status: PlanStatus,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelProfile {
    pub id: String,
    /// Display name. Empty displays `id`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Collapsed model-list group. Empty is inferred from `id`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub group: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    /// Explicit capability set.
    #[serde(default)]
    pub capabilities: BTreeSet<ModelCapability>,
    /// Reasoning return form. Always written explicitly: a record that omits it
    /// is repaired on load by `resolve_reasoning_content`.
    #[serde(default)]
    pub reasoning_content: ReasoningContent,
    /// Whether requests carry Claude Code's prompt-cache breakpoints. Claude
    /// models only cache what the client marks, so this defaults on; a record
    /// that omits it is repaired to `true` on load. Consumed by families where
    /// `ProviderFamily::prompt_cache_takes_effect` holds; elsewhere the value
    /// is stored but has no wire effect.
    #[serde(default = "default_prompt_cache")]
    pub prompt_cache: bool,
}

fn default_prompt_cache() -> bool {
    true
}

impl ModelProfile {
    pub fn has(&self, capability: ModelCapability) -> bool {
        self.capabilities.contains(&capability)
    }

    pub fn set_capability(&mut self, capability: ModelCapability, on: bool) {
        if on {
            self.capabilities.insert(capability);
        } else {
            self.capabilities.remove(&capability);
        }
    }

    pub fn supports_vision(&self) -> bool {
        self.has(ModelCapability::ImageRecognition)
    }

}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HookDefinition {
    pub id: String,
    pub name: String,
    pub event: HookEvent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_windows: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_message: Option<String>,
    pub enabled: bool,
    pub timeout_ms: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    SessionStart,
    InstructionsLoaded,
    UserPromptSubmit,
    PreToolUse,
    PermissionRequest,
    PostToolUse,
    Stop,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HookContextMetadata {
    pub execution_id: String,
    pub hook_id: String,
    pub hook_name: String,
    pub event: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub context_injected: bool,
}

/// Per-tool user text for a conversation or preset. `schema_notes` replaces the
/// built-in model-visible description; `usage_guidance` appends to it. Neither
/// changes parameter schema, permissions, or execution.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolDescriptionEntry {
    pub tool_name: String,
    #[serde(default)]
    pub schema_notes: String,
    #[serde(default)]
    pub usage_guidance: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConversationPreset {
    pub id: String,
    pub name: String,
    pub description: String,
    pub settings: ConversationPresetSettings,
}

/// Reusable settings owned by a conversation preset. Resource IDs are stored
/// directly rather than through preset layers.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConversationPresetSettings {
    pub system_prompt: String,
    pub enabled_tools: Vec<String>,
    /// At most one selected description file. A missing file falls back to the
    /// built-in descriptions.
    #[serde(default)]
    pub tool_description_file_id: Option<String>,
    /// Subagent roles. Models select only `name`; source, model, and memory
    /// identity are never accepted from `agent_spawn`.
    #[serde(default)]
    pub agent_definitions: Vec<AgentDefinition>,
    /// Template for allowing roleless subagents. A missing key means roles are
    /// required.
    #[serde(default)]
    pub allow_roleless_subagents: bool,
    /// Directly selected capability resource IDs.
    #[serde(default)]
    pub hook_ids: Vec<String>,
    #[serde(default)]
    pub skill_ids: Vec<String>,
    #[serde(default)]
    pub mcp_ids: Vec<String>,
    /// Web-search behavior template copied into a new conversation.
    #[serde(default)]
    pub web_search: ConversationWebSearchSettings,
    /// Security-policy template copied into a new conversation.
    #[serde(default)]
    pub security_level: SecurityLevel,
    /// Memory-tier switches copied into a new conversation. Each enabled tier
    /// contributes its instructions, index, and tools.
    #[serde(default)]
    pub global_memory_enabled: bool,
    #[serde(default)]
    pub project_memory_enabled: bool,
    /// On-demand skill-delivery template. A missing key delivers skill bodies
    /// in the initial system prompt.
    #[serde(default)]
    pub skill_tool_enabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityCatalog {
    #[serde(default)]
    pub hooks: Vec<ResourceDescriptor>,
    pub skills: Vec<ResourceDescriptor>,
    pub mcps: Vec<ResourceDescriptor>,
    /// Read-only tool-description files discovered on disk. The app applies the
    /// selected file's content but never edits these resources.
    #[serde(default)]
    pub tool_description_files: Vec<ResourceDescriptor>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResourceDescriptor {
    pub id: String,
    pub name: String,
    pub description: String,
    pub location: String,
    pub source: ResourceSource,
    pub available: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ResourceSource {
    Builtin,
    User,
    Workspace,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub kind: WorkspaceKind,
    pub path: String,
    pub created_at: String,
    /// Preset forced for workspace-created conversations. Empty or dangling IDs
    /// use `last_conversation_settings`.
    #[serde(default)]
    pub default_conversation_preset_id: String,
    /// Most recent conversation settings snapshot for this workspace. The
    /// renderer writes it; the host only persists and validates it.
    #[serde(default)]
    pub last_conversation_settings: Option<ConversationSettings>,
    pub conversations: Vec<Conversation>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceKind {
    #[default]
    Directory,
    #[serde(alias = "none")]
    Temporary,
    #[serde(other)]
    Unsupported,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UserAbortedTaskMetrics {
    pub child_count: Option<u64>,
    pub tokens: Option<u64>,
    pub tool_count: Option<u64>,
    pub elapsed_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UserAbortedTaskRecord {
    pub id: String,
    pub source_kind: String,
    pub source_identity: String,
    pub label: String,
    pub detail: String,
    pub metrics: UserAbortedTaskMetrics,
    pub started_at: String,
    pub ended_at: String,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub settings: ConversationSettings,
    pub contexts: Vec<ContextItem>,
    /// Follow-up messages submitted while a model turn is running. They are
    /// persisted before execution so cancellation or reload never drops them.
    #[serde(default)]
    pub queued_messages: Vec<QueuedMessage>,
    #[serde(default)]
    pub branches: Vec<ConversationBranch>,
    #[serde(default)]
    pub user_aborted_tasks: Vec<UserAbortedTaskRecord>,
    /// Isolated Git worktree for this conversation. It belongs on the
    /// conversation because copying settings must not copy a worktree path.
    #[serde(default)]
    pub worktree: Option<ConversationWorktree>,
    /// Shell execution location. `None` means local execution. It belongs on the
    /// conversation because settings copies must not copy an SSH machine binding.
    #[serde(default)]
    pub run_target: Option<RunTarget>,
    /// The conversation this one was forked from. `None` is a top-level
    /// conversation. Nesting is a renderer concept: the child's permissions
    /// come from its own `settings`. Lives on the conversation, not in
    /// `settings`, for the same reason as `worktree`: settings are copied
    /// wholesale by presets and workspace snapshots.
    #[serde(default)]
    pub parent_conversation_id: Option<String>,
}

/// Shell execution location selected by a conversation. SSH stores only a stable
/// catalog ID; connection settings are resolved at dispatch time.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RunTarget {
    /// Execute in a WSL distribution addressed by its current name.
    #[serde(rename_all = "camelCase")]
    Wsl { distro: String },
    /// Execute on a user-configured SSH machine.
    #[serde(rename_all = "camelCase")]
    Ssh { machine_id: String },
}

/// Isolated conversation worktree. Branch and baseline are both required for
/// safe release and extra-commit detection.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConversationWorktree {
    pub path: String,
    pub branch: String,
    pub base_oid: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct QueuedMessage {
    pub id: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageAttachment>,
    pub created_at: String,
}

/// One suffix choice after a user-message fork point. The active slot is an
/// empty marker; its live suffix remains in `Conversation::contexts`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConversationBranch {
    pub id: String,
    pub fork_context_id: String,
    pub active: bool,
    #[serde(default)]
    pub contexts: Vec<ContextItem>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSettings {
    pub system_prompt: String,
    #[serde(default)]
    pub include_app_data_path: bool,
    pub enabled_tools: Vec<String>,
    /// Directly selected capability resource IDs.
    #[serde(default)]
    pub hook_ids: Vec<String>,
    #[serde(default)]
    pub skill_ids: Vec<String>,
    #[serde(default)]
    pub mcp_ids: Vec<String>,
    /// At most one selected description file; `None` uses built-in descriptions.
    #[serde(default)]
    pub tool_description_file_id: Option<String>,
    /// Subagent roles selectable by model name.
    #[serde(default)]
    pub agent_definitions: Vec<AgentDefinition>,
    /// Whether subagents may omit a named role.
    ///
    /// When disabled, `agent_spawn` and workflow steps require a role. When no
    /// role is available, the host treats this as enabled: requiring an impossible
    /// value would make every call fail. A missing key means disabled.
    #[serde(default)]
    pub allow_roleless_subagents: bool,
    /// Conversation search behavior, initially copied from its preset template.
    #[serde(default)]
    pub web_search: ConversationWebSearchSettings,
    #[serde(default)]
    pub reasoning_effort: ReasoningEffort,
    #[serde(default)]
    pub security_level: SecurityLevel,
    /// Whether this conversation loads the global memory tier (`~/.mework`).
    ///
    /// When true, that tier's `MEWORK.md` instructions and `MEMORY.md` index
    /// are concatenated into the context and its three tools
    /// (`read`/`create`/`edit_global_memory`) are exposed. When false, the tier
    /// is not read, nothing is injected, and those three tools are withheld —
    /// so the conversation cannot observe or modify global memory at all.
    ///
    /// A missing key means OFF. Memory reads and writes files the user may not
    /// expect a fresh conversation to touch, so the built-in engineering
    /// defaults preset — not a serde fallback — is where "on" is decided.
    #[serde(default)]
    pub global_memory_enabled: bool,
    /// Whether this conversation loads the project memory tier
    /// (`<workspace>/.mework`). Independent of [`Self::global_memory_enabled`]
    /// in both directions: either, both or neither may be on.
    #[serde(default)]
    pub project_memory_enabled: bool,
    /// Skill delivery mode for this conversation. When disabled, all skill bodies
    /// enter the system prompt at turn start; when enabled, the `skill` tool
    /// supplies bodies on demand. Both modes use the same `skill_ids`.
    #[serde(default)]
    pub skill_tool_enabled: bool,
}

/// Skill resolved from disk during trusted request construction. The model sees
/// only its name and trigger; body and directory remain host-only until use.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResolvedSkill {
    /// Name used by the model and displayed in the catalog.
    pub name: String,
    /// Trigger text explaining when to use the skill.
    pub trigger: String,
    /// `SKILL.md` body with front matter removed.
    pub body: String,
    /// Absolute skill directory for resolving relative body references.
    pub directory: String,
}

/// One provider-cited web source on an assistant round (server-side search /
/// grounding). Wire and store shape are identical; the renderer shows these as
/// citation chips under the prose.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ContextSource {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ContextItem {
    System {
        id: String,
        content: String,
        /// Local lifecycle diagnostics that must never be promoted into model input.
        #[serde(rename = "localOnly", default, skip_serializing_if = "is_false")]
        local_only: bool,
        #[serde(
            rename = "hookExecution",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        hook_execution: Option<HookContextMetadata>,
        #[serde(rename = "createdAt")]
        created_at: String,
    },
    User {
        id: String,
        content: String,
        /// Provider-neutral references into the external image attachment store.
        /// Raw bytes/base64 never enter the conversation document.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<ImageAttachment>,
        #[serde(rename = "createdAt")]
        created_at: String,
    },
    Assistant {
        id: String,
        content: String,
        /// One-based model request round. Manual and legacy assistant contexts omit it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        round: Option<usize>,
        /// Stable local association for every canonical item emitted by this model round.
        #[serde(
            rename = "modelTurnId",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        model_turn_id: Option<String>,
        /// A locally retained fragment from an interrupted stream. It remains visible but is
        /// never projected back into provider history.
        #[serde(default, skip_serializing_if = "is_false")]
        interrupted: bool,
        /// Web sources the provider cited for this round (server-side search /
        /// grounding). Empty for providers that don't cite; never projected
        /// back into model input — citations are a UI fact, not history.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        sources: Vec<ContextSource>,
        #[serde(rename = "createdAt")]
        created_at: String,
    },
    Reasoning {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        /// Form of this reasoning card. It is resolved from the producing
        /// model's `ReasoningContent`, not inferred from empty content, because
        /// a plaintext model may produce no reasoning text.
        ///
        /// Absence identifies cards written before this field and uses the legacy
        /// empty-content heuristic. The old `encrypted` boolean is intentionally
        /// ignored rather than reused because it had incompatible semantics.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        form: Option<ReasoningForm>,
        /// One-based model request round. Manual and legacy reasoning contexts omit it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        round: Option<usize>,
        /// Stable local association for every canonical item emitted by this model round.
        #[serde(
            rename = "modelTurnId",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        model_turn_id: Option<String>,
        /// A locally retained fragment from an interrupted stream. It remains visible but is
        /// never projected back into provider history.
        #[serde(default, skip_serializing_if = "is_false")]
        interrupted: bool,
        /// Wall-clock milliseconds the provider spent inside this round's
        /// reasoning items, measured by the sidecar.
        ///
        /// Present even when `content` is absent: a Responses round that only
        /// returns `encrypted_content` emits no summary text at all, and this
        /// field plus `tokens` is the entire visible trace of it.
        #[serde(
            rename = "durationMs",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        duration_ms: Option<u64>,
        /// Provider-reported reasoning tokens for this round. A subset of the
        /// round's output tokens; see [`ModelUsage::reasoning_tokens`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tokens: Option<u64>,
        /// Signed or encrypted provider payload replayed on later turns. See
        /// [`ReasoningReplay`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replay: Option<ReasoningReplay>,
        #[serde(rename = "createdAt")]
        created_at: String,
    },
    Tool {
        id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        /// One-based model request round. Manual and legacy tool contexts omit it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        round: Option<usize>,
        /// Stable local association for every canonical item emitted by this model round.
        #[serde(
            rename = "modelTurnId",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        model_turn_id: Option<String>,
        /// Model-requested arguments before hooks changed the executed arguments.
        #[serde(
            rename = "requestedInput",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        requested_input: Option<JsonObject>,
        input: JsonObject,
        result: ToolResult,
        /// Full child transcript and progress updates for a `subagent` call.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent: Option<SubagentRunRecord>,
        /// Host proof that this card's result came from this application's own
        /// execution rather than from the renderer.
        ///
        /// Issued when the card is built and carried by the renderer like any
        /// other field. It replaces a process-local map of executed payloads,
        /// which could not survive a restart and could not hold more than a few
        /// hundred entries — both of which left ordinary cards permanently
        /// unsaveable. Absent on cards the host does not attest (a manually
        /// inserted card, or one written before this field existed), which are
        /// quarantined at save time rather than trusted.
        #[serde(
            rename = "attestation",
            default,
            skip_serializing_if = "String::is_empty"
        )]
        attestation: String,
        #[serde(rename = "createdAt")]
        created_at: String,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SubagentRunStatus {
    Completed,
    /// Legacy catch-all, still written when the host cancels an agent at turn
    /// end and nothing more specific is known. Documents predating the split
    /// carry only this and `Completed`.
    Interrupted,
    /// The child's provider call errored. Retryable in principle.
    Failed,
    /// Deliberately halted — an explicit `agent_stop`, a hook stop, or a
    /// revoked definition. Retrying reproduces the stop.
    Stopped,
    /// Truncated by a host round/tool ceiling rather than finishing. The output
    /// is partial but valid, which is why it must not read as a failure.
    // `rename_all = "lowercase"` inserts no separator, so the wire value would
    // be "roundlimit"; the renderer union spells it camelCase like every other
    // multi-word wire string.
    #[serde(rename = "roundLimit")]
    RoundLimit,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SubagentRunKind {
    #[default]
    General,
    /// One step of a `workflow` run. The synthesized parent record carries one
    /// `workflow_step` tool context per step; the step itself is an ordinary
    /// child run inside the workflow's private pool.
    WorkflowStep,
    /// A `bash`/`powershell` command the model explicitly backgrounded
    /// (`run_in_background`). The pool entry is lifecycle
    /// plumbing only — the worker polls the OS process instead of running a
    /// model, the model addresses the task as `shell:<id>`, and no subagent
    /// record is backfilled (the shell-task registry row and the folded result
    /// notification are the durable surfaces).
    ShellCommand,
    /// **Retired:** `web_search` is an ordinary concurrent async tool whose
    /// result comes back as its own tool result. Nothing constructs this variant.
    ///
    /// The variant remains because persisted `SubagentRunRecord`s from previously
    /// saved conversations may carry `"webSearch"` on disk. Deleting it would make
    /// those documents unreadable; retired mechanisms must not brick archives.
    WebSearch,
}


#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SubagentUpdate {
    pub content: String,
    pub created_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SubagentRunRecord {
    /// Host-selected role. Web research is never selectable through the
    /// generic agent_spawn arguments and uses a narrower capability profile.
    #[serde(default)]
    pub kind: SubagentRunKind,
    /// Stable per-conversation agent name used by messaging and `agent_wait`.
    /// Absent on legacy `subagent` records, which are not continuable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The model-visible task address of a managed run (`workflow:<runId>`),
    /// or a workflow step's display label. Pool names (`aN`/`wsN`) are host
    /// pipeline internals; this is what `task_list` prints for the run after
    /// its turn ended. Absent on ordinary subagents — their address IS `name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// `agent_spawn context:"conversation"` is a fork: it receives the
    /// parent's host-generated auto-memory block and lease. Ordinary named
    /// agents remain isolated. Persisting this bit lets a continued fork
    /// reacquire the current parent snapshot after reload without serializing
    /// the host-only lease itself.
    #[serde(default, skip_serializing_if = "is_false")]
    pub inherits_model_memory: bool,
    /// Exact trusted provider/model and keyed auto-memory snapshot receipt for
    /// a conversation fork. New fork records have this field together with
    /// `inherits_model_memory`; ordinary and named agents never have it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_model_binding: Option<ForkModelBinding>,
    /// Exact trusted named-agent definition/model binding. Ordinary agents
    /// have neither this field nor inherited memory; conversation forks use
    /// `inherits_model_memory` and never this binding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_definition: Option<AgentDefinitionBinding>,
    /// Keyed receipt over the owning conversation, exact addressable name,
    /// ordinary/fork/named execution mode and its trusted binding. Records
    /// without a valid receipt remain visible as legacy history but cannot be
    /// continued.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub execution_mode_receipt: String,
    pub task: String,
    pub status: SubagentRunStatus,
    pub contexts: Vec<ContextItem>,
    pub updates: Vec<SubagentUpdate>,
    /// Parent messages not yet consumed by a child turn. Queue-only messages
    /// remain dormant; follow-up tasks wake the child when resumed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queued_messages: Vec<QueuedSubagentMessage>,
    /// The latest value this agent returned through `structured_output`.
    /// `#[serde(default)]` is mandatory: `task`/`status`/`contexts`/`updates`
    /// above carry no serde attributes and are required keys, so every existing
    /// conversation document would fail to load without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_output: Option<Value>,
    /// The spawn-time `output_schema` document of a schema-bound run.
    ///
    /// `RunModelRequest.output_schema` is `#[serde(skip)]` and the live
    /// `AgentPool` is turn-scoped, so without this field a continuation in a
    /// later turn rehydrates schema-less and quietly finishes with prose —
    /// exactly what the schema existed to prevent. The raw document (not a
    /// compiled form) is persisted because the record is renderer-writable:
    /// rehydration re-runs `orchestration::compile_output_schema`, bounding a
    /// forged value like a fresh spawn's, and refuses the continuation when the
    /// precheck fails. Deleting the key instead of forging it unbinds the
    /// continuation without a signal — that is deliberate scope, not an
    /// oversight: record content is unattested by design (a schema constrains
    /// output and grants nothing), so removal is exactly as powerful as any
    /// other edit of the record's contexts. Absent on runs spawned without a
    /// schema and on schema-bound records that predate this field; those records
    /// stay continuable but — unrecoverably — schema-less.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
    /// Tokens this agent alone consumed across every one of its turns.
    ///
    /// The parent's own `usage` is a turn total that already absorbed these
    /// numbers through `take_usage`, so this is a separate never-taken copy
    /// rather than a view of the same counter — the task sidebar needs the
    /// per-agent figure to survive both the drain and a reload. Defaulted
    /// because records that predate this field lack it.
    #[serde(default, skip_serializing_if = "is_empty_usage")]
    pub usage: ModelUsage,
}

fn is_empty_usage(usage: &ModelUsage) -> bool {
    usage == &ModelUsage::default()
}

/// Frozen v1 projection of [`ForkModelBinding`] for the execution-mode receipt.
///
/// The receipt payload must never widen implicitly. Serialising the live struct
/// put every future field into the signed bytes, and a changed payload does not
/// merely drop a record to view-only — `reserve_subagent_execution_mode_receipt`
/// has no update path and hard-errors `subagent name is permanently reserved to
/// another execution mode`, permanently burning that (conversation, name) pair.
///
/// Field list and order are byte-frozen. NEVER add, reorder or rename a field
/// here: extend the payload with a v2 projection plus a recorded version instead.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ForkModelBindingV1<'a> {
    provider_id: &'a str,
    model_id: &'a str,
    memory_language: ResolvedLanguage,
    memory_tool_names: &'a [String],
    system_prompt_snapshot: &'a str,
    system_prompt_receipt: &'a str,
    /// Omitted entirely — not `null` — when absent, reproducing the original
    /// `skip_serializing_if = "Option::is_none"` on the live struct.
    #[serde(skip_serializing_if = "Option::is_none")]
    memory_snapshot_receipt: Option<&'a str>,
    binding_receipt: &'a str,
}

impl<'a> From<&'a ForkModelBinding> for ForkModelBindingV1<'a> {
    fn from(binding: &'a ForkModelBinding) -> Self {
        Self {
            provider_id: binding.provider_id.as_str(),
            model_id: binding.model_id.as_str(),
            memory_language: binding.memory_language,
            memory_tool_names: binding.memory_tool_names.as_slice(),
            system_prompt_snapshot: binding.system_prompt_snapshot.as_str(),
            system_prompt_receipt: binding.system_prompt_receipt.as_str(),
            memory_snapshot_receipt: binding.memory_snapshot_receipt.as_deref(),
            binding_receipt: binding.binding_receipt.as_str(),
        }
    }
}

/// Frozen v1 projection of [`AgentDefinitionBinding`]. See
/// [`ForkModelBindingV1`] for why this exists and why it must not grow.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentDefinitionBindingV1<'a> {
    source: AgentDefinitionSource,
    source_key: &'a str,
    name: &'a str,
    revision: u64,
    memory_epoch: u64,
    provider_id: &'a str,
    model_id: &'a str,
    memory: AgentDefinitionMemory,
    scope_key: &'a str,
    configuration_receipt: &'a str,
}

impl<'a> From<&'a AgentDefinitionBinding> for AgentDefinitionBindingV1<'a> {
    fn from(binding: &'a AgentDefinitionBinding) -> Self {
        Self {
            source: binding.source,
            source_key: binding.source_key.as_str(),
            name: binding.name.as_str(),
            revision: binding.revision,
            memory_epoch: binding.memory_epoch,
            provider_id: binding.provider_id.as_str(),
            model_id: binding.model_id.as_str(),
            memory: binding.memory,
            scope_key: binding.scope_key.as_str(),
            configuration_receipt: binding.configuration_receipt.as_str(),
        }
    }
}

/// Canonical host-only identity for one addressable child execution mode.
/// The public agent name and owning conversation are part of the payload, as
/// are the complete trusted fork/named bindings. Model arguments never supply
/// this value.
///
/// The bindings are projected through frozen v1 views rather than serialised
/// directly, so adding a field to `ForkModelBinding` or `AgentDefinitionBinding`
/// cannot change these bytes. The output stays a six-element JSON array whose
/// binding elements are camelCase objects.
///
/// # There is no v2 — and a frozen field SET is not frozen BYTES
///
/// A binding's `receipt_version` selects which payload
/// `api::fork_model_binding_receipt_payload` /
/// `api::agent_definition_receipt_payload` sign, and it must never be
/// enumerated here. There is no v2 of this payload and there must not be one:
/// `MemoryStore::reserve_subagent_execution_mode_receipt` has only insert /
/// identical-no-op / hard-error branches, no UPDATE and no production DELETE.
/// A move in these bytes therefore burns any `(conversation_id, name)` pair that
/// already holds a row under the old bytes, with `subagent name is permanently
/// reserved to another execution mode`. It is not unconditional: a name with no
/// row yet is simply reserved at the new bytes, and the overwhelmingly common
/// case — a name whose run persisted — is re-spawned under a FRESH name by the
/// dedupe gate. What the burn needs is the orphan window described below.
///
/// That closes the SHAPE route only. The projections freeze a field SET over
/// PER-SPAWN VALUES, so the property that actually has to hold is: two spawn
/// attempts at the same `(conversation_id, name)` must project identical
/// VALUES. Every projected value is a route into these bytes.
///
/// Stable by construction — this is what bounds the blast radius.
/// `conversation_id` and `name` ARE the reservation key. `kind` is fixed per
/// call site (`api::run_agent_spawn` always passes `General`). And both binding
/// elements are `None` for an ordinary non-fork spawn (`api::agent_child_template`
/// sets `inherits_parent_model_memory`, `fork_model_binding` and
/// `agent_definition_binding` off, and only the `context: "conversation"` /
/// `agent_type` arms fill them in) and for every web-search worker
/// (`api::web_research_executor_template`, same three fields), leaving the
/// binding-free `[conv, name, kind, false, null, null]`. The search stack
/// re-reserves `rx1` in turn after turn and survives only because of that.
///
/// Volatile between two attempts at the same name, every one of them an
/// ordinary user or model action: `inherits_model_memory` (fork vs plain spawn
/// is a per-call model choice); a fork's `providerId`/`modelId` (the parent's
/// CURRENT provider/model); `memoryLanguage` and `memoryToolNames`
/// (`api::request_memory_language`, `api::enabled_memory_tool_names`);
/// `systemPromptSnapshot`/`systemPromptReceipt` (the parent's assembled prompt
/// plus `subagent_prompt::render(PromptVersion::current(..), ..)`); a named
/// binding's `source`/`sourceKey`/`name` (WHICH definition the model resolved,
/// not just its content) together with `revision`, `memoryEpoch`,
/// provider/model, `memory` and `scopeKey` (any definition edit); and
/// `bindingReceipt`/`configurationReceipt`, whose values are HMACs over a
/// version-DISPATCHED payload — which makes bumping
/// `api::CURRENT_FORK_BINDING_RECEIPT_VERSION` or
/// `api::CURRENT_DEFINITION_RECEIPT_VERSION` ONE instance of this rule, not the
/// rule. `configurationReceipt` additionally carries a transitive CONTENT route:
/// its payload covers `system_prompt`, `enabled`, `deleted` and `model_selection`,
/// none of which appear in the projection, so editing a definition's prompt moves
/// these bytes without moving any field you can see here.
///
/// `memorySnapshotReceipt` is the one projected field that is `Option` with
/// `skip_serializing_if`, so it does not merely change VALUE between attempts —
/// its presence or absence adds or removes a key and changes the payload SHAPE.
/// A fork of a parent carrying an auto-memory snapshot and one without therefore
/// cannot share a name, independently of every value route above.
///
/// # Why a value change USED to be fatal: the reserve-before-persist window
///
/// `api::sign_subagent_execution_mode` writes the reservation row BEFORE
/// `AgentPool::register` and before the turn's tool result is persisted. A turn
/// that dies in between leaves the name held by a row that the spawn dedupe
/// gate's other sources — persisted records, the branch-wide
/// `subagent_reserved_names` assembled in `lib.rs`, the live pool — cannot see,
/// while `AgentPool::auto_name` restarts deterministically at `a1`. The row has
/// no UPDATE and no production DELETE, so the next spawn landing on that name
/// either matched byte-for-byte (silent no-op) or errored FOREVER — and since
/// auto-naming kept choosing it, the conversation lost `agent_spawn` outright
/// rather than losing one name.
///
/// Every volatile input above can change while the durable reservation remains
/// invisible, so the spawn gate must account for that reservation.
///
/// **The spawn gate closes this window (S7):**
/// `api::run_agent_spawn` reads
/// `memory::MemoryStore::reserved_subagent_execution_mode_names` and feeds it to
/// both the explicit-name check and `auto_name`. Every route into these bytes is
/// covered at once, because the fix is about the NAME being invisible, not about
/// any particular value moving. Resume never re-issues anyway (it re-verifies the
/// STORED receipt). The deliberate exception is `api::web_research_executor_name`,
/// which must keep naming from the live pool alone — a search group re-reserves
/// `rx1` turn after turn by design and lives on the identical-no-op branch.
///
/// Guards. `tests::the_execution_mode_payload_depends_on_the_binding_receipt_value`
/// pins the receipt-value route so the receipts cannot later be dropped from
/// the payload as inert.
/// `tests::a_new_binding_field_cannot_reach_the_frozen_payload` proves FIELD-SET
/// immunity and nothing more: it varies `receipt_version`, which the projections
/// exclude, so it stays green through every value route above. The issuance
/// versions are checked against their dispatchers by
/// `api::tests::receipt_payload_dispatchers_support_exactly_their_issued_versions`,
/// the prompt version by
/// `subagent_prompt::tests::the_current_prompt_version_is_pinned_by_the_fork_reservation`,
/// and the window closure itself by
/// `api::tests::orphan_reservation_rows_are_visible_to_the_spawn_dedupe_gate` —
/// which is the one to look at first if a burn is ever observed again.
pub(crate) fn canonical_subagent_execution_mode_payload(
    conversation_id: &str,
    name: &str,
    kind: SubagentRunKind,
    inherits_model_memory: bool,
    fork_model_binding: Option<&ForkModelBinding>,
    agent_definition: Option<&AgentDefinitionBinding>,
) -> Result<String, String> {
    if inherits_model_memory != fork_model_binding.is_some()
        || (agent_definition.is_some() && (inherits_model_memory || fork_model_binding.is_some()))
    {
        return Err("子代理执行模式字段互相冲突，无法签发宿主回执".into());
    }
    serde_json::to_string(&(
        conversation_id,
        name,
        kind,
        inherits_model_memory,
        fork_model_binding.map(ForkModelBindingV1::from),
        agent_definition.map(AgentDefinitionBindingV1::from),
    ))
    .map_err(|error| format!("无法编码子代理执行模式回执: {error}"))
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct QueuedSubagentMessage {
    pub content: String,
    #[serde(default)]
    pub trigger_turn: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn default_true() -> bool {
    true
}

fn default_agent_definition_epoch() -> u64 {
    1
}

impl ContextItem {
    pub fn id(&self) -> &str {
        match self {
            Self::System { id, .. }
            | Self::User { id, .. }
            | Self::Assistant { id, .. }
            | Self::Reasoning { id, .. }
            | Self::Tool { id, .. } => id,
        }
    }

    /// Serde `kind` tag value, stored separately so persistence can identify a
    /// row without parsing JSON.
    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::System { .. } => "system",
            Self::User { .. } => "user",
            Self::Assistant { .. } => "assistant",
            Self::Reasoning { .. } => "reasoning",
            Self::Tool { .. } => "tool",
        }
    }

    pub fn round(&self) -> Option<usize> {
        match self {
            Self::Assistant { round, .. }
            | Self::Reasoning { round, .. }
            | Self::Tool { round, .. } => *round,
            Self::System { .. } | Self::User { .. } => None,
        }
    }

    pub fn model_turn_id(&self) -> Option<&str> {
        match self {
            Self::Assistant { model_turn_id, .. }
            | Self::Reasoning { model_turn_id, .. }
            | Self::Tool { model_turn_id, .. } => model_turn_id.as_deref(),
            Self::System { .. } | Self::User { .. } => None,
        }
    }

    pub fn created_at(&self) -> &str {
        match self {
            Self::System { created_at, .. }
            | Self::User { created_at, .. }
            | Self::Assistant { created_at, .. }
            | Self::Reasoning { created_at, .. }
            | Self::Tool { created_at, .. } => created_at,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ImageAttachment {
    /// Full lowercase SHA-256 digest of the image bytes.
    pub id: String,
    pub name: String,
    pub mime: String,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
    /// Conversation-local number the model cites as `[Image #N]` and passes to
    /// the playwright `upload_image` action. Numbers are allocated by scanning the transcript
    /// for the current maximum and are never reused within a conversation, so
    /// an instance keeps its number even after other images are removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_id: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolResult {
    pub success: bool,
    pub output: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageAttachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    pub executed_at: String,
    pub duration_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolDescriptor {
    pub name: String,
    pub label: String,
    pub description: String,
    pub category: ToolCategory,
    pub dangerous: bool,
    pub parameters: Vec<ToolParameter>,
    /// Raw JSON Schema for dynamically discovered tools such as MCP. Built-in tools continue to
    /// derive their schemas from the typed parameter list below.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<Value>,
    /// This particular approval must be answered by a human, whatever the
    /// classifier says about the tool.
    ///
    /// Host-set and `#[serde(skip)]`, exactly like `subagent_name`: the renderer
    /// supplies `tools` on a run request, so a field it could set would be a
    /// hole rather than a guard. Only the run loop turns it on, and only on the
    /// **clone** it hands to one approval call.
    ///
    /// It exists because a Hook `permissionDecision: ask` forces a confirmation
    /// at the call site, while the approval closure recomputes "is this
    /// mandatory" from the ordinary classifier alone. Without the bit, a
    /// A standing allow decision for a tool must not bypass a hook-forced
    /// confirmation.
    #[serde(skip)]
    pub force_confirmation: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ToolCategory {
    Filesystem,
    Shell,
    /// Everything that reaches the network: `web_search` (dispatched by the
    /// trusted model loop so renderer input can never select the search
    /// provider, its credentials, or its endpoint) and the `playwright`
    /// browser tool.
    Web,
    /// Host-owned coordination: the subagent tools, `workflow`, the `todo`
    /// state tool, and `ask_user`. Every member is executed by the
    /// model run loop itself, never by the manual tool executor.
    Orchestration,
    /// Model-owned long-term memory. The model identity and workspace scope are
    /// injected by the trusted host request and are never accepted as tool
    /// arguments.
    Memory,
    /// Tools discovered from an enabled MCP server. They are executable only inside the trusted
    /// model loop and use a dedicated approval path.
    Mcp,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolParameter {
    pub name: String,
    pub label: String,
    #[serde(rename = "type")]
    pub parameter_type: ToolParameterType,
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_value: Option<Value>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ToolParameterType {
    String,
    Number,
    Boolean,
    Multiline,
    Json,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecutionRequest {
    pub conversation_id: String,
    pub workspace_path: String,
    pub tool_name: String,
    pub input: JsonObject,
}

pub type ToolExecutionResponse = ToolResult;

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RunModelRequest {
    pub provider: ApiProvider,
    /// Backend-owned web-search configuration. Renderer IPC is never allowed
    /// to choose an external endpoint, credential binding or browser policy.
    #[serde(skip)]
    pub web_search: WebSearchSettings,
    /// Shared main/child session counter. Renderer input is discarded.
    /// True only for the host-minted one-shot request that `web_search`
    /// dispatches. It is the sole gate that attaches the provider-native
    /// `web_search` server tool to a wire request — the one tool in the
    /// codebase that reaches the wire without a `ToolDescriptor`. Renderer IPC
    /// and model arguments cannot set it; only the host template does.
    #[serde(skip)]
    pub native_search_call: bool,
    /// Trusted copy of this conversation's two memory-tier switches, hydrated
    /// from the persisted document at turn start. Renderer IPC cannot assert
    /// them and the model cannot pass them as tool arguments, so a tier the
    /// user left off can neither be handed to the model as a snapshot nor
    /// reached through a tool call.
    #[serde(skip)]
    pub global_memory_enabled: bool,
    #[serde(skip)]
    pub project_memory_enabled: bool,
    /// Trusted skill bodies exposed to this run through the `skill` tool,
    /// resolved from the persisted conversation's `skill_ids` at turn start.
    ///
    /// Host-only for the same reason the two memory switches are: the renderer
    /// must not be able to hand a run a body it never selected, and the model
    /// must not be able to name a path instead of a skill. Empty whenever the
    /// conversation left `skill_tool_enabled` off — those bodies went into the
    /// system prompt instead, and carrying them here too would send them twice.
    #[serde(skip)]
    pub skills: Vec<ResolvedSkill>,
    /// Forced result shape for this run, resolved from the spawning
    /// `agent_spawn` call. `#[serde(skip)]` following the host-only pattern
    /// above: the renderer must not be able to assert a schema on a run, since
    /// the schema decides whether the run is allowed to finish at all.
    ///
    /// It carries a [`crate::subagent_schema::Schema`], not a raw `Value`, so
    /// every consumer knows the validity precheck already ran.
    #[serde(skip)]
    pub output_schema: Option<crate::subagent_schema::Schema>,
    pub model: ModelProfile,
    #[serde(default, alias = "thinkingEffort")]
    pub reasoning_effort: ReasoningEffort,
    pub conversation_id: String,
    /// Stable current workspace identity resolved from the persisted document.
    /// This is host-only policy: renderer input and model tool arguments can
    /// never select another project's memory namespace.
    #[serde(skip)]
    pub workspace_id: String,
    /// Exact host-created ephemeral context carrying the two-tier Markdown
    /// memory block. Refresh removes only the context it previously created,
    /// so marker text in unrelated instructions is never mistaken for it.
    #[serde(skip)]
    pub memory_context_id: Option<String>,
    /// Exact host-created startup context carrying trusted project/user
    /// instructions. It is distinct from model-owned memory capability and
    /// lets refresh remove only the context it previously created.
    #[serde(skip)]
    pub project_memory_context_id: Option<String>,
    /// Exact trusted named-agent definition selected for this child. It is
    /// persisted on the subagent record, but never accepted through renderer
    /// IPC or exposed to model tool arguments.
    #[serde(skip)]
    pub agent_definition_binding: Option<AgentDefinitionBinding>,
    /// Explicit fork identity. Never infer this from either main-model or
    /// named-agent memory leases: all three child modes are distinct.
    #[serde(skip)]
    pub inherits_parent_model_memory: bool,
    /// Host-only exact provider/model and auto-memory snapshot receipt selected
    /// for a conversation fork. Renderer IPC and model arguments cannot
    /// manufacture or replace this persisted binding.
    #[serde(skip)]
    pub fork_model_binding: Option<ForkModelBinding>,
    /// Host-only receipt copied into persisted addressable subagent records.
    /// It is generated after the exact ordinary/fork/named child mode has
    /// been selected and is never accepted from renderer IPC.
    #[serde(skip)]
    pub subagent_execution_mode_receipt: Option<String>,
    /// Complete host-derived set of addressable subagent names already used
    /// anywhere in this conversation's active or inactive branch tree.
    /// Names are permanently reserved so a receipt from one branch cannot be
    /// replayed to downgrade another branch's fork/named execution mode.
    #[serde(skip)]
    pub subagent_reserved_names: Vec<String>,
    /// Host-generated top-level run identifier used only for content-free
    /// memory audit correlation. It is never accepted from renderer IPC or
    /// exposed to model tool schemas.
    #[serde(skip)]
    pub memory_run_id: Option<String>,
    /// Host-resolved addressable child name used only by the context-load
    /// manifest. Main runs keep this empty. It is never accepted from IPC.
    #[serde(skip)]
    pub context_load_actor_name: Option<String>,
    pub workspace_path: String,
    pub system_prompt: String,
    pub enabled_tools: Vec<String>,
    pub contexts: Vec<ContextItem>,
    /// Per-run low-priority context assembled by the trusted host (for
    /// example project instructions and model-owned memory). It is projected
    /// as user context before conversation history and is never persisted.
    #[serde(skip)]
    pub ephemeral_contexts: Vec<ContextItem>,
    pub tools: Vec<ToolDescriptor>,
    /// Resolved from the persisted conversation by Rust. Renderer-provided values are replaced.
    #[serde(default)]
    pub active_hooks: Vec<HookDefinition>,
    /// Trusted execution policy injected from the persisted document by Rust.
    /// This is the level the run started with; read `effective_security_level`
    /// for the level in force right now.
    #[serde(default)]
    pub security_level: SecurityLevel,
    /// Shared with every descendant of this run so a mid-turn switch (plan
    /// approval) reaches them. Absent outside a registered run, and never on
    /// the wire — the renderer must not be able to name a level.
    #[serde(skip)]
    pub live_security_level: Option<std::sync::Arc<LiveSecurityLevel>>,
    /// Trusted Tauri application-data root injected by Rust. Renderer input is ignored.
    #[serde(default)]
    pub app_data_path: String,
    /// Trusted shell environment resolved from persisted `run_target`. It is
    /// host-only, so neither the renderer nor tool arguments can select a
    /// machine or inject variables. Child agents inherit it unchanged.
    #[serde(skip)]
    pub run_environment: crate::run_environment::ShellRunner,
    /// The prompt profile this run renders every host-authored text with:
    /// tool-description overrides plus the wording of every fixed injection
    /// point. Resolved by `trusted_run_request` from the conversation's
    /// selection (a built-in or a discovered `.mework/tool-descriptions` file)
    /// and inherited unchanged by child agents. Host-only for the same reason
    /// as the fields above: neither the renderer nor the model may hand a run a
    /// text the user never selected.
    #[serde(skip)]
    pub prompt_profile: std::sync::Arc<crate::prompt_profile::PromptProfile>,
    /// MCP transports dialed for this run. Host-owned; never accepted from renderer IPC.
    /// Filled by `trusted_run_request` from the document's enabled MCP servers that this
    /// conversation actually selected.
    #[serde(skip)]
    pub mcp_servers: Vec<crate::mcp::RuntimeMcpServer>,
    /// Tool bindings discovered from trusted server transports for this run. Child agents inherit
    /// them, while renderer IPC can never provide or replace them.
    #[serde(skip)]
    pub mcp_bindings: Vec<crate::mcp::McpToolBinding>,
    /// Orchestration nesting depth. Never read from IPC input; `run_model` sets it
    /// when spawning a subagent so nested spawns can be refused deterministically.
    #[serde(skip)]
    pub subagent_depth: usize,
    /// The turn this request belongs to, as the host knows it.
    ///
    /// Hydrated in `trusted_run_request` from `run_model`'s own argument, never
    /// from the request body — the renderer already passes `requestId` as a
    /// separate parameter, and letting the body assert one would let a caller
    /// claim a turn it does not own. Empty for child agents, which have no
    /// registered run of their own.
    #[serde(skip)]
    pub request_id: String,
    /// Addressable name of the agent this request runs as, or `None` for the
    /// main session. Host-set alongside `subagent_depth`; never from IPC.
    ///
    /// Its only consumer is the dangerous-tool confirmation dialog: with up to
    /// `MAX_LIVE_AGENTS` children running concurrently, a prompt that does not
    /// say who is asking is not an informed decision.
    #[serde(skip)]
    pub subagent_name: Option<String>,
    /// Parent-timeline call id of the child turn this request runs as, or
    /// `None` for the main session. Host-set by the task workers alongside
    /// `subagent_name`; never from IPC.
    ///
    /// Same single consumer as `subagent_name` — the dangerous-tool
    /// confirmation card — but machine-facing: the renderer's streaming view of
    /// a workflow step is keyed by this call id, not by the pool name the card
    /// displays, so this is what lets the card open the requester's own page.
    #[serde(skip)]
    pub subagent_call_id: Option<String>,
    /// Parent→child mailbox attached only to trusted child requests built by
    /// the run loop; the child drains it before every model round so queued
    /// Queue-only child-agent messages arrive mid-turn. Never crosses IPC.
    #[serde(skip)]
    pub agent_mailbox: crate::agents::AgentMailboxHandle,
    /// User steer inbox attached only by the trusted `run_model` command.
    /// Queue cards remain persisted until the loop emits `UserInputReceived`.
    #[serde(skip)]
    pub steer_mailbox: crate::agents::AgentMailboxHandle,
    /// Cancellation signal for the owning task, attached by workers for child
    /// rounds. It must reach synchronously blocking work. A nonempty task signal
    /// is the sole cancellation source for a task round; a top-level round uses
    /// its own signal. Renderer IPC and model arguments cannot supply either.
    #[serde(skip)]
    pub task_cancel: crate::cancel::CancelSignal,
    /// Cancellation signal for this run, minted by `begin_model_run`. All
    /// cancellation checks use [`Self::round_cancellation`] to select the signal
    /// by ownership rather than looking up another conversation's current run.
    #[serde(skip)]
    pub run_cancel: crate::cancel::CancelSignal,
}

impl RunModelRequest {
    /// Which memory tiers this run may read and write.
    ///
    /// The single source of truth for tier gating: context assembly, tool
    /// granting and tool execution all derive from this rather than from the
    /// two booleans directly, so they cannot drift apart.
    pub fn memory_tier_access(&self) -> crate::mework_memory::MemoryTierAccess {
        crate::mework_memory::MemoryTierAccess {
            global: self.global_memory_enabled,
            project: self.project_memory_enabled,
        }
    }

    /// True when at least one tier is on. For the coarse questions — is there
    /// any memory block at all, is any memory tool reachable — where the tier
    /// does not matter.
    pub fn memory_enabled(&self) -> bool {
        self.global_memory_enabled || self.project_memory_enabled
    }

    /// The security level in force for the next decision.
    ///
    /// Plan approval switches the level in the middle of a turn, so every gate
    /// evaluated during a run reads this rather than `security_level`, which is
    /// only the value the run started with.
    pub fn effective_security_level(&self) -> SecurityLevel {
        match &self.live_security_level {
            Some(cell) => cell.get(),
            None => self.security_level,
        }
    }

    /// Select the cancellation signal by round ownership.
    ///
    /// * Task rounds use only `task_cancel`.
    /// * Top-level rounds use only `run_cancel`.
    /// * Empty signals used by direct IPC or tests never cancel.
    ///
    /// All cancellable work observes this one value to prevent divergent stop
    /// semantics or unsafe current-run lookups.
    pub fn round_cancellation(&self) -> crate::cancel::CancelSignal {
        if !self.task_cancel.is_empty() {
            return self.task_cancel.clone();
        }
        self.run_cancel.clone()
    }
}

impl std::fmt::Debug for RunModelRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mcp_tool_names = self
            .mcp_bindings
            .iter()
            .map(|binding| binding.exposed_name.as_str())
            .collect::<Vec<_>>();
        formatter
            .debug_struct("RunModelRequest")
            .field("provider_id", &self.provider.id)
            .field("model_id", &self.model.id)
            .field("reasoning_effort", &self.reasoning_effort)
            .field("conversation_id", &self.conversation_id)
            .field("workspace_id", &self.workspace_id)
            .field("has_memory_context", &self.memory_context_id.is_some())
            .field(
                "has_project_memory_context",
                &self.project_memory_context_id.is_some(),
            )
            .field(
                "has_agent_definition_binding",
                &self.agent_definition_binding.is_some(),
            )
            .field(
                "inherits_parent_model_memory",
                &self.inherits_parent_model_memory,
            )
            .field("has_fork_model_binding", &self.fork_model_binding.is_some())
            .field(
                "has_subagent_execution_mode_receipt",
                &self.subagent_execution_mode_receipt.is_some(),
            )
            .field(
                "subagent_reserved_name_count",
                &self.subagent_reserved_names.len(),
            )
            .field("has_memory_run_id", &self.memory_run_id.is_some())
            .field(
                "has_context_load_actor_name",
                &self.context_load_actor_name.is_some(),
            )
            .field("system_prompt_bytes", &self.system_prompt.len())
            .field("enabled_tools", &self.enabled_tools)
            .field("context_count", &self.contexts.len())
            .field("ephemeral_context_count", &self.ephemeral_contexts.len())
            .field("tool_count", &self.tools.len())
            .field("active_hook_count", &self.active_hooks.len())
            .field("security_level", &self.security_level)
            .field("effective_security_level", &self.effective_security_level())
            .field("mcp_server_count", &self.mcp_servers.len())
            .field("mcp_tools", &mcp_tool_names)
            .field("subagent_depth", &self.subagent_depth)
            .finish()
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    /// Reasoning tokens the provider billed for this request.
    ///
    /// **A subset of `output_tokens`, never an addend.** Same discipline as
    /// `cached_input_tokens`: it exists so the timeline can say how much
    /// thinking a round actually cost, and adding it to any total would bill
    /// the same tokens twice. OpenAI Responses reports `0` rather than
    /// omitting the field when a round did no reasoning at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RunModelResponse {
    pub contexts: Vec<ContextItem>,
    pub usage: ModelUsage,
    pub model: String,
    pub provider_name: String,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    /// Terminal request failure of the turn (`stop_reason == "error"`). Kept
    /// out of `contexts` on purpose: the renderer shows it as a dismissable
    /// notice instead of persisting error text into the timeline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ModelRunError>,
    /// The value this turn handed back through `structured_output`, already
    /// validated against the run's `output_schema`.
    ///
    /// It has to travel on the response rather than be recovered from the
    /// contexts, because `agent_worker_loop` never sees tool calls — it builds a
    /// child request, recurses into `run_model` and inspects only what comes
    /// back here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_output: Option<Value>,
}

/// Structured description of the API failure that ended a model turn after
/// automatic retries were exhausted (or a permanent failure was detected).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelRunError {
    pub message: String,
    /// Tool round the failing request belonged to (1-based).
    pub round: usize,
    /// Total requests attempted for that round, including the first one.
    pub attempts: u32,
}

/// Normalized incremental output sent over the per-invocation Tauri channel.
/// Provider-specific protocol events, signatures, and encrypted payloads deliberately
/// never cross the IPC boundary. Only provider-designated visible reasoning is emitted.
// `Eq` stops at `ToolContextSettled` because its `ContextItem` card can contain
// a subagent record with fields that implement only `PartialEq`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelStreamEvent {
    #[cfg(debug_assertions)]
    DebugRequestBody {
        round: usize,
        body: Value,
    },
    TextDelta {
        round: usize,
        delta: String,
    },
    /// The provider opened a reasoning item.
    ///
    /// Deliberately *not* implied by the first [`Self::ReasoningDelta`]: a
    /// Responses round that only returns `encrypted_content` never emits a
    /// single delta, and this is then the one signal that the model is
    /// thinking at all. The renderer starts its live clock here.
    ReasoningStart {
        round: usize,
        /// Zero-based ordinal of this reasoning item inside the round. The
        /// sidecar only numbers items that survive its evidence gate, so the
        /// ordinal is dense. The renderer keys one live reasoning row per
        /// ordinal — without it, interleaved reasoning items collapse into a
        /// single card while settlement splits them into several.
        #[serde(default)]
        item: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        form: Option<ReasoningForm>,
    },
    ReasoningDelta {
        round: usize,
        #[serde(default)]
        item: usize,
        delta: String,
    },
    ReasoningDone {
        round: usize,
        #[serde(default)]
        item: usize,
        /// Wall-clock milliseconds this step has spent reasoning so far,
        /// measured by the sidecar. Cumulative across the step's reasoning
        /// items, so repeated `ReasoningDone` events carry the same figure
        /// and applying them is idempotent.
        ///
        /// Explicitly renamed: this enum's `rename_all` only touches variant
        /// names, so `duration_ms` would otherwise reach the renderer as
        /// `duration_ms` while every other field there is camelCase.
        #[serde(rename = "durationMs", default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
    },
    /// Provider-reported cumulative usage for the current API round. A retry
    /// reuses `round`, so renderers replace the previous attempt's snapshot
    /// instead of adding it.
    UsageUpdated {
        round: usize,
        usage: ModelUsage,
    },
    /// A persisted queue item has crossed the backend boundary and joined the
    /// current model turn as a real user context.
    UserInputReceived {
        round: usize,
        id: String,
        content: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<ImageAttachment>,
        #[serde(rename = "createdAt")]
        created_at: String,
    },
    ToolCallAnnounced {
        round: usize,
        #[serde(rename = "callId")]
        call_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        /// The timeline id this call's card will carry once it is persisted.
        ///
        /// The renderer has to give the streaming row an id the moment a call
        /// is announced, long before the host builds the card. Sending the
        /// host's id here is what keeps the two from inventing separate ones
        /// and then disagreeing about which is real at save time — a
        /// disagreement that used to strand a card's attestation permanently.
        #[serde(rename = "contextId")]
        context_id: String,
    },
    ToolCallArgumentsReady {
        round: usize,
        #[serde(rename = "callId")]
        call_id: String,
        input: JsonObject,
    },
    /// A tool call is waiting on the user. The renderer shows an approval card
    /// above the composer and answers with `resolve_tool_prompt`; the worker
    /// thread that raised it is blocked until then. Superseded by
    /// `ToolApprovalResolved` the moment any answer lands, including a denial
    /// the host produced itself (run cancelled, prompt timed out).
    ///
    /// Carries no round or call id: approval is raised from the tool executor,
    /// which is handed the execution request and the descriptor but not the
    /// surrounding turn. A card is identified by its prompt id alone, so
    /// inventing a round here would only be plausible-looking noise.
    ToolApprovalRequested {
        #[serde(rename = "promptId")]
        prompt_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        /// Which card the renderer draws: an ordinary tool approval, or one of
        /// the two plan-mode cards, which offer feedback instead of "always".
        #[serde(default)]
        kind: crate::tool_prompt::PromptKind,
        /// Human-facing tool label, already localized by the catalog.
        label: String,
        /// A short description of what the model is asking to do, derived from
        /// the call's own redacted arguments and truncated for display.
        summary: String,
        #[serde(rename = "riskLevel")]
        risk_level: String,
        reason: String,
        /// The requesting subagent's name, or absent for the main session.
        #[serde(skip_serializing_if = "Option::is_none")]
        requester: Option<String>,
        /// Machine-readable requester address, unlike `requester` which is
        /// display-escaped text: the child's addressable name plus the
        /// parent-timeline call id of its turn. Both absent for the main
        /// session's own calls. The renderer uses them to open the requesting
        /// child's page when the card arrives.
        #[serde(rename = "sourceAgent", skip_serializing_if = "Option::is_none")]
        source_agent: Option<String>,
        #[serde(rename = "sourceCallId", skip_serializing_if = "Option::is_none")]
        source_call_id: Option<String>,
        /// False for shell and MCP tools, and for any decision the classifier
        /// marked mandatory: those must be answered one call at a time.
        #[serde(rename = "allowAlwaysOffered")]
        allow_always_offered: bool,
        /// Whether this card appears whatever the conversation's security level
        /// is. The renderer says so on the card, so full access showing a prompt
        /// reads as the deliberate exception it is rather than a malfunction.
        #[serde(default)]
        mandatory: bool,
    },
    /// The pending approval card for `promptId` is over. `approved` reports
    /// what the host concluded, which is not always what the user clicked —
    /// a cancelled run resolves outstanding cards as denied.
    ToolApprovalResolved {
        #[serde(rename = "promptId")]
        prompt_id: String,
        approved: bool,
    },
    ToolExecutionStarted {
        round: usize,
        #[serde(rename = "callId")]
        call_id: String,
    },
    /// A normalized child model event, attributed to the parent agent tool
    /// call. Keeping the event shape intact lets the frontend feed both parent
    /// and child streams through the same context projection and renderer.
    SubagentEvent {
        round: usize,
        #[serde(rename = "callId")]
        call_id: String,
        event: Box<ModelStreamEvent>,
    },
    /// Parent-facing child lifecycle and explicit progress updates. Raw model
    /// content and tool events use `SubagentEvent` instead.
    SubagentDelta {
        round: usize,
        #[serde(rename = "callId")]
        call_id: String,
        channel: SubagentChannel,
        delta: String,
    },
    /// One transition of a running workflow's progress ledger, attributed to
    /// the `workflow` tool call that owns the run.
    ///
    /// Per-step transcripts keep riding `SubagentEvent`'s double nesting, so
    /// this carries only what the progress card draws. A renderer that ignores
    /// it loses the card, not the transcripts — which is why the card was not
    /// folded into `SubagentChannel` as a sixth channel: a status delta is a
    /// string, and squeezing a mergeable row through it would force the
    /// renderer to parse its own wire format back out.
    WorkflowProgress {
        round: usize,
        #[serde(rename = "callId")]
        call_id: String,
        /// The run this transition belongs to. The card's Skip/Retry commands
        /// address a step by (request_id, run_id, step_index), and the run id is
        /// minted host-side per run — including on resume, where it is reused —
        /// so the renderer can only learn it from the stream.
        #[serde(rename = "runId")]
        run_id: String,
        entry: Box<workflow_core::progress::ProgressRow>,
    },
    ToolExecutionCompleted {
        round: usize,
        #[serde(rename = "callId")]
        call_id: String,
        result: ToolResult,
    },
    /// The settled, host-attested form of an already-emitted tool card —
    /// today that means an agent/workflow card whose terminal child record was
    /// just backfilled at a round boundary, while the parent run keeps going.
    ///
    /// The renderer replaces its persisted copy of the card by id and lets the
    /// ordinary debounced save make the record durable immediately, instead of
    /// waiting for the request to settle: a process that dies mid-run used to
    /// take every finished child transcript with it. The card carries a fresh
    /// attestation token covering the record, so the replacement passes save
    /// validation on its own even after a restart.
    ToolContextSettled {
        round: usize,
        context: Box<ContextItem>,
    },
    HookExecutionStarted {
        round: usize,
        #[serde(rename = "executionId")]
        execution_id: String,
        #[serde(rename = "hookId")]
        hook_id: String,
        #[serde(rename = "hookName")]
        hook_name: String,
        event: String,
        #[serde(rename = "statusMessage", skip_serializing_if = "Option::is_none")]
        status_message: Option<String>,
    },
    HookExecutionCompleted {
        round: usize,
        #[serde(rename = "executionId")]
        execution_id: String,
        #[serde(rename = "hookId")]
        hook_id: String,
        #[serde(rename = "hookName")]
        hook_name: String,
        event: String,
        result: ToolResult,
        blocked: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(rename = "contextInjected")]
        context_injected: bool,
    },
    /// A transient model-request failure is about to be retried. The renderer
    /// discards the failed attempt's partial content for `round` and shows a
    /// temporary retry notice; the error text is never a timeline context.
    StreamRetryScheduled {
        round: usize,
        /// 1-based retry attempt about to run.
        attempt: u32,
        #[serde(rename = "maxAttempts")]
        max_attempts: u32,
        #[serde(rename = "delayMs")]
        delay_ms: u64,
        message: String,
    },
    /// Cancellation probe emitted while no provider data is flowing (retry
    /// backoff waits, keep-alive gaps). Carries no payload; renderers ignore it.
    Ping,
    /// The turn behind this conversation's run has settled host-side and its
    /// outcome is waiting in the run-stream hub. Renderers that own the
    /// original `run_model` invocation ignore it (their invoke promise carries
    /// the same settlement); renderers that adopted the run via
    /// `attach_model_run` respond by calling `take_run_settlement`.
    RunConcluded {
        #[serde(rename = "requestId")]
        request_id: String,
    },
}

/// Which stream of the nested run a `SubagentDelta` fragment belongs to.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SubagentChannel {
    Text,
    Reasoning,
    Activity,
    Update,
    /// Lifecycle transitions of a background agent (every value of
    /// `AgentLiveStatus::wire`). An empty delta is a liveness heartbeat emitted
    /// while `task_wait` blocks; renderers must ignore it.
    Status,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Catalogs cover each variant exactly once and every slug round-trips. The
    /// TypeScript vocabulary guard reads this file as text, so this test is its
    /// Rust-side compiled reader.
    #[test]
    fn endpoint_and_capability_vocabularies_are_self_consistent() {
        let endpoint_slugs: BTreeSet<&str> =
            EndpointType::CATALOG.iter().map(|entry| entry.slug()).collect();
        assert_eq!(
            endpoint_slugs.len(),
            EndpointType::CATALOG.len(),
            "两个端点共用一个 slug 会让跨语言比对沉默地通过"
        );
        for endpoint in EndpointType::CATALOG {
            assert_eq!(EndpointType::from_slug(endpoint.slug()), Some(*endpoint));
        }
        // `openai_chat` is a ProviderFamily slug, not an endpoint slug.
        // Mixing the vocabularies silently invalidates endpoint coverage.
        assert_eq!(EndpointType::from_slug("openai_chat"), None);

        let capability_slugs: BTreeSet<&str> = ModelCapability::CATALOG
            .iter()
            .map(|entry| entry.slug())
            .collect();
        assert_eq!(capability_slugs.len(), ModelCapability::CATALOG.len());
        for capability in ModelCapability::CATALOG {
            assert_eq!(
                ModelCapability::from_slug(capability.slug()),
                Some(*capability)
            );
        }
        assert_eq!(ModelCapability::from_slug("vision"), None);
        // Retired capability slugs must not resolve: a stale archive that still
        // carries them is read as "no capability", not as a live variant.
        for retired in [
            "function_call",
            "reasoning",
            "image_generation",
            "audio_generation",
            "audio_transcript",
            "embedding",
            "rerank",
        ] {
            assert_eq!(ModelCapability::from_slug(retired), None, "{retired}");
        }

        let reasoning_slugs: BTreeSet<&str> = ReasoningContent::CATALOG
            .iter()
            .map(|entry| entry.slug())
            .collect();
        assert_eq!(reasoning_slugs.len(), ReasoningContent::CATALOG.len());
        for form in ReasoningContent::CATALOG {
            assert_eq!(ReasoningContent::from_slug(form.slug()), Some(*form));
        }
        // `auto` was retired in favour of an explicit value written at load time.
        assert_eq!(ReasoningContent::from_slug("auto"), None);

        // Chat endpoints must be a proper subset to keep image and audio models
        // out of the chat selector.
        for chat in EndpointType::CHAT {
            assert!(EndpointType::CATALOG.contains(chat));
        }
        assert!(EndpointType::CHAT.len() < EndpointType::CATALOG.len());

        // Every chat protocol endpoint belongs to CHAT.
        for format in [
            ProviderFamily::OpenaiResponses,
            ProviderFamily::OpenaiChat,
            ProviderFamily::Anthropic,
        ] {
            assert!(EndpointType::CHAT.contains(&format.chat_endpoint()));
        }
    }

    /// `supports_vision` projects the capability set rather than an independent
    /// boolean.
    #[test]
    fn vision_support_projects_from_the_capability_set() {
        let mut model = ModelProfile {
            id: "m".into(),
            name: String::new(),
            group: String::new(),
            context_window: None,
            max_output_tokens: None,
            capabilities: BTreeSet::new(),
            reasoning_content: Default::default(),
            prompt_cache: true,
        };
        assert!(!model.supports_vision());

        model.set_capability(ModelCapability::ImageRecognition, true);
        assert!(model.supports_vision());

        model.set_capability(ModelCapability::ImageRecognition, false);
        assert!(!model.supports_vision());
    }

    /// `prompt_cache` is a concrete, always-written attribute that defaults on:
    /// a profile written before the key existed loads as enabled, an explicit
    /// `false` survives the round trip, and the key is never omitted on save.
    #[test]
    fn prompt_cache_defaults_on_and_round_trips_false() {
        let legacy: ModelProfile = serde_json::from_value(serde_json::json!({
            "id": "m",
            "capabilities": [],
            "reasoningContent": "plaintext"
        }))
        .unwrap();
        assert!(legacy.prompt_cache);
        assert_eq!(serde_json::to_value(&legacy).unwrap()["promptCache"], serde_json::json!(true));

        let disabled: ModelProfile = serde_json::from_value(serde_json::json!({
            "id": "m",
            "capabilities": [],
            "reasoningContent": "plaintext",
            "promptCache": false
        }))
        .unwrap();
        assert!(!disabled.prompt_cache);
        assert_eq!(serde_json::to_value(&disabled).unwrap()["promptCache"], serde_json::json!(false));
    }

    /// Only the Messages protocol consumes the attribute; every other family
    /// stores it without a wire effect.
    #[test]
    fn prompt_cache_takes_effect_only_on_the_messages_protocol() {
        for family in ProviderFamily::CATALOG {
            assert_eq!(
                family.prompt_cache_takes_effect(),
                matches!(family, ProviderFamily::Anthropic),
                "{family:?}"
            );
        }
    }

    /// Every literal a provider or the renderer can put in a tool payload,
    /// paired with what one trip through JavaScript turns it into. The right
    /// column is not hand-written: it was produced by actually running
    /// `JSON.stringify(JSON.parse(x))` in node.
    const JAVASCRIPT_ROUND_TRIP: &[(&str, &str)] = &[
        // Integral floats lose the fractional part.
        ("1.0", "1"),
        ("-1.0", "-1"),
        ("1.0e2", "100"),
        ("123456789.0", "123456789"),
        // Negative zero is not representable in JSON output.
        ("-0.0", "0"),
        // Integers past 2^53 round to the nearest representable f64.
        ("9007199254740993", "9007199254740992"),
        ("-9007199254740993", "-9007199254740992"),
        // Ordinary values are untouched.
        ("0", "0"),
        ("3", "3"),
        ("-5", "-5"),
        ("2.5", "2.5"),
        ("0.1", "0.1"),
        ("-0.5", "-0.5"),
        ("0.30000000000000004", "0.30000000000000004"),
        ("3.0e-7", "3e-7"),
        ("1.5e-10", "1.5e-10"),
        ("9007199254740992", "9007199254740992"),
    ];

    /// Canonicalizing what a payload becomes after the round trip must give
    /// the same answer as canonicalizing what it started as, or a tool card
    /// stops matching its own attestation and the document becomes unsaveable.
    #[test]
    fn the_canonical_number_form_is_what_a_javascript_round_trip_produces() {
        for (input, javascript) in JAVASCRIPT_ROUND_TRIP {
            let mut original = serde_json::from_str::<Value>(&format!("{{\"d\":{input}}}")).unwrap();
            canonicalize_json_numbers(&mut original);
            assert_eq!(
                serde_json::to_string(&original).unwrap(),
                format!("{{\"d\":{javascript}}}"),
                "{input} should canonicalize to the value JavaScript hands back"
            );
        }
    }

    /// The property persistence actually depends on: canonicalizing a payload
    /// that already made the trip changes nothing, so the bytes attested at
    /// execution equal the bytes presented at save.
    #[test]
    fn canonicalizing_a_value_that_already_crossed_the_renderer_is_a_no_op() {
        for (_, javascript) in JAVASCRIPT_ROUND_TRIP {
            let document = format!("{{\"d\":{javascript}}}");
            let mut once = serde_json::from_str::<Value>(&document).unwrap();
            canonicalize_json_numbers(&mut once);
            let mut twice = once.clone();
            canonicalize_json_numbers(&mut twice);
            assert_eq!(once, twice, "{javascript} should be a fixed point");
            assert_eq!(serde_json::to_string(&once).unwrap(), document);
        }
    }

    /// A card is attested when it is executed and re-checked when it is saved,
    /// with a full JSON trip through the renderer in between. Every literal a
    /// provider can write has to survive that trip byte for byte, because the
    /// comparison at the far end is exact.
    #[test]
    fn a_tool_payload_survives_the_round_trip_that_attestation_spans() {
        // Every value here is one the renderer hands back verbatim; the
        // simulation below is what the host does to both ends.
        for (input, _) in JAVASCRIPT_ROUND_TRIP {
            let executed = format!("{{\"argument\":{input}}}");
            let mut attested = serde_json::from_str::<Value>(&executed).unwrap();
            canonicalize_json_numbers(&mut attested);
            let attested_bytes = serde_json::to_string(&attested).unwrap();

            // The renderer parses those bytes and hands them back, and the
            // host re-canonicalizes what arrives before comparing.
            let mut returned = serde_json::from_str::<Value>(&attested_bytes).unwrap();
            canonicalize_json_numbers(&mut returned);

            assert_eq!(
                serde_json::to_string(&returned).unwrap(),
                attested_bytes,
                "{input} must present the same bytes at save that it attested at execution"
            );
        }
    }

    /// Numbers inside strings are data, not values. Tool output is the field
    /// most likely to contain JSON-looking text, and rewriting it would both
    /// corrupt the output and break the card it belongs to.
    #[test]
    fn number_shaped_text_inside_strings_and_keys_is_left_alone() {
        for document in [
            r#"{"output":"cost 1.0 and -0.0 and 9007199254740993"}"#,
            r#"{"1.0":"key stays"}"#,
            r#"{"output":"escaped \" then 1.0"}"#,
        ] {
            let original = serde_json::from_str::<Value>(document).unwrap();
            let mut canonical = original.clone();
            canonicalize_json_numbers(&mut canonical);
            assert_eq!(
                canonical, original,
                "{document} must not be rewritten inside strings or keys"
            );
        }
    }

    /// Nested payloads are the common shape for tool arguments.
    #[test]
    fn canonicalization_reaches_nested_objects_and_arrays() {
        let mut value = serde_json::from_str::<Value>(r#"{"a":[1.0,{"b":[-0.0,2.0]}]}"#).unwrap();
        canonicalize_json_numbers(&mut value);
        assert_eq!(
            serde_json::to_string(&value).unwrap(),
            r#"{"a":[1,{"b":[0,2]}]}"#
        );

        let mut object = serde_json::from_str::<JsonObject>(r#"{"a":[1.0],"b":2.0}"#).unwrap();
        canonicalize_object_numbers(&mut object);
        assert_eq!(serde_json::to_string(&object).unwrap(), r#"{"a":[1],"b":2}"#);
    }

    /// The four literals below freeze the *shape* of the execution-mode
    /// payload: field order, nesting, and which fields are omitted when unset.
    /// A shape change silently re-points an addressable name at a different
    /// execution mode mid-conversation, which is exactly what
    /// `AppState::reserve_subagent_execution_mode` exists to refuse.
    ///
    /// If this test fails, the payload shape moved. Do not re-capture the
    /// literals — revert the shape change and extend via a v2 projection.
    ///
    /// The one exception is a rename of an enum the payload embeds: the
    /// reservation registry is an in-process map rebuilt every launch, and
    /// incompatible persisted documents are rejected, so no stored value can
    /// disagree with the current name.
    #[test]
    fn execution_mode_payload_v1_is_frozen() {
        let fork_without_snapshot_receipt = ForkModelBinding {
            provider_id: "anthropic".into(),
            model_id: "claude-opus-5".into(),
            memory_language: ResolvedLanguage::ZhCn,
            memory_tool_names: vec!["memory_read".into(), "memory_write".into()],
            system_prompt_snapshot: "快照提示词".into(),
            system_prompt_receipt: "spr-1".into(),
            memory_snapshot_receipt: None,
            binding_receipt: "br-1".into(),
            receipt_version: 1,
        };
        let mut fork_with_snapshot_receipt = fork_without_snapshot_receipt.clone();
        fork_with_snapshot_receipt.memory_snapshot_receipt = Some("msr-1".into());

        let definition = AgentDefinitionBinding {
            source: AgentDefinitionSource::Project,
            source_key: "workspace-key".into(),
            name: "reviewer".into(),
            revision: 7,
            memory_epoch: 3,
            provider_id: "anthropic".into(),
            model_id: "claude-sonnet-5".into(),
            memory: AgentDefinitionMemory::Project,
            scope_key: "scope-key".into(),
            configuration_receipt: "cr-1".into(),
            receipt_version: 1,
        };

        let fixtures = [
            (
                "no_bindings",
                canonical_subagent_execution_mode_payload(
                    "conv-1",
                    "helper",
                    SubagentRunKind::General,
                    false,
                    None,
                    None,
                ),
            ),
            (
                "fork_without_snapshot_receipt",
                canonical_subagent_execution_mode_payload(
                    "conv-1",
                    "helper",
                    SubagentRunKind::General,
                    true,
                    Some(&fork_without_snapshot_receipt),
                    None,
                ),
            ),
            (
                "fork_with_snapshot_receipt",
                canonical_subagent_execution_mode_payload(
                    "conv-1",
                    "helper",
                    SubagentRunKind::WorkflowStep,
                    true,
                    Some(&fork_with_snapshot_receipt),
                    None,
                ),
            ),
            (
                "definition_binding",
                canonical_subagent_execution_mode_payload(
                    "conv-1",
                    "reviewer",
                    SubagentRunKind::General,
                    false,
                    None,
                    Some(&definition),
                ),
            ),
        ];

        let expected = [
            r#"["conv-1","helper","general",false,null,null]"#,
            concat!(
                r#"["conv-1","helper","general",true,{"providerId":"anthropic","#,
                r#""modelId":"claude-opus-5","memoryLanguage":"zh-CN","#,
                r#""memoryToolNames":["memory_read","memory_write"],"#,
                r#""systemPromptSnapshot":"快照提示词","systemPromptReceipt":"spr-1","#,
                r#""bindingReceipt":"br-1"},null]"#
            ),
            concat!(
                r#"["conv-1","helper","workflowStep",true,{"providerId":"anthropic","#,
                r#""modelId":"claude-opus-5","memoryLanguage":"zh-CN","#,
                r#""memoryToolNames":["memory_read","memory_write"],"#,
                r#""systemPromptSnapshot":"快照提示词","systemPromptReceipt":"spr-1","#,
                r#""memorySnapshotReceipt":"msr-1","bindingReceipt":"br-1"},null]"#
            ),
            concat!(
                r#"["conv-1","reviewer","general",false,null,{"source":"project","#,
                r#""sourceKey":"workspace-key","name":"reviewer","revision":7,"#,
                r#""memoryEpoch":3,"providerId":"anthropic","modelId":"claude-sonnet-5","#,
                r#""memory":"project","scopeKey":"scope-key","configurationReceipt":"cr-1"}]"#
            ),
        ];

        for ((label, payload), want) in fixtures.iter().zip(expected) {
            assert_eq!(
                payload.as_deref(),
                Ok(want),
                "execution-mode payload drifted for fixture `{label}`"
            );
        }
    }

    /// Adding `receipt_version` to the live binding structs is exactly the
    /// change S1's projections exist to absorb. If this ever fails, a struct
    /// field leaked into the signed payload and every persisted
    /// (conversation, name) pair is about to be permanently burned.
    ///
    /// SCOPE: this is a FIELD-SET assertion. It varies `receipt_version`, which
    /// the projections exclude, so it stays green when a bumped issuance version
    /// moves the payload through the receipt VALUES it does project — see
    /// `the_execution_mode_payload_depends_on_the_binding_receipt_value`. Do not
    /// cite this test as proof that a version bump is safe.
    #[test]
    fn a_new_binding_field_cannot_reach_the_frozen_payload() {
        let fork = ForkModelBinding {
            provider_id: "anthropic".into(),
            model_id: "claude-opus-5".into(),
            memory_language: ResolvedLanguage::ZhCn,
            memory_tool_names: Vec::new(),
            system_prompt_snapshot: String::new(),
            system_prompt_receipt: String::new(),
            memory_snapshot_receipt: None,
            binding_receipt: String::new(),
            receipt_version: 1,
        };
        let mut bumped = fork.clone();
        bumped.receipt_version = 7;

        let payload_of = |binding: &ForkModelBinding| {
            canonical_subagent_execution_mode_payload(
                "c",
                "n",
                SubagentRunKind::General,
                true,
                Some(binding),
                None,
            )
            .expect("payload builds")
        };
        assert_eq!(payload_of(&fork), payload_of(&bumped));
        assert!(!payload_of(&fork).contains("receiptVersion"));
    }

    /// The projections are what make later fields non-breaking, so their key
    /// sets are asserted directly. Adding a field to `ForkModelBinding` or
    /// `AgentDefinitionBinding` must leave these untouched; adding one to a
    /// `*V1` projection trips this test.
    ///
    /// Key ORDER is pinned byte-exactly by `execution_mode_payload_v1_is_frozen`;
    /// it cannot be re-checked here because `serde_json` is built without
    /// `preserve_order`, so parsing back into a `Value` sorts the keys.
    #[test]
    fn execution_mode_payload_projects_only_the_frozen_field_set() {
        let fork = ForkModelBinding {
            provider_id: "p".into(),
            model_id: "m".into(),
            memory_language: ResolvedLanguage::ZhCn,
            memory_tool_names: Vec::new(),
            system_prompt_snapshot: String::new(),
            system_prompt_receipt: String::new(),
            memory_snapshot_receipt: Some("msr".into()),
            binding_receipt: String::new(),
            receipt_version: 1,
        };
        let definition = AgentDefinitionBinding {
            source: AgentDefinitionSource::User,
            source_key: String::new(),
            name: "n".into(),
            revision: 1,
            memory_epoch: 1,
            provider_id: "p".into(),
            model_id: "m".into(),
            memory: AgentDefinitionMemory::None,
            scope_key: String::new(),
            configuration_receipt: String::new(),
            receipt_version: 1,
        };

        let keys = |payload: &str, element: usize| -> Vec<String> {
            let parsed: serde_json::Value =
                serde_json::from_str(payload).expect("payload is valid JSON");
            let mut names: Vec<String> = parsed[element]
                .as_object()
                .expect("binding element is an object")
                .keys()
                .cloned()
                .collect();
            names.sort();
            names
        };
        let sorted = |names: &[&str]| {
            let mut owned: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
            owned.sort();
            owned
        };

        let fork_payload = canonical_subagent_execution_mode_payload(
            "c",
            "n",
            SubagentRunKind::General,
            true,
            Some(&fork),
            None,
        )
        .expect("payload builds");
        assert_eq!(
            keys(&fork_payload, 4),
            sorted(&[
                "providerId",
                "modelId",
                "memoryLanguage",
                "memoryToolNames",
                "systemPromptSnapshot",
                "systemPromptReceipt",
                "memorySnapshotReceipt",
                "bindingReceipt",
            ])
        );

        let definition_payload = canonical_subagent_execution_mode_payload(
            "c",
            "n",
            SubagentRunKind::General,
            false,
            None,
            Some(&definition),
        )
        .expect("payload builds");
        assert_eq!(
            keys(&definition_payload, 5),
            sorted(&[
                "source",
                "sourceKey",
                "name",
                "revision",
                "memoryEpoch",
                "providerId",
                "modelId",
                "memory",
                "scopeKey",
                "configurationReceipt",
            ])
        );
    }

    /// The transitive burn route, asserted instead of described. The frozen
    /// projections carry `bindingReceipt` / `configurationReceipt`, whose VALUES
    /// are HMACs over a version-dispatched payload — so anything that changes
    /// how a binding receipt is computed, an issuance-version bump included,
    /// moves the execution-mode bytes and burns reservations. Two consequences,
    /// both pinned here: the receipts must stay IN the payload (they are what
    /// binds an execution mode to the exact binding it was reserved for, so no
    /// later "these look inert" cleanup may drop them), and no one may claim a
    /// version bump cannot reach these bytes.
    #[test]
    fn the_execution_mode_payload_depends_on_the_binding_receipt_value() {
        let fork = ForkModelBinding {
            provider_id: "anthropic".into(),
            model_id: "claude-opus-5".into(),
            memory_language: ResolvedLanguage::ZhCn,
            memory_tool_names: Vec::new(),
            system_prompt_snapshot: "快照".into(),
            system_prompt_receipt: "spr".into(),
            memory_snapshot_receipt: None,
            binding_receipt: "signed-at-v1".into(),
            receipt_version: 1,
        };
        // Everything a v2 issuance changes about the record reaching this
        // function: the receipt value, and only the receipt value.
        let mut reissued_fork = fork.clone();
        reissued_fork.binding_receipt = "signed-at-v2".into();

        let fork_payload = |binding: &ForkModelBinding| {
            canonical_subagent_execution_mode_payload(
                "conv-1",
                "helper",
                SubagentRunKind::General,
                true,
                Some(binding),
                None,
            )
            .expect("payload builds")
        };
        assert!(fork_payload(&fork).contains("signed-at-v1"));
        assert_ne!(
            fork_payload(&fork),
            fork_payload(&reissued_fork),
            "a re-issued fork binding_receipt must move the execution-mode bytes; \
             if it no longer does, the receipt was dropped from the frozen projection"
        );

        let definition = AgentDefinitionBinding {
            source: AgentDefinitionSource::Project,
            source_key: "workspace-key".into(),
            name: "reviewer".into(),
            revision: 7,
            memory_epoch: 3,
            provider_id: "anthropic".into(),
            model_id: "claude-sonnet-5".into(),
            memory: AgentDefinitionMemory::Project,
            scope_key: "scope-key".into(),
            configuration_receipt: "signed-at-v1".into(),
            receipt_version: 1,
        };
        let mut reissued_definition = definition.clone();
        reissued_definition.configuration_receipt = "signed-at-v2".into();

        let definition_payload = |binding: &AgentDefinitionBinding| {
            canonical_subagent_execution_mode_payload(
                "conv-1",
                "reviewer",
                SubagentRunKind::General,
                false,
                None,
                Some(binding),
            )
            .expect("payload builds")
        };
        assert!(definition_payload(&definition).contains("signed-at-v1"));
        assert_ne!(
            definition_payload(&definition),
            definition_payload(&reissued_definition),
            "a re-issued configuration_receipt must move the execution-mode bytes; \
             if it no longer does, the receipt was dropped from the frozen projection"
        );
    }

    #[test]
    fn the_effective_view_resolves_a_backend_or_refuses_without_falling_back() {
        let mut assets = WebSearchAssets {
            providers: SearchProviderKind::CATALOG
                .iter()
                .copied()
                .map(SearchProviderConfig::new)
                .collect(),
            ..Default::default()
        };

        // Native ignores assets and always resolves; protocol support is checked
        // when the conversation model calls it.
        let native = ConversationWebSearchSettings::default();
        assert_eq!(
            WebSearchSettings::effective(&native, &assets).backend,
            Some(SearchBackend::Native)
        );

        // Disabled explicit choices remain absent; do not silently fall back to
        // another provider or Native.
        let explicit_disabled = ConversationWebSearchSettings {
            provider: SearchProviderSelection::Explicit {
                provider_kind: SearchProviderKind::Tavily,
            },
            ..Default::default()
        };
        assert!(WebSearchSettings::effective(&explicit_disabled, &assets)
            .backend
            .is_none());

        // User overrides take precedence over catalog defaults.
        let tavily = assets
            .providers
            .iter_mut()
            .find(|entry| entry.kind == SearchProviderKind::Tavily)
            .expect("目录里有 tavily");
        tavily.enabled = true;
        tavily.search_api_host = " https://gateway.example/tavily ".into();
        let Some(SearchBackend::Provider(overridden)) =
            WebSearchSettings::effective(&explicit_disabled, &assets).backend
        else {
            panic!("启用后可解析");
        };
        assert_eq!(overridden.kind, SearchProviderKind::Tavily);
        assert_eq!(overridden.api_host, "https://gateway.example/tavily");

        // Unavailable is explicit and does not fall back.
        let unavailable = ConversationWebSearchSettings {
            provider: SearchProviderSelection::Unavailable,
            ..Default::default()
        };
        assert!(WebSearchSettings::effective(&unavailable, &assets)
            .backend
            .is_none());
    }

    #[test]
    fn a_search_only_provider_can_never_resolve_as_the_fetch_backend() {
        let mut assets = WebSearchAssets {
            providers: SearchProviderKind::CATALOG
                .iter()
                .copied()
                .map(SearchProviderConfig::enabled)
                .collect(),
            ..Default::default()
        };

        // Selecting a search-only provider for fetch leaves no backend, so
        // `web_fetch` returns a recoverable error rather than targeting it.
        assets.fetch_provider = Some(SearchProviderKind::Tavily);
        let conversation = ConversationWebSearchSettings::default();
        assert!(WebSearchSettings::effective(&conversation, &assets)
            .fetch
            .is_none());

        // Jina supports both capabilities and fetch must resolve its distinct
        // `r.jina.ai` endpoint rather than search's `s.jina.ai`.
        assets.fetch_provider = Some(SearchProviderKind::Jina);
        let resolved = WebSearchSettings::effective(&conversation, &assets)
            .fetch
            .expect("jina 支持 fetchUrls");
        assert_eq!(resolved.kind, SearchProviderKind::Jina);
        assert_eq!(resolved.api_host, "https://r.jina.ai");

        // A disabled provider is equivalent to no selection.
        let jina = assets
            .providers
            .iter_mut()
            .find(|entry| entry.kind == SearchProviderKind::Jina)
            .expect("目录里有 jina");
        jina.enabled = false;
        assert!(WebSearchSettings::effective(&conversation, &assets)
            .fetch
            .is_none());
    }

    #[test]
    fn the_catalog_mirrors_cherry_studio_row_for_row() {
        // Each kind has exactly one row in catalog order.
        assert_eq!(SEARCH_PROVIDER_CATALOG.len(), 10);
        assert_eq!(
            SearchProviderKind::CATALOG
                .iter()
                .map(|kind| kind.slug())
                .collect::<Vec<_>>(),
            SEARCH_PROVIDER_CATALOG
                .iter()
                .map(|entry| entry.slug)
                .collect::<Vec<_>>()
        );
        // The serde name must exactly match the catalog slug because documents
        // and the credential store use the same key.
        for kind in SearchProviderKind::CATALOG.iter().copied() {
            let wire = serde_json::to_string(&kind).expect("kind 可序列化");
            assert_eq!(wire, format!("\"{}\"", kind.slug()));
            assert_eq!(SearchProviderKind::from_slug(kind.slug()), Some(kind));
        }

        // Every catalog entry must expose at least one capability.
        for entry in SEARCH_PROVIDER_CATALOG {
            assert!(
                entry.search.is_some() || entry.fetch.is_some(),
                "{} 至少要声明一条能力",
                entry.slug
            );
        }

        // Only local `fetch` needs no third-party endpoint.
        let hostless = SEARCH_PROVIDER_CATALOG
            .iter()
            .filter(|entry| {
                [entry.search.as_ref(), entry.fetch.as_ref()]
                    .into_iter()
                    .flatten()
                    .any(|spec| !spec.requires_api_host())
            })
            .map(|entry| entry.slug)
            .collect::<Vec<_>>();
        assert_eq!(hostless, vec!["fetch"]);
    }


    #[test]
    fn legacy_global_preference_defaults_preserve_chinese_day_mode() {
        assert_eq!(AppLanguage::default(), AppLanguage::ZhCn);
        assert_eq!(
            ResolvedLanguage::default(),
            ResolvedLanguage::ZhCn
        );
        assert_eq!(ThemePreference::default(), ThemePreference::Day);
    }

    #[test]
    fn subagent_stream_events_serialize_with_camel_case_call_id() {
        let nested = ModelStreamEvent::SubagentEvent {
            round: 3,
            call_id: "call_9".into(),
            event: Box::new(ModelStreamEvent::ToolCallAnnounced {
                round: 2,
                call_id: "child_call_1".into(),
                tool_name: "read".into(),
                context_id: "ctx_tool_child".into(),
            }),
        };
        assert_eq!(
            serde_json::to_value(nested).unwrap(),
            json!({
                "type": "subagent_event",
                "round": 3,
                "callId": "call_9",
                "event": {
                    "type": "tool_call_announced",
                    "round": 2,
                    "callId": "child_call_1",
                    "toolName": "read",
                    "contextId": "ctx_tool_child"
                }
            })
        );

        let event = ModelStreamEvent::SubagentDelta {
            round: 3,
            call_id: "call_9".into(),
            channel: SubagentChannel::Activity,
            delta: "→ read".into(),
        };
        assert_eq!(
            serde_json::to_value(event).unwrap(),
            json!({
                "type": "subagent_delta",
                "round": 3,
                "callId": "call_9",
                "channel": "activity",
                "delta": "→ read"
            })
        );

        let update = ModelStreamEvent::SubagentDelta {
            round: 3,
            call_id: "call_9".into(),
            channel: SubagentChannel::Update,
            delta: "已完成接口审查".into(),
        };
        assert_eq!(
            serde_json::to_value(update).unwrap(),
            json!({
                "type": "subagent_delta",
                "round": 3,
                "callId": "call_9",
                "channel": "update",
                "delta": "已完成接口审查"
            })
        );
    }

    #[test]
    fn retry_events_and_run_error_use_camel_case_wire_shapes() {
        let usage = ModelStreamEvent::UsageUpdated {
            round: 2,
            usage: ModelUsage {
                input_tokens: Some(120),
                cached_input_tokens: Some(80),
                output_tokens: Some(7),
                total_tokens: Some(127),
                reasoning_tokens: None,
            },
        };
        assert_eq!(
            serde_json::to_value(usage).unwrap(),
            json!({
                "type": "usage_updated",
                "round": 2,
                "usage": {
                    "inputTokens": 120,
                    "cachedInputTokens": 80,
                    "outputTokens": 7,
                    "totalTokens": 127
                }
            })
        );

        let retry = ModelStreamEvent::StreamRetryScheduled {
            round: 2,
            attempt: 1,
            max_attempts: 5,
            delay_ms: 400,
            message: "模型 API 请求失败".into(),
        };
        assert_eq!(
            serde_json::to_value(retry).unwrap(),
            json!({
                "type": "stream_retry_scheduled",
                "round": 2,
                "attempt": 1,
                "maxAttempts": 5,
                "delayMs": 400,
                "message": "模型 API 请求失败"
            })
        );
        assert_eq!(
            serde_json::to_value(ModelStreamEvent::Ping).unwrap(),
            json!({"type": "ping"})
        );
        let error = ModelRunError {
            message: "模型 API 请求失败：HTTP 503".into(),
            round: 1,
            attempts: 6,
        };
        assert_eq!(
            serde_json::to_value(error).unwrap(),
            json!({
                "message": "模型 API 请求失败：HTTP 503",
                "round": 1,
                "attempts": 6
            })
        );
    }

    #[test]
    fn tool_context_serializes_canonical_model_turn_and_subagent_record() {
        let context = ContextItem::Tool {
            id: "ctx_parent_tool".into(),
            tool_name: "subagent".into(),
            round: Some(2),
            model_turn_id: Some("turn_parent_2".into()),
            requested_input: Some(Map::from_iter([("task".into(), json!("原始任务"))])),
            input: Map::from_iter([("task".into(), json!("审查 API"))]),
            result: ToolResult {
                success: true,
                output: "审查完成".into(),
                images: Vec::new(),
                diff: None,
                executed_at: "2026-07-14T00:00:03Z".into(),
                duration_ms: 20,
            },
            subagent: Some(SubagentRunRecord {
                kind: SubagentRunKind::General,
                name: Some("a1".into()),
                label: None,
                inherits_model_memory: false,
                fork_model_binding: None,
                agent_definition: None,
                execution_mode_receipt: String::new(),
                task: "审查 API".into(),
                status: SubagentRunStatus::Completed,
                contexts: vec![ContextItem::Assistant {
                    id: "ctx_child_answer".into(),
                    content: "审查完成".into(),
                    round: None,
                    model_turn_id: None,
                    interrupted: false,
                    sources: Vec::new(),
                    created_at: "2026-07-14T00:00:02Z".into(),
                }],
                updates: vec![SubagentUpdate {
                    content: "已完成接口审查".into(),
                    created_at: "2026-07-14T00:00:01Z".into(),
                }],
                queued_messages: Vec::new(),
                structured_output: None,
                output_schema: None,
                usage: ModelUsage::default(),
            }),
            attestation: String::new(),
            created_at: "2026-07-14T00:00:03Z".into(),
        };
        let value = serde_json::to_value(&context).unwrap();
        assert_eq!(value["kind"], "tool");
        assert_eq!(value["modelTurnId"], "turn_parent_2");
        assert_eq!(value["requestedInput"]["task"], "原始任务");
        assert!(value.get("callId").is_none());
        assert_eq!(value["subagent"]["task"], "审查 API");
        assert_eq!(value["subagent"]["status"], "completed");
        assert_eq!(value["subagent"]["contexts"][0]["kind"], "assistant");
        // A schema-less record must not serialize the key: records that predate
        // it lack the key, and rehydration treats absence as "not schema-bound".
        assert!(value["subagent"].get("outputSchema").is_none());
        assert_eq!(
            value["subagent"]["updates"][0],
            json!({
                "content": "已完成接口审查",
                "createdAt": "2026-07-14T00:00:01Z"
            })
        );

        let legacy: ContextItem = serde_json::from_value(json!({
            "kind": "tool",
            "id": "ctx_legacy",
            "toolName": "read",
            "callId": "provider-call-id",
            "input": {"path": "README.md"},
            "result": {
                "success": true,
                "output": "ok",
                "executedAt": "2026-07-14T00:00:00Z",
                "durationMs": 1
            },
            "createdAt": "2026-07-14T00:00:00Z"
        }))
        .unwrap();
        assert!(matches!(
            &legacy,
            ContextItem::Tool {
                model_turn_id: None,
                requested_input: None,
                subagent: None,
                ..
            }
        ));
        assert!(serde_json::to_value(legacy)
            .unwrap()
            .get("callId")
            .is_none());

        let assistant: ContextItem = serde_json::from_value(json!({
            "kind": "assistant",
            "id": "ctx_legacy_assistant",
            "content": "answer",
            "openAiChat": {"providerId":"p","modelId":"m","message":{"role":"assistant"}},
            "createdAt": "2026-07-14T00:00:00Z"
        }))
        .unwrap();
        let reasoning: ContextItem = serde_json::from_value(json!({
            "kind": "reasoning",
            "id": "ctx_legacy_reasoning",
            "encrypted": true,
            "encryptedPayload": "opaque-provider-payload",
            "createdAt": "2026-07-14T00:00:00Z"
        }))
        .unwrap();
        let canonical = serde_json::to_value([assistant, reasoning]).unwrap();
        assert!(!canonical.to_string().contains("openAiChat"));
        assert!(!canonical.to_string().contains("encryptedPayload"));
    }

    /// The reasoning form is an independent optional key that never inherits the
    /// legacy `encrypted` flag. Explicit forms serialize as the TypeScript
    /// `ReasoningForm` literals; absence preserves legacy-card identification.
    #[test]
    fn reasoning_form_round_trips_and_never_inherits_the_legacy_encrypted_flag() {
        let card = |form: Option<ReasoningForm>| ContextItem::Reasoning {
            id: "ctx_reasoning".into(),
            content: None,
            form,
            round: Some(1),
            model_turn_id: None,
            interrupted: false,
            duration_ms: Some(1_200),
            tokens: Some(64),
            replay: None,
            created_at: "2026-08-30T00:00:00Z".into(),
        };

        assert_eq!(
            serde_json::to_value(card(Some(ReasoningForm::Encrypted))).unwrap()["form"],
            json!("encrypted")
        );
        assert_eq!(
            serde_json::to_value(card(Some(ReasoningForm::Plaintext))).unwrap()["form"],
            json!("plaintext")
        );
        assert!(serde_json::to_value(card(None))
            .unwrap()
            .get("form")
            .is_none());

        let round_tripped: ContextItem =
            serde_json::from_value(serde_json::to_value(card(Some(ReasoningForm::Plaintext))).unwrap())
                .unwrap();
        assert!(matches!(
            round_tripped,
            ContextItem::Reasoning {
                form: Some(ReasoningForm::Plaintext),
                ..
            }
        ));

        let legacy: ContextItem = serde_json::from_value(json!({
            "kind": "reasoning",
            "id": "ctx_legacy_reasoning",
            "encrypted": true,
            "createdAt": "2026-07-14T00:00:00Z"
        }))
        .unwrap();
        assert!(matches!(legacy, ContextItem::Reasoning { form: None, .. }));
    }

    /// The replay payload is opaque provider data: it round-trips byte for byte,
    /// is absent from the wire when there is nothing to replay, and a record
    /// written before the field existed still loads without it.
    #[test]
    fn reasoning_replay_round_trips_and_is_absent_when_none() {
        let card = |replay: Option<ReasoningReplay>| ContextItem::Reasoning {
            id: "ctx_reasoning".into(),
            content: Some("thought".into()),
            form: Some(ReasoningForm::Plaintext),
            round: Some(1),
            model_turn_id: None,
            interrupted: false,
            duration_ms: None,
            tokens: None,
            replay,
            created_at: "2026-09-03T00:00:00Z".into(),
        };
        let replay = ReasoningReplay {
            model: "claude-opus-5".into(),
            parts: vec![json!({
                "text": "thought",
                "providerOptions": { "anthropic": { "signature": "sig-bytes" } }
            })],
        };
        let encoded = serde_json::to_value(card(Some(replay.clone()))).unwrap();
        assert_eq!(encoded["replay"]["model"], json!("claude-opus-5"));
        assert_eq!(
            encoded["replay"]["parts"][0]["providerOptions"]["anthropic"]["signature"],
            json!("sig-bytes")
        );
        let round_tripped: ContextItem = serde_json::from_value(encoded).unwrap();
        assert!(matches!(
            round_tripped,
            ContextItem::Reasoning { replay: Some(ref stored), .. } if *stored == replay
        ));

        assert!(serde_json::to_value(card(None))
            .unwrap()
            .get("replay")
            .is_none());
        let legacy: ContextItem = serde_json::from_value(json!({
            "kind": "reasoning",
            "id": "ctx_legacy_reasoning",
            "content": "old thought",
            "createdAt": "2026-07-14T00:00:00Z"
        }))
        .unwrap();
        assert!(matches!(legacy, ContextItem::Reasoning { replay: None, .. }));
    }

    #[test]
    fn minimal_reasoning_effort_migrates_to_low() {
        let effort: ReasoningEffort = serde_json::from_value(json!("minimal")).unwrap();
        assert_eq!(effort, ReasoningEffort::Low);
        assert_eq!(serde_json::to_value(effort).unwrap(), json!("low"));
    }

    /// A record's `outputSchema` document round-trips under its camelCase wire
    /// name, and a record without the key still loads — it rehydrates schema-less.
    #[test]
    fn subagent_record_output_schema_round_trips_and_legacy_records_load() {
        let schema_document = json!({
            "type": "object",
            "properties": {"verdict": {"type": "string"}},
            "required": ["verdict"]
        });
        let record = SubagentRunRecord {
            kind: SubagentRunKind::General,
            name: Some("a1".into()),
            label: None,
            inherits_model_memory: false,
            fork_model_binding: None,
            agent_definition: None,
            execution_mode_receipt: String::new(),
            task: "审查 API".into(),
            status: SubagentRunStatus::Completed,
            contexts: Vec::new(),
            updates: Vec::new(),
            queued_messages: Vec::new(),
            structured_output: None,
            output_schema: Some(schema_document.clone()),
            usage: ModelUsage::default(),
        };
        let value = serde_json::to_value(&record).unwrap();
        assert_eq!(value["outputSchema"], schema_document);
        let restored: SubagentRunRecord = serde_json::from_value(value).unwrap();
        assert_eq!(restored.output_schema, Some(schema_document));

        let legacy: SubagentRunRecord = serde_json::from_value(json!({
            "task": "历史任务",
            "status": "completed",
            "contexts": [],
            "updates": []
        }))
        .unwrap();
        assert_eq!(legacy.output_schema, None);
    }

    #[test]
    fn tool_stream_events_use_stable_camel_case_payload_fields() {
        let event = ModelStreamEvent::ToolCallArgumentsReady {
            round: 2,
            call_id: "call_7".into(),
            input: Map::from_iter([("path".into(), json!("README.md"))]),
        };
        assert_eq!(
            serde_json::to_value(event).unwrap(),
            json!({
                "type": "tool_call_arguments_ready",
                "round": 2,
                "callId": "call_7",
                "input": {"path": "README.md"}
            })
        );

        let completed = ModelStreamEvent::ToolExecutionCompleted {
            round: 2,
            call_id: "call_7".into(),
            result: ToolResult {
                success: true,
                output: "ok".into(),
                images: Vec::new(),
                diff: None,
                executed_at: "2026-07-12T00:00:00Z".into(),
                duration_ms: 4,
            },
        };
        assert_eq!(
            serde_json::to_value(completed).unwrap()["callId"],
            json!("call_7")
        );
    }

    #[test]
    fn tool_result_diff_is_optional_and_omitted_for_legacy_results() {
        let legacy: ToolResult = serde_json::from_value(json!({
            "success": true,
            "output": "ok",
            "executedAt": "2026-07-12T00:00:00Z",
            "durationMs": 4
        }))
        .unwrap();
        assert_eq!(legacy.diff, None);
        assert!(serde_json::to_value(&legacy).unwrap().get("diff").is_none());

        let with_diff = ToolResult {
            diff: Some("--- old\n+++ new\n".into()),
            ..legacy
        };
        assert_eq!(
            serde_json::to_value(with_diff).unwrap()["diff"],
            json!("--- old\n+++ new\n")
        );
    }

    /// The wire names are read by the renderer, by stored documents and by the
    /// hook `permission_mode`, so a rename is a data migration rather than a
    /// refactor. `ALL` is asserted to be complete here because nothing else can.
    #[test]
    fn security_level_wire_names_are_stable() {
        let names: Vec<Value> = SecurityLevel::ALL
            .iter()
            .map(|level| serde_json::to_value(level).unwrap())
            .collect();
        assert_eq!(
            names,
            vec![
                json!("plan"),
                json!("request_approval"),
                json!("allow_edits"),
                json!("full_access"),
            ]
        );
        for level in SecurityLevel::ALL {
            let name = serde_json::to_value(level).unwrap();
            assert_eq!(
                serde_json::from_value::<SecurityLevel>(name).unwrap(),
                level
            );
        }
        // A document written before plan mode carries no level at all, and must
        // keep landing on the prompting level rather than the new one.
        assert_eq!(SecurityLevel::default(), SecurityLevel::RequestApproval);
    }

    /// Plan approval switches the level in the middle of a turn, so the cell has
    /// to survive a round trip through the byte it stores for every level, not
    /// just the two that plan mode moves between.
    #[test]
    fn the_live_cell_round_trips_every_level() {
        let cell = LiveSecurityLevel::new(SecurityLevel::Plan);
        assert_eq!(cell.get(), SecurityLevel::Plan);
        for level in SecurityLevel::ALL {
            cell.set(level);
            assert_eq!(cell.get(), level);
        }
    }
}
