//! Dev servers started for the preview pane: spawn, supervise, buffer, stop.
//!
//! A preview server is a long-lived child process the model asks for by name and
//! then never waits on. That makes it unlike every other child Mework starts: the
//! `bash` tool blocks its own worker thread and the terminal is driven by a human,
//! but `preview_start` has to return a *receipt* within a few seconds and leave the
//! process running behind it. So the startup gate here is a race, not a wait — three
//! seconds in which an exit means failure and silence means success — and everything
//! after it (readiness polling, log capture, exit detection) happens on detached
//! threads that report back through this registry.
//!
//! The registry does not read `.mework/launch.json`. It takes an already-resolved
//! [`PreviewServerConfig`] so that config discovery, its many failure messages, and
//! the port/url validation stay in one place and this module stays about processes.
//!
//! Model-facing text is copied byte for byte from the Claude Code desktop app,
//! except for the config path, which is `.mework/launch.json` here. The strings
//! are the product: they tell the model which of `autoPort`, a hardcoded `--port`
//! flag, a missing `cwd`, or another chat's server is the actual problem, and
//! paraphrasing one turns a diagnosis back into "it didn't start".

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::console_text::ConsoleTextDecoder;
use crate::tool_executor::{kill_process_tree_or_child, ShellJob};
use crate::host_platform::host_platform;

/// Dev servers one worktree may run at once.
pub const MAX_SERVERS_PER_WORKTREE: usize = 5;

/// Spawn attempts before a start gives up.
pub const MAX_SPAWN_ATTEMPTS: u32 = 3;

/// Spawn failures that are the configuration's fault rather than a transient. Retrying
/// a missing command three times only delays the message that says it is missing.
pub const NON_RETRYABLE_SPAWN_CODES: [&str; 4] = ["ENOENT", "EPERM", "EACCES", "ENOTDIR"];

/// Log chunks retained per server. One entry per read from the pipe, not per line.
pub const PREVIEW_LOG_CAPACITY: usize = 1000;

/// Host the readiness probe and the pane's URL both use.
pub const PREVIEW_HOST: &str = "localhost";

const PORT_PROBE_ATTEMPTS: u32 = 40;
const PORT_PROBE_ZERO_ATTEMPTS: u32 = 5;
const PORT_PROBE_RETRY: Duration = Duration::from_millis(100);
const PORT_CONNECT_TIMEOUT: Duration = Duration::from_millis(500);

const STARTUP_GATE: Duration = Duration::from_millis(3000);
const STARTUP_GATE_POLL: Duration = Duration::from_millis(25);
const SUPERVISOR_POLL: Duration = Duration::from_millis(200);

const READINESS_TIMEOUT: Duration = Duration::from_secs(60);
const READINESS_CONNECT_TIMEOUT: Duration = Duration::from_millis(1000);
const READINESS_TCP_RETRY: Duration = Duration::from_millis(200);
const READINESS_HTTP_TIMEOUT: Duration = Duration::from_millis(2000);
const READINESS_HTTP_RETRY: Duration = Duration::from_millis(300);

const DEFAULT_LOG_LINES: u32 = 50;
const MAX_LOG_LINES: u32 = 200;

/// Longest chain of executable rewrites (`foo` -> `foo.cmd` -> `cmd /C`, `x.js` -> `node`).
const MAX_COMMAND_RESOLVE_DEPTH: u32 = 4;

const PIPE_CHUNK: usize = 8192;

// ---------------------------------------------------------------------------
// Input configuration
// ---------------------------------------------------------------------------

/// One resolved launch configuration, as this registry needs it.
///
/// Deliberately not the launch.json parser's own type: everything here is already
/// resolved (variables substituted, url validated, port decided), so the wiring
/// layer converts once and the process code never has to know a config file exists.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PreviewServerConfig {
    pub name: String,
    /// `None` is an attach-only entry (url with no command). [`crate::preview::start`]
    /// answers those itself, before anything reaches this registry, so one that gets
    /// this far is an entry with no command *and* no url: [`PreviewServerRegistry::start`]
    /// refuses it rather than guessing.
    pub command: Option<String>,
    pub args: Vec<String>,
    /// Absolute, or relative to the worktree. Joined the way `path.resolve` joins.
    pub cwd: PathBuf,
    /// `0` asks the operating system to assign one.
    pub port: u16,
    pub env: BTreeMap<String, String>,
    /// Tri-state on purpose. `Some(false)` means this port and no other, `Some(true)`
    /// means any port, and `None` means the user never said — and each gets a
    /// different message when the port is taken.
    pub auto_port: Option<bool>,
    pub url: Option<String>,
}

// ---------------------------------------------------------------------------
// Snapshots
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PreviewServerStatus {
    Starting,
    Running,
    Stopped,
    Failed,
}

/// One server as `preview_list` and the pane see it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewServerSnapshot {
    pub server_id: String,
    pub name: String,
    pub port: u16,
    pub status: PreviewServerStatus,
    pub started_at: String,
    /// The worktree the server is registered under, not the process's own directory.
    pub cwd: String,
    pub session_id: Option<String>,
    /// The machine the server runs on, by name, when that is not this computer. Absent for a
    /// local server, so what the model reads about one is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    /// Where this computer reaches the server, when that is not `http://localhost:<port>`.
    /// The registry leaves it unset; the layer that knows how a machine is reached fills it in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Which of a conversation's workspaces the server belongs to, in a list that spans them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<u32>,
}

// ---------------------------------------------------------------------------
// Log ring buffer
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PreviewLogStream {
    Stdout,
    Stderr,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewLogEntry {
    /// A whole read from the pipe, newlines and all — not a single line, despite the
    /// name the source gave it. `preview_logs` joins entries with nothing for exactly
    /// this reason.
    pub line: String,
    pub stream: PreviewLogStream,
    pub timestamp: String,
}

/// Fixed-capacity buffer holding both pipes in arrival order.
#[derive(Clone, Debug)]
pub struct PreviewLogRing {
    max_size: usize,
    buffer: Vec<PreviewLogEntry>,
    head: usize,
}

impl PreviewLogRing {
    pub fn new() -> Self {
        Self::with_capacity(PREVIEW_LOG_CAPACITY)
    }

    pub fn with_capacity(max_size: usize) -> Self {
        Self {
            max_size: max_size.max(1),
            buffer: Vec::new(),
            head: 0,
        }
    }

    pub fn push(&mut self, line: String, stream: PreviewLogStream) {
        let entry = PreviewLogEntry {
            line,
            stream,
            timestamp: Utc::now().to_rfc3339(),
        };
        if self.buffer.len() < self.max_size {
            self.buffer.push(entry);
        } else {
            self.buffer[self.head] = entry;
            self.head = (self.head + 1) % self.max_size;
        }
    }

    /// Entries oldest first.
    pub fn to_vec(&self) -> Vec<PreviewLogEntry> {
        if self.buffer.len() < self.max_size {
            return self.buffer.clone();
        }
        let mut entries = Vec::with_capacity(self.buffer.len());
        entries.extend_from_slice(&self.buffer[self.head..]);
        entries.extend_from_slice(&self.buffer[..self.head]);
        entries
    }
}

impl Default for PreviewLogRing {
    fn default() -> Self {
        Self::new()
    }
}

/// What `preview_logs` asks for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PreviewLogQuery {
    /// `level: "error"`.
    pub errors_only: bool,
    /// Case-sensitive substring filter. An empty string means no filter.
    pub search: Option<String>,
    /// `None` is 50. Clamped to 1..=200.
    pub lines: Option<u32>,
}

/// The text `preview_logs` returns, including the empty-result wording.
pub fn render_preview_logs(entries: &[PreviewLogEntry], query: &PreviewLogQuery) -> String {
    let search = query.search.as_deref().filter(|search| !search.is_empty());
    let mut selected: Vec<&PreviewLogEntry> = entries
        .iter()
        .filter(|entry| {
            if !query.errors_only {
                return true;
            }
            let lowered = entry.line.to_lowercase();
            entry.stream == PreviewLogStream::Stderr
                && (lowered.contains("error")
                    || lowered.contains("exception")
                    || lowered.contains("failed")
                    || lowered.contains("fatal"))
        })
        .collect();
    if let Some(search) = search {
        selected.retain(|entry| entry.line.contains(search));
    }
    let limit = query
        .lines
        .unwrap_or(DEFAULT_LOG_LINES)
        .clamp(1, MAX_LOG_LINES) as usize;
    let tail = &selected[selected.len().saturating_sub(limit)..];
    if tail.is_empty() {
        if query.errors_only {
            return "No server errors found.".to_owned();
        }
        return match search {
            Some(search) => format!("No logs matching \"{search}\"."),
            None => "No logs yet.".to_owned(),
        };
    }
    // Entries are raw pipe chunks that already carry their own newlines.
    tail.iter().map(|entry| entry.line.as_str()).collect()
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewStartErrorKind {
    SpawnError,
    EarlyExit,
    /// The worktree already runs [`MAX_SERVERS_PER_WORKTREE`].
    Capacity,
    /// The machine the server was to run on could not be asked: its link is down, or its agent
    /// cannot run there. The link has already waited as long as it is worth waiting, so another
    /// attempt would only wait again.
    Unreachable,
}

/// A start that never produced a running server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewStartError {
    pub message: String,
    pub kind: PreviewStartErrorKind,
    pub code: Option<String>,
    pub exit_code: Option<i32>,
    pub output: Option<String>,
}

impl PreviewStartError {
    /// Whether another attempt could plausibly do better.
    pub fn is_retryable(&self) -> bool {
        if self.kind != PreviewStartErrorKind::SpawnError {
            return !matches!(
                self.kind,
                PreviewStartErrorKind::Capacity | PreviewStartErrorKind::Unreachable
            );
        }
        match &self.code {
            Some(code) => !NON_RETRYABLE_SPAWN_CODES.contains(&code.as_str()),
            None => true,
        }
    }
}

impl std::fmt::Display for PreviewStartError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PreviewStartError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortConflictSource {
    /// Another preview server this app started.
    Launch,
    /// Something outside the app.
    External,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortInUseError {
    pub port: u16,
    pub source: PortConflictSource,
    pub message: String,
}

impl PortInUseError {
    fn launch(port: u16, message: String) -> Self {
        Self {
            port,
            source: PortConflictSource::Launch,
            message,
        }
    }

    fn external(port: u16, message: String) -> Self {
        Self {
            port,
            source: PortConflictSource::External,
            message,
        }
    }
}

impl std::fmt::Display for PortInUseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PortInUseError {}

// ---------------------------------------------------------------------------
// Model-facing text
// ---------------------------------------------------------------------------

/// Quote-like characters folded to a plain apostrophe. Copied from Claude Code's
/// display sanitizer so a server name cannot forge quoting in a message the model
/// then reasons about.
const QUOTE_LIKE_PATTERN: &str = concat!(
    r"[`\x22\x{00a8}\x{00b4}\x{0374}\x{0384}\x{0385}\x{02b9}-\x{02bd}\x{02ca}\x{02cb}",
    r"\x{02dd}\x{02ee}\x{02f4}-\x{02f6}\x{05f3}\x{05f4}\x{07f4}\x{07f5}\x{1fbd}\x{1fbf}",
    r"\x{1fcd}-\x{1fcf}\x{1fdd}-\x{1fdf}\x{1fed}-\x{1fef}\x{1ffd}\x{1ffe}\x{201a}\x{201e}",
    r"\x{2032}-\x{2037}\x{2057}\x{275b}-\x{2760}\x{276e}\x{276f}\x{2e42}\x{3003}",
    r"\x{301d}-\x{301f}\x{ff02}\x{ff07}\x{ff40}\x{1f676}-\x{1f678}\p{Pi}\p{Pf}]",
);

/// Longest sanitized value before the ellipsis.
const SANITIZE_LIMIT: usize = 120;

fn quote_like() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(QUOTE_LIKE_PATTERN).expect("quote-like class compiles"))
}

fn invisible() -> &'static Regex {
    // `\p{Cs}` is in the source's class and omitted here: a Rust `str` cannot hold a
    // lone surrogate, so the class would never match.
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"[\p{Cc}\p{Cf}]").expect("invisible class compiles"))
}

/// One line of untrusted text, safe to interpolate into a message for the model.
///
/// Any change at all — a folded quote as much as a truncation — earns the trailing
/// ellipsis, which is what tells a reader the value is not verbatim.
pub fn sanitize_message_text(value: &str) -> String {
    let first_line = value
        .split([
            '\r', '\n', '\u{000b}', '\u{000c}', '\u{0085}', '\u{2028}', '\u{2029}',
        ])
        .next()
        .unwrap_or_default();
    let folded = quote_like().replace_all(first_line, "'");
    let cleaned = invisible().replace_all(&folded, "\u{fffd}");
    let truncated = if cleaned.chars().count() > SANITIZE_LIMIT {
        cleaned.chars().take(SANITIZE_LIMIT).collect::<String>()
    } else {
        cleaned.clone().into_owned()
    };
    if truncated == value {
        return truncated;
    }
    format!("{truncated}\u{2026}")
}

/// What to do when the port is taken and `autoPort` was never decided.
pub fn auto_port_hint(port: u16) -> String {
    format!(
        "Ask the user: does this server need port {port} specifically (e.g. for OAuth callbacks, \
         webhooks, or CORS)? If yes, set \"autoPort\": false in .mework/launch.json and free port \
         {port}. If no, set \"autoPort\": true in .mework/launch.json AND check the start command \
         for hardcoded port flags (e.g. --port, -p) \u{2014} remove them so the server uses the \
         assigned port via the PORT environment variable. Then retry."
    )
}

/// `autoPort: false` — this port and no other.
pub fn port_required_message(
    port: u16,
    occupant: Option<&PreviewServerSnapshot>,
    occupant_text: Option<&str>,
) -> String {
    if let Some(occupant) = occupant {
        let name = sanitize_message_text(&occupant.name);
        let server_id = &occupant.server_id;
        return format!(
            "Port {port} is required by this server (autoPort is false) but is in use by preview \
             server \"{name}\" ({server_id}). Ask the user if they want to stop \"{name}\" to free \
             port {port}. If yes, call preview_stop with serverId \"{server_id}\" and retry."
        );
    }
    if let Some(occupant_text) = occupant_text {
        return format!(
            "Port {port} is required by this server but is in use by {occupant_text}. Stop that \
             process to free port {port} and try again."
        );
    }
    format!(
        "Port {port} is required by this server but is in use by another process. Run \
         `lsof -i :{port}` to find what's using it, then free port {port} and try again."
    )
}

/// The port is held by a preview server this app started.
///
/// `auto_port == Some(true)` never reaches here — that case reassigns instead of
/// refusing — and is treated as the undecided branch if it somehow does.
pub fn preview_port_conflict(
    port: u16,
    auto_port: Option<bool>,
    occupant: Option<&PreviewServerSnapshot>,
    cross_session: bool,
) -> PortInUseError {
    if cross_session {
        let name = occupant.map(|occupant| sanitize_message_text(&occupant.name));
        let name = name.as_deref().unwrap_or_default();
        let tail = if auto_port == Some(false) {
            "Ask the user to stop it from that chat, or to change \"autoPort\" in \
             .mework/launch.json so this session can use a different port."
                .to_owned()
        } else {
            auto_port_hint(port)
        };
        return PortInUseError::launch(
            port,
            format!(
                "Port {port} is in use by another chat's dev server \"{name}\". preview_stop won't \
                 stop another chat's server. {tail}"
            ),
        );
    }
    if auto_port == Some(false) {
        return PortInUseError::launch(port, port_required_message(port, occupant, None));
    }
    let prefix = match occupant {
        Some(occupant) => format!(
            "Port {port} is in use by preview server \"{}\" ({}). ",
            sanitize_message_text(&occupant.name),
            occupant.server_id
        ),
        None => format!("Port {port} is in use by another preview server. "),
    };
    PortInUseError::launch(port, format!("{prefix}{}", auto_port_hint(port)))
}

/// The port is held by something this app did not start.
pub fn external_port_conflict(
    port: u16,
    auto_port: Option<bool>,
    occupant_text: Option<&str>,
) -> PortInUseError {
    if auto_port == Some(false) {
        return PortInUseError::external(port, port_required_message(port, None, occupant_text));
    }
    let prefix = match occupant_text {
        Some(occupant_text) => {
            format!("Port {port} is in use by {occupant_text} (not a preview server). ")
        }
        None => format!(
            "Port {port} is in use by another process (not a preview server). Run \
             `lsof -i :{port}` to identify what's using it. "
        ),
    };
    PortInUseError::external(port, format!("{prefix}{}", auto_port_hint(port)))
}

/// The kernel refuses the bind outright, which no amount of stopping things fixes.
pub fn os_reserved_port_message(port: u16) -> String {
    let reason = if host_platform().is_windows() {
        "a Windows excluded port range, or a privileged port"
    } else {
        "a privileged port below 1024"
    };
    format!(
        "Port {port} is reserved by the OS ({reason}) and cannot be bound. Pick a different port \
         in .mework/launch.json, or set \"autoPort\": true to use an OS-assigned port."
    )
}

/// `autoPort: true`, but even a fresh OS-assigned port could not be reserved.
fn auto_port_reassignment_failed(
    port: u16,
    occupant: Option<&PreviewServerSnapshot>,
    occupant_text: Option<&str>,
) -> PortInUseError {
    if let Some(occupant) = occupant {
        let name = sanitize_message_text(&occupant.name);
        let server_id = &occupant.server_id;
        return PortInUseError::launch(
            port,
            format!(
                "Port {port} is in use by preview server \"{name}\" ({server_id}) and automatic \
                 reassignment to a fresh port failed. Retry in a moment, or call preview_stop with \
                 serverId \"{server_id}\" to free port {port} and retry."
            ),
        );
    }
    let by = match occupant_text {
        Some(occupant_text) => format!(" by {occupant_text}"),
        None => String::new(),
    };
    PortInUseError::external(
        port,
        format!(
            "Port {port} is in use{by} and automatic port reassignment failed. Find and stop \
             whatever is using port {port}, then try again."
        ),
    )
}

/// A `reuse` decision whose server died between the decision and the start.
pub fn dead_reuse_refusal_message(name: &str) -> String {
    format!(
        "The \"{}\" server is no longer running. Call preview_start again to start it.",
        sanitize_message_text(name)
    )
}

/// The resolved configuration had no command to run.
///
/// An entry that names a url instead of a command is not this case: it attaches,
/// and [`crate::preview::start`] decides that before a start reaches here.
pub const NO_COMMAND_MESSAGE: &str =
    "The resolved launch configuration has no command. Check the configuration's \
     \"runtimeExecutable\" or \"program\" field in .mework/launch.json, then try again.";

// ---------------------------------------------------------------------------
// Capacity
// ---------------------------------------------------------------------------

/// Refuses a sixth server, naming the other chats' servers when there are any —
/// "stop one first" is unhelpful advice if the ones you can see are not yours.
pub fn capacity_error(
    existing: &[PreviewServerSnapshot],
    session_id: Option<&str>,
) -> Option<PreviewStartError> {
    if existing.len() < MAX_SERVERS_PER_WORKTREE {
        return None;
    }
    let others = existing
        .iter()
        .filter(|server| {
            server
                .session_id
                .as_deref()
                .is_some_and(|owner| Some(owner) != session_id)
        })
        .count();
    let message = if session_id.is_some() && others > 0 {
        format!(
            "Maximum {MAX_SERVERS_PER_WORKTREE} dev servers per folder reached; {others} belong to \
             other chats. Stop one of this chat's servers, or ask the user to stop one from the \
             other chat."
        )
    } else {
        format!("Maximum {MAX_SERVERS_PER_WORKTREE} servers per worktree. Stop one first.")
    };
    Some(PreviewStartError {
        message,
        kind: PreviewStartErrorKind::Capacity,
        code: None,
        exit_code: None,
        output: None,
    })
}

// ---------------------------------------------------------------------------
// Reuse
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewReuseReason {
    /// The running server answers to the requested configuration's name.
    NameMatch,
    /// It holds the configuration's port, and the caller named nothing.
    PortMatch,
    /// It is the only server running, from the only configuration, and the caller
    /// named nothing — so whatever it is, it is what they meant.
    SingleRunning,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreviewStartAction {
    Reuse {
        server: PreviewServerSnapshot,
        reason: PreviewReuseReason,
    },
    Start,
}

/// Servers a session is allowed to reuse: its own, plus any with no owner.
pub fn running_for_session(
    servers: &[PreviewServerSnapshot],
    session_id: Option<&str>,
) -> Vec<PreviewServerSnapshot> {
    servers
        .iter()
        .filter(|server| {
            matches!(
                server.status,
                PreviewServerStatus::Running | PreviewServerStatus::Starting
            ) && (session_id.is_none()
                || server.session_id.is_none()
                || server.session_id.as_deref() == session_id)
        })
        .cloned()
        .collect()
}

/// Whether to reuse a running server instead of starting another.
///
/// `running` must already be narrowed by [`running_for_session`]. `requested_name` is
/// what the *caller* passed, not the configuration's name: the port and single-server
/// arms only apply when the caller named nothing, because naming a server is a request
/// for that server.
pub fn decide_start_action(
    running: &[PreviewServerSnapshot],
    config_name: &str,
    config_port: u16,
    requested_name: Option<&str>,
    configured_server_count: usize,
) -> PreviewStartAction {
    if running.is_empty() {
        return PreviewStartAction::Start;
    }
    let named = requested_name.is_some_and(|name| !name.is_empty());
    if let Some(server) = running
        .iter()
        .find(|server| server.name.eq_ignore_ascii_case(config_name))
    {
        return PreviewStartAction::Reuse {
            server: server.clone(),
            reason: PreviewReuseReason::NameMatch,
        };
    }
    if !named {
        if let Some(server) = running.iter().find(|server| server.port == config_port) {
            return PreviewStartAction::Reuse {
                server: server.clone(),
                reason: PreviewReuseReason::PortMatch,
            };
        }
        if running.len() == 1 && configured_server_count == 1 {
            return PreviewStartAction::Reuse {
                server: running[0].clone(),
                reason: PreviewReuseReason::SingleRunning,
            };
        }
    }
    PreviewStartAction::Start
}

// ---------------------------------------------------------------------------
// Spawn-failure classification
// ---------------------------------------------------------------------------

/// What went wrong before the server was considered started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreviewSpawnFailure {
    SpawnError {
        code: Option<String>,
        error: String,
        /// The classification came from parsing the child's own exit text rather
        /// than from an errno, which changes the advice: the file was found and
        /// something downstream of it failed.
        via_exit_text: bool,
    },
    EarlyExit {
        exit_code: Option<i32>,
        error: Option<String>,
    },
}

/// A launcher that could not exec its target exits 127 and says so on stderr. The
/// errno never reaches this process, so it is recovered from the text.
pub fn reclassify_exit_127(failure: PreviewSpawnFailure) -> PreviewSpawnFailure {
    let PreviewSpawnFailure::EarlyExit { exit_code, error } = &failure else {
        return failure;
    };
    if *exit_code != Some(127) {
        return failure;
    }
    let text = error.as_deref().unwrap_or_default();
    let Some(index) = text.find("Failed to spawn process: ") else {
        return failure;
    };
    let detail = text["Failed to spawn process: ".len() + index..]
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned();
    if detail.is_empty() {
        return failure;
    }
    let code = if detail.contains("No such file") {
        Some("ENOENT")
    } else if detail.contains("Permission denied") {
        Some("EACCES")
    } else if detail.contains("Operation not permitted") {
        Some("EPERM")
    } else if detail.contains("Not a directory") {
        Some("ENOTDIR")
    } else {
        None
    };
    PreviewSpawnFailure::SpawnError {
        code: code.map(str::to_owned),
        error: detail,
        via_exit_text: true,
    }
}

/// Turns a failure into the message the model acts on.
///
/// `output` is everything the process wrote before it died; `cwd_missing` disambiguates
/// the one errno that means two different things.
pub fn preview_start_failure(
    failure: PreviewSpawnFailure,
    command: &str,
    output: &str,
    cwd_missing: bool,
) -> PreviewStartError {
    let failure = reclassify_exit_127(failure);
    let command = sanitize_message_text(command);
    let retained = (!output.is_empty()).then(|| output.to_owned());
    match failure {
        PreviewSpawnFailure::SpawnError {
            code,
            error: _,
            via_exit_text,
        } => {
            let suffix = match &code {
                Some(code) => format!(" ({code})"),
                None => String::new(),
            };
            let message = match code.as_deref() {
                Some("ENOENT") if cwd_missing => "The working directory set by `cwd` does not \
                     exist \u{2014} spawn reports this as ENOENT, the same errno as a missing \
                     command. Check the `cwd` field in .mework/launch.json (the folder may have \
                     been moved or renamed)."
                    .to_owned(),
                Some("ENOENT") => format!(
                    "Command not found: `{command}`. Check the `command`/`runtimeExecutable` \
                     field in .mework/launch.json and make sure it's installed and on PATH."
                ),
                Some(code @ ("EPERM" | "EACCES")) => {
                    if via_exit_text {
                        format!(
                            "Permission denied starting `{command}` ({code}). Check the file's \
                             permissions \u{2014} the command may not be executable."
                        )
                    } else {
                        let detail = if host_platform().is_macos() && code == "EPERM" {
                            " macOS privacy protection likely blocked access to this folder. \
                             Grant access in System Settings > Privacy & Security > Files and \
                             Folders, or move the project outside Documents/Desktop/Downloads."
                        } else {
                            " Check the file's permissions \u{2014} the command may not be \
                             executable."
                        };
                        format!(
                            "Permission denied starting `{command}` ({code}).{detail} The command \
                             in .mework/launch.json is likely fine \u{2014} don't edit it."
                        )
                    }
                }
                _ => {
                    let head = if via_exit_text {
                        format!(
                            "Could not start `{command}`{suffix} \u{2014} see the output below."
                        )
                    } else {
                        format!("Could not start `{command}`{suffix}.")
                    };
                    // Only this branch attaches the output; a missing command or a
                    // permission error has nothing useful in the child's stream.
                    match &retained {
                        Some(output) => format!("{head}\n\nOutput:\n{output}"),
                        None => head,
                    }
                }
            };
            PreviewStartError {
                message,
                kind: PreviewStartErrorKind::SpawnError,
                code,
                exit_code: None,
                output: retained,
            }
        }
        PreviewSpawnFailure::EarlyExit { exit_code, error } => {
            let head = match exit_code {
                None => "The dev server exited unexpectedly during startup.".to_owned(),
                Some(exit_code) => {
                    format!("The dev server exited during startup (code {exit_code}).")
                }
            };
            let detail = retained.clone().or(error);
            let message = match detail {
                Some(detail) if !detail.is_empty() => format!(
                    "{head} Fix the error in the output below, then start the server again.\n\n\
                     Output:\n{detail}"
                ),
                _ => head,
            };
            PreviewStartError {
                message,
                kind: PreviewStartErrorKind::EarlyExit,
                code: None,
                exit_code,
                output: retained,
            }
        }
    }
}

/// Errno spelling for a spawn failure, in the vocabulary the messages use.
fn spawn_error_code(error: &std::io::Error) -> Option<String> {
    #[cfg(windows)]
    {
        // ERROR_FILE_NOT_FOUND / ERROR_PATH_NOT_FOUND / ERROR_ACCESS_DENIED / ERROR_DIRECTORY.
        match error.raw_os_error() {
            Some(2) | Some(3) => return Some("ENOENT".to_owned()),
            Some(5) => return Some("EACCES".to_owned()),
            Some(267) => return Some("ENOTDIR".to_owned()),
            _ => {}
        }
    }
    #[cfg(unix)]
    {
        match error.raw_os_error() {
            Some(libc::ENOENT) => return Some("ENOENT".to_owned()),
            Some(libc::EPERM) => return Some("EPERM".to_owned()),
            Some(libc::EACCES) => return Some("EACCES".to_owned()),
            Some(libc::ENOTDIR) => return Some("ENOTDIR".to_owned()),
            _ => {}
        }
    }
    match error.kind() {
        std::io::ErrorKind::NotFound => Some("ENOENT".to_owned()),
        std::io::ErrorKind::PermissionDenied => Some("EACCES".to_owned()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Executable resolution
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedPreviewCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
}

/// Finds the real thing to run for a launch entry's `command`.
///
/// Not `mcp::resolve_windows_stdio_command`: that one deliberately refuses `.ps1`
/// and `.js` and treats anything unresolvable as an error, both of which are right
/// for an MCP server and wrong here — a launch entry may legitimately name a
/// PowerShell script, and `npm` on Windows is `npm.cmd`, which has to become
/// `cmd /C npm.cmd` because a batch file is not an executable image.
pub fn resolve_preview_command(
    command: &str,
    args: &[String],
    path_directories: &[PathBuf],
) -> ResolvedPreviewCommand {
    resolve_command_with(
        command,
        args,
        path_directories,
        host_platform().is_windows(),
        &system_root(),
        &|path| {
            std::fs::metadata(path)
                .map(|metadata| metadata.is_file())
                .unwrap_or(false)
        },
        0,
    )
}

fn system_root() -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:\\Windows"))
}

fn has_path_separator(value: &str) -> bool {
    value.contains('\\') || value.contains('/')
}

fn path_candidates(
    command: &str,
    path_directories: &[PathBuf],
    exists: &dyn Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    if has_path_separator(command) {
        return vec![PathBuf::from(command)];
    }
    let found: Vec<PathBuf> = path_directories
        .iter()
        .map(|directory| directory.join(command))
        .filter(|candidate| exists(candidate))
        .collect();
    if found.is_empty() {
        vec![PathBuf::from(command)]
    } else {
        found
    }
}

fn resolve_command_with(
    command: &str,
    args: &[String],
    path_directories: &[PathBuf],
    windows: bool,
    system_root: &Path,
    exists: &dyn Fn(&Path) -> bool,
    depth: u32,
) -> ResolvedPreviewCommand {
    if !windows {
        let mut candidates = path_candidates(command, path_directories, exists);
        return ResolvedPreviewCommand {
            program: candidates.remove(0),
            args: args.to_vec(),
        };
    }
    if depth < MAX_COMMAND_RESOLVE_DEPTH
        && (!has_path_separator(command) || !exists(Path::new(command)))
    {
        for extension in [".exe", ".bat", ".cmd", ".ps1"] {
            let spelled = format!("{command}{extension}");
            for candidate in path_candidates(&spelled, path_directories, exists) {
                let candidate = candidate.to_string_lossy().into_owned();
                if has_path_separator(&candidate) && exists(Path::new(&candidate)) {
                    return resolve_command_with(
                        &candidate,
                        args,
                        path_directories,
                        windows,
                        system_root,
                        exists,
                        depth + 1,
                    );
                }
            }
        }
    }
    let lowered = command.to_ascii_lowercase();
    if lowered.ends_with(".ps1") {
        let mut resolved = vec![
            "-ExecutionPolicy".to_owned(),
            "Unrestricted".to_owned(),
            "-NoLogo".to_owned(),
            "-NonInteractive".to_owned(),
            "-File".to_owned(),
            command.to_owned(),
        ];
        resolved.extend(args.iter().cloned());
        return ResolvedPreviewCommand {
            program: system_root
                .join("System32")
                .join("WindowsPowerShell")
                .join("v1.0")
                .join("PowerShell.exe"),
            args: resolved,
        };
    }
    if lowered.ends_with(".bat") || lowered.ends_with(".cmd") {
        let mut resolved = vec!["/C".to_owned(), command.to_owned()];
        resolved.extend(args.iter().cloned());
        return ResolvedPreviewCommand {
            program: system_root.join("System32").join("cmd.exe"),
            args: resolved,
        };
    }
    if lowered.ends_with(".js") && depth < MAX_COMMAND_RESOLVE_DEPTH {
        let mut node_args = vec![command.to_owned()];
        node_args.extend(args.iter().cloned());
        return resolve_command_with(
            "node",
            &node_args,
            path_directories,
            windows,
            system_root,
            exists,
            depth + 1,
        );
    }
    ResolvedPreviewCommand {
        program: PathBuf::from(command),
        args: args.to_vec(),
    }
}

/// `PATH` the child will actually search, which is also what the executable lookup
/// must search. A configured `PATH` in the entry's `env` replaces the inherited one.
fn preview_path_directories(env: &BTreeMap<String, String>) -> Vec<PathBuf> {
    let configured = env
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("PATH"))
        .map(|(_, value)| OsString::from(value))
        .or_else(|| std::env::var_os("PATH"))
        .unwrap_or_default();
    std::env::split_paths(&configured)
        .filter(|directory| !directory.as_os_str().is_empty())
        .collect()
}

/// Rewrites `--port`/`-p` arguments after `autoPort` moved the server elsewhere.
/// A start script that hardcodes the flag would otherwise ignore `PORT` and bind
/// the port we just decided not to use.
pub fn rewrite_port_arguments(args: &[String], port: u16) -> Vec<String> {
    let mut rewritten = args.to_vec();
    let mut index = 0;
    while index < rewritten.len() {
        let argument = rewritten[index].clone();
        if (argument == "--port" || argument == "-p") && index + 1 < rewritten.len() {
            rewritten[index + 1] = port.to_string();
            index += 2;
            continue;
        }
        if let Some(equals) = argument
            .starts_with("--port=")
            .then_some("--port=")
            .or_else(|| argument.starts_with("-p=").then_some("-p="))
        {
            rewritten[index] = format!("{equals}{port}");
        }
        index += 1;
    }
    rewritten
}

// ---------------------------------------------------------------------------
// Port probing
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
enum PortProbeFailure {
    InUse,
    Denied,
    Other(String),
}

/// Binds and immediately releases, which is the only way to learn whether a port is
/// free without asking the kernel a question it answers differently per interface.
fn bind_probe(port: u16) -> Result<u16, PortProbeFailure> {
    match TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
        Ok(listener) => match listener.local_addr() {
            Ok(address) => Ok(address.port()),
            Err(error) => Err(PortProbeFailure::Other(error.to_string())),
        },
        Err(error) => Err(match error.kind() {
            std::io::ErrorKind::AddrInUse => PortProbeFailure::InUse,
            std::io::ErrorKind::PermissionDenied => PortProbeFailure::Denied,
            _ => PortProbeFailure::Other(error.to_string()),
        }),
    }
}

/// True when something answers on this address.
fn connect_probe(port: u16, host: IpAddr, timeout: Duration) -> bool {
    TcpStream::connect_timeout(&SocketAddr::new(host, port), timeout).is_ok()
}

/// Reserves a port, or reports why not.
///
/// The bind alone is not enough: a server bound to `[::]` on a dual-stack box leaves
/// `127.0.0.1` bindable while still owning the port for everything that connects by
/// name, so a successful bind is followed by connect probes on both loopbacks and a
/// live answer is treated as the `EADDRINUSE` the bind should have produced.
fn reserve_port(port: u16, attempts: Option<u32>) -> Result<u16, PortProbeFailure> {
    let attempts = attempts.unwrap_or(if port == 0 {
        PORT_PROBE_ZERO_ATTEMPTS
    } else {
        PORT_PROBE_ATTEMPTS
    });
    let mut last = PortProbeFailure::InUse;
    for attempt in 0..attempts.max(1) {
        if attempt > 0 {
            thread::sleep(PORT_PROBE_RETRY);
        }
        let bound = match bind_probe(port) {
            Ok(bound) => bound,
            Err(PortProbeFailure::InUse) => {
                last = PortProbeFailure::InUse;
                continue;
            }
            Err(other) => return Err(other),
        };
        let squatted = [
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
        ]
        .into_iter()
        .any(|host| connect_probe(bound, host, PORT_CONNECT_TIMEOUT));
        if squatted {
            last = PortProbeFailure::InUse;
            continue;
        }
        return Ok(bound);
    }
    Err(last)
}

// ---------------------------------------------------------------------------
// Port occupant lookup
// ---------------------------------------------------------------------------

/// Local addresses that count as "this machine's loopback" for the occupant lookup.
fn is_loopback_listener_address(address: &str) -> bool {
    let lowered = address.to_ascii_lowercase();
    lowered == "::1"
        || lowered == "::"
        || lowered == "0.0.0.0"
        || lowered.starts_with("127.")
        || lowered.starts_with("::ffff:127.")
}

/// Splits `127.0.0.1:3000` / `[::1]:3000` into address and port.
fn split_listener_address(value: &str) -> Option<(String, u16)> {
    let (address, port) = if let Some(rest) = value.strip_prefix('[') {
        let close = rest.find("]:")?;
        (rest[..close].to_owned(), &rest[close + 2..])
    } else {
        let colon = value.rfind(':')?;
        (value[..colon].to_owned(), &value[colon + 1..])
    };
    Some((address, port.parse().ok()?))
}

/// Names whatever holds the port, so the message can say "node (PID 1234)" instead of
/// "another process".
///
/// Read through `netstat`/`tasklist` rather than `GetExtendedTcpTable`, because the
/// crate's `windows-sys` feature list is deliberately closed and IP Helper is not on
/// it. Only ever reached on the error path, where two short child processes cost less
/// than an unactionable message.
pub fn port_occupant_description(port: u16) -> Option<String> {
    let listeners = tcp_listeners_on_port(port);
    if listeners.is_empty() {
        return None;
    }
    let chosen = listeners
        .iter()
        .find(|(pid, _)| *pid != 0)
        .unwrap_or(&listeners[0]);
    let name = process_name(chosen.0)
        .map(|name| {
            name.chars()
                .filter(|character| {
                    character.is_ascii_alphanumeric()
                        || matches!(character, '.' | '_' | '+' | '-' | ' ')
                })
                .take(32)
                .collect::<String>()
                .trim()
                .to_owned()
        })
        .filter(|name| !name.is_empty());
    let prefix = match &name {
        Some(name) => format!("\"{name}\" "),
        None => String::new(),
    };
    let identity = if chosen.0 == 0 {
        "unknown PID".to_owned()
    } else {
        format!("PID {}", chosen.0)
    };
    let mut distinct: Vec<u32> = listeners.iter().map(|(pid, _)| *pid).collect();
    distinct.sort_unstable();
    distinct.dedup();
    let others = distinct.len().saturating_sub(1);
    let tail = if others > 0 {
        format!(" and {others} other process(es)")
    } else {
        String::new()
    };
    Some(format!("{prefix}({identity}){tail}"))
}

fn tcp_listeners_on_port(port: u16) -> Vec<(u32, String)> {
    let Some(output) = run_system_tool("netstat.exe", &["-ano"], Duration::from_secs(3)) else {
        return Vec::new();
    };
    let mut listeners = Vec::new();
    for line in output.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        // `TCP <local> <foreign> <state> <pid>`. The state word is not relied on; a
        // listener is the row whose foreign address is the wildcard, which no locale
        // translates.
        if fields.len() != 5 || !fields[0].eq_ignore_ascii_case("TCP") {
            continue;
        }
        if fields[2] != "0.0.0.0:0" && fields[2] != "[::]:0" && fields[2] != "*:*" {
            continue;
        }
        let Some((address, local_port)) = split_listener_address(fields[1]) else {
            continue;
        };
        if local_port != port || !is_loopback_listener_address(&address) {
            continue;
        }
        let Ok(pid) = fields[4].parse::<u32>() else {
            continue;
        };
        listeners.push((pid, address));
    }
    listeners
}

fn process_name(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    let filter = format!("PID eq {pid}");
    let output = run_system_tool(
        "tasklist.exe",
        &["/FI", filter.as_str(), "/FO", "CSV", "/NH"],
        Duration::from_secs(3),
    )?;
    let first = output.lines().find(|line| line.starts_with('"'))?;
    first.split('"').nth(1).map(str::to_owned)
}

/// Runs a Windows system tool from `System32`, never from `PATH`: under the dev
/// launcher `PATH` leads with MSYS2 and a bare name can resolve to a shell script.
fn run_system_tool(name: &str, args: &[&str], timeout: Duration) -> Option<String> {
    #[cfg(not(windows))]
    {
        let _ = (name, args, timeout);
        None
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use wait_timeout::ChildExt;

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let program = system_root().join("System32").join(name);
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .ok()?;
        let mut pipe = child.stdout.take()?;
        let reader = thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = pipe.read_to_end(&mut bytes);
            bytes
        });
        match child.wait_timeout(timeout) {
            Ok(Some(_)) => {}
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
        let bytes = reader.join().ok()?;
        Some(crate::console_text::decode_console_text(&bytes))
    }
}

// ---------------------------------------------------------------------------
// Readiness
// ---------------------------------------------------------------------------

/// Waits for the server to answer, up to `timeout`, abandoned early when `keep_going`
/// says the server it was started for no longer exists. Without it a stopped server
/// leaves a thread probing a dead port for the rest of the minute.
///
/// Two stages, because they fail differently: a refused TCP connection means the
/// process has not bound yet, while a bound-but-silent port means the framework is
/// still compiling. Only the HTTP answer proves it is serving.
///
/// Builds its own blocking HTTP client and must therefore run on a plain thread, not
/// inside the async runtime.
fn wait_until_ready_while(
    port: u16,
    timeout: Duration,
    https: bool,
    keep_going: &dyn Fn() -> bool,
) -> bool {
    let scheme = if https { "https" } else { "http" };
    let url = format!("{scheme}://{PREVIEW_HOST}:{port}");
    let client = reqwest::blocking::Client::builder()
        .timeout(READINESS_HTTP_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build();
    let Ok(client) = client else {
        return false;
    };
    let started = Instant::now();
    while started.elapsed() < timeout {
        if !keep_going() {
            return false;
        }
        if !connect_to_host(port, READINESS_CONNECT_TIMEOUT) {
            thread::sleep(READINESS_TCP_RETRY);
            continue;
        }
        match client.head(&url).send() {
            Ok(_) => return true,
            Err(error) => {
                // A self-signed dev certificate is still a server that answered.
                if https && mentions_tls(&error_chain(&error)) {
                    return true;
                }
            }
        }
        thread::sleep(READINESS_HTTP_RETRY);
    }
    false
}

fn connect_to_host(port: u16, timeout: Duration) -> bool {
    let Ok(addresses) = (PREVIEW_HOST, port).to_socket_addrs() else {
        return false;
    };
    addresses
        .into_iter()
        .any(|address| TcpStream::connect_timeout(&address, timeout).is_ok())
}

fn error_chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(current) = source {
        text.push_str("; ");
        text.push_str(&current.to_string());
        source = current.source();
    }
    text
}

fn mentions_tls(text: &str) -> bool {
    let lowered = text.to_lowercase();
    lowered.contains("cert") || lowered.contains("ssl") || lowered.contains("tls")
}

/// Whether the entry's url asks for https on the loopback, which is the only case
/// where the readiness probe should speak TLS.
fn readiness_is_https(url: Option<&str>) -> bool {
    let Some(url) = url else { return false };
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    if parsed.scheme() != "https" {
        return false;
    }
    matches!(
        parsed.host_str(),
        Some("localhost") | Some("127.0.0.1") | Some("::1") | Some("[::1]")
    ) || parsed
        .host_str()
        .is_some_and(|host| host.ends_with(".localhost"))
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// A server on another machine: where it runs, and everything the registry has to ask that machine
/// for. Implemented over the machine's agent ([`crate::preview_remote`]); the registry itself only
/// keeps the books, exactly as it does for a process of its own.
pub trait RemoteServerHost: Send + Sync {
    /// The machine's environment key, which is what "the same machine" means for ports.
    fn machine_key(&self) -> &str;
    /// The machine's name as the user knows it.
    fn machine_label(&self) -> &str;
    /// Starts the configured command there, in `config.cwd`, with `PORT` and the entry's variables.
    fn spawn(
        &self,
        config: &PreviewServerConfig,
    ) -> Result<remote_agent::client::RemoteProcess, PreviewStartError>;
    /// Whether `port` could be bound there, and whether something already answers on it.
    fn port_state(&self, port: u16) -> Result<(bool, bool), String>;
    /// A port that machine's system hands out as free.
    fn free_port(&self) -> Result<u16, String>;
    /// Waits, there, until the server answers; the polling never crosses the network.
    fn wait_ready(&self, port: u16, timeout: Duration, https: bool) -> bool;
}

/// Which machine a registered server runs on.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ServerMachine {
    key: String,
    label: String,
}

/// A process the agent on another machine runs for this registry.
struct RemoteServerProcess {
    process: remote_agent::client::RemoteProcess,
    stopped: AtomicBool,
}

/// What a registered server's process is.
#[derive(Clone)]
enum ServerProcess {
    Local(Arc<Mutex<PreviewProcess>>),
    Remote(Arc<RemoteServerProcess>),
}

/// A running child and the job object that can kill everything it started.
struct PreviewProcess {
    child: std::process::Child,
    job: ShellJob,
    /// Someone already killed and reaped this tree. The supervisor uses it to tell a
    /// deliberate stop from an exit it must report.
    stopped: bool,
}

struct ServerEntry {
    server_id: String,
    name: String,
    port: u16,
    status: PreviewServerStatus,
    started_at: String,
    session_id: Option<String>,
    process: ServerProcess,
    /// `None` on this computer.
    machine: Option<ServerMachine>,
    /// The directory reported as `cwd` when the registry key is not a path on this computer.
    display_cwd: Option<String>,
}

impl ServerEntry {
    fn snapshot(&self, worktree: &Path) -> PreviewServerSnapshot {
        PreviewServerSnapshot {
            server_id: self.server_id.clone(),
            name: self.name.clone(),
            port: self.port,
            status: self.status,
            started_at: self.started_at.clone(),
            cwd: self
                .display_cwd
                .clone()
                .unwrap_or_else(|| worktree.to_string_lossy().into_owned()),
            session_id: self.session_id.clone(),
            machine: self.machine.as_ref().map(|machine| machine.label.clone()),
            url: None,
            workspace: None,
        }
    }

    fn machine_key(&self) -> Option<&str> {
        self.machine.as_ref().map(|machine| machine.key.as_str())
    }
}

#[derive(Default)]
struct Registry {
    worktrees: HashMap<PathBuf, HashMap<String, ServerEntry>>,
    logs: HashMap<String, PreviewLogRing>,
    /// Ids of servers that have been stopped. They are never minted again.
    ///
    /// A `preview_start` receipt naming its `serverId` is persisted in the conversation, so a
    /// reused id would let a model read an old receipt and address a different server than the one
    /// that receipt reported. It also closes a race: `stop` frees the id before the process dies,
    /// and a log-drain thread still holding that id would otherwise append the dead process's last
    /// bytes to whatever ring now sits under it. The set is bounded by how many servers one app
    /// session starts, and the registry itself does not outlive the process.
    retired_ids: HashSet<String>,
}

impl Registry {
    /// The id the model addresses this server by: its launch configuration's own name.
    ///
    /// That name is the one thing the model already knows — it is what it passed to
    /// `preview_start` — and it has to quote the id back on every later preview call. A uuid would
    /// spend 36 characters on each of those calls to say nothing.
    ///
    /// Ids have to be free across the whole registry rather than within one worktree, because
    /// `logs` is a flat map and both `get` and `stop` are given an id alone. Two worktrees can
    /// hold the same configuration name, and `autoPort` lets a second conversation start another
    /// process under it, so a name already in use is numbered the way a workflow run's is. The
    /// loop terminates because the taken set is finite: every entry in it owns a live process.
    fn mint_server_id(&self, name: &str) -> String {
        let base = slug_server_id(name);
        let mut candidate = base.clone();
        let mut number = 1u32;
        while self.server_id_taken(&candidate) {
            number += 1;
            candidate = format!("{base}-{number}");
        }
        candidate
    }

    fn server_id_taken(&self, server_id: &str) -> bool {
        self.retired_ids.contains(server_id)
            || self.logs.contains_key(server_id)
            || self
                .worktrees
                .values()
                .any(|entries| entries.contains_key(server_id))
    }

    /// Retires an id so nothing is ever started under it again.
    fn retire(&mut self, server_id: &str) {
        self.retired_ids.insert(server_id.to_owned());
    }
}

/// Reduces a launch configuration name to something usable as an id: the model reads it, quotes it
/// back, and the host uses it as a map key, so spaces, case and punctuation all have to go. A name
/// with nothing usable in it still needs an id, hence the fallback.
fn slug_server_id(name: &str) -> String {
    let mut slug = String::new();
    for character in name.chars() {
        match character {
            'a'..='z' | '0'..='9' | '_' => slug.push(character),
            'A'..='Z' => slug.push(character.to_ascii_lowercase()),
            _ if slug.ends_with('-') || slug.is_empty() => {}
            _ => slug.push('-'),
        }
        if slug.len() >= MAX_SERVER_ID_CHARS {
            break;
        }
    }
    // An id that reads as an attach receipt would make the attach-only paths answer for a real
    // process. Numbering cannot undo that, because every variant keeps the prefix, so the prefix
    // is dropped here and process ids are disjoint from attach ids by construction.
    let slug = slug
        .trim_matches('-')
        .trim_start_matches(crate::preview::ATTACH_SERVER_ID_PREFIX)
        .trim_matches('-');
    if slug.is_empty() {
        return "server".to_owned();
    }
    slug.to_owned()
}

/// Long enough for a descriptive configuration name, short enough that repeating it on every
/// preview call stays cheap.
const MAX_SERVER_ID_CHARS: usize = 40;

/// Called after every change to the server list, so the pane can redraw.
///
/// The argument is the ids this change *deliberately* stopped, which is never
/// the same question as "which ids are gone". A server that exits on its own is
/// gone too, and the two endings are answered differently: a stop takes the page
/// it was serving with it, a crash leaves the page there to be looked at.
pub type PreviewChangeListener = Arc<dyn Fn(&[String]) + Send + Sync>;

/// Every dev server this app started, keyed by worktree.
#[derive(Clone, Default)]
pub struct PreviewServerRegistry {
    inner: Arc<Mutex<Registry>>,
    on_change: Arc<Mutex<Option<PreviewChangeListener>>>,
}

impl PreviewServerRegistry {
    fn lock(&self) -> std::sync::MutexGuard<'_, Registry> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Called after every change to the server list. The pane redraws from it.
    pub fn set_change_listener(&self, listener: PreviewChangeListener) {
        *self
            .on_change
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(listener);
    }

    fn emit_change(&self) {
        self.emit_stopped(&[]);
    }

    /// Reports a change that stopped `stopped_ids` on purpose.
    fn emit_stopped(&self, stopped_ids: &[String]) {
        // Cloned out first: a listener that reads the registry back would otherwise
        // deadlock against the write that triggered it.
        let listener = self
            .on_change
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Some(listener) = listener {
            listener(stopped_ids);
        }
    }

    /// Every server, across worktrees, for tests.
    #[cfg(test)]
    pub fn servers(&self) -> Vec<PreviewServerSnapshot> {
        let registry = self.lock();
        let mut servers = Vec::new();
        for (worktree, entries) in registry.worktrees.iter() {
            for entry in entries.values() {
                servers.push(entry.snapshot(worktree));
            }
        }
        servers
    }

    /// The live servers on one machine (`None` is this computer). Ports belong to a machine, so
    /// only these can be what holds a port there.
    fn live_on(&self, machine_key: Option<&str>) -> Vec<PreviewServerSnapshot> {
        let registry = self.lock();
        registry
            .worktrees
            .iter()
            .flat_map(|(worktree, entries)| {
                entries
                    .values()
                    .filter(|entry| entry.machine_key() == machine_key)
                    .filter(|entry| {
                        matches!(
                            entry.status,
                            PreviewServerStatus::Running | PreviewServerStatus::Starting
                        )
                    })
                    .map(move |entry| entry.snapshot(worktree))
            })
            .collect()
    }

    /// Every server `session_id` started, in whichever worktree and on whichever machine.
    pub fn servers_owned_by(&self, session_id: &str) -> Vec<PreviewServerSnapshot> {
        let registry = self.lock();
        registry
            .worktrees
            .iter()
            .flat_map(|(worktree, entries)| {
                entries
                    .values()
                    .filter(|entry| entry.session_id.as_deref() == Some(session_id))
                    .map(move |entry| entry.snapshot(worktree))
            })
            .collect()
    }

    pub fn servers_for_worktree(&self, worktree: &Path) -> Vec<PreviewServerSnapshot> {
        let registry = self.lock();
        registry
            .worktrees
            .get(worktree)
            .map(|entries| {
                entries
                    .values()
                    .map(|entry| entry.snapshot(worktree))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn get(&self, server_id: &str) -> Option<PreviewServerSnapshot> {
        let registry = self.lock();
        registry.worktrees.iter().find_map(|(worktree, entries)| {
            entries.get(server_id).map(|entry| entry.snapshot(worktree))
        })
    }

    /// The buffered output, oldest first. Empty once the server has exited or stopped.
    pub fn logs(&self, server_id: &str) -> Vec<PreviewLogEntry> {
        let registry = self.lock();
        registry
            .logs
            .get(server_id)
            .map(PreviewLogRing::to_vec)
            .unwrap_or_default()
    }

    /// Refuses a sixth server for this worktree.
    pub fn ensure_capacity(
        &self,
        worktree: &Path,
        session_id: Option<&str>,
    ) -> Result<(), PreviewStartError> {
        match capacity_error(&self.servers_for_worktree(worktree), session_id) {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Picks the port the server will actually bind, honouring `autoPort`.
    pub fn select_port(
        &self,
        port: u16,
        auto_port: Option<bool>,
        session_id: Option<&str>,
    ) -> Result<u16, PortInUseError> {
        let running = self.live_on(None);
        if let Some(occupant) = running.iter().find(|server| server.port == port) {
            let cross_session = session_id.is_some()
                && occupant.session_id.is_some()
                && occupant.session_id.as_deref() != session_id;
            if auto_port == Some(true) {
                let occupant = (!cross_session).then_some(occupant);
                return self.reassign_port(port, occupant);
            }
            return Err(preview_port_conflict(
                port,
                auto_port,
                Some(occupant),
                cross_session,
            ));
        }
        let attempts = (auto_port == Some(true)).then_some(1);
        match reserve_port(port, attempts) {
            Ok(bound) => Ok(bound),
            Err(failure) => {
                if auto_port == Some(true) {
                    return self.reassign_port(port, None);
                }
                if failure == PortProbeFailure::Denied {
                    return Err(PortInUseError::external(
                        port,
                        os_reserved_port_message(port),
                    ));
                }
                let occupant = port_occupant_description(port);
                Err(external_port_conflict(port, auto_port, occupant.as_deref()))
            }
        }
    }

    fn reassign_port(
        &self,
        port: u16,
        occupant: Option<&PreviewServerSnapshot>,
    ) -> Result<u16, PortInUseError> {
        match reserve_port(0, None) {
            Ok(fresh) => Ok(fresh),
            Err(_) => {
                let occupant_text = occupant
                    .is_none()
                    .then(|| port_occupant_description(port))
                    .flatten();
                Err(auto_port_reassignment_failed(
                    port,
                    occupant,
                    occupant_text.as_deref(),
                ))
            }
        }
    }

    /// [`Self::select_port`] on another machine: the same `autoPort` rules and the same messages,
    /// with the probing done there by its agent — a port is that machine's, and whether it is
    /// free here says nothing about it. What holds a port there is named when it is one of this
    /// registry's servers; anything else is only known to be there.
    pub fn select_remote_port(
        &self,
        host: &dyn RemoteServerHost,
        port: u16,
        auto_port: Option<bool>,
        session_id: Option<&str>,
    ) -> Result<u16, PortInUseError> {
        let running = self.live_on(Some(host.machine_key()));
        let reassign = |occupant: Option<&PreviewServerSnapshot>| {
            host.free_port().map_err(|_| auto_port_reassignment_failed(port, occupant, None))
        };
        if let Some(occupant) = running.iter().find(|server| server.port == port) {
            let cross_session = session_id.is_some()
                && occupant.session_id.is_some()
                && occupant.session_id.as_deref() != session_id;
            if auto_port == Some(true) {
                return reassign((!cross_session).then_some(occupant));
            }
            return Err(preview_port_conflict(
                port,
                auto_port,
                Some(occupant),
                cross_session,
            ));
        }
        if port == 0 {
            return reassign(None);
        }
        match host.port_state(port) {
            Ok((true, false)) => Ok(port),
            Ok(_) if auto_port == Some(true) => reassign(None),
            Ok(_) => Err(external_port_conflict(port, auto_port, None)),
            Err(error) => Err(PortInUseError::external(
                port,
                format!(
                    "Could not check port {port} on {}: {}",
                    host.machine_label(),
                    sanitize_message_text(&error)
                ),
            )),
        }
    }

    /// Starts a server and returns once it has survived the startup gate.
    ///
    /// Success here means "did not die in three seconds", not "is serving". Readiness
    /// is decided afterwards on its own thread, and it only ever moves the row from
    /// `starting` to `running` — even a readiness timeout does, because a build that
    /// takes longer than a minute is slow, not broken, and a row stuck on `starting`
    /// forever is worse than an optimistic one.
    pub fn start(
        &self,
        worktree: &Path,
        config: &PreviewServerConfig,
        session_id: Option<&str>,
    ) -> Result<PreviewServerSnapshot, PreviewStartError> {
        let Some(command) = config.command.as_deref() else {
            return Err(PreviewStartError {
                message: NO_COMMAND_MESSAGE.to_owned(),
                kind: PreviewStartErrorKind::SpawnError,
                code: None,
                exit_code: None,
                output: None,
            });
        };
        let cwd = worktree.join(&config.cwd);
        let path_directories = preview_path_directories(&config.env);
        let resolved = resolve_preview_command(command, &config.args, &path_directories);

        let mut process = Command::new(&resolved.program);
        process
            .args(&resolved.args)
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("FORCE_COLOR", "1");
        for (name, value) in &config.env {
            process.env(name, value);
        }
        // The port this host reserved wins over anything the entry configured; the
        // scrub runs last so a config file cannot reintroduce a harness token.
        process.env("PORT", config.port.to_string());
        for name in crate::child_environment::private_child_environment_names() {
            process.env_remove(&name);
        }
        process.env_remove(crate::child_environment::DEV_APPLICATION_PATH_ENVIRONMENT_NAME);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            process.creation_flags(CREATE_NO_WINDOW);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Its own session, so the stop can target the group instead of the app's.
            unsafe {
                process.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }

        let mut child = match process.spawn() {
            Ok(child) => child,
            Err(error) => {
                let code = spawn_error_code(&error);
                let cwd_missing = code.as_deref() == Some("ENOENT") && !cwd.exists();
                return Err(preview_start_failure(
                    PreviewSpawnFailure::SpawnError {
                        code,
                        error: error.to_string(),
                        via_exit_text: false,
                    },
                    command,
                    "",
                    cwd_missing,
                ));
            }
        };

        let job = ShellJob::create();
        job.assign(&child);
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let process = Arc::new(Mutex::new(PreviewProcess {
            child,
            job,
            stopped: false,
        }));

        let server_id = {
            let mut registry = self.lock();
            // Minted under the same lock that inserts the entry, so two concurrent starts of one
            // configuration cannot settle on the same id.
            let server_id = registry.mint_server_id(&config.name);
            registry
                .logs
                .insert(server_id.clone(), PreviewLogRing::new());
            registry
                .worktrees
                .entry(worktree.to_path_buf())
                .or_default()
                .insert(
                    server_id.clone(),
                    ServerEntry {
                        server_id: server_id.clone(),
                        name: config.name.clone(),
                        port: config.port,
                        status: PreviewServerStatus::Starting,
                        started_at: Utc::now().to_rfc3339(),
                        session_id: session_id.map(str::to_owned),
                        process: ServerProcess::Local(process.clone()),
                        machine: None,
                        display_cwd: None,
                    },
                );
            server_id
        };
        self.emit_change();

        let capturing = Arc::new(AtomicBool::new(true));
        let early_stderr = Arc::new(Mutex::new(String::new()));
        if let Some(stdout) = stdout {
            drain_pipe(
                stdout,
                PreviewLogSink::new(self.clone(), &server_id, PreviewLogStream::Stdout),
                None,
            );
        }
        if let Some(stderr) = stderr {
            drain_pipe(
                stderr,
                PreviewLogSink::new(self.clone(), &server_id, PreviewLogStream::Stderr),
                Some((early_stderr.clone(), capturing.clone())),
            );
        }

        let exit = self.await_startup_gate(&process);
        capturing.store(false, Ordering::Release);
        if let Some(exit_code) = exit {
            let output = self
                .logs(&server_id)
                .iter()
                .map(|entry| entry.line.as_str())
                .collect::<String>()
                .trim()
                .to_owned();
            let error = early_stderr
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            self.remove(worktree, &server_id);
            self.emit_change();
            return Err(preview_start_failure(
                PreviewSpawnFailure::EarlyExit {
                    exit_code,
                    error: (!error.is_empty()).then_some(error),
                },
                command,
                &output,
                false,
            ));
        }

        self.watch(worktree, &server_id, process);
        self.watch_readiness(
            worktree,
            &server_id,
            config.port,
            readiness_is_https(config.url.as_deref()),
        );
        self.get(&server_id).ok_or_else(|| PreviewStartError {
            message: dead_reuse_refusal_message(&config.name),
            kind: PreviewStartErrorKind::EarlyExit,
            code: None,
            exit_code: None,
            output: None,
        })
    }

    /// [`Self::start`] on another machine, through its agent.
    ///
    /// The books are the same — one entry under `worktree`, its output in a ring, a startup gate
    /// in which an exit means failure, readiness decided on its own thread — and so is every
    /// message. `worktree` is the registry's key for the remote directory, which names no path on
    /// this computer; `display_cwd` is what the snapshot reports instead. The process belongs to
    /// the machine's agent, not to the SSH connection: a dropped link pauses it rather than ending
    /// it, and its output is kept there until the link is back.
    pub fn start_remote(
        &self,
        worktree: &Path,
        display_cwd: &str,
        host: Arc<dyn RemoteServerHost>,
        config: &PreviewServerConfig,
        session_id: Option<&str>,
    ) -> Result<PreviewServerSnapshot, PreviewStartError> {
        let Some(command) = config.command.as_deref() else {
            return Err(PreviewStartError {
                message: NO_COMMAND_MESSAGE.to_owned(),
                kind: PreviewStartErrorKind::SpawnError,
                code: None,
                exit_code: None,
                output: None,
            });
        };
        let mut remote = host.spawn(config)?;
        let stdout = remote.take_stdout();
        let stderr = remote.take_stderr();
        let process = Arc::new(RemoteServerProcess {
            process: remote,
            stopped: AtomicBool::new(false),
        });
        let server_id = {
            let mut registry = self.lock();
            let server_id = registry.mint_server_id(&config.name);
            registry
                .logs
                .insert(server_id.clone(), PreviewLogRing::new());
            registry
                .worktrees
                .entry(worktree.to_path_buf())
                .or_default()
                .insert(
                    server_id.clone(),
                    ServerEntry {
                        server_id: server_id.clone(),
                        name: config.name.clone(),
                        port: config.port,
                        status: PreviewServerStatus::Starting,
                        started_at: Utc::now().to_rfc3339(),
                        session_id: session_id.map(str::to_owned),
                        process: ServerProcess::Remote(process.clone()),
                        machine: Some(ServerMachine {
                            key: host.machine_key().to_owned(),
                            label: host.machine_label().to_owned(),
                        }),
                        display_cwd: Some(display_cwd.to_owned()),
                    },
                );
            server_id
        };
        self.emit_change();

        let capturing = Arc::new(AtomicBool::new(true));
        let early_stderr = Arc::new(Mutex::new(String::new()));
        if let Some(stdout) = stdout {
            drain_pipe(
                stdout,
                PreviewLogSink::new(self.clone(), &server_id, PreviewLogStream::Stdout),
                None,
            );
        }
        if let Some(stderr) = stderr {
            drain_pipe(
                stderr,
                PreviewLogSink::new(self.clone(), &server_id, PreviewLogStream::Stderr),
                Some((early_stderr.clone(), capturing.clone())),
            );
        }

        // The gate is the same race as a local one; only the clock it waits on is the agent's.
        let exit = match process.process.wait_timeout(STARTUP_GATE) {
            Ok(Some(exit)) => Some(exit.code),
            Ok(None) => None,
            Err(error) => {
                self.remove(worktree, &server_id);
                self.emit_change();
                return Err(PreviewStartError {
                    message: format!(
                        "Lost track of the dev server on {} while it was starting: {error}",
                        host.machine_label()
                    ),
                    kind: PreviewStartErrorKind::Unreachable,
                    code: None,
                    exit_code: None,
                    output: None,
                });
            }
        };
        capturing.store(false, Ordering::Release);
        if let Some(exit_code) = exit {
            // Everything the process wrote has arrived once its exit has: the agent publishes the
            // exit only after both streams ended. The drains still need a moment to hand it over.
            thread::sleep(Duration::from_millis(50));
            let output = self
                .logs(&server_id)
                .iter()
                .map(|entry| entry.line.as_str())
                .collect::<String>()
                .trim()
                .to_owned();
            let error = early_stderr
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            self.remove(worktree, &server_id);
            self.emit_change();
            // The launch script's own verdicts, which read like the spawn errors a local start
            // reports for the same mistakes.
            let failure = match exit_code {
                Some(REMOTE_EXIT_NO_CWD) => PreviewSpawnFailure::SpawnError {
                    code: Some("ENOENT".to_owned()),
                    error: error.trim().to_owned(),
                    via_exit_text: false,
                },
                _ => PreviewSpawnFailure::EarlyExit {
                    exit_code,
                    error: (!error.is_empty()).then_some(error),
                },
            };
            return Err(preview_start_failure(
                reclassify_exit_127(failure),
                command,
                &output,
                exit_code == Some(REMOTE_EXIT_NO_CWD),
            ));
        }

        self.watch_remote(worktree, &server_id, process);
        let registry = self.clone();
        let readiness_worktree = worktree.to_path_buf();
        let readiness_id = server_id.clone();
        let port = config.port;
        let https = readiness_is_https(config.url.as_deref());
        thread::spawn(move || {
            // Waited out on the machine itself, so a slow link costs one round trip rather than
            // one per probe; like a local start, a timeout still promotes the row.
            host.wait_ready(port, READINESS_TIMEOUT, https);
            registry.promote(&readiness_worktree, &readiness_id);
        });
        self.get(&server_id).ok_or_else(|| PreviewStartError {
            message: dead_reuse_refusal_message(&config.name),
            kind: PreviewStartErrorKind::EarlyExit,
            code: None,
            exit_code: None,
            output: None,
        })
    }

    /// Moves a starting row to running, once.
    fn promote(&self, worktree: &Path, server_id: &str) {
        let promoted = {
            let mut inner = self.lock();
            inner
                .worktrees
                .get_mut(worktree)
                .and_then(|entries| entries.get_mut(server_id))
                .filter(|entry| entry.status == PreviewServerStatus::Starting)
                .map(|entry| entry.status = PreviewServerStatus::Running)
                .is_some()
        };
        if promoted {
            self.emit_change();
        }
    }

    /// Forgets a remote server once it has exited, or once the host can no longer find out how
    /// it ended — its machine reclaimed it, or its link gave up for good. A link that is merely
    /// reconnecting is neither: the process keeps running there, and the wait keeps waiting.
    fn watch_remote(&self, worktree: &Path, server_id: &str, process: Arc<RemoteServerProcess>) {
        let registry = self.clone();
        let worktree = worktree.to_path_buf();
        let server_id = server_id.to_owned();
        thread::spawn(move || {
            loop {
                if process.stopped.load(Ordering::Acquire) {
                    return;
                }
                match process.process.wait_timeout(Duration::from_secs(5)) {
                    Ok(None) => continue,
                    Ok(Some(_)) | Err(_) => break,
                }
            }
            if process.stopped.load(Ordering::Acquire) {
                return;
            }
            if registry.remove(&worktree, &server_id) {
                registry.emit_change();
            }
        });
    }

    /// Starts, retrying transient failures. Configuration mistakes are reported at once.
    pub fn start_with_retries(
        &self,
        worktree: &Path,
        config: &PreviewServerConfig,
        session_id: Option<&str>,
    ) -> Result<PreviewServerSnapshot, PreviewStartError> {
        let mut last = None;
        for attempt in 1..=MAX_SPAWN_ATTEMPTS {
            match self.start(worktree, config, session_id) {
                Ok(snapshot) => return Ok(snapshot),
                Err(error) if !error.is_retryable() || attempt == MAX_SPAWN_ATTEMPTS => {
                    return Err(error)
                }
                Err(error) => last = Some(error),
            }
        }
        Err(last.unwrap_or_else(|| PreviewStartError {
            message: "Failed to start preview server after retries".to_owned(),
            kind: PreviewStartErrorKind::SpawnError,
            code: None,
            exit_code: None,
            output: None,
        }))
    }

    /// `Some(exit code)` when the process died inside the gate, `None` when it survived.
    fn await_startup_gate(&self, process: &Arc<Mutex<PreviewProcess>>) -> Option<Option<i32>> {
        let deadline = Instant::now() + STARTUP_GATE;
        loop {
            {
                let mut process = process
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if process.stopped {
                    return None;
                }
                if let Ok(Some(status)) = process.child.try_wait() {
                    return Some(status.code());
                }
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(STARTUP_GATE_POLL);
        }
    }

    fn watch(&self, worktree: &Path, server_id: &str, process: Arc<Mutex<PreviewProcess>>) {
        let registry = self.clone();
        let worktree = worktree.to_path_buf();
        let server_id = server_id.to_owned();
        thread::spawn(move || {
            loop {
                {
                    let mut process = process
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    if process.stopped {
                        return;
                    }
                    if matches!(process.child.try_wait(), Ok(Some(_)) | Err(_)) {
                        break;
                    }
                }
                thread::sleep(SUPERVISOR_POLL);
            }
            if registry.remove(&worktree, &server_id) {
                registry.emit_change();
            }
        });
    }

    fn watch_readiness(&self, worktree: &Path, server_id: &str, port: u16, https: bool) {
        let registry = self.clone();
        let worktree = worktree.to_path_buf();
        let server_id = server_id.to_owned();
        thread::spawn(move || {
            let alive = {
                let registry = registry.clone();
                let worktree = worktree.clone();
                let server_id = server_id.clone();
                move || {
                    registry
                        .lock()
                        .worktrees
                        .get(&worktree)
                        .is_some_and(|entries| entries.contains_key(&server_id))
                }
            };
            wait_until_ready_while(port, READINESS_TIMEOUT, https, &alive);
            registry.promote(&worktree, &server_id);
        });
    }

    fn remove(&self, worktree: &Path, server_id: &str) -> bool {
        let mut registry = self.lock();
        let mut removed = false;
        if let Some(entries) = registry.worktrees.get_mut(worktree) {
            removed = entries.remove(server_id).is_some();
            if entries.is_empty() {
                registry.worktrees.remove(worktree);
            }
        }
        registry.logs.remove(server_id);
        registry.retire(server_id);
        removed
    }

    fn append_log(&self, server_id: &str, stream: PreviewLogStream, text: String) {
        let mut registry = self.lock();
        if let Some(buffer) = registry.logs.get_mut(server_id) {
            buffer.push(text, stream);
        }
    }

    /// Stops one server: kills its whole tree and forgets it, buffer included.
    pub fn stop(&self, server_id: &str) -> bool {
        let process = {
            let mut registry = self.lock();
            let Some(worktree) = registry
                .worktrees
                .iter()
                .find(|(_, entries)| entries.contains_key(server_id))
                .map(|(worktree, _)| worktree.clone())
            else {
                return false;
            };
            let entries = registry
                .worktrees
                .get_mut(&worktree)
                .expect("the worktree was just found");
            let Some(mut entry) = entries.remove(server_id) else {
                return false;
            };
            if entries.is_empty() {
                registry.worktrees.remove(&worktree);
            }
            entry.status = PreviewServerStatus::Stopped;
            registry.logs.remove(server_id);
            registry.retire(server_id);
            entry.process.clone()
        };
        kill_server_process(&process);
        self.emit_stopped(&[server_id.to_owned()]);
        true
    }

    /// Kills every server. Safe to call from the app-exit hook: synchronous, and the
    /// job objects mean a process that ignores the kill still dies with this one.
    pub fn stop_all(&self) {
        let (processes, stopped_ids): (Vec<ServerProcess>, Vec<String>) = {
            let mut registry = self.lock();
            let processes = registry
                .worktrees
                .values()
                .flat_map(|entries| entries.values())
                .map(|entry| entry.process.clone())
                .collect();
            let retiring = registry
                .worktrees
                .values()
                .flat_map(|entries| entries.keys().cloned())
                .collect::<Vec<_>>();
            for server_id in &retiring {
                registry.retire(server_id);
            }
            registry.worktrees.clear();
            registry.logs.clear();
            (processes, retiring)
        };
        for process in &processes {
            kill_server_process(process);
        }
        if !processes.is_empty() {
            self.emit_stopped(&stopped_ids);
        }
    }
}

/// How long a remote server is given to exit on SIGTERM before its tree is killed.
const REMOTE_STOP_GRACE: Duration = Duration::from_secs(3);

/// The launch script's exit code for a working directory that is not there.
pub const REMOTE_EXIT_NO_CWD: i32 = 64;

fn kill_server_process(process: &ServerProcess) {
    match process {
        ServerProcess::Local(process) => kill_preview_process(process),
        ServerProcess::Remote(process) => {
            if process.stopped.swap(true, Ordering::AcqRel) {
                return;
            }
            // Terminate first, the way a local stop's tree kill gives a dev server its chance to
            // clean up, then the whole group. Queued if the link is down: the agent acts on it
            // when the link is back, and reclaims the session on its own if it never is.
            process
                .process
                .signal(remote_agent::protocol::SignalKind::Terminate);
            let process = Arc::clone(process);
            thread::spawn(move || {
                if !matches!(process.process.wait_timeout(REMOTE_STOP_GRACE), Ok(Some(_))) {
                    process.process.kill();
                }
            });
        }
    }
}

fn kill_preview_process(process: &Arc<Mutex<PreviewProcess>>) {
    let mut process = process
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if process.stopped {
        return;
    }
    process.stopped = true;
    let PreviewProcess { child, job, .. } = &mut *process;
    kill_process_tree_or_child(child, job);
    let _ = child.wait();
}

/// One pipe's write end into a server's ring buffer.
struct PreviewLogSink {
    registry: PreviewServerRegistry,
    server_id: String,
    stream: PreviewLogStream,
    decoder: ConsoleTextDecoder,
}

impl PreviewLogSink {
    fn new(registry: PreviewServerRegistry, server_id: &str, stream: PreviewLogStream) -> Self {
        Self {
            registry,
            server_id: server_id.to_owned(),
            stream,
            decoder: ConsoleTextDecoder::new(),
        }
    }

    fn append(&mut self, bytes: &[u8]) -> String {
        let text = self.decoder.push(bytes);
        self.forward(&text);
        text
    }

    fn finish(&mut self) -> String {
        let text = self.decoder.finish();
        self.forward(&text);
        text
    }

    fn forward(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.registry
            .append_log(&self.server_id, self.stream, text.to_owned());
    }
}

/// Where the startup gate's copy of stderr accumulates, and the flag that closes it.
type EarlyStderr = (Arc<Mutex<String>>, Arc<AtomicBool>);

/// Drains one pipe to end-of-stream, one ring entry per read.
fn drain_pipe<R: Read + Send + 'static>(
    mut pipe: R,
    mut sink: PreviewLogSink,
    early: Option<EarlyStderr>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut chunk = [0_u8; PIPE_CHUNK];
        loop {
            let read = match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            };
            let text = sink.append(&chunk[..read]);
            accumulate_early(&early, &text);
        }
        let text = sink.finish();
        accumulate_early(&early, &text);
    })
}

/// Keeps a copy of stderr while the startup gate is open, so a server that dies in
/// its first three seconds can be explained by what it said.
fn accumulate_early(early: &Option<EarlyStderr>, text: &str) {
    let Some((buffer, capturing)) = early else {
        return;
    };
    if text.is_empty() || !capturing.load(Ordering::Acquire) {
        return;
    }
    buffer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push_str(text);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(line: &str, stream: PreviewLogStream) -> PreviewLogEntry {
        PreviewLogEntry {
            line: line.to_owned(),
            stream,
            timestamp: "2026-01-01T00:00:00+00:00".to_owned(),
        }
    }

    fn snapshot(
        server_id: &str,
        name: &str,
        port: u16,
        session_id: Option<&str>,
    ) -> PreviewServerSnapshot {
        PreviewServerSnapshot {
            server_id: server_id.to_owned(),
            name: name.to_owned(),
            port,
            status: PreviewServerStatus::Running,
            started_at: "2026-01-01T00:00:00+00:00".to_owned(),
            cwd: "C:\\repo".to_owned(),
            session_id: session_id.map(str::to_owned),
            machine: None,
            url: None,
            workspace: None,
        }
    }

    /// The model quotes a server id back on every later preview call, so the id is the launch
    /// configuration's own name. Names that are not usable as ids, and names already in use, still
    /// have to resolve to something unique — and never to something an attach receipt could be.
    #[test]
    fn a_server_id_is_its_configuration_name_made_usable_and_unique() {
        assert_eq!(slug_server_id("dev"), "dev");
        assert_eq!(slug_server_id("Dev Server"), "dev-server");
        assert_eq!(slug_server_id("web_app.2"), "web_app-2");
        assert_eq!(
            slug_server_id("npm run dev -- --claude"),
            "npm-run-dev-claude"
        );
        // Nothing usable left, but a server still needs an address.
        assert_eq!(slug_server_id("  ---  "), "server");
        assert_eq!(slug_server_id("名字"), "server");
        assert_eq!(slug_server_id(""), "server");
        let long = slug_server_id(&"d".repeat(200));
        assert!(long.len() <= MAX_SERVER_ID_CHARS, "{long}");

        // A process id can never read as an attach receipt, because numbering would keep the
        // prefix and the attach-only paths would then answer for a live process.
        for attach_shaped in [
            "browser-preview-abc",
            "browser-preview-browser-preview-abc",
            "Browser Preview abc",
        ] {
            let slug = slug_server_id(attach_shaped);
            assert!(
                !crate::preview::is_attach_server_id(&slug),
                "{attach_shaped} → {slug}"
            );
        }
        // Dropping the prefix can leave nothing but the prefix's own stem, which is a perfectly
        // ordinary id: only the trailing separator makes an id an attach receipt.
        assert_eq!(slug_server_id("browser-preview-"), "browser-preview");
        assert!(!crate::preview::is_attach_server_id("browser-preview"));

        let mut registry = Registry::default();
        assert_eq!(registry.mint_server_id("dev"), "dev");
        registry.logs.insert("dev".into(), PreviewLogRing::new());
        assert_eq!(registry.mint_server_id("Dev"), "dev-2");
        registry.logs.insert("dev-2".into(), PreviewLogRing::new());
        assert_eq!(registry.mint_server_id("dev"), "dev-3");
        // A stopped server keeps its id forever: an old `preview_start` receipt in the
        // conversation names it, and a dying process's log drain may still be holding it.
        registry.logs.remove("dev");
        registry.retire("dev");
        assert_eq!(registry.mint_server_id("dev"), "dev-3");
    }

    /// The buffer is a ring, not a queue: once full it overwrites the oldest entry and
    /// still reads back in arrival order. Getting this wrong shows up as a log view
    /// that scrolls backwards.
    #[test]
    fn the_ring_buffer_keeps_the_newest_entries_in_order() {
        let mut ring = PreviewLogRing::with_capacity(3);
        for index in 1..=5 {
            ring.push(format!("chunk-{index}"), PreviewLogStream::Stdout);
        }
        let lines: Vec<String> = ring.to_vec().into_iter().map(|entry| entry.line).collect();
        assert_eq!(lines, ["chunk-3", "chunk-4", "chunk-5"]);
    }

    #[test]
    fn a_partly_filled_ring_reads_back_unrotated() {
        let mut ring = PreviewLogRing::with_capacity(4);
        ring.push("a".to_owned(), PreviewLogStream::Stdout);
        ring.push("b".to_owned(), PreviewLogStream::Stderr);
        let entries = ring.to_vec();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].line, "a");
        assert_eq!(entries[1].stream, PreviewLogStream::Stderr);
    }

    /// `level: "error"` is two conditions, not one. A stdout line that says "error" is
    /// usually a framework banner, and a stderr line without one of the four words is
    /// usually a deprecation warning.
    #[test]
    fn the_error_filter_wants_stderr_and_an_error_word() {
        let entries = [
            entry("ERROR: on stdout\n", PreviewLogStream::Stdout),
            entry("warning: nothing wrong\n", PreviewLogStream::Stderr),
            entry("Build Failed\n", PreviewLogStream::Stderr),
            entry("uncaught Exception\n", PreviewLogStream::Stderr),
        ];
        let rendered = render_preview_logs(
            &entries,
            &PreviewLogQuery {
                errors_only: true,
                ..PreviewLogQuery::default()
            },
        );
        assert_eq!(rendered, "Build Failed\nuncaught Exception\n");
    }

    #[test]
    fn the_search_filter_is_case_sensitive() {
        let entries = [
            entry("listening on 3000\n", PreviewLogStream::Stdout),
            entry("Listening on 4000\n", PreviewLogStream::Stdout),
        ];
        let rendered = render_preview_logs(
            &entries,
            &PreviewLogQuery {
                search: Some("Listening".to_owned()),
                ..PreviewLogQuery::default()
            },
        );
        assert_eq!(rendered, "Listening on 4000\n");
    }

    #[test]
    fn the_line_limit_defaults_to_fifty_and_clamps_to_two_hundred() {
        let entries: Vec<PreviewLogEntry> = (0..300)
            .map(|index| entry(&format!("{index}\n"), PreviewLogStream::Stdout))
            .collect();
        assert_eq!(
            render_preview_logs(&entries, &PreviewLogQuery::default())
                .lines()
                .count(),
            50
        );
        assert_eq!(
            render_preview_logs(
                &entries,
                &PreviewLogQuery {
                    lines: Some(5000),
                    ..PreviewLogQuery::default()
                }
            )
            .lines()
            .count(),
            200
        );
        assert_eq!(
            render_preview_logs(
                &entries,
                &PreviewLogQuery {
                    lines: Some(0),
                    ..PreviewLogQuery::default()
                }
            ),
            "299\n"
        );
    }

    /// Each empty result says which filter produced it, so the model does not read
    /// "no errors" as "no output".
    #[test]
    fn each_empty_result_names_its_own_filter() {
        let entries = [entry("all fine\n", PreviewLogStream::Stdout)];
        assert_eq!(
            render_preview_logs(
                &entries,
                &PreviewLogQuery {
                    errors_only: true,
                    ..PreviewLogQuery::default()
                }
            ),
            "No server errors found."
        );
        assert_eq!(
            render_preview_logs(
                &entries,
                &PreviewLogQuery {
                    search: Some("absent".to_owned()),
                    ..PreviewLogQuery::default()
                }
            ),
            "No logs matching \"absent\"."
        );
        assert_eq!(
            render_preview_logs(&[], &PreviewLogQuery::default()),
            "No logs yet."
        );
    }

    /// The three `autoPort` states are three different diagnoses. `false` says the port
    /// is non-negotiable and offers to stop the occupant; unset asks the user which it
    /// is; `true` never reaches this function at all.
    #[test]
    fn the_port_conflict_message_follows_the_auto_port_tri_state() {
        let occupant = snapshot("srv-1", "frontend", 3000, Some("chat-a"));

        let explicit = preview_port_conflict(3000, Some(false), Some(&occupant), false);
        assert_eq!(explicit.source, PortConflictSource::Launch);
        assert_eq!(
            explicit.message,
            "Port 3000 is required by this server (autoPort is false) but is in use by preview \
             server \"frontend\" (srv-1). Ask the user if they want to stop \"frontend\" to free \
             port 3000. If yes, call preview_stop with serverId \"srv-1\" and retry."
        );

        let undecided = preview_port_conflict(3000, None, Some(&occupant), false);
        assert!(undecided
            .message
            .starts_with("Port 3000 is in use by preview server \"frontend\" (srv-1). "));
        assert!(undecided.message.ends_with(&auto_port_hint(3000)));

        let unknown_occupant = preview_port_conflict(3000, None, None, false);
        assert!(unknown_occupant
            .message
            .starts_with("Port 3000 is in use by another preview server. "));
    }

    /// Another chat's server cannot be stopped from this one, so the message must not
    /// offer `preview_stop` — the tool would refuse and the model would loop.
    #[test]
    fn a_cross_chat_occupant_is_named_but_not_offered_for_stopping() {
        let occupant = snapshot("srv-9", "backend", 3000, Some("chat-b"));
        let message = preview_port_conflict(3000, Some(false), Some(&occupant), true).message;
        assert_eq!(
            message,
            "Port 3000 is in use by another chat's dev server \"backend\". preview_stop won't stop \
             another chat's server. Ask the user to stop it from that chat, or to change \
             \"autoPort\" in .mework/launch.json so this session can use a different port."
        );

        let undecided = preview_port_conflict(3000, None, Some(&occupant), true).message;
        assert!(undecided.ends_with(&auto_port_hint(3000)));
    }

    #[test]
    fn an_external_occupant_is_named_when_it_can_be_identified() {
        let named = external_port_conflict(5173, None, Some("\"node\" (PID 42)"));
        assert!(named
            .message
            .starts_with("Port 5173 is in use by \"node\" (PID 42) (not a preview server). "));
        assert_eq!(named.source, PortConflictSource::External);

        let anonymous = external_port_conflict(5173, None, None);
        assert!(anonymous
            .message
            .contains("Run `lsof -i :5173` to identify"));

        let required = external_port_conflict(5173, Some(false), Some("\"node\" (PID 42)"));
        assert_eq!(
            required.message,
            "Port 5173 is required by this server but is in use by \"node\" (PID 42). Stop that \
             process to free port 5173 and try again."
        );
    }

    #[test]
    fn the_os_reserved_message_explains_which_kind_of_reservation() {
        let message = os_reserved_port_message(80);
        assert!(message.starts_with("Port 80 is reserved by the OS ("));
        assert!(message.contains("set \"autoPort\": true to use an OS-assigned port."));
        if host_platform().is_windows() {
            assert!(message.contains("a Windows excluded port range, or a privileged port"));
        } else {
            assert!(message.contains("a privileged port below 1024"));
        }
    }

    /// Name beats port beats "there is only one". A caller who named a server never
    /// reaches the last two: naming is a request for that server, not a hint.
    #[test]
    fn reuse_prefers_the_name_then_the_port_then_the_only_server() {
        let by_port = snapshot("srv-port", "api", 3000, None);
        let by_name = snapshot("srv-name", "Frontend", 9999, None);
        let running = [by_port.clone(), by_name.clone()];

        assert_eq!(
            decide_start_action(&running, "frontend", 3000, None, 2),
            PreviewStartAction::Reuse {
                server: by_name,
                reason: PreviewReuseReason::NameMatch,
            }
        );
        assert_eq!(
            decide_start_action(&running, "nothing-matches", 3000, None, 2),
            PreviewStartAction::Reuse {
                server: by_port,
                reason: PreviewReuseReason::PortMatch,
            }
        );
        assert_eq!(
            decide_start_action(
                &running,
                "nothing-matches",
                3000,
                Some("nothing-matches"),
                2
            ),
            PreviewStartAction::Start
        );
    }

    #[test]
    fn a_single_running_server_is_reused_despite_a_mismatch() {
        let only = snapshot("srv-only", "dev", 5173, None);
        let running = std::slice::from_ref(&only);
        assert_eq!(
            decide_start_action(running, "web", 3000, None, 1),
            PreviewStartAction::Reuse {
                server: only.clone(),
                reason: PreviewReuseReason::SingleRunning,
            }
        );
        // The tolerance is for the single-configuration case only; with two configs a
        // mismatch means the other server, not this one.
        assert_eq!(
            decide_start_action(running, "web", 3000, None, 2),
            PreviewStartAction::Start
        );
    }

    #[test]
    fn another_sessions_server_is_not_reusable_but_an_unowned_one_is() {
        let servers = [
            snapshot("srv-a", "mine", 3000, Some("chat-a")),
            snapshot("srv-b", "theirs", 3001, Some("chat-b")),
            snapshot("srv-c", "shared", 3002, None),
        ];
        let visible: Vec<String> = running_for_session(&servers, Some("chat-a"))
            .into_iter()
            .map(|server| server.server_id)
            .collect();
        assert_eq!(visible, ["srv-a", "srv-c"]);

        let mut stopped = servers[2].clone();
        stopped.status = PreviewServerStatus::Stopped;
        assert!(running_for_session(&[stopped], Some("chat-a")).is_empty());
    }

    /// ENOENT means two different things and the messages have to disagree: a missing
    /// command tells the model to fix `runtimeExecutable`, a missing `cwd` tells it to
    /// fix `cwd`, and giving the wrong one sends it editing the wrong field.
    #[test]
    fn enoent_is_split_between_a_missing_command_and_a_missing_cwd() {
        let failure = || PreviewSpawnFailure::SpawnError {
            code: Some("ENOENT".to_owned()),
            error: "spawn npm ENOENT".to_owned(),
            via_exit_text: false,
        };
        assert_eq!(
            preview_start_failure(failure(), "npm", "", false).message,
            "Command not found: `npm`. Check the `command`/`runtimeExecutable` field in \
             .mework/launch.json and make sure it's installed and on PATH."
        );
        assert_eq!(
            preview_start_failure(failure(), "npm", "", true).message,
            "The working directory set by `cwd` does not exist \u{2014} spawn reports this as \
             ENOENT, the same errno as a missing command. Check the `cwd` field in \
             .mework/launch.json (the folder may have been moved or renamed)."
        );
    }

    #[test]
    fn permission_errors_say_not_to_edit_the_config() {
        let denied = preview_start_failure(
            PreviewSpawnFailure::SpawnError {
                code: Some("EACCES".to_owned()),
                error: "permission denied".to_owned(),
                via_exit_text: false,
            },
            "./serve.sh",
            "ignored output",
            false,
        );
        assert!(denied
            .message
            .starts_with("Permission denied starting `./serve.sh` (EACCES)."));
        assert!(denied.message.ends_with(
            "The command in .mework/launch.json is likely fine \u{2014} don't edit it."
        ));
        // The stream is retained for the caller but never pasted into these two
        // branches: a permission error has nothing useful in it.
        assert!(!denied.message.contains("ignored output"));
        assert_eq!(denied.output.as_deref(), Some("ignored output"));

        let via_text = preview_start_failure(
            PreviewSpawnFailure::SpawnError {
                code: Some("EACCES".to_owned()),
                error: "Permission denied".to_owned(),
                via_exit_text: true,
            },
            "./serve.sh",
            "",
            false,
        );
        assert_eq!(
            via_text.message,
            "Permission denied starting `./serve.sh` (EACCES). Check the file's permissions \
             \u{2014} the command may not be executable."
        );
    }

    #[test]
    fn an_unclassified_spawn_error_carries_the_output() {
        let error = preview_start_failure(
            PreviewSpawnFailure::SpawnError {
                code: None,
                error: "something".to_owned(),
                via_exit_text: false,
            },
            "vite",
            "boom",
            false,
        );
        assert_eq!(error.message, "Could not start `vite`.\n\nOutput:\nboom");
    }

    #[test]
    fn an_early_exit_reports_its_code_and_output() {
        assert_eq!(
            preview_start_failure(
                PreviewSpawnFailure::EarlyExit {
                    exit_code: Some(1),
                    error: None
                },
                "npm",
                "EADDRINUSE",
                false,
            )
            .message,
            "The dev server exited during startup (code 1). Fix the error in the output below, \
             then start the server again.\n\nOutput:\nEADDRINUSE"
        );
        assert_eq!(
            preview_start_failure(
                PreviewSpawnFailure::EarlyExit {
                    exit_code: None,
                    error: None
                },
                "npm",
                "",
                false,
            )
            .message,
            "The dev server exited unexpectedly during startup."
        );
    }

    /// A launcher that could not exec its target exits 127 with the reason on stderr.
    /// Reported as an early exit it looks like the server's own crash.
    #[test]
    fn exit_code_127_is_reclassified_from_the_exit_text() {
        let reclassified = reclassify_exit_127(PreviewSpawnFailure::EarlyExit {
            exit_code: Some(127),
            error: Some("Failed to spawn process: No such file or directory\n".to_owned()),
        });
        assert_eq!(
            reclassified,
            PreviewSpawnFailure::SpawnError {
                code: Some("ENOENT".to_owned()),
                error: "No such file or directory".to_owned(),
                via_exit_text: true,
            }
        );
        let untouched = PreviewSpawnFailure::EarlyExit {
            exit_code: Some(127),
            error: Some("just broke".to_owned()),
        };
        assert_eq!(reclassify_exit_127(untouched.clone()), untouched);

        // The reclassified ENOENT keeps the command-not-found wording: the caller
        // computed `cwd_missing` from the pre-reclassification kind, which was an exit.
        assert!(preview_start_failure(
            PreviewSpawnFailure::EarlyExit {
                exit_code: Some(127),
                error: Some("Failed to spawn process: Not a directory".to_owned()),
            },
            "pnpm",
            "",
            false,
        )
        .message
        .starts_with("Could not start `pnpm` (ENOTDIR) \u{2014} see the output below."));
    }

    #[test]
    fn only_configuration_mistakes_stop_the_retries() {
        for code in NON_RETRYABLE_SPAWN_CODES {
            let error = preview_start_failure(
                PreviewSpawnFailure::SpawnError {
                    code: Some(code.to_owned()),
                    error: String::new(),
                    via_exit_text: false,
                },
                "npm",
                "",
                false,
            );
            assert!(!error.is_retryable(), "{code} must not be retried");
        }
        let transient = preview_start_failure(
            PreviewSpawnFailure::EarlyExit {
                exit_code: Some(1),
                error: None,
            },
            "npm",
            "",
            false,
        );
        assert!(transient.is_retryable());
    }

    /// The sixth server is refused, and the refusal has to say when the ones in the way
    /// are not this chat's — otherwise "stop one first" points at rows the model cannot
    /// stop.
    #[test]
    fn the_sixth_server_is_refused_and_says_whose_the_others_are() {
        let mine: Vec<PreviewServerSnapshot> = (0..MAX_SERVERS_PER_WORKTREE)
            .map(|index| {
                snapshot(
                    &format!("srv-{index}"),
                    &format!("dev-{index}"),
                    3000 + index as u16,
                    Some("chat-a"),
                )
            })
            .collect();
        assert!(capacity_error(&mine[..MAX_SERVERS_PER_WORKTREE - 1], Some("chat-a")).is_none());
        let refused = capacity_error(&mine, Some("chat-a")).expect("the cap is reached");
        assert_eq!(refused.kind, PreviewStartErrorKind::Capacity);
        assert!(!refused.is_retryable());
        assert_eq!(
            refused.message,
            "Maximum 5 servers per worktree. Stop one first."
        );

        let mut mixed = mine;
        mixed[0].session_id = Some("chat-b".to_owned());
        mixed[1].session_id = Some("chat-b".to_owned());
        assert_eq!(
            capacity_error(&mixed, Some("chat-a"))
                .expect("the cap is reached")
                .message,
            "Maximum 5 dev servers per folder reached; 2 belong to other chats. Stop one of this \
             chat's servers, or ask the user to stop one from the other chat."
        );
    }

    #[test]
    fn the_registry_enforces_the_cap_from_its_own_rows() {
        let registry = PreviewServerRegistry::default();
        assert!(registry
            .ensure_capacity(Path::new("C:\\repo"), Some("chat-a"))
            .is_ok());
    }

    /// A `.cmd` is not an executable image, so `npm` on Windows has to become
    /// `cmd /C ...\npm.cmd`; spawning the batch file directly fails with ENOENT.
    /// Windows-only: the fixture's `C:\tools` candidates are built with the
    /// host's own `Path::join`, which only a Windows host spells with `\`.
    #[cfg(windows)]
    #[test]
    fn windows_resolution_wraps_scripts_in_their_interpreters() {
        let directories = [PathBuf::from("C:\\tools")];
        let known = |path: &Path| {
            matches!(
                path.to_string_lossy().as_ref(),
                "C:\\tools\\npm.cmd"
                    | "C:\\tools\\build.ps1"
                    | "C:\\tools\\node.exe"
                    | "C:\\tools\\server.js"
            )
        };
        let system_root = PathBuf::from("C:\\Windows");

        let npm = resolve_command_with(
            "npm",
            &["run".to_owned(), "dev".to_owned()],
            &directories,
            true,
            &system_root,
            &known,
            0,
        );
        assert_eq!(npm.program, PathBuf::from("C:\\Windows\\System32\\cmd.exe"));
        assert_eq!(npm.args, ["/C", "C:\\tools\\npm.cmd", "run", "dev"]);

        let script = resolve_command_with(
            "build",
            &["--watch".to_owned()],
            &directories,
            true,
            &system_root,
            &known,
            0,
        );
        assert_eq!(
            script.program,
            PathBuf::from("C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\PowerShell.exe")
        );
        assert_eq!(
            script.args,
            [
                "-ExecutionPolicy",
                "Unrestricted",
                "-NoLogo",
                "-NonInteractive",
                "-File",
                "C:\\tools\\build.ps1",
                "--watch"
            ]
        );

        let javascript = resolve_command_with(
            "C:\\tools\\server.js",
            &[],
            &directories,
            true,
            &system_root,
            &known,
            0,
        );
        assert_eq!(javascript.program, PathBuf::from("C:\\tools\\node.exe"));
        assert_eq!(javascript.args, ["C:\\tools\\server.js"]);

        let unknown =
            resolve_command_with("mystery", &[], &directories, true, &system_root, &known, 0);
        assert_eq!(unknown.program, PathBuf::from("mystery"));
    }

    #[test]
    fn a_reassigned_port_replaces_hardcoded_port_flags() {
        let args = [
            "run".to_owned(),
            "dev".to_owned(),
            "--port".to_owned(),
            "3000".to_owned(),
            "-p=3000".to_owned(),
            "--port=3000".to_owned(),
        ];
        assert_eq!(
            rewrite_port_arguments(&args, 4321),
            ["run", "dev", "--port", "4321", "-p=4321", "--port=4321"]
        );
    }

    /// A bare bind is not proof: a dual-stack listener on `[::]` leaves the IPv4
    /// loopback bindable while still owning the port for anything that connects.
    #[test]
    fn the_port_probe_rejects_a_port_something_answers_on() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("a free port");
        let port = listener.local_addr().expect("bound").port();
        assert_eq!(reserve_port(port, Some(1)), Err(PortProbeFailure::InUse));
        drop(listener);
        assert!(reserve_port(0, Some(1)).is_ok());
    }

    #[test]
    fn listener_addresses_split_and_classify() {
        assert_eq!(
            split_listener_address("127.0.0.1:3000"),
            Some(("127.0.0.1".to_owned(), 3000))
        );
        assert_eq!(
            split_listener_address("[::1]:3000"),
            Some(("::1".to_owned(), 3000))
        );
        assert!(is_loopback_listener_address("0.0.0.0"));
        assert!(is_loopback_listener_address("::FFFF:127.0.0.1"));
        assert!(!is_loopback_listener_address("192.168.1.4"));
    }

    /// "in use by another process" is not a diagnosis. Naming the occupant is what
    /// lets the model tell a stale dev server from an unrelated service, so the
    /// netstat/tasklist path is checked against a listener we actually own.
    #[cfg(windows)]
    #[test]
    fn the_occupant_lookup_names_this_test_process() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("a free port");
        let port = listener.local_addr().expect("bound").port();
        let description = port_occupant_description(port).expect("this process is listening");
        assert!(
            description.contains(&format!("(PID {})", std::process::id())),
            "{description}"
        );
        assert!(description.contains("mework"), "{description}");
        drop(listener);
        assert!(port_occupant_description(port).is_none());
    }

    /// Untrusted names reach the model inside quoted messages, so the folding runs even
    /// when nothing is truncated — and any change at all earns the ellipsis that says
    /// the value is not verbatim.
    #[test]
    fn untrusted_names_cannot_forge_quoting() {
        assert_eq!(sanitize_message_text("frontend"), "frontend");
        assert_eq!(sanitize_message_text("say \"stop\""), "say 'stop'\u{2026}");
        assert_eq!(sanitize_message_text("first\nsecond"), "first\u{2026}");
        assert_eq!(sanitize_message_text("a\u{0007}b"), "a\u{fffd}b\u{2026}");
        assert_eq!(
            sanitize_message_text(&"x".repeat(200)).chars().count(),
            SANITIZE_LIMIT + 1
        );
    }

    #[test]
    fn only_a_loopback_https_url_makes_the_readiness_probe_speak_tls() {
        assert!(readiness_is_https(Some("https://localhost:8443")));
        assert!(readiness_is_https(Some("https://app.localhost:8443")));
        assert!(!readiness_is_https(Some("http://localhost:3000")));
        assert!(!readiness_is_https(Some("https://example.com")));
        assert!(!readiness_is_https(None));
    }

    /// An absolute path so the lookup does not have to agree with whatever `PATH` the
    /// test runner inherited — under the dev launcher that leads with MSYS2, where a
    /// bare `cmd` is a shell script.
    fn trivial_command(arguments: &[&str]) -> (Option<String>, Vec<String>) {
        #[cfg(windows)]
        let program = system_root()
            .join("System32")
            .join("cmd.exe")
            .to_string_lossy()
            .into_owned();
        #[cfg(not(windows))]
        let program = "/bin/sh".to_owned();
        #[cfg(windows)]
        let lead = "/C";
        #[cfg(not(windows))]
        let lead = "-c";
        let mut args = vec![lead.to_owned()];
        args.extend(arguments.iter().map(|argument| (*argument).to_owned()));
        (Some(program), args)
    }

    fn trivial_config(name: &str, arguments: &[&str]) -> PreviewServerConfig {
        let (command, args) = trivial_command(arguments);
        PreviewServerConfig {
            name: name.to_owned(),
            command,
            args,
            cwd: PathBuf::new(),
            port: 0,
            env: BTreeMap::new(),
            auto_port: Some(true),
            url: None,
        }
    }

    /// A process that dies inside the gate is a failed start, and its row and buffer
    /// must be gone: a `preview_list` that still shows it would offer a stop button
    /// for a process that no longer exists.
    #[test]
    fn a_child_that_dies_inside_the_gate_fails_the_start_and_leaves_nothing_behind() {
        let registry = PreviewServerRegistry::default();
        let worktree = std::env::temp_dir();
        let error = registry
            .start(&worktree, &trivial_config("doomed", &["exit 3"]), None)
            .expect_err("the child exits immediately");
        assert_eq!(error.kind, PreviewStartErrorKind::EarlyExit);
        assert_eq!(error.exit_code, Some(3));
        assert!(error
            .message
            .starts_with("The dev server exited during startup (code 3)."));
        assert!(registry.servers().is_empty());
    }

    /// The real spawn path has to produce the same ENOENT the classification table was
    /// written for, or the message never reaches the model.
    #[test]
    fn a_missing_command_is_reported_as_enoent_from_the_real_spawn() {
        let registry = PreviewServerRegistry::default();
        let mut config = trivial_config("missing", &[]);
        config.command = Some("mework-no-such-dev-server".to_owned());
        config.args = Vec::new();
        let error = registry
            .start(&std::env::temp_dir(), &config, None)
            .expect_err("nothing by that name exists");
        assert_eq!(error.code.as_deref(), Some("ENOENT"));
        assert!(error
            .message
            .starts_with("Command not found: `mework-no-such-dev-server`."));
        assert!(registry.servers().is_empty());
    }

    /// The whole point of the gate: survive it and the call returns a receipt while the
    /// process keeps running, and stopping it later kills the tree and clears the row.
    #[test]
    fn a_surviving_child_registers_and_then_stops() {
        let registry = PreviewServerRegistry::default();
        let worktree = std::env::temp_dir();
        let changes = Arc::new(AtomicBool::new(false));
        let flag = changes.clone();
        // Every stop this registry makes on purpose has to be named, because the renderer takes
        // down the preview page of a server that was stopped and leaves the page of one that died.
        let stopped_ids = Arc::new(Mutex::new(Vec::<String>::new()));
        let recorded = stopped_ids.clone();
        registry.set_change_listener(Arc::new(move |stopped: &[String]| {
            flag.store(true, Ordering::Release);
            recorded
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .extend(stopped.iter().cloned());
        }));

        #[cfg(windows)]
        let arguments = ["ping -n 30 127.0.0.1"];
        #[cfg(not(windows))]
        let arguments = ["sleep 30"];
        let started = registry
            .start(
                &worktree,
                &trivial_config("slow", &arguments),
                Some("chat-a"),
            )
            .expect("the child outlives the startup gate");

        assert_eq!(started.name, "slow");
        assert_eq!(started.status, PreviewServerStatus::Starting);
        assert_eq!(started.session_id.as_deref(), Some("chat-a"));
        assert!(changes.load(Ordering::Acquire));
        assert_eq!(registry.servers_for_worktree(&worktree).len(), 1);
        assert!(registry.get(&started.server_id).is_some());

        assert!(registry.stop(&started.server_id));
        assert!(registry.servers().is_empty());
        assert!(registry.logs(&started.server_id).is_empty());
        assert!(!registry.stop(&started.server_id));
        assert_eq!(
            *stopped_ids
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            vec![started.server_id.clone()]
        );
    }
}
