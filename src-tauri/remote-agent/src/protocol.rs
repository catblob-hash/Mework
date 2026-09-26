//! The wire protocol between the host and the agent on a remote machine.
//!
//! One byte stream carries everything — requests, replies, heartbeats and the
//! output of every process the agent runs for the host — so a machine costs one
//! SSH connection however much is happening on it. The stream is a sequence of
//! frames:
//!
//! ```text
//! u32 BE header length | u32 BE body length | header (JSON) | body (raw bytes)
//! ```
//!
//! The header is a [`Message`]; the body is whatever bytes the message is about
//! (process output, process input, a script's standard input) and travels
//! without any encoding, so a binary file or a terminal's escape sequences cost
//! exactly their own size.
//!
//! # Surviving a dropped link
//!
//! The link is expected to drop — a laptop sleeps, Wi-Fi changes, a tunnel
//! restarts — and nothing the agent runs may be lost or done twice because of
//! it. Two rules make a reconnect invisible:
//!
//! * **Every request is idempotent under retransmission.** A request the host
//!   sent but got no reply to is sent again, with the same id, on the next
//!   connection. [`Op::Spawn`] is keyed by the session id the host chose, so a
//!   second copy finds the first session instead of starting another process;
//!   signals, resizes and releases do the same thing however many times they
//!   arrive.
//! * **Every byte stream is addressed by offset.** Output frames carry the
//!   offset of their first byte in the session's stream, and the host resumes a
//!   connection by saying which offset it has reached ([`ResumePoint`]). Input
//!   runs the other way on the same rule: [`SessionInfo::input_end`] tells the
//!   host how much input already arrived, and it re-sends only the rest.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

/// Bumped whenever a message changes shape. The two ends refuse each other
/// rather than guess: the host uploads the agent it was built with, so a
/// mismatch only ever means a stale daemon is still answering.
pub const PROTOCOL_VERSION: u32 = 2;

/// `argv[0]` naming the agent's own executable. The host cannot know where the
/// build it uploaded sits on the machine, and its helpers — `net`, above all —
/// are part of that build, so a spawn asks for them by this name and the agent
/// runs itself.
pub const SELF_PROGRAM: &str = "@mework-remote";

/// Largest header either end accepts. Headers are small descriptive JSON; a
/// larger one is a corrupt stream, not a large request.
pub const MAX_HEADER_BYTES: usize = 1 << 20;

/// Largest body in one frame. Output is chunked far below this; the bound is
/// for a request body such as a file written through a script's stdin.
pub const MAX_BODY_BYTES: usize = 32 << 20;

/// How much output one frame carries at most, so one busy session cannot hold
/// the stream while another has a line waiting.
pub const OUTPUT_CHUNK_BYTES: usize = 64 * 1024;

/// One message on the link. The tag is `t`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Message {
    /// First frame from the host on every connection.
    Hello(Hello),
    /// The agent's answer to [`Message::Hello`]: who it is and which of this
    /// host's sessions it still holds.
    Welcome(Welcome),
    /// The agent will not serve this connection, and says why.
    Refused { reason: String },
    /// Heartbeat from the host. Answered at once, whatever else is queued.
    Ping { seq: u64 },
    Pong { seq: u64 },
    Request { id: u64, op: Op },
    Response { id: u64, outcome: Outcome },
    /// Output of a session; the body is the bytes, starting at `offset` in
    /// that stream.
    Output {
        sid: String,
        stream: Stream,
        offset: u64,
    },
    /// Output the agent no longer holds: `from..to` of the stream was dropped
    /// from its buffer before the host could receive it.
    Gap {
        sid: String,
        stream: Stream,
        from: u64,
        to: u64,
    },
    /// A session's process is gone. Sent after every byte of its output, and
    /// again on every resumed connection until the host releases the session.
    Exit { sid: String, exit: ExitInfo },
    /// Input for a session; the body is the bytes, starting at `offset` in the
    /// session's input stream.
    Input { sid: String, offset: u64 },
    /// The host is leaving on purpose. With `release`, every session it owns
    /// is ended now instead of waiting out its orphan time.
    Bye { release: bool },
    /// From an agent that has no network of its own — a sandboxed cell (see
    /// [`SandboxSpec`]) — to the side that started it: open a connection to
    /// `host:port` for one of its processes. That side decides by the cell's
    /// [`NetworkPolicy`] and answers with [`Message::Dialed`]. `conn` is the
    /// asker's own number for the connection.
    Dial { conn: u64, host: String, port: u16 },
    /// The answer to [`Message::Dial`]: connected when `error` is `None`.
    /// `refused` says the policy refused the destination, rather than the
    /// network failing to reach it.
    Dialed {
        conn: u64,
        #[serde(default)]
        error: Option<String>,
        #[serde(default)]
        refused: bool,
    },
    /// Bytes of a dialed connection, in either direction; the body is the
    /// bytes. A side never has more than [`TUNNEL_WINDOW`] bytes out that the
    /// other has not acknowledged.
    Tunnel { conn: u64 },
    /// The receiver delivered `bytes` more of a connection's bytes where they
    /// were going, and has room for as many again.
    TunnelAck { conn: u64, bytes: u64 },
    /// The sender has nothing more to send on this connection; what it
    /// receives still arrives.
    TunnelEnd { conn: u64 },
    /// The connection is over in both directions.
    TunnelClose { conn: u64 },
}

/// Bytes of one tunnelled connection that may be in flight, unacknowledged, in
/// each direction. Large enough that a download is not held up by the round
/// trip of a local pipe, small enough that a hundred stalled connections are
/// not a memory problem.
pub const TUNNEL_WINDOW: u64 = 512 * 1024;

/// How the host introduces itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u32,
    /// Stable for a host installation. Sessions belong to a client, so two
    /// computers using one machine never see each other's shells.
    pub client: String,
    /// Changes every time the host process starts. A new epoch means the
    /// previous host process is gone and nothing can reach its sessions any
    /// more, so the agent ends them rather than leaving them to their orphan
    /// time.
    pub epoch: String,
    pub policy: Policy,
    /// Where the host's copy of each live session's output has reached.
    #[serde(default)]
    pub resume: Vec<ResumePoint>,
}

/// The lifetimes the host asks the agent to enforce for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    /// How long a session outlives its host's connection before it is
    /// reclaimed, unless the session asked for its own time.
    pub orphan_ttl_secs: u64,
    /// How long a connection may stay silent before the agent treats it as dead.
    /// The host pings well inside this.
    pub silence_timeout_secs: u64,
    /// How long a finished session's output is kept for a host that has not
    /// released it.
    pub finished_ttl_secs: u64,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            orphan_ttl_secs: 30 * 60,
            silence_timeout_secs: 30,
            finished_ttl_secs: 10 * 60,
        }
    }
}

/// Where the host's copy of one session's output stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumePoint {
    pub sid: String,
    pub stdout: u64,
    pub stderr: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Welcome {
    pub protocol: u32,
    pub agent: AgentInfo,
    /// Every live or unreleased session this client owns.
    pub sessions: Vec<SessionInfo>,
}

/// What the agent reports about itself and the machine it runs on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub version: String,
    /// Digest of the agent's own executable, so the host can tell a daemon
    /// started from an older upload apart from the one it just uploaded.
    pub build: String,
    /// [`crate::SOURCE_ID`] of the agent's build. `None` from an agent older
    /// than source identities.
    #[serde(default)]
    pub source: Option<String>,
    pub pid: u32,
    /// `std::env::consts::OS` and `ARCH` of the machine.
    pub os: String,
    pub arch: String,
    pub home: String,
    /// The account's login shell, as its environment names it.
    pub shell: Option<String>,
    pub started_unix_ms: u64,
    /// Whether the agent can run sandboxed sessions here.
    #[serde(default)]
    pub sandbox: SandboxSupport,
}

/// An operation the host asks for. The tag is `op`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Starts a process. The request body is its standard input when
    /// `stdin` is [`StdinMode::Body`].
    Spawn(SpawnSpec),
    /// Signals the session's whole process group.
    Signal { sid: String, signal: SignalKind },
    /// Closes the session's standard input once everything sent before this
    /// has been written.
    CloseStdin { sid: String },
    Resize {
        sid: String,
        size: TerminalSize,
    },
    /// Forgets a session. A process still running is killed first.
    Release { sid: String },
    /// Every session this client owns.
    Sessions,
    /// Resolves program names on the machine's `PATH`.
    Which { names: Vec<String> },
}

/// How a process is started.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnSpec {
    /// Chosen by the host and unique within it; retransmitting the same spawn
    /// finds this session instead of starting a second process.
    pub sid: String,
    /// `argv[0]` is the program, resolved on the machine's `PATH` when it has
    /// no directory in it. Nothing here is read by a shell unless the program
    /// is one.
    pub argv: Vec<String>,
    /// Working directory, with a leading `~` expanded on the machine. `None`
    /// starts in the home directory.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Set on top of the account's login environment.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Removed from the login environment before `env` is applied.
    #[serde(default)]
    pub env_remove: Vec<String>,
    /// `Some` runs the process on a pseudo terminal of this size; its output
    /// then arrives on [`Stream::Stdout`] alone.
    #[serde(default)]
    pub terminal: Option<TerminalSize>,
    #[serde(default)]
    pub stdin: StdinMode,
    /// Bytes of each output stream the agent keeps for a host that has not
    /// received them yet. Past it the oldest are dropped and reported as a
    /// [`Message::Gap`].
    #[serde(default)]
    pub output_limit: Option<u64>,
    /// Overrides the client's [`Policy::orphan_ttl_secs`] for this session.
    #[serde(default)]
    pub orphan_ttl_secs: Option<u64>,
    /// Reported back in [`SessionInfo::label`].
    #[serde(default)]
    pub label: Option<String>,
    /// Runs the process in a sandbox rather than as the account itself.
    #[serde(default)]
    pub sandbox: Option<SandboxSpec>,
}

/// Where a sandboxed process runs: in a *cell*, one sandboxed agent process
/// that starts the sessions given to it. Every session of one conversation
/// names the same cell, so what the conversation starts can see and stop what
/// it started before, and nothing else — not the agent, not other
/// conversations' processes, not the account's own.
///
/// The agent starts a cell the first time a spawn names it and keeps it while
/// it has sessions. A cell is its name *and* its policy: a spawn that names a
/// running cell with a different policy starts a new cell, and the old one
/// leaves once its sessions have ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxSpec {
    pub cell: String,
    pub policy: SandboxPolicy,
}

/// What a cell may touch. Paths are the machine's own, absolute, with a
/// leading `~` expanded there. The agent adds its own protections on top — the
/// account's credential stores are never readable, and files that something
/// outside the sandbox would later execute are never writable (see
/// `agent::sandbox`) — so a policy can only open less than it says, not more.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxPolicy {
    /// Directories the cell may change: the conversation's workspaces on this
    /// machine. Everything else is read-only, apart from the cell's own
    /// temporary and cache directories.
    pub writable: Vec<String>,
    /// Paths the cell may not read, beyond the agent's own list.
    #[serde(default)]
    pub deny_read: Vec<String>,
    /// Paths inside unreadable ones that are readable after all.
    #[serde(default)]
    pub readable: Vec<String>,
    /// Paths inside writable ones that stay read-only, beyond the agent's own
    /// list.
    #[serde(default)]
    pub deny_write: Vec<String>,
    #[serde(default)]
    pub network: NetworkPolicy,
}

/// Which hosts a cell's processes may connect to. A cell has no network of its
/// own: its processes reach the network through a proxy the agent runs
/// outside the sandbox (`HTTP_PROXY`, `HTTPS_PROXY` and `ALL_PROXY` point at
/// it), which applies this policy to every connection. Whatever the policy, the
/// proxy never connects to a loopback, private, link-local or cloud metadata
/// address unless an allowed entry names that address itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkPolicy {
    pub mode: NetworkMode,
    /// Host patterns: `example.com`, `*.example.com` (its subdomains, not
    /// itself), or `*`; each optionally with `:port`. Only read in
    /// [`NetworkMode::Allowlist`].
    #[serde(default)]
    pub allow: Vec<String>,
    /// Host patterns refused in every mode, checked first.
    #[serde(default)]
    pub deny: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    /// No connection leaves the cell.
    #[default]
    Off,
    /// Connections to the hosts [`NetworkPolicy::allow`] names.
    Allowlist,
    /// Connections to any public host.
    Open,
}

/// Whether the agent can run cells on its machine, and with what.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxSupport {
    /// The mechanism: `seatbelt`, `bubblewrap`, `srt-win`; empty when none.
    pub backend: String,
    pub available: bool,
    /// Why it is not available, or a note about it.
    #[serde(default)]
    pub detail: String,
    /// Not available until the machine is set up for it, once, with
    /// administrator rights (Windows: `mework-remote sandbox-setup`).
    #[serde(default)]
    pub setup: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StdinMode {
    /// `/dev/null`.
    #[default]
    Null,
    /// Open for [`Message::Input`] until [`Op::CloseStdin`].
    Pipe,
    /// The spawn request's body, then end of file.
    Body,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalKind {
    Terminate,
    Hangup,
    Kill,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stream {
    /// A process's standard output, or everything a terminal session prints.
    Stdout,
    Stderr,
}

/// A reply, or why there is none.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    Ok { reply: Reply },
    Err { failure: Failure },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum Reply {
    Spawned { pid: u32 },
    Done,
    Sessions { sessions: Vec<SessionInfo> },
    Which { found: BTreeMap<String, Option<String>> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    pub kind: FailureKind,
    pub message: String,
}

impl Failure {
    pub fn new(kind: FailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// The program, directory or session does not exist.
    NotFound,
    /// The request itself is malformed.
    Invalid,
    /// A limit of the agent was reached.
    Limit,
    /// The machine refused: permissions, resources, an I/O error.
    Io,
    /// The agent cannot do this on this machine.
    Unsupported,
}

/// One session as the agent sees it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub sid: String,
    pub pid: u32,
    pub terminal: bool,
    /// `None` while the process runs.
    pub exit: Option<ExitInfo>,
    /// Where each output stream ends so far.
    pub ends: StreamEnds,
    /// How much of the session's input has arrived.
    pub input_end: u64,
    pub label: Option<String>,
    pub age_secs: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamEnds {
    pub stdout: u64,
    pub stderr: u64,
}

impl StreamEnds {
    pub fn get(&self, stream: Stream) -> u64 {
        match stream {
            Stream::Stdout => self.stdout,
            Stream::Stderr => self.stderr,
        }
    }
}

/// How a session's process ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitInfo {
    /// The exit code, when the process exited by itself.
    pub code: Option<i32>,
    /// The signal that ended it, on machines that have signals.
    pub signal: Option<i32>,
    pub reason: ExitReason,
    /// Final length of each output stream: the host has everything once it
    /// has received this much.
    pub ends: StreamEnds,
    /// How long the process ran, from its start to its reaping, by the
    /// agent's own monotonic clock. The host's view of the same span also
    /// holds the round trips, the draining of output the process wrote last,
    /// and any time the link was down before this reached it; this one holds
    /// none of that, and needs no agreement between the two machines' clocks.
    /// `None` from an agent older than this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitReason {
    /// The process ended by itself, or by a signal someone other than the
    /// agent sent.
    Exited,
    /// The host asked for it.
    Signalled,
    /// Its host stayed away past the orphan time, or restarted.
    Reclaimed,
    /// The agent itself was shutting down.
    Shutdown,
}

/// The preface a proxy sends the daemon over the machine-local socket before
/// it starts relaying the host's bytes. Never crosses the network.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalHello {
    /// The secret the daemon wrote to its private state file; proves the
    /// proxy runs as the account that owns the daemon.
    pub token: String,
    /// The environment the proxy's SSH login produced. Processes the daemon
    /// starts for this connection inherit it, so they see what a fresh SSH
    /// command would see now rather than what the daemon saw when it started.
    pub env: Vec<(String, String)>,
}

/// One decoded frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub message: Message,
    pub body: Vec<u8>,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// Encodes one frame into a single buffer, so a writer shared behind a lock
/// issues one write per frame.
pub fn encode_frame<T: Serialize>(header: &T, body: &[u8]) -> io::Result<Vec<u8>> {
    let header = serde_json::to_vec(header).map_err(|error| invalid(error.to_string()))?;
    if header.len() > MAX_HEADER_BYTES {
        return Err(invalid(format!("frame header of {} bytes", header.len())));
    }
    if body.len() > MAX_BODY_BYTES {
        return Err(invalid(format!("frame body of {} bytes", body.len())));
    }
    let mut frame = Vec::with_capacity(8 + header.len() + body.len());
    frame.extend_from_slice(&(header.len() as u32).to_be_bytes());
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(&header);
    frame.extend_from_slice(body);
    Ok(frame)
}

/// Writes one frame and flushes it.
pub fn write_frame<W: Write + ?Sized, T: Serialize>(
    out: &mut W,
    header: &T,
    body: &[u8],
) -> io::Result<()> {
    let frame = encode_frame(header, body)?;
    out.write_all(&frame)?;
    out.flush()
}

/// Reads one frame whose header is a `T`. `Ok(None)` is a clean end of stream
/// between frames; an end inside a frame is an error.
pub fn read_frame_as<R: Read + ?Sized, T: for<'de> Deserialize<'de>>(
    input: &mut R,
) -> io::Result<Option<(T, Vec<u8>)>> {
    let mut prefix = [0u8; 8];
    let mut filled = 0;
    while filled < prefix.len() {
        match input.read(&mut prefix[filled..]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the link ended inside a frame",
                ))
            }
            Ok(count) => filled += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    let header_len = u32::from_be_bytes(prefix[..4].try_into().expect("four bytes")) as usize;
    let body_len = u32::from_be_bytes(prefix[4..].try_into().expect("four bytes")) as usize;
    if header_len == 0 || header_len > MAX_HEADER_BYTES {
        return Err(invalid(format!("frame header length {header_len}")));
    }
    if body_len > MAX_BODY_BYTES {
        return Err(invalid(format!("frame body length {body_len}")));
    }
    let mut header = vec![0u8; header_len];
    input.read_exact(&mut header)?;
    let mut body = vec![0u8; body_len];
    input.read_exact(&mut body)?;
    let header = serde_json::from_slice(&header)
        .map_err(|error| invalid(format!("unreadable frame header: {error}")))?;
    Ok(Some((header, body)))
}

/// Reads one [`Message`] frame.
pub fn read_frame<R: Read + ?Sized>(input: &mut R) -> io::Result<Option<Frame>> {
    Ok(read_frame_as::<R, Message>(input)?.map(|(message, body)| Frame { message, body }))
}

/// The line a proxy prints before it starts relaying frames. A login shell's
/// startup files may print anything first (a banner, a `echo` in `.bashrc`),
/// so the host reads up to this line and discards what came before it. The
/// nonce is the host's own, so nothing a startup file prints can pass for it.
pub fn sync_line(nonce: &str) -> String {
    format!("\nMEWORK-REMOTE-READY {nonce}\n")
}

/// What a login shell prints instead of the sync line when the agent is not
/// installed yet: the machine's `uname -s` and `uname -m`.
pub const MISSING_MARKER: &str = "MEWORK-REMOTE-MISSING";

/// Most bytes the host discards while looking for the sync line.
pub const MAX_PREAMBLE_BYTES: usize = 64 * 1024;

/// What a login produced before the protocol began.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Preamble {
    /// The proxy is relaying; frames follow.
    Ready,
    /// The agent is not installed; the machine reported its `uname -s -m`.
    Missing { os: String, arch: String },
}

/// Reads a login's output up to the sync line or the missing-agent report
/// for `nonce`, one byte at a time so nothing past the line is consumed.
pub fn read_preamble<R: Read + ?Sized>(input: &mut R, nonce: &str) -> io::Result<Preamble> {
    let ready = sync_line(nonce);
    let ready = ready.trim_start_matches('\n').trim_end_matches('\n');
    let missing = format!("{MISSING_MARKER} {nonce} ");
    let mut line: Vec<u8> = Vec::with_capacity(128);
    // The last line the login printed, which is usually the reason it stopped:
    // a shell's "command not found", a "Permission denied".
    let mut last_line: Vec<u8> = Vec::new();
    let mut consumed = 0usize;
    let mut byte = [0u8; 1];
    loop {
        match input.read(&mut byte) {
            Ok(0) => {
                let tail = if line.iter().any(|byte| !byte.is_ascii_whitespace()) {
                    &line
                } else {
                    &last_line
                };
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!(
                        "the machine closed the connection before the agent answered{}",
                        describe_tail(tail)
                    ),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
        consumed += 1;
        if consumed > MAX_PREAMBLE_BYTES {
            return Err(invalid(
                "the login printed more than 64 KiB without the agent answering",
            ));
        }
        if byte[0] != b'\n' {
            if line.len() < 4096 {
                line.push(byte[0]);
            }
            continue;
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end_matches('\r');
        if text == ready {
            return Ok(Preamble::Ready);
        }
        if let Some(rest) = text.strip_prefix(&missing) {
            let mut fields = rest.split_whitespace();
            let os = fields.next().unwrap_or_default().to_owned();
            let arch = fields.next().unwrap_or_default().to_owned();
            return Ok(Preamble::Missing { os, arch });
        }
        if line.iter().any(|byte| !byte.is_ascii_whitespace()) {
            last_line = std::mem::take(&mut line);
        } else {
            line.clear();
        }
    }
}

fn describe_tail(line: &[u8]) -> String {
    let text = String::from_utf8_lossy(line);
    let text = text.trim();
    if text.is_empty() {
        String::new()
    } else {
        format!(" (last output: {text})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_round_trips_with_its_body_untouched() {
        let message = Message::Output {
            sid: "s1".into(),
            stream: Stream::Stderr,
            offset: 42,
        };
        let body: Vec<u8> = (0..=255).collect();
        let frame = encode_frame(&message, &body).unwrap();
        let mut cursor = std::io::Cursor::new(frame);
        let decoded = read_frame(&mut cursor).unwrap().unwrap();
        assert_eq!(decoded.message, message);
        assert_eq!(decoded.body, body);
        assert!(read_frame(&mut cursor).unwrap().is_none());
    }

    #[test]
    fn a_stream_cut_inside_a_frame_is_an_error_not_an_end() {
        let frame = encode_frame(&Message::Ping { seq: 1 }, b"").unwrap();
        for cut in 1..frame.len() {
            let mut cursor = std::io::Cursor::new(frame[..cut].to_vec());
            assert!(read_frame(&mut cursor).is_err(), "cut at {cut}");
        }
    }

    #[test]
    fn oversized_or_garbage_lengths_are_refused_before_allocating() {
        let mut garbage = Vec::new();
        garbage.extend_from_slice(&u32::MAX.to_be_bytes());
        garbage.extend_from_slice(&0u32.to_be_bytes());
        assert!(read_frame(&mut std::io::Cursor::new(garbage)).is_err());
        let mut zero = Vec::new();
        zero.extend_from_slice(&0u32.to_be_bytes());
        zero.extend_from_slice(&0u32.to_be_bytes());
        assert!(read_frame(&mut std::io::Cursor::new(zero)).is_err());
    }

    #[test]
    fn requests_serialize_with_readable_tags() {
        let request = Message::Request {
            id: 7,
            op: Op::Signal {
                sid: "s".into(),
                signal: SignalKind::Kill,
            },
        };
        let text = serde_json::to_string(&request).unwrap();
        assert_eq!(
            text,
            r#"{"t":"request","id":7,"op":{"op":"signal","sid":"s","signal":"kill"}}"#
        );
        let spawn: Op = serde_json::from_str(r#"{"op":"spawn","sid":"a","argv":["true"]}"#).unwrap();
        let Op::Spawn(spec) = spawn else {
            panic!("expected a spawn")
        };
        assert_eq!(spec.stdin, StdinMode::Null);
        assert!(spec.terminal.is_none() && spec.env.is_empty());
    }

    #[test]
    fn the_preamble_skips_startup_noise_and_stops_at_the_sync_line() {
        let mut stream = b"Welcome!\r\nsome .bashrc echo\n".to_vec();
        stream.extend_from_slice(sync_line("n0nce").as_bytes());
        stream.extend_from_slice(b"FRAMES");
        let mut cursor = std::io::Cursor::new(stream);
        assert_eq!(read_preamble(&mut cursor, "n0nce").unwrap(), Preamble::Ready);
        let mut rest = Vec::new();
        cursor.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, b"FRAMES");
    }

    #[test]
    fn the_preamble_ignores_another_nonce_and_reads_a_missing_report() {
        let mut stream = sync_line("someone-else").into_bytes();
        stream.extend_from_slice(b"MEWORK-REMOTE-MISSING n0nce Linux x86_64\n");
        assert_eq!(
            read_preamble(&mut std::io::Cursor::new(stream), "n0nce").unwrap(),
            Preamble::Missing {
                os: "Linux".into(),
                arch: "x86_64".into()
            }
        );
        let error = read_preamble(&mut std::io::Cursor::new(b"bash: nope\n".to_vec()), "n0nce")
            .unwrap_err();
        assert!(error.to_string().contains("bash: nope"), "{error}");
    }
}
