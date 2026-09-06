use reqwest::{Client, Response};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fmt;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::thread;
use std::time::{Duration, Instant};
use url::Url;
use wait_timeout::ChildExt;

#[path = "managed_mcp_http.rs"]
mod managed_mcp_http;
use managed_mcp_http::{
    acquire_managed_http_sidecar, validate_managed_http_launch, ManagedHttpLaunch,
    ManagedHttpSidecar,
};

pub const LATEST_PROTOCOL_VERSION: &str = "2025-11-25";
pub const COMPATIBLE_PROTOCOL_VERSIONS: &[&str] =
    &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const CLIENT_NAME: &str = "Mework";
const MAX_SERVERS: usize = 128;
const MAX_TOOLS: usize = 2_048;
const MAX_TOOL_PAGES: usize = 128;
const MAX_CONTENT_ITEMS: usize = 512;
const MAX_SERVER_SCHEMA_BYTES: usize = 512 * 1024;
const DEFAULT_TOTAL_SCHEMA_BYTES: usize = 2 * 1024 * 1024;
const MAX_CONFIGURED_SCHEMA_BYTES: usize = 8 * 1024 * 1024;
const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;
const MAX_OUTBOUND_BYTES: usize = 2 * 1024 * 1024;
const MAX_HTTP_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_ARGUMENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_COMMAND_BYTES: usize = 32 * 1024;
const MAX_ARGUMENT_COUNT: usize = 1_024;
const MAX_ENVIRONMENT_ENTRIES: usize = 256;
const MAX_HEADERS: usize = 128;
const MAX_HEADER_VALUE_BYTES: usize = 16 * 1024;
const MAX_CURSOR_BYTES: usize = 16 * 1024;
const MAX_REMOTE_NAME_BYTES: usize = 512;
const MAX_SESSION_ID_BYTES: usize = 1_024;
const MAX_TOOL_TITLE_DISPLAY_CHARS: usize = 240;
const MAX_TOOL_DESCRIPTION_DISPLAY_CHARS: usize = 4_000;
const MAX_REQUEST_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MAX_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const MANAGED_SESSION_LIMIT: usize = 32;
const MANAGED_SESSION_TTL: Duration = Duration::from_secs(30 * 60);
const MANAGED_COMMAND_QUEUE: usize = 8;
const MANAGED_WAIT_SLICE: Duration = Duration::from_millis(25);
const MANAGED_IDLE_PUMP_SLICE: Duration = Duration::from_millis(200);
const MANAGED_IDLE_FRAME_BUDGET: usize = 32;
/// Maximum bytes retained from one stdio server stderr line.
const MAX_STDERR_LINE_BYTES: usize = 8 * 1024;
/// Maximum retained stderr lines. This diagnostic buffer answers only what the
/// most recent connection attempt reported.
const MAX_STDERR_LINES: usize = 200;
/// Per-page entry limits for `prompts/list` and `resources/list`.
const MAX_PROMPTS: usize = 512;
const MAX_RESOURCES: usize = 512;

const SESSION_HEADER: &str = "mcp-session-id";
const PROTOCOL_HEADER: &str = "mcp-protocol-version";

/// Bounded ring buffer for stdio subprocess stderr. Its contents are displayed
/// only as settings diagnostics and never enter model context or persistence.
#[derive(Default)]
struct StderrLog {
    lines: Mutex<VecDeque<String>>,
}

impl StderrLog {
    fn push(&self, line: &str) {
        let mut lines = self
            .lines
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if lines.len() == MAX_STDERR_LINES {
            lines.pop_front();
        }
        let mut text: String = line
            .chars()
            .filter(|character| !character.is_control() || *character == '\t')
            .take(MAX_STDERR_LINE_BYTES)
            .collect();
        text.truncate(MAX_STDERR_LINE_BYTES);
        lines.push_back(text);
    }

    fn snapshot(&self) -> Vec<String> {
        self.lines
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .cloned()
            .collect()
    }
}

/// A way to start an MCP server. Credentials are used only for connection and
/// are always redacted by `Debug` to prevent log or crash-report disclosure.
#[derive(Clone, PartialEq)]
pub enum RuntimeMcpTransport {
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
        cwd: Option<String>,
    },
    Http {
        url: String,
        headers: BTreeMap<String, String>,
    },
    #[allow(dead_code)]
    ManagedHttp {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
        cwd: Option<String>,
        url: String,
        headers: BTreeMap<String, String>,
    },
}

impl fmt::Debug for RuntimeMcpTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stdio { args, env, cwd, .. } => formatter
                .debug_struct("Stdio")
                .field("command", &"<redacted>")
                .field("argument_count", &args.len())
                .field("environment_count", &env.len())
                .field("has_working_directory", &cwd.is_some())
                .finish(),
            Self::Http { headers, .. } => formatter
                .debug_struct("Http")
                .field("url", &"<redacted>")
                .field("header_count", &headers.len())
                .finish(),
            Self::ManagedHttp {
                args,
                env,
                cwd,
                headers,
                ..
            } => formatter
                .debug_struct("ManagedHttp")
                .field("command", &"<redacted>")
                .field("argument_count", &args.len())
                .field("environment_count", &env.len())
                .field("has_working_directory", &cwd.is_some())
                .field("url", &"<loopback>")
                .field("header_count", &headers.len())
                .finish(),
        }
    }
}

/// Enabled document servers selected by this conversation, in document order.
/// Both conditions are evaluated by the host: renderer-provided `mcp_ids` only
/// intersect trusted document configuration because commands and environments
/// are executed directly.
pub fn selected_runtime_servers(
    document: &crate::model::AppDocument,
    conversation: &crate::model::Conversation,
) -> Vec<RuntimeMcpServer> {
    let selected = conversation
        .settings
        .mcp_ids
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    document
        .assets
        .mcp_servers
        .iter()
        .filter(|server| server.enabled && selected.contains(server.id.as_str()))
        .map(RuntimeMcpServer::from_config)
        .collect()
}

/// An MCP server awaiting connection. `artifact_id` identifies its source for
/// session isolation and eviction.
#[derive(Clone, PartialEq)]
pub struct RuntimeMcpServer {
    pub artifact_id: String,
    pub server_id: String,
    pub name: String,
    pub description: String,
    pub transport: RuntimeMcpTransport,
    /// Remote tool names the user switched off on the server's Tools tab.
    /// They are dropped at discovery, so the model never sees them.
    pub disabled_tools: Vec<String>,
    /// Remote tool names whose auto-approve switch the user turned off: every
    /// call asks, at every security level, like a remote
    /// `requiresUserInteraction` declaration.
    pub confirm_every_call_tools: Vec<String>,
    /// Per-server `tools/call` timeout; `None` uses the client default.
    pub request_timeout: Option<Duration>,
}

impl RuntimeMcpServer {
    /// Builds a connectable server from trusted document configuration.
    /// Never construct this from renderer IPC: `command` executes directly and
    /// `env` enters its process environment. `artifact_id` is the server id so a
    /// configuration change evicts and reconnects only that server.
    pub fn from_config(config: &crate::model::McpServerConfig) -> Self {
        let transport = match config.transport {
            crate::model::McpTransportKind::Stdio => RuntimeMcpTransport::Stdio {
                command: config.command.clone(),
                args: config.args.clone(),
                env: with_registry_mirror(&config.command, &config.registry_url, &config.env),
                cwd: None,
            },
            crate::model::McpTransportKind::StreamableHttp => RuntimeMcpTransport::Http {
                url: config.url.clone(),
                headers: config.headers.clone(),
            },
        };
        Self {
            artifact_id: config.id.clone(),
            server_id: config.id.clone(),
            name: config.name.clone(),
            description: config.description.clone(),
            transport,
            disabled_tools: config.disabled_tools.clone(),
            confirm_every_call_tools: config.disabled_auto_approve_tools.clone(),
            request_timeout: configured_request_timeout(config.timeout_seconds, config.long_running),
        }
    }

    /// The timeout one of this server's `tools/call` requests gets.
    pub fn effective_request_timeout(&self, default: Duration) -> Duration {
        self.request_timeout.unwrap_or(default)
    }
}

/// Maps the two user-facing timeout settings onto one duration. An explicit
/// `timeoutSeconds` wins and is clamped to the transport ceiling; `longRunning`
/// alone lifts the default to that ceiling; neither keeps the client default.
fn configured_request_timeout(timeout_seconds: u32, long_running: bool) -> Option<Duration> {
    if timeout_seconds > 0 {
        return Some(Duration::from_secs(u64::from(timeout_seconds)).min(MAX_REQUEST_TIMEOUT));
    }
    long_running.then_some(MAX_REQUEST_TIMEOUT)
}

/// Converts the package-registry mirror setting into child-process environment
/// variables. Only recognized toolchains receive it, and explicit environment
/// entries always take precedence.
fn with_registry_mirror(
    command: &str,
    registry_url: &str,
    env: &std::collections::BTreeMap<String, String>,
) -> std::collections::BTreeMap<String, String> {
    let mut merged = env.clone();
    let registry = registry_url.trim();
    if registry.is_empty() {
        return merged;
    }
    // Accept only HTTP(S): a `file:` URL or bare path is not a package registry
    // and must not reach a child environment.
    if !registry.starts_with("http://") && !registry.starts_with("https://") {
        return merged;
    }
    for variable in registry_mirror_variables(command) {
        merged
            .entry((*variable).to_owned())
            .or_insert_with(|| registry.to_owned());
    }
    merged
}

/// Maps a command to its supported registry-mirror environment variables.
fn registry_mirror_variables(command: &str) -> &'static [&'static str] {
    let executable = command
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(command)
        .to_ascii_lowercase();
    let stem = executable
        .strip_suffix(".exe")
        .or_else(|| executable.strip_suffix(".cmd"))
        .or_else(|| executable.strip_suffix(".bat"))
        .unwrap_or(&executable);
    match stem {
        "npx" | "npm" | "bun" | "bunx" | "pnpm" | "yarn" => &["npm_config_registry"],
        "uv" | "uvx" | "pip" | "pipx" | "python" | "python3" => &["UV_INDEX_URL", "PIP_INDEX_URL"],
        _ => &[],
    }
}

impl fmt::Debug for RuntimeMcpServer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeMcpServer")
            .field("artifact_id", &self.artifact_id)
            .field("server_id", &self.server_id)
            .field("name", &self.name)
            .field("description_bytes", &self.description.len())
            .field("transport", &self.transport)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpErrorKind {
    InvalidConfiguration,
    Bounds,
    Transport,
    Timeout,
    Cancelled,
    Protocol,
    Remote,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpError {
    pub kind: McpErrorKind,
    pub server_id: Option<String>,
    pub message: String,
}

impl McpError {
    fn new(kind: McpErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            server_id: None,
            message: message.into(),
        }
    }

    fn for_server(mut self, server: &RuntimeMcpServer) -> Self {
        if self.server_id.is_none() {
            self.server_id = Some(sanitize_error_text(&server.server_id, 160));
        }
        self
    }
}

impl fmt::Display for McpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(server_id) = &self.server_id {
            write!(formatter, "MCP {server_id}: {}", self.message)
        } else {
            formatter.write_str(&self.message)
        }
    }
}

impl std::error::Error for McpError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct McpClientOptions {
    pub request_timeout: Duration,
    pub shutdown_timeout: Duration,
    /// Hard wall-clock budget for discovering every enabled server in one model run.
    pub discovery_timeout: Duration,
    /// Cumulative serialized MCP tool-definition budget added to a provider request.
    pub max_total_schema_bytes: usize,
}

impl Default for McpClientOptions {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(45),
            shutdown_timeout: Duration::from_millis(500),
            discovery_timeout: Duration::from_secs(60),
            max_total_schema_bytes: DEFAULT_TOTAL_SCHEMA_BYTES,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpRemoteTool {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default = "empty_object_schema")]
    pub input_schema: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Value>,
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

#[derive(Clone, PartialEq)]
pub struct McpToolBinding {
    pub exposed_name: String,
    pub remote_name: String,
    pub title: String,
    pub description: String,
    pub input_schema: Value,
    pub output_schema: Option<Value>,
    pub annotations: Option<Value>,
    /// Claude-compatible per-call interaction requirement advertised by the
    /// remote tool through `_meta["anthropic/requiresUserInteraction"]`.
    pub requires_user_interaction: bool,
    /// The user turned this tool's auto-approve switch off on the server's
    /// Tools tab. Same effect as the remote declaration, different origin.
    pub user_requires_confirmation: bool,
    pub negotiated_protocol_version: String,
    pub server: RuntimeMcpServer,
}

impl McpToolBinding {
    /// Whether every call of this tool must be confirmed by the user, whoever
    /// asked for it: the remote server or the user's own configuration.
    pub fn confirmation_required(&self) -> bool {
        self.requires_user_interaction || self.user_requires_confirmation
    }
}

impl fmt::Debug for McpToolBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let transport = match &self.server.transport {
            RuntimeMcpTransport::Stdio { .. } => "stdio",
            RuntimeMcpTransport::Http { .. } => "http",
            RuntimeMcpTransport::ManagedHttp { .. } => "managed-http",
        };
        formatter
            .debug_struct("McpToolBinding")
            .field("exposed_name", &self.exposed_name)
            .field("remote_name", &self.remote_name)
            .field("title_bytes", &self.title.len())
            .field("description_bytes", &self.description.len())
            .field(
                "input_schema_bytes",
                &serialized_len(&self.input_schema).unwrap_or(usize::MAX),
            )
            .field("has_output_schema", &self.output_schema.is_some())
            .field("has_annotations", &self.annotations.is_some())
            .field("requires_user_interaction", &self.requires_user_interaction)
            .field(
                "negotiated_protocol_version",
                &self.negotiated_protocol_version,
            )
            .field("artifact_id", &self.server.artifact_id)
            .field("server_id", &self.server.server_id)
            .field("server_name", &self.server.name)
            .field("transport", &transport)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpDiscoveryFailure {
    pub server_id: String,
    pub server_name: String,
    pub kind: McpErrorKind,
    /// Deliberately coarse: never includes command arguments, env, URLs,
    /// headers, server-returned messages, or any other secret-bearing value.
    pub message: String,
}

impl McpDiscoveryFailure {
    fn from_error(server: &RuntimeMcpServer, error: &McpError) -> Self {
        let message = match error.kind {
            McpErrorKind::InvalidConfiguration => "MCP configuration is invalid",
            McpErrorKind::Bounds => "MCP tool list exceeds safety limits",
            McpErrorKind::Transport => "Could not connect to MCP server",
            McpErrorKind::Timeout => "MCP server probe timed out",
            McpErrorKind::Cancelled => "MCP server probe was cancelled",
            McpErrorKind::Protocol => "MCP server returned incompatible protocol data",
            McpErrorKind::Remote => "MCP server rejected tool discovery",
        };
        Self {
            server_id: sanitize_error_text(&server.server_id, 160),
            server_name: sanitize_error_text(&server.name, 160),
            kind: error.kind,
            message: message.to_owned(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct McpDiscoveryReport {
    pub bindings: Vec<McpToolBinding>,
    pub failures: Vec<McpDiscoveryFailure>,
    pub schema_bytes: usize,
    pub deadline_reached: bool,
}

/// One `prompts/list` entry for the settings page.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct McpPromptSummary {
    pub name: String,
    pub title: String,
    pub description: String,
    pub arguments: Vec<McpPromptArgumentSummary>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct McpPromptArgumentSummary {
    pub name: String,
    pub description: String,
    pub required: bool,
}

/// One `resources/list` entry for the settings page.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct McpResourceSummary {
    pub uri: String,
    pub name: String,
    pub title: String,
    pub description: String,
    pub mime_type: String,
    pub size: u64,
}

/// Complete result of one settings-page probe.
/// Unlike model-run discovery, probing exposes remote metadata, prompts,
/// resources, and stderr diagnostics for human inspection.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct McpProbeOutcome {
    pub protocol_version: String,
    pub server_name: String,
    pub server_version: String,
    pub tools: Vec<McpToolBinding>,
    pub prompts: Vec<McpPromptSummary>,
    pub resources: Vec<McpResourceSummary>,
    /// Subprocess stderr, including failure-path diagnostics.
    pub diagnostics: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolCallResult {
    #[serde(default)]
    pub content: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
    #[serde(default)]
    pub is_error: bool,
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

impl McpToolCallResult {
    /// Keeps model-visible MCP content types intact while excluding top-level
    /// `_meta`, which is reserved for host/application data such as auth
    /// challenges and must not enter model context or durable ToolResult data.
    pub fn model_output(&self) -> String {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct ModelOutput<'a> {
            content: &'a [Value],
            #[serde(skip_serializing_if = "Option::is_none")]
            structured_content: Option<&'a Value>,
            is_error: bool,
        }

        serde_json::to_string(&ModelOutput {
            content: &self.content,
            structured_content: self.structured_content.as_ref(),
            is_error: self.is_error,
        })
        .unwrap_or_else(|_| r#"{"content":[],"isError":true}"#.to_owned())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpSessionInfo {
    pub protocol_version: String,
    pub server_name: Option<String>,
    pub server_version: Option<String>,
    /// Whether the remote declared prompts support at initialization. Do not call
    /// `prompts/list` without it: missing prompts are not an error.
    pub supports_prompts: bool,
    /// Equivalent declaration for resources support.
    pub supports_resources: bool,
}

/// Client for one-shot bulk server probes. It opens and closes a connection
/// without touching conversation-owned pooled sessions.
#[derive(Clone, Debug)]
pub struct McpClient {
    options: McpClientOptions,
}

impl Default for McpClient {
    fn default() -> Self {
        Self {
            options: McpClientOptions::default(),
        }
    }
}

impl McpClient {
    /// Builds a client with non-default budgets. Production discovery uses [`Self::default`]
    /// and tool calls run through [`McpSessionManager`], so tuned budgets only matter to the
    /// transport tests below.
    #[cfg(test)]
    pub fn new(options: McpClientOptions) -> Result<Self, McpError> {
        validate_options(options)?;
        Ok(Self { options })
    }

    #[allow(dead_code)]
    pub fn discover_tools(
        &self,
        servers: &[RuntimeMcpServer],
    ) -> Result<Vec<McpToolBinding>, McpError> {
        let report = self.discover_tools_report(servers);
        if let Some(failure) = report.failures.first() {
            return Err(McpError {
                kind: failure.kind,
                server_id: Some(failure.server_id.clone()),
                message: failure.message.clone(),
            });
        }
        Ok(report.bindings)
    }

    /// Best-effort discovery used by model runs. Every server is an isolation
    /// boundary: a broken or malicious server is reported and skipped without
    /// removing tools successfully discovered from other servers.
    pub fn discover_tools_report(&self, servers: &[RuntimeMcpServer]) -> McpDiscoveryReport {
        let mut report = McpDiscoveryReport::default();
        let mut exposed_names = HashSet::new();
        let deadline = Instant::now()
            .checked_add(self.options.discovery_timeout)
            .unwrap_or_else(Instant::now);
        for (index, server) in servers.iter().enumerate() {
            if index >= MAX_SERVERS {
                let error = McpError::new(
                    McpErrorKind::Bounds,
                    format!("At most {MAX_SERVERS} MCP servers can be connected per run"),
                );
                report
                    .failures
                    .push(McpDiscoveryFailure::from_error(server, &error));
                break;
            }
            if Instant::now() >= deadline {
                report.deadline_reached = true;
                let error = McpError::new(
                    McpErrorKind::Timeout,
                    "MCP tool discovery exceeded the global time budget",
                );
                for pending in &servers[index..servers.len().min(MAX_SERVERS)] {
                    report
                        .failures
                        .push(McpDiscoveryFailure::from_error(pending, &error));
                }
                break;
            }
            let server_bindings = match self.discover_server_tools_until(server, Some(deadline)) {
                Ok(bindings) => bindings,
                Err(error) => {
                    if error.kind == McpErrorKind::Timeout && Instant::now() >= deadline {
                        report.deadline_reached = true;
                    }
                    report
                        .failures
                        .push(McpDiscoveryFailure::from_error(server, &error));
                    continue;
                }
            };
            if let Err(error) = merge_discovered_bindings(
                &mut report,
                &mut exposed_names,
                server_bindings,
                self.options.max_total_schema_bytes,
            ) {
                report
                    .failures
                    .push(McpDiscoveryFailure::from_error(server, &error));
            }
        }
        report
    }

    /// Single-server discovery. Conversations discover through [`Self::discover_tools`] over the
    /// whole enabled set, so this narrower entry point covers the transport tests below.
    #[cfg(test)]
    pub fn discover_server_tools(
        &self,
        server: &RuntimeMcpServer,
    ) -> Result<Vec<McpToolBinding>, McpError> {
        let deadline = Instant::now()
            .checked_add(self.options.discovery_timeout)
            .ok_or_else(|| {
                McpError::new(
                    McpErrorKind::Bounds,
                    "MCP tool discovery time budget is invalid",
                )
            })?;
        self.discover_server_tools_until(server, Some(deadline))
    }

    fn discover_server_tools_until(
        &self,
        server: &RuntimeMcpServer,
        deadline: Option<Instant>,
    ) -> Result<Vec<McpToolBinding>, McpError> {
        validate_runtime_server(server).map_err(|error| error.for_server(server))?;
        let (mut connection, session_info) =
            initialize_connection(server, self.options, deadline, None, None)
                .map_err(|error| error.for_server(server))?;
        let tools = list_tools(&mut connection, self.options.request_timeout, deadline)
            .map_err(|error| error.for_server(server))?;
        Ok(bindings_from_remote_tools(server, &session_info, tools))
    }

    /// Settings connectivity probe: connects, handshakes, lists tools/prompts/
    /// resources, then disconnects. Failure results retain stderr diagnostics.
    ///
    /// `abort` is the in-flight registration's token, when the probe is
    /// cancellable. Abandoning it terminates a stdio child at once, and every
    /// phase boundary re-reads it so no further request is issued. Cancelling
    /// tears the connection down, so the transport reports a closed pipe or a
    /// timeout; the probe reports [`McpErrorKind::Cancelled`] instead of that
    /// symptom, and never absorbs a cancellation into an empty optional section
    /// that would let the probe finish as a success.
    fn probe_server(
        &self,
        server: &RuntimeMcpServer,
        abort: Option<&Arc<ManagedAbortToken>>,
    ) -> Result<McpProbeOutcome, (McpError, Vec<String>)> {
        let deadline = Instant::now().checked_add(self.options.discovery_timeout);
        validate_runtime_server(server).map_err(|error| (error.for_server(server), Vec::new()))?;
        if probe_cancelled(abort) {
            return Err((probe_cancelled_error().for_server(server), Vec::new()));
        }
        let (mut connection, session_info) =
            initialize_connection(server, self.options, deadline, None, abort.cloned()).map_err(
                |error| {
                    (
                        probe_outcome_error(error, abort).for_server(server),
                        Vec::new(),
                    )
                },
            )?;

        let request_timeout = self.options.request_timeout;
        if probe_cancelled(abort) {
            return Err((
                probe_cancelled_error().for_server(server),
                connection.diagnostics(),
            ));
        }
        let tools = match list_tools(&mut connection, request_timeout, deadline) {
            Ok(tools) => tools,
            Err(error) => {
                return Err((
                    probe_outcome_error(error, abort).for_server(server),
                    connection.diagnostics(),
                ))
            }
        };
        // Prompts and resources are optional. Do not query undeclared support;
        // failures there leave only that section empty, while tools are required.
        // A cancellation is not such a failure: swallowing it here would report a
        // probe the user stopped as a successful connection.
        let prompts = if session_info.supports_prompts && !probe_cancelled(abort) {
            match list_prompts(&mut connection, request_timeout, deadline) {
                Ok(prompts) => prompts,
                Err(_) if probe_cancelled(abort) => {
                    return Err((
                        probe_cancelled_error().for_server(server),
                        connection.diagnostics(),
                    ))
                }
                Err(_) => Vec::new(),
            }
        } else {
            Vec::new()
        };
        let resources = if session_info.supports_resources && !probe_cancelled(abort) {
            match list_resources(&mut connection, request_timeout, deadline) {
                Ok(resources) => resources,
                Err(_) if probe_cancelled(abort) => {
                    return Err((
                        probe_cancelled_error().for_server(server),
                        connection.diagnostics(),
                    ))
                }
                Err(_) => Vec::new(),
            }
        } else {
            Vec::new()
        };
        if probe_cancelled(abort) {
            return Err((
                probe_cancelled_error().for_server(server),
                connection.diagnostics(),
            ));
        }

        Ok(McpProbeOutcome {
            protocol_version: session_info.protocol_version.clone(),
            server_name: session_info.server_name.clone().unwrap_or_default(),
            server_version: session_info.server_version.clone().unwrap_or_default(),
            diagnostics: connection.diagnostics(),
            tools: bindings_from_remote_tools(server, &session_info, tools),
            prompts,
            resources,
        })
    }

    /// One-shot tool call that connects, calls and shuts the server down again. Conversations
    /// call through [`McpSessionManager`], which keeps one initialized session per artifact;
    /// these entry points exercise the shared connection and result-parsing layer in tests.
    #[cfg(test)]
    pub fn call_tool(
        &self,
        binding: &McpToolBinding,
        arguments: Value,
    ) -> Result<McpToolCallResult, McpError> {
        self.call_server_tool(&binding.server, &binding.remote_name, arguments)
    }

    #[cfg(test)]
    pub fn call_server_tool(
        &self,
        server: &RuntimeMcpServer,
        remote_name: &str,
        arguments: Value,
    ) -> Result<McpToolCallResult, McpError> {
        validate_runtime_server(server).map_err(|error| error.for_server(server))?;
        validate_remote_name(remote_name).map_err(|error| error.for_server(server))?;
        let arguments = normalize_arguments(arguments).map_err(|error| error.for_server(server))?;
        let (mut connection, _) = initialize_connection(server, self.options, None, None, None)
            .map_err(|error| error.for_server(server))?;
        let result = connection
            .request(
                "tools/call",
                json!({
                    "name": remote_name,
                    "arguments": arguments,
                }),
                self.options.request_timeout,
            )
            .map_err(|error| error.for_server(server))?;
        parse_call_result(result).map_err(|error| error.for_server(server))
    }
}

/// Maximum bytes in a renderer-minted probe id.
const MAX_PROBE_ID_BYTES: usize = 128;
/// Cancels that arrive before their probe registers are remembered so the probe
/// never starts. Ids are minted per probe, so an entry no probe ever claims only
/// ages out of this bounded queue.
const MAX_PENDING_PROBE_CANCELS: usize = 64;

/// Settings-page probes the user can cancel.
///
/// Registration is keyed by the renderer-minted probe id rather than by server id,
/// so a cancel reaches exactly the dial the settings page started: never a second
/// probe of the same server, and never a conversation's pooled session.
static PROBE_REGISTRY: OnceLock<Mutex<ProbeRegistry>> = OnceLock::new();

#[derive(Default)]
struct ProbeRegistry {
    live: HashMap<String, Arc<ManagedAbortToken>>,
    cancelled_before_start: VecDeque<String>,
}

fn probe_registry() -> &'static Mutex<ProbeRegistry> {
    PROBE_REGISTRY.get_or_init(|| Mutex::new(ProbeRegistry::default()))
}

fn lock_probe_registry() -> std::sync::MutexGuard<'static, ProbeRegistry> {
    probe_registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn validate_probe_id(probe_id: &str) -> Result<(), McpError> {
    if probe_id.is_empty() || probe_id.len() > MAX_PROBE_ID_BYTES {
        return Err(McpError::new(
            McpErrorKind::InvalidConfiguration,
            "MCP probe id must be between 1 and 128 bytes",
        ));
    }
    if !probe_id
        .chars()
        .all(|value| value.is_ascii_alphanumeric() || value == '-' || value == '_')
    {
        return Err(McpError::new(
            McpErrorKind::InvalidConfiguration,
            "MCP probe id must use ASCII letters, digits, '-' or '_'",
        ));
    }
    Ok(())
}

/// One registered probe. Dropping it releases the registration, so success,
/// failure, cancellation and panic all reclaim the entry.
pub struct McpProbeLease {
    probe_id: String,
    abort: Arc<ManagedAbortToken>,
}

impl McpProbeLease {
    fn abort(&self) -> &Arc<ManagedAbortToken> {
        &self.abort
    }
}

impl fmt::Debug for McpProbeLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpProbeLease")
            .field("probe_id", &self.probe_id)
            .field("cancelled", &self.abort.is_abandoned())
            .finish()
    }
}

impl Drop for McpProbeLease {
    fn drop(&mut self) {
        let mut registry = lock_probe_registry();
        // Remove only this lease's own registration: a later probe that reused the
        // id owns its own entry.
        if registry
            .live
            .get(&self.probe_id)
            .is_some_and(|token| Arc::ptr_eq(token, &self.abort))
        {
            registry.live.remove(&self.probe_id);
        }
    }
}

/// Registers a probe. A cancel that arrived before the worker got here wins,
/// because the user asked for it before any connection existed.
pub fn begin_probe(probe_id: &str) -> Result<McpProbeLease, McpError> {
    validate_probe_id(probe_id)?;
    let mut registry = lock_probe_registry();
    if let Some(index) = registry
        .cancelled_before_start
        .iter()
        .position(|pending| pending == probe_id)
    {
        registry.cancelled_before_start.remove(index);
        return Err(probe_cancelled_error());
    }
    if registry.live.contains_key(probe_id) {
        return Err(McpError::new(
            McpErrorKind::InvalidConfiguration,
            "MCP probe id is already in flight",
        ));
    }
    let abort = Arc::new(ManagedAbortToken::default());
    registry.live.insert(probe_id.to_owned(), abort.clone());
    Ok(McpProbeLease {
        probe_id: probe_id.to_owned(),
        abort,
    })
}

/// Cancels one probe. Idempotent: cancelling an unknown, already finished or
/// already cancelled probe succeeds without touching anything else.
pub fn cancel_probe(probe_id: &str) -> Result<(), McpError> {
    validate_probe_id(probe_id)?;
    let token = {
        let mut registry = lock_probe_registry();
        match registry.live.get(probe_id) {
            Some(token) => Some(token.clone()),
            None => {
                if !registry
                    .cancelled_before_start
                    .iter()
                    .any(|pending| pending == probe_id)
                {
                    if registry.cancelled_before_start.len() >= MAX_PENDING_PROBE_CANCELS {
                        registry.cancelled_before_start.pop_front();
                    }
                    registry
                        .cancelled_before_start
                        .push_back(probe_id.to_owned());
                }
                None
            }
        }
    };
    // Terminating a stdio child blocks, so it must not happen under the lock.
    if let Some(token) = token {
        token.abandon();
    }
    Ok(())
}

/// Runs one settings probe that `probe_id` can cancel. Production timeouts and
/// schema limits keep probe results comparable with runs.
pub fn probe_server_for_settings(
    server: &RuntimeMcpServer,
    probe_id: &str,
) -> Result<McpProbeOutcome, (McpError, Vec<String>)> {
    let lease = begin_probe(probe_id).map_err(|error| (error.for_server(server), Vec::new()))?;
    McpClient::default().probe_server(server, Some(lease.abort()))
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct ManagedSessionKey {
    conversation_id: String,
    artifact_id: String,
    server_id: String,
    config_fingerprint: [u8; 32],
}

impl fmt::Debug for ManagedSessionKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagedSessionKey")
            .field("conversation_id", &self.conversation_id)
            .field("artifact_id", &self.artifact_id)
            .field("server_id", &self.server_id)
            .field("config_fingerprint", &"<redacted>")
            .finish()
    }
}

struct ManagedListResult {
    tools: Vec<McpRemoteTool>,
    session_info: McpSessionInfo,
}

enum ManagedSessionCommand {
    List {
        deadline: Instant,
        cancellation: crate::cancel::CancelSignal,
        operation_abort: Arc<ManagedAbortToken>,
        reply: SyncSender<Result<ManagedListResult, McpError>>,
    },
    Call {
        remote_name: String,
        arguments: Value,
        deadline: Instant,
        cancellation: crate::cancel::CancelSignal,
        operation_abort: Arc<ManagedAbortToken>,
        reply: SyncSender<Result<McpToolCallResult, McpError>>,
    },
    Shutdown,
}

struct ManagedLiveSession {
    connection: McpConnection,
    session_info: McpSessionInfo,
}

#[derive(Default)]
struct ManagedAbortToken {
    abandoned: AtomicBool,
    started: AtomicBool,
    stdio: Mutex<Option<Weak<ManagedStdioProcess>>>,
}

impl ManagedAbortToken {
    fn mark_started(&self) {
        self.started.store(true, Ordering::Release);
    }

    fn started(&self) -> bool {
        self.started.load(Ordering::Acquire)
    }

    fn is_abandoned(&self) -> bool {
        self.abandoned.load(Ordering::Acquire)
    }

    fn register_stdio(&self, process: &Arc<ManagedStdioProcess>) {
        let previous = {
            let mut current = self
                .stdio
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let previous = current.as_ref().and_then(Weak::upgrade);
            *current = Some(Arc::downgrade(process));
            previous
        };
        if let Some(previous) = previous {
            if !Arc::ptr_eq(&previous, process) {
                previous.terminate();
            }
        }
        if self.is_abandoned() {
            process.terminate();
        }
    }

    fn clear_stdio(&self, process: &Arc<ManagedStdioProcess>) {
        let mut current = self
            .stdio
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let should_clear = current
            .as_ref()
            .and_then(Weak::upgrade)
            .map_or(true, |registered| Arc::ptr_eq(&registered, process));
        if should_clear {
            *current = None;
        }
    }

    fn abandon(&self) {
        self.abandoned.store(true, Ordering::Release);
        let process = self
            .stdio
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .and_then(Weak::upgrade);
        if let Some(process) = process {
            process.terminate();
        }
    }
}

struct ManagedSessionWorker {
    key: ManagedSessionKey,
    sender: SyncSender<ManagedSessionCommand>,
    pending: Arc<AtomicUsize>,
    last_used: Arc<Mutex<Instant>>,
    shutdown_abort: Arc<ManagedAbortToken>,
    stopping: Arc<AtomicBool>,
    actor_done: Mutex<Receiver<()>>,
    shutdown_timeout: Duration,
    join: Mutex<Option<thread::JoinHandle<()>>>,
}

impl fmt::Debug for ManagedSessionWorker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagedSessionWorker")
            .field("key", &self.key)
            .field("pending", &self.pending.load(Ordering::Acquire))
            .finish()
    }
}

impl ManagedSessionWorker {
    fn spawn(
        key: ManagedSessionKey,
        server: RuntimeMcpServer,
        options: McpClientOptions,
    ) -> Result<Self, McpError> {
        let (sender, receiver) = mpsc::sync_channel(MANAGED_COMMAND_QUEUE);
        let pending = Arc::new(AtomicUsize::new(0));
        let actor_pending = pending.clone();
        let last_used = Arc::new(Mutex::new(Instant::now()));
        let actor_last_used = last_used.clone();
        let shutdown_abort = Arc::new(ManagedAbortToken::default());
        let actor_shutdown_abort = shutdown_abort.clone();
        let stopping = Arc::new(AtomicBool::new(false));
        let actor_stopping = stopping.clone();
        let (actor_done_sender, actor_done) = mpsc::sync_channel(1);
        let join = thread::Builder::new()
            .name("mework-mcp-session".into())
            .spawn(move || {
                managed_session_actor(
                    server,
                    options,
                    receiver,
                    actor_pending,
                    actor_last_used,
                    actor_shutdown_abort,
                    actor_stopping,
                );
                let _ = actor_done_sender.send(());
            })
            .map_err(|_| {
                McpError::new(
                    McpErrorKind::Transport,
                    "Could not start MCP session worker",
                )
            })?;
        Ok(Self {
            key,
            sender,
            pending,
            last_used,
            shutdown_abort,
            stopping,
            actor_done: Mutex::new(actor_done),
            shutdown_timeout: options.shutdown_timeout,
            join: Mutex::new(Some(join)),
        })
    }

    fn pending(&self) -> usize {
        self.pending.load(Ordering::Acquire)
    }

    fn last_used(&self) -> Instant {
        *self
            .last_used
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn touch(&self) {
        *self
            .last_used
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Instant::now();
    }

    fn list(
        &self,
        deadline: Instant,
        cancellation: crate::cancel::CancelSignal,
    ) -> Result<ManagedListResult, McpError> {
        // A rendezvous channel lets the actor distinguish a delivered reply
        // from a caller that already cancelled, timed out, or unwound. A
        // response from an abandoned request must never leave its session
        // available to a later model turn.
        let (reply, response) = mpsc::sync_channel(0);
        let operation_abort = Arc::new(ManagedAbortToken::default());
        if let Err(error) = self.enqueue(
            ManagedSessionCommand::List {
                deadline,
                cancellation: cancellation.clone(),
                operation_abort: operation_abort.clone(),
                reply,
            },
            deadline,
            &cancellation,
        ) {
            operation_abort.abandon();
            return Err(error);
        }
        let result = wait_for_managed_response(response, deadline, &cancellation);
        if result.is_err() {
            operation_abort.abandon();
        }
        result
    }

    fn call(
        &self,
        remote_name: String,
        arguments: Value,
        deadline: Instant,
        cancellation: crate::cancel::CancelSignal,
    ) -> Result<McpToolCallResult, McpError> {
        let (reply, response) = mpsc::sync_channel(0);
        let operation_abort = Arc::new(ManagedAbortToken::default());
        if let Err(error) = self.enqueue(
            ManagedSessionCommand::Call {
                remote_name,
                arguments,
                deadline,
                cancellation: cancellation.clone(),
                operation_abort: operation_abort.clone(),
                reply,
            },
            deadline,
            &cancellation,
        ) {
            operation_abort.abandon();
            return Err(error);
        }
        let result = wait_for_managed_response(response, deadline, &cancellation);
        if result.is_err() {
            operation_abort.abandon();
        }
        result
    }

    fn enqueue(
        &self,
        mut command: ManagedSessionCommand,
        deadline: Instant,
        cancellation: &crate::cancel::CancelSignal,
    ) -> Result<(), McpError> {
        self.pending.fetch_add(1, Ordering::AcqRel);
        self.touch();
        loop {
            if cancellation_requested(cancellation) {
                self.pending.fetch_sub(1, Ordering::AcqRel);
                return Err(cancelled_error());
            }
            if Instant::now() >= deadline {
                self.pending.fetch_sub(1, Ordering::AcqRel);
                return Err(McpError::new(
                    McpErrorKind::Timeout,
                    "Timed out waiting for the MCP session queue",
                ));
            }
            match self.sender.try_send(command) {
                Ok(()) => return Ok(()),
                Err(TrySendError::Full(returned)) => {
                    command = returned;
                    thread::sleep(MANAGED_WAIT_SLICE);
                }
                Err(TrySendError::Disconnected(_)) => {
                    self.pending.fetch_sub(1, Ordering::AcqRel);
                    return Err(McpError::new(
                        McpErrorKind::Transport,
                        "MCP session worker has closed",
                    ));
                }
            }
        }
    }
}

impl Drop for ManagedSessionWorker {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        let idle = self.pending.load(Ordering::Acquire) == 0;
        let _ = self.sender.try_send(ManagedSessionCommand::Shutdown);
        let done = self
            .actor_done
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut finished = idle
            && !matches!(
                done.recv_timeout(self.shutdown_timeout),
                Err(RecvTimeoutError::Timeout)
            );
        if !finished {
            self.shutdown_abort.abandon();
            let _ = self.sender.try_send(ManagedSessionCommand::Shutdown);
            finished = !matches!(
                done.recv_timeout(self.shutdown_timeout),
                Err(RecvTimeoutError::Timeout)
            );
        }
        let join = self
            .join
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if finished {
            let Some(join) = join else {
                return;
            };
            let _ = join.join();
        }
    }
}

struct McpSessionManagerInner {
    entries: Mutex<HashMap<ManagedSessionKey, Arc<ManagedSessionWorker>>>,
    options: McpClientOptions,
    max_sessions: usize,
    idle_ttl: Duration,
    _reaper_stop: SyncSender<()>,
}

impl fmt::Debug for McpSessionManagerInner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let count = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len();
        formatter
            .debug_struct("McpSessionManager")
            .field("session_count", &count)
            .field("max_sessions", &self.max_sessions)
            .field("idle_ttl", &self.idle_ttl)
            .finish()
    }
}

#[derive(Clone)]
pub struct McpSessionManager {
    inner: Arc<McpSessionManagerInner>,
}

impl fmt::Debug for McpSessionManager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner.fmt(formatter)
    }
}

impl Default for McpSessionManager {
    fn default() -> Self {
        Self::new(
            McpClientOptions::default(),
            MANAGED_SESSION_LIMIT,
            MANAGED_SESSION_TTL,
        )
        .expect("built-in MCP session manager limits are valid")
    }
}

impl McpSessionManager {
    pub fn new(
        options: McpClientOptions,
        max_sessions: usize,
        idle_ttl: Duration,
    ) -> Result<Self, McpError> {
        validate_options(options)?;
        if max_sessions == 0 || max_sessions > 256 {
            return Err(McpError::new(
                McpErrorKind::InvalidConfiguration,
                "MCP session limit must be between 1 and 256",
            ));
        }
        if idle_ttl.is_zero() || idle_ttl > Duration::from_secs(24 * 60 * 60) {
            return Err(McpError::new(
                McpErrorKind::InvalidConfiguration,
                "MCP session idle TTL must be greater than 0 and no more than 24 hours",
            ));
        }
        let (reaper_stop, reaper_receiver) = mpsc::sync_channel(1);
        let inner = Arc::new(McpSessionManagerInner {
            entries: Mutex::new(HashMap::new()),
            options,
            max_sessions,
            idle_ttl,
            _reaper_stop: reaper_stop,
        });
        if let Err(error) = spawn_managed_session_reaper(&inner, reaper_receiver) {
            // Session lookup also performs synchronous TTL eviction, so a
            // reaper-thread failure degrades resource reclamation timing
            // without making the whole desktop state impossible to create.
            eprintln!("MCP session 后台清理不可用：{error}");
        }
        Ok(Self { inner })
    }

    pub fn discover_for_conversation(
        &self,
        conversation_id: &str,
        servers: &[RuntimeMcpServer],
        cancellation: crate::cancel::CancelSignal,
    ) -> McpDiscoveryReport {
        let mut report = McpDiscoveryReport::default();
        let mut exposed_names = HashSet::new();
        if validate_managed_conversation_id(conversation_id).is_err() {
            let error = McpError::new(
                McpErrorKind::InvalidConfiguration,
                "MCP conversation id is invalid",
            );
            for server in servers.iter().take(MAX_SERVERS) {
                report
                    .failures
                    .push(McpDiscoveryFailure::from_error(server, &error));
            }
            return report;
        }
        let deadline = Instant::now()
            .checked_add(self.inner.options.discovery_timeout)
            .unwrap_or_else(Instant::now);
        for (index, server) in servers.iter().enumerate() {
            if index >= MAX_SERVERS {
                let error = McpError::new(
                    McpErrorKind::Bounds,
                    format!("At most {MAX_SERVERS} MCP servers can be connected per run"),
                );
                report
                    .failures
                    .push(McpDiscoveryFailure::from_error(server, &error));
                break;
            }
            if cancellation_requested(&cancellation) {
                let error = cancelled_error();
                for pending in &servers[index..servers.len().min(MAX_SERVERS)] {
                    report
                        .failures
                        .push(McpDiscoveryFailure::from_error(pending, &error));
                }
                break;
            }
            if Instant::now() >= deadline {
                report.deadline_reached = true;
                let error = McpError::new(
                    McpErrorKind::Timeout,
                    "MCP tool discovery exceeded the global time budget",
                );
                for pending in &servers[index..servers.len().min(MAX_SERVERS)] {
                    report
                        .failures
                        .push(McpDiscoveryFailure::from_error(pending, &error));
                }
                break;
            }
            let result = validate_runtime_server(server)
                .map_err(|error| error.for_server(server))
                .and_then(|_| self.worker(conversation_id, server))
                .and_then(|worker| worker.list(deadline, cancellation.clone()))
                .map(|listed| {
                    bindings_from_remote_tools(server, &listed.session_info, listed.tools)
                });
            let server_bindings = match result {
                Ok(bindings) => bindings,
                Err(error) => {
                    if error.kind == McpErrorKind::Timeout && Instant::now() >= deadline {
                        report.deadline_reached = true;
                    }
                    report
                        .failures
                        .push(McpDiscoveryFailure::from_error(server, &error));
                    continue;
                }
            };
            if let Err(error) = merge_discovered_bindings(
                &mut report,
                &mut exposed_names,
                server_bindings,
                self.inner.options.max_total_schema_bytes,
            ) {
                report
                    .failures
                    .push(McpDiscoveryFailure::from_error(server, &error));
            }
        }
        report
    }

    pub fn call_for_conversation(
        &self,
        conversation_id: &str,
        binding: &McpToolBinding,
        arguments: Value,
        cancellation: crate::cancel::CancelSignal,
    ) -> Result<McpToolCallResult, McpError> {
        validate_managed_conversation_id(conversation_id)
            .map_err(|error| error.for_server(&binding.server))?;
        validate_runtime_server(&binding.server)
            .map_err(|error| error.for_server(&binding.server))?;
        validate_remote_name(&binding.remote_name)
            .map_err(|error| error.for_server(&binding.server))?;
        let arguments =
            normalize_arguments(arguments).map_err(|error| error.for_server(&binding.server))?;
        if cancellation_requested(&cancellation) {
            return Err(cancelled_error().for_server(&binding.server));
        }
        let deadline = Instant::now()
            .checked_add(
                binding
                    .server
                    .effective_request_timeout(self.inner.options.request_timeout),
            )
            .ok_or_else(|| {
                McpError::new(McpErrorKind::Bounds, "MCP tool call time budget is invalid")
            })
            .map_err(|error| error.for_server(&binding.server))?;
        self.worker(conversation_id, &binding.server)
            .map_err(|error| error.for_server(&binding.server))?
            .call(
                binding.remote_name.clone(),
                arguments,
                deadline,
                cancellation,
            )
            .map_err(|error| error.for_server(&binding.server))
    }

    #[allow(dead_code)]
    pub fn evict_artifact(&self, artifact_id: &str) {
        let removed = {
            let mut entries = self
                .inner
                .entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let keys = entries
                .keys()
                .filter(|key| key.artifact_id == artifact_id)
                .cloned()
                .collect::<Vec<_>>();
            keys.into_iter()
                .filter_map(|key| entries.remove(&key))
                .collect::<Vec<_>>()
        };
        drop(removed);
    }

    pub fn evict_conversations<'a>(&self, conversation_ids: impl IntoIterator<Item = &'a str>) {
        let requested = conversation_ids
            .into_iter()
            .map(str::to_owned)
            .collect::<HashSet<_>>();
        if requested.is_empty() {
            return;
        }
        let removed = {
            let mut entries = self
                .inner
                .entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let keys = entries
                .keys()
                .filter(|key| requested.contains(&key.conversation_id))
                .cloned()
                .collect::<Vec<_>>();
            keys.into_iter()
                .filter_map(|key| entries.remove(&key))
                .collect::<Vec<_>>()
        };
        drop(removed);
    }

    pub fn clear(&self) {
        let removed = {
            let mut entries = self
                .inner
                .entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            entries
                .drain()
                .map(|(_, worker)| worker)
                .collect::<Vec<_>>()
        };
        drop(removed);
    }

    #[cfg(test)]
    fn session_count(&self) -> usize {
        self.inner
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }

    fn worker(
        &self,
        conversation_id: &str,
        server: &RuntimeMcpServer,
    ) -> Result<Arc<ManagedSessionWorker>, McpError> {
        let key = managed_session_key(conversation_id, server);
        let now = Instant::now();
        let mut removed = Vec::new();
        let worker = {
            let mut entries = self
                .inner
                .entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let expired = entries
                .iter()
                .filter(|(_, worker)| {
                    Arc::strong_count(worker) == 1
                        && worker.pending() == 0
                        && now.saturating_duration_since(worker.last_used()) >= self.inner.idle_ttl
                })
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>();
            for expired_key in expired {
                if let Some(worker) = entries.remove(&expired_key) {
                    removed.push(worker);
                }
            }
            if let Some(worker) = entries.get(&key) {
                worker.touch();
                worker.clone()
            } else {
                while entries.len() >= self.inner.max_sessions {
                    let candidate = entries
                        .iter()
                        .filter(|(_, worker)| {
                            Arc::strong_count(worker) == 1 && worker.pending() == 0
                        })
                        .min_by_key(|(_, worker)| worker.last_used())
                        .map(|(key, _)| key.clone());
                    let Some(candidate) = candidate else {
                        return Err(McpError::new(
                            McpErrorKind::Bounds,
                            "All MCP sessions are busy; cannot create another connection",
                        ));
                    };
                    if let Some(worker) = entries.remove(&candidate) {
                        removed.push(worker);
                    }
                }
                let worker = Arc::new(ManagedSessionWorker::spawn(
                    key.clone(),
                    server.clone(),
                    self.inner.options,
                )?);
                entries.insert(key, worker.clone());
                worker
            }
        };
        drop(removed);
        Ok(worker)
    }
}

fn spawn_managed_session_reaper(
    inner: &Arc<McpSessionManagerInner>,
    stop: Receiver<()>,
) -> Result<(), McpError> {
    let weak = Arc::downgrade(inner);
    let cadence = inner.idle_ttl.min(Duration::from_secs(60));
    thread::Builder::new()
        .name("mework-mcp-session-reaper".into())
        .spawn(move || loop {
            match stop.recv_timeout(cadence) {
                Err(RecvTimeoutError::Timeout) => {}
                Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
            }
            let Some(inner) = weak.upgrade() else {
                break;
            };
            let now = Instant::now();
            let removed = {
                let mut entries = inner
                    .entries
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let expired = entries
                    .iter()
                    .filter(|(_, worker)| {
                        Arc::strong_count(worker) == 1
                            && worker.pending() == 0
                            && now.saturating_duration_since(worker.last_used()) >= inner.idle_ttl
                    })
                    .map(|(key, _)| key.clone())
                    .collect::<Vec<_>>();
                expired
                    .into_iter()
                    .filter_map(|key| entries.remove(&key))
                    .collect::<Vec<_>>()
            };
            drop(inner);
            drop(removed);
        })
        .map(|_| ())
        .map_err(|_| {
            McpError::new(
                McpErrorKind::Transport,
                "Could not start MCP session reaper thread",
            )
        })
}

fn managed_session_actor(
    server: RuntimeMcpServer,
    options: McpClientOptions,
    receiver: Receiver<ManagedSessionCommand>,
    pending: Arc<AtomicUsize>,
    last_used: Arc<Mutex<Instant>>,
    shutdown_abort: Arc<ManagedAbortToken>,
    stopping: Arc<AtomicBool>,
) {
    let mut live: Option<ManagedLiveSession> = None;
    loop {
        if stopping.load(Ordering::Acquire) {
            break;
        }
        let command = match receiver.recv_timeout(MANAGED_IDLE_PUMP_SLICE) {
            Ok(command) => command,
            Err(RecvTimeoutError::Timeout) => {
                if stopping.load(Ordering::Acquire) {
                    break;
                }
                let idle_result = live
                    .as_mut()
                    .map(|session| session.connection.pump_idle(MANAGED_IDLE_FRAME_BUDGET));
                if idle_result.is_some_and(|result| result.is_err()) {
                    live.take();
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if stopping.load(Ordering::Acquire) {
            break;
        }
        match command {
            ManagedSessionCommand::Shutdown => break,
            ManagedSessionCommand::List {
                deadline,
                cancellation,
                operation_abort,
                reply,
            } => {
                let cancelled_before_start =
                    cancellation_requested(&cancellation) || operation_abort.is_abandoned();
                let mut result = if cancelled_before_start {
                    Err(cancelled_error())
                } else {
                    operation_abort.mark_started();
                    managed_list_tools(
                        &server,
                        options,
                        &mut live,
                        deadline,
                        &cancellation,
                        &shutdown_abort,
                        &operation_abort,
                    )
                };
                if let Some(session) = live.as_mut() {
                    session.connection.clear_operation_abort(&operation_abort);
                }
                if cancellation_requested(&cancellation) || operation_abort.is_abandoned() {
                    if operation_abort.started() {
                        live.take();
                    }
                    result = Err(cancelled_error());
                } else if result
                    .as_ref()
                    .err()
                    .map_or(false, managed_error_invalidates_session)
                {
                    live.take();
                }
                if reply.send(result).is_err() && operation_abort.started() {
                    live.take();
                }
                pending.fetch_sub(1, Ordering::AcqRel);
                *last_used
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Instant::now();
            }
            ManagedSessionCommand::Call {
                remote_name,
                arguments,
                deadline,
                cancellation,
                operation_abort,
                reply,
            } => {
                let cancelled_before_start =
                    cancellation_requested(&cancellation) || operation_abort.is_abandoned();
                let mut result = if cancelled_before_start {
                    Err(cancelled_error())
                } else {
                    operation_abort.mark_started();
                    managed_call_tool(
                        &server,
                        options,
                        &mut live,
                        &remote_name,
                        arguments,
                        deadline,
                        &cancellation,
                        &shutdown_abort,
                        &operation_abort,
                    )
                };
                if let Some(session) = live.as_mut() {
                    session.connection.clear_operation_abort(&operation_abort);
                }
                if cancellation_requested(&cancellation) || operation_abort.is_abandoned() {
                    if operation_abort.started() {
                        live.take();
                    }
                    result = Err(cancelled_error());
                } else if result
                    .as_ref()
                    .err()
                    .map_or(false, managed_error_invalidates_session)
                {
                    live.take();
                }
                if reply.send(result).is_err() && operation_abort.started() {
                    live.take();
                }
                pending.fetch_sub(1, Ordering::AcqRel);
                *last_used
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Instant::now();
            }
        }
    }
}

fn managed_list_tools(
    server: &RuntimeMcpServer,
    options: McpClientOptions,
    live: &mut Option<ManagedLiveSession>,
    deadline: Instant,
    cancellation: &crate::cancel::CancelSignal,
    shutdown_abort: &Arc<ManagedAbortToken>,
    operation_abort: &Arc<ManagedAbortToken>,
) -> Result<ManagedListResult, McpError> {
    if cancellation_requested(cancellation) || operation_abort.is_abandoned() {
        return Err(cancelled_error());
    }
    let session = ensure_managed_live_session(
        server,
        options,
        live,
        deadline,
        shutdown_abort,
        operation_abort,
    )?;
    if cancellation_requested(cancellation) || operation_abort.is_abandoned() {
        return Err(cancelled_error());
    }
    let tools = list_tools(
        &mut session.connection,
        options.request_timeout,
        Some(deadline),
    )?;
    Ok(ManagedListResult {
        tools,
        session_info: session.session_info.clone(),
    })
}

fn managed_call_tool(
    server: &RuntimeMcpServer,
    options: McpClientOptions,
    live: &mut Option<ManagedLiveSession>,
    remote_name: &str,
    arguments: Value,
    deadline: Instant,
    cancellation: &crate::cancel::CancelSignal,
    shutdown_abort: &Arc<ManagedAbortToken>,
    operation_abort: &Arc<ManagedAbortToken>,
) -> Result<McpToolCallResult, McpError> {
    if cancellation_requested(cancellation) || operation_abort.is_abandoned() {
        return Err(cancelled_error());
    }
    let session = ensure_managed_live_session(
        server,
        options,
        live,
        deadline,
        shutdown_abort,
        operation_abort,
    )?;
    if cancellation_requested(cancellation) || operation_abort.is_abandoned() {
        return Err(cancelled_error());
    }
    let timeout = operation_timeout(
        server.effective_request_timeout(options.request_timeout),
        Some(deadline),
    )?;
    let result = session.connection.request(
        "tools/call",
        json!({
            "name": remote_name,
            "arguments": arguments,
        }),
        timeout,
    )?;
    parse_call_result(result)
}

fn ensure_managed_live_session<'a>(
    server: &RuntimeMcpServer,
    options: McpClientOptions,
    live: &'a mut Option<ManagedLiveSession>,
    deadline: Instant,
    shutdown_abort: &Arc<ManagedAbortToken>,
    operation_abort: &Arc<ManagedAbortToken>,
) -> Result<&'a mut ManagedLiveSession, McpError> {
    if live.is_none() {
        let (connection, session_info) = initialize_connection(
            server,
            options,
            Some(deadline),
            Some(shutdown_abort.clone()),
            Some(operation_abort.clone()),
        )?;
        *live = Some(ManagedLiveSession {
            connection,
            session_info,
        });
    }
    let session = live.as_mut().ok_or_else(|| {
        McpError::new(
            McpErrorKind::Transport,
            "MCP session is unavailable after initialization",
        )
    })?;
    session.connection.bind_operation_abort(operation_abort);
    Ok(session)
}

fn managed_error_invalidates_session(error: &McpError) -> bool {
    matches!(
        error.kind,
        McpErrorKind::Bounds
            | McpErrorKind::Transport
            | McpErrorKind::Timeout
            | McpErrorKind::Cancelled
            | McpErrorKind::Protocol
    )
}

fn wait_for_managed_response<T>(
    response: Receiver<Result<T, McpError>>,
    deadline: Instant,
    cancellation: &crate::cancel::CancelSignal,
) -> Result<T, McpError> {
    loop {
        if cancellation_requested(cancellation) {
            return Err(cancelled_error());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(McpError::new(
                McpErrorKind::Timeout,
                "Timed out waiting for an MCP session response",
            ));
        }
        match response.recv_timeout(remaining.min(MANAGED_WAIT_SLICE)) {
            Ok(result) => return result,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(McpError::new(
                    McpErrorKind::Transport,
                    "MCP session response channel is closed",
                ))
            }
        }
    }
}

fn cancellation_requested(cancellation: &crate::cancel::CancelSignal) -> bool {
    cancellation.cancelled()
}

fn cancelled_error() -> McpError {
    McpError::new(McpErrorKind::Cancelled, "MCP operation was cancelled")
}

/// Whether the settings probe holding `abort` has been cancelled. A probe without
/// a registration (discovery, tests) can never be cancelled this way.
fn probe_cancelled(abort: Option<&Arc<ManagedAbortToken>>) -> bool {
    abort.is_some_and(|token| token.is_abandoned())
}

fn probe_cancelled_error() -> McpError {
    McpError::new(McpErrorKind::Cancelled, "MCP probe was cancelled")
}

/// Cancelling tears the connection down, so the transport reports a closed pipe or
/// a timeout. Report the cause the user chose rather than the symptom it produced.
fn probe_outcome_error(error: McpError, abort: Option<&Arc<ManagedAbortToken>>) -> McpError {
    if probe_cancelled(abort) {
        probe_cancelled_error()
    } else {
        error
    }
}

fn validate_managed_conversation_id(conversation_id: &str) -> Result<(), McpError> {
    if conversation_id.trim().is_empty()
        || conversation_id.len() > 512
        || conversation_id.chars().any(char::is_control)
    {
        return Err(McpError::new(
            McpErrorKind::InvalidConfiguration,
            "MCP conversation id is invalid",
        ));
    }
    Ok(())
}

fn managed_session_key(conversation_id: &str, server: &RuntimeMcpServer) -> ManagedSessionKey {
    ManagedSessionKey {
        conversation_id: conversation_id.to_owned(),
        artifact_id: server.artifact_id.clone(),
        server_id: server.server_id.clone(),
        config_fingerprint: runtime_server_fingerprint(server),
    }
}

fn runtime_server_fingerprint(server: &RuntimeMcpServer) -> [u8; 32] {
    let mut hasher = Sha256::new();
    fingerprint_field(&mut hasher, b"mework-mcp-session-v1");
    fingerprint_field(&mut hasher, server.artifact_id.as_bytes());
    fingerprint_field(&mut hasher, server.server_id.as_bytes());
    match &server.transport {
        RuntimeMcpTransport::Stdio {
            command,
            args,
            env,
            cwd,
        } => {
            fingerprint_field(&mut hasher, b"stdio");
            fingerprint_field(&mut hasher, command.as_bytes());
            for argument in args {
                fingerprint_field(&mut hasher, argument.as_bytes());
            }
            for (name, value) in env {
                fingerprint_field(&mut hasher, name.as_bytes());
                fingerprint_field(&mut hasher, value.as_bytes());
            }
            match cwd {
                Some(cwd) => fingerprint_field(&mut hasher, cwd.as_bytes()),
                None => fingerprint_field(&mut hasher, b"<no-cwd>"),
            }
        }
        RuntimeMcpTransport::Http { url, headers } => {
            fingerprint_field(&mut hasher, b"http");
            fingerprint_field(&mut hasher, url.as_bytes());
            for (name, value) in headers {
                fingerprint_field(&mut hasher, name.as_bytes());
                fingerprint_field(&mut hasher, value.as_bytes());
            }
        }
        RuntimeMcpTransport::ManagedHttp {
            command,
            args,
            env,
            cwd,
            url,
            headers,
        } => {
            fingerprint_field(&mut hasher, b"managed-http");
            fingerprint_field(&mut hasher, command.as_bytes());
            for argument in args {
                fingerprint_field(&mut hasher, argument.as_bytes());
            }
            for (name, value) in env {
                fingerprint_field(&mut hasher, name.as_bytes());
                fingerprint_field(&mut hasher, value.as_bytes());
            }
            match cwd {
                Some(cwd) => fingerprint_field(&mut hasher, cwd.as_bytes()),
                None => fingerprint_field(&mut hasher, b"<no-cwd>"),
            }
            fingerprint_field(&mut hasher, url.as_bytes());
            for (name, value) in headers {
                fingerprint_field(&mut hasher, name.as_bytes());
                fingerprint_field(&mut hasher, value.as_bytes());
            }
        }
    }
    hasher.finalize().into()
}

fn fingerprint_field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

fn bindings_from_remote_tools(
    server: &RuntimeMcpServer,
    session_info: &McpSessionInfo,
    tools: Vec<McpRemoteTool>,
) -> Vec<McpToolBinding> {
    tools
        .into_iter()
        .filter(|tool| !server.disabled_tools.iter().any(|name| name == &tool.name))
        .map(|tool| {
            let requires_user_interaction = tool_requires_user_interaction(tool.meta.as_ref());
            let user_requires_confirmation = server
                .confirm_every_call_tools
                .iter()
                .any(|name| name == &tool.name);
            let title = tool
                .title
                .as_deref()
                .map(|value| normalize_mcp_display_text(value, MAX_TOOL_TITLE_DISPLAY_CHARS))
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| {
                    normalize_mcp_display_text(&tool.name, MAX_TOOL_TITLE_DISPLAY_CHARS)
                });
            let description = tool
                .description
                .as_deref()
                .map(|value| normalize_mcp_display_text(value, MAX_TOOL_DESCRIPTION_DISPLAY_CHARS))
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| {
                    normalize_mcp_display_text(
                        &server.description,
                        MAX_TOOL_DESCRIPTION_DISPLAY_CHARS,
                    )
                });
            McpToolBinding {
                exposed_name: exposed_tool_name(server, &tool.name),
                remote_name: tool.name,
                title,
                description,
                input_schema: tool.input_schema,
                output_schema: tool.output_schema,
                annotations: tool.annotations,
                requires_user_interaction,
                user_requires_confirmation,
                negotiated_protocol_version: session_info.protocol_version.clone(),
                server: server.clone(),
            }
        })
        .collect()
}

fn merge_discovered_bindings(
    report: &mut McpDiscoveryReport,
    exposed_names: &mut HashSet<String>,
    server_bindings: Vec<McpToolBinding>,
    max_total_schema_bytes: usize,
) -> Result<(), McpError> {
    let server_schema_bytes = server_bindings.iter().try_fold(0usize, |total, binding| {
        total
            .checked_add(binding_schema_bytes(binding)?)
            .ok_or_else(|| McpError::new(McpErrorKind::Bounds, "MCP schema byte count overflowed"))
    })?;
    if server_schema_bytes > MAX_SERVER_SCHEMA_BYTES {
        return Err(McpError::new(
            McpErrorKind::Bounds,
            "A single MCP server's tool schema is too large",
        ));
    }
    let next_schema_bytes = report
        .schema_bytes
        .checked_add(server_schema_bytes)
        .ok_or_else(|| McpError::new(McpErrorKind::Bounds, "MCP schema byte count overflowed"))?;
    if next_schema_bytes > max_total_schema_bytes {
        return Err(McpError::new(
            McpErrorKind::Bounds,
            "MCP tool schemas exceed this run's cumulative budget",
        ));
    }
    if report.bindings.len() + server_bindings.len() > MAX_TOOLS {
        return Err(McpError::new(
            McpErrorKind::Bounds,
            format!("Total MCP tool count exceeds {MAX_TOOLS}"),
        ));
    }
    let mut candidate_names = HashSet::new();
    if server_bindings.iter().any(|binding| {
        exposed_names.contains(&binding.exposed_name)
            || !candidate_names.insert(binding.exposed_name.clone())
    }) {
        return Err(McpError::new(
            McpErrorKind::Protocol,
            "MCP tool names conflict",
        ));
    }
    for binding in &server_bindings {
        exposed_names.insert(binding.exposed_name.clone());
    }
    report.schema_bytes = next_schema_bytes;
    report.bindings.extend(server_bindings);
    Ok(())
}

fn tool_requires_user_interaction(meta: Option<&Value>) -> bool {
    let Some(value) = meta
        .and_then(Value::as_object)
        .and_then(|meta| meta.get("anthropic/requiresUserInteraction"))
    else {
        return false;
    };
    // The extension contract requires a JSON boolean. A malformed advertised
    // value is treated as mandatory rather than allowing Full Access to erase
    // a safety signal from an untrusted server.
    value.as_bool().unwrap_or(true)
}

#[allow(dead_code)]
pub fn discover_tools_strict(servers: &[RuntimeMcpServer]) -> Result<Vec<McpToolBinding>, String> {
    McpClient::default()
        .discover_tools(servers)
        .map_err(|error| error.to_string())
}

pub fn is_compatible_protocol_version(version: &str) -> bool {
    COMPATIBLE_PROTOCOL_VERSIONS.contains(&version)
}

pub fn exposed_tool_name(server: &RuntimeMcpServer, remote_name: &str) -> String {
    let server_slug = permission_server_name(server);
    let tool_slug = slug(remote_name, "tool", 18);
    let mut hasher = Sha256::new();
    hasher.update(server.artifact_id.as_bytes());
    hasher.update([0]);
    hasher.update(server.server_id.as_bytes());
    hasher.update([0]);
    hasher.update(remote_name.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    format!("mcp__{server_slug}__{tool_slug}__{}", &digest[..10])
}

fn permission_server_name(server: &RuntimeMcpServer) -> String {
    let display_slug = slug(&server.name, "server", 12);
    let mut hasher = Sha256::new();
    fingerprint_field(&mut hasher, b"mework-mcp-permission-server-v1");
    fingerprint_field(&mut hasher, server.artifact_id.as_bytes());
    fingerprint_field(&mut hasher, server.server_id.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    format!("{display_slug}_{}", &digest[..10])
}

fn validate_options(options: McpClientOptions) -> Result<(), McpError> {
    if options.request_timeout.is_zero() || options.request_timeout > MAX_REQUEST_TIMEOUT {
        return Err(McpError::new(
            McpErrorKind::InvalidConfiguration,
            "MCP request timeout must be greater than 0 and no more than 5 minutes",
        ));
    }
    if options.shutdown_timeout > MAX_SHUTDOWN_TIMEOUT {
        return Err(McpError::new(
            McpErrorKind::InvalidConfiguration,
            "MCP shutdown wait must not exceed 5 seconds",
        ));
    }
    if options.discovery_timeout.is_zero() || options.discovery_timeout > MAX_REQUEST_TIMEOUT {
        return Err(McpError::new(
            McpErrorKind::InvalidConfiguration,
            "MCP tool discovery time budget must be greater than 0 and no more than 5 minutes",
        ));
    }
    if options.max_total_schema_bytes == 0
        || options.max_total_schema_bytes > MAX_CONFIGURED_SCHEMA_BYTES
    {
        return Err(McpError::new(
            McpErrorKind::InvalidConfiguration,
            "MCP schema budget must be greater than 0 and no more than 8 MiB",
        ));
    }
    Ok(())
}

fn serialized_len(value: &Value) -> Result<usize, McpError> {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .map_err(|_| McpError::new(McpErrorKind::Protocol, "MCP schema could not be serialized"))
}

fn binding_schema_bytes(binding: &McpToolBinding) -> Result<usize, McpError> {
    serde_json::to_vec(&json!({
        "name": binding.exposed_name,
        "title": binding.title,
        "description": binding.description,
        "inputSchema": binding.input_schema,
        "outputSchema": binding.output_schema,
        "annotations": binding.annotations,
    }))
    .map(|bytes| bytes.len())
    .map_err(|_| {
        McpError::new(
            McpErrorKind::Protocol,
            "MCP tool definition could not be serialized",
        )
    })
}

fn validate_runtime_server(server: &RuntimeMcpServer) -> Result<(), McpError> {
    validate_identifier(&server.artifact_id, "artifact id", 512)?;
    validate_identifier(&server.server_id, "server id", 512)?;
    validate_identifier(&server.name, "server name", 512)?;
    if server.name.chars().any(is_untrusted_display_control) {
        return Err(McpError::new(
            McpErrorKind::InvalidConfiguration,
            "MCP server name contains unsafe display control characters",
        ));
    }
    match &server.transport {
        RuntimeMcpTransport::Stdio {
            command,
            args,
            env,
            cwd,
        } => {
            if command.trim().is_empty()
                || command.len() > MAX_COMMAND_BYTES
                || command.chars().any(char::is_control)
                || command.contains('\0')
            {
                return Err(McpError::new(
                    McpErrorKind::InvalidConfiguration,
                    "MCP stdio command is invalid",
                ));
            }
            if args.len() > MAX_ARGUMENT_COUNT {
                return Err(McpError::new(
                    McpErrorKind::Bounds,
                    format!("MCP stdio argument count exceeds {MAX_ARGUMENT_COUNT}"),
                ));
            }
            let mut argument_bytes = 0usize;
            for argument in args {
                if argument.contains('\0') {
                    return Err(McpError::new(
                        McpErrorKind::InvalidConfiguration,
                        "MCP stdio argument contains NUL",
                    ));
                }
                argument_bytes = argument_bytes.saturating_add(argument.len());
                if argument_bytes > MAX_OUTBOUND_BYTES {
                    return Err(McpError::new(
                        McpErrorKind::Bounds,
                        "MCP stdio arguments are too large",
                    ));
                }
            }
            if env.len() > MAX_ENVIRONMENT_ENTRIES {
                return Err(McpError::new(
                    McpErrorKind::Bounds,
                    format!("MCP environment variable count exceeds {MAX_ENVIRONMENT_ENTRIES}"),
                ));
            }
            for (name, value) in env {
                if name.is_empty()
                    || name.contains('=')
                    || name.contains('\0')
                    || value.contains('\0')
                    || name.chars().any(char::is_control)
                {
                    return Err(McpError::new(
                        McpErrorKind::InvalidConfiguration,
                        "MCP environment variable is invalid",
                    ));
                }
                if name.len().saturating_add(value.len()) > MAX_HEADER_VALUE_BYTES {
                    return Err(McpError::new(
                        McpErrorKind::Bounds,
                        "An MCP environment variable is too large",
                    ));
                }
            }
            validate_stdio_cwd(cwd.as_deref())?;
        }
        RuntimeMcpTransport::Http { url, headers } => {
            validate_http_endpoint(url)?;
            build_header_map(headers)?;
        }
        RuntimeMcpTransport::ManagedHttp {
            command,
            args,
            env,
            cwd,
            url,
            headers,
        } => {
            validate_managed_http_launch(&ManagedHttpLaunch {
                command: command.clone(),
                args: args.clone(),
                env: env.clone(),
                cwd: cwd.clone(),
                url: url.clone(),
            })?;
            build_header_map(headers)?;
        }
    }
    Ok(())
}

fn validate_identifier(value: &str, label: &str, max_bytes: usize) -> Result<(), McpError> {
    if value.trim().is_empty() || value.len() > max_bytes || value.chars().any(char::is_control) {
        return Err(McpError::new(
            McpErrorKind::InvalidConfiguration,
            format!("MCP {label} is invalid"),
        ));
    }
    Ok(())
}

fn is_untrusted_display_control(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{00ad}'
                | '\u{061c}'
                | '\u{200b}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2060}'..='\u{206f}'
                | '\u{feff}'
        )
}

/// MCP labels are untrusted UI text. Collapse all whitespace to ordinary spaces
/// and remove line/bidi controls before the text reaches provider schemas or a
/// native approval dialog.
fn normalize_mcp_display_text(value: &str, maximum_chars: usize) -> String {
    let mut output = String::new();
    let mut output_chars = 0usize;
    let mut pending_space = false;
    for character in value.chars() {
        if character.is_whitespace() || is_untrusted_display_control(character) {
            pending_space = !output.is_empty();
            continue;
        }
        if pending_space {
            // Reserve one character for the next visible code point so the
            // normalized value can never end in an injected/truncated spacer.
            if output_chars.saturating_add(1) >= maximum_chars {
                break;
            }
            output.push(' ');
            output_chars += 1;
            pending_space = false;
        }
        if output_chars >= maximum_chars {
            break;
        }
        output.push(character);
        output_chars += 1;
    }
    output
}

fn validate_stdio_cwd(value: Option<&str>) -> Result<(), McpError> {
    let Some(value) = value else {
        return Ok(());
    };
    if value.is_empty()
        || value.len() > MAX_COMMAND_BYTES
        || value.contains('\0')
        || value.chars().any(char::is_control)
    {
        return Err(McpError::new(
            McpErrorKind::InvalidConfiguration,
            "MCP stdio working directory is invalid",
        ));
    }
    let path = std::path::Path::new(value);
    if !path.is_absolute() {
        return Err(McpError::new(
            McpErrorKind::InvalidConfiguration,
            "MCP stdio working directory must be absolute",
        ));
    }
    let metadata = std::fs::metadata(path).map_err(|_| {
        McpError::new(
            McpErrorKind::InvalidConfiguration,
            "MCP stdio working directory does not exist or is inaccessible",
        )
    })?;
    if !metadata.is_dir() {
        return Err(McpError::new(
            McpErrorKind::InvalidConfiguration,
            "MCP stdio working directory is not a directory",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn configured_windows_environment_value<'a>(
    env: &'a std::collections::BTreeMap<String, String>,
    name: &str,
) -> Option<&'a str> {
    // `Command` applies this BTreeMap in iteration order and Windows variable
    // names are case-insensitive. Match the final configured spelling.
    env.iter()
        .rev()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

#[cfg(windows)]
fn windows_stdio_command_extensions(
    env: &std::collections::BTreeMap<String, String>,
) -> Vec<String> {
    const SAFE_DEFAULTS: &[&str] = &[".COM", ".EXE", ".BAT", ".CMD"];
    let configured = configured_windows_environment_value(env, "PATHEXT")
        .map(str::to_owned)
        .or_else(|| std::env::var_os("PATHEXT").map(|value| value.to_string_lossy().into_owned()))
        .unwrap_or_default();
    let mut extensions = Vec::new();
    for candidate in configured.split(';').chain(SAFE_DEFAULTS.iter().copied()) {
        let candidate = candidate.trim();
        if candidate.len() < 2
            || candidate.len() > 16
            || !candidate.starts_with('.')
            || !candidate[1..]
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric())
        {
            continue;
        }
        let normalized = candidate.to_ascii_uppercase();
        // `Command` can directly create PE/COM programs and Rust safely
        // handles BAT/CMD through its batch-file spawning path. Do not honor
        // arbitrary shell/association extensions such as PS1 or JS.
        if !SAFE_DEFAULTS.contains(&normalized.as_str()) {
            continue;
        }
        if !extensions.contains(&normalized) {
            extensions.push(normalized);
        }
    }
    extensions
}

#[cfg(windows)]
fn canonical_windows_command_file(path: &std::path::Path) -> Option<std::path::PathBuf> {
    if !std::fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let canonical = std::fs::canonicalize(path).ok()?;
    std::fs::metadata(&canonical)
        .ok()
        .filter(|metadata| metadata.is_file())
        .map(|_| canonical)
}

#[cfg(windows)]
fn resolve_windows_command_candidate(
    base: &std::path::Path,
    extensions: &[String],
) -> Option<std::path::PathBuf> {
    if base.extension().is_some() {
        return canonical_windows_command_file(base);
    }
    for extension in extensions {
        let mut candidate = base.to_path_buf();
        candidate.set_extension(extension.trim_start_matches('.'));
        if let Some(candidate) = canonical_windows_command_file(&candidate) {
            return Some(candidate);
        }
    }
    // Preserve support for an explicitly configured extensionless PE binary,
    // but only after PATHEXT candidates (for example npx.cmd) were considered.
    canonical_windows_command_file(base)
}

#[cfg(windows)]
fn resolve_windows_stdio_command(
    command: &str,
    env: &std::collections::BTreeMap<String, String>,
    cwd: Option<&str>,
) -> Result<std::path::PathBuf, McpError> {
    use std::path::Component;

    let extensions = windows_stdio_command_extensions(env);
    let path = std::path::Path::new(command);
    let explicit = path.components().any(|component| {
        matches!(
            component,
            Component::Prefix(_) | Component::RootDir | Component::CurDir | Component::ParentDir
        )
    }) || command.contains('\\')
        || command.contains('/');

    if explicit {
        if path.has_root() && !path.is_absolute()
            || matches!(path.components().next(), Some(Component::Prefix(_))) && !path.is_absolute()
        {
            return Err(McpError::new(
                McpErrorKind::InvalidConfiguration,
                "MCP stdio command does not accept ambiguous drive-relative paths",
            ));
        }
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            let base = match cwd {
                Some(cwd) => std::path::PathBuf::from(cwd),
                None => std::env::current_dir().map_err(|_| {
                    McpError::new(
                        McpErrorKind::Transport,
                        "Could not resolve the current directory for the MCP stdio command",
                    )
                })?,
            };
            base.join(path)
        };
        return resolve_windows_command_candidate(&candidate, &extensions).ok_or_else(|| {
            McpError::new(
                McpErrorKind::Transport,
                "Could not resolve the explicit path for the MCP stdio command",
            )
        });
    }

    let configured_path = configured_windows_environment_value(env, "PATH")
        .map(std::ffi::OsString::from)
        .or_else(|| std::env::var_os("PATH"))
        .unwrap_or_default();
    for directory in std::env::split_paths(&configured_path) {
        // Never let a plugin-supplied relative/empty PATH component turn the
        // process cwd into an implicit executable search directory.
        if !directory.is_absolute() {
            continue;
        }
        if let Some(candidate) =
            resolve_windows_command_candidate(&directory.join(command), &extensions)
        {
            return Ok(candidate);
        }
    }
    Err(McpError::new(
        McpErrorKind::Transport,
        "Could not resolve the MCP stdio command on the safe PATH",
    ))
}

fn validate_remote_name(value: &str) -> Result<(), McpError> {
    validate_identifier(value, "tool name", MAX_REMOTE_NAME_BYTES)
}

fn normalize_arguments(arguments: Value) -> Result<Value, McpError> {
    let arguments = if arguments.is_null() {
        Value::Object(Map::new())
    } else if arguments.is_object() {
        arguments
    } else {
        return Err(McpError::new(
            McpErrorKind::InvalidConfiguration,
            "MCP tool arguments must be a JSON object",
        ));
    };
    let bytes = serde_json::to_vec(&arguments).map_err(|_| {
        McpError::new(
            McpErrorKind::InvalidConfiguration,
            "MCP tool arguments could not be serialized",
        )
    })?;
    if bytes.len() > MAX_ARGUMENT_BYTES {
        return Err(McpError::new(
            McpErrorKind::Bounds,
            "MCP tool arguments exceed 2 MiB",
        ));
    }
    Ok(arguments)
}

fn initialize_connection(
    server: &RuntimeMcpServer,
    options: McpClientOptions,
    deadline: Option<Instant>,
    session_abort: Option<Arc<ManagedAbortToken>>,
    operation_abort: Option<Arc<ManagedAbortToken>>,
) -> Result<(McpConnection, McpSessionInfo), McpError> {
    operation_timeout(options.request_timeout, deadline)?;
    let transport = match &server.transport {
        RuntimeMcpTransport::Stdio {
            command,
            args,
            env,
            cwd,
        } => ConnectionTransport::Stdio(StdioSession::spawn(
            command,
            args,
            env,
            cwd.as_deref(),
            options.request_timeout,
            options.shutdown_timeout,
            session_abort.clone(),
            operation_abort.clone(),
        )?),
        RuntimeMcpTransport::Http { url, headers } => ConnectionTransport::Http(HttpSession::new(
            url,
            headers,
            options.request_timeout,
            options.shutdown_timeout,
        )?),
        RuntimeMcpTransport::ManagedHttp {
            command,
            args,
            env,
            cwd,
            url,
            headers,
        } => {
            let launch_timeout = operation_timeout(options.request_timeout, deadline)?;
            let launch_deadline = Instant::now().checked_add(launch_timeout).ok_or_else(|| {
                McpError::new(
                    McpErrorKind::Bounds,
                    "Managed MCP launch timeout is invalid",
                )
            })?;
            let sidecar = acquire_managed_http_sidecar(
                ManagedHttpLaunch {
                    command: command.clone(),
                    args: args.clone(),
                    env: env.clone(),
                    cwd: cwd.clone(),
                    url: url.clone(),
                },
                launch_deadline,
                operation_abort.as_deref().map(|abort| &abort.abandoned),
                session_abort.as_deref().map(|abort| &abort.abandoned),
            )?;
            let http = HttpSession::new(
                sidecar.url(),
                headers,
                options.request_timeout,
                options.shutdown_timeout,
            )?;
            ConnectionTransport::ManagedHttp(ManagedHttpSession {
                http,
                _sidecar: sidecar,
            })
        }
    };
    let mut transport = transport;
    let http = match &mut transport {
        ConnectionTransport::Http(session) => Some(session),
        ConnectionTransport::ManagedHttp(session) => Some(&mut session.http),
        ConnectionTransport::Stdio(_) => None,
    };
    if let Some(http) = http {
        http.abort = HttpAbort { session: session_abort, operation: operation_abort };
    }
    let mut connection = McpConnection {
        transport,
        next_id: 1,
    };
    let initialize_timeout = operation_timeout(options.request_timeout, deadline)?;
    let result = connection.request(
        "initialize",
        json!({
            "protocolVersion": LATEST_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {
                "name": CLIENT_NAME,
                "version": env!("CARGO_PKG_VERSION"),
            },
        }),
        initialize_timeout,
    )?;
    let object = result.as_object().ok_or_else(|| {
        McpError::new(
            McpErrorKind::Protocol,
            "initialize result must be a JSON object",
        )
    })?;
    let protocol_version = object
        .get("protocolVersion")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            McpError::new(
                McpErrorKind::Protocol,
                "initialize result is missing protocolVersion",
            )
        })?;
    if !is_compatible_protocol_version(protocol_version) {
        return Err(McpError::new(
            McpErrorKind::Protocol,
            format!(
                "Server negotiated an incompatible MCP protocol version: {}",
                sanitize_error_text(protocol_version, 64)
            ),
        ));
    }
    if matches!(
        &server.transport,
        RuntimeMcpTransport::Http { .. } | RuntimeMcpTransport::ManagedHttp { .. }
    ) && protocol_version == "2024-11-05"
    {
        return Err(McpError::new(
            McpErrorKind::Protocol,
            "MCP HTTP server negotiated 2024-11-05, which only supports legacy HTTP+SSE; the current transport requires Streamable HTTP",
        ));
    }
    let capabilities = object
        .get("capabilities")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            McpError::new(
                McpErrorKind::Protocol,
                "initialize result is missing the capabilities object",
            )
        })?;
    if !capabilities.get("tools").is_some_and(Value::is_object) {
        return Err(McpError::new(
            McpErrorKind::Protocol,
            "MCP server did not declare the tools capability",
        ));
    }
    let server_info = object
        .get("serverInfo")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            McpError::new(
                McpErrorKind::Protocol,
                "initialize result is missing the serverInfo object",
            )
        })?;
    let server_name = server_info
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| McpError::new(McpErrorKind::Protocol, "serverInfo is missing name"))
        .and_then(|value| sanitize_metadata(value, 512))?;
    let server_version = server_info
        .get("version")
        .and_then(Value::as_str)
        .ok_or_else(|| McpError::new(McpErrorKind::Protocol, "serverInfo is missing version"))
        .and_then(|value| sanitize_metadata(value, 512))?;
    connection.set_protocol_version(protocol_version)?;
    let notification_timeout = operation_timeout(options.request_timeout, deadline)?;
    connection.notify(
        "notifications/initialized",
        Value::Object(Map::new()),
        notification_timeout,
    )?;
    Ok((
        connection,
        McpSessionInfo {
            protocol_version: protocol_version.to_owned(),
            server_name: Some(server_name),
            server_version: Some(server_version),
            supports_prompts: capabilities.get("prompts").is_some_and(Value::is_object),
            supports_resources: capabilities.get("resources").is_some_and(Value::is_object),
        },
    ))
}

fn sanitize_metadata(value: &str, max_bytes: usize) -> Result<String, McpError> {
    if value.len() > max_bytes || value.chars().any(char::is_control) {
        return Err(McpError::new(
            McpErrorKind::Protocol,
            "MCP server metadata is invalid",
        ));
    }
    Ok(value.to_owned())
}

/// Fetches one `{method}/list` page. Cursors must be strings, must not cycle,
/// and are bounded by the page limit.
fn list_page(
    connection: &mut McpConnection,
    method: &str,
    field: &str,
    request_timeout: Duration,
    deadline: Option<Instant>,
    limit: usize,
) -> Result<Vec<Value>, McpError> {
    let mut items: Vec<Value> = Vec::new();
    let mut seen_cursors = HashSet::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_TOOL_PAGES {
        let params = match &cursor {
            Some(cursor) => json!({ "cursor": cursor }),
            None => Value::Object(Map::new()),
        };
        let timeout = operation_timeout(request_timeout, deadline)?;
        let result = connection.request(method, params, timeout)?;
        let object = result.as_object().ok_or_else(|| {
            McpError::new(
                McpErrorKind::Protocol,
                format!("{method} result must be a JSON object"),
            )
        })?;
        let page = object.get(field).and_then(Value::as_array).ok_or_else(|| {
            McpError::new(
                McpErrorKind::Protocol,
                format!("{method} result is missing the {field} array"),
            )
        })?;
        if items.len() + page.len() > limit {
            return Err(McpError::new(
                McpErrorKind::Bounds,
                format!("{method} returned more than {limit} entries"),
            ));
        }
        items.extend(page.iter().cloned());
        let next = object.get("nextCursor").and_then(Value::as_str);
        let Some(next) = next else { return Ok(items) };
        if next.len() > MAX_CURSOR_BYTES || next.is_empty() {
            return Err(McpError::new(
                McpErrorKind::Protocol,
                format!("{method} returned an invalid pagination cursor"),
            ));
        }
        if !seen_cursors.insert(next.to_owned()) {
            return Err(McpError::new(
                McpErrorKind::Protocol,
                format!("{method} pagination cursor forms a cycle"),
            ));
        }
        cursor = Some(next.to_owned());
    }
    Err(McpError::new(
        McpErrorKind::Bounds,
        format!("{method} pagination exceeds {MAX_TOOL_PAGES} pages"),
    ))
}

/// Converts a remote string field into one displayable UI line.
fn display_field(value: Option<&Value>, limit: usize) -> String {
    value
        .and_then(Value::as_str)
        .map(|text| normalize_mcp_display_text(text, limit))
        .unwrap_or_default()
}

fn list_prompts(
    connection: &mut McpConnection,
    request_timeout: Duration,
    deadline: Option<Instant>,
) -> Result<Vec<McpPromptSummary>, McpError> {
    let items = list_page(
        connection,
        "prompts/list",
        "prompts",
        request_timeout,
        deadline,
        MAX_PROMPTS,
    )?;
    Ok(items
        .iter()
        .filter_map(|item| {
            let object = item.as_object()?;
            let name = display_field(object.get("name"), MAX_TOOL_TITLE_DISPLAY_CHARS);
            if name.is_empty() {
                return None;
            }
            let title = display_field(object.get("title"), MAX_TOOL_TITLE_DISPLAY_CHARS);
            Some(McpPromptSummary {
                title: if title.is_empty() {
                    name.clone()
                } else {
                    title
                },
                name,
                description: display_field(
                    object.get("description"),
                    MAX_TOOL_DESCRIPTION_DISPLAY_CHARS,
                ),
                arguments: object
                    .get("arguments")
                    .and_then(Value::as_array)
                    .map(|arguments| {
                        arguments
                            .iter()
                            .filter_map(|argument| {
                                let argument = argument.as_object()?;
                                let name = display_field(
                                    argument.get("name"),
                                    MAX_TOOL_TITLE_DISPLAY_CHARS,
                                );
                                if name.is_empty() {
                                    return None;
                                }
                                Some(McpPromptArgumentSummary {
                                    name,
                                    description: display_field(
                                        argument.get("description"),
                                        MAX_TOOL_DESCRIPTION_DISPLAY_CHARS,
                                    ),
                                    required: argument
                                        .get("required")
                                        .and_then(Value::as_bool)
                                        .unwrap_or(false),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            })
        })
        .collect())
}

fn list_resources(
    connection: &mut McpConnection,
    request_timeout: Duration,
    deadline: Option<Instant>,
) -> Result<Vec<McpResourceSummary>, McpError> {
    let items = list_page(
        connection,
        "resources/list",
        "resources",
        request_timeout,
        deadline,
        MAX_RESOURCES,
    )?;
    Ok(items
        .iter()
        .filter_map(|item| {
            let object = item.as_object()?;
            let uri = display_field(object.get("uri"), MAX_REMOTE_NAME_BYTES);
            if uri.is_empty() {
                return None;
            }
            let name = display_field(object.get("name"), MAX_TOOL_TITLE_DISPLAY_CHARS);
            let title = display_field(object.get("title"), MAX_TOOL_TITLE_DISPLAY_CHARS);
            Some(McpResourceSummary {
                name: if name.is_empty() { uri.clone() } else { name },
                uri,
                title,
                description: display_field(
                    object.get("description"),
                    MAX_TOOL_DESCRIPTION_DISPLAY_CHARS,
                ),
                mime_type: display_field(object.get("mimeType"), 160),
                size: object.get("size").and_then(Value::as_u64).unwrap_or(0),
            })
        })
        .collect())
}

fn list_tools(
    connection: &mut McpConnection,
    request_timeout: Duration,
    deadline: Option<Instant>,
) -> Result<Vec<McpRemoteTool>, McpError> {
    let mut tools = Vec::new();
    let mut tool_names = HashSet::new();
    let mut seen_cursors = HashSet::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_TOOL_PAGES {
        let params = match &cursor {
            Some(cursor) => json!({ "cursor": cursor }),
            None => Value::Object(Map::new()),
        };
        let timeout = operation_timeout(request_timeout, deadline)?;
        let result = connection.request("tools/list", params, timeout)?;
        let object = result.as_object().ok_or_else(|| {
            McpError::new(
                McpErrorKind::Protocol,
                "tools/list result must be a JSON object",
            )
        })?;
        let page = object
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                McpError::new(
                    McpErrorKind::Protocol,
                    "tools/list result is missing the tools array",
                )
            })?;
        if tools.len() + page.len() > MAX_TOOLS {
            return Err(McpError::new(
                McpErrorKind::Bounds,
                format!("A single MCP server returned more than {MAX_TOOLS} tools"),
            ));
        }
        for value in page {
            let tool: McpRemoteTool = serde_json::from_value(value.clone()).map_err(|_| {
                McpError::new(
                    McpErrorKind::Protocol,
                    "tools/list returned an invalid tool description",
                )
            })?;
            validate_remote_tool(&tool)?;
            if !tool_names.insert(tool.name.clone()) {
                return Err(McpError::new(
                    McpErrorKind::Protocol,
                    format!(
                        "tools/list returned a duplicate tool: {}",
                        sanitize_error_text(&tool.name, 160)
                    ),
                ));
            }
            tools.push(tool);
        }
        let next_cursor = object
            .get("nextCursor")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let Some(next_cursor) = next_cursor else {
            return Ok(tools);
        };
        if next_cursor.len() > MAX_CURSOR_BYTES || next_cursor.chars().any(char::is_control) {
            return Err(McpError::new(
                McpErrorKind::Protocol,
                "tools/list returned an invalid pagination cursor",
            ));
        }
        if !seen_cursors.insert(next_cursor.clone()) {
            return Err(McpError::new(
                McpErrorKind::Protocol,
                "tools/list pagination cursor forms a cycle",
            ));
        }
        cursor = Some(next_cursor);
    }
    Err(McpError::new(
        McpErrorKind::Bounds,
        format!("tools/list pagination exceeds {MAX_TOOL_PAGES} pages"),
    ))
}

fn operation_timeout(
    request_timeout: Duration,
    deadline: Option<Instant>,
) -> Result<Duration, McpError> {
    let Some(deadline) = deadline else {
        return Ok(request_timeout);
    };
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(McpError::new(
            McpErrorKind::Timeout,
            "MCP tool discovery exceeded the global time budget",
        ));
    }
    Ok(request_timeout.min(remaining))
}

fn validate_remote_tool(tool: &McpRemoteTool) -> Result<(), McpError> {
    validate_remote_name(&tool.name).map_err(|_| {
        McpError::new(
            McpErrorKind::Protocol,
            "tools/list returned an invalid tool name",
        )
    })?;
    if !tool.input_schema.is_object() {
        return Err(McpError::new(
            McpErrorKind::Protocol,
            format!(
                "Tool {} inputSchema is not a JSON object",
                sanitize_error_text(&tool.name, 160)
            ),
        ));
    }
    if tool
        .output_schema
        .as_ref()
        .map_or(false, |schema| !schema.is_object())
    {
        return Err(McpError::new(
            McpErrorKind::Protocol,
            format!(
                "Tool {} outputSchema is not a JSON object",
                sanitize_error_text(&tool.name, 160)
            ),
        ));
    }
    for value in [&tool.title, &tool.description] {
        if value.as_ref().map_or(false, |value| {
            value.len() > MAX_FRAME_BYTES || value.contains('\0')
        }) {
            return Err(McpError::new(
                McpErrorKind::Protocol,
                "MCP tool metadata is invalid",
            ));
        }
    }
    Ok(())
}

fn parse_call_result(result: Value) -> Result<McpToolCallResult, McpError> {
    let object = result.as_object().ok_or_else(|| {
        McpError::new(
            McpErrorKind::Protocol,
            "tools/call result must be a JSON object",
        )
    })?;
    let content = object
        .get("content")
        .map(|value| {
            value.as_array().cloned().ok_or_else(|| {
                McpError::new(
                    McpErrorKind::Protocol,
                    "tools/call content must be an array",
                )
            })
        })
        .transpose()?
        .unwrap_or_default();
    if content.len() > MAX_CONTENT_ITEMS {
        return Err(McpError::new(
            McpErrorKind::Bounds,
            format!("tools/call content has more than {MAX_CONTENT_ITEMS} items"),
        ));
    }
    if content.iter().any(|value| !value.is_object()) {
        return Err(McpError::new(
            McpErrorKind::Protocol,
            "tools/call content items must be JSON objects",
        ));
    }
    let structured_content = object.get("structuredContent").cloned();
    if structured_content
        .as_ref()
        .map_or(false, |value| !value.is_object())
    {
        return Err(McpError::new(
            McpErrorKind::Protocol,
            "tools/call structuredContent must be a JSON object",
        ));
    }
    let is_error = object
        .get("isError")
        .map(|value| {
            value.as_bool().ok_or_else(|| {
                McpError::new(
                    McpErrorKind::Protocol,
                    "tools/call isError must be a boolean",
                )
            })
        })
        .transpose()?
        .unwrap_or(false);
    Ok(McpToolCallResult {
        content,
        structured_content,
        is_error,
        meta: object.get("_meta").cloned(),
    })
}

fn empty_object_schema() -> Value {
    json!({ "type": "object" })
}

struct McpConnection {
    transport: ConnectionTransport,
    next_id: u64,
}

impl McpConnection {
    fn request(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, McpError> {
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).ok_or_else(|| {
            McpError::new(McpErrorKind::Bounds, "MCP JSON-RPC request ID is exhausted")
        })?;
        let payload = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.transport
            .exchange(payload, Some(Value::from(id)), timeout)
    }

    fn notify(&mut self, method: &str, params: Value, timeout: Duration) -> Result<(), McpError> {
        let payload = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.transport.exchange(payload, None, timeout)?;
        Ok(())
    }

    fn set_protocol_version(&mut self, version: &str) -> Result<(), McpError> {
        self.transport.set_protocol_version(version)
    }

    fn pump_idle(&mut self, frame_budget: usize) -> Result<(), McpError> {
        self.transport.pump_idle(frame_budget)
    }

    fn bind_operation_abort(&mut self, abort: &Arc<ManagedAbortToken>) {
        self.transport.bind_operation_abort(abort);
    }

    fn clear_operation_abort(&mut self, abort: &Arc<ManagedAbortToken>) {
        self.transport.clear_operation_abort(abort);
    }

    /// Stderr lines produced during this connection. HTTP transports have none.
    fn diagnostics(&self) -> Vec<String> {
        self.transport.diagnostics()
    }
}

enum ConnectionTransport {
    Stdio(StdioSession),
    Http(HttpSession),
    ManagedHttp(ManagedHttpSession),
}

struct ManagedHttpSession {
    // Declare HTTP first so its Drop sends the MCP session DELETE while the
    // sidecar is still alive; the Arc then releases the pooled process.
    http: HttpSession,
    _sidecar: Arc<ManagedHttpSidecar>,
}

impl ConnectionTransport {
    fn exchange(
        &mut self,
        payload: Value,
        expected_id: Option<Value>,
        timeout: Duration,
    ) -> Result<Value, McpError> {
        match self {
            Self::Stdio(session) => session.exchange(payload, expected_id, timeout),
            Self::Http(session) => session.exchange(payload, expected_id, timeout),
            Self::ManagedHttp(session) => session.http.exchange(payload, expected_id, timeout),
        }
    }

    fn set_protocol_version(&mut self, version: &str) -> Result<(), McpError> {
        match self {
            Self::Http(session) => session.set_protocol_version(version)?,
            Self::ManagedHttp(session) => session.http.set_protocol_version(version)?,
            Self::Stdio(_) => {}
        }
        Ok(())
    }

    fn pump_idle(&mut self, frame_budget: usize) -> Result<(), McpError> {
        match self {
            Self::Stdio(session) => session.pump_idle(frame_budget),
            // Streamable HTTP has no client-owned long-lived GET stream.
            Self::Http(_) | Self::ManagedHttp(_) => Ok(()),
        }
    }

    fn bind_operation_abort(&mut self, abort: &Arc<ManagedAbortToken>) {
        match self {
            Self::Stdio(session) => abort.register_stdio(&session.process),
            Self::Http(session) => session.abort.operation = Some(abort.clone()),
            Self::ManagedHttp(session) => session.http.abort.operation = Some(abort.clone()),
        }
    }

    fn clear_operation_abort(&mut self, abort: &Arc<ManagedAbortToken>) {
        match self {
            Self::Stdio(session) => abort.clear_stdio(&session.process),
            Self::Http(session) => session.abort.operation = None,
            Self::ManagedHttp(session) => session.http.abort.operation = None,
        }
    }

    fn diagnostics(&self) -> Vec<String> {
        match self {
            Self::Stdio(session) => session.diagnostics.snapshot(),
            Self::Http(_) | Self::ManagedHttp(_) => Vec::new(),
        }
    }
}

#[cfg(windows)]
struct WindowsProcessJob {
    handle: usize,
}

#[cfg(windows)]
fn configure_windows_managed_child(command: &mut Command) {
    use std::os::windows::process::CommandExt;

    const CREATE_SUSPENDED: u32 = 0x0000_0004;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW);
}

#[cfg(windows)]
fn resume_windows_managed_child(child: &Child) -> Result<(), McpError> {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtResumeProcess(process_handle: *mut c_void) -> i32;
    }

    // SAFETY: Child owns a live process handle. The process was created with
    // CREATE_SUSPENDED and has already been assigned to its kill-on-close Job.
    let status = unsafe { NtResumeProcess(child.as_raw_handle()) };
    if status < 0 {
        return Err(McpError::new(
            McpErrorKind::Transport,
            "Could not resume the managed MCP Windows process",
        ));
    }
    Ok(())
}

#[cfg(windows)]
impl WindowsProcessJob {
    fn new() -> Result<Self, McpError> {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };

        // SAFETY: null security/name pointers request an anonymous job. The
        // returned owned handle is closed in Drop on every success path.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(McpError::new(
                McpErrorKind::Transport,
                "Could not create MCP Windows Job Object",
            ));
        }
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `limits` has the exact layout and byte length requested by
        // JobObjectExtendedLimitInformation; `handle` is live and owned here.
        let configured = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if configured == 0 {
            // SAFETY: the handle was created above and has not been transferred.
            unsafe {
                CloseHandle(handle);
            }
            return Err(McpError::new(
                McpErrorKind::Transport,
                "Could not configure MCP Windows Job Object",
            ));
        }
        Ok(Self {
            handle: handle as usize,
        })
    }

    fn assign(&self, child: &Child) -> Result<(), McpError> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;

        let job = self.handle as HANDLE;
        let process = child.as_raw_handle() as HANDLE;
        // SAFETY: both handles remain live for this call.
        if unsafe { AssignProcessToJobObject(job, process) } == 0 {
            return Err(McpError::new(
                McpErrorKind::Transport,
                "Could not assign MCP process to Windows Job Object",
            ));
        }
        Ok(())
    }

    fn terminate(&self) {
        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;

        // SAFETY: self owns a live job handle until Drop closes it.
        let _ = unsafe { TerminateJobObject(self.handle as HANDLE, 1) };
    }
}

#[cfg(windows)]
impl Drop for WindowsProcessJob {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};

        self.terminate();
        // SAFETY: this is the unique owned handle and Drop runs once.
        unsafe {
            CloseHandle(self.handle as HANDLE);
        }
    }
}

struct ManagedStdioProcess {
    child: Mutex<Child>,
    #[cfg(windows)]
    process_job: WindowsProcessJob,
}

impl ManagedStdioProcess {
    fn terminate(&self) {
        #[cfg(windows)]
        self.process_job.terminate();
        let mut child = self
            .child
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = child.kill();
    }
}

struct StdioWriteRequest {
    bytes: Vec<u8>,
    reply: SyncSender<Result<(), McpError>>,
}

struct StdioSession {
    process: Arc<ManagedStdioProcess>,
    writes: Option<SyncSender<StdioWriteRequest>>,
    responses: Receiver<Result<Value, McpError>>,
    request_timeout: Duration,
    shutdown_timeout: Duration,
    session_abort: Option<Arc<ManagedAbortToken>>,
    diagnostics: Arc<StderrLog>,
}

impl StdioSession {
    fn spawn(
        command: &str,
        args: &[String],
        env: &std::collections::BTreeMap<String, String>,
        cwd: Option<&str>,
        request_timeout: Duration,
        shutdown_timeout: Duration,
        session_abort: Option<Arc<ManagedAbortToken>>,
        operation_abort: Option<Arc<ManagedAbortToken>>,
    ) -> Result<Self, McpError> {
        validate_stdio_cwd(cwd)?;
        #[cfg(windows)]
        let process_job = WindowsProcessJob::new()?;
        #[cfg(windows)]
        let mut process = Command::new(resolve_windows_stdio_command(command, env, cwd)?);
        #[cfg(not(windows))]
        let mut process = Command::new(command);
        process
            .args(args)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        inherit_runtime_environment(&mut process);
        // Explicit MCP configuration wins over the small inherited runtime allowlist.
        process.envs(env);
        if let Some(cwd) = cwd {
            process.current_dir(cwd);
        }
        #[cfg(windows)]
        configure_windows_managed_child(&mut process);
        let mut child = process.spawn().map_err(|error| {
            McpError::new(
                McpErrorKind::Transport,
                format!("Could not start MCP stdio process: {}", error.kind()),
            )
        })?;
        #[cfg(windows)]
        if let Err(error) = process_job.assign(&child) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        #[cfg(windows)]
        if let Err(error) = resume_windows_managed_child(&child) {
            process_job.terminate();
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        let mut stdin = child.stdin.take().ok_or_else(|| {
            McpError::new(McpErrorKind::Transport, "Could not acquire MCP stdio stdin")
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            McpError::new(
                McpErrorKind::Transport,
                "Could not acquire MCP stdio stdout",
            )
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            McpError::new(
                McpErrorKind::Transport,
                "Could not acquire MCP stdio stderr",
            )
        })?;
        let process = Arc::new(ManagedStdioProcess {
            child: Mutex::new(child),
            #[cfg(windows)]
            process_job,
        });
        if let Some(abort) = &session_abort {
            abort.register_stdio(&process);
        }
        if let Some(abort) = &operation_abort {
            abort.register_stdio(&process);
        }
        let (writes, write_requests) = mpsc::sync_channel::<StdioWriteRequest>(1);
        thread::spawn(move || {
            while let Ok(request) = write_requests.recv() {
                let result = stdin
                    .write_all(&request.bytes)
                    .and_then(|_| stdin.flush())
                    .map_err(|_| {
                        McpError::new(
                            McpErrorKind::Transport,
                            "Failed to write to MCP stdio process",
                        )
                    });
                let failed = result.is_err();
                let _ = request.reply.send(result);
                if failed {
                    return;
                }
            }
        });
        let (sender, responses) = mpsc::sync_channel::<Result<Value, McpError>>(8);
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = Vec::new();
                let read_result = (&mut reader)
                    .take((MAX_FRAME_BYTES + 1) as u64)
                    .read_until(b'\n', &mut line);
                let bytes_read = match read_result {
                    Ok(0) => {
                        let _ = sender.send(Err(McpError::new(
                            McpErrorKind::Transport,
                            "MCP stdio process closed its output",
                        )));
                        return;
                    }
                    Ok(bytes_read) => bytes_read,
                    Err(_) => {
                        let _ = sender.send(Err(McpError::new(
                            McpErrorKind::Transport,
                            "Failed to read MCP stdio output",
                        )));
                        return;
                    }
                };
                if bytes_read > MAX_FRAME_BYTES {
                    let _ = sender.send(Err(McpError::new(
                        McpErrorKind::Bounds,
                        "MCP stdio message exceeds 4 MiB",
                    )));
                    return;
                }
                while matches!(line.last(), Some(b'\n' | b'\r')) {
                    line.pop();
                }
                if line.is_empty() {
                    continue;
                }
                let value = match serde_json::from_slice::<Value>(&line) {
                    Ok(value) => value,
                    Err(_) => {
                        let _ = sender.send(Err(McpError::new(
                            McpErrorKind::Protocol,
                            "MCP stdio output is not valid single-line JSON-RPC",
                        )));
                        return;
                    }
                };
                if sender.send(Ok(value)).is_err() {
                    return;
                }
            }
        });
        // Stderr is stdio's diagnostic channel. Keep a bounded buffer for the
        // settings log while keeping it out of model context and persistence.
        let diagnostics = Arc::new(StderrLog::default());
        let stderr_log = Arc::clone(&diagnostics);
        thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            loop {
                let mut line = Vec::new();
                let read = (&mut reader)
                    .take((MAX_STDERR_LINE_BYTES + 1) as u64)
                    .read_until(b'\n', &mut line);
                match read {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {}
                }
                while matches!(line.last(), Some(b'\n' | b'\r')) {
                    line.pop();
                }
                if line.is_empty() {
                    continue;
                }
                stderr_log.push(&String::from_utf8_lossy(&line));
            }
        });
        Ok(Self {
            process,
            writes: Some(writes),
            responses,
            request_timeout,
            shutdown_timeout,
            session_abort,
            diagnostics,
        })
    }

    fn exchange(
        &mut self,
        payload: Value,
        expected_id: Option<Value>,
        timeout: Duration,
    ) -> Result<Value, McpError> {
        let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
            McpError::new(McpErrorKind::Bounds, "MCP request timeout value is invalid")
        })?;
        self.write_payload(&payload, deadline.saturating_duration_since(Instant::now()))?;
        let Some(expected_id) = expected_id else {
            return Ok(Value::Null);
        };
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(McpError::new(
                    McpErrorKind::Timeout,
                    "Timed out waiting for MCP stdio response",
                ));
            }
            let value = match self.responses.recv_timeout(remaining) {
                Ok(Ok(value)) => value,
                Ok(Err(error)) => return Err(error),
                Err(RecvTimeoutError::Timeout) => {
                    return Err(McpError::new(
                        McpErrorKind::Timeout,
                        "Timed out waiting for MCP stdio response",
                    ))
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(McpError::new(
                        McpErrorKind::Transport,
                        "MCP stdio response channel is closed",
                    ))
                }
            };
            match inspect_rpc_message(value, Some(&expected_id))? {
                RpcMessage::Response(result) => return Ok(result),
                RpcMessage::Notification => {}
                RpcMessage::ServerRequest { id, method, params } => {
                    let write_timeout = deadline.saturating_duration_since(Instant::now());
                    self.handle_server_request(id, &method, params.as_ref(), write_timeout)?
                }
                RpcMessage::UnexpectedResponse => {
                    return Err(McpError::new(
                        McpErrorKind::Protocol,
                        "MCP stdio returned an unexpected JSON-RPC request ID",
                    ))
                }
            }
        }
    }

    fn pump_idle(&mut self, frame_budget: usize) -> Result<(), McpError> {
        for _ in 0..frame_budget {
            let value = match self.responses.try_recv() {
                Ok(Ok(value)) => value,
                Ok(Err(error)) => return Err(error),
                Err(TryRecvError::Empty) => return Ok(()),
                Err(TryRecvError::Disconnected) => {
                    return Err(McpError::new(
                        McpErrorKind::Transport,
                        "MCP stdio response channel is closed",
                    ))
                }
            };
            match inspect_rpc_message(value, None)? {
                RpcMessage::Notification => {}
                RpcMessage::ServerRequest { id, method, params } => {
                    self.handle_server_request(id, &method, params.as_ref(), self.request_timeout)?
                }
                RpcMessage::Response(_) | RpcMessage::UnexpectedResponse => {
                    return Err(McpError::new(
                        McpErrorKind::Protocol,
                        "MCP stdio returned an unexpected response while idle",
                    ))
                }
            }
        }
        Ok(())
    }

    fn write_payload(&mut self, payload: &Value, timeout: Duration) -> Result<(), McpError> {
        let mut bytes = serde_json::to_vec(payload).map_err(|_| {
            McpError::new(
                McpErrorKind::InvalidConfiguration,
                "Could not serialize MCP JSON-RPC request",
            )
        })?;
        if bytes.len() > MAX_OUTBOUND_BYTES {
            return Err(McpError::new(
                McpErrorKind::Bounds,
                "MCP JSON-RPC request exceeds 2 MiB",
            ));
        }
        bytes.push(b'\n');
        let (reply, response) = mpsc::sync_channel(1);
        let writes = self
            .writes
            .as_ref()
            .ok_or_else(|| McpError::new(McpErrorKind::Transport, "MCP stdio input is closed"))?;
        match writes.try_send(StdioWriteRequest { bytes, reply }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.process.terminate();
                return Err(McpError::new(
                    McpErrorKind::Transport,
                    "MCP stdio write queue is unexpectedly busy",
                ));
            }
            Err(TrySendError::Disconnected(_)) => {
                return Err(McpError::new(
                    McpErrorKind::Transport,
                    "MCP stdio input is closed",
                ))
            }
        }
        match response.recv_timeout(timeout) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => {
                self.process.terminate();
                Err(McpError::new(
                    McpErrorKind::Timeout,
                    "Timed out writing to MCP stdio process",
                ))
            }
            Err(RecvTimeoutError::Disconnected) => Err(McpError::new(
                McpErrorKind::Transport,
                "MCP stdio writer thread is closed",
            )),
        }
    }

    fn handle_server_request(
        &mut self,
        id: Value,
        method: &str,
        params: Option<&Value>,
        timeout: Duration,
    ) -> Result<(), McpError> {
        validate_rpc_request_id(&id)?;
        if method == "ping" {
            if params.is_some_and(|value| !value.is_object()) {
                return Err(McpError::new(
                    McpErrorKind::Protocol,
                    "MCP ping params must be a JSON object",
                ));
            }
            self.write_payload(
                &json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {},
                }),
                timeout,
            )
        } else {
            self.write_payload(
                &json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32601,
                        "message": "Client capability not advertised",
                    },
                }),
                timeout,
            )
        }
    }
}

fn validate_rpc_request_id(id: &Value) -> Result<(), McpError> {
    let valid_number = id
        .as_number()
        .is_some_and(|number| number.is_i64() || number.is_u64());
    if id.is_string() || valid_number {
        Ok(())
    } else {
        Err(McpError::new(
            McpErrorKind::Protocol,
            "MCP server request ID must be a string or integer",
        ))
    }
}

fn inherit_runtime_environment(command: &mut Command) {
    // A plugin process must not inherit Mework/provider/debug credentials merely because the
    // desktop app happens to have them in its environment. Keep only variables required to find
    // executables and ordinary package-manager/cache directories; plugin-declared env is applied
    // afterwards and may intentionally override these values.
    const SAFE_NAMES: &[&str] = &[
        "PATH",
        "PATHEXT",
        "SystemRoot",
        "WINDIR",
        "COMSPEC",
        "TEMP",
        "TMP",
        "TMPDIR",
        "HOME",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "PROGRAMDATA",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "XDG_CACHE_HOME",
        "XDG_CONFIG_HOME",
    ];
    for name in SAFE_NAMES {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
}

impl Drop for StdioSession {
    fn drop(&mut self) {
        if let Some(abort) = &self.session_abort {
            abort.clear_stdio(&self.process);
        }
        self.writes.take();
        let mut child = self
            .process
            .child
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match child.wait_timeout(self.shutdown_timeout) {
            Ok(Some(_)) => return,
            Ok(None) | Err(_) => {}
        }
        #[cfg(windows)]
        self.process.process_job.terminate();
        let _ = child.kill();
        let _ = child.wait_timeout(self.shutdown_timeout);
    }
}

#[derive(Clone, Default)]
struct HttpAbort {
    session: Option<Arc<ManagedAbortToken>>,
    operation: Option<Arc<ManagedAbortToken>>,
}

impl HttpAbort {
    fn cancelled(&self) -> bool {
        self.session.iter().chain(self.operation.iter()).any(|token| token.is_abandoned())
    }

    fn wait<T>(
        &self,
        runtime: &tokio::runtime::Runtime,
        deadline: Instant,
        future: impl std::future::Future<Output = Result<T, reqwest::Error>>,
    ) -> Result<T, McpError> {
        runtime.block_on(async {
            let cancelled = async {
                loop {
                    if self.cancelled() { break; }
                    tokio::time::sleep(MANAGED_WAIT_SLICE).await;
                }
            };
            tokio::select! {
                biased;
                _ = cancelled => Err(cancelled_error()),
                _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) =>
                    Err(McpError::new(McpErrorKind::Timeout, "MCP HTTP request timed out")),
                result = future => result.map_err(|error| {
                    McpError::new(if error.is_timeout() { McpErrorKind::Timeout } else { McpErrorKind::Transport }, "MCP HTTP request failed")
                }),
            }
        })
    }
}

// Keep the existing bounded JSON/SSE parsers, but every network read awaits an
// abortable async chunk. No detached blocking IO survives a cancelled read.
struct HttpResponseReader {
    response: Response,
    runtime: Arc<tokio::runtime::Runtime>,
    abort: HttpAbort,
    deadline: Instant,
    buffered: std::io::Cursor<Vec<u8>>,
}

impl Read for HttpResponseReader {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if output.is_empty() { return Ok(0); }
        loop {
            let read = self.buffered.read(output)?;
            if read > 0 { return Ok(read); }
            let chunk = self.abort.wait(&self.runtime, self.deadline, self.response.chunk())
                .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
            match chunk {
                Some(chunk) => self.buffered = std::io::Cursor::new(chunk.to_vec()),
                None => return Ok(0),
            }
        }
    }
}

struct HttpSession {
    client: Client,
    runtime: Arc<tokio::runtime::Runtime>,
    abort: HttpAbort,
    url: String,
    headers: HeaderMap,
    session_id: Option<HeaderValue>,
    protocol_version: Option<HeaderValue>,
    shutdown_timeout: Duration,
}

impl HttpSession {
    fn new(
        url: &str,
        headers: &std::collections::BTreeMap<String, String>,
        request_timeout: Duration,
        shutdown_timeout: Duration,
    ) -> Result<Self, McpError> {
        validate_http_endpoint(url)?;
        let headers = build_header_map(headers)?;
        let connect_timeout = request_timeout.min(Duration::from_secs(10));
        let client = Client::builder()
            .connect_timeout(connect_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(format!("{CLIENT_NAME}/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| {
                McpError::new(McpErrorKind::Transport, "Could not create MCP HTTP client")
            })?;
        let runtime = Arc::new(tokio::runtime::Builder::new_current_thread().enable_all().build()
            .map_err(|_| McpError::new(McpErrorKind::Transport, "Could not create MCP HTTP runtime"))?);
        Ok(Self {
            client,
            runtime,
            abort: HttpAbort::default(),
            url: url.to_owned(),
            headers,
            session_id: None,
            protocol_version: None,
            shutdown_timeout,
        })
    }

    fn set_protocol_version(&mut self, version: &str) -> Result<(), McpError> {
        if !is_compatible_protocol_version(version) {
            return Err(McpError::new(
                McpErrorKind::Protocol,
                "Cannot set an incompatible MCP-Protocol-Version",
            ));
        }
        if version == "2024-11-05" {
            return Err(McpError::new(
                McpErrorKind::Protocol,
                "Streamable HTTP does not support legacy HTTP+SSE protocol version 2024-11-05",
            ));
        }
        self.protocol_version = Some(HeaderValue::from_str(version).map_err(|_| {
            McpError::new(
                McpErrorKind::Protocol,
                "MCP-Protocol-Version response value is invalid",
            )
        })?);
        Ok(())
    }

    fn exchange(
        &mut self,
        payload: Value,
        expected_id: Option<Value>,
        timeout: Duration,
    ) -> Result<Value, McpError> {
        let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
            McpError::new(
                McpErrorKind::Bounds,
                "MCP HTTP request timeout value is invalid",
            )
        })?;
        let body = serde_json::to_vec(&payload).map_err(|_| {
            McpError::new(
                McpErrorKind::InvalidConfiguration,
                "Could not serialize MCP HTTP request",
            )
        })?;
        if body.len() > MAX_OUTBOUND_BYTES {
            return Err(McpError::new(
                McpErrorKind::Bounds,
                "MCP HTTP request exceeds 2 MiB",
            ));
        }
        let mut request = self
            .client
            .post(&self.url)
            .headers(self.headers.clone())
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json, text/event-stream")
            .body(body)
            .timeout(timeout);
        if let Some(session_id) = &self.session_id {
            request = request.header(SESSION_HEADER, session_id.clone());
        }
        if let Some(protocol_version) = &self.protocol_version {
            request = request.header(PROTOCOL_HEADER, protocol_version.clone());
        }
        let response = self.abort.wait(&self.runtime, deadline, async { request.send().await })?;
        if response.status().is_success() {
            self.capture_session_id(response.headers())?;
        }
        if !response.status().is_success() {
            return Err(McpError::new(
                McpErrorKind::Transport,
                format!("MCP HTTP returned status {}", response.status().as_u16()),
            ));
        }
        let Some(expected_id) = expected_id else {
            return Ok(Value::Null);
        };
        if response.status().as_u16() == 202 {
            return Err(McpError::new(
                McpErrorKind::Protocol,
                "MCP HTTP request did not return a JSON-RPC response",
            ));
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let mut response = HttpResponseReader {
            response,
            runtime: self.runtime.clone(),
            abort: self.abort.clone(),
            deadline,
            buffered: std::io::Cursor::new(Vec::new()),
        };
        let result = (|| {
        if content_type.contains("text/event-stream") {
            read_sse_response(self, &mut response, &expected_id, deadline)
        } else {
            let value = read_json_response(&mut response)?;
            match inspect_rpc_message(value, Some(&expected_id))? {
                RpcMessage::Response(result) => Ok(result),
                RpcMessage::Notification => Err(McpError::new(
                    McpErrorKind::Protocol,
                    "MCP HTTP returned only a notification",
                )),
                RpcMessage::ServerRequest { id, method, params } => {
                    self.respond_server_request(
                        id,
                        &method,
                        params.as_ref(),
                        deadline.saturating_duration_since(Instant::now()),
                    )?;
                    Err(McpError::new(
                        McpErrorKind::Protocol,
                        "MCP HTTP did not return the target response after a server request",
                    ))
                }
                RpcMessage::UnexpectedResponse => Err(McpError::new(
                    McpErrorKind::Protocol,
                    "MCP HTTP returned an unexpected JSON-RPC request ID",
                )),
            }
        }
        })();
        if self.abort.cancelled() { Err(cancelled_error()) } else { result }
    }

    fn capture_session_id(&mut self, headers: &HeaderMap) -> Result<(), McpError> {
        let Some(value) = headers.get(SESSION_HEADER) else {
            return Ok(());
        };
        if value.as_bytes().len() > MAX_SESSION_ID_BYTES {
            return Err(McpError::new(
                McpErrorKind::Bounds,
                "MCP-Session-Id is too long",
            ));
        }
        value.to_str().map_err(|_| {
            McpError::new(
                McpErrorKind::Protocol,
                "MCP-Session-Id is not a valid HTTP header",
            )
        })?;
        if let Some(current) = &self.session_id {
            if current != value {
                return Err(McpError::new(
                    McpErrorKind::Protocol,
                    "MCP server changed the Session ID during the session",
                ));
            }
        } else {
            let mut session_id = value.clone();
            session_id.set_sensitive(true);
            self.session_id = Some(session_id);
        }
        Ok(())
    }

    fn respond_server_request(
        &self,
        id: Value,
        method: &str,
        params: Option<&Value>,
        timeout: Duration,
    ) -> Result<(), McpError> {
        validate_rpc_request_id(&id)?;
        if timeout.is_zero() {
            return Err(McpError::new(
                McpErrorKind::Timeout,
                "Timed out responding to MCP HTTP server request",
            ));
        }
        let payload = if method == "ping" {
            if params.is_some_and(|value| !value.is_object()) {
                return Err(McpError::new(
                    McpErrorKind::Protocol,
                    "MCP ping params must be a JSON object",
                ));
            }
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {},
            })
        } else {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32601,
                    "message": "Client capability not advertised",
                },
            })
        };
        let body = serde_json::to_vec(&payload).map_err(|_| {
            McpError::new(
                McpErrorKind::Protocol,
                "Could not serialize MCP HTTP server response",
            )
        })?;
        if body.len() > MAX_OUTBOUND_BYTES {
            return Err(McpError::new(
                McpErrorKind::Bounds,
                "MCP HTTP server response is too large",
            ));
        }
        let mut request = self
            .client
            .post(&self.url)
            .headers(self.headers.clone())
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json, text/event-stream")
            .body(body)
            .timeout(timeout);
        if let Some(session_id) = &self.session_id {
            request = request.header(SESSION_HEADER, session_id.clone());
        }
        if let Some(protocol_version) = &self.protocol_version {
            request = request.header(PROTOCOL_HEADER, protocol_version.clone());
        }
        let response = self.abort.wait(&self.runtime, Instant::now() + timeout, async { request.send().await })?;
        if !response.status().is_success() {
            return Err(McpError::new(
                McpErrorKind::Transport,
                format!(
                    "MCP HTTP server response returned status {}",
                    response.status().as_u16()
                ),
            ));
        }
        Ok(())
    }
}

impl Drop for HttpSession {
    fn drop(&mut self) {
        let (Some(session_id), Some(protocol_version)) = (&self.session_id, &self.protocol_version)
        else {
            return;
        };
        let request = self
            .client
            .delete(&self.url)
            .headers(self.headers.clone())
            .header(SESSION_HEADER, session_id.clone())
            .header(PROTOCOL_HEADER, protocol_version.clone())
            .timeout(self.shutdown_timeout);
        let _ = HttpAbort::default().wait(&self.runtime, Instant::now() + self.shutdown_timeout, async { request.send().await });
    }
}

fn validate_http_endpoint(value: &str) -> Result<(), McpError> {
    if value.len() > MAX_HEADER_VALUE_BYTES {
        return Err(McpError::new(
            McpErrorKind::Bounds,
            "MCP HTTP URL is too long",
        ));
    }
    let url = Url::parse(value).map_err(|_| {
        McpError::new(
            McpErrorKind::InvalidConfiguration,
            "MCP HTTP URL is invalid",
        )
    })?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(McpError::new(
            McpErrorKind::InvalidConfiguration,
            "MCP HTTP URL must be an http(s) URL without credentials or fragments",
        ));
    }
    if url.scheme() == "http" && !crate::http_util::is_local_network_url(&url) {
        return Err(McpError::new(
            McpErrorKind::InvalidConfiguration,
            "Public MCP servers must use HTTPS; HTTP is allowed only for local and private-network addresses",
        ));
    }
    Ok(())
}

fn build_header_map(
    values: &std::collections::BTreeMap<String, String>,
) -> Result<HeaderMap, McpError> {
    if values.len() > MAX_HEADERS {
        return Err(McpError::new(
            McpErrorKind::Bounds,
            format!("MCP HTTP header count exceeds {MAX_HEADERS}"),
        ));
    }
    let reserved = [
        "host",
        "content-length",
        "connection",
        "transfer-encoding",
        "accept",
        "content-type",
        SESSION_HEADER,
        PROTOCOL_HEADER,
    ];
    let mut headers = HeaderMap::new();
    for (name, value) in values {
        let header_name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
            McpError::new(
                McpErrorKind::InvalidConfiguration,
                "MCP HTTP header name is invalid",
            )
        })?;
        if reserved.contains(&header_name.as_str()) {
            return Err(McpError::new(
                McpErrorKind::InvalidConfiguration,
                format!(
                    "MCP HTTP header {} is managed by the client",
                    header_name.as_str()
                ),
            ));
        }
        if value.len() > MAX_HEADER_VALUE_BYTES {
            return Err(McpError::new(
                McpErrorKind::Bounds,
                "MCP HTTP header value is too long",
            ));
        }
        let mut header_value = HeaderValue::from_str(value).map_err(|_| {
            McpError::new(
                McpErrorKind::InvalidConfiguration,
                "MCP HTTP header value is invalid",
            )
        })?;
        let lower = header_name.as_str();
        if lower == "authorization"
            || lower == "cookie"
            || lower.contains("token")
            || lower.contains("secret")
            || lower.contains("api-key")
        {
            header_value.set_sensitive(true);
        }
        headers.insert(header_name, header_value);
    }
    Ok(headers)
}

fn read_json_response(response: &mut impl Read) -> Result<Value, McpError> {
    let bytes = read_bounded(response, MAX_HTTP_BODY_BYTES)?;
    if bytes.is_empty() {
        return Err(McpError::new(
            McpErrorKind::Protocol,
            "MCP HTTP response is empty",
        ));
    }
    serde_json::from_slice(&bytes).map_err(|_| {
        McpError::new(
            McpErrorKind::Protocol,
            "MCP HTTP response is not valid JSON",
        )
    })
}

fn read_bounded(reader: &mut impl Read, maximum: usize) -> Result<Vec<u8>, McpError> {
    let mut bytes = Vec::new();
    reader
        .take((maximum + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| McpError::new(McpErrorKind::Transport, "Failed to read MCP HTTP response"))?;
    if bytes.len() > maximum {
        return Err(McpError::new(
            McpErrorKind::Bounds,
            "MCP HTTP response is too large",
        ));
    }
    Ok(bytes)
}

fn read_sse_response(
    session: &HttpSession,
    response: &mut impl Read,
    expected_id: &Value,
    deadline: Instant,
) -> Result<Value, McpError> {
    read_sse_stream_with_handler(response, expected_id, &mut |id, method, params| {
        session.respond_server_request(
            id,
            &method,
            params.as_ref(),
            deadline.saturating_duration_since(Instant::now()),
        )
    })
}

fn read_sse_stream_with_handler(
    reader: &mut impl Read,
    expected_id: &Value,
    on_server_request: &mut impl FnMut(Value, String, Option<Value>) -> Result<(), McpError>,
) -> Result<Value, McpError> {
    let limited = reader.take((MAX_HTTP_BODY_BYTES + 1) as u64);
    let mut reader = BufReader::new(limited);
    let mut total = 0usize;
    let mut data_lines: Vec<String> = Vec::new();
    loop {
        let mut line = Vec::new();
        let bytes_read = reader.read_until(b'\n', &mut line).map_err(|_| {
            McpError::new(McpErrorKind::Transport, "Failed to read MCP SSE response")
        })?;
        if bytes_read == 0 {
            if let Some(result) = parse_sse_event(&mut data_lines, expected_id, on_server_request)?
            {
                return Ok(result);
            }
            return Err(McpError::new(
                McpErrorKind::Protocol,
                "MCP SSE ended before returning the target response",
            ));
        }
        total = total.saturating_add(bytes_read);
        if total > MAX_HTTP_BODY_BYTES {
            return Err(McpError::new(
                McpErrorKind::Bounds,
                "MCP SSE response exceeds 8 MiB",
            ));
        }
        while matches!(line.last(), Some(b'\n' | b'\r')) {
            line.pop();
        }
        if line.is_empty() {
            if let Some(result) = parse_sse_event(&mut data_lines, expected_id, on_server_request)?
            {
                return Ok(result);
            }
            continue;
        }
        let line = std::str::from_utf8(&line)
            .map_err(|_| McpError::new(McpErrorKind::Protocol, "MCP SSE is not valid UTF-8"))?;
        if line.starts_with(':') {
            continue;
        }
        let (field, value) = line
            .split_once(':')
            .map(|(field, value)| (field, value.strip_prefix(' ').unwrap_or(value)))
            .unwrap_or((line, ""));
        if field == "data" {
            data_lines.push(value.to_owned());
        }
    }
}

fn parse_sse_event(
    data_lines: &mut Vec<String>,
    expected_id: &Value,
    on_server_request: &mut impl FnMut(Value, String, Option<Value>) -> Result<(), McpError>,
) -> Result<Option<Value>, McpError> {
    if data_lines.is_empty() {
        return Ok(None);
    }
    let data = data_lines.join("\n");
    data_lines.clear();
    let value: Value = serde_json::from_str(&data)
        .map_err(|_| McpError::new(McpErrorKind::Protocol, "MCP SSE data is not valid JSON-RPC"))?;
    match inspect_rpc_message(value, Some(expected_id))? {
        RpcMessage::Response(result) => Ok(Some(result)),
        RpcMessage::Notification => Ok(None),
        RpcMessage::ServerRequest { id, method, params } => {
            on_server_request(id, method, params)?;
            Ok(None)
        }
        RpcMessage::UnexpectedResponse => Err(McpError::new(
            McpErrorKind::Protocol,
            "MCP SSE returned an unexpected JSON-RPC request ID",
        )),
    }
}

enum RpcMessage {
    Response(Value),
    /// Server-to-client notification. Every caller ignores it, so its body is not carried.
    Notification,
    ServerRequest {
        id: Value,
        method: String,
        params: Option<Value>,
    },
    UnexpectedResponse,
}

fn inspect_rpc_message(value: Value, expected_id: Option<&Value>) -> Result<RpcMessage, McpError> {
    let object = value.as_object().ok_or_else(|| {
        McpError::new(McpErrorKind::Protocol, "MCP message must be a JSON object")
    })?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(McpError::new(
            McpErrorKind::Protocol,
            "MCP message is missing jsonrpc 2.0",
        ));
    }
    if let Some(method) = object.get("method").and_then(Value::as_str) {
        if method.is_empty() || method.chars().any(char::is_control) {
            return Err(McpError::new(
                McpErrorKind::Protocol,
                "MCP request method is invalid",
            ));
        }
        let params = object.get("params").cloned();
        return Ok(match object.get("id") {
            Some(id) => RpcMessage::ServerRequest {
                id: id.clone(),
                method: method.to_owned(),
                params,
            },
            None => RpcMessage::Notification,
        });
    }
    let Some(id) = object.get("id") else {
        return Err(McpError::new(
            McpErrorKind::Protocol,
            "MCP response is missing id",
        ));
    };
    if expected_id.map_or(true, |expected_id| id != expected_id) {
        return Ok(RpcMessage::UnexpectedResponse);
    }
    if let Some(error) = object.get("error") {
        return Err(parse_remote_error(error));
    }
    let result = object.get("result").cloned().ok_or_else(|| {
        McpError::new(
            McpErrorKind::Protocol,
            "MCP response is missing both result and error",
        )
    })?;
    Ok(RpcMessage::Response(result))
}

fn parse_remote_error(value: &Value) -> McpError {
    let message = value
        .as_object()
        .and_then(|object| object.get("message"))
        .and_then(Value::as_str)
        .map(|value| sanitize_error_text(value, 512))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "MCP server returned a JSON-RPC error".to_owned());
    McpError::new(McpErrorKind::Remote, message)
}

fn sanitize_error_text(value: &str, maximum_chars: usize) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(maximum_chars)
        .collect()
}

fn slug(value: &str, fallback: &str, maximum: usize) -> String {
    let mut output = String::new();
    let mut pending_separator = false;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            if pending_separator && !output.is_empty() && output.len() < maximum {
                output.push('_');
            }
            pending_separator = false;
            if output.len() < maximum {
                output.push(character.to_ascii_lowercase());
            }
        } else {
            pending_separator = true;
        }
        if output.len() >= maximum {
            break;
        }
    }
    while output.ends_with('_') {
        output.pop();
    }
    if output.is_empty() {
        fallback.to_owned()
    } else {
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    fn stdio_env(server: &RuntimeMcpServer) -> BTreeMap<String, String> {
        match &server.transport {
            RuntimeMcpTransport::Stdio { env, .. } => env.clone(),
            _ => panic!("expected a stdio transport"),
        }
    }

    fn config_with_registry(command: &str, registry_url: &str) -> crate::model::McpServerConfig {
        crate::model::McpServerConfig {
            id: "mcp-registry".into(),
            name: "Registry".into(),
            transport: crate::model::McpTransportKind::Stdio,
            command: command.into(),
            registry_url: registry_url.into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_registry_mirror_only_reaches_the_toolchain_it_belongs_to() {
        let npm = RuntimeMcpServer::from_config(&config_with_registry(
            "npx",
            "https://registry.npmmirror.com",
        ));
        assert_eq!(
            stdio_env(&npm)
                .get("npm_config_registry")
                .map(String::as_str),
            Some("https://registry.npmmirror.com")
        );
        assert!(stdio_env(&npm).get("PIP_INDEX_URL").is_none());

        let uv = RuntimeMcpServer::from_config(&config_with_registry(
            "uvx.exe",
            "https://pypi.tuna.tsinghua.edu.cn/simple",
        ));
        assert_eq!(
            stdio_env(&uv).get("UV_INDEX_URL").map(String::as_str),
            Some("https://pypi.tuna.tsinghua.edu.cn/simple")
        );
        assert!(stdio_env(&uv).get("npm_config_registry").is_none());

        // Commands outside known toolchains must not receive a mirror URL in
        // their environment.
        let other = RuntimeMcpServer::from_config(&config_with_registry(
            "node",
            "https://registry.npmmirror.com",
        ));
        assert!(stdio_env(&other).is_empty());
    }

    #[test]
    fn a_registry_mirror_never_overrides_an_explicit_environment_entry() {
        let mut config = config_with_registry("npx", "https://registry.npmmirror.com");
        config.env.insert(
            "npm_config_registry".into(),
            "https://npm.internal.example".into(),
        );
        let server = RuntimeMcpServer::from_config(&config);
        assert_eq!(
            stdio_env(&server)
                .get("npm_config_registry")
                .map(String::as_str),
            Some("https://npm.internal.example")
        );
    }

    #[test]
    fn a_registry_mirror_that_is_not_http_is_ignored() {
        for registry in ["file:///etc/passwd", "C:\\packages", "  ", "javascript:0"] {
            let server = RuntimeMcpServer::from_config(&config_with_registry("npx", registry));
            assert!(
                stdio_env(&server).is_empty(),
                "{registry} must not reach the child environment"
            );
        }
    }

    #[test]
    fn a_bounded_stderr_log_keeps_only_the_most_recent_lines() {
        let log = StderrLog::default();
        for index in 0..(MAX_STDERR_LINES + 5) {
            log.push(&format!("line {index}"));
        }
        let snapshot = log.snapshot();
        assert_eq!(snapshot.len(), MAX_STDERR_LINES);
        assert_eq!(snapshot.first().map(String::as_str), Some("line 5"));

        // Control characters break log-panel layout; tabs remain valid indentation.
        log.push("a\u{1b}[31mb\tc");
        assert_eq!(log.snapshot().last().map(String::as_str), Some("a[31mb\tc"));
    }

    fn node_available() -> bool {
        Command::new("node").arg("--version").output().is_ok()
    }

    fn test_options() -> McpClientOptions {
        McpClientOptions {
            request_timeout: Duration::from_secs(5),
            shutdown_timeout: Duration::from_millis(250),
            discovery_timeout: Duration::from_secs(10),
            max_total_schema_bytes: DEFAULT_TOTAL_SCHEMA_BYTES,
        }
    }

    fn mock_server() -> RuntimeMcpServer {
        RuntimeMcpServer {
            artifact_id: "plugin:default/mock@latest".into(),
            server_id: "plugin:default/mock@latest:mcp:echo".into(),
            name: "Mock Server".into(),
            description: "Mock MCP server".into(),
            disabled_tools: Vec::new(),
            confirm_every_call_tools: Vec::new(),
            request_timeout: None,
            transport: RuntimeMcpTransport::Stdio {
                command: "node".into(),
                args: vec![PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("tests")
                    .join("fixtures")
                    .join("mock_mcp_server.js")
                    .to_string_lossy()
                    .into_owned()],
                env: BTreeMap::new(),
                cwd: None,
            },
        }
    }

    #[test]
    fn exposed_names_are_stable_bounded_and_provider_safe() {
        let server = mock_server();
        let first = exposed_tool_name(&server, "Search files (fast)");
        let second = exposed_tool_name(&server, "Search files (fast)");
        assert_eq!(first, second);
        assert!(first.len() <= 64);
        assert!(first
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || value == '_'));
    }

    #[test]
    fn exposed_tool_name_keeps_the_collision_resistant_tool_suffix() {
        let server = mock_server();
        let exposed_name = exposed_tool_name(&server, "Search files (fast)");
        let tool = exposed_name
            .strip_prefix(&format!("mcp__{}__", permission_server_name(&server)))
            .expect("host MCP name uses the trusted server prefix");
        assert!(
            tool.rsplit_once("__")
                .is_some_and(|(_, digest)| digest.len() == 10),
            "exact MCP tool names must retain the host collision suffix"
        );
    }

    #[test]
    fn exposed_tool_name_distinguishes_servers_with_the_same_display_name() {
        let first = mock_server();
        let mut second = first.clone();
        second.artifact_id = "plugin:default/other@latest".into();
        second.server_id = "plugin:default/other@latest:mcp:echo".into();

        let first_name = exposed_tool_name(&first, "read");
        let second_name = exposed_tool_name(&second, "read");
        assert_ne!(first_name, second_name);

        let first_server = permission_server_name(&first);
        let second_server = permission_server_name(&second);
        assert_ne!(first_server, second_server);
        assert!(first_name.starts_with(&format!("mcp__{first_server}__")));
        assert!(second_name.starts_with(&format!("mcp__{second_server}__")));
    }

    #[test]
    fn remote_display_metadata_is_single_line_without_changing_permission_identity() {
        let mut server = mock_server();
        server.description = "Fallback\r\nserver description".into();
        let session = McpSessionInfo {
            protocol_version: LATEST_PROTOCOL_VERSION.into(),
            server_name: None,
            server_version: None,
            supports_prompts: false,
            supports_resources: false,
        };
        let binding = bindings_from_remote_tools(
            &server,
            &session,
            vec![McpRemoteTool {
                name: "read".into(),
                title: Some("Read\r\nWorkspace:\tC:\\temp\u{202e}spoof".into()),
                description: Some("First line\nSecond\tline\u{2028}Third".into()),
                input_schema: json!({"type":"object"}),
                output_schema: None,
                annotations: None,
                meta: None,
            }],
        )
        .pop()
        .unwrap();

        assert_eq!(binding.title, "Read Workspace: C:\\temp spoof");
        assert_eq!(binding.description, "First line Second line Third");
        assert!(!binding.title.chars().any(is_untrusted_display_control));
        assert!(!binding.description.chars().any(char::is_control));
        let expected_name = exposed_tool_name(&server, "read");
        assert_eq!(binding.exposed_name, expected_name);
    }

    #[test]
    fn approval_server_name_rejects_unicode_line_and_bidi_controls() {
        for name in [
            "server\u{2028}injected",
            "server\u{202e}spoof",
            "server\u{2065}spoof",
        ] {
            let mut server = mock_server();
            server.name = name.into();
            let error = validate_runtime_server(&server).unwrap_err();
            assert_eq!(error.kind, McpErrorKind::InvalidConfiguration);
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_bare_npx_resolves_and_launches_cmd_shim() {
        let path_directory = tempfile::tempdir().unwrap();
        let shim = path_directory.path().join("npx.cmd");
        std::fs::write(&shim, "@echo off\r\necho MEWORK_NPX_CMD_OK\r\n").unwrap();
        let env = BTreeMap::from([
            (
                "PATH".into(),
                path_directory.path().to_string_lossy().into_owned(),
            ),
            // Common CMD/BAT fallbacks remain available even when a damaged
            // PATHEXT only advertises EXE.
            ("PATHEXT".into(), ".EXE".into()),
        ]);

        let resolved = resolve_windows_stdio_command("npx", &env, None).unwrap();
        assert_eq!(
            std::fs::canonicalize(&resolved).unwrap(),
            std::fs::canonicalize(&shim).unwrap()
        );
        let mut process = Command::new(resolved);
        process
            .env_clear()
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        inherit_runtime_environment(&mut process);
        process.envs(&env);
        let output = process.output().unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("MEWORK_NPX_CMD_OK"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_command_resolution_keeps_explicit_cwd_and_skips_relative_path_entries() {
        let safe_path = tempfile::tempdir().unwrap();
        let working_directory = tempfile::tempdir().unwrap();
        let safe_shim = safe_path.path().join("runner.cmd");
        let cwd_shim = working_directory.path().join("runner.cmd");
        std::fs::write(&safe_shim, "@echo off\r\n").unwrap();
        std::fs::write(&cwd_shim, "@echo off\r\n").unwrap();
        let env = BTreeMap::from([
            (
                "PATH".into(),
                safe_path.path().to_string_lossy().into_owned(),
            ),
            ("PATHEXT".into(), ".CMD".into()),
        ]);

        let bare = resolve_windows_stdio_command(
            "runner",
            &env,
            Some(&working_directory.path().to_string_lossy()),
        )
        .unwrap();
        assert_eq!(
            std::fs::canonicalize(bare).unwrap(),
            std::fs::canonicalize(&safe_shim).unwrap()
        );
        let explicit = resolve_windows_stdio_command(
            ".\\runner",
            &env,
            Some(&working_directory.path().to_string_lossy()),
        )
        .unwrap();
        assert_eq!(
            std::fs::canonicalize(explicit).unwrap(),
            std::fs::canonicalize(&cwd_shim).unwrap()
        );

        let relative_path_env = BTreeMap::from([
            ("PATH".into(), ".".into()),
            ("PATHEXT".into(), ".CMD".into()),
        ]);
        assert!(resolve_windows_stdio_command(
            "runner",
            &relative_path_env,
            Some(&working_directory.path().to_string_lossy())
        )
        .is_err());
    }

    #[test]
    fn parses_multi_event_sse_until_matching_response() {
        let bytes = b": keepalive\n\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\ndata: {\"jsonrpc\":\"2.0\",\"id\":7,\ndata: \"result\":{\"tools\":[]}}\n\n";
        let result =
            read_sse_stream_with_handler(&mut &bytes[..], &Value::from(7), &mut |_, _, _| {
                panic!("this stream carries no server request")
            })
            .unwrap();
        assert_eq!(result, json!({ "tools": [] }));
    }

    #[test]
    fn sse_server_request_handler_replies_before_target_response() {
        let bytes = b"data: {\"jsonrpc\":\"2.0\",\"id\":\"ping-1\",\"method\":\"ping\",\"params\":{}}\n\ndata: {\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{\"tools\":[]}}\n\n";
        let mut requests = Vec::new();
        let result = read_sse_stream_with_handler(
            &mut &bytes[..],
            &Value::from(7),
            &mut |id, method, params| {
                requests.push((id, method, params));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(result, json!({ "tools": [] }));
        assert_eq!(
            requests,
            vec![(
                Value::String("ping-1".into()),
                "ping".into(),
                Some(json!({}))
            )]
        );
    }

    #[test]
    fn rejects_protocol_owned_http_headers() {
        let values = BTreeMap::from([("MCP-Session-Id".into(), "secret".into())]);
        let error = build_header_map(&values).unwrap_err();
        assert_eq!(error.kind, McpErrorKind::InvalidConfiguration);
    }

    #[test]
    fn recognizes_only_supported_protocol_versions() {
        assert!(!is_compatible_protocol_version("2026-01-01"));
        assert!(is_compatible_protocol_version("2025-11-25"));
    }

    /// Plaintext is accepted exactly where TLS protects nothing: addresses that
    /// cannot be routed off the local network. A public host still requires HTTPS,
    /// because the bearer token in these headers would otherwise cross the internet
    /// in the clear.
    #[test]
    fn http_requires_https_except_on_local_network_hosts() {
        assert!(validate_http_endpoint("http://127.0.0.1:12121/mcp").is_ok());
        assert!(validate_http_endpoint("http://127.42.0.8/mcp").is_ok());
        assert!(validate_http_endpoint("http://[::1]:12121/mcp").is_ok());
        assert!(validate_http_endpoint("http://localhost:12121/mcp").is_ok());
        // A self-hosted MCP server on another machine in the house.
        assert!(validate_http_endpoint("http://10.0.0.4/mcp").is_ok());
        assert!(validate_http_endpoint("http://192.168.1.4/mcp").is_ok());
        assert!(validate_http_endpoint("http://169.254.2.3/mcp").is_ok());
        assert!(validate_http_endpoint("http://[fe80::1]/mcp").is_ok());
        assert!(validate_http_endpoint("http://printer.local/mcp").is_ok());
        // Public names and addresses keep the TLS requirement.
        assert!(validate_http_endpoint("http://example.com/mcp").is_err());
        assert!(validate_http_endpoint("http://93.184.216.34/mcp").is_err());
        assert!(validate_http_endpoint("https://10.0.0.4/mcp").is_ok());
        assert!(validate_http_endpoint("https://example.com/mcp").is_ok());
    }

    #[test]
    fn secret_bearing_debug_output_is_redacted() {
        let server = RuntimeMcpServer {
            artifact_id: "plugin:test".into(),
            server_id: "secret-test".into(),
            name: "Secret test".into(),
            description: "description".into(),
            disabled_tools: Vec::new(),
            confirm_every_call_tools: Vec::new(),
            request_timeout: None,
            transport: RuntimeMcpTransport::Http {
                url: "https://example.com/mcp?access_token=do-not-print".into(),
                headers: BTreeMap::from([("Authorization".into(), "Bearer do-not-print".into())]),
            },
        };
        let binding = McpToolBinding {
            exposed_name: "mcp__secret_test__read__0000000000".into(),
            remote_name: "read".into(),
            title: "Read".into(),
            description: "Read".into(),
            input_schema: json!({
                "type": "object",
                "default": "schema-secret-do-not-print",
            }),
            output_schema: None,
            annotations: None,
            requires_user_interaction: false,
            user_requires_confirmation: false,
            negotiated_protocol_version: LATEST_PROTOCOL_VERSION.into(),
            server,
        };
        let output = format!("{binding:?}");
        let server_output = format!("{:?}", binding.server);
        assert!(!output.contains("do-not-print"));
        assert!(!output.contains("schema-secret"));
        assert!(!server_output.contains("do-not-print"));
        assert!(output.contains("input_schema_bytes"));
    }

    #[test]
    fn model_output_excludes_host_only_mcp_meta() {
        let result = McpToolCallResult {
            content: vec![json!({ "type": "text", "text": "visible" })],
            structured_content: Some(json!({ "answer": 42 })),
            is_error: false,
            meta: Some(json!({
                "mcp/www_authenticate": "Bearer host-secret-do-not-persist",
                "trace": "host-only"
            })),
        };
        let output = result.model_output();
        assert!(output.contains("visible"));
        assert!(output.contains("\"answer\":42"));
        assert!(!output.contains("host-secret"));
        assert!(!output.contains("host-only"));
        assert!(!output.contains("_meta"));
    }

    #[test]
    fn parses_requires_user_interaction_meta_and_fails_closed_on_invalid_values() {
        let parse = |meta: Value| {
            let tool: McpRemoteTool = serde_json::from_value(json!({
                "name": "dangerous",
                "inputSchema": {"type":"object"},
                "_meta": meta
            }))
            .unwrap();
            tool_requires_user_interaction(tool.meta.as_ref())
        };

        assert!(parse(json!({"anthropic/requiresUserInteraction":true})));
        assert!(!parse(json!({"anthropic/requiresUserInteraction":false})));
        assert!(parse(json!({"anthropic/requiresUserInteraction":"true"})));
        assert!(!parse(json!({"unrelated":true})));
    }

    /// The Tools tab's two switches are persisted per remote tool name and
    /// must reach the bindings: a tool switched off never becomes a binding,
    /// and a tool whose auto-approve is off asks on every call exactly like a
    /// remote `requiresUserInteraction` declaration.
    #[test]
    fn user_tool_switches_filter_and_mark_the_bindings() {
        let mut server = mock_server();
        server.disabled_tools = vec!["hidden".into()];
        server.confirm_every_call_tools = vec!["careful".into()];
        let tools = ["hidden", "careful", "plain"]
            .iter()
            .map(|name| {
                serde_json::from_value::<McpRemoteTool>(json!({
                    "name": name,
                    "inputSchema": {"type": "object"}
                }))
                .unwrap()
            })
            .collect::<Vec<_>>();
        let session_info = McpSessionInfo {
            protocol_version: LATEST_PROTOCOL_VERSION.into(),
            server_name: None,
            server_version: None,
            supports_prompts: false,
            supports_resources: false,
        };
        let bindings = bindings_from_remote_tools(&server, &session_info, tools);
        let names = bindings
            .iter()
            .map(|binding| binding.remote_name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["careful", "plain"]);
        let careful = &bindings[0];
        assert!(careful.user_requires_confirmation);
        assert!(!careful.requires_user_interaction);
        assert!(careful.confirmation_required());
        let plain = &bindings[1];
        assert!(!plain.user_requires_confirmation);
        assert!(!plain.confirmation_required());
    }

    /// `timeoutSeconds` wins over `longRunning`; `longRunning` alone lifts the
    /// default to the transport ceiling; neither keeps the client default.
    #[test]
    fn the_two_timeout_settings_map_onto_one_effective_timeout() {
        let default = Duration::from_secs(45);
        assert_eq!(configured_request_timeout(0, false), None);
        assert_eq!(
            configured_request_timeout(0, true),
            Some(MAX_REQUEST_TIMEOUT)
        );
        assert_eq!(
            configured_request_timeout(120, true),
            Some(Duration::from_secs(120))
        );
        assert_eq!(
            configured_request_timeout(100_000, false),
            Some(MAX_REQUEST_TIMEOUT)
        );
        let mut server = mock_server();
        assert_eq!(server.effective_request_timeout(default), default);
        server.request_timeout = Some(Duration::from_secs(120));
        assert_eq!(
            server.effective_request_timeout(default),
            Duration::from_secs(120)
        );
        let config = crate::model::McpServerConfig {
            timeout_seconds: 90,
            long_running: true,
            disabled_tools: vec!["a".into()],
            disabled_auto_approve_tools: vec!["b".into()],
            ..Default::default()
        };
        let runtime = RuntimeMcpServer::from_config(&config);
        assert_eq!(runtime.request_timeout, Some(Duration::from_secs(90)));
        assert_eq!(runtime.disabled_tools, ["a"]);
        assert_eq!(runtime.confirm_every_call_tools, ["b"]);
    }

    #[test]
    fn elapsed_discovery_deadline_refuses_another_request() {
        let deadline = Instant::now()
            .checked_sub(Duration::from_millis(1))
            .unwrap();
        let error = operation_timeout(Duration::from_secs(1), Some(deadline)).unwrap_err();
        assert_eq!(error.kind, McpErrorKind::Timeout);
    }

    #[test]
    fn stdio_write_uses_the_same_bounded_request_deadline() {
        if !node_available() {
            return;
        }
        let server = RuntimeMcpServer {
            artifact_id: "plugin:default/blocking-write@latest".into(),
            server_id: "plugin:default/blocking-write@latest:mcp:block".into(),
            name: "Blocking write".into(),
            description: "Stops reading stdin after initialize".into(),
            disabled_tools: Vec::new(),
            confirm_every_call_tools: Vec::new(),
            request_timeout: None,
            transport: RuntimeMcpTransport::Stdio {
                command: "node".into(),
                args: vec![
                    "-e".into(),
                    r#"
let buffered = "";
let handled = false;
process.stdin.setEncoding("utf8");
process.stdin.on("data", (chunk) => {
  if (handled) return;
  buffered += chunk;
  const newline = buffered.indexOf("\n");
  if (newline < 0) return;
  handled = true;
  const message = JSON.parse(buffered.slice(0, newline));
  process.stdout.write(JSON.stringify({
    jsonrpc: "2.0",
    id: message.id,
    result: {
      protocolVersion: "2025-11-25",
      capabilities: { tools: {} },
      serverInfo: { name: "blocking-write", version: "1.0.0" }
    }
  }) + "\n");
  process.stdin.pause();
  setInterval(() => {}, 1000);
});
"#
                    .into(),
                ],
                env: BTreeMap::new(),
                cwd: None,
            },
        };
        let client = McpClient::new(McpClientOptions {
            request_timeout: Duration::from_millis(200),
            shutdown_timeout: Duration::from_millis(50),
            discovery_timeout: Duration::from_secs(1),
            max_total_schema_bytes: DEFAULT_TOTAL_SCHEMA_BYTES,
        })
        .unwrap();
        let started = Instant::now();
        let error = client
            .call_server_tool(&server, "large", json!({ "value": "x".repeat(512 * 1024) }))
            .unwrap_err();
        assert!(matches!(
            error.kind,
            McpErrorKind::Timeout | McpErrorKind::Transport
        ));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn stdio_initialize_list_pagination_and_call() {
        if !node_available() {
            return;
        }
        let client = McpClient::new(test_options()).unwrap();
        let bindings = client.discover_server_tools(&mock_server()).unwrap();
        assert_eq!(bindings.len(), 2);
        assert_eq!(bindings[0].remote_name, "echo");
        assert_eq!(bindings[1].remote_name, "add");
        let result = client
            .call_tool(&bindings[0], json!({ "value": "hello" }))
            .unwrap();
        assert!(!result.is_error);
        assert_eq!(
            result.content[0].get("text").and_then(Value::as_str),
            Some("hello")
        );
        assert_eq!(
            result
                .structured_content
                .as_ref()
                .and_then(|value| value.get("echo"))
                .and_then(Value::as_str),
            Some("hello")
        );
    }

    #[test]
    fn resilient_discovery_isolates_a_bad_server_but_strict_probe_fails() {
        if !node_available() {
            return;
        }
        let mut invalid = mock_server();
        invalid.server_id = "invalid-server".into();
        invalid.name = "Invalid server".into();
        invalid.transport = RuntimeMcpTransport::Stdio {
            command: String::new(),
            args: Vec::new(),
            env: BTreeMap::from([(
                "SHOULD_NOT_APPEAR".into(),
                "discovery-secret-do-not-print".into(),
            )]),
            cwd: None,
        };
        let client = McpClient::new(test_options()).unwrap();
        let servers = vec![invalid, mock_server()];
        let report = client.discover_tools_report(&servers);
        assert_eq!(report.bindings.len(), 2);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].server_id, "invalid-server");
        assert!(!format!("{:?}", report.failures).contains("do-not-print"));
        assert!(client.discover_tools(&servers).is_err());
    }

    #[test]
    fn cumulative_schema_budget_skips_oversized_server() {
        if !node_available() {
            return;
        }
        let client = McpClient::new(McpClientOptions {
            request_timeout: Duration::from_secs(5),
            shutdown_timeout: Duration::from_millis(250),
            discovery_timeout: Duration::from_secs(10),
            max_total_schema_bytes: 32,
        })
        .unwrap();
        let report = client.discover_tools_report(&[mock_server()]);
        assert!(report.bindings.is_empty());
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].kind, McpErrorKind::Bounds);
        assert_eq!(report.schema_bytes, 0);
    }

    #[test]
    fn cancelling_http_body_closes_the_inflight_connection() {
        for content_type in ["application/json", "text/event-stream"] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/mcp", listener.local_addr().unwrap());
            let (body_started_tx, body_started_rx) = mpsc::sync_channel(1);
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                let mut request = [0u8; 8192];
                assert!(stream.read(&mut request).unwrap() > 0);
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n{{").unwrap();
                stream.flush().unwrap();
                body_started_tx.send(()).unwrap();
                matches!(stream.read(&mut request), Ok(0))
            });
            let abort = Arc::new(ManagedAbortToken::default());
            let operation_abort = abort.clone();
            let client = thread::spawn(move || {
                let session = HttpSession::new(&url, &BTreeMap::new(), Duration::from_secs(30), Duration::from_millis(50)).unwrap();
                let mut transport = ConnectionTransport::Http(session);
                transport.bind_operation_abort(&operation_abort);
                transport.exchange(json!({"jsonrpc":"2.0","id":1,"method":"tools/call"}), Some(json!(1)), Duration::from_secs(30))
            });
            body_started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            abort.abandon();
            let closed = server.join().unwrap();
            let result = client.join().unwrap();
            assert!(closed, "{content_type}: cancellation left the body connection alive");
            assert_eq!(result.unwrap_err().kind, McpErrorKind::Cancelled);
        }
    }

    #[test]
    fn cancelled_http_actor_drains_pending_and_reinitializes() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (closed_tx, closed_rx) = mpsc::sync_channel(1);
        let upstream = thread::spawn(move || {
            let mut initializes = 0;
            let mut calls = 0;
            loop {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                let (headers, payload) = {
                    let mut reader = BufReader::new(&mut stream);
                    let mut headers = String::new();
                    let mut length = 0;
                    loop {
                        let mut line = String::new();
                        reader.read_line(&mut line).unwrap();
                        if line == "\r\n" { break; }
                        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                            length = value.trim().parse::<usize>().unwrap();
                        }
                        headers.push_str(&line);
                    }
                    let mut bytes = vec![0; length];
                    reader.read_exact(&mut bytes).unwrap();
                    (headers, serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null))
                };
                let method = payload["method"].as_str().unwrap_or("");
                let result = match method {
                    "initialize" => {
                        initializes += 1;
                        assert!(!headers.to_ascii_lowercase().contains("mcp-session-id:"));
                        json!({"protocolVersion":LATEST_PROTOCOL_VERSION,"capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}})
                    }
                    "tools/list" => json!({"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}),
                    "tools/call" => {
                        calls += 1;
                        assert!(headers.contains(&format!("session-{initializes}")));
                        if calls == 1 {
                            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n{").unwrap();
                            stream.flush().unwrap();
                            started_tx.send(()).unwrap();
                            let mut byte = [0];
                            closed_tx.send(matches!(stream.read(&mut byte), Ok(0))).unwrap();
                            continue;
                        }
                        json!({"content":[{"type":"text","text":"fresh"}]})
                    }
                    _ => Value::Null,
                };
                let body = json!({"jsonrpc":"2.0","id":payload["id"],"result":result}).to_string();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nMcp-Session-Id: session-{initializes}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                if calls == 2 { return initializes; }
            }
        });
        let mut server = mock_server();
        server.transport = RuntimeMcpTransport::Http { url, headers: BTreeMap::new() };
        let mut options = test_options();
        options.request_timeout = Duration::from_secs(30);
        options.shutdown_timeout = Duration::from_millis(50);
        let manager = McpSessionManager::new(options, 2, Duration::from_secs(30)).unwrap();
        let report = manager.discover_for_conversation("http-cancel", &[server], crate::cancel::CancelSignal::default());
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        let binding = report.bindings[0].clone();
        let flag = Arc::new(AtomicBool::new(false));
        let worker_flag = flag.clone();
        let worker_manager = manager.clone();
        let worker_binding = binding.clone();
        let call = thread::spawn(move || worker_manager.call_for_conversation("http-cancel", &worker_binding, json!({}), crate::cancel::CancelSignal::from_flag(worker_flag)));
        started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        flag.store(true, Ordering::Release);
        assert_eq!(call.join().unwrap().unwrap_err().kind, McpErrorKind::Cancelled);
        assert!(closed_rx.recv_timeout(Duration::from_secs(1)).unwrap());
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let pending: usize = manager.inner.entries.lock().unwrap().values().map(|worker| worker.pending.load(Ordering::Acquire)).sum();
            if pending == 0 { break; }
            assert!(Instant::now() < deadline, "cancelled actor retained pending work");
            thread::yield_now();
        }
        manager.call_for_conversation("http-cancel", &binding, json!({}), crate::cancel::CancelSignal::default()).unwrap();
        assert_eq!(upstream.join().unwrap(), 2);
    }

    #[test]
    fn managed_sessions_reuse_state_within_a_conversation_and_isolate_conversations() {
        if !node_available() {
            return;
        }
        let manager = McpSessionManager::new(test_options(), 4, Duration::from_secs(30)).unwrap();
        let server = mock_server();
        let first_report = manager.discover_for_conversation(
            "conversation-a",
            &[server.clone()],
            crate::cancel::CancelSignal::default(),
        );
        assert!(first_report.failures.is_empty());
        let binding = first_report.bindings[0].clone();

        let first = manager
            .call_for_conversation(
                "conversation-a",
                &binding,
                json!({ "value": "first" }),
                crate::cancel::CancelSignal::default(),
            )
            .unwrap();
        let second = manager
            .call_for_conversation(
                "conversation-a",
                &binding,
                json!({ "value": "second" }),
                crate::cancel::CancelSignal::default(),
            )
            .unwrap();
        assert_eq!(
            first
                .structured_content
                .as_ref()
                .and_then(|value| value.get("callCount"))
                .and_then(Value::as_u64),
            Some(1)
        );
        assert_eq!(
            second
                .structured_content
                .as_ref()
                .and_then(|value| value.get("callCount"))
                .and_then(Value::as_u64),
            Some(2)
        );

        let second_report = manager.discover_for_conversation(
            "conversation-b",
            &[server],
            crate::cancel::CancelSignal::default(),
        );
        assert!(second_report.failures.is_empty());
        let isolated = manager
            .call_for_conversation(
                "conversation-b",
                &second_report.bindings[0],
                json!({ "value": "isolated" }),
                crate::cancel::CancelSignal::default(),
            )
            .unwrap();
        assert_eq!(
            isolated
                .structured_content
                .as_ref()
                .and_then(|value| value.get("callCount"))
                .and_then(Value::as_u64),
            Some(1)
        );
        assert_eq!(manager.session_count(), 2);
    }

    #[test]
    fn managed_session_pumps_idle_stdio_ping_without_losing_state() {
        if !node_available() {
            return;
        }
        let manager = McpSessionManager::new(test_options(), 2, Duration::from_secs(30)).unwrap();
        let mut server = mock_server();
        let RuntimeMcpTransport::Stdio { env, .. } = &mut server.transport else {
            unreachable!();
        };
        env.insert("MCP_MOCK_IDLE_PING".into(), "1".into());
        let report = manager.discover_for_conversation(
            "conversation-idle-ping",
            &[server],
            crate::cancel::CancelSignal::default(),
        );
        assert!(report.failures.is_empty());
        thread::sleep(MANAGED_IDLE_PUMP_SLICE + Duration::from_millis(150));
        let result = manager
            .call_for_conversation(
                "conversation-idle-ping",
                &report.bindings[0],
                json!({ "value": "after-ping" }),
                crate::cancel::CancelSignal::default(),
            )
            .unwrap();
        assert_eq!(
            result
                .structured_content
                .as_ref()
                .and_then(|value| value.get("callCount"))
                .and_then(Value::as_u64),
            Some(1)
        );
    }

    #[test]
    fn cancelled_managed_call_returns_promptly_and_drops_the_late_session() {
        if !node_available() {
            return;
        }
        let manager = McpSessionManager::new(test_options(), 2, Duration::from_secs(30)).unwrap();
        let report = manager.discover_for_conversation(
            "conversation-cancel",
            &[mock_server()],
            crate::cancel::CancelSignal::default(),
        );
        assert!(report.failures.is_empty());
        let binding = report.bindings[0].clone();
        let cancellation = Arc::new(AtomicBool::new(false));
        let worker_manager = manager.clone();
        let worker_binding = binding.clone();
        let worker_cancellation = cancellation.clone();
        let call = thread::spawn(move || {
            worker_manager.call_for_conversation(
                "conversation-cancel",
                &worker_binding,
                json!({ "value": "late", "delayMs": 400 }),
                crate::cancel::CancelSignal::from_flag(worker_cancellation),
            )
        });

        thread::sleep(Duration::from_millis(75));
        let cancelled_at = Instant::now();
        cancellation.store(true, Ordering::Release);
        let error = call.join().unwrap().unwrap_err();
        assert_eq!(error.kind, McpErrorKind::Cancelled);
        assert!(cancelled_at.elapsed() < Duration::from_secs(1));

        let next = manager
            .call_for_conversation(
                "conversation-cancel",
                &binding,
                json!({ "value": "fresh" }),
                crate::cancel::CancelSignal::default(),
            )
            .unwrap();
        assert_eq!(
            next.structured_content
                .as_ref()
                .and_then(|value| value.get("callCount"))
                .and_then(Value::as_u64),
            Some(1)
        );
    }

    #[test]
    fn cancelling_a_queued_call_does_not_abort_or_reset_the_active_session() {
        if !node_available() {
            return;
        }
        let manager = McpSessionManager::new(test_options(), 2, Duration::from_secs(30)).unwrap();
        let report = manager.discover_for_conversation(
            "conversation-queued-cancel",
            &[mock_server()],
            crate::cancel::CancelSignal::default(),
        );
        assert!(report.failures.is_empty());
        let binding = report.bindings[0].clone();

        let first_manager = manager.clone();
        let first_binding = binding.clone();
        let first = thread::spawn(move || {
            first_manager.call_for_conversation(
                "conversation-queued-cancel",
                &first_binding,
                json!({ "value": "active", "delayMs": 400 }),
                crate::cancel::CancelSignal::default(),
            )
        });
        thread::sleep(Duration::from_millis(50));

        let queued_cancellation = Arc::new(AtomicBool::new(false));
        let queued_manager = manager.clone();
        let queued_binding = binding.clone();
        let queued_flag = queued_cancellation.clone();
        let queued = thread::spawn(move || {
            queued_manager.call_for_conversation(
                "conversation-queued-cancel",
                &queued_binding,
                json!({ "value": "must-not-run" }),
                crate::cancel::CancelSignal::from_flag(queued_flag),
            )
        });
        thread::sleep(Duration::from_millis(50));
        queued_cancellation.store(true, Ordering::Release);

        let queued_error = queued.join().unwrap().unwrap_err();
        assert_eq!(queued_error.kind, McpErrorKind::Cancelled);
        let first_result = first.join().unwrap().unwrap();
        assert_eq!(
            first_result
                .structured_content
                .as_ref()
                .and_then(|value| value.get("callCount"))
                .and_then(Value::as_u64),
            Some(1)
        );

        let next = manager
            .call_for_conversation(
                "conversation-queued-cancel",
                &binding,
                json!({ "value": "still-live" }),
                crate::cancel::CancelSignal::default(),
            )
            .unwrap();
        assert_eq!(
            next.structured_content
                .as_ref()
                .and_then(|value| value.get("callCount"))
                .and_then(Value::as_u64),
            Some(2)
        );
    }

    #[test]
    fn held_worker_cannot_be_lru_evicted_before_it_reserves_pending_work() {
        let manager = McpSessionManager::new(test_options(), 1, Duration::from_secs(30)).unwrap();
        let server = mock_server();
        let held = manager.worker("conversation-held-a", &server).unwrap();
        let error = manager.worker("conversation-held-b", &server).unwrap_err();
        assert_eq!(error.kind, McpErrorKind::Bounds);
        assert_eq!(manager.session_count(), 1);

        drop(held);
        let replacement = manager.worker("conversation-held-b", &server).unwrap();
        assert_eq!(manager.session_count(), 1);
        drop(replacement);
    }

    #[test]
    fn managed_sessions_enforce_lru_capacity_and_idle_ttl() {
        if !node_available() {
            return;
        }
        let server = mock_server();
        let capacity_manager =
            McpSessionManager::new(test_options(), 1, Duration::from_secs(30)).unwrap();
        let first = capacity_manager.discover_for_conversation(
            "conversation-lru-a",
            &[server.clone()],
            crate::cancel::CancelSignal::default(),
        );
        assert!(first.failures.is_empty());
        let second = capacity_manager.discover_for_conversation(
            "conversation-lru-b",
            &[server.clone()],
            crate::cancel::CancelSignal::default(),
        );
        assert!(second.failures.is_empty());
        assert_eq!(capacity_manager.session_count(), 1);

        let ttl_manager =
            McpSessionManager::new(test_options(), 2, Duration::from_millis(40)).unwrap();
        let first = ttl_manager.discover_for_conversation(
            "conversation-ttl-a",
            &[server.clone()],
            crate::cancel::CancelSignal::default(),
        );
        assert!(first.failures.is_empty());
        thread::sleep(Duration::from_millis(80));
        let second = ttl_manager.discover_for_conversation(
            "conversation-ttl-b",
            &[server],
            crate::cancel::CancelSignal::default(),
        );
        assert!(second.failures.is_empty());
        assert_eq!(ttl_manager.session_count(), 1);
    }

    #[test]
    fn managed_sessions_support_explicit_lifecycle_eviction() {
        if !node_available() {
            return;
        }
        let manager = McpSessionManager::new(test_options(), 4, Duration::from_secs(30)).unwrap();
        let server = mock_server();
        assert!(manager
            .discover_for_conversation(
                "conversation-evict-a",
                &[server.clone()],
                crate::cancel::CancelSignal::default()
            )
            .failures
            .is_empty());
        assert!(manager
            .discover_for_conversation(
                "conversation-evict-b",
                &[server.clone()],
                crate::cancel::CancelSignal::default()
            )
            .failures
            .is_empty());
        assert_eq!(manager.session_count(), 2);

        manager.evict_conversations(["conversation-evict-a"]);
        assert_eq!(manager.session_count(), 1);
        manager.evict_artifact(&server.artifact_id);
        assert_eq!(manager.session_count(), 0);

        assert!(manager
            .discover_for_conversation(
                "conversation-evict-c",
                &[server],
                crate::cancel::CancelSignal::default()
            )
            .failures
            .is_empty());
        manager.clear();
        assert_eq!(manager.session_count(), 0);
    }

    #[test]
    fn managed_session_debug_does_not_expose_transport_secrets() {
        let server = RuntimeMcpServer {
            artifact_id: "plugin:test".into(),
            server_id: "secret-test".into(),
            name: "Secret test".into(),
            description: "description".into(),
            disabled_tools: Vec::new(),
            confirm_every_call_tools: Vec::new(),
            request_timeout: None,
            transport: RuntimeMcpTransport::Http {
                url: "https://example.com/mcp?access_token=do-not-print".into(),
                headers: BTreeMap::from([("Authorization".into(), "Bearer do-not-print".into())]),
            },
        };
        let key = managed_session_key("conversation-secret", &server);
        let output = format!("{key:?}");
        let manager = McpSessionManager::default();
        let manager_output = format!("{manager:?}");
        assert!(!output.contains("do-not-print"));
        assert!(!manager_output.contains("do-not-print"));
        assert!(output.contains("config_fingerprint"));
    }

    #[test]
    fn stdio_cwd_must_be_an_existing_absolute_directory_and_is_applied() {
        let mut server = mock_server();
        let RuntimeMcpTransport::Stdio { cwd, .. } = &mut server.transport else {
            unreachable!();
        };
        *cwd = Some("relative-directory".into());
        let error = validate_runtime_server(&server).unwrap_err();
        assert_eq!(error.kind, McpErrorKind::InvalidConfiguration);

        let temporary = tempfile::tempdir().unwrap();
        let missing = temporary.path().join("missing");
        let RuntimeMcpTransport::Stdio { cwd, .. } = &mut server.transport else {
            unreachable!();
        };
        *cwd = Some(missing.to_string_lossy().into_owned());
        let error = validate_runtime_server(&server).unwrap_err();
        assert_eq!(error.kind, McpErrorKind::InvalidConfiguration);

        let RuntimeMcpTransport::Stdio { cwd, .. } = &mut server.transport else {
            unreachable!();
        };
        *cwd = Some(temporary.path().to_string_lossy().into_owned());
        validate_runtime_server(&server).unwrap();
        if !node_available() {
            return;
        }
        let client = McpClient::new(test_options()).unwrap();
        let binding = client.discover_server_tools(&server).unwrap()[0].clone();
        let result = client
            .call_tool(&binding, json!({ "value": "cwd" }))
            .unwrap();
        let actual = result
            .structured_content
            .as_ref()
            .and_then(|value| value.get("cwd"))
            .and_then(Value::as_str)
            .unwrap();
        assert_eq!(
            std::fs::canonicalize(actual).unwrap(),
            std::fs::canonicalize(temporary.path()).unwrap()
        );
    }

    /// Probe ids arrive from the renderer, so the host validates them before one
    /// can name a registration.
    #[test]
    fn probe_ids_must_be_bounded_ascii_identifiers() {
        assert_eq!(
            validate_probe_id("").unwrap_err().kind,
            McpErrorKind::InvalidConfiguration
        );
        assert_eq!(
            validate_probe_id(&"a".repeat(MAX_PROBE_ID_BYTES + 1))
                .unwrap_err()
                .kind,
            McpErrorKind::InvalidConfiguration
        );
        assert_eq!(
            validate_probe_id("probe id").unwrap_err().kind,
            McpErrorKind::InvalidConfiguration
        );
        // A refused cancel is reported rather than quietly succeeding, so the
        // settings page cannot show a stop button that does nothing.
        assert_eq!(
            cancel_probe("probe/../elsewhere").unwrap_err().kind,
            McpErrorKind::InvalidConfiguration
        );
        validate_probe_id("probe-1_A").unwrap();
    }

    /// The renderer cancels by id, which may reach the host before the blocking
    /// worker registers. The user asked for it first, so it wins.
    #[test]
    fn a_cancel_that_arrives_before_the_probe_starts_stops_it_from_starting() {
        cancel_probe("probe-early-cancel").unwrap();
        let error = begin_probe("probe-early-cancel").unwrap_err();
        assert_eq!(error.kind, McpErrorKind::Cancelled);

        // The record is consumed, so the next probe of that dial still runs.
        let lease = begin_probe("probe-early-cancel").unwrap();
        assert!(!lease.abort().is_abandoned());
    }

    #[test]
    fn a_probe_id_is_reusable_only_after_its_probe_released_the_registration() {
        let lease = begin_probe("probe-registration").unwrap();
        assert_eq!(
            begin_probe("probe-registration").unwrap_err().kind,
            McpErrorKind::InvalidConfiguration
        );

        // Success, failure and panic all drop the lease, which reclaims the entry.
        drop(lease);
        let second = begin_probe("probe-registration").unwrap();
        cancel_probe("probe-registration").unwrap();
        assert!(second.abort().is_abandoned());
        drop(second);
        assert!(!lock_probe_registry().live.contains_key("probe-registration"));
    }

    #[test]
    fn cancelling_one_probe_is_idempotent_and_leaves_other_probes_running() {
        let first = begin_probe("probe-scope-first").unwrap();
        let second = begin_probe("probe-scope-second").unwrap();

        cancel_probe("probe-scope-first").unwrap();
        cancel_probe("probe-scope-first").unwrap();
        assert!(first.abort().is_abandoned());
        // A second dial of the same server is a different probe.
        assert!(!second.abort().is_abandoned());

        // Cancelling a probe that already finished is not an error either.
        drop(first);
        cancel_probe("probe-scope-first").unwrap();
        drop(second);
    }

    /// Builds a probe server whose mock hangs on `method`, and the marker file the
    /// mock writes once that request arrives.
    fn hanging_probe_server(
        temporary: &tempfile::TempDir,
        method: &str,
        declare_prompts: bool,
    ) -> (RuntimeMcpServer, PathBuf) {
        let marker = temporary.path().join("request-arrived");
        let mut server = mock_server();
        let RuntimeMcpTransport::Stdio { env, .. } = &mut server.transport else {
            unreachable!();
        };
        env.insert("MCP_MOCK_HANG_METHOD".into(), method.into());
        env.insert(
            "MCP_MOCK_MARKER".into(),
            marker.to_string_lossy().into_owned(),
        );
        if declare_prompts {
            env.insert("MCP_MOCK_DECLARE_PROMPTS".into(), "1".into());
        }
        (server, marker)
    }

    /// Waits until the mock reports that the hanging request arrived, so the
    /// cancel lands on a probe that is provably in flight.
    fn await_probe_request(marker: &Path) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !marker.exists() {
            assert!(
                Instant::now() < deadline,
                "the probe never reached the mock server"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn cancelling_a_stdio_probe_reports_cancellation_without_waiting_out_the_request() {
        if !node_available() {
            return;
        }
        let temporary = tempfile::tempdir().unwrap();
        let (server, marker) = hanging_probe_server(&temporary, "tools/list", false);
        let probe = thread::spawn(move || probe_server_for_settings(&server, "probe-live-stdio"));
        await_probe_request(&marker);

        let cancelled_at = Instant::now();
        cancel_probe("probe-live-stdio").unwrap();
        let (error, _logs) = probe.join().unwrap().unwrap_err();

        // The user stopped the probe: report that, not the closed pipe the
        // cancellation itself caused, and do not wait out the request timeout.
        assert_eq!(error.kind, McpErrorKind::Cancelled);
        assert!(
            cancelled_at.elapsed() < Duration::from_secs(5),
            "cancelling took {:?}",
            cancelled_at.elapsed()
        );
        // The registration is released, so the same dial can be probed again.
        begin_probe("probe-live-stdio").unwrap();
    }

    /// Prompts and resources are optional sections whose failures leave only that
    /// section empty. A cancellation must not disappear into that allowance.
    #[test]
    fn a_cancel_during_the_optional_prompts_phase_is_not_reported_as_success() {
        if !node_available() {
            return;
        }
        let temporary = tempfile::tempdir().unwrap();
        let (server, marker) = hanging_probe_server(&temporary, "prompts/list", true);
        let probe = thread::spawn(move || probe_server_for_settings(&server, "probe-live-prompts"));
        await_probe_request(&marker);

        cancel_probe("probe-live-prompts").unwrap();
        let (error, _logs) = probe.join().unwrap().unwrap_err();
        assert_eq!(error.kind, McpErrorKind::Cancelled);
    }
}
