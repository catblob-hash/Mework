//! Language servers: spawning them, speaking the LSP base protocol to them,
//! and keeping the ones a workspace needs alive between calls.
//!
//! The process machinery is the same shape as [`crate::preview_servers`] — a
//! registry behind one mutex, a Windows job object so the whole tree dies
//! together, stderr drained so the child never blocks on a full pipe. What is
//! new is the framing: every existing pipe in this repo is newline-delimited
//! JSON, and LSP is `Content-Length: N\r\n\r\n<body>`, so the codec below is
//! written fresh rather than borrowed from [`crate::mcp`].
//!
//! Servers start lazily, on the first request for a file whose extension they
//! claim, and are keyed by `(machine, root, name)` so every conversation in a
//! workspace shares one `rust-analyzer` rather than paying its index cost again.
//!
//! A workspace on another machine gets its server *there*: the process is
//! started through that machine's shell transport — the same `wsl.exe` or
//! `ssh` invocation the remote file tools use — and speaks the base protocol
//! over the wrapper's pipes. That is how Claude Code has it when it runs on a
//! remote: the language server is a child of whatever runs where the code is,
//! never a host-side process guessing at files it cannot open. Only the
//! wrapper lives in this process; the paths, the `rootUri` and every
//! `file://` URI are the remote machine's.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicI64, Ordering},
        mpsc::{self, RecvTimeoutError, SyncSender},
        Arc, Mutex, MutexGuard,
    },
    thread,
    time::Duration,
};

use serde_json::{json, Map, Value};

use crate::{
    lsp_config::LspServerConfig,
    run_environment::ShellRunner,
    tool_executor::{kill_shell_child, ShellChild, ShellJob},
};

/// Where a language server runs: in this process, or on the machine a
/// workspace lives on.
#[derive(Clone, Debug)]
pub enum ServerHost {
    Local,
    Remote {
        /// The transport to that machine, with its variable table.
        runner: ShellRunner,
        /// `run_environment::env_key` of the machine — its identity everywhere
        /// else in the host, and the first component of every key here.
        machine_key: String,
    },
}

impl ServerHost {
    /// The machine component of a [`ServerRoot`]: `local` for this process,
    /// the machine's environment key otherwise.
    pub fn machine_key(&self) -> String {
        match self {
            Self::Local => crate::run_environment::env_key(None),
            Self::Remote { machine_key, .. } => machine_key.clone(),
        }
    }

    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local)
    }
}

/// The directory one server indexes, on the machine it indexes it.
///
/// Two machines can each have a `/srv/app`; a diagnostics fan-out or a registry
/// lookup that compared paths alone would hand one machine's compile errors to
/// a conversation working on the other.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ServerRoot {
    pub machine: String,
    pub path: PathBuf,
}

impl ServerRoot {
    pub fn local(path: impl Into<PathBuf>) -> Self {
        Self {
            machine: crate::run_environment::env_key(None),
            path: path.into(),
        }
    }

    pub fn new(host: &ServerHost, path: impl Into<PathBuf>) -> Self {
        Self {
            machine: host.machine_key(),
            path: path.into(),
        }
    }
}

/// Largest message body accepted from a server, matching the source's 32 MiB
/// cap. A server that declares more has desynchronized from the protocol.
const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;
/// Largest header block accepted before the blank line.
const MAX_HEADER_BYTES: usize = 64 * 1024;
/// Stderr kept per server for the settings row. Never reaches the model.
const MAX_STDERR_BYTES: usize = 64 * 1024;

/// Diagnostics volume limits, ported from the source: at most this many per
/// file and this many in total in one injection, and this many fingerprints
/// remembered per conversation so the same problem is not reported twice.
const MAX_DIAGNOSTICS_PER_FILE: usize = 10;
const MAX_DIAGNOSTICS_TOTAL: usize = 30;
const MAX_DELIVERED_FINGERPRINTS: usize = 500;
/// Longest rendered diagnostics body, after which it is cut with a marker.
const MAX_DIAGNOSTICS_CHARS: usize = 4_000;

/// JSON-RPC error code a server returns when the document changed under a
/// request. The source retries these three times with exponential backoff.
const CONTENT_MODIFIED: i64 = -32801;
const CONTENT_MODIFIED_RETRIES: u32 = 3;
const CONTENT_MODIFIED_BACKOFF: Duration = Duration::from_millis(500);

/// Wall clock one LSP request gets.
///
/// The source applies **no** per-request timeout: its tool call is driven by an
/// abort signal and a user who can press escape. Mework runs builtin tools in a
/// synchronous call slot, so an unbounded request would wedge the turn on any
/// server that simply never answers. This deadline is therefore a deliberate
/// deviation, not an omission.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Wall clock one outbound frame gets before the caller gives up on the pipe.
///
/// Distinct from [`REQUEST_TIMEOUT`]: that one bounds the *answer*, this one
/// bounds the *ask*. A server that stopped draining its stdin would otherwise
/// block the writer forever with no deadline anywhere.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a `shutdown`/`exit` handshake is given before the tree is killed.
const STOP_GRACE: Duration = Duration::from_millis(250);

// ---------------------------------------------------------------------------
// Base-protocol framing
// ---------------------------------------------------------------------------

/// Reads one `Content-Length`-framed message body.
///
/// Returns `Ok(None)` at a clean end of stream. Any other shape is an error:
/// the source kills the process on a protocol violation rather than trying to
/// resynchronize, because a server writing logs to stdout will never recover.
fn read_message(reader: &mut impl BufRead) -> Result<Option<Vec<u8>>, String> {
    let mut content_length: Option<usize> = None;
    let mut header_bytes = 0usize;
    loop {
        let mut line = Vec::new();
        let read = reader
            .take((MAX_HEADER_BYTES - header_bytes + 1) as u64)
            .read_until(b'\n', &mut line)
            .map_err(|error| format!("Failed to read from the language server: {error}"))?;
        if read == 0 {
            return if content_length.is_none() && header_bytes == 0 {
                Ok(None)
            } else {
                Err("The language server closed its output mid-message".into())
            };
        }
        header_bytes += read;
        if header_bytes > MAX_HEADER_BYTES {
            return Err(
                "The language server sent a header block larger than the protocol allows".into(),
            );
        }
        while matches!(line.last(), Some(b'\n' | b'\r')) {
            line.pop();
        }
        if line.is_empty() {
            break;
        }
        let Ok(text) = std::str::from_utf8(&line) else {
            return Err(
                "The language server sent non-protocol output in the header block — its stdout is desynchronized (logs on stdout instead of stderr?)"
                    .into(),
            );
        };
        let Some((name, value)) = text.split_once(':') else {
            return Err(
                "The language server sent non-protocol output in the header block — its stdout is desynchronized (logs on stdout instead of stderr?)"
                    .into(),
            );
        };
        if name.trim().eq_ignore_ascii_case("content-length") {
            content_length = value.trim().parse::<usize>().ok();
        }
    }
    let Some(length) = content_length else {
        return Err(
            "The language server sent a header block without a Content-Length — its stdout is desynchronized from the base protocol"
                .into(),
        );
    };
    if length > MAX_BODY_BYTES {
        return Err(
            "The language server declared a message larger than the size limit — refusing to buffer it".into(),
        );
    }
    let mut body = vec![0u8; length];
    reader
        .read_exact(&mut body)
        .map_err(|error| format!("Failed to read the language server's message body: {error}"))?;
    Ok(Some(body))
}

/// Frames one message for the base protocol: `Content-Length` then the body.
fn encode_message(payload: &Value) -> Result<Vec<u8>, String> {
    let body = serde_json::to_vec(payload)
        .map_err(|error| format!("Could not encode an LSP message: {error}"))?;
    let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    frame.extend_from_slice(&body);
    Ok(frame)
}

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

/// One problem a server reported, already flattened into what the injection
/// prints.
#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    pub severity: String,
    pub line: u64,
    pub character: u64,
    pub message: String,
    pub code: String,
    pub source: String,
}

/// `publishDiagnostics` severity integers, as the source maps them.
fn severity_name(value: Option<&Value>) -> String {
    match value.and_then(Value::as_u64) {
        Some(2) => "Warning",
        Some(3) => "Info",
        Some(4) => "Hint",
        _ => "Error",
    }
    .to_owned()
}

/// The latest published set for one file. LSP replaces the whole set per URI,
/// so this is assignment, never accumulation.
#[derive(Clone, Debug, Default)]
struct FileDiagnostics {
    diagnostics: Vec<Diagnostic>,
}

/// What one conversation is owed.
#[derive(Debug, Default)]
struct ConversationLedger {
    /// The roots this conversation has reached a language server in. A publish
    /// from any other root is not its business — without this, a conversation
    /// in one project would be handed another project's compile errors.
    roots: BTreeSet<ServerRoot>,
    pending: BTreeSet<String>,
    delivered: VecDeque<u64>,
}

/// Published diagnostics, and who has already been told about them.
///
/// A conversation has to register before it is owed anything: without that, a
/// conversation that never touched a language server would open with a backlog
/// of problems from somebody else's turn.
#[derive(Debug, Default)]
struct DiagnosticsLedger {
    current: HashMap<String, FileDiagnostics>,
    /// Insertion order of `current`, so the oldest file is evicted first.
    order: VecDeque<String>,
    conversations: HashMap<String, ConversationLedger>,
    /// Insertion order of `conversations`, same reason.
    conversation_order: VecDeque<String>,
}

/// Files whose latest diagnostics are kept. A session that touches more than
/// this many files drops the oldest rather than growing for its whole life.
const MAX_TRACKED_FILES: usize = 512;
/// Conversations tracked at once, evicted oldest-first.
const MAX_TRACKED_CONVERSATIONS: usize = 64;

impl DiagnosticsLedger {
    fn register(&mut self, conversation_id: &str, root: &ServerRoot) {
        if !self.conversations.contains_key(conversation_id) {
            self.conversation_order.push_back(conversation_id.to_owned());
            while self.conversation_order.len() > MAX_TRACKED_CONVERSATIONS {
                if let Some(oldest) = self.conversation_order.pop_front() {
                    self.conversations.remove(&oldest);
                }
            }
        }
        self.conversations
            .entry(conversation_id.to_owned())
            .or_default()
            .roots
            .insert(root.clone());
    }

    fn publish(&mut self, root: &ServerRoot, uri: String, diagnostics: Vec<Diagnostic>) {
        let empty = diagnostics.is_empty();
        if !self.current.contains_key(&uri) {
            self.order.push_back(uri.clone());
            while self.order.len() > MAX_TRACKED_FILES {
                if let Some(oldest) = self.order.pop_front() {
                    self.current.remove(&oldest);
                    for ledger in self.conversations.values_mut() {
                        ledger.pending.remove(&oldest);
                    }
                }
            }
        }
        self.current
            .insert(uri.clone(), FileDiagnostics { diagnostics });
        // An empty publish is how a server says "this file is clean now". It
        // clears the stored set but is not itself news, so it is not queued.
        if empty {
            for ledger in self.conversations.values_mut() {
                ledger.pending.remove(&uri);
            }
            return;
        }
        for ledger in self.conversations.values_mut() {
            if ledger.roots.contains(root) {
                ledger.pending.insert(uri.clone());
            }
        }
    }

    /// Drains what `conversation_id` has not been shown, applying the source's
    /// dedup and volume limits.
    fn take_new(&mut self, conversation_id: &str) -> Vec<(String, Vec<Diagnostic>)> {
        let Some(ledger) = self.conversations.get_mut(conversation_id) else {
            return Vec::new();
        };
        let pending = std::mem::take(&mut ledger.pending);
        let mut out: Vec<(String, Vec<Diagnostic>)> = Vec::new();
        let mut total = 0usize;
        for uri in pending {
            if total >= MAX_DIAGNOSTICS_TOTAL {
                break;
            }
            let Some(file) = self.current.get(&uri) else {
                continue;
            };
            let mut fresh = Vec::new();
            for diagnostic in &file.diagnostics {
                if fresh.len() >= MAX_DIAGNOSTICS_PER_FILE || total + fresh.len() >= MAX_DIAGNOSTICS_TOTAL
                {
                    break;
                }
                let fingerprint = fingerprint_diagnostic(&uri, diagnostic);
                if ledger.delivered.contains(&fingerprint) {
                    continue;
                }
                ledger.delivered.push_back(fingerprint);
                while ledger.delivered.len() > MAX_DELIVERED_FINGERPRINTS {
                    ledger.delivered.pop_front();
                }
                fresh.push(diagnostic.clone());
            }
            if !fresh.is_empty() {
                total += fresh.len();
                out.push((uri, fresh));
            }
        }
        out
    }
}

fn fingerprint_diagnostic(uri: &str, diagnostic: &Diagnostic) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    uri.hash(&mut hasher);
    diagnostic.severity.hash(&mut hasher);
    diagnostic.line.hash(&mut hasher);
    diagnostic.character.hash(&mut hasher);
    diagnostic.message.hash(&mut hasher);
    diagnostic.code.hash(&mut hasher);
    hasher.finish()
}

/// Renders what [`LspRegistry::take_new_diagnostics`] returned into the block
/// the model reads. Verbatim from the source, including the wrapper tag, the
/// per-line shape and the 4000-character cut.
pub fn render_diagnostics(files: &[(String, Vec<Diagnostic>)]) -> String {
    let body = files
        .iter()
        .map(|(uri, diagnostics)| {
            let name = uri.rsplit('/').next().unwrap_or(uri.as_str());
            let lines = diagnostics
                .iter()
                .map(|diagnostic| {
                    let code = if diagnostic.code.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}]", diagnostic.code)
                    };
                    let source = if diagnostic.source.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", diagnostic.source)
                    };
                    format!(
                        "  {} [Line {}:{}] {}{code}{source}",
                        diagnostic.severity,
                        diagnostic.line + 1,
                        diagnostic.character + 1,
                        diagnostic.message
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!("{name}:\n{lines}")
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let body = if body.chars().count() > MAX_DIAGNOSTICS_CHARS {
        let kept: String = body
            .chars()
            .take(MAX_DIAGNOSTICS_CHARS - 12)
            .collect::<String>();
        format!("{kept}…[truncated]")
    } else {
        body
    };
    format!("<new-diagnostics>The following new diagnostic issues were detected:\n\n{body}</new-diagnostics>")
}

// ---------------------------------------------------------------------------
// One live server
// ---------------------------------------------------------------------------

/// One outbound message and the slot its writer reports back through.
struct WriteRequest {
    bytes: Vec<u8>,
    reply: SyncSender<Result<(), String>>,
}

/// A running language server and everything needed to talk to it.
pub struct Connection {
    child: Mutex<ShellChild>,
    /// Behind a lock because the reader thread holds an `Arc<Connection>`, and
    /// a raw Windows job handle is `Send` but not `Sync`.
    job: Mutex<ShellJob>,
    /// Writes go to a dedicated thread rather than a `Mutex<ChildStdin>`.
    ///
    /// A server that stops draining its stdin makes `write_all` block forever.
    /// Under a mutex that would wedge the caller *and* the reader thread, which
    /// needs the same pipe to answer `workspace/configuration` — the two would
    /// deadlock with no deadline anywhere. A channel lets every sender give up
    /// on its own schedule and leaves exactly one thread blocked on the pipe.
    writes: SyncSender<WriteRequest>,
    next_id: AtomicI64,
    alive: Arc<AtomicBool>,
    pending: Arc<Mutex<HashMap<i64, SyncSender<Result<Value, String>>>>>,
    /// `uri` to the version last sent, which is also the open-document set.
    documents: Mutex<BTreeMap<String, i64>>,
    stderr: Arc<Mutex<String>>,
    /// Why the reader stopped, when it stopped for a reason worth reporting.
    failure: Arc<Mutex<Option<String>>>,
    shutdown_timeout: Duration,
    /// Fires once the stderr drain reaches end of file, so a failure report
    /// can wait for the server's last words rather than race the drain.
    stderr_done: Mutex<Option<mpsc::Receiver<()>>>,
}

impl Connection {
    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    fn send(&self, payload: &Value) -> Result<(), String> {
        if !self.is_alive() {
            return Err(self.failure_reason());
        }
        let bytes = encode_message(payload)?;
        let (reply, written) = mpsc::sync_channel(1);
        self.writes
            .try_send(WriteRequest { bytes, reply })
            .map_err(|_| {
                // The writer is either gone or still blocked on a previous
                // frame the server never drained.
                format!(
                    "{} (its input pipe is not being read)",
                    self.failure_reason()
                )
            })?;
        match written.recv_timeout(WRITE_TIMEOUT) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => {
                Err("The language server stopped reading its input".to_owned())
            }
            Err(RecvTimeoutError::Disconnected) => Err(self.failure_reason()),
        }
    }

    fn failure_reason(&self) -> String {
        self.failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .unwrap_or_else(|| "The language server is no longer running".to_owned())
    }

    fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }

    fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = mpsc::sync_channel(1);
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id, sender);
        let sent = self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
        if let Err(error) = sent {
            self.pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&id);
            return Err(error);
        }
        let outcome = match receiver.recv_timeout(timeout) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(format!(
                "The language server did not answer '{method}' within {} seconds",
                timeout.as_secs()
            )),
            Err(RecvTimeoutError::Disconnected) => Err(self.failure_reason()),
        };
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&id);
        outcome
    }

    /// Sends a request, retrying the one error the source retries.
    pub fn request_with_retry(&self, method: &str, params: Value) -> Result<Value, String> {
        let mut attempt = 0u32;
        loop {
            match self.request(method, params.clone(), REQUEST_TIMEOUT) {
                Err(error)
                    if error.starts_with(CONTENT_MODIFIED_MARKER)
                        && attempt < CONTENT_MODIFIED_RETRIES =>
                {
                    thread::sleep(CONTENT_MODIFIED_BACKOFF * 2u32.pow(attempt));
                    attempt += 1;
                }
                other => return other.map_err(strip_marker),
            }
        }
    }

    /// Makes sure the server has `path` open at `text`, opening it the first
    /// time and sending an incremental full-text change afterwards.
    ///
    /// The document map is held across the notification on purpose. Releasing
    /// it first lets two concurrent calls assign versions 1 and 2 and then
    /// write them in the other order, so the server sees `didChange` for a
    /// document it was never told to open.
    pub fn sync_document(&self, path: &Path, language_id: &str, text: &str) -> Result<(), String> {
        let uri = path_to_uri(path);
        let mut documents = self
            .documents
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let known = documents.get(&uri).copied();
        let version = known.unwrap_or(0) + 1;
        let sent = match known {
            None => self.notify(
                "textDocument/didOpen",
                json!({
                    "textDocument": {
                        "uri": uri,
                        "languageId": language_id,
                        "version": version,
                        "text": text,
                    }
                }),
            ),
            Some(_) => self.notify(
                "textDocument/didChange",
                json!({
                    "textDocument": { "uri": uri, "version": version },
                    "contentChanges": [{ "text": text }],
                }),
            ),
        };
        // Only record the version the server was actually told about, so a
        // failed write does not leave this side believing the file is open.
        if sent.is_ok() {
            documents.insert(uri, version);
        }
        sent
    }

    /// Whether this server already holds `path`.
    pub fn has_document(&self, path: &Path) -> bool {
        self.documents
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains_key(&path_to_uri(path))
    }

    /// The last lines the server wrote to stderr, for the settings row. Never
    /// reaches the model.
    pub fn diagnostics_log(&self) -> String {
        self.stderr
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

/// Prefix the reader puts on a `ContentModified` failure so the retry loop can
/// recognize it without a second error type crossing the channel.
const CONTENT_MODIFIED_MARKER: &str = "\u{1}content-modified\u{1}";

/// Strips the retry marker before a failure reaches the model.
fn strip_marker(error: String) -> String {
    error
        .strip_prefix(CONTENT_MODIFIED_MARKER)
        .map(str::to_owned)
        .unwrap_or(error)
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Identity of one server instance: the root it indexes — machine included —
/// and the name it was configured under. Two workspaces get two
/// `rust-analyzer` processes; two conversations in one workspace share one.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct ServerKey {
    root: ServerRoot,
    name: String,
}

struct ServerEntry {
    config: LspServerConfig,
    connection: Option<Arc<Connection>>,
    /// Set once the server has failed in a way that should not be retried this
    /// session, so a broken command is reported rather than respawned per call.
    fatal: Option<String>,
    restarts: u32,
}

#[derive(Default)]
struct Registry {
    servers: BTreeMap<ServerKey, ServerEntry>,
    diagnostics: DiagnosticsLedger,
}

/// Host-owned language servers. Cloneable handle over one registry, like
/// [`crate::preview_servers::PreviewServerRegistry`].
#[derive(Clone, Default)]
pub struct LspRegistry {
    inner: Arc<Mutex<Registry>>,
}

impl LspRegistry {
    fn lock(&self) -> MutexGuard<'_, Registry> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Notes that `conversation_id` is now owed diagnostics. Called the first
    /// time a conversation reaches a language server, so a conversation that
    /// never uses one is never handed a backlog.
    pub fn register_conversation(&self, conversation_id: &str, root: &ServerRoot) {
        self.lock().diagnostics.register(conversation_id, root);
    }

    /// Drains the diagnostics `conversation_id` has not been shown.
    pub fn take_new_diagnostics(&self, conversation_id: &str) -> Vec<(String, Vec<Diagnostic>)> {
        self.lock().diagnostics.take_new(conversation_id)
    }

    /// Whether any server is currently up.
    ///
    /// The edit hook asks this before resolving configuration off disk: with no
    /// server running there is nobody to notify, and a `write` should not pay
    /// two file reads to discover that.
    pub fn has_running_servers(&self) -> bool {
        self.lock().servers.values().any(|entry| {
            entry
                .connection
                .as_ref()
                .is_some_and(|connection| connection.is_alive())
        })
    }

    /// Picks the server that claims `path`, out of the ones configured for this
    /// run. The first configuration claiming an extension wins, as in the
    /// source; `configs` therefore arrives in precedence order.
    pub fn config_for_path<'a>(
        configs: &'a [LspServerConfig],
        path: &Path,
    ) -> Option<(&'a LspServerConfig, String)> {
        let extension = path
            .extension()
            .map(|value| crate::lsp_config::normalize_extension(&value.to_string_lossy()))?;
        configs.iter().find_map(|config| {
            config
                .extension_to_language
                .get(&extension)
                .map(|language| (config, language.clone()))
        })
    }

    /// Returns a live server for `path`, starting it if this is the first call
    /// that needs it, together with the language id its configuration gives the
    /// file and the root that server indexes. `workspace` is the root a server
    /// with no `workspaceFolder` of its own is started in, and `host` is the
    /// machine both of them are on.
    ///
    /// The root is handed back rather than left implicit because the diagnostics
    /// ledger fans out by it: a caller that registers under `workspace` while
    /// the server publishes under its configured `workspaceFolder` is registered
    /// for a root nothing ever publishes to.
    pub fn connection_for(
        &self,
        host: &ServerHost,
        configs: &[LspServerConfig],
        workspace: &Path,
        path: &Path,
    ) -> Result<(Arc<Connection>, String, ServerRoot), String> {
        let Some((config, language_id)) = Self::config_for_path(configs, path) else {
            return Err(no_server_for(path));
        };
        let root = ServerRoot::new(host, server_root(config, workspace));
        let connection = self.ensure_started(host, config, &root)?;
        Ok((connection, language_id, root))
    }

    /// Returns a live connection for `config` rooted at `root`, starting or
    /// restarting as needed.
    fn ensure_started(
        &self,
        host: &ServerHost,
        config: &LspServerConfig,
        root: &ServerRoot,
    ) -> Result<Arc<Connection>, String> {
        let key = ServerKey {
            root: root.clone(),
            name: config.name.clone(),
        };

        // Fast path under the registry lock: an already-live connection.
        //
        // A live connection is consulted BEFORE `fatal`. The two can coexist:
        // one call's start can fail while another's succeeded, and a latched
        // reason must never hide a server that is up and answering.
        {
            let mut registry = self.lock();
            let entry = registry
                .servers
                .entry(key.clone())
                .or_insert_with(|| ServerEntry {
                    config: config.clone(),
                    connection: None,
                    fatal: None,
                    restarts: 0,
                });
            if let Some(connection) = &entry.connection {
                if connection.is_alive() {
                    return Ok(Arc::clone(connection));
                }
                // The process died since the last call. Whether that earns a
                // restart is the config's call, and the budget is finite.
                entry.connection = None;
                if !entry.config.restart_on_crash {
                    let reason = format!(
                        "The language server '{}' stopped and restartOnCrash is false",
                        config.name
                    );
                    entry.fatal = Some(reason.clone());
                    return Err(reason);
                }
                if entry.restarts >= entry.config.max_restarts {
                    let reason = format!(
                        "The language server '{}' crashed more than {} times and was given up on",
                        config.name, entry.config.max_restarts
                    );
                    entry.fatal = Some(reason.clone());
                    return Err(reason);
                }
                entry.restarts += 1;
            } else if let Some(reason) = &entry.fatal {
                return Err(reason.clone());
            }
        }

        // Spawning and the initialize handshake happen outside the registry
        // lock: a cold server takes seconds, and holding the lock would stall
        // every other conversation's language server too.
        let started = start_server(host, config, root, Arc::downgrade(&self.inner));
        let mut registry = self.lock();
        let entry = registry
            .servers
            .entry(key)
            .or_insert_with(|| ServerEntry {
                config: config.clone(),
                connection: None,
                fatal: None,
                restarts: 0,
            });
        match started {
            Ok(connection) => {
                // Two calls can reach the spawn at once, because it deliberately
                // happens outside the registry lock. The loser stops the process
                // it just started rather than dropping it on the floor: only
                // Windows has a kill-on-close backstop, so on every other
                // platform a dropped `Connection` is a leaked language server.
                if let Some(existing) = entry
                    .connection
                    .as_ref()
                    .filter(|existing| existing.is_alive())
                    .cloned()
                {
                    drop(registry);
                    stop_connection(&connection);
                    return Ok(existing);
                }
                entry.connection = Some(Arc::clone(&connection));
                // A start that worked clears whatever a previous one latched:
                // the reason is no longer true of this entry.
                entry.fatal = None;
                Ok(connection)
            }
            Err(reason) => {
                // A command that is not there will not be there next call
                // either, so this is latched rather than retried per request.
                //
                // Not latched over a live connection: a concurrent call may
                // have started one while this attempt was failing, and that
                // server is working. Latching there would disable code
                // navigation for the rest of the session over a race.
                if !entry
                    .connection
                    .as_ref()
                    .is_some_and(|existing| existing.is_alive())
                {
                    entry.fatal = Some(reason.clone());
                }
                Err(reason)
            }
        }
    }

    /// Tells the server that claims `path` that it changed on disk, so both the
    /// diagnostics injected next round and the next navigation call describe the
    /// file as it is now.
    ///
    /// Silent on every failure: an edit must not fail because a language server
    /// is unhappy.
    pub fn notify_file_changed(
        &self,
        configs: &[LspServerConfig],
        workspace: &Path,
        path: &Path,
        conversation_id: &str,
    ) {
        let Some((config, language_id)) = Self::config_for_path(configs, path) else {
            return;
        };
        let root = ServerRoot::local(server_root(config, workspace));
        // `diagnostics: false` suppresses only the reporting half. The document
        // still has to be re-synced, or the server keeps answering navigation
        // from the text as it was before this edit.
        if config.diagnostics {
            self.register_conversation(conversation_id, &root);
        }
        // Only a server that is already up: an edit is not a reason to pay a
        // cold `rust-analyzer` start, and the next navigation call will.
        let connection = {
            let registry = self.lock();
            registry
                .servers
                .get(&ServerKey {
                    root,
                    name: config.name.clone(),
                })
                .and_then(|entry| entry.connection.clone())
        };
        let Some(connection) = connection else {
            return;
        };
        if !connection.is_alive() || !connection.has_document(path) {
            // Nothing to re-sync: the next call opens it at its current text.
            return;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        resync(&connection, path, &language_id, &text);
    }

    /// The remote counterpart of [`Self::notify_file_changed`]: a file on
    /// `machine_key` changed, and whichever live server there holds it is told.
    ///
    /// No configuration is resolved for this. A server that is up already knows
    /// which extensions it claims, and the document map says whether it holds
    /// this one; reading two `lsp.json` files off another machine to learn what
    /// the registry can answer from memory would be a round trip per `write`.
    /// `read_text` is asked for the new contents only once a holder is found,
    /// so a write on a machine with no live server costs nothing extra.
    pub fn notify_remote_file_changed(
        &self,
        machine_key: &str,
        path: &Path,
        conversation_id: &str,
        read_text: &dyn Fn() -> Option<String>,
    ) {
        let holders: Vec<(Arc<Connection>, String, ServerRoot, bool)> = {
            let registry = self.lock();
            registry
                .servers
                .iter()
                .filter(|(key, _)| key.root.machine == machine_key)
                .filter_map(|(key, entry)| {
                    let connection = entry.connection.clone()?;
                    let (_, language_id) =
                        Self::config_for_path(std::slice::from_ref(&entry.config), path)?;
                    Some((connection, language_id, key.root.clone(), entry.config.diagnostics))
                })
                .collect()
        };
        let mut text: Option<String> = None;
        for (connection, language_id, root, diagnostics) in holders {
            if !connection.is_alive() || !connection.has_document(path) {
                continue;
            }
            if diagnostics {
                self.register_conversation(conversation_id, &root);
            }
            if text.is_none() {
                text = read_text();
            }
            let Some(text) = text.as_deref() else {
                return;
            };
            resync(&connection, path, &language_id, text);
        }
    }

    /// Stops every server. Called on app exit and on document reset.
    pub fn stop_all(&self) {
        let entries: Vec<Arc<Connection>> = {
            let mut registry = self.lock();
            let connections = registry
                .servers
                .values_mut()
                .filter_map(|entry| entry.connection.take())
                .collect();
            registry.servers.clear();
            registry.diagnostics = DiagnosticsLedger::default();
            connections
        };
        for connection in entries {
            stop_connection(&connection);
        }
    }
}

/// The directory one server indexes: its own `workspaceFolder` when the entry
/// names one, the conversation's workspace otherwise.
///
/// Every caller has to agree on this, not just the one that starts the process:
/// the registry key, the `rootUri` in `initialize`, and the root the diagnostics
/// ledger fans out by are all this same value.
fn server_root(config: &LspServerConfig, workspace: &Path) -> PathBuf {
    if config.workspace_folder.trim().is_empty() {
        workspace.to_path_buf()
    } else {
        PathBuf::from(config.workspace_folder.trim())
    }
}

/// Re-sends a document the server already holds, then tells it the file was
/// saved. Failures are the server's problem, not the edit's.
fn resync(connection: &Connection, path: &Path, language_id: &str, text: &str) {
    let _ = connection.sync_document(path, language_id, text);
    let _ = connection.notify(
        "textDocument/didSave",
        json!({ "textDocument": { "uri": path_to_uri(path) } }),
    );
}

/// The sentence a call gets when nothing is configured for the file it named.
fn no_server_for(path: &Path) -> String {
    let extension = path
        .extension()
        .map(|value| crate::lsp_config::normalize_extension(&value.to_string_lossy()))
        .unwrap_or_else(|| "(none)".to_owned());
    format!(
        "No LSP server available for file type: {extension}. Configure one in .mework/lsp.json under \"lspServers\", or install a language server Mework already knows (rust-analyzer, typescript-language-server, pyright, gopls, clangd, lua-language-server, bash-language-server)."
    )
}

/// `file://` URI for an absolute path, in the spelling the source produces:
/// forward slashes, a leading slash before a Windows drive letter.
///
/// Windows' extended-length spellings are normalized away first, and that part
/// is a deliberate deviation. The source never meets them: Node's
/// `path.resolve` does not produce `\\?\`. Mework's path guard hands this
/// function `fs::canonicalize`'s answer, which on Windows always is
/// `\\?\C:\…` — and for a network location `\\?\UNC\server\share\…`. Replacing
/// the backslashes alone turns the first into `//?/C:/…`, which already starts
/// with a slash, so the leading-slash branch below does not fire and the result
/// is `file:////?/C:/…`: four slashes, an empty authority, a URI no server can
/// turn back into a path. A real `rust-analyzer` answers every request naming
/// one with `-32603 url is not a file`, which is every `lsp` call on Windows.
pub fn path_to_uri(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    // `\\?\UNC\server\share` and `\\server\share` name the same location, and
    // both reach a URI the same way: the host belongs in the authority.
    let text = match text.strip_prefix("//?/") {
        Some(rest) => match strip_ascii_case_insensitive_prefix(rest, "UNC/") {
            Some(share) => format!("//{share}"),
            None => rest.to_owned(),
        },
        None => text,
    };
    // `//server/share/…` is already authority-and-path, `/home/…` needs the
    // empty authority spelled out, and `C:/…` needs it plus the root slash.
    let prefix = if text.starts_with("//") {
        "file:"
    } else if text.starts_with('/') {
        "file://"
    } else {
        "file:///"
    };
    let mut encoded = String::with_capacity(text.len() + prefix.len());
    encoded.push_str(prefix);
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'.' | b'_' | b'~' | b':' => {
                encoded.push(byte as char)
            }
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

/// A `file://` URI back to a path. Mirrors the source's decoder, including the
/// leading-slash strip in front of a Windows drive letter.
pub fn uri_to_path(uri: &str) -> String {
    let Some(rest) = uri.strip_prefix("file://") else {
        return percent_decode(uri);
    };
    // An authority means a network location. Dropping its two slashes with the
    // scheme would turn `file://server/share/x` into the relative path
    // `server/share/x`, which resolves against whatever the process's current
    // directory happens to be.
    if !rest.starts_with('/') {
        return format!("//{}", percent_decode(rest));
    }
    let decoded = percent_decode(rest);
    let bytes = decoded.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':' {
        decoded[1..].to_owned()
    } else {
        decoded
    }
}

/// `\\?\UNC\` is spelled in either case by different Windows APIs.
fn strip_ascii_case_insensitive_prefix<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &text[prefix.len()..])
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let high = (bytes[index + 1] as char).to_digit(16);
            let low = (bytes[index + 2] as char).to_digit(16);
            if let (Some(high), Some(low)) = (high, low) {
                out.push((high * 16 + low) as u8);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Spawns one server and completes the `initialize`/`initialized` handshake.
fn start_server(
    host: &ServerHost,
    config: &LspServerConfig,
    root: &ServerRoot,
    sink: std::sync::Weak<Mutex<Registry>>,
) -> Result<Arc<Connection>, String> {
    let child = match host {
        ServerHost::Local => ShellChild::Local(spawn_local(config, &root.path)?),
        ServerHost::Remote { runner, .. } => spawn_remote(runner, config, &root.path)?,
    };
    attach_server(child, host, config, root, sink)
}

/// Takes a freshly spawned server (or the wrapper relaying one), wires its
/// pipes, and completes the handshake. Separate from the spawn so the remote
/// leg's script can be driven through any shell in a test.
fn attach_server(
    mut child: ShellChild,
    host: &ServerHost,
    config: &LspServerConfig,
    root: &ServerRoot,
    sink: std::sync::Weak<Mutex<Registry>>,
) -> Result<Arc<Connection>, String> {
    // Before it can spawn grandchildren of its own. A server the agent runs
    // is already its own process group on the machine; nothing here owns it.
    let job = ShellJob::create();
    if let ShellChild::Local(local) = &child {
        job.assign(local);
    }

    let stdin = child
        .take_stdin()
        .ok_or_else(|| "Could not acquire the language server's stdin".to_owned())?;
    let stdout = child
        .take_stdout()
        .ok_or_else(|| "Could not acquire the language server's stdout".to_owned())?;
    let stderr = child
        .take_stderr()
        .ok_or_else(|| "Could not acquire the language server's stderr".to_owned())?;

    let (writes, write_requests) = mpsc::sync_channel::<WriteRequest>(1);
    thread::spawn(move || {
        let mut stdin = stdin;
        while let Ok(request) = write_requests.recv() {
            let result = stdin
                .write_all(&request.bytes)
                .and_then(|()| stdin.flush())
                .map_err(|error| format!("Failed to write to the language server: {error}"));
            let failed = result.is_err();
            let _ = request.reply.send(result);
            if failed {
                return;
            }
        }
    });

    let connection = Arc::new(Connection {
        child: Mutex::new(child),
        job: Mutex::new(job),
        writes,
        next_id: AtomicI64::new(1),
        alive: Arc::new(AtomicBool::new(true)),
        pending: Arc::new(Mutex::new(HashMap::new())),
        documents: Mutex::new(BTreeMap::new()),
        stderr: Arc::new(Mutex::new(String::new())),
        failure: Arc::new(Mutex::new(None)),
        shutdown_timeout: Duration::from_millis(config.shutdown_timeout_millis),
        stderr_done: Mutex::new(None),
    });

    let stderr_done = spawn_stderr_drain(
        stderr,
        Arc::clone(&connection.stderr),
        matches!(host, ServerHost::Remote { runner: ShellRunner::Wsl { .. }, .. }),
    );
    *connection
        .stderr_done
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(stderr_done);
    spawn_reader(
        stdout,
        Arc::clone(&connection.alive),
        Arc::clone(&connection.pending),
        Arc::clone(&connection.failure),
        Arc::downgrade(&connection),
        config.clone(),
        root.clone(),
        sink,
    );

    match handshake(&connection, host, config, &root.path) {
        Ok(()) => Ok(connection),
        Err(error) => {
            stop_connection(&connection);
            // Whatever the server (or the wrapper that could not reach it)
            // said on stderr is the reason more often than the timeout is: a
            // command not on the remote PATH, an SSH key the machine refused.
            // The process is gone, so its stderr is at end of file; give the
            // drain a moment to have read it.
            let done = connection
                .stderr_done
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take();
            if let Some(done) = done {
                let _ = done.recv_timeout(STDERR_SETTLE);
            }
            let tail = last_stderr_line(&connection.diagnostics_log());
            Err(if tail.is_empty() {
                error
            } else {
                format!("{error} ({tail})")
            })
        }
    }
}

/// How long a failed start waits for the stderr drain to finish after the
/// process is gone.
const STDERR_SETTLE: Duration = Duration::from_millis(500);

/// The last non-empty line a server wrote to stderr, cut to a length that fits
/// in an error sentence.
fn last_stderr_line(log: &str) -> String {
    const MAX_TAIL_CHARS: usize = 240;
    let line = log
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    if line.chars().count() > MAX_TAIL_CHARS {
        let kept: String = line.chars().take(MAX_TAIL_CHARS).collect();
        format!("{kept}…")
    } else {
        line.to_owned()
    }
}

/// Starts the server as a child of this process, in `root` when it exists.
fn spawn_local(config: &LspServerConfig, root: &Path) -> Result<Child, String> {
    let resolved = crate::environment_tools::resolve_on_path(&config.command)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| config.command.clone());
    let mut process = Command::new(&resolved);
    process
        .args(&config.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if root.is_dir() {
        process.current_dir(root);
    }
    for (name, value) in &config.env {
        process.env(name, value);
    }
    // A language server needs the developer's toolchain environment, so this
    // inherits rather than using the MCP allowlist — but the harness secrets
    // every child spawn strips are still stripped.
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
        // Its own session, so the negative-pid group kill reaches the
        // proc-macro and build-script children a server starts.
        unsafe {
            process.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    process.spawn().map_err(|error| {
        format!(
            "Could not start the language server '{}' ({}): {error}",
            config.name, config.command
        )
    })
}

/// Starts the server on the machine `runner` reaches, in `root` there.
///
/// What this process holds is the `wsl.exe` or `ssh` wrapper; the server is its
/// grandchild on the other side, and the wrapper relays both pipes verbatim, so
/// the base protocol is spoken through it unchanged. The wrapper's own
/// environment table is applied by the transport ([`ShellRunner`]), the entry's
/// `env` by the script — and `exec` makes the server the process the wrapper
/// is talking to, so `shutdown`/`exit` reach it and its end of the pipe closes
/// when it goes.
fn spawn_remote(
    runner: &ShellRunner,
    config: &LspServerConfig,
    root: &Path,
) -> Result<ShellChild, String> {
    let script = match runner.script_dialect() {
        crate::shell_backend::ScriptDialect::Posix => remote_launch_script(config, root)?,
        crate::shell_backend::ScriptDialect::PowerShell => remote_launch_powershell(config, root)?,
    };
    let failed = |error: String| {
        format!(
            "Could not start the language server '{}' ({}) on the remote machine: {error}",
            config.name, config.command
        )
    };
    // An SSH machine the agent serves runs the server itself: the server then
    // belongs to the machine's agent, not to an SSH session, and a dropped
    // link pauses its conversation instead of ending it.
    let argv = runner
        .agent_shell()
        .cloned()
        .unwrap_or_default()
        .script_argv(&script);
    if let Some(spawned) = crate::remote_link::spawn(
        runner,
        argv,
        None,
        remote_agent::protocol::StdinMode::Pipe,
        "lsp",
    ) {
        return spawned
            .map(|child| ShellChild::Remote {
                child,
                killed: false,
            })
            .map_err(failed);
    }
    crate::run_environment::spawn_remote_script(runner, &script, true)
        .map(ShellChild::Local)
        .map_err(failed)
}

/// The script that starts one configured server on a remote machine.
///
/// Every fragment is single-quoted: the command, its arguments and the entry's
/// environment all come from an `lsp.json` a repository may have shipped, and
/// none of them may rewrite the script. A command that is not on the remote
/// PATH is reported by name before anything is executed, because the wrapper
/// exiting with `127` would otherwise read as "the language server exited".
pub fn remote_launch_script(config: &LspServerConfig, root: &Path) -> Result<String, String> {
    use crate::run_environment::{quote_remote_path, sh_single_quote};

    fn check(text: &str, label: &str) -> Result<(), String> {
        if text.trim().is_empty() {
            return Err(format!("The language server's {label} is empty"));
        }
        if text.chars().any(char::is_control) {
            return Err(format!(
                "The language server's {label} contains control characters"
            ));
        }
        Ok(())
    }

    let root = root.to_string_lossy();
    check(&root, "workspace root")?;
    check(&config.command, "command")?;
    let mut script = String::with_capacity(512);
    script.push_str(&format!(
        "cd -- {} || exit 64\n",
        quote_remote_path(root.trim())
    ));
    script.push_str(&format!(
        "command -v {cmd} >/dev/null 2>&1 || {{ printf '%s\\n' {message} >&2; exit 127; }}\n",
        cmd = sh_single_quote(&config.command),
        message = sh_single_quote(&format!(
            "{}: command not found on the remote machine's PATH",
            config.command
        )),
    ));
    script.push_str("exec ");
    if !config.env.is_empty() {
        script.push_str("env ");
        for (name, value) in &config.env {
            crate::run_environment::validate_env_var_name(name)?;
            // Empty is a legal value (`RUST_LOG=`); only control characters
            // cannot be quoted into safety.
            if value.chars().any(char::is_control) {
                return Err(
                    "The language server's environment values contain control characters".into(),
                );
            }
            script.push_str(&sh_single_quote(&format!("{name}={value}")));
            script.push(' ');
        }
    }
    script.push_str(&sh_single_quote(&config.command));
    for argument in &config.args {
        if argument.chars().any(char::is_control) {
            return Err("The language server's arguments contain control characters".into());
        }
        script.push(' ');
        script.push_str(&sh_single_quote(argument));
    }
    script.push('\n');
    Ok(script)
}

/// [`remote_launch_script`] for a Windows machine whose agent shell is
/// PowerShell, with the same checks on everything a repository's `lsp.json`
/// supplies and the same exit codes (64, 127).
pub fn remote_launch_powershell(config: &LspServerConfig, root: &Path) -> Result<String, String> {
    let root = root.to_string_lossy();
    for (text, label) in [(root.as_ref(), "workspace root"), (config.command.as_str(), "command")] {
        if text.trim().is_empty() {
            return Err(format!("The language server's {label} is empty"));
        }
        if text.chars().any(char::is_control) {
            return Err(format!("The language server's {label} contains control characters"));
        }
    }
    let mut env = Vec::with_capacity(config.env.len());
    for (name, value) in &config.env {
        crate::run_environment::validate_env_var_name(name)?;
        if value.chars().any(char::is_control) {
            return Err("The language server's environment values contain control characters".into());
        }
        env.push((name.clone(), value.clone()));
    }
    if config.args.iter().any(|argument| argument.chars().any(char::is_control)) {
        return Err("The language server's arguments contain control characters".into());
    }
    Ok(crate::remote_powershell::lsp_launch(
        root.trim(),
        &config.command,
        &config.args,
        &env,
    ))
}

fn handshake(
    connection: &Connection,
    host: &ServerHost,
    config: &LspServerConfig,
    root: &Path,
) -> Result<(), String> {
    let root_uri = path_to_uri(root);
    let name = root
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.to_string_lossy().into_owned());
    // `processId` is the parent a server may watch and exit with. This
    // process is that parent only when the server runs here; on another
    // machine the number would name whatever happens to hold that pid there.
    let process_id = if host.is_local() {
        json!(std::process::id())
    } else {
        Value::Null
    };
    let params = json!({
        "processId": process_id,
        "clientInfo": { "name": "Mework", "version": env!("CARGO_PKG_VERSION") },
        "initializationOptions": config
            .initialization_options
            .clone()
            .unwrap_or(Value::Object(Map::new())),
        "workspaceFolders": [{ "uri": root_uri, "name": name }],
        "rootPath": root.to_string_lossy(),
        "rootUri": root_uri,
        "capabilities": client_capabilities(config),
    });
    connection
        .request(
            "initialize",
            params,
            Duration::from_millis(config.startup_timeout_millis),
        )
        .map_err(|error| {
            format!(
                "The language server '{}' failed to initialize: {}",
                config.name,
                strip_marker(error)
            )
        })?;
    connection.notify("initialized", json!({}))?;
    if let Some(settings) = &config.settings {
        let _ = connection.notify(
            "workspace/didChangeConfiguration",
            json!({ "settings": settings }),
        );
    }
    Ok(())
}

/// The client capabilities literal, ported from the source.
///
/// Every `dynamicRegistration: false` and `workspaceFolders: false` here is
/// load-bearing: they are what keep a server from sending the client requests
/// this client does not implement.
fn client_capabilities(config: &LspServerConfig) -> Value {
    json!({
        "workspace": {
            "configuration": config.settings.is_some(),
            "workspaceFolders": false,
        },
        "textDocument": {
            "synchronization": {
                "dynamicRegistration": false,
                "willSave": false,
                "willSaveWaitUntil": false,
                "didSave": true,
            },
            "publishDiagnostics": {
                "relatedInformation": true,
                "tagSupport": { "valueSet": [1, 2] },
                "versionSupport": false,
                "codeDescriptionSupport": true,
                "dataSupport": false,
            },
            "hover": {
                "dynamicRegistration": false,
                "contentFormat": ["markdown", "plaintext"],
            },
            "definition": { "dynamicRegistration": false, "linkSupport": true },
            "references": { "dynamicRegistration": false },
            "implementation": { "dynamicRegistration": false, "linkSupport": true },
            "documentSymbol": {
                "dynamicRegistration": false,
                "hierarchicalDocumentSymbolSupport": true,
            },
            "callHierarchy": { "dynamicRegistration": false },
        },
        "workspace_symbol": { "dynamicRegistration": false },
        "general": { "positionEncodings": ["utf-16"] },
    })
}

/// Drains stderr into the settings-row log. `wsl` marks a `wsl.exe` wrapper,
/// whose own complaints (a distribution that is not there) arrive as UTF-16LE
/// even though everything the server itself writes is UTF-8. The returned
/// receiver fires when the pipe reaches end of file.
fn spawn_stderr_drain(
    stderr: Box<dyn Read + Send>,
    sink: Arc<Mutex<String>>,
    wsl: bool,
) -> mpsc::Receiver<()> {
    let (done, finished) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        // A `wsl.exe` complaint is UTF-16LE, and a `\n` byte falls in the
        // middle of its code units: decoding line by line would leave every
        // line after the first misaligned. Its stream is kept raw and decoded
        // whole each time instead, cut at an even offset so the alignment
        // survives the trim.
        let mut raw: Vec<u8> = Vec::new();
        loop {
            let mut line = Vec::new();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => {
                    let _ = done.send(());
                    return;
                }
                Ok(_) => {}
            }
            let mut log = sink.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if wsl {
                raw.extend_from_slice(&line);
                if raw.len() > MAX_STDERR_BYTES {
                    let cut = (raw.len() - MAX_STDERR_BYTES) & !1;
                    raw.drain(..cut);
                }
                *log = crate::run_environment::decode_wsl_output(&raw);
                continue;
            }
            log.push_str(&String::from_utf8_lossy(&line));
            if log.len() > MAX_STDERR_BYTES {
                let cut = log.len() - MAX_STDERR_BYTES;
                let boundary = (cut..log.len())
                    .find(|index| log.is_char_boundary(*index))
                    .unwrap_or(log.len());
                *log = log[boundary..].to_owned();
            }
        }
    });
    finished
}

#[allow(clippy::too_many_arguments)]
fn spawn_reader(
    stdout: Box<dyn Read + Send>,
    alive: Arc<AtomicBool>,
    pending: Arc<Mutex<HashMap<i64, SyncSender<Result<Value, String>>>>>,
    failure: Arc<Mutex<Option<String>>>,
    connection: std::sync::Weak<Connection>,
    config: LspServerConfig,
    root: ServerRoot,
    sink: std::sync::Weak<Mutex<Registry>>,
) {
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let stop = |reason: Option<String>| {
            alive.store(false, Ordering::SeqCst);
            if let Some(reason) = reason {
                *failure
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(reason);
            }
            let message = failure
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
                .unwrap_or_else(|| "The language server stopped".to_owned());
            // Every waiter learns at once rather than each timing out alone.
            let mut waiting = pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for (_, sender) in waiting.drain() {
                let _ = sender.send(Err(message.clone()));
            }
        };
        loop {
            let message = match read_message(&mut reader) {
                Ok(Some(body)) => body,
                Ok(None) => {
                    stop(Some(format!(
                        "The language server '{}' exited",
                        config.name
                    )));
                    return;
                }
                Err(error) => {
                    stop(Some(error));
                    return;
                }
            };
            let Ok(value) = serde_json::from_slice::<Value>(&message) else {
                // A body that is not JSON is a protocol violation, not a
                // recoverable hiccup: the stream offset is no longer trusted.
                stop(Some(format!(
                    "The language server '{}' sent a message that is not valid JSON",
                    config.name
                )));
                return;
            };
            let Some(object) = value.as_object() else {
                continue;
            };
            let id = object.get("id");
            let method = object.get("method").and_then(Value::as_str);
            match (id, method) {
                // A response to something this client asked.
                (Some(id), None) => {
                    let Some(id) = id.as_i64() else { continue };
                    let sender = pending
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .get(&id)
                        .cloned();
                    let Some(sender) = sender else { continue };
                    let payload = if let Some(error) = object.get("error") {
                        let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
                        let text = error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown error");
                        if code == CONTENT_MODIFIED {
                            Err(format!("{CONTENT_MODIFIED_MARKER}{text}"))
                        } else {
                            Err(text.to_owned())
                        }
                    } else {
                        Ok(object.get("result").cloned().unwrap_or(Value::Null))
                    };
                    let _ = sender.send(payload);
                }
                // A request from the server. Answering is not optional: a
                // server left waiting on `client/registerCapability` never
                // finishes starting.
                (Some(id), Some(method)) => {
                    let Some(connection) = connection.upgrade() else {
                        return;
                    };
                    let reply = answer_server_request(method, object.get("params"), &config, &root.path);
                    let payload = match reply {
                        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                        Err(message) => json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": { "code": -32601, "message": message },
                        }),
                    };
                    let _ = connection.send(&payload);
                }
                // A notification.
                (None, Some("textDocument/publishDiagnostics")) => {
                    if !config.diagnostics {
                        continue;
                    }
                    let Some(params) = object.get("params") else {
                        continue;
                    };
                    let Some(uri) = params.get("uri").and_then(Value::as_str) else {
                        continue;
                    };
                    let diagnostics = params
                        .get("diagnostics")
                        .and_then(Value::as_array)
                        .map(|items| items.iter().map(parse_diagnostic).collect::<Vec<_>>())
                        .unwrap_or_default();
                    // The registry that owns this server, not a process-wide
                    // one: a weak handle so a reader thread can never keep a
                    // dead registry (or a test's) alive.
                    if let Some(registry) = sink.upgrade() {
                        registry
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .diagnostics
                            .publish(&root, uri.to_owned(), diagnostics);
                    }                }
                // Everything else a server volunteers — log messages, progress
                // — is noise this client has no surface for.
                (None, Some(_)) | (None, None) => {}
            }
        }
    });
}

fn parse_diagnostic(value: &Value) -> Diagnostic {
    let start = value.get("range").and_then(|range| range.get("start"));
    Diagnostic {
        severity: severity_name(value.get("severity")),
        line: start
            .and_then(|start| start.get("line"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        character: start
            .and_then(|start| start.get("character"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        message: value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        code: match value.get("code") {
            Some(Value::String(code)) => code.clone(),
            Some(Value::Number(code)) => code.to_string(),
            _ => String::new(),
        },
        source: value
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    }
}

/// Answers the server-to-client requests a real server sends.
///
/// The source keeps this set small by advertising `dynamicRegistration: false`
/// everywhere, but `client/registerCapability` and the progress-token requests
/// arrive anyway; a `-32601` to those is what makes `rust-analyzer` hang.
fn answer_server_request(
    method: &str,
    params: Option<&Value>,
    config: &LspServerConfig,
    root: &Path,
) -> Result<Value, String> {
    match method {
        "workspace/configuration" => {
            let items = params
                .and_then(|params| params.get("items"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            Ok(Value::Array(
                items
                    .iter()
                    .map(|item| {
                        let section = item.get("section").and_then(Value::as_str);
                        settings_section(config.settings.as_ref(), section)
                    })
                    .collect(),
            ))
        }
        "workspace/workspaceFolders" => Ok(json!([{
            "uri": path_to_uri(root),
            "name": root
                .file_name()
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_default(),
        }])),
        // Accepted and ignored: this client has no dynamic capability table,
        // and a server that registers one only needs to be told it landed.
        "client/registerCapability" | "client/unregisterCapability" => Ok(Value::Null),
        // Progress reporting has no surface here, but refusing the token makes
        // some servers treat startup as failed.
        "window/workDoneProgress/create" => Ok(Value::Null),
        // This client never applies server-authored edits: the model edits
        // through the `write` and `edit` tools, which are approved.
        "workspace/applyEdit" => Ok(json!({ "applied": false })),
        "window/showMessageRequest" => Ok(Value::Null),
        other => Err(format!("Mework's LSP client does not implement {other}")),
    }
}

/// Reads one dotted section out of a server's `settings` object, as the source
/// does for `workspace/configuration`.
fn settings_section(settings: Option<&Value>, section: Option<&str>) -> Value {
    let Some(settings) = settings else {
        return Value::Null;
    };
    let Some(section) = section.filter(|value| !value.is_empty()) else {
        return settings.clone();
    };
    let mut cursor = settings;
    for part in section.split('.') {
        match cursor.get(part) {
            Some(next) => cursor = next,
            None => return Value::Null,
        }
    }
    cursor.clone()
}

/// Asks a server to shut down, then makes sure it and its children are gone.
///
/// The graceful half is bounded by the entry's `shutdownTimeout`; the kill is
/// not optional, because a language server that ignores `exit` still holds the
/// inherited stdout handle and keeps a reader thread alive forever.
fn stop_connection(connection: &Connection) {
    if connection.is_alive() {
        let _ = connection.request("shutdown", Value::Null, connection.shutdown_timeout);
        let _ = connection.notify("exit", Value::Null);
    }
    connection.alive.store(false, Ordering::SeqCst);
    let deadline = std::time::Instant::now() + STOP_GRACE;
    let mut child = connection
        .child
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    loop {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let job = connection
        .job
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    kill_shell_child(&mut child, &job);
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn a_framed_message_round_trips_through_the_reader() {
        let body = br#"{"jsonrpc":"2.0","id":1,"result":null}"#;
        let mut framed = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        framed.extend_from_slice(body);
        let mut reader = BufReader::new(Cursor::new(framed));
        let read = read_message(&mut reader)
            .expect("the frame parses")
            .expect("there is a message");
        assert_eq!(read, body);
        assert!(
            read_message(&mut reader).expect("a clean end is not an error").is_none(),
            "end of stream is None, not an error"
        );
    }

    #[test]
    fn headers_are_case_insensitive_and_extra_headers_are_ignored() {
        let body = b"{}";
        let mut framed =
            b"content-type: application/vscode-jsonrpc; charset=utf-8\r\ncontent-length: 2\r\n\r\n"
                .to_vec();
        framed.extend_from_slice(body);
        let mut reader = BufReader::new(Cursor::new(framed));
        assert_eq!(
            read_message(&mut reader).expect("parses").expect("present"),
            body
        );
    }

    #[test]
    fn a_header_block_without_a_content_length_is_a_protocol_violation() {
        let framed = b"Content-Type: text/plain\r\n\r\n".to_vec();
        let mut reader = BufReader::new(Cursor::new(framed));
        let error = read_message(&mut reader).expect_err("a frame with no length cannot be read");
        assert!(error.contains("Content-Length"), "{error}");
    }

    #[test]
    fn a_server_logging_to_stdout_is_reported_rather_than_resynchronized() {
        let framed = b"info: starting up\r\n\r\n".to_vec();
        let mut reader = BufReader::new(Cursor::new(framed));
        let error = read_message(&mut reader).expect_err("plain text is not a header block");
        assert!(error.contains("desynchronized"), "{error}");
    }

    #[test]
    fn two_messages_in_one_buffer_are_read_in_order() {
        let mut framed = Vec::new();
        for body in [br#"{"id":1}"#.as_slice(), br#"{"id":2}"#.as_slice()] {
            framed.extend_from_slice(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes());
            framed.extend_from_slice(body);
        }
        let mut reader = BufReader::new(Cursor::new(framed));
        assert_eq!(
            read_message(&mut reader).unwrap().unwrap(),
            br#"{"id":1}"#.to_vec()
        );
        assert_eq!(
            read_message(&mut reader).unwrap().unwrap(),
            br#"{"id":2}"#.to_vec()
        );
        assert!(read_message(&mut reader).unwrap().is_none());
    }

    #[test]
    fn a_windows_path_uri_round_trips() {
        let uri = path_to_uri(Path::new("C:\\Users\\me\\src\\main.rs"));
        assert_eq!(uri, "file:///C:/Users/me/src/main.rs");
        assert_eq!(uri_to_path(&uri), "C:/Users/me/src/main.rs");
    }

    /// The path guard hands navigation `fs::canonicalize`'s answer, and on
    /// Windows that is always the extended-length spelling. Encoding it
    /// literally produced `file:////?/C:/…`, which `rust-analyzer` rejects with
    /// `-32603 url is not a file` — every `lsp` call on Windows, not an edge
    /// case.
    #[test]
    fn a_verbatim_windows_path_loses_its_extended_length_prefix() {
        let uri = path_to_uri(Path::new("\\\\?\\C:\\Users\\me\\src\\main.rs"));
        assert_eq!(uri, "file:///C:/Users/me/src/main.rs");
        assert_eq!(uri_to_path(&uri), "C:/Users/me/src/main.rs");
        assert!(!uri.starts_with("file:////"), "{uri}");
    }

    /// The same file reached through the guard and through the edit hook must
    /// produce one URI, because the document map is keyed by it.
    #[test]
    fn the_verbatim_and_plain_spellings_of_one_file_agree() {
        assert_eq!(
            path_to_uri(Path::new("\\\\?\\C:\\work\\a.rs")),
            path_to_uri(Path::new("C:\\work\\a.rs"))
        );
    }

    #[test]
    fn a_network_path_keeps_its_host_in_the_authority() {
        let uri = path_to_uri(Path::new("\\\\build01\\share\\src\\main.rs"));
        assert_eq!(uri, "file://build01/share/src/main.rs");
        assert_eq!(uri_to_path(&uri), "//build01/share/src/main.rs");
        // Canonicalizing a network path yields the verbatim UNC spelling, which
        // has to land on the same URI as the plain one.
        assert_eq!(
            path_to_uri(Path::new("\\\\?\\UNC\\build01\\share\\src\\main.rs")),
            uri
        );
    }

    #[test]
    fn a_posix_path_uri_round_trips_and_spaces_are_encoded() {
        let uri = path_to_uri(Path::new("/home/me/my project/main.rs"));
        assert_eq!(uri, "file:///home/me/my%20project/main.rs");
        assert_eq!(uri_to_path(&uri), "/home/me/my project/main.rs");
    }

    #[test]
    fn a_configuration_request_is_answered_section_by_section() {
        let settings = json!({ "rust-analyzer": { "cargo": { "allFeatures": true } } });
        assert_eq!(
            settings_section(Some(&settings), Some("rust-analyzer.cargo")),
            json!({ "allFeatures": true })
        );
        assert_eq!(
            settings_section(Some(&settings), Some("missing.section")),
            Value::Null
        );
        assert_eq!(settings_section(Some(&settings), None), settings);
        assert_eq!(settings_section(None, Some("anything")), Value::Null);
    }

    fn test_config() -> LspServerConfig {
        LspServerConfig {
            id: "id".into(),
            name: "test".into(),
            description: String::new(),
            command: "server".into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            extension_to_language: [(".rs".to_owned(), "rust".to_owned())]
                .into_iter()
                .collect(),
            initialization_options: None,
            settings: None,
            workspace_folder: String::new(),
            startup_timeout_millis: 1_000,
            shutdown_timeout_millis: 1_000,
            restart_on_crash: true,
            max_restarts: 3,
            diagnostics: true,
        }
    }

    #[test]
    fn capability_registration_is_acknowledged_rather_than_refused() {
        let config = test_config();
        let root = Path::new("/tmp");
        for method in [
            "client/registerCapability",
            "client/unregisterCapability",
            "window/workDoneProgress/create",
            "window/showMessageRequest",
        ] {
            assert!(
                answer_server_request(method, None, &config, root).is_ok(),
                "{method} must be answered, not refused: a server waiting on it never finishes starting"
            );
        }
        assert_eq!(
            answer_server_request("workspace/applyEdit", None, &config, root).unwrap(),
            json!({ "applied": false })
        );
        assert!(
            answer_server_request("workspace/somethingElse", None, &config, root).is_err(),
            "an unknown request still gets a method-not-found"
        );
    }

    #[test]
    fn the_extension_table_picks_the_first_configuration_that_claims_a_file() {
        let mut first = test_config();
        first.name = "first".into();
        let mut second = test_config();
        second.name = "second".into();
        let configs = vec![first, second];
        let (chosen, language) =
            LspRegistry::config_for_path(&configs, Path::new("/src/main.rs")).expect("claimed");
        assert_eq!(chosen.name, "first");
        assert_eq!(language, "rust");
        assert!(
            LspRegistry::config_for_path(&configs, Path::new("/src/main.py")).is_none(),
            "an unclaimed extension resolves to nothing"
        );
        assert!(
            LspRegistry::config_for_path(&configs, Path::new("/src/MAIN.RS")).is_some(),
            "extension matching is case-insensitive"
        );
    }

    fn project_root() -> PathBuf {
        PathBuf::from("/work/project")
    }

    fn other_root() -> PathBuf {
        PathBuf::from("/work/other")
    }

    fn local(path: PathBuf) -> ServerRoot {
        ServerRoot::local(path)
    }

    fn diagnostic(message: &str, line: u64) -> Diagnostic {
        Diagnostic {
            severity: "Error".into(),
            line,
            character: 0,
            message: message.into(),
            code: String::new(),
            source: "test".into(),
        }
    }

    #[test]
    fn a_conversation_is_owed_nothing_until_it_registers() {
        let mut ledger = DiagnosticsLedger::default();
        ledger.publish(&local(project_root()), "file:///a.rs".into(), vec![diagnostic("before", 1)]);
        ledger.register("c1", &local(project_root()));
        assert!(
            ledger.take_new("c1").is_empty(),
            "a conversation must not open with somebody else's backlog"
        );
        ledger.publish(&local(project_root()), "file:///a.rs".into(), vec![diagnostic("after", 2)]);
        assert_eq!(ledger.take_new("c1").len(), 1);
    }

    #[test]
    fn the_same_problem_is_reported_once() {
        let mut ledger = DiagnosticsLedger::default();
        ledger.register("c1", &local(project_root()));
        ledger.publish(&local(project_root()), "file:///a.rs".into(), vec![diagnostic("same", 1)]);
        assert_eq!(ledger.take_new("c1").len(), 1);
        ledger.publish(&local(project_root()), "file:///a.rs".into(), vec![diagnostic("same", 1)]);
        assert!(
            ledger.take_new("c1").is_empty(),
            "an unchanged republish is not news"
        );
    }

    #[test]
    fn an_empty_publish_clears_the_file_without_becoming_news() {
        let mut ledger = DiagnosticsLedger::default();
        ledger.register("c1", &local(project_root()));
        ledger.publish(&local(project_root()), "file:///a.rs".into(), vec![diagnostic("broken", 1)]);
        ledger.publish(&local(project_root()), "file:///a.rs".into(), Vec::new());
        assert!(
            ledger.take_new("c1").is_empty(),
            "a file that became clean has nothing to report"
        );
    }

    #[test]
    fn volume_limits_cap_one_injection() {
        let mut ledger = DiagnosticsLedger::default();
        ledger.register("c1", &local(project_root()));
        for file in 0..5 {
            let diagnostics = (0..20)
                .map(|index| diagnostic(&format!("problem {file}-{index}"), index))
                .collect();
            ledger.publish(&local(project_root()), format!("file:///f{file}.rs"), diagnostics);
        }
        let taken = ledger.take_new("c1");
        let total: usize = taken.iter().map(|(_, items)| items.len()).sum();
        assert!(total <= MAX_DIAGNOSTICS_TOTAL, "{total} exceeded the total cap");
        for (uri, items) in &taken {
            assert!(
                items.len() <= MAX_DIAGNOSTICS_PER_FILE,
                "{uri} exceeded the per-file cap"
            );
        }
    }

    #[test]
    fn two_conversations_are_owed_independently() {
        let mut ledger = DiagnosticsLedger::default();
        ledger.register("c1", &local(project_root()));
        ledger.register("c2", &local(project_root()));
        ledger.publish(&local(project_root()), "file:///a.rs".into(), vec![diagnostic("shared", 1)]);
        assert_eq!(ledger.take_new("c1").len(), 1);
        assert_eq!(
            ledger.take_new("c2").len(),
            1,
            "draining one conversation must not consume another's"
        );
    }

    #[test]
    fn a_conversation_is_not_told_about_another_projects_problems() {
        let mut ledger = DiagnosticsLedger::default();
        ledger.register("here", &local(project_root()));
        ledger.register("there", &local(other_root()));
        ledger.publish(
            &local(other_root()),
            "file:///work/other/src/main.rs".into(),
            vec![diagnostic("broken over there", 1)],
        );
        assert!(
            ledger.take_new("here").is_empty(),
            "a conversation in one project must never be handed another project's compile errors"
        );
        assert_eq!(ledger.take_new("there").len(), 1);
    }

    /// A server entry that names its own `workspaceFolder` publishes under that
    /// folder, not under the conversation's workspace. Registering the
    /// conversation under the workspace left it subscribed to a root nothing
    /// ever published to, so that server's problems were dropped in silence.
    #[test]
    fn a_server_with_its_own_workspace_folder_is_registered_under_the_root_it_publishes_to() {
        let mut config = test_config();
        config.workspace_folder = other_root().to_string_lossy().into_owned();
        let workspace = project_root();
        let root = server_root(&config, &workspace);
        assert_eq!(root, other_root(), "the entry's own folder wins");

        let mut ledger = DiagnosticsLedger::default();
        ledger.register("c1", &local(root.clone()));
        ledger.publish(&local(root.clone()), "file:///work/other/src/main.rs".into(), vec![diagnostic("broken", 1)]);
        assert_eq!(
            ledger.take_new("c1").len(),
            1,
            "a configured workspaceFolder must not swallow the server's diagnostics"
        );
    }

    #[test]
    fn a_server_without_a_workspace_folder_is_rooted_at_the_conversations_workspace() {
        let config = test_config();
        assert_eq!(server_root(&config, &project_root()), project_root());
    }

    #[test]
    fn a_conversation_that_works_in_two_projects_is_owed_both() {
        let mut ledger = DiagnosticsLedger::default();
        ledger.register("both", &local(project_root()));
        ledger.register("both", &local(other_root()));
        ledger.publish(&local(project_root()), "file:///a.rs".into(), vec![diagnostic("a", 1)]);
        ledger.publish(&local(other_root()), "file:///b.rs".into(), vec![diagnostic("b", 1)]);
        assert_eq!(ledger.take_new("both").len(), 2);
    }

    #[test]
    fn tracked_files_and_conversations_are_evicted_rather_than_growing_forever() {
        let mut ledger = DiagnosticsLedger::default();
        ledger.register("c1", &local(project_root()));
        for index in 0..(MAX_TRACKED_FILES + 50) {
            ledger.publish(
                &local(project_root()),
                format!("file:///f{index}.rs"),
                vec![diagnostic("problem", 1)],
            );
        }
        assert!(
            ledger.current.len() <= MAX_TRACKED_FILES,
            "a long session must not accumulate every file it ever saw: {}",
            ledger.current.len()
        );

        for index in 0..(MAX_TRACKED_CONVERSATIONS + 10) {
            ledger.register(&format!("conversation-{index}"), &local(project_root()));
        }
        assert!(
            ledger.conversations.len() <= MAX_TRACKED_CONVERSATIONS,
            "conversation ledgers are evicted oldest-first: {}",
            ledger.conversations.len()
        );
    }

    #[test]
    fn the_rendered_block_matches_the_sources_shape() {
        let rendered = render_diagnostics(&[(
            "file:///c/src/main.rs".into(),
            vec![Diagnostic {
                severity: "Error".into(),
                line: 9,
                character: 4,
                message: "cannot find value `x`".into(),
                code: "E0425".into(),
                source: "rustc".into(),
            }],
        )]);
        assert_eq!(
            rendered,
            "<new-diagnostics>The following new diagnostic issues were detected:\n\nmain.rs:\n  Error [Line 10:5] cannot find value `x` [E0425] (rustc)</new-diagnostics>"
        );
    }

    #[test]
    fn a_long_block_is_cut_with_the_sources_marker() {
        let diagnostics = (0..2_000)
            .map(|index| diagnostic(&format!("a fairly long diagnostic message {index}"), index))
            .collect::<Vec<_>>();
        let rendered = render_diagnostics(&[("file:///a.rs".into(), diagnostics)]);
        assert!(rendered.contains("…[truncated]"), "long bodies are cut");
    }

    #[test]
    fn severity_integers_map_to_the_sources_names() {
        assert_eq!(severity_name(Some(&json!(1))), "Error");
        assert_eq!(severity_name(Some(&json!(2))), "Warning");
        assert_eq!(severity_name(Some(&json!(3))), "Info");
        assert_eq!(severity_name(Some(&json!(4))), "Hint");
        assert_eq!(
            severity_name(None),
            "Error",
            "an absent severity defaults to Error, as in the source"
        );
    }

    /// `/srv/app` on a WSL distribution and `/srv/app` on an SSH machine are
    /// two projects. A ledger keyed by path alone would hand the conversation
    /// working on one the other's compile errors.
    #[test]
    fn the_same_path_on_two_machines_is_two_roots() {
        let wsl = ServerRoot {
            machine: "wsl:Ubuntu".into(),
            path: PathBuf::from("/srv/app"),
        };
        let ssh = ServerRoot {
            machine: "ssh:m1".into(),
            path: PathBuf::from("/srv/app"),
        };
        let mut ledger = DiagnosticsLedger::default();
        ledger.register("on-wsl", &wsl);
        ledger.register("on-ssh", &ssh);
        ledger.publish(&ssh, "file:///srv/app/main.rs".into(), vec![diagnostic("there", 1)]);
        assert!(
            ledger.take_new("on-wsl").is_empty(),
            "a publish from one machine is not news on another"
        );
        assert_eq!(ledger.take_new("on-ssh").len(), 1);
    }

    /// The remote launch script is the one place a repository-shipped
    /// `lsp.json` reaches a shell: every fragment of it is single-quoted, the
    /// server is `exec`ed so the wrapper's pipes are its pipes, and a command
    /// that is not there is named before anything runs.
    #[test]
    fn the_remote_launch_script_quotes_everything_and_execs_the_server() {
        let mut config = test_config();
        config.command = "rust-analyzer".into();
        config.args = vec!["--log-file".into(), "/tmp/it's here.log".into()];
        config.env = [("RA_LOG".to_owned(), "error".to_owned())].into_iter().collect();
        let script = remote_launch_script(&config, Path::new("/home/dev/app")).unwrap();
        assert_eq!(
            script,
            "cd -- '/home/dev/app' || exit 64\n\
             command -v 'rust-analyzer' >/dev/null 2>&1 || { printf '%s\\n' 'rust-analyzer: command not found on the remote machine'\\''s PATH' >&2; exit 127; }\n\
             exec env 'RA_LOG=error' 'rust-analyzer' '--log-file' '/tmp/it'\\''s here.log'\n"
        );

        // A `~`-spelled root stays expandable, as the file tools spell it.
        let script = remote_launch_script(&config, Path::new("~/app")).unwrap();
        assert!(script.starts_with("cd -- ~/'app' || exit 64\n"), "{script}");

        // Control characters cannot be quoted into safety and are refused.
        config.args = vec!["--x\n; rm -rf /".into()];
        assert!(remote_launch_script(&config, Path::new("/home/dev/app")).is_err());
        config.args.clear();
        config.env = [("BAD NAME".to_owned(), "x".to_owned())].into_iter().collect();
        assert!(remote_launch_script(&config, Path::new("/home/dev/app")).is_err());
    }

    #[test]
    fn the_stderr_tail_is_the_last_non_empty_line_cut_short() {
        assert_eq!(last_stderr_line("a\nb\n\n  \n"), "b");
        assert_eq!(last_stderr_line(""), "");
        let long = "x".repeat(300);
        let tail = last_stderr_line(&long);
        assert_eq!(tail.chars().count(), 241);
        assert!(tail.ends_with('…'));
    }

    /// A language server that speaks the base protocol over stdio, written in
    /// Node so the test needs nothing this repository does not already need.
    /// It answers `initialize`, `hover` (with its working directory and one
    /// environment variable, so the launch script's `cd` and `env` are
    /// observable), `shutdown` and `exit`, and writes one line to stderr.
    const FAKE_SERVER: &str = r#"
let buffer = Buffer.alloc(0);
function send(payload) {
  const body = Buffer.from(JSON.stringify(payload), "utf8");
  process.stdout.write(`Content-Length: ${body.length}\r\n\r\n`);
  process.stdout.write(body);
}
function handle(message) {
  if (message.method === "initialize") {
    send({ jsonrpc: "2.0", id: message.id, result: { capabilities: {}, serverInfo: { name: "fake" } } });
  } else if (message.method === "textDocument/hover") {
    const value = `cwd=${process.cwd()} env=${process.env.FAKE_LSP || ""}`;
    send({ jsonrpc: "2.0", id: message.id, result: { contents: { kind: "plaintext", value } } });
  } else if (message.method === "shutdown") {
    send({ jsonrpc: "2.0", id: message.id, result: null });
  } else if (message.method === "exit") {
    process.exit(0);
  }
}
process.stdin.on("data", (chunk) => {
  buffer = Buffer.concat([buffer, chunk]);
  for (;;) {
    const headerEnd = buffer.indexOf("\r\n\r\n");
    if (headerEnd < 0) return;
    const match = /Content-Length:\s*(\d+)/i.exec(buffer.slice(0, headerEnd).toString("utf8"));
    if (!match) process.exit(3);
    const start = headerEnd + 4;
    const length = Number(match[1]);
    if (buffer.length < start + length) return;
    const body = JSON.parse(buffer.slice(start, start + length).toString("utf8"));
    buffer = buffer.slice(start + length);
    handle(body);
  }
});
process.stdin.on("end", () => process.exit(0));
process.stderr.write("fake server up\n");
"#;

    /// A local Bash running the remote launch script, standing in for the
    /// `wsl.exe`/`ssh` wrapper exactly as the remote file tools' tests do.
    fn spawn_through_local_bash(script: &str) -> Option<Child> {
        let bash = crate::run_environment::local_bash_candidates().into_iter().next()?;
        Command::new(bash)
            .args(["--noprofile", "--norc", "-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .ok()
    }

    fn fake_host() -> ServerHost {
        ServerHost::Remote {
            runner: ShellRunner::Ssh {
                agent_shell: Default::default(),
                host: "fake".into(),
                port: 0,
                identity_file: String::new(),
                env: BTreeMap::new(),
            },
            machine_key: "ssh:fake".into(),
        }
    }

    /// The whole remote leg short of the machine: the launch script starts the
    /// server through a shell, the wrapper's pipes carry the protocol, the
    /// entry's `env` and the root's `cd` reach the server, the handshake
    /// completes with a `null` `processId`, documents sync, requests are
    /// answered, stderr is kept, and `shutdown`/`exit` end the process.
    #[test]
    fn a_remote_launch_script_drives_a_server_through_a_shell() {
        let Some(node) = crate::environment_tools::resolve_on_path("node") else {
            eprintln!("skipped: node is not on PATH");
            return;
        };
        let temp = tempfile::tempdir().expect("temp dir");
        let server = temp.path().join("fake-lsp.js");
        std::fs::write(&server, FAKE_SERVER).expect("fake server written");
        let root = temp.path().to_string_lossy().replace('\\', "/");

        let mut config = test_config();
        config.command = node.to_string_lossy().replace('\\', "/");
        config.args = vec![server.to_string_lossy().replace('\\', "/")];
        config.env = [("FAKE_LSP".to_owned(), "yes".to_owned())].into_iter().collect();
        let script = remote_launch_script(&config, Path::new(&root)).expect("launch script");
        let Some(child) = spawn_through_local_bash(&script) else {
            eprintln!("skipped: no local bash");
            return;
        };

        let host = fake_host();
        let server_root = ServerRoot::new(&host, PathBuf::from(&root));
        let connection = attach_server(
            ShellChild::Local(child),
            &host,
            &config,
            &server_root,
            std::sync::Weak::new(),
        )
        .expect("the handshake completes through the shell");
        assert!(connection.is_alive());

        let document = PathBuf::from(format!("{root}/main.rs"));
        connection
            .sync_document(&document, "rust", "fn main() {}\n")
            .expect("didOpen reaches the server");
        assert!(connection.has_document(&document));
        let answer = connection
            .request_with_retry(
                "textDocument/hover",
                json!({
                    "textDocument": { "uri": path_to_uri(&document) },
                    "position": { "line": 0, "character": 3 },
                }),
            )
            .expect("hover is answered");
        let value = answer["contents"]["value"].as_str().unwrap_or_default();
        assert!(value.contains("env=yes"), "the entry's env reached the server: {value}");
        let cwd = value
            .split_once("cwd=")
            .and_then(|(_, rest)| rest.split_once(" env="))
            .map(|(cwd, _)| cwd.replace('\\', "/"))
            .unwrap_or_default();
        let leaf = Path::new(&root)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        assert!(
            cwd.to_ascii_lowercase()
                .ends_with(&leaf.to_ascii_lowercase()),
            "the script's cd put the server in the root: {cwd} vs {root}"
        );

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !connection.diagnostics_log().contains("fake server up") {
            assert!(std::time::Instant::now() < deadline, "stderr is drained");
            thread::sleep(Duration::from_millis(20));
        }

        stop_connection(&connection);
        assert!(!connection.is_alive());
        let mut child = connection.child.lock().unwrap();
        assert!(
            matches!(child.try_wait(), Ok(Some(_))),
            "shutdown/exit ended the process behind the shell"
        );
    }

    /// A command the machine does not have is the most likely remote failure,
    /// and the script names it on stderr before exiting — which is what the
    /// handshake failure hands back, instead of "the language server exited".
    #[test]
    fn a_missing_remote_command_is_named_in_the_failure() {
        let mut config = test_config();
        config.command = "definitely-not-a-language-server-xyz".into();
        config.args.clear();
        config.startup_timeout_millis = 5_000;
        let script = remote_launch_script(&config, Path::new("/")).expect("launch script");
        let Some(child) = spawn_through_local_bash(&script) else {
            eprintln!("skipped: no local bash");
            return;
        };
        let host = fake_host();
        let Err(error) = attach_server(
            ShellChild::Local(child),
            &host,
            &config,
            &ServerRoot::new(&host, PathBuf::from("/")),
            std::sync::Weak::new(),
        ) else {
            panic!("a missing command cannot complete the handshake");
        };
        assert!(
            error.contains("command not found on the remote machine's PATH"),
            "{error}"
        );
    }
}
