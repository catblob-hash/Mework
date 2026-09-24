//! The daemon: the one long-lived process the host keeps on a machine.
//!
//! It accepts proxies on a machine-local socket, one per SSH session. Each
//! connection belongs to a *client* — a host installation — and a client has at
//! most one connection: a new one from the same client replaces the old,
//! which is exactly what a reconnect after a dropped link looks like from here.
//!
//! Sessions belong to a client and an *epoch*, the host process that created
//! them. Three things end a session without the host asking:
//!
//! * its host stays away longer than the session's orphan time (the janitor);
//! * its host comes back as a new process, which can no longer reach it
//!   ([`Daemon::introduce`]);
//! * the daemon itself shuts down ([`Daemon::shutdown`]).
//!
//! A connection that stops sending — the host pings every few seconds — is
//! closed after the client's silence timeout even when the socket itself
//! looks healthy, so a proxy stranded behind a dead network does not hold the
//! client's place.
//!
//! The same daemon also runs over its own standard input and output
//! ([`run_stdio`]): as the agent Mework keeps on its own machine, started by
//! the host for its sandboxed conversations, and as a *cell* — one sandboxed
//! conversation — started by another daemon (see [`cells`]). Such a daemon
//! has exactly one connection, its parent, and ends with it.

pub mod cells;
pub mod net;
pub mod paths;
pub mod platform;
pub mod proxy;
pub mod sandbox;
pub mod session;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::protocol::{
    self, AgentInfo, ExitReason, Failure, FailureKind, Hello, LocalHello, Message, Op, Outcome,
    Policy, Reply, Stream, Welcome, OUTPUT_CHUNK_BYTES, PROTOCOL_VERSION,
};
use paths::Paths;
use platform::LocalStream;
use session::{lock, Notifier, Owner, Session, SpawnContext};

use std::io::Read;
use std::sync::OnceLock;

/// Knobs the daemon is started with. The defaults are what a real machine
/// gets; the tests shorten them.
#[derive(Clone, Debug)]
pub struct DaemonOptions {
    /// With no connection and no session for this long, the daemon exits.
    pub idle_exit: Duration,
    /// How often the janitor looks at connections and sessions.
    pub tick: Duration,
    pub max_sessions_per_client: usize,
    /// A finished session its connected host never released is forgotten
    /// after this long, so a host that loses track cannot grow the daemon.
    pub finished_linger: Duration,
    /// Write the log to the log file itself rather than to the standard
    /// handles the daemon was started with ([`platform::OWN_LOG_FLAG`]).
    pub own_log: bool,
    /// The root directory, as the proxy that started the daemon found it.
    pub root: Option<std::path::PathBuf>,
}

impl Default for DaemonOptions {
    fn default() -> Self {
        Self {
            idle_exit: Duration::from_secs(10 * 60),
            tick: Duration::from_secs(1),
            max_sessions_per_client: 256,
            finished_linger: Duration::from_secs(60 * 60),
            own_log: false,
            root: None,
        }
    }
}

/// Bounds on what a host may ask of the janitor. A policy outside them is
/// clamped rather than refused: the host's intent is clear, only its numbers
/// are not.
const MIN_SILENCE: Duration = Duration::from_secs(5);
const MAX_SILENCE: Duration = Duration::from_secs(10 * 60);
const MAX_ORPHAN_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// How long a proxy has to present itself before its connection is dropped.
const PREFACE_TIMEOUT: Duration = Duration::from_secs(15);

struct Connection {
    id: u64,
    /// Ends the transport: shuts a socket down. A daemon on standard input
    /// and output has nothing to shut; it ends when its input does.
    shutdown: Box<dyn Fn() + Send + Sync>,
    last_inbound: Mutex<Instant>,
    closed: AtomicBool,
}

impl Connection {
    fn close(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            (self.shutdown)();
        }
    }
}

/// What a daemon that is itself a cell holds: the connections its processes
/// open, carried to its parent inside the link, and the proxy they use.
struct CellRole {
    tunnels: Arc<crate::tunnel::Tunnels>,
    _proxy: Option<sandbox::proxy::Proxy>,
}

struct Client {
    epoch: String,
    policy: Policy,
    notifier: Arc<Notifier>,
    /// Held across a spawn's check, start and insertion, so a retransmitted
    /// spawn arriving on a new connection while the first copy is still
    /// starting on the old one finds the session instead of starting another.
    spawning: Arc<Mutex<()>>,
    connection: Option<Arc<Connection>>,
    base_env: Vec<(String, String)>,
    disconnected_at: Option<Instant>,
    /// Encoded frames for the current connection, sent ahead of output.
    outbox: VecDeque<Vec<u8>>,
}

#[derive(Default)]
struct State {
    clients: HashMap<String, Client>,
    /// Keyed by client id and session id; the epoch is on the session.
    sessions: BTreeMap<(String, String), Arc<Session>>,
    idle_since: Option<Instant>,
}

pub struct Daemon {
    /// `None` for a daemon on standard input and output, which has no socket,
    /// lock or log of its own.
    paths: Option<Paths>,
    home: std::path::PathBuf,
    tag: String,
    token: String,
    info: AgentInfo,
    options: DaemonOptions,
    state: Mutex<State>,
    next_connection: AtomicU64,
    stopping: AtomicBool,
    /// The sandboxed cells this daemon runs sessions in.
    cells: cells::Cells,
    /// Set when this daemon is a cell.
    cell: OnceLock<CellRole>,
}

/// Runs the daemon until it is idle long enough, asked to stop, or finds
/// another daemon of the same build already running (then it exits at once:
/// the proxy that started it will find the other one).
pub fn run_daemon(options: DaemonOptions) -> Result<(), String> {
    let digest = platform::self_digest()?;
    let paths = Paths::discover_at(&platform::build_tag(&digest), options.root.clone())?;
    trim_log(&paths);
    if options.own_log {
        platform::redirect_output_to_log(&paths)
            .map_err(|error| format!("Cannot open the agent log: {error}"))?;
    }
    // A proxy on Windows takes this lock for an instant to see whether a
    // daemon holds it; a moment's patience keeps that from reading as
    // another daemon.
    let Some(_lifetime) = platform::FileLock::acquire(&paths.lifetime_lock(), Duration::from_secs(1))
        .map_err(|error| format!("Cannot take the daemon lock: {error}"))?
    else {
        log("another daemon of this build is already running");
        return Ok(());
    };
    sweep_retired_builds(&paths);
    // The log, which is the daemon's standard output and error, is not every
    // session's business.
    #[cfg(windows)]
    platform::keep_standard_handles_private();
    platform::install_daemon_signal_handlers();
    platform::become_subreaper();
    let token = platform::random_hex(32);
    let listener = platform::LocalListener::bind(&paths, &token)
        .map_err(|error| format!("Cannot listen for proxies: {error}"))?;
    let machine = cells::machine(paths.home.clone(), Some(paths.root.clone()), Some(paths.run_dir.clone()));
    let daemon = Arc::new(Daemon {
        info: agent_info(digest, &paths.home),
        home: paths.home.clone(),
        tag: paths.tag.clone(),
        paths: Some(paths),
        token,
        options,
        state: Mutex::new(State::default()),
        next_connection: AtomicU64::new(1),
        stopping: AtomicBool::new(false),
        cells: cells::Cells::new(machine),
        cell: OnceLock::new(),
    });
    log(&format!("daemon {} started (pid {})", daemon.tag, daemon.info.pid));
    let listener = Arc::new(listener);
    {
        let daemon = Arc::clone(&daemon);
        let listener = Arc::clone(&listener);
        std::thread::Builder::new()
            .name("janitor".into())
            .spawn(move || daemon.janitor(Some(&listener)))
            .map_err(|error| format!("Cannot start the janitor: {error}"))?;
    }
    loop {
        match listener.accept() {
            Ok(stream) => {
                if daemon.stopping.load(Ordering::SeqCst) {
                    continue;
                }
                let daemon = Arc::clone(&daemon);
                let _ = std::thread::Builder::new()
                    .name("connection".into())
                    .spawn(move || daemon.serve(stream));
            }
            Err(error) => {
                log(&format!("accept failed: {error}"));
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

/// What a daemon says about itself and its machine.
fn agent_info(digest: String, home: &std::path::Path) -> AgentInfo {
    AgentInfo {
        version: crate::AGENT_VERSION.to_owned(),
        build: digest,
        source: Some(crate::marked_source_id().to_owned()),
        pid: std::process::id(),
        os: std::env::consts::OS.to_owned(),
        arch: std::env::consts::ARCH.to_owned(),
        home: home.to_string_lossy().into_owned(),
        // Windows' sshd sets `SHELL` to its configured shell too; without
        // it, `ComSpec` is what a Windows program would start.
        shell: std::env::var("SHELL")
            .ok()
            .filter(|shell| !shell.is_empty())
            .or_else(|| cfg!(windows).then(|| std::env::var("ComSpec").ok()).flatten()),
        started_unix_ms: unix_ms(),
        sandbox: sandbox::support(),
    }
}

/// Runs a daemon whose one connection is its own standard input and output:
/// the host's agent on its own machine, or — with `cell` — a sandboxed cell
/// started by another daemon. Prints the sync line for `sync` once it is
/// ready, and returns when its input ends, after ending every session.
pub fn run_stdio(options: DaemonOptions, cell: Option<sandbox::CellConfig>, sync: Option<String>) -> Result<(), String> {
    let digest = platform::self_digest()?;
    let home = paths::home_dir().ok_or("The account on this machine has no home directory")?;
    // The cell's proxy has to exist before the filter makes sockets harder
    // to come by, and the filter before anything else runs.
    let cell_listener = match &cell {
        Some(config) => match config.proxy_port {
            Some(port) => Some(
                std::net::TcpListener::bind(("127.0.0.1", port))
                    .map_err(|error| format!("Cannot listen for the sandbox's proxy: {error}"))?,
            ),
            None => None,
        },
        None => None,
    };
    if let Some(config) = &cell {
        if config.seccomp {
            #[cfg(target_os = "linux")]
            {
                sandbox::seccomp::install()?;
                // Nothing the cell starts may read its memory or trace it.
                unsafe {
                    libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
                }
            }
            #[cfg(not(target_os = "linux"))]
            return Err("This machine has no seccomp".into());
        }
    }
    #[cfg(unix)]
    platform::install_daemon_signal_handlers();
    platform::become_subreaper();
    let machine = cells::machine(home.clone(), None, None);
    let daemon = Arc::new(Daemon {
        info: agent_info(digest.clone(), &home),
        tag: platform::build_tag(&digest),
        home,
        paths: None,
        token: String::new(),
        options,
        state: Mutex::new(State::default()),
        next_connection: AtomicU64::new(1),
        stopping: AtomicBool::new(false),
        cells: cells::Cells::new(machine),
        cell: OnceLock::new(),
    });
    if let Some(config) = cell {
        let weak = Arc::downgrade(&daemon);
        let tunnels = crate::tunnel::Tunnels::new(Arc::new(move |message: &Message, body: &[u8]| {
            weak.upgrade().is_some_and(|daemon| daemon.queue_to_parent(message, body))
        }));
        let proxy = match cell_listener {
            Some(listener) => Some(
                sandbox::proxy::Proxy::start(
                    listener,
                    Some(config.proxy_token.clone()),
                    Arc::new(CellDial(Arc::clone(&tunnels))),
                )
                .map_err(|error| format!("Cannot start the sandbox's proxy: {error}"))?,
            ),
            None => None,
        };
        let _ = daemon.cell.set(CellRole { tunnels, _proxy: proxy });
    }
    {
        let daemon = Arc::clone(&daemon);
        std::thread::Builder::new()
            .name("janitor".into())
            .spawn(move || daemon.janitor(None))
            .map_err(|error| format!("Cannot start the janitor: {error}"))?;
    }
    let stdout = std::io::stdout();
    if let Some(nonce) = sync {
        use std::io::Write as _;
        let mut out = stdout.lock();
        out.write_all(protocol::sync_line(&nonce).as_bytes())
            .and_then(|_| out.flush())
            .map_err(|error| format!("Cannot write to standard output: {error}"))?;
    }
    let mut reader: Box<dyn Read + Send> = Box::new(std::io::stdin());
    let hello = match protocol::read_frame(&mut reader) {
        Ok(Some(protocol::Frame {
            message: Message::Hello(hello),
            ..
        })) => hello,
        _ => return Ok(()),
    };
    let writer: Box<dyn std::io::Write + Send> = Box::new(stdout);
    if let Some(client) = Arc::clone(&daemon).serve_connection(hello, Vec::new(), reader, writer, Box::new(|| {})) {
        daemon.release_client(&client, ExitReason::Shutdown);
    }
    daemon.stopping.store(true, Ordering::SeqCst);
    daemon.cells.shutdown();
    daemon.finish_sessions();
    Ok(())
}

/// A cell's proxy dials through the link, which the parent answers by its
/// policy.
struct CellDial(Arc<crate::tunnel::Tunnels>);

impl sandbox::proxy::Dial for CellDial {
    fn dial(&self, host: &str, port: u16) -> Result<crate::tunnel::Upstream, sandbox::proxy::DialError> {
        self.0
            .dial(host, port)
            .map_err(|(refused, message)| sandbox::proxy::DialError { refused, message })
    }
}

impl Daemon {
    /// One proxy's connection, from its preface to its end.
    fn serve(self: Arc<Self>, stream: LocalStream) {
        let _ = stream.set_read_timeout(Some(PREFACE_TIMEOUT));
        let mut reader = match stream.try_clone() {
            Ok(reader) => reader,
            Err(_) => return,
        };
        let local = match protocol::read_frame_as::<_, LocalHello>(&mut reader) {
            Ok(Some((local, _))) => local,
            _ => return,
        };
        if !constant_time_eq(local.token.as_bytes(), self.token.as_bytes()) {
            log("refused a proxy with the wrong secret");
            return;
        }
        let hello = match protocol::read_frame(&mut reader) {
            Ok(Some(protocol::Frame {
                message: Message::Hello(hello),
                ..
            })) => hello,
            _ => return,
        };
        let _ = stream.set_read_timeout(None);
        let Ok(writer) = stream.try_clone() else {
            return;
        };
        self.serve_connection(
            hello,
            local.env,
            Box::new(reader),
            Box::new(writer),
            Box::new(move || {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }),
        );
    }

    /// Serves one connection whose hello has been read, until it ends.
    /// Returns its client, if it got as far as having one.
    fn serve_connection(
        self: Arc<Self>,
        hello: Hello,
        env: Vec<(String, String)>,
        mut reader: Box<dyn Read + Send>,
        mut writer: Box<dyn std::io::Write + Send>,
        shutdown: Box<dyn Fn() + Send + Sync>,
    ) -> Option<String> {
        if hello.protocol != PROTOCOL_VERSION {
            let _ = protocol::write_frame(
                &mut writer,
                &Message::Refused {
                    reason: format!(
                        "The agent speaks protocol {PROTOCOL_VERSION}, the host {}",
                        hello.protocol
                    ),
                },
                &[],
            );
            return None;
        }
        if hello.client.is_empty() || hello.client.len() > 128 || hello.epoch.len() > 128 {
            return None;
        }
        let connection = Arc::new(Connection {
            id: self.next_connection.fetch_add(1, Ordering::SeqCst),
            shutdown,
            last_inbound: Mutex::new(Instant::now()),
            closed: AtomicBool::new(false),
        });
        let client_id = hello.client.clone();
        let notifier = self.introduce(&connection, hello, env);
        {
            let daemon = Arc::clone(&self);
            let writer_connection = Arc::clone(&connection);
            let client_id = client_id.clone();
            let notifier = Arc::clone(&notifier);
            let spawned = std::thread::Builder::new()
                .name("writer".into())
                .spawn(move || daemon.write_loop(&client_id, &writer_connection, &notifier, writer));
            if spawned.is_err() {
                connection.close();
            }
        }
        self.read_loop(&client_id, &connection, &mut reader);
        connection.close();
        {
            let mut state = lock(&self.state);
            if let Some(client) = state.clients.get_mut(&client_id) {
                if client
                    .connection
                    .as_ref()
                    .is_some_and(|current| current.id == connection.id)
                {
                    client.connection = None;
                    client.disconnected_at = Some(Instant::now());
                    client.outbox.clear();
                }
            }
        }
        notifier.notify();
        Some(client_id)
    }

    /// Registers a connection as its client's current one and queues the
    /// welcome. A new epoch reclaims every session of the previous one: that
    /// host process is gone, and nothing can reach them any more.
    fn introduce(
        &self,
        connection: &Arc<Connection>,
        hello: Hello,
        base_env: Vec<(String, String)>,
    ) -> Arc<Notifier> {
        let policy = clamp_policy(hello.policy);
        let mut stale = Vec::new();
        let (notifier, replaced) = {
            let mut state = lock(&self.state);
            let client = state
                .clients
                .entry(hello.client.clone())
                .or_insert_with(|| Client {
                    epoch: hello.epoch.clone(),
                    policy,
                    notifier: Arc::new(Notifier::default()),
                    spawning: Arc::new(Mutex::new(())),
                    connection: None,
                    base_env: Vec::new(),
                    disconnected_at: None,
                    outbox: VecDeque::new(),
                });
            let epoch_changed = client.epoch != hello.epoch;
            client.epoch = hello.epoch.clone();
            client.policy = policy;
            client.base_env = base_env;
            client.disconnected_at = None;
            client.outbox.clear();
            let replaced = client.connection.replace(Arc::clone(connection));
            let notifier = Arc::clone(&client.notifier);
            if epoch_changed {
                let keys: Vec<_> = state
                    .sessions
                    .iter()
                    .filter(|((client, _), session)| {
                        *client == hello.client && session.owner.epoch != hello.epoch
                    })
                    .map(|(key, _)| key.clone())
                    .collect();
                for key in keys {
                    if let Some(session) = state.sessions.remove(&key) {
                        stale.push(session);
                    }
                }
            }
            let resume: HashMap<&str, &protocol::ResumePoint> = hello
                .resume
                .iter()
                .map(|point| (point.sid.as_str(), point))
                .collect();
            let mut sessions = Vec::new();
            for ((client, sid), session) in &state.sessions {
                if *client != hello.client {
                    continue;
                }
                {
                    let mut session_state = lock(&session.state);
                    let (stdout, stderr) = match resume.get(sid.as_str()) {
                        Some(point) => (point.stdout, point.stderr),
                        // A session the host does not mention is one it lost
                        // track of; it gets whatever is still held.
                        None => (session_state.stdout.start(), session_state.stderr.start()),
                    };
                    session_state.cursors = session::Cursors {
                        connection: connection.id,
                        stdout,
                        stderr,
                        exit_sent: false,
                    };
                }
                sessions.push(session.info());
            }
            let welcome = Message::Welcome(Welcome {
                protocol: PROTOCOL_VERSION,
                agent: self.info.clone(),
                sessions,
            });
            if let Ok(frame) = protocol::encode_frame(&welcome, &[]) {
                let client = state
                    .clients
                    .get_mut(&hello.client)
                    .expect("the client was inserted above");
                client.outbox.push_front(frame);
            }
            state.idle_since = None;
            (notifier, replaced)
        };
        if let Some(replaced) = replaced {
            replaced.close();
        }
        for session in stale {
            log(&format!("reclaiming {} from a previous host process", session.sid));
            session.terminate(ExitReason::Reclaimed);
        }
        notifier.notify();
        notifier
    }

    fn read_loop(&self, client_id: &str, connection: &Arc<Connection>, reader: &mut Box<dyn Read + Send>) {
        loop {
            let frame = match protocol::read_frame(reader) {
                Ok(Some(frame)) => frame,
                Ok(None) | Err(_) => return,
            };
            *lock(&connection.last_inbound) = Instant::now();
            match frame.message {
                Message::Ping { seq } => self.queue(client_id, &Message::Pong { seq }, &[]),
                Message::Request { id, op } => {
                    let outcome = match self.handle(client_id, op, frame.body) {
                        Ok(reply) => Outcome::Ok { reply },
                        Err(failure) => Outcome::Err { failure },
                    };
                    self.queue(client_id, &Message::Response { id, outcome }, &[]);
                }
                Message::Input { sid, offset } => {
                    if let Some(session) = self.session(client_id, &sid) {
                        let _ = session.write_input(offset, &frame.body);
                    }
                }
                Message::Bye { release } => {
                    if release {
                        self.release_client(client_id, ExitReason::Signalled);
                    }
                    return;
                }
                // A cell's parent answering the connections its processes
                // asked for.
                message @ (Message::Dialed { .. }
                | Message::Tunnel { .. }
                | Message::TunnelAck { .. }
                | Message::TunnelEnd { .. }
                | Message::TunnelClose { .. })
                    if self.cell.get().is_some() =>
                {
                    if let Some(cell) = self.cell.get() {
                        cell.tunnels.dispatch(&message, &frame.body);
                    }
                }
                // Anything else is not the host's to send.
                _ => return,
            }
        }
    }

    fn handle(&self, client_id: &str, op: Op, body: Vec<u8>) -> Result<Reply, Failure> {
        match op {
            Op::Spawn(spec) => {
                let spawning = lock(&self.state)
                    .clients
                    .get(client_id)
                    .map(|client| Arc::clone(&client.spawning))
                    .ok_or_else(|| Failure::new(FailureKind::Invalid, "Unknown client"))?;
                let _one_at_a_time = lock(&spawning);
                if let Some(existing) = self.session(client_id, &spec.sid) {
                    // A retransmitted spawn: the first copy already started it.
                    return Ok(Reply::Spawned { pid: existing.pid });
                }
                let (owner, notifier, base_env, orphan_ttl, count) = {
                    let state = lock(&self.state);
                    let client = state
                        .clients
                        .get(client_id)
                        .ok_or_else(|| Failure::new(FailureKind::Invalid, "Unknown client"))?;
                    let count = state
                        .sessions
                        .keys()
                        .filter(|(client, _)| client == client_id)
                        .count();
                    (
                        Owner {
                            client: client_id.to_owned(),
                            epoch: client.epoch.clone(),
                        },
                        Arc::clone(&client.notifier),
                        client.base_env.clone(),
                        Duration::from_secs(client.policy.orphan_ttl_secs),
                        count,
                    )
                };
                if count >= self.options.max_sessions_per_client {
                    return Err(Failure::new(
                        FailureKind::Limit,
                        format!(
                            "This host already has {count} sessions on the machine; release some first"
                        ),
                    ));
                }
                if self.stopping.load(Ordering::SeqCst) {
                    return Err(Failure::new(FailureKind::Unsupported, "The agent is shutting down"));
                }
                let context = SpawnContext {
                    owner,
                    notifier: Arc::clone(&notifier),
                    base_env: &base_env,
                    home: &self.home,
                    orphan_ttl: orphan_ttl.min(MAX_ORPHAN_TTL),
                };
                let session = match &spec.sandbox {
                    Some(_) if self.cell.get().is_some() => {
                        return Err(Failure::new(
                            FailureKind::Invalid,
                            "A sandboxed session cannot start another sandbox",
                        ))
                    }
                    Some(sandbox) => {
                        let env = session::session_env(&base_env, &[], &BTreeMap::new());
                        let process = self.cells.spawn(client_id, &spec, sandbox, &body, &env)?;
                        session::spawn_relayed(&spec, process, context)?
                    }
                    None => session::spawn(&spec, body, context)?,
                };
                let pid = session.pid;
                {
                    // Stamped with whichever connection is current *now*: a
                    // reconnect that happened while this spawn was starting
                    // reset every other session's cursors, and this one must
                    // stream to the same connection they do.
                    let mut state = lock(&self.state);
                    let current = state
                        .clients
                        .get(client_id)
                        .and_then(|client| client.connection.as_ref().map(|connection| connection.id))
                        .unwrap_or(0);
                    lock(&session.state).cursors.connection = current;
                    state
                        .sessions
                        .insert((client_id.to_owned(), spec.sid.clone()), session);
                }
                notifier.notify();
                Ok(Reply::Spawned { pid })
            }
            Op::Signal { sid, signal } => {
                if let Some(session) = self.session(client_id, &sid) {
                    session.signal(signal, ExitReason::Signalled);
                }
                Ok(Reply::Done)
            }
            Op::CloseStdin { sid } => {
                if let Some(session) = self.session(client_id, &sid) {
                    session.close_input();
                }
                Ok(Reply::Done)
            }
            Op::Resize { sid, size } => match self.session(client_id, &sid) {
                Some(session) => session.resize(size).map(|()| Reply::Done),
                None => Err(Failure::new(FailureKind::NotFound, "No such session")),
            },
            Op::Release { sid } => {
                let removed = lock(&self.state)
                    .sessions
                    .remove(&(client_id.to_owned(), sid));
                if let Some(session) = removed {
                    if session.is_running() {
                        session.terminate(ExitReason::Signalled);
                    }
                }
                Ok(Reply::Done)
            }
            Op::Sessions => Ok(Reply::Sessions {
                sessions: self.client_sessions(client_id).iter().map(|s| s.info()).collect(),
            }),
            Op::Which { names } => {
                let base_env = lock(&self.state)
                    .clients
                    .get(client_id)
                    .map(|client| client.base_env.clone())
                    .unwrap_or_default();
                let env = session::session_env(&base_env, &[], &BTreeMap::new());
                let found = names
                    .into_iter()
                    .take(64)
                    .map(|name| {
                        let path = session::resolve_program(&name, &env, &self.home, &self.home)
                            .map(|path| path.to_string_lossy().into_owned());
                        (name, path)
                    })
                    .collect();
                Ok(Reply::Which { found })
            }
        }
    }

    /// Sends everything owed to one connection: queued frames first, then each
    /// session's output a chunk at a time, round-robin, then exits whose output
    /// is complete. Never holds a lock while writing.
    fn write_loop(
        &self,
        client_id: &str,
        connection: &Arc<Connection>,
        notifier: &Notifier,
        mut writer: Box<dyn std::io::Write + Send>,
    ) {
        loop {
            if connection.closed.load(Ordering::SeqCst) {
                return;
            }
            let mut wrote = false;
            let queued: Vec<Vec<u8>> = {
                let mut state = lock(&self.state);
                match state.clients.get_mut(client_id) {
                    Some(client)
                        if client
                            .connection
                            .as_ref()
                            .is_some_and(|current| current.id == connection.id) =>
                    {
                        client.outbox.drain(..).collect()
                    }
                    _ => return,
                }
            };
            for frame in queued {
                if writer.write_all(&frame).is_err() {
                    connection.close();
                    return;
                }
                wrote = true;
            }
            for session in self.client_sessions(client_id) {
                for frame in next_frames(&session, connection.id) {
                    if writer.write_all(&frame).is_err() {
                        connection.close();
                        return;
                    }
                    wrote = true;
                }
            }
            if wrote {
                let _ = writer.flush();
                continue;
            }
            notifier.wait(Duration::from_millis(500));
        }
    }

    /// Queues a frame for a cell's one connection, its parent. `false` when
    /// there is none.
    fn queue_to_parent(&self, message: &Message, body: &[u8]) -> bool {
        let client = lock(&self.state)
            .clients
            .iter()
            .find(|(_, client)| client.connection.is_some())
            .map(|(id, _)| id.clone());
        match client {
            Some(client) => {
                self.queue(&client, message, body);
                true
            }
            None => false,
        }
    }

    fn queue(&self, client_id: &str, message: &Message, body: &[u8]) {
        let Ok(frame) = protocol::encode_frame(message, body) else {
            return;
        };
        let notifier = {
            let mut state = lock(&self.state);
            let Some(client) = state.clients.get_mut(client_id) else {
                return;
            };
            client.outbox.push_back(frame);
            Arc::clone(&client.notifier)
        };
        notifier.notify();
    }

    fn session(&self, client_id: &str, sid: &str) -> Option<Arc<Session>> {
        lock(&self.state)
            .sessions
            .get(&(client_id.to_owned(), sid.to_owned()))
            .cloned()
    }

    fn client_sessions(&self, client_id: &str) -> Vec<Arc<Session>> {
        lock(&self.state)
            .sessions
            .iter()
            .filter(|((client, _), _)| client == client_id)
            .map(|(_, session)| Arc::clone(session))
            .collect()
    }

    fn release_client(&self, client_id: &str, reason: ExitReason) {
        let sessions: Vec<Arc<Session>> = {
            let mut state = lock(&self.state);
            let keys: Vec<_> = state
                .sessions
                .keys()
                .filter(|(client, _)| client == client_id)
                .cloned()
                .collect();
            keys.into_iter()
                .filter_map(|key| state.sessions.remove(&key))
                .collect()
        };
        for session in sessions {
            if session.is_running() {
                session.terminate(reason);
            }
        }
    }

    /// Enforces every lifetime the daemon promises: silent connections are
    /// closed, orphaned sessions reclaimed, finished ones forgotten, and the
    /// daemon itself leaves once it has had nothing to do for long enough.
    fn janitor(&self, listener: Option<&platform::LocalListener>) {
        loop {
            std::thread::sleep(self.options.tick);
            if platform::stop_requested() {
                self.shutdown(listener, "asked to stop");
            }
            self.cells.sweep();
            let now = Instant::now();
            let mut silent = Vec::new();
            let mut reclaim = Vec::new();
            let idle_for = {
                let mut state = lock(&self.state);
                let mut forget = Vec::new();
                // A pipe cannot go half-open the way a network can: a daemon on
                // standard input and output hears of its parent's end as
                // the end of its input.
                for client in state.clients.values().filter(|_| self.paths.is_some()) {
                    if let Some(connection) = &client.connection {
                        let quiet = now.duration_since(*lock(&connection.last_inbound));
                        if quiet >= Duration::from_secs(client.policy.silence_timeout_secs) {
                            silent.push(Arc::clone(connection));
                        }
                    }
                }
                for (key, session) in &state.sessions {
                    let client = state.clients.get(&key.0);
                    let current_epoch = client.is_some_and(|client| client.epoch == session.owner.epoch);
                    let connected = current_epoch && client.is_some_and(|client| client.connection.is_some());
                    let away = client
                        .and_then(|client| client.disconnected_at)
                        .map(|since| now.duration_since(since))
                        .unwrap_or_else(|| session.started.elapsed());
                    let finished_for = lock(&session.state)
                        .exited_at
                        .map(|at| now.duration_since(at));
                    match finished_for {
                        None if !current_epoch || (!connected && away >= session.orphan_ttl) => {
                            if !session.reclaiming.load(Ordering::SeqCst) {
                                reclaim.push(Arc::clone(session));
                            }
                        }
                        Some(finished) => {
                            let linger = if connected {
                                self.options.finished_linger
                            } else {
                                client
                                    .map(|client| Duration::from_secs(client.policy.finished_ttl_secs))
                                    .unwrap_or_default()
                            };
                            if finished >= linger && (connected || away >= linger) {
                                forget.push(key.clone());
                            }
                        }
                        None => {}
                    }
                }
                for key in forget {
                    state.sessions.remove(&key);
                }
                // A client with neither a connection nor a session has nothing
                // left for the daemon to remember.
                let live: std::collections::HashSet<String> =
                    state.sessions.keys().map(|(client, _)| client.clone()).collect();
                state.clients.retain(|id, client| {
                    client.connection.is_some()
                        || live.contains(id)
                        || client
                            .disconnected_at
                            .is_some_and(|since| now.duration_since(since) < Duration::from_secs(3600))
                });
                let busy = !state.sessions.is_empty()
                    || state.clients.values().any(|client| client.connection.is_some())
                    || self.cells.count() > 0;
                if busy {
                    state.idle_since = None;
                    None
                } else {
                    let since = *state.idle_since.get_or_insert(now);
                    Some(now.duration_since(since))
                }
            };
            for connection in silent {
                log("closing a connection that stopped answering");
                connection.close();
            }
            for session in reclaim {
                log(&format!("reclaiming orphaned session {}", session.sid));
                session.reclaiming.store(true, Ordering::SeqCst);
                session.terminate(ExitReason::Reclaimed);
            }
            platform::reap_orphans();
            // A daemon on standard input and output leaves when its input
            // ends, not when it is idle: its parent decides.
            if self.paths.is_some() && idle_for.is_some_and(|idle| idle >= self.options.idle_exit) {
                self.shutdown(listener, "idle");
            }
        }
    }

    /// Ends every session, removes the socket and leaves.
    fn shutdown(&self, listener: Option<&platform::LocalListener>, why: &str) -> ! {
        self.stopping.store(true, Ordering::SeqCst);
        if let Some(listener) = listener {
            listener.remove_files();
        }
        self.cells.shutdown();
        self.finish_sessions();
        log(&format!("daemon {} exiting: {why}", self.tag));
        std::process::exit(0)
    }

    /// Ends every session's processes: politely, then not.
    fn finish_sessions(&self) {
        let sessions: Vec<Arc<Session>> = lock(&self.state).sessions.values().cloned().collect();
        for session in &sessions {
            if session.is_running() {
                session.terminate(ExitReason::Shutdown);
            }
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && sessions.iter().any(|session| session.is_running()) {
            std::thread::sleep(Duration::from_millis(50));
        }
        for session in &sessions {
            if session.is_running() {
                session.signal(protocol::SignalKind::Kill, ExitReason::Shutdown);
            }
        }
    }
}

/// The next frames one session owes its owner's connection: at most one
/// output chunk per stream, a gap notice where the ring lost bytes, and the
/// exit once everything before it is out.
fn next_frames(session: &Session, connection: u64) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    let mut state = lock(&session.state);
    if state.cursors.connection != connection {
        return frames;
    }
    for stream in [Stream::Stdout, Stream::Stderr] {
        let cursor = match stream {
            Stream::Stdout => state.cursors.stdout,
            Stream::Stderr => state.cursors.stderr,
        };
        let ring = state.ring(stream);
        if cursor >= ring.end() {
            continue;
        }
        let (from, bytes) = ring.read_from(cursor, OUTPUT_CHUNK_BYTES);
        if from > cursor {
            if let Ok(frame) = protocol::encode_frame(
                &Message::Gap {
                    sid: session.sid.clone(),
                    stream,
                    from: cursor,
                    to: from,
                },
                &[],
            ) {
                frames.push(frame);
            }
        }
        let next = from + bytes.len() as u64;
        if !bytes.is_empty() {
            if let Ok(frame) = protocol::encode_frame(
                &Message::Output {
                    sid: session.sid.clone(),
                    stream,
                    offset: from,
                },
                &bytes,
            ) {
                frames.push(frame);
            }
        }
        match stream {
            Stream::Stdout => state.cursors.stdout = next,
            Stream::Stderr => state.cursors.stderr = next,
        }
    }
    if let Some(exit) = state.exit.clone() {
        let caught_up =
            state.cursors.stdout >= exit.ends.stdout && state.cursors.stderr >= exit.ends.stderr;
        if caught_up && !state.cursors.exit_sent {
            state.cursors.exit_sent = true;
            if let Ok(frame) = protocol::encode_frame(
                &Message::Exit {
                    sid: session.sid.clone(),
                    exit,
                },
                &[],
            ) {
                frames.push(frame);
            }
        }
    }
    frames
}

fn clamp_policy(policy: Policy) -> Policy {
    Policy {
        orphan_ttl_secs: policy.orphan_ttl_secs.min(MAX_ORPHAN_TTL.as_secs()),
        silence_timeout_secs: policy
            .silence_timeout_secs
            .clamp(MIN_SILENCE.as_secs(), MAX_SILENCE.as_secs()),
        finished_ttl_secs: policy.finished_ttl_secs.min(MAX_ORPHAN_TTL.as_secs()),
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// Removes the lock files other builds' daemons left in the runtime directory,
/// once nothing holds them: an exited daemon releases its lock but cannot
/// safely delete the file it locked while it still held it.
fn sweep_retired_builds(paths: &Paths) {
    let Ok(entries) = std::fs::read_dir(&paths.run_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(tag) = name.strip_suffix(".lock") else {
            continue;
        };
        if tag == paths.tag {
            continue;
        }
        if let Ok(Some(lock)) = platform::FileLock::try_acquire(&path) {
            // A daemon that was killed never removed its endpoint either.
            for leftover in ["start", "sock", "token", "json"] {
                let _ = std::fs::remove_file(paths.run_dir.join(format!("{tag}.{leftover}")));
            }
            // Unix unlinks the lock while holding it, so no daemon of that
            // build can take it in between; Windows refuses to delete a file
            // that is still open, and takes the small window instead.
            #[cfg(unix)]
            let _ = std::fs::remove_file(&path);
            drop(lock);
            #[cfg(windows)]
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Keeps the shared log from growing without bound: past 1 MiB it starts over.
fn trim_log(paths: &Paths) {
    let path = paths.log_file();
    if std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() > 1 << 20) {
        if let Ok(file) = std::fs::OpenOptions::new().write(true).open(&path) {
            let _ = file.set_len(0);
        }
    }
}

/// One line to the daemon's log, which is its standard error.
pub fn log(message: &str) {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    eprintln!("[{seconds}] [{}] {message}", std::process::id());
}
