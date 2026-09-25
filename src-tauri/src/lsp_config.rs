//! `lsp.json` — language servers as a file, in the `lspServers` shape Claude
//! Code's plugins use.
//!
//! Mework reads `~/.mework/lsp.json` for every workspace and
//! `<workspace>/.mework/lsp.json` for one, exactly like [`crate::mcp_config`].
//! Every entry becomes a catalog row; an entry Mework cannot launch stays
//! listed as unavailable with the reason in its description, so a file copied
//! from another client explains itself instead of silently shrinking.
//!
//! Claude Code sources language servers *only* from plugins, and ships no
//! server definitions of its own — its marketplace supplies them. Mework has no
//! plugin system and no marketplace, so a literal port would leave the `lsp`
//! tool inert until the user hand-wrote a config. The built-in table below is
//! the deliberate deviation: each preset is offered only when its command
//! resolves on `PATH`, and any `lsp.json` entry claiming the same name or the
//! same file extension wins over it.

use std::{collections::BTreeMap, path::Path};

use serde_json::Value;

use crate::{
    capabilities::stable_id,
    model::{ResolvedLanguage, ResourceDescriptor, ResourceSource},
};

/// Longest description shown in the catalog.
const DESCRIPTION_LIMIT: usize = 240;

/// Startup budget when the entry does not set one. A cold `rust-analyzer` on a
/// large workspace answers `initialize` well inside this; anything slower is
/// indistinguishable from a server that will never answer.
pub const DEFAULT_STARTUP_TIMEOUT_MILLIS: u64 = 30_000;
/// Graceful-shutdown budget when the entry does not set one.
pub const DEFAULT_SHUTDOWN_TIMEOUT_MILLIS: u64 = 5_000;
/// Crash restarts before a server is given up on. Claude Code's default.
pub const DEFAULT_MAX_RESTARTS: u32 = 3;

/// One launchable language server.
#[derive(Clone, Debug, PartialEq)]
pub struct LspServerConfig {
    pub id: String,
    pub name: String,
    pub description: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// Lowercased file extension (with the leading dot) to LSP language id.
    /// Both the set of extensions this server handles and the `languageId` sent
    /// in `textDocument/didOpen` are derived from this one map, as in Claude
    /// Code: there is no second extension table.
    pub extension_to_language: BTreeMap<String, String>,
    /// Passed through as `initialize.initializationOptions`.
    pub initialization_options: Option<Value>,
    /// Answered to `workspace/configuration` and pushed as
    /// `workspace/didChangeConfiguration`; `None` means the client advertises
    /// no `workspace.configuration` capability at all.
    pub settings: Option<Value>,
    /// Root the server is started in and advertised as its workspace folder.
    /// Empty means "the workspace the conversation runs in".
    pub workspace_folder: String,
    pub startup_timeout_millis: u64,
    pub shutdown_timeout_millis: u64,
    pub restart_on_crash: bool,
    pub max_restarts: u32,
    /// Whether `textDocument/publishDiagnostics` from this server is pushed
    /// into the conversation after an edit. Navigation is unaffected.
    pub diagnostics: bool,
}

/// One `lspServers` entry: the catalog row, and the launch configuration when
/// the row is available.
#[derive(Clone, Debug)]
pub struct LspEntry {
    pub descriptor: ResourceDescriptor,
    /// `None` exactly when `descriptor.available` is false.
    pub config: Option<LspServerConfig>,
}

fn id_prefix(source: ResourceSource) -> &'static str {
    match source {
        ResourceSource::User => "lsp_user",
        ResourceSource::Workspace => "lsp_workspace",
        ResourceSource::Builtin => "lsp_builtin",
    }
}

/// The address of one entry: the file, then the JSON pointer of the key.
pub fn location_for(path: &Path, name: &str) -> String {
    format!("{}#/lspServers/{name}", path.to_string_lossy())
}

/// Server names Claude Code accepts, so a file round-trips between the two
/// products. Same rule as [`crate::mcp_config::server_name_is_valid`].
pub fn server_name_is_valid(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        && !matches!(name, "__proto__" | "constructor" | "prototype")
}

// ---------------------------------------------------------------------------
// Built-in presets
// ---------------------------------------------------------------------------

/// A language server Mework offers when its command is already installed.
///
/// This table exists because Mework has no plugin marketplace to supply one.
/// It is deliberately small: every entry is a server whose stdio invocation is
/// stable, needs no per-project configuration to answer navigation requests,
/// and is the ordinary choice for its language.
pub struct LspServerPreset {
    pub name: &'static str,
    pub command: &'static str,
    pub args: &'static [&'static str],
    /// `(extension, languageId)`, extensions lowercased and dot-prefixed.
    pub extensions: &'static [(&'static str, &'static str)],
    pub description_zh: &'static str,
    pub description_en: &'static str,
}

pub const LSP_SERVER_PRESETS: &[LspServerPreset] = &[
    LspServerPreset {
        name: "rust-analyzer",
        command: "rust-analyzer",
        args: &[],
        extensions: &[(".rs", "rust")],
        description_zh: "Rust 官方语言服务器。随 rustup component add rust-analyzer 安装。",
        description_en: "The official Rust language server. Install with `rustup component add rust-analyzer`.",
    },
    LspServerPreset {
        name: "typescript-language-server",
        command: "typescript-language-server",
        args: &["--stdio"],
        extensions: &[
            (".ts", "typescript"),
            (".mts", "typescript"),
            (".cts", "typescript"),
            (".tsx", "typescriptreact"),
            (".js", "javascript"),
            (".mjs", "javascript"),
            (".cjs", "javascript"),
            (".jsx", "javascriptreact"),
        ],
        description_zh: "TypeScript 与 JavaScript 语言服务器。npm i -g typescript-language-server typescript。",
        description_en: "TypeScript and JavaScript language server. `npm i -g typescript-language-server typescript`.",
    },
    LspServerPreset {
        name: "pyright",
        command: "pyright-langserver",
        args: &["--stdio"],
        extensions: &[(".py", "python"), (".pyi", "python")],
        description_zh: "Python 类型检查与导航。npm i -g pyright。",
        description_en: "Python type checking and navigation. `npm i -g pyright`.",
    },
    LspServerPreset {
        name: "gopls",
        command: "gopls",
        args: &[],
        extensions: &[(".go", "go")],
        description_zh: "Go 官方语言服务器。go install golang.org/x/tools/gopls@latest。",
        description_en: "The official Go language server. `go install golang.org/x/tools/gopls@latest`.",
    },
    LspServerPreset {
        name: "clangd",
        command: "clangd",
        args: &[],
        extensions: &[
            (".c", "c"),
            (".h", "c"),
            (".cpp", "cpp"),
            (".cc", "cpp"),
            (".cxx", "cpp"),
            (".hpp", "cpp"),
            (".hxx", "cpp"),
            (".hh", "cpp"),
        ],
        description_zh: "C/C++ 语言服务器，随 LLVM 发行。需要 compile_commands.json 才能解析包含路径。",
        description_en: "C/C++ language server, shipped with LLVM. Needs a compile_commands.json to resolve includes.",
    },
    LspServerPreset {
        name: "lua-language-server",
        command: "lua-language-server",
        args: &[],
        extensions: &[(".lua", "lua")],
        description_zh: "Lua 语言服务器。",
        description_en: "The Lua language server.",
    },
    LspServerPreset {
        name: "bash-language-server",
        command: "bash-language-server",
        args: &["start"],
        extensions: &[(".sh", "shellscript"), (".bash", "shellscript")],
        description_zh: "Shell 脚本语言服务器。npm i -g bash-language-server。",
        description_en: "Shell script language server. `npm i -g bash-language-server`.",
    },
];

/// Built-in presets as catalog entries.
///
/// A preset whose command is not on `PATH` is still listed — as unavailable,
/// with the install line as its reason. A row a user can read and act on beats
/// a row that silently is not there; this is the same rule `mcp.json` follows
/// for an entry it cannot dial.
pub fn builtin_entries(language: ResolvedLanguage) -> Vec<LspEntry> {
    builtin_entries_with_resolver(language, &|command| {
        crate::environment_tools::resolve_on_path(command).is_some()
    })
}

pub(crate) fn builtin_entries_with_resolver(
    language: ResolvedLanguage,
    installed: &dyn Fn(&str) -> bool,
) -> Vec<LspEntry> {
    LSP_SERVER_PRESETS
        .iter()
        .map(|preset| {
            let location = format!("builtin:lsp/{}", preset.name);
            let id = stable_id(
                id_prefix(ResourceSource::Builtin),
                preset.name,
                &location,
            );
            let blurb = match language {
                ResolvedLanguage::EnUs => preset.description_en,
                ResolvedLanguage::ZhCn => preset.description_zh,
            };
            let available = installed(preset.command);
            let description = if available {
                blurb.to_owned()
            } else {
                match language {
                    ResolvedLanguage::EnUs => format!(
                        "`{}` was not found on PATH, so this server is not offered. {blurb}",
                        preset.command
                    ),
                    ResolvedLanguage::ZhCn => format!(
                        "PATH 上找不到 `{}`，因此不提供这台服务器。{blurb}",
                        preset.command
                    ),
                }
            };
            LspEntry {
                descriptor: ResourceDescriptor {
                    id: id.clone(),
                    name: preset.name.to_owned(),
                    description,
                    location,
                    source: ResourceSource::Builtin,
                    available,
                    workspace_id: None,
                },
                config: available.then(|| LspServerConfig {
                    id,
                    name: preset.name.to_owned(),
                    description: blurb.to_owned(),
                    command: preset.command.to_owned(),
                    args: preset.args.iter().map(|value| (*value).to_owned()).collect(),
                    env: BTreeMap::new(),
                    extension_to_language: preset
                        .extensions
                        .iter()
                        .map(|(extension, language)| {
                            ((*extension).to_owned(), (*language).to_owned())
                        })
                        .collect(),
                    initialization_options: None,
                    settings: None,
                    workspace_folder: String::new(),
                    startup_timeout_millis: DEFAULT_STARTUP_TIMEOUT_MILLIS,
                    shutdown_timeout_millis: DEFAULT_SHUTDOWN_TIMEOUT_MILLIS,
                    restart_on_crash: true,
                    max_restarts: DEFAULT_MAX_RESTARTS,
                    diagnostics: true,
                }),
            }
        })
        .collect()
}

/// Every language server one workspace may use, in the order they claim file
/// extensions: the workspace's own `lsp.json` first, then the user's, then the
/// built-in presets that are installed.
///
/// A project's file beats the user's global one because it is the more specific
/// statement about this code; both beat the presets, which exist only so the
/// tool is not inert before anyone has written a config. Only *available*
/// entries are returned — an unavailable row explains itself in the catalog,
/// but there is nothing to launch.
pub fn servers_for_workspace(
    workspace: &Path,
    language: ResolvedLanguage,
) -> Vec<LspServerConfig> {
    let mut levels: Vec<Vec<LspEntry>> = Vec::with_capacity(3);
    if !workspace.as_os_str().is_empty() {
        levels.push(read_file(
            &crate::capabilities::config_path_for(workspace, crate::capabilities::CapabilityKind::Lsp),
            ResourceSource::Workspace,
            None,
        ));
    }
    if let Some(home) = dirs::home_dir() {
        levels.push(read_file(
            &crate::capabilities::config_path_for(&home, crate::capabilities::CapabilityKind::Lsp),
            ResourceSource::User,
            None,
        ));
    }
    levels.push(builtin_entries(language));
    merge_servers(levels)
}

/// Folds the levels of configuration — most specific first — into the list a
/// call routes on. Only *available* entries survive, and a name already taken
/// by a more specific level is an override, not a second server: `lsp.json`
/// naming `rust-analyzer` replaces the preset rather than racing it for `.rs`.
pub fn merge_servers(levels: impl IntoIterator<Item = Vec<LspEntry>>) -> Vec<LspServerConfig> {
    let mut configs: Vec<LspServerConfig> = Vec::new();
    let mut claimed_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    for entries in levels {
        for entry in entries {
            let Some(config) = entry.config else { continue };
            if claimed_names.insert(config.name.clone()) {
                configs.push(config);
            }
        }
    }
    configs
}

/// The commands the built-in presets launch, for a caller that has to ask
/// another machine which of them are installed.
pub fn preset_commands() -> impl Iterator<Item = &'static str> {
    LSP_SERVER_PRESETS.iter().map(|preset| preset.command)
}

// ---------------------------------------------------------------------------
// lsp.json
// ---------------------------------------------------------------------------

/// Every entry of one `lsp.json`. A missing or unreadable file yields nothing;
/// a file whose top level is not `{ "lspServers": { … } }` yields nothing and
/// says why on stderr, because there is no row to hang the reason on.
pub fn read_file(path: &Path, source: ResourceSource, workspace_id: Option<&str>) -> Vec<LspEntry> {
    read_file_with_env(path, source, workspace_id, &|name| {
        std::env::var(name).ok()
    })
}

pub fn read_file_with_env(
    path: &Path,
    source: ResourceSource,
    workspace_id: Option<&str>,
    env: &dyn Fn(&str) -> Option<String>,
) -> Vec<LspEntry> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    parse_contents(&bytes, path, source, workspace_id, env)
}

/// Every entry of one `lsp.json` whose bytes the caller already holds — a file
/// read off another machine, where `path` is its spelling there and is used
/// only for the entries' locations and the messages.
pub fn parse_contents(
    bytes: &[u8],
    path: &Path,
    source: ResourceSource,
    workspace_id: Option<&str>,
    env: &dyn Fn(&str) -> Option<String>,
) -> Vec<LspEntry> {
    let display = path.display();
    let value: Value = match serde_json::from_slice(bytes) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("{display} is not valid JSON and was skipped: {error}");
            return Vec::new();
        }
    };
    let Some(root) = value.as_object() else {
        eprintln!("{display} must be a JSON object with an \"lspServers\" key; skipped");
        return Vec::new();
    };
    let servers = match root.get("lspServers") {
        Some(Value::Object(servers)) => servers,
        Some(_) => {
            eprintln!("{display}: \"lspServers\" must be an object keyed by server name; skipped");
            return Vec::new();
        }
        None if root.contains_key("languageServers") => {
            eprintln!(
                "{display}: missing \"lspServers\" — found \"languageServers\" instead. Mework reads language servers from the \"lspServers\" key; rename it."
            );
            return Vec::new();
        }
        None => return Vec::new(),
    };
    servers
        .iter()
        .map(|(name, entry)| {
            let location = location_for(path, name);
            let id = stable_id(id_prefix(source), name, &location);
            let workspace_id = workspace_id.map(str::to_owned);
            match parse_entry(&id, name, entry, env) {
                Ok(config) => LspEntry {
                    descriptor: ResourceDescriptor {
                        id,
                        name: name.clone(),
                        description: if config.description.is_empty() {
                            describe_extensions(&config)
                        } else {
                            config.description.clone()
                        },
                        location,
                        source,
                        available: true,
                        workspace_id,
                    },
                    config: Some(config),
                },
                Err(reason) => LspEntry {
                    descriptor: ResourceDescriptor {
                        id,
                        name: name.clone(),
                        description: reason,
                        location,
                        source,
                        available: false,
                        workspace_id,
                    },
                    config: None,
                },
            }
        })
        .collect()
}

/// The fallback row text for an entry that named no description: the file kinds
/// it answers for, which is the one thing every entry has.
fn describe_extensions(config: &LspServerConfig) -> String {
    let mut extensions: Vec<&str> = config
        .extension_to_language
        .keys()
        .map(String::as_str)
        .collect();
    extensions.sort_unstable();
    format!("{} — {}", config.command, extensions.join(" "))
}

/// Builds the launch configuration for one entry, or the reason it cannot be
/// launched. The accepted shape is Claude Code's per-server LSP schema.
fn parse_entry(
    id: &str,
    name: &str,
    entry: &Value,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<LspServerConfig, String> {
    if !server_name_is_valid(name) {
        return Err(
            "Server names may only contain letters, numbers, hyphens and underscores (and may not be __proto__, constructor or prototype)".into(),
        );
    }
    let Some(entry) = entry.as_object() else {
        return Err("The entry must be an object".into());
    };

    // Claude Code's schema accepts `socket` but its LSP path only ever spawns
    // a stdio child, so the value does nothing there. Refusing it here — rather
    // than accepting and ignoring it — is what lets a copied file explain
    // itself instead of appearing to work over a transport that is not wired.
    match entry.get("transport") {
        None | Some(Value::Null) => {}
        Some(Value::String(kind)) if kind == "stdio" => {}
        Some(Value::String(kind)) if kind == "socket" => {
            return Err(
                "The socket transport is not supported; language servers are launched over stdio"
                    .into(),
            )
        }
        Some(Value::String(other)) => {
            return Err(format!(
                "Unknown transport \"{other}\"; the only supported transport is stdio"
            ))
        }
        Some(_) => return Err("\"transport\" must be a string".into()),
    }

    let mut missing = Vec::new();
    let mut expand = |text: &str| {
        crate::mcp_config::expand_env_references(text, env, &mut missing)
    };

    let command = expand(&string_field(entry, "command")?);
    let args = string_list_field(entry, "args")?
        .iter()
        .map(|argument| expand(argument))
        .collect::<Vec<_>>();
    let env_values = string_map_field(entry, "env")?
        .into_iter()
        .map(|(key, value)| (key, expand(&value)))
        .collect::<BTreeMap<_, _>>();
    let workspace_folder = expand(&string_field(entry, "workspaceFolder")?);
    drop(expand);
    if !missing.is_empty() {
        missing.sort();
        missing.dedup();
        return Err(format!(
            "Missing environment variables: {}",
            missing.join(", ")
        ));
    }

    if command.trim().is_empty() {
        return Err("The entry needs a non-empty \"command\"".into());
    }
    // Claude Code refuses a bare command containing spaces so an argument
    // string is never mistaken for a program name. An absolute path may hold
    // spaces, which is why the check exempts it.
    if command.contains(' ') && !Path::new(&command).is_absolute() {
        return Err(
            "\"command\" should not contain spaces. Use the \"args\" array for arguments.".into(),
        );
    }

    let extension_to_language = extension_map(entry)?;
    if extension_to_language.is_empty() {
        return Err(
            "\"extensionToLanguage\" must map at least one file extension to an LSP language id, for example { \".rs\": \"rust\" }"
                .into(),
        );
    }

    Ok(LspServerConfig {
        id: id.to_owned(),
        name: name.to_owned(),
        description: truncate_chars(string_field(entry, "description")?.trim(), DESCRIPTION_LIMIT),
        command,
        args,
        env: env_values,
        extension_to_language,
        initialization_options: value_field(entry, "initializationOptions"),
        settings: value_field(entry, "settings"),
        workspace_folder,
        startup_timeout_millis: millis_field(
            entry,
            "startupTimeout",
            DEFAULT_STARTUP_TIMEOUT_MILLIS,
        )?,
        shutdown_timeout_millis: millis_field(
            entry,
            "shutdownTimeout",
            DEFAULT_SHUTDOWN_TIMEOUT_MILLIS,
        )?,
        restart_on_crash: optional_bool_field(entry, "restartOnCrash")?.unwrap_or(true),
        max_restarts: match entry.get("maxRestarts") {
            None | Some(Value::Null) => DEFAULT_MAX_RESTARTS,
            Some(value) => u32::try_from(
                value
                    .as_u64()
                    .ok_or_else(|| "\"maxRestarts\" must be a whole number".to_owned())?,
            )
            .unwrap_or(u32::MAX),
        },
        diagnostics: optional_bool_field(entry, "diagnostics")?.unwrap_or(true),
    })
}

/// `extensionToLanguage`, normalized: keys lowercased and dot-prefixed so
/// `"rs"`, `".RS"` and `".rs"` all address the same files.
fn extension_map(
    entry: &serde_json::Map<String, Value>,
) -> Result<BTreeMap<String, String>, String> {
    let raw = match entry.get("extensionToLanguage") {
        None | Some(Value::Null) => return Ok(BTreeMap::new()),
        Some(Value::Object(values)) => values,
        Some(_) => {
            return Err(
                "\"extensionToLanguage\" must be an object mapping file extensions to language ids"
                    .into(),
            )
        }
    };
    let mut map = BTreeMap::new();
    for (extension, language) in raw {
        let Some(language) = language.as_str() else {
            return Err(format!(
                "\"extensionToLanguage\" entry \"{extension}\" must name a language id string"
            ));
        };
        let normalized = normalize_extension(extension);
        if normalized == "." {
            return Err("\"extensionToLanguage\" has an empty file extension".into());
        }
        map.insert(normalized, language.to_owned());
    }
    Ok(map)
}

/// Lowercases an extension and gives it the leading dot `Path::extension` does
/// not produce, so lookups can use one spelling.
pub fn normalize_extension(extension: &str) -> String {
    let trimmed = extension.trim();
    let bare = trimmed.strip_prefix('.').unwrap_or(trimmed);
    format!(".{}", bare.to_ascii_lowercase())
}

fn value_field(entry: &serde_json::Map<String, Value>, key: &str) -> Option<Value> {
    match entry.get(key) {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.clone()),
    }
}

fn millis_field(
    entry: &serde_json::Map<String, Value>,
    key: &str,
    default: u64,
) -> Result<u64, String> {
    match entry.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(value) => {
            let millis = value
                .as_u64()
                .ok_or_else(|| format!("\"{key}\" must be a whole number of milliseconds"))?;
            if millis == 0 {
                Err(format!("\"{key}\" must be greater than zero"))
            } else {
                Ok(millis)
            }
        }
    }
}

fn string_field(entry: &serde_json::Map<String, Value>, key: &str) -> Result<String, String> {
    match entry.get(key) {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(value)) => Ok(value.clone()),
        Some(_) => Err(format!("\"{key}\" must be a string")),
    }
}

fn optional_bool_field(
    entry: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<bool>, String> {
    match entry.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(format!("\"{key}\" must be true or false")),
    }
}

fn string_list_field(
    entry: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Vec<String>, String> {
    match entry.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("\"{key}\" must be an array of strings"))
            })
            .collect(),
        Some(_) => Err(format!("\"{key}\" must be an array of strings")),
    }
}

fn string_map_field(
    entry: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<BTreeMap<String, String>, String> {
    match entry.get(key) {
        None | Some(Value::Null) => Ok(BTreeMap::new()),
        Some(Value::Object(values)) => values
            .iter()
            .map(|(name, value)| {
                value
                    .as_str()
                    .map(|value| (name.clone(), value.to_owned()))
                    .ok_or_else(|| format!("\"{key}\" must map names to strings"))
            })
            .collect(),
        Some(_) => Err(format!("\"{key}\" must be an object of strings")),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_platform::host_platform;

    fn fake_env(name: &str) -> Option<String> {
        match name {
            "TOOLCHAIN" => Some("/opt/toolchain".into()),
            _ => None,
        }
    }

    fn parse(name: &str, entry: Value) -> Result<LspServerConfig, String> {
        parse_entry("id", name, &entry, &fake_env)
    }

    #[test]
    fn a_minimal_entry_needs_only_a_command_and_one_extension() {
        let config = parse(
            "rust",
            serde_json::json!({
                "command": "rust-analyzer",
                "extensionToLanguage": { ".rs": "rust" }
            }),
        )
        .expect("entry parses");
        assert_eq!(config.command, "rust-analyzer");
        assert_eq!(
            config.extension_to_language.get(".rs").map(String::as_str),
            Some("rust")
        );
        assert_eq!(
            config.startup_timeout_millis,
            DEFAULT_STARTUP_TIMEOUT_MILLIS
        );
        assert_eq!(config.max_restarts, DEFAULT_MAX_RESTARTS);
        assert!(config.restart_on_crash);
        assert!(config.diagnostics);
    }

    #[test]
    fn extensions_are_normalized_to_a_lowercase_dotted_form() {
        let config = parse(
            "ts",
            serde_json::json!({
                "command": "typescript-language-server",
                "args": ["--stdio"],
                "extensionToLanguage": { "TS": "typescript", ".Tsx": "typescriptreact" }
            }),
        )
        .expect("entry parses");
        let keys: Vec<&str> = config
            .extension_to_language
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, vec![".ts", ".tsx"]);
    }

    #[test]
    fn an_entry_without_an_extension_map_is_unavailable_rather_than_silent() {
        let reason = parse(
            "empty",
            serde_json::json!({ "command": "some-server" }),
        )
        .expect_err("an entry that claims no file kind can never be selected");
        assert!(reason.contains("extensionToLanguage"), "{reason}");
    }

    #[test]
    fn a_bare_command_with_spaces_is_refused_but_an_absolute_path_is_not() {
        let reason = parse(
            "spaced",
            serde_json::json!({
                "command": "typescript-language-server --stdio",
                "extensionToLanguage": { ".ts": "typescript" }
            }),
        )
        .expect_err("a bare command with spaces is an argument mistake");
        assert!(reason.contains("args"), "{reason}");

        let absolute = if host_platform().is_windows() {
            "C:\\Program Files\\lsp\\server.exe"
        } else {
            "/opt/Language Servers/server"
        };
        parse(
            "absolute",
            serde_json::json!({
                "command": absolute,
                "extensionToLanguage": { ".ts": "typescript" }
            }),
        )
        .expect("an absolute path may contain spaces");
    }

    #[test]
    fn the_socket_transport_says_why_it_is_not_available() {
        let reason = parse(
            "socketed",
            serde_json::json!({
                "command": "server",
                "transport": "socket",
                "extensionToLanguage": { ".x": "x" }
            }),
        )
        .expect_err("socket is in the source schema but never wired");
        assert!(reason.contains("stdio"), "{reason}");
    }

    #[test]
    fn environment_references_expand_and_a_missing_one_makes_the_row_unavailable() {
        let config = parse(
            "expanded",
            serde_json::json!({
                "command": "server",
                "env": { "TOOLCHAIN_ROOT": "${TOOLCHAIN}/lib" },
                "extensionToLanguage": { ".x": "x" }
            }),
        )
        .expect("entry parses");
        assert_eq!(
            config.env.get("TOOLCHAIN_ROOT").map(String::as_str),
            Some("/opt/toolchain/lib")
        );

        let reason = parse(
            "unset",
            serde_json::json!({
                "command": "${NOT_SET}",
                "extensionToLanguage": { ".x": "x" }
            }),
        )
        .expect_err("an unresolved reference must not be launched literally");
        assert!(reason.contains("NOT_SET"), "{reason}");
    }

    #[test]
    fn every_preset_declares_at_least_one_extension_and_a_bare_command() {
        for preset in LSP_SERVER_PRESETS {
            assert!(
                !preset.extensions.is_empty(),
                "{} claims no file extension",
                preset.name
            );
            assert!(
                !preset.command.contains(' '),
                "{} uses a spaced command instead of args",
                preset.name
            );
            assert!(
                server_name_is_valid(preset.name),
                "{} is not a name lsp.json could override",
                preset.name
            );
            for (extension, _) in preset.extensions {
                assert_eq!(
                    normalize_extension(extension),
                    *extension,
                    "{} declares an unnormalized extension",
                    preset.name
                );
            }
        }
    }

    #[test]
    fn a_preset_that_is_not_installed_is_listed_with_its_reason_and_no_config() {
        let entries = builtin_entries_with_resolver(ResolvedLanguage::EnUs, &|command| {
            command == "rust-analyzer"
        });
        let rust = entries
            .iter()
            .find(|entry| entry.descriptor.name == "rust-analyzer")
            .expect("the preset is listed");
        assert!(rust.descriptor.available);
        assert!(rust.config.is_some());

        let gopls = entries
            .iter()
            .find(|entry| entry.descriptor.name == "gopls")
            .expect("the preset is listed even when it is not installed");
        assert!(!gopls.descriptor.available);
        assert!(gopls.config.is_none());
        assert!(gopls.descriptor.description.contains("PATH"), "{}", gopls.descriptor.description);
    }
}
