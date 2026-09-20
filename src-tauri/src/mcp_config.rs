//! `mcp.json` — MCP servers as a file, in the `mcpServers` shape Claude Code's
//! `.mcp.json` and most MCP clients share.
//!
//! Mework reads `~/.mework/mcp.json` for every workspace and
//! `<workspace>/.mework/mcp.json` for one. Every entry becomes a catalog row a
//! conversation can select; an entry Mework cannot dial stays listed as
//! unavailable with the reason in its description, so a file copied from
//! another client explains itself instead of silently shrinking.

use std::{
    collections::BTreeMap,
    fs::{self, File},
    path::{Path, PathBuf},
};

use serde_json::Value;

use crate::{
    capabilities::{stable_id, write_config_file},
    model::{McpServerConfig, McpTransportKind, ResourceDescriptor, ResourceSource},
    prompt_profile::PromptKey,
};

/// Longest description shown in the catalog and the system prompt.
const DESCRIPTION_LIMIT: usize = 240;
/// Claude Code ignores per-server `timeout` values below one second.
const MINIMUM_TIMEOUT_MILLIS: u64 = 1_000;

/// One `mcpServers` entry: the catalog row, and the launch configuration when
/// the row is available.
#[derive(Clone, Debug)]
pub struct McpEntry {
    pub descriptor: ResourceDescriptor,
    /// `None` exactly when `descriptor.available` is false.
    pub config: Option<McpServerConfig>,
}

fn id_prefix(source: ResourceSource) -> &'static str {
    match source {
        ResourceSource::User => "mcp_user",
        ResourceSource::Workspace => "mcp_workspace",
        ResourceSource::Builtin => "mcp_builtin",
    }
}

/// The address of one entry: the file, then the JSON pointer of the key.
pub fn location_for(path: &Path, name: &str) -> String {
    format!("{}#/mcpServers/{name}", path.to_string_lossy())
}

/// Reads an entry's `location` back into the file and key it was built from.
pub fn parse_location(location: &str) -> Option<(PathBuf, String)> {
    let (path, name) = location.rsplit_once("#/mcpServers/")?;
    if path.is_empty() || name.is_empty() {
        return None;
    }
    Some((PathBuf::from(path), name.to_owned()))
}

/// Server names Claude Code accepts, so a file round-trips between the two
/// products; the three reserved words are prototype-pollution guards there and
/// stay refused here for the same portability.
pub fn server_name_is_valid(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        && !matches!(name, "__proto__" | "constructor" | "prototype")
}

/// Every entry of one `mcp.json`. A missing or unreadable file yields nothing;
/// a file whose top level is not `{ "mcpServers": { … } }` yields nothing and
/// says why on stderr, because there is no row to hang the reason on.
pub fn read_file(path: &Path, source: ResourceSource, workspace_id: Option<&str>) -> Vec<McpEntry> {
    read_file_with_env(path, source, workspace_id, &|name| std::env::var(name).ok())
}

pub fn read_file_with_env(
    path: &Path,
    source: ResourceSource,
    workspace_id: Option<&str>,
    env: &dyn Fn(&str) -> Option<String>,
) -> Vec<McpEntry> {
    let Ok(file) = File::open(path) else {
        return Vec::new();
    };
    let display = path.display();
    let value: Value = match serde_json::from_reader(file) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("{display} is not valid JSON and was skipped: {error}");
            return Vec::new();
        }
    };
    let Some(root) = value.as_object() else {
        eprintln!("{display} must be a JSON object with an \"mcpServers\" key; skipped");
        return Vec::new();
    };
    let servers = match root.get("mcpServers") {
        Some(Value::Object(servers)) => servers,
        Some(_) => {
            eprintln!("{display}: \"mcpServers\" must be an object keyed by server name; skipped");
            return Vec::new();
        }
        None if root.contains_key("servers") => {
            eprintln!(
                "{display}: missing \"mcpServers\" — found \"servers\" instead. Mework reads MCP servers from the \"mcpServers\" key; rename it."
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
                Ok(config) => McpEntry {
                    descriptor: ResourceDescriptor {
                        id,
                        name: name.clone(),
                        description: if config.description.is_empty() {
                            PromptKey::SystemMcpServerDefaultDescription
                                .builtin_en()
                                .to_owned()
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
                Err(reason) => McpEntry {
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

/// Builds the launch configuration for one entry, or the reason it cannot be
/// dialed.
fn parse_entry(
    id: &str,
    name: &str,
    entry: &Value,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<McpServerConfig, String> {
    if !server_name_is_valid(name) {
        return Err(
            "Server names may only contain letters, numbers, hyphens and underscores (and may not be __proto__, constructor or prototype)".into(),
        );
    }
    let Some(entry) = entry.as_object() else {
        return Err("The entry must be an object".into());
    };
    if entry.contains_key("headersHelper") {
        return Err(
            "\"headersHelper\" runs a program to obtain headers; Mework does not support it — put the headers in \"headers\" or use ${VAR} references".into(),
        );
    }
    if entry.contains_key("oauth") {
        return Err("\"oauth\" is not supported; Mework has no MCP OAuth client — send a token in \"headers\"".into());
    }

    let command = string_field(entry, "command")?;
    let url = string_field(entry, "url")?;
    let transport = match entry.get("type") {
        None => {
            if !command.trim().is_empty() {
                McpTransportKind::Stdio
            } else if !url.trim().is_empty() {
                return Err("The entry has a \"url\" but no \"type\"; add \"type\": \"http\"".into());
            } else {
                return Err("The entry needs a \"command\" (stdio) or a \"url\" with \"type\": \"http\"".into());
            }
        }
        Some(Value::String(kind)) => match kind.as_str() {
            "stdio" => McpTransportKind::Stdio,
            "http" | "streamable-http" | "streamable_http" => McpTransportKind::StreamableHttp,
            "sse" => {
                return Err("The HTTP+SSE transport (\"type\": \"sse\") is not supported; Streamable HTTP servers use \"type\": \"http\"".into())
            }
            "ws" => return Err("The WebSocket transport (\"type\": \"ws\") is not supported".into()),
            "sdk" => return Err("\"type\": \"sdk\" is an in-process Claude Code transport and has no file form".into()),
            other => return Err(format!("Unknown server type \"{other}\"; valid types are stdio and http (streamable-http)")),
        },
        Some(_) => return Err("\"type\" must be a string".into()),
    };

    let mut missing = Vec::new();
    let mut expand = |text: &str| expand_env_references(text, env, &mut missing);

    let command = expand(&command);
    let args = string_list_field(entry, "args")?
        .iter()
        .map(|argument| expand(argument))
        .collect::<Vec<_>>();
    let env_values = string_map_field(entry, "env")?
        .into_iter()
        .map(|(key, value)| (key, expand(&value)))
        .collect::<BTreeMap<_, _>>();
    let url = expand(&url);
    let headers = string_map_field(entry, "headers")?
        .into_iter()
        .map(|(key, value)| (key, expand(&value)))
        .collect::<BTreeMap<_, _>>();
    drop(expand);
    if !missing.is_empty() {
        missing.sort();
        missing.dedup();
        return Err(format!(
            "Missing environment variables: {}",
            missing.join(", ")
        ));
    }

    match transport {
        McpTransportKind::Stdio if command.trim().is_empty() => {
            return Err("A stdio server needs a non-empty \"command\"".into());
        }
        McpTransportKind::StreamableHttp if url.trim().is_empty() => {
            return Err("An http server needs a non-empty \"url\"".into());
        }
        _ => {}
    }

    let timeout_seconds = match (entry.get("timeoutSeconds"), entry.get("timeout")) {
        (Some(value), _) => {
            let seconds = value
                .as_u64()
                .ok_or_else(|| "\"timeoutSeconds\" must be a whole number of seconds".to_owned())?;
            u32::try_from(seconds).unwrap_or(u32::MAX)
        }
        (None, Some(value)) => {
            let millis = value
                .as_u64()
                .ok_or_else(|| "\"timeout\" must be a whole number of milliseconds".to_owned())?;
            if millis < MINIMUM_TIMEOUT_MILLIS {
                0
            } else {
                u32::try_from(millis.div_ceil(1_000)).unwrap_or(u32::MAX)
            }
        }
        (None, None) => 0,
    };

    let config = McpServerConfig {
        id: id.to_owned(),
        name: name.to_owned(),
        description: truncate_chars(
            string_field(entry, "description")?.trim(),
            DESCRIPTION_LIMIT,
        ),
        transport,
        command,
        args,
        env: env_values,
        cwd: string_field(entry, "cwd")?,
        env_passthrough: string_list_field(entry, "envPassthrough")?,
        registry_url: string_field(entry, "registryUrl")?,
        url,
        headers,
        timeout_seconds,
        long_running: bool_field(entry, "longRunning")?,
        disabled_tools: string_list_field(entry, "disabledTools")?,
        disabled_auto_approve_tools: string_list_field(entry, "disabledAutoApproveTools")?,
    };
    crate::mcp::validate_server_config(&config)?;
    Ok(config)
}

/// Replaces `${VAR}` and `${VAR:-default}` with the environment's value, as
/// Claude Code does in `.mcp.json`. A reference to an unset variable without a
/// default is kept verbatim and reported, so the entry is listed as unavailable
/// rather than dialed with a literal `${VAR}` in it.
pub fn expand_env_references(
    text: &str,
    env: &dyn Fn(&str) -> Option<String>,
    missing: &mut Vec<String>,
) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        output.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            output.push_str(&rest[start..]);
            return output;
        };
        let reference = &after[..end];
        let (name, default) = match reference.split_once(":-") {
            Some((name, default)) => (name, Some(default)),
            None => (reference, None),
        };
        let is_name = !name.is_empty()
            && name
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_');
        if !is_name {
            // Not a variable reference; leave the text as the user wrote it.
            output.push_str(&rest[start..start + 2 + end + 1]);
        } else {
            match env(name) {
                Some(value) => output.push_str(&value),
                None => match default {
                    Some(default) => output.push_str(default),
                    None => {
                        missing.push(name.to_owned());
                        output.push_str(&rest[start..start + 2 + end + 1]);
                    }
                },
            }
        }
        rest = &after[end + 1..];
    }
    output.push_str(rest);
    output
}

fn string_field(entry: &serde_json::Map<String, Value>, key: &str) -> Result<String, String> {
    match entry.get(key) {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(value)) => Ok(value.clone()),
        Some(_) => Err(format!("\"{key}\" must be a string")),
    }
}

fn bool_field(entry: &serde_json::Map<String, Value>, key: &str) -> Result<bool, String> {
    match entry.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
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

/// Drops one server from its `mcp.json`, leaving every other entry and every
/// other top-level key exactly as the user wrote them — byte for byte, in the
/// order they wrote them. A project may have this file in version control, so
/// deleting one server must not reflow the rest into a whole-file diff.
pub fn remove_server_from_file(path: &Path, name: &str) -> Result<(), String> {
    let display = path.display();
    let text = fs::read_to_string(path).map_err(|error| format!("无法读取 {display}：{error}"))?;
    // Parse first, so a malformed file is reported as such rather than cut into.
    let document: Value = serde_json::from_str(&text)
        .map_err(|error| format!("{display} 不是合法的 JSON：{error}"))?;
    if !document
        .get("mcpServers")
        .and_then(Value::as_object)
        .is_some_and(|servers| servers.contains_key(name))
    {
        return Err(format!("{display} 里已经没有这台服务器了"));
    }
    let edited = crate::json_edit::remove_object_member(&text, &["mcpServers"], name)
        .map_err(|error| format!("无法从 {display} 里删除这台服务器：{error}"))?;
    write_config_file(path, &edited)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_env(name: &str) -> Option<String> {
        match name {
            "HOME_DIR" => Some("/home/me".into()),
            "TOKEN" => Some("secret".into()),
            _ => None,
        }
    }

    fn write_file(temp: &tempfile::TempDir, json: &str) -> PathBuf {
        let path = temp.path().join("mcp.json");
        fs::write(&path, json).unwrap();
        path
    }

    #[test]
    fn claude_code_shaped_entries_become_dialable_servers() {
        let temp = tempfile::tempdir().unwrap();
        let path = write_file(
            &temp,
            r#"{
              "mcpServers": {
                "filesystem": {
                  "command": "npx",
                  "args": ["-y", "@modelcontextprotocol/server-filesystem", "${HOME_DIR}/projects"],
                  "env": { "LOG_LEVEL": "${LOG_LEVEL:-info}" }
                },
                "docs": {
                  "type": "http",
                  "url": "http://127.0.0.1:3000/mcp",
                  "headers": { "Authorization": "Bearer ${TOKEN}" },
                  "timeout": 90000,
                  "description": "Project docs"
                }
              }
            }"#,
        );

        let entries = read_file_with_env(&path, ResourceSource::User, None, &fake_env);

        assert_eq!(entries.len(), 2);
        let filesystem = entries
            .iter()
            .find(|entry| entry.descriptor.name == "filesystem")
            .unwrap();
        assert!(filesystem.descriptor.available);
        assert!(filesystem.descriptor.id.starts_with("mcp_user_filesystem_"));
        assert_eq!(
            filesystem.descriptor.location,
            location_for(&path, "filesystem")
        );
        let config = filesystem.config.as_ref().unwrap();
        assert_eq!(config.transport, McpTransportKind::Stdio);
        assert_eq!(config.args[2], "/home/me/projects");
        assert_eq!(config.env["LOG_LEVEL"], "info");
        assert_eq!(config.timeout_seconds, 0);

        let docs = entries
            .iter()
            .find(|entry| entry.descriptor.name == "docs")
            .unwrap();
        let config = docs.config.as_ref().unwrap();
        assert_eq!(config.transport, McpTransportKind::StreamableHttp);
        assert_eq!(config.headers["Authorization"], "Bearer secret");
        assert_eq!(config.timeout_seconds, 90);
        assert_eq!(docs.descriptor.description, "Project docs");
    }

    #[test]
    fn entries_mework_cannot_dial_stay_listed_with_the_reason() {
        let temp = tempfile::tempdir().unwrap();
        let path = write_file(
            &temp,
            r#"{
              "mcpServers": {
                "legacy": { "type": "sse", "url": "http://127.0.0.1:1/sse" },
                "untyped": { "url": "http://127.0.0.1:1/mcp" },
                "needs-var": { "command": "${MISSING_TOOL}", "args": ["${ALSO_MISSING}"] },
                "helper": { "type": "http", "url": "http://127.0.0.1:1/mcp", "headersHelper": "gettoken" },
                "bad args": { "command": "x" },
                "__proto__": { "command": "x" },
                "empty": {}
              }
            }"#,
        );

        let entries = read_file_with_env(&path, ResourceSource::Workspace, Some("ws"), &fake_env);
        let reason = |name: &str| {
            let entry = entries
                .iter()
                .find(|entry| entry.descriptor.name == name)
                .unwrap();
            assert!(!entry.descriptor.available, "{name} should be unavailable");
            assert!(entry.config.is_none());
            assert_eq!(entry.descriptor.workspace_id.as_deref(), Some("ws"));
            entry.descriptor.description.clone()
        };

        assert!(reason("legacy").contains("SSE"));
        assert!(reason("untyped").contains("\"type\": \"http\""));
        assert_eq!(
            reason("needs-var"),
            "Missing environment variables: ALSO_MISSING, MISSING_TOOL"
        );
        assert!(reason("helper").contains("headersHelper"));
        assert!(reason("bad args").contains("letters, numbers"));
        assert!(reason("__proto__").contains("__proto__"));
        assert!(reason("empty").contains("\"command\""));
    }

    #[test]
    fn the_claude_desktop_array_form_and_a_servers_key_yield_nothing() {
        let temp = tempfile::tempdir().unwrap();
        for json in [
            r#"{ "servers": { "a": { "command": "x" } } }"#,
            r#"{ "mcpServers": [ { "name": "a", "command": "x" } ] }"#,
            "[]",
            "not json",
        ] {
            let path = write_file(&temp, json);
            assert!(read_file_with_env(&path, ResourceSource::User, None, &fake_env).is_empty());
        }
        assert!(read_file(&temp.path().join("absent.json"), ResourceSource::User, None).is_empty());
    }

    #[test]
    fn env_references_expand_like_claude_code() {
        let mut missing = Vec::new();
        assert_eq!(
            expand_env_references("${HOME_DIR}/x-${TOKEN}", &fake_env, &mut missing),
            "/home/me/x-secret"
        );
        assert_eq!(
            expand_env_references("${NOPE:-fallback}|${NOPE:-}", &fake_env, &mut missing),
            "fallback|"
        );
        assert!(missing.is_empty());
        // Malformed references are text, not variables.
        assert_eq!(
            expand_env_references("${not a var} ${", &fake_env, &mut missing),
            "${not a var} ${"
        );
        assert!(missing.is_empty());
        assert_eq!(
            expand_env_references("${GONE}", &fake_env, &mut missing),
            "${GONE}"
        );
        assert_eq!(missing, vec!["GONE".to_owned()]);
    }

    #[test]
    fn removing_a_server_leaves_the_rest_of_the_file_alone() {
        let temp = tempfile::tempdir().unwrap();
        let path = write_file(
            &temp,
            r#"{ "note": "mine", "mcpServers": { "a": { "command": "x" }, "b": { "command": "y", "args": [] } } }"#,
        );

        remove_server_from_file(&path, "a").unwrap();

        let value: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["note"], "mine");
        assert!(value["mcpServers"].get("a").is_none());
        assert_eq!(value["mcpServers"]["b"]["command"], "y");
        let error = remove_server_from_file(&path, "a").unwrap_err();
        assert!(error.contains("已经没有"), "{error}");
        assert!(read_file_with_env(&path, ResourceSource::User, None, &fake_env).len() == 1);
    }

    #[test]
    fn locations_parse_back_into_file_and_name() {
        let path = Path::new("C:\\Users\\me\\.mework\\mcp.json");
        let location = location_for(path, "docs");
        assert_eq!(
            parse_location(&location),
            Some((path.to_path_buf(), "docs".to_owned()))
        );
        assert_eq!(
            parse_location("C:\\x\\hooks.json#/hooks/Stop/0/hooks/0"),
            None
        );
        assert_eq!(parse_location("#/mcpServers/x"), None);
    }
}
