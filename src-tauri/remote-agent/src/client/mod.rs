//! The host's link to the agent on one machine.
//!
//! A [`Link`] owns one transport at a time — in practice an `ssh` process
//! whose stdin and stdout reach the agent's proxy — and multiplexes every
//! request, heartbeat and session stream over it. It is built to lose that
//! transport:
//!
//! * **Heartbeat.** The link pings every [`LinkConfig::ping_interval`]. Any
//!   frame counts as a sign of life; when nothing arrives for
//!   [`LinkConfig::dead_after`], the transport is presumed dead even if the
//!   socket looks healthy — a sleeping laptop's TCP connection can stay "open"
//!   for many minutes — and it is torn down.
//! * **Reconnect.** A supervisor thread starts a new transport with growing
//!   backoff. The agent recognizes the host by its client id and epoch, keeps
//!   its sessions, and resumes each output stream from the offset this side
//!   reports; requests that got no reply are sent again (every operation is
//!   idempotent, see [`crate::protocol`]), and so is input the agent did not
//!   confirm.
//! * **Giving up.** After [`LinkConfig::give_up_after`] of failed attempts the
//!   link reports [`LinkStatus::Lost`] and fails what is waiting on it. The
//!   sessions are still on the machine until their orphan time; a later
//!   request tries again.
//!
//! Callers never see any of this unless the link gives up: a process started
//! through [`Link::spawn`] reads, writes and waits like a local child whose
//! output occasionally pauses.

mod child;
mod session;

use std::collections::{BTreeMap, HashMap};
use std::hash::{BuildHasher, Hasher};
use std::io::{BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::protocol::{
    self, AgentInfo, Failure, Frame, Hello, Message, Op, Outcome, Policy, Reply, ResumePoint,
    SpawnSpec, PROTOCOL_VERSION,
};
pub use child::{ChildLauncher, StderrSink};
pub use session::{RemoteProcess, SessionReader, SessionWriter};
use session::SessionPipe;

/// Answers a sandboxed agent's [`Message::Dial`]: opens the connection, or
/// says whether the policy refused it and why. Only a daemon's link to one of
/// its cells has one (see `agent::cells`); every other link refuses dials.
#[derive(Clone)]
pub struct DialHandler(Arc<dyn Fn(&str, u16) -> Result<std::net::TcpStream, (bool, String)> + Send + Sync>);

impl DialHandler {
    pub fn new(handler: impl Fn(&str, u16) -> Result<std::net::TcpStream, (bool, String)> + Send + Sync + 'static) -> Self {
        Self(Arc::new(handler))
    }
}

impl std::fmt::Debug for DialHandler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DialHandler")
    }
}

/// Starts a fresh transport to the agent's proxy on one machine.
///
/// The launcher owns everything specific to reaching the machine: which
/// program to run, how to install the agent when it is missing, and reading
/// the proxy's sync line ([`protocol::read_preamble`]). What it returns is
/// positioned at the first frame.
pub trait Launcher: Send + Sync + 'static {
    fn launch(&self, nonce: &str) -> Result<Transport, LaunchError>;
}

pub struct Transport {
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    /// Ends the transport. Must make `reader` reach end of file promptly: the
    /// link calls it to abandon a transport that stopped answering.
    pub closer: Box<dyn FnOnce() + Send>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchError {
    /// The machine could not be reached, or the attempt failed in a way a
    /// later attempt may not. Retried with backoff.
    Unreachable(String),
    /// The machine was reached but the agent cannot run there: no build for
    /// its platform, a home it cannot write to. Not retried; the owner of the
    /// link decides what to do instead.
    Unavailable(String),
}

#[derive(Clone, Debug)]
pub struct LinkConfig {
    /// Stable for this host installation.
    pub client_id: String,
    /// New for every host process.
    pub epoch: String,
    pub policy: Policy,
    pub ping_interval: Duration,
    pub dead_after: Duration,
    /// How long the agent has to answer the hello before the attempt counts
    /// as failed.
    pub handshake_timeout: Duration,
    pub give_up_after: Duration,
    pub backoff_initial: Duration,
    pub backoff_max: Duration,
    /// Opens the connections a sandboxed agent asks for.
    pub dialer: Option<DialHandler>,
}

impl LinkConfig {
    pub fn new(client_id: impl Into<String>, epoch: impl Into<String>) -> Self {
        Self {
            client_id: client_id.into(),
            epoch: epoch.into(),
            policy: Policy::default(),
            ping_interval: Duration::from_secs(5),
            dead_after: Duration::from_secs(20),
            handshake_timeout: Duration::from_secs(45),
            give_up_after: Duration::from_secs(5 * 60),
            backoff_initial: Duration::from_millis(500),
            backoff_max: Duration::from_secs(15),
            dialer: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkStatus {
    /// The first attempt is in progress.
    Connecting,
    Connected {
        agent: AgentInfo,
    },
    /// The transport dropped and a new one is being attempted.
    Reconnecting {
        attempt: u32,
        error: String,
    },
    /// Attempts kept failing for too long. A new request tries once more.
    Lost {
        error: String,
    },
    /// The agent cannot run on this machine.
    Unavailable {
        error: String,
    },
    Closed,
}

/// Why a request got no reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallError {
    /// The agent answered with a failure.
    Failed(Failure),
    /// No reply within the caller's time; the link may be reconnecting.
    Timeout,
    /// The link gave up, was closed, or cannot reach an agent at all.
    Link(String),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Failed(failure) => formatter.write_str(&failure.message),
            Self::Timeout => formatter.write_str("the remote machine did not answer in time"),
            Self::Link(reason) => formatter.write_str(reason),
        }
    }
}

type Observer = Arc<dyn Fn(&LinkStatus) + Send + Sync>;
/// The current connection's writer, shared by every thread that sends.
type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;
type Closer = Box<dyn FnOnce() + Send>;
/// A closer that whichever of two parties gets there first may run.
type SharedCloser = Arc<Mutex<Option<Closer>>>;

struct Connection {
    generation: u64,
    writer: SharedWriter,
    closer: Option<Closer>,
    /// Set by the connection's reader when the stream ended.
    reader_done: Arc<AtomicBool>,
}

struct Pending {
    frame: Vec<u8>,
    /// The connection this copy was last written to.
    sent_generation: u64,
    outcome: Option<Outcome>,
    /// Nobody waits for the reply; it is dropped when it arrives.
    detached: bool,
    /// The session a spawn request creates, so a reconnect does not mistake a
    /// session still being started for one the agent lost.
    spawns: Option<String>,
}

struct LinkState {
    status: LinkStatus,
    connection: Option<Connection>,
    next_id: u64,
    pending: BTreeMap<u64, Pending>,
    sessions: HashMap<String, Arc<SessionPipe>>,
    last_inbound: Instant,
    last_ping: Instant,
    ping_seq: u64,
    closed: bool,
    /// Someone asked for another attempt after the link gave up.
    wake: bool,
    /// Counts give-ups, so a caller can tell a give-up that happened while it
    /// waited from one that was already there when it started.
    lost_count: u64,
    ever_connected: bool,
}

struct Shared {
    config: LinkConfig,
    launcher: Box<dyn Launcher>,
    /// Connections the agent asked for, carried inside the link.
    tunnels: Arc<crate::tunnel::Tunnels>,
    state: Mutex<LinkState>,
    cond: Condvar,
    /// The current connection's generation, readable without the state lock.
    generation: AtomicU64,
    observer: Mutex<Option<Observer>>,
    sid_counter: AtomicU64,
}

/// The host's link to one machine. Cheap to clone; every clone is the same
/// link.
#[derive(Clone)]
pub struct Link {
    shared: Arc<Shared>,
}

impl Link {
    /// Creates the link and starts connecting in the background.
    pub fn start(config: LinkConfig, launcher: impl Launcher) -> Self {
        let now = Instant::now();
        let shared = Arc::new_cyclic(|this: &std::sync::Weak<Shared>| Shared {
            config,
            launcher: Box::new(launcher),
            tunnels: {
                let this = this.clone();
                crate::tunnel::Tunnels::new(Arc::new(move |message: &Message, body: &[u8]| {
                    let Some(shared) = this.upgrade() else {
                        return false;
                    };
                    let Some((writer, generation)) = shared.current_writer() else {
                        return false;
                    };
                    match protocol::encode_frame(message, body) {
                        Ok(frame) => shared.write(&writer, generation, &frame),
                        Err(_) => false,
                    }
                }))
            },
            state: Mutex::new(LinkState {
                status: LinkStatus::Connecting,
                connection: None,
                next_id: 1,
                pending: BTreeMap::new(),
                sessions: HashMap::new(),
                last_inbound: now,
                last_ping: now,
                ping_seq: 0,
                closed: false,
                wake: false,
                lost_count: 0,
                ever_connected: false,
            }),
            cond: Condvar::new(),
            generation: AtomicU64::new(0),
            observer: Mutex::new(None),
            sid_counter: AtomicU64::new(0),
        });
        let supervisor = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("remote-link".into())
            .spawn(move || supervise(supervisor))
            .expect("the link's supervisor thread starts");
        Self { shared }
    }

    /// Called with every status change, outside any lock of the link's.
    pub fn set_observer(&self, observer: impl Fn(&LinkStatus) + Send + Sync + 'static) {
        *lock(&self.shared.observer) = Some(Arc::new(observer));
    }

    pub fn status(&self) -> LinkStatus {
        lock(&self.shared.state).status.clone()
    }

    /// Waits until the link is connected, or until it is clear it will not be.
    pub fn wait_connected(&self, timeout: Duration) -> Result<AgentInfo, CallError> {
        let deadline = Instant::now() + timeout;
        let mut state = lock(&self.shared.state);
        loop {
            match &state.status {
                LinkStatus::Connected { agent } => return Ok(agent.clone()),
                LinkStatus::Unavailable { error } | LinkStatus::Lost { error } => {
                    return Err(CallError::Link(error.clone()))
                }
                LinkStatus::Closed => return Err(CallError::Link("the link is closed".into())),
                LinkStatus::Connecting | LinkStatus::Reconnecting { .. } => {}
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(CallError::Timeout);
            }
            state = self
                .shared
                .cond
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
    }

    /// Like [`Self::wait_connected`], except that a link which has never
    /// connected reports its first failed attempt at once instead of waiting
    /// through its retries: a machine that cannot be reached right now is
    /// better reported to the caller than retried in front of it.
    pub fn wait_ready(&self, timeout: Duration) -> Result<AgentInfo, CallError> {
        let deadline = Instant::now() + timeout;
        let mut state = lock(&self.shared.state);
        loop {
            match &state.status {
                LinkStatus::Connected { agent } => return Ok(agent.clone()),
                LinkStatus::Unavailable { error } | LinkStatus::Lost { error } => {
                    return Err(CallError::Link(error.clone()))
                }
                LinkStatus::Closed => return Err(CallError::Link("the link is closed".into())),
                LinkStatus::Reconnecting { error, .. } if !state.ever_connected && !error.is_empty() => {
                    return Err(CallError::Link(error.clone()))
                }
                LinkStatus::Connecting | LinkStatus::Reconnecting { .. } => {}
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(CallError::Timeout);
            }
            state = self
                .shared
                .cond
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
    }

    /// A session id no other session of this host process uses.
    pub fn new_sid(&self, prefix: &str) -> String {
        let count = self.shared.sid_counter.fetch_add(1, Ordering::SeqCst);
        format!("{prefix}-{count}-{}", &random_token()[..8])
    }

    /// Sends one request and waits up to `timeout` for its reply.
    pub fn call(&self, op: Op, body: &[u8], timeout: Duration) -> Result<Reply, CallError> {
        self.call_inner(op, body, timeout, None)
    }

    /// Starts a process on the machine.
    pub fn spawn(&self, spec: SpawnSpec, body: &[u8], timeout: Duration) -> Result<RemoteProcess, CallError> {
        let sid = spec.sid.clone();
        let pipe = Arc::new(SessionPipe::new(sid.clone()));
        {
            let mut state = lock(&self.shared.state);
            if state.sessions.contains_key(&sid) {
                return Err(CallError::Failed(Failure::new(
                    protocol::FailureKind::Invalid,
                    "A session with this id is already open",
                )));
            }
            state.sessions.insert(sid.clone(), Arc::clone(&pipe));
        }
        match self.call_inner(Op::Spawn(spec), body, timeout, Some(sid.clone())) {
            Ok(Reply::Spawned { pid }) => {
                lock(&pipe.state).spawned = true;
                Ok(RemoteProcess::new(self.clone(), pipe, pid))
            }
            Ok(other) => {
                self.forget_session(&sid, true);
                Err(CallError::Link(format!("unexpected reply to a spawn: {other:?}")))
            }
            Err(error) => {
                // Without a reply the process may or may not have started;
                // releasing covers both.
                self.forget_session(&sid, !matches!(error, CallError::Failed(_)));
                Err(error)
            }
        }
    }

    /// Ends the link. With `release`, the agent ends every session this host
    /// owns right away instead of keeping them for its orphan time.
    pub fn close(&self, release: bool) {
        let (connection, pipes) = {
            let mut state = lock(&self.shared.state);
            if state.closed {
                return;
            }
            state.closed = true;
            state.status = LinkStatus::Closed;
            state.pending.clear();
            let pipes: Vec<_> = state.sessions.drain().map(|(_, pipe)| pipe).collect();
            (state.connection.take(), pipes)
        };
        if let Some(mut connection) = connection {
            if let Ok(frame) = protocol::encode_frame(&Message::Bye { release }, &[]) {
                let written = {
                    let mut writer = lock(&connection.writer);
                    writer.write_all(&frame).and_then(|_| writer.flush()).is_ok()
                };
                // The agent ends the connection once it has acted on the
                // goodbye; killing the transport first could lose it in a
                // pipe buffer on the way.
                if written {
                    let deadline = Instant::now() + Duration::from_secs(2);
                    while Instant::now() < deadline && !connection.reader_done.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
            }
            if let Some(closer) = connection.closer.take() {
                closer();
            }
        }
        for pipe in pipes {
            pipe.fail("the link to the machine was closed");
        }
        self.shared.cond.notify_all();
        self.shared.notify_observer();
    }

    fn call_inner(
        &self,
        op: Op,
        body: &[u8],
        timeout: Duration,
        spawns: Option<String>,
    ) -> Result<Reply, CallError> {
        let deadline = Instant::now() + timeout;
        let (id, lost_at_start) = self.register(op, body, false, spawns)?;
        let mut state = lock(&self.shared.state);
        loop {
            if let Some(pending) = state.pending.get_mut(&id) {
                if let Some(outcome) = pending.outcome.take() {
                    state.pending.remove(&id);
                    return match outcome {
                        Outcome::Ok { reply } => Ok(reply),
                        Outcome::Err { failure } => Err(CallError::Failed(failure)),
                    };
                }
            } else {
                // Cleared by a close.
                return Err(CallError::Link("the link to the machine was closed".into()));
            }
            match &state.status {
                LinkStatus::Unavailable { error } => {
                    let error = error.clone();
                    state.pending.remove(&id);
                    return Err(CallError::Link(error));
                }
                LinkStatus::Lost { error } if state.lost_count > lost_at_start => {
                    let error = error.clone();
                    state.pending.remove(&id);
                    return Err(CallError::Link(error));
                }
                LinkStatus::Closed => {
                    state.pending.remove(&id);
                    return Err(CallError::Link("the link to the machine was closed".into()));
                }
                _ => {}
            }
            let now = Instant::now();
            if now >= deadline {
                state.pending.remove(&id);
                return Err(CallError::Timeout);
            }
            state = self
                .shared
                .cond
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
    }

    /// Records a request and writes it if a connection is up. Returns its id
    /// and the give-up count at the time, for [`Self::call_inner`].
    fn register(
        &self,
        op: Op,
        body: &[u8],
        detached: bool,
        spawns: Option<String>,
    ) -> Result<(u64, u64), CallError> {
        let (id, frame, writer, lost_count) = {
            let mut state = lock(&self.shared.state);
            match &state.status {
                LinkStatus::Closed => return Err(CallError::Link("the link to the machine was closed".into())),
                LinkStatus::Unavailable { error } => return Err(CallError::Link(error.clone())),
                LinkStatus::Lost { .. } => {
                    state.wake = true;
                    self.shared.cond.notify_all();
                }
                _ => {}
            }
            let id = state.next_id;
            state.next_id += 1;
            let frame = protocol::encode_frame(&Message::Request { id, op }, body)
                .map_err(|error| CallError::Link(format!("cannot encode the request: {error}")))?;
            let writer = state
                .connection
                .as_ref()
                .map(|connection| (Arc::clone(&connection.writer), connection.generation));
            state.pending.insert(
                id,
                Pending {
                    frame: frame.clone(),
                    sent_generation: writer.as_ref().map(|(_, generation)| *generation).unwrap_or(0),
                    outcome: None,
                    detached,
                    spawns,
                },
            );
            (id, frame, writer, state.lost_count)
        };
        if let Some((writer, generation)) = writer {
            self.shared.write(&writer, generation, &frame);
        }
        Ok((id, lost_count))
    }

    fn request_detached(&self, op: Op) {
        let _ = self.register(op, &[], true, None);
    }

    fn release(&self, sid: &str) {
        self.forget_session(sid, true);
    }

    fn forget_session(&self, sid: &str, tell_agent: bool) {
        lock(&self.shared.state).sessions.remove(sid);
        if tell_agent {
            self.request_detached(Op::Release { sid: sid.to_owned() });
        }
    }

    /// Writes whatever input of `pipe` no connection has been given yet.
    ///
    /// The pipe's sending lock is held across the write, so two writers of
    /// one session cannot put its input on the wire out of order; the agent
    /// accepts input strictly by offset. The pipe's state lock is not: the
    /// link's reader takes it to deliver output, and must not wait behind a
    /// write stuck on a congested network.
    fn send_input(&self, pipe: &SessionPipe) {
        let Some((writer, generation)) = self.shared.current_writer() else {
            return;
        };
        let _in_order = lock(&pipe.sending);
        let unsent = {
            let state = lock(&pipe.state);
            if !state.spawned {
                return;
            }
            state.input.unsent()
        };
        let Some((offset, bytes)) = unsent else {
            return;
        };
        let mut position = offset;
        for chunk in bytes.chunks(protocol::OUTPUT_CHUNK_BYTES) {
            let Ok(frame) = protocol::encode_frame(
                &Message::Input {
                    sid: pipe.sid.clone(),
                    offset: position,
                },
                chunk,
            ) else {
                return;
            };
            let written = {
                let mut writer = lock(&writer);
                writer.write_all(&frame).and_then(|_| writer.flush()).is_ok()
            };
            if !written {
                self.shared.connection_lost(generation, "writing to the machine failed");
                return;
            }
            position += chunk.len() as u64;
        }
        // Written to a connection that has since been replaced counts for
        // nothing: the new one resends from what the agent confirmed.
        if self.shared.generation.load(Ordering::SeqCst) == generation {
            lock(&pipe.state).input.mark_sent(position);
        }
    }

    /// Whether the link carries nothing: no session open, no request waiting.
    pub fn is_idle(&self) -> bool {
        let state = lock(&self.shared.state);
        state.sessions.is_empty() && state.pending.values().all(|pending| pending.detached)
    }
}

impl Shared {
    fn current_writer(&self) -> Option<(SharedWriter, u64)> {
        lock(&self.state)
            .connection
            .as_ref()
            .map(|connection| (Arc::clone(&connection.writer), connection.generation))
    }

    /// Writes one frame to the connection of `generation`. A failure ends
    /// that connection; whatever the frame carried is resent on the next.
    fn write(&self, writer: &Mutex<Box<dyn Write + Send>>, generation: u64, frame: &[u8]) -> bool {
        let written = {
            let mut writer = lock(writer);
            writer.write_all(frame).and_then(|_| writer.flush()).is_ok()
        };
        if !written {
            self.connection_lost(generation, "writing to the machine failed");
        }
        written
    }

    /// Abandons the connection of `generation`, if it is still the current one.
    fn connection_lost(&self, generation: u64, reason: &str) {
        let closer = {
            let mut state = lock(&self.state);
            let current = state
                .connection
                .as_ref()
                .is_some_and(|connection| connection.generation == generation);
            if !current {
                return;
            }
            let mut connection = state.connection.take().expect("checked above");
            if !state.closed {
                state.status = LinkStatus::Reconnecting {
                    attempt: 0,
                    error: reason.to_owned(),
                };
            }
            connection.closer.take()
        };
        if let Some(closer) = closer {
            closer();
        }
        // A tunnelled connection cannot be resumed on another transport.
        self.tunnels.close_all();
        self.cond.notify_all();
        self.notify_observer();
    }

    fn notify_observer(&self) {
        let observer = lock(&self.observer).clone();
        if let Some(observer) = observer {
            let status = lock(&self.state).status.clone();
            observer(&status);
        }
    }

    fn set_status(&self, status: LinkStatus) {
        {
            let mut state = lock(&self.state);
            if state.closed || state.status == status {
                return;
            }
            state.status = status;
        }
        self.cond.notify_all();
        self.notify_observer();
    }

    /// Handles one frame from the agent.
    fn dispatch(&self, frame: Frame) {
        let mut state = lock(&self.state);
        state.last_inbound = Instant::now();
        match frame.message {
            // Any frame is a sign of life, which `last_inbound` has recorded.
            Message::Pong { .. } => {}
            Message::Response { id, outcome } => {
                if let Some(pending) = state.pending.get_mut(&id) {
                    if pending.detached {
                        state.pending.remove(&id);
                    } else {
                        pending.outcome = Some(outcome);
                        drop(state);
                        self.cond.notify_all();
                    }
                }
            }
            Message::Output { sid, stream, offset } => {
                if let Some(pipe) = state.sessions.get(&sid).cloned() {
                    drop(state);
                    pipe.deliver(stream, offset, &frame.body);
                }
            }
            Message::Gap { sid, stream, to, .. } => {
                if let Some(pipe) = state.sessions.get(&sid).cloned() {
                    drop(state);
                    pipe.gap(stream, to);
                }
            }
            Message::Exit { sid, exit } => {
                if let Some(pipe) = state.sessions.get(&sid).cloned() {
                    drop(state);
                    pipe.finish(exit);
                }
            }
            Message::Dial { conn, host, port } => {
                drop(state);
                let tunnels = Arc::clone(&self.tunnels);
                let Some(dialer) = self.config.dialer.clone() else {
                    tunnels.refuse(conn, true, "this link carries no connections".into());
                    return;
                };
                if tunnels.open_count() >= crate::tunnel::MAX_CONNECTIONS {
                    tunnels.refuse(conn, false, "too many connections are open".into());
                    return;
                }
                let spawned = std::thread::Builder::new()
                    .name("tunnel-dial".into())
                    .spawn(move || match (dialer.0)(&host, port) {
                        Ok(stream) => {
                            let _ = stream.set_nodelay(true);
                            if let Some(upstream) = tunnels.accept(conn) {
                                crate::tunnel::relay(stream, Vec::new(), upstream);
                            }
                        }
                        Err((refused, error)) => tunnels.refuse(conn, refused, error),
                    });
                if spawned.is_err() {
                    self.tunnels.refuse(conn, false, "the agent is out of threads".into());
                }
            }
            message @ (Message::Dialed { .. }
            | Message::Tunnel { .. }
            | Message::TunnelAck { .. }
            | Message::TunnelEnd { .. }
            | Message::TunnelClose { .. }) => {
                drop(state);
                self.tunnels.dispatch(&message, &frame.body);
            }
            _ => {}
        }
    }

    /// One attempt: launch, hello, welcome, then hand the stream to a reader
    /// thread and resend what the previous connection left unconfirmed.
    fn connect_once(self: &Arc<Self>) -> Result<(), LaunchError> {
        let nonce = random_token();
        let transport = self.launcher.launch(&nonce)?;
        let Transport {
            reader,
            writer,
            closer,
        } = transport;
        let closer: SharedCloser = Arc::new(Mutex::new(Some(closer)));
        let close_now = |closer: &SharedCloser| {
            if let Some(closer) = lock(closer).take() {
                closer();
            }
        };
        let mut writer = writer;
        let mut reader = BufReader::with_capacity(256 * 1024, reader);

        let resume: Vec<ResumePoint> = {
            let state = lock(&self.state);
            state
                .sessions
                .values()
                .map(|pipe| {
                    let (stdout, stderr) = pipe.received();
                    ResumePoint {
                        sid: pipe.sid.clone(),
                        stdout,
                        stderr,
                    }
                })
                .collect()
        };
        let hello = Message::Hello(Hello {
            protocol: PROTOCOL_VERSION,
            client: self.config.client_id.clone(),
            epoch: self.config.epoch.clone(),
            policy: self.config.policy,
            resume,
        });
        if let Err(error) = protocol::write_frame(&mut writer, &hello, &[]) {
            close_now(&closer);
            return Err(LaunchError::Unreachable(format!("cannot greet the agent: {error}")));
        }

        // The agent answers the hello at once; a transport that does not is
        // abandoned rather than waited on forever.
        let answered = Arc::new(AtomicBool::new(false));
        {
            let answered = Arc::clone(&answered);
            let closer = Arc::clone(&closer);
            let timeout = self.config.handshake_timeout;
            std::thread::spawn(move || {
                let deadline = Instant::now() + timeout;
                while Instant::now() < deadline {
                    if answered.load(Ordering::SeqCst) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                if !answered.load(Ordering::SeqCst) {
                    if let Some(closer) = lock(&closer).take() {
                        closer();
                    }
                }
            });
        }
        let welcome = loop {
            match protocol::read_frame(&mut reader) {
                Ok(Some(Frame {
                    message: Message::Welcome(welcome),
                    ..
                })) => break welcome,
                Ok(Some(Frame {
                    message: Message::Refused { reason },
                    ..
                })) => {
                    answered.store(true, Ordering::SeqCst);
                    close_now(&closer);
                    return Err(LaunchError::Unavailable(format!("the agent refused the host: {reason}")));
                }
                Ok(Some(_)) => continue,
                Ok(None) => {
                    answered.store(true, Ordering::SeqCst);
                    close_now(&closer);
                    return Err(LaunchError::Unreachable(
                        "the connection closed before the agent answered".into(),
                    ));
                }
                Err(error) => {
                    answered.store(true, Ordering::SeqCst);
                    close_now(&closer);
                    return Err(LaunchError::Unreachable(format!("the agent's answer was unreadable: {error}")));
                }
            }
        };
        answered.store(true, Ordering::SeqCst);
        let Some(closer) = lock(&closer).take() else {
            return Err(LaunchError::Unreachable("the agent did not answer in time".into()));
        };
        if welcome.protocol != PROTOCOL_VERSION {
            closer();
            return Err(LaunchError::Unavailable(format!(
                "the agent speaks protocol {}, the host {PROTOCOL_VERSION}",
                welcome.protocol
            )));
        }

        let writer = Arc::new(Mutex::new(writer));
        let reader_done = Arc::new(AtomicBool::new(false));
        let (generation, resend, pipes) = {
            let mut state = lock(&self.state);
            if state.closed {
                drop(state);
                closer();
                return Ok(());
            }
            let generation = self.generation.load(Ordering::SeqCst) + 1;
            self.generation.store(generation, Ordering::SeqCst);
            state.connection = Some(Connection {
                generation,
                writer: Arc::clone(&writer),
                closer: Some(closer),
                reader_done: Arc::clone(&reader_done),
            });
            state.status = LinkStatus::Connected {
                agent: welcome.agent.clone(),
            };
            state.ever_connected = true;
            let now = Instant::now();
            state.last_inbound = now;
            state.last_ping = now;

            let known: HashMap<&str, &protocol::SessionInfo> = welcome
                .sessions
                .iter()
                .map(|session| (session.sid.as_str(), session))
                .collect();
            let starting: std::collections::HashSet<String> = state
                .pending
                .values()
                .filter_map(|pending| pending.spawns.clone())
                .collect();
            let mut lost = Vec::new();
            for (sid, pipe) in &state.sessions {
                match known.get(sid.as_str()) {
                    Some(info) => lock(&pipe.state).input.rewind(info.input_end),
                    None if starting.contains(sid) => {}
                    None => lost.push(Arc::clone(pipe)),
                }
            }
            for pipe in &lost {
                state.sessions.remove(&pipe.sid);
                pipe.fail("the machine no longer has this process: it was reclaimed, or the agent restarted, while the link was down");
            }
            let mut resend = Vec::new();
            for pending in state.pending.values_mut() {
                if pending.sent_generation < generation {
                    pending.sent_generation = generation;
                    resend.push(pending.frame.clone());
                }
            }
            let pipes: Vec<_> = state.sessions.values().cloned().collect();
            (generation, resend, pipes)
        };

        {
            let shared = Arc::clone(self);
            std::thread::Builder::new()
                .name("remote-link-reader".into())
                .spawn(move || {
                    let reason = loop {
                        match protocol::read_frame(&mut reader) {
                            Ok(Some(frame)) => shared.dispatch(frame),
                            Ok(None) => break "the machine closed the connection".to_owned(),
                            Err(error) => break format!("the link broke: {error}"),
                        }
                    };
                    reader_done.store(true, Ordering::SeqCst);
                    shared.connection_lost(generation, &reason);
                })
                .map_err(|error| LaunchError::Unreachable(format!("cannot start the link reader: {error}")))?;
        }
        for frame in resend {
            if !self.write(&writer, generation, &frame) {
                return Ok(());
            }
        }
        let link = Link {
            shared: Arc::clone(self),
        };
        for pipe in pipes {
            link.send_input(&pipe);
        }
        self.cond.notify_all();
        self.notify_observer();
        Ok(())
    }

    /// Fails everything waiting on the link after it gave up.
    fn give_up(&self, status: LinkStatus) {
        let pipes = {
            let mut state = lock(&self.state);
            if state.closed {
                return;
            }
            state.status = status.clone();
            state.lost_count += 1;
            state.wake = false;
            state.pending.retain(|_, pending| !pending.detached);
            if matches!(status, LinkStatus::Unavailable { .. }) {
                state.sessions.drain().map(|(_, pipe)| pipe).collect::<Vec<_>>()
            } else {
                state.sessions.values().cloned().collect()
            }
        };
        let reason = match &status {
            LinkStatus::Lost { error } | LinkStatus::Unavailable { error } => error.clone(),
            _ => String::new(),
        };
        for pipe in pipes {
            pipe.fail(&format!("lost the link to the machine: {reason}"));
        }
        self.cond.notify_all();
        self.notify_observer();
    }
}

/// The link's own thread: connects, keeps the connection alive, and replaces
/// it when it dies.
fn supervise(shared: Arc<Shared>) {
    let config = shared.config.clone();
    let mut attempt: u32 = 0;
    let mut failing_since: Option<Instant> = None;
    // After a give-up, a new request buys a short window of fresh attempts
    // rather than the whole give-up time again.
    let mut revive_until: Option<Instant> = None;
    let mut last_error = String::new();
    loop {
        // Phase one: decide what to do under the lock.
        enum Next {
            Stop,
            Connect,
            Idle,
            Ping(SharedWriter, u64, u64),
            Dead(u64),
            Wait(Duration),
        }
        let next = {
            let mut state = lock(&shared.state);
            if state.closed {
                Next::Stop
            } else if state.connection.is_none() {
                match state.status {
                    LinkStatus::Lost { .. } | LinkStatus::Unavailable { .. } if !state.wake => Next::Idle,
                    _ => {
                        if state.wake {
                            state.wake = false;
                            revive_until = Some(Instant::now() + config.backoff_max * 2);
                        }
                        Next::Connect
                    }
                }
            } else {
                let (writer, generation) = {
                    let connection = state.connection.as_ref().expect("checked");
                    (Arc::clone(&connection.writer), connection.generation)
                };
                let now = Instant::now();
                if now.duration_since(state.last_inbound) >= config.dead_after {
                    Next::Dead(generation)
                } else if now.duration_since(state.last_ping) >= config.ping_interval {
                    state.ping_seq += 1;
                    state.last_ping = now;
                    let seq = state.ping_seq;
                    Next::Ping(writer, generation, seq)
                } else {
                    let until_ping = config.ping_interval.saturating_sub(now.duration_since(state.last_ping));
                    let until_dead = config.dead_after.saturating_sub(now.duration_since(state.last_inbound));
                    Next::Wait(until_ping.min(until_dead).max(Duration::from_millis(10)))
                }
            }
        };
        // Phase two: act without it.
        match next {
            Next::Stop => return,
            Next::Idle => {
                let state = lock(&shared.state);
                let _ = shared.cond.wait_timeout(state, Duration::from_secs(1));
            }
            Next::Wait(duration) => {
                let state = lock(&shared.state);
                let _ = shared.cond.wait_timeout(state, duration);
            }
            Next::Ping(writer, generation, seq) => {
                if let Ok(frame) = protocol::encode_frame(&Message::Ping { seq }, &[]) {
                    shared.write(&writer, generation, &frame);
                }
            }
            Next::Dead(generation) => {
                shared.connection_lost(generation, "the machine stopped answering heartbeats");
            }
            Next::Connect => {
                attempt += 1;
                let ever_connected = lock(&shared.state).ever_connected;
                shared.set_status(if ever_connected || attempt > 1 {
                    LinkStatus::Reconnecting {
                        attempt,
                        error: last_error.clone(),
                    }
                } else {
                    LinkStatus::Connecting
                });
                match shared.connect_once() {
                    Ok(()) => {
                        attempt = 0;
                        failing_since = None;
                        revive_until = None;
                        last_error.clear();
                    }
                    Err(LaunchError::Unavailable(error)) => {
                        shared.give_up(LinkStatus::Unavailable { error });
                    }
                    Err(LaunchError::Unreachable(error)) => {
                        let since = *failing_since.get_or_insert_with(Instant::now);
                        last_error = error.clone();
                        let exhausted = match revive_until {
                            Some(until) => Instant::now() >= until,
                            None => since.elapsed() >= config.give_up_after,
                        };
                        if exhausted {
                            shared.give_up(LinkStatus::Lost { error });
                            attempt = 0;
                            failing_since = None;
                            revive_until = None;
                            continue;
                        }
                        shared.set_status(LinkStatus::Reconnecting {
                            attempt,
                            error,
                        });
                        let backoff = config
                            .backoff_initial
                            .saturating_mul(1u32 << attempt.min(16).saturating_sub(1))
                            .min(config.backoff_max);
                        let state = lock(&shared.state);
                        if !state.closed {
                            let _ = shared.cond.wait_timeout(state, backoff);
                        }
                    }
                }
            }
        }
    }
}

/// A random alphanumeric token, for sync nonces and session ids. Seeded by
/// the standard library's per-instance random hash keys; unpredictability
/// here guards against accidents, not attackers.
pub fn random_token() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut out = String::with_capacity(32);
    for round in 0..2u64 {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u128(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0),
        );
        hasher.write_u64(COUNTER.fetch_add(1, Ordering::SeqCst));
        hasher.write_u64(round);
        out.push_str(&format!("{:016x}", hasher.finish()));
    }
    out
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}
