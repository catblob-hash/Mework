//! One process the daemon runs for a host, from spawn to reclamation.
//!
//! A session owns its whole process tree ([`ProcessTree`]), buffers its output
//! in rings addressed by offset, and publishes its end only once that output is
//! complete: the process has exited *and* its streams reached end of file. A
//! host that reads the exit therefore knows it has, or can still ask for,
//! every byte the process wrote.
//!
//! Nothing here blocks on the host. Output is pushed into the rings whether or
//! not anyone is connected, input is queued to a writer thread of the session's
//! own, and a link that stalls only means the rings fill and drop their oldest
//! bytes — the process itself never waits for the network.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::paths::native_path;
use super::platform::{self, ProcessTree};
use crate::protocol::{
    ExitInfo, ExitReason, Failure, FailureKind, SessionInfo, SignalKind, SpawnSpec, StdinMode,
    Stream, StreamEnds, TerminalSize,
};
use crate::ring::Ring;

/// Output kept per stream for a process with pipes, unless the host asks for
/// another amount. A connected host drains it continuously; the ring only
/// matters while the link is down.
pub const DEFAULT_PIPE_OUTPUT: usize = 4 << 20;
/// Output kept for a terminal: the host keeps the same 2 MiB for replay.
pub const DEFAULT_TERMINAL_OUTPUT: usize = 2 << 20;
/// Largest ring a host may ask for.
pub const MAX_OUTPUT_LIMIT: u64 = 64 << 20;

/// How long a terminal's output may keep draining after its shell exited.
/// Background jobs can hold the terminal open indefinitely; what they print
/// after the shell is gone has nobody to read it.
const TERMINAL_DRAIN: Duration = Duration::from_millis(500);
/// How long a hung-up terminal gets before it is killed outright.
const HANGUP_GRACE: Duration = Duration::from_secs(2);

/// Names the daemon uses for itself; a session never inherits them.
const PRIVATE_ENV: &[&str] = &["MEWORK_REMOTE_ROOT", "MEWORK_REMOTE_LOG"];

/// Wakes a connection's writer when something it should send appeared.
#[derive(Default)]
pub struct Notifier {
    dirty: Mutex<bool>,
    cond: Condvar,
}

impl Notifier {
    pub fn notify(&self) {
        *lock(&self.dirty) = true;
        self.cond.notify_all();
    }

    /// Waits until notified or `timeout`, then clears the flag. Cleared
    /// before the caller looks at the state, so anything that arrives while it
    /// looks sets the flag again and the next wait returns at once.
    pub fn wait(&self, timeout: Duration) {
        let mut dirty = lock(&self.dirty);
        if !*dirty {
            dirty = self
                .cond
                .wait_timeout(dirty, timeout)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
        *dirty = false;
    }
}

/// Which host a session belongs to: the installation and the process of it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Owner {
    pub client: String,
    pub epoch: String,
}

/// What the owner's current connection has been sent of this session.
#[derive(Clone, Copy, Debug, Default)]
pub struct Cursors {
    /// The connection these positions belong to. A writer for any other
    /// connection — one that was replaced a moment ago and has not noticed
    /// yet — must not move them, or the new connection would never be sent
    /// what the old one swallowed.
    pub connection: u64,
    pub stdout: u64,
    pub stderr: u64,
    pub exit_sent: bool,
}

pub struct SessionState {
    pub stdout: Ring,
    pub stderr: Ring,
    stdout_done: bool,
    stderr_done: bool,
    /// Set when the process was reaped; the exit is published once the
    /// streams are done too.
    reaped: Option<(Option<i32>, Option<i32>)>,
    pub exit: Option<ExitInfo>,
    pub exited_at: Option<Instant>,
    /// Why the daemon itself ended the process, if it did.
    kill_reason: Option<ExitReason>,
    pub cursors: Cursors,
}

impl SessionState {
    fn new(capacity: usize, has_stderr: bool) -> Self {
        Self {
            stdout: Ring::new(capacity),
            stderr: Ring::new(if has_stderr { capacity } else { 1 }),
            stdout_done: false,
            stderr_done: !has_stderr,
            reaped: None,
            exit: None,
            exited_at: None,
            kill_reason: None,
            cursors: Cursors::default(),
        }
    }

    pub fn ring(&self, stream: Stream) -> &Ring {
        match stream {
            Stream::Stdout => &self.stdout,
            Stream::Stderr => &self.stderr,
        }
    }

    pub fn ends(&self) -> StreamEnds {
        StreamEnds {
            stdout: self.stdout.end(),
            stderr: self.stderr.end(),
        }
    }

    /// Publishes the exit once the process is reaped and both streams ended.
    fn settle(&mut self) -> bool {
        if self.exit.is_some() || !self.stdout_done || !self.stderr_done {
            return false;
        }
        let Some((code, signal)) = self.reaped else {
            return false;
        };
        self.exit = Some(ExitInfo {
            code,
            signal,
            reason: self.kill_reason.unwrap_or(ExitReason::Exited),
            ends: self.ends(),
        });
        self.exited_at = Some(Instant::now());
        true
    }
}

enum InputCommand {
    Data(Vec<u8>),
    Close,
}

/// What a session's signals reach.
enum Process {
    /// A child of this daemon's, and everything it started.
    Tree(ProcessTree),
    /// A process in a sandboxed cell (see [`super::cells`]); the cell
    /// signals its tree.
    Relayed(Arc<crate::client::RemoteProcess>),
}

impl Process {
    fn signal(&self, signal: SignalKind) {
        match self {
            Self::Tree(tree) => {
                tree.signal(signal);
            }
            Self::Relayed(process) => process.signal(signal),
        }
    }
}

struct InputPort {
    /// Bytes of input that have arrived, whether or not written yet.
    received: u64,
    sender: Option<mpsc::Sender<InputCommand>>,
}

pub struct Session {
    pub sid: String,
    pub owner: Owner,
    pub pid: u32,
    pub terminal: bool,
    pub label: Option<String>,
    pub started: Instant,
    pub orphan_ttl: Duration,
    pub state: Mutex<SessionState>,
    process: Process,
    input: Mutex<InputPort>,
    /// The pseudo terminal's master, kept for resizing; dropped when the
    /// session ends.
    master: Mutex<Option<Box<dyn portable_pty::MasterPty + Send>>>,
    /// Tells a terminal's reader to stop even though the terminal is still
    /// open, once its shell has exited.
    closing: AtomicBool,
    /// Set by the janitor when it starts reclaiming the session, so a
    /// session that takes a moment to die is not signalled on every tick.
    pub reclaiming: AtomicBool,
    notifier: Arc<Notifier>,
}

/// What a spawn needs from the daemon besides the host's request.
pub struct SpawnContext<'a> {
    pub owner: Owner,
    pub notifier: Arc<Notifier>,
    /// The environment of the owner's SSH login, reported by its proxy.
    pub base_env: &'a [(String, String)],
    pub home: &'a Path,
    pub orphan_ttl: Duration,
}

impl Session {
    pub fn info(&self) -> SessionInfo {
        let state = lock(&self.state);
        SessionInfo {
            sid: self.sid.clone(),
            pid: self.pid,
            terminal: self.terminal,
            exit: state.exit.clone(),
            ends: state.ends(),
            input_end: lock(&self.input).received,
            label: self.label.clone(),
            age_secs: self.started.elapsed().as_secs(),
        }
    }

    pub fn is_running(&self) -> bool {
        lock(&self.state).exit.is_none()
    }

    /// Accepts input that starts at `offset` in the session's input stream.
    /// A retransmitted copy of input already received is dropped here, which
    /// is what makes resending after a reconnect safe.
    pub fn write_input(&self, offset: u64, data: &[u8]) -> Result<(), Failure> {
        let mut input = lock(&self.input);
        let end = offset + data.len() as u64;
        if end <= input.received {
            return Ok(());
        }
        let fresh = if offset < input.received {
            &data[(input.received - offset) as usize..]
        } else {
            // Input between `received` and `offset` never arrived. It cannot
            // be recovered here; what did arrive is still better written.
            data
        };
        input.received = end;
        let Some(sender) = input.sender.as_ref() else {
            return Err(Failure::new(
                FailureKind::Invalid,
                "This session's input is closed",
            ));
        };
        let _ = sender.send(InputCommand::Data(fresh.to_vec()));
        Ok(())
    }

    /// Ends the input stream after what was queued before it.
    pub fn close_input(&self) {
        if let Some(sender) = lock(&self.input).sender.take() {
            let _ = sender.send(InputCommand::Close);
        }
    }

    pub fn resize(&self, size: TerminalSize) -> Result<(), Failure> {
        if let Process::Relayed(process) = &self.process {
            if !self.terminal {
                return Err(Failure::new(FailureKind::Invalid, "This session has no terminal"));
            }
            process.resize(size);
            return Ok(());
        }
        let master = lock(&self.master);
        let Some(master) = master.as_ref() else {
            return Err(Failure::new(
                FailureKind::Invalid,
                if self.terminal {
                    "This terminal has already ended"
                } else {
                    "This session has no terminal"
                },
            ));
        };
        master
            .resize(pty_size(size))
            .map_err(|error| Failure::new(FailureKind::Io, format!("Cannot resize: {error}")))
    }

    /// Signals the whole process tree. `reason` is what the exit will report
    /// if this is what ends it.
    pub fn signal(self: &Arc<Self>, signal: SignalKind, reason: ExitReason) {
        {
            let mut state = lock(&self.state);
            if state.exit.is_some() {
                return;
            }
            // A process already reaped ended by itself; the signal is only for
            // the stragglers still holding its pipes, and does not change why
            // it ended.
            if state.reaped.is_none() && state.kill_reason.is_none() {
                state.kill_reason = Some(reason);
            }
        }
        self.process.signal(signal);
        // A terminal's shell may ignore the hangup; it does not get to keep
        // the session alive by doing so.
        if matches!(signal, SignalKind::Hangup | SignalKind::Terminate) {
            let session = Arc::clone(self);
            std::thread::spawn(move || {
                let deadline = Instant::now() + HANGUP_GRACE;
                while Instant::now() < deadline {
                    if lock(&session.state).reaped.is_some() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                session.process.signal(SignalKind::Kill);
            });
        }
    }

    /// Ends the session's processes: a hangup for a terminal, as closing one
    /// would, and a kill for everything else.
    pub fn terminate(self: &Arc<Self>, reason: ExitReason) {
        let signal = if self.terminal {
            SignalKind::Hangup
        } else {
            SignalKind::Kill
        };
        self.signal(signal, reason);
        self.close_input();
    }

    fn record_output(&self, stream: Stream, data: &[u8]) {
        {
            let mut state = lock(&self.state);
            match stream {
                Stream::Stdout => state.stdout.push(data),
                Stream::Stderr => state.stderr.push(data),
            }
        }
        self.notifier.notify();
    }

    fn stream_done(&self, stream: Stream) {
        let settled = {
            let mut state = lock(&self.state);
            match stream {
                Stream::Stdout => state.stdout_done = true,
                Stream::Stderr => state.stderr_done = true,
            }
            state.settle()
        };
        if settled {
            self.notifier.notify();
        }
    }

    fn reaped(&self, code: Option<i32>, signal: Option<i32>) {
        let settled = {
            let mut state = lock(&self.state);
            state.reaped = Some((code, signal));
            state.settle()
        };
        if settled {
            self.notifier.notify();
        }
    }
}

/// Starts the process `spec` describes. `body` is its standard input when the
/// spec asks for [`StdinMode::Body`].
pub fn spawn(spec: &SpawnSpec, body: Vec<u8>, context: SpawnContext<'_>) -> Result<Arc<Session>, Failure> {
    if spec.sid.is_empty() || spec.sid.len() > 128 || spec.sid.chars().any(char::is_control) {
        return Err(Failure::new(FailureKind::Invalid, "Invalid session id"));
    }
    let Some(program) = spec.argv.first().filter(|program| !program.is_empty()) else {
        return Err(Failure::new(FailureKind::Invalid, "The command is empty"));
    };
    if spec.argv.iter().any(|argument| argument.contains('\0')) {
        return Err(Failure::new(FailureKind::Invalid, "An argument contains NUL"));
    }
    if spec.output_limit.is_some_and(|limit| limit > MAX_OUTPUT_LIMIT) {
        return Err(Failure::new(FailureKind::Limit, "The output limit is too large"));
    }
    if spec.terminal.is_some() && spec.stdin == StdinMode::Body {
        return Err(Failure::new(
            FailureKind::Invalid,
            "A terminal session takes its input as it runs",
        ));
    }

    let env = session_env(context.base_env, &spec.env_remove, &spec.env);
    let cwd = match spec.cwd.as_deref().filter(|cwd| !cwd.is_empty()) {
        Some(cwd) => native_path(cwd, context.home),
        None => context.home.to_path_buf(),
    };
    if !cwd.is_dir() {
        return Err(Failure::new(
            FailureKind::NotFound,
            format!("The working directory does not exist: {}", cwd.display()),
        ));
    }
    let resolved = if program == crate::protocol::SELF_PROGRAM {
        std::env::current_exe().map_err(|error| {
            Failure::new(FailureKind::Io, format!("The agent cannot find its own executable: {error}"))
        })?
    } else {
        resolve_program(program, &env, &cwd, context.home).ok_or_else(|| {
            Failure::new(
                FailureKind::NotFound,
                format!("{program} was not found on this machine's PATH"),
            )
        })?
    };
    let orphan_ttl = spec
        .orphan_ttl_secs
        .map(Duration::from_secs)
        .unwrap_or(context.orphan_ttl);
    let label = spec.label.clone();

    match spec.terminal {
        Some(size) => spawn_terminal(spec, &resolved, &cwd, &env, size, orphan_ttl, label, context),
        None => spawn_pipes(spec, body, &resolved, &cwd, &env, orphan_ttl, label, context),
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_pipes(
    spec: &SpawnSpec,
    body: Vec<u8>,
    program: &Path,
    cwd: &Path,
    env: &BTreeMap<String, String>,
    orphan_ttl: Duration,
    label: Option<String>,
    context: SpawnContext<'_>,
) -> Result<Arc<Session>, Failure> {
    let mut command = Command::new(program);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.arg0(&spec.argv[0]);
    }
    command
        .args(&spec.argv[1..])
        .current_dir(cwd)
        .env_clear()
        .envs(env)
        .stdin(if spec.stdin == StdinMode::Null {
            Stdio::null()
        } else {
            Stdio::piped()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    platform::prepare_child(&mut command);
    let mut child = command.spawn().map_err(|error| spawn_failure(program, error))?;
    let pid = child.id();
    #[cfg(unix)]
    let tree = ProcessTree::adopt(pid);
    #[cfg(windows)]
    let tree = {
        use std::os::windows::io::AsRawHandle;
        let tree = ProcessTree::adopt(child.as_raw_handle() as _);
        // Started suspended (see `prepare_child`); now that the job holds
        // it, nothing it starts can be outside the tree.
        if let Err(error) = platform::resume_suspended(pid) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Failure::new(
                FailureKind::Io,
                format!("Cannot start {}: {error}", program.display()),
            ));
        }
        tree
    };

    let capacity = spec
        .output_limit
        .map(|limit| limit as usize)
        .unwrap_or(DEFAULT_PIPE_OUTPUT);
    let (sender, receiver) = mpsc::channel();
    let stdin = child.stdin.take();
    let session = Arc::new(Session {
        sid: spec.sid.clone(),
        owner: context.owner,
        pid,
        terminal: false,
        label,
        started: Instant::now(),
        orphan_ttl,
        state: Mutex::new(SessionState::new(capacity, true)),
        process: Process::Tree(tree),
        input: Mutex::new(InputPort {
            received: 0,
            sender: stdin.is_some().then_some(sender.clone()),
        }),
        master: Mutex::new(None),
        closing: AtomicBool::new(false),
        reclaiming: AtomicBool::new(false),
        notifier: context.notifier,
    });

    if let Some(stdin) = stdin {
        start_input_writer(&session, Box::new(stdin), receiver);
        if spec.stdin == StdinMode::Body {
            lock(&session.input).received = body.len() as u64;
            let _ = sender.send(InputCommand::Data(body));
            session.close_input();
        }
    }
    drop(sender);
    if let Some(stdout) = child.stdout.take() {
        start_pipe_reader(&session, Stream::Stdout, stdout);
    }
    if let Some(stderr) = child.stderr.take() {
        start_pipe_reader(&session, Stream::Stderr, stderr);
    }
    let waiter = Arc::clone(&session);
    std::thread::Builder::new()
        .name(format!("wait-{}", spec.sid))
        .spawn(move || {
            let (code, signal) = match child.wait() {
                Ok(status) => exit_parts(status),
                Err(_) => (None, None),
            };
            waiter.reaped(code, signal);
        })
        .map_err(|error| Failure::new(FailureKind::Io, format!("Cannot start a thread: {error}")))?;
    Ok(session)
}

#[allow(clippy::too_many_arguments)]
fn spawn_terminal(
    spec: &SpawnSpec,
    program: &Path,
    cwd: &Path,
    env: &BTreeMap<String, String>,
    size: TerminalSize,
    orphan_ttl: Duration,
    label: Option<String>,
    context: SpawnContext<'_>,
) -> Result<Arc<Session>, Failure> {
    use portable_pty::{native_pty_system, CommandBuilder};
    let pair = native_pty_system()
        .openpty(pty_size(size))
        .map_err(|error| Failure::new(FailureKind::Io, format!("Cannot open a terminal: {error}")))?;
    let mut builder = CommandBuilder::new(program.as_os_str());
    builder.args(&spec.argv[1..]);
    builder.cwd(cwd.as_os_str());
    builder.env_clear();
    for (name, value) in env {
        builder.env(name, value);
    }
    let child = pair
        .slave
        .spawn_command(builder)
        .map_err(|error| Failure::new(FailureKind::Io, format!("Cannot start {}: {error}", program.display())))?;
    drop(pair.slave);
    let pid = child.process_id().unwrap_or(0);
    #[cfg(unix)]
    let tree = ProcessTree::adopt(pid);
    #[cfg(windows)]
    let tree = ProcessTree::adopt(child.as_raw_handle().unwrap_or(std::ptr::null_mut()) as _);

    let writer = pair
        .master
        .take_writer()
        .map_err(|error| Failure::new(FailureKind::Io, format!("Cannot write to the terminal: {error}")))?;
    #[cfg(unix)]
    let reader_fd = {
        let fd = pair
            .master
            .as_raw_fd()
            .ok_or_else(|| Failure::new(FailureKind::Io, "The terminal has no descriptor"))?;
        let duplicate = unsafe { libc::dup(fd) };
        if duplicate < 0 {
            return Err(Failure::new(
                FailureKind::Io,
                format!("Cannot read the terminal: {}", std::io::Error::last_os_error()),
            ));
        }
        unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(duplicate) }
    };
    #[cfg(windows)]
    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|error| Failure::new(FailureKind::Io, format!("Cannot read the terminal: {error}")))?;

    let capacity = spec
        .output_limit
        .map(|limit| limit as usize)
        .unwrap_or(DEFAULT_TERMINAL_OUTPUT);
    let (sender, receiver) = mpsc::channel();
    let session = Arc::new(Session {
        sid: spec.sid.clone(),
        owner: context.owner,
        pid,
        terminal: true,
        label,
        started: Instant::now(),
        orphan_ttl,
        state: Mutex::new(SessionState::new(capacity, false)),
        process: Process::Tree(tree),
        // A terminal is typed into for as long as it runs, whatever the spec
        // says: closing its input would hand the shell an end of file.
        input: Mutex::new(InputPort {
            received: 0,
            sender: Some(sender),
        }),
        master: Mutex::new(Some(pair.master)),
        closing: AtomicBool::new(false),
        reclaiming: AtomicBool::new(false),
        notifier: context.notifier,
    });
    start_input_writer(&session, writer, receiver);
    #[cfg(unix)]
    start_terminal_reader(&session, reader_fd);
    #[cfg(windows)]
    start_pipe_reader(&session, Stream::Stdout, reader);

    let waiter = Arc::clone(&session);
    std::thread::Builder::new()
        .name(format!("wait-{}", spec.sid))
        .spawn(move || {
            let (code, signal) = wait_terminal_child(child);
            // Let the last output drain, then stop reading: a background job
            // holding the terminal open must not keep the session alive.
            let deadline = Instant::now() + TERMINAL_DRAIN;
            while Instant::now() < deadline && !lock(&waiter.state).stdout_done {
                std::thread::sleep(Duration::from_millis(20));
            }
            waiter.closing.store(true, Ordering::SeqCst);
            // Windows: closing the pseudo console is what ends its reader.
            #[cfg(windows)]
            drop(lock(&waiter.master).take());
            waiter.reaped(code, signal);
        })
        .map_err(|error| Failure::new(FailureKind::Io, format!("Cannot start a thread: {error}")))?;
    Ok(session)
}

/// Takes a process a cell started (see [`super::cells`]) as a session of this
/// daemon's: its output read into this daemon's rings, its input, signals
/// and resizes passed on, its exit taken from the cell's. The host sees an
/// ordinary session.
pub fn spawn_relayed(
    spec: &SpawnSpec,
    mut process: crate::client::RemoteProcess,
    context: SpawnContext<'_>,
) -> Result<Arc<Session>, Failure> {
    let terminal = spec.terminal.is_some();
    let stdout = process.take_stdout();
    let stderr = process.take_stderr();
    let process = Arc::new(process);
    let capacity = spec.output_limit.map(|limit| limit as usize).unwrap_or(if terminal {
        DEFAULT_TERMINAL_OUTPUT
    } else {
        DEFAULT_PIPE_OUTPUT
    });
    let orphan_ttl = spec
        .orphan_ttl_secs
        .map(Duration::from_secs)
        .unwrap_or(context.orphan_ttl);
    let takes_input = terminal || spec.stdin == StdinMode::Pipe;
    let (sender, receiver) = mpsc::channel();
    let session = Arc::new(Session {
        sid: spec.sid.clone(),
        owner: context.owner,
        pid: process.pid(),
        terminal,
        label: spec.label.clone(),
        started: Instant::now(),
        orphan_ttl,
        state: Mutex::new(SessionState::new(capacity, !terminal)),
        process: Process::Relayed(Arc::clone(&process)),
        input: Mutex::new(InputPort {
            received: 0,
            sender: takes_input.then_some(sender),
        }),
        master: Mutex::new(None),
        closing: AtomicBool::new(false),
        reclaiming: AtomicBool::new(false),
        notifier: context.notifier,
    });
    if takes_input {
        start_input_writer(
            &session,
            Box::new(RelayedInput {
                writer: process.stdin(),
                process: Arc::clone(&process),
            }),
            receiver,
        );
    }
    // The cell had the body with the spawn itself.
    if spec.stdin == StdinMode::Body {
        lock(&session.input).received = 0;
    }
    match stdout {
        Some(stdout) => start_pipe_reader(&session, Stream::Stdout, stdout),
        None => session.stream_done(Stream::Stdout),
    }
    if !terminal {
        match stderr {
            Some(stderr) => start_pipe_reader(&session, Stream::Stderr, stderr),
            None => session.stream_done(Stream::Stderr),
        }
    }
    let waiter = Arc::clone(&session);
    std::thread::Builder::new()
        .name(format!("wait-{}", spec.sid))
        .spawn(move || {
            let (code, signal) = match process.wait() {
                Ok(exit) => (exit.code, exit.signal),
                // The cell is gone, and the process with it.
                Err(_) => (None, None),
            };
            waiter.reaped(code, signal);
        })
        .map_err(|error| Failure::new(FailureKind::Io, format!("Cannot start a thread: {error}")))?;
    Ok(session)
}

/// A relayed session's input: writes go to the cell, and closing it closes
/// the process's input there.
struct RelayedInput {
    writer: crate::client::SessionWriter,
    process: Arc<crate::client::RemoteProcess>,
}

impl Write for RelayedInput {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.writer.write(buffer)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

impl Drop for RelayedInput {
    fn drop(&mut self) {
        self.process.close_stdin();
    }
}

fn wait_terminal_child(child: Box<dyn portable_pty::Child + Send + Sync>) -> (Option<i32>, Option<i32>) {
    let child: Box<dyn portable_pty::Child> = child;
    // On Unix the terminal's child is a plain `std::process::Child`, whose
    // status carries the signal number rather than its description.
    #[cfg(unix)]
    let child = match child.downcast::<std::process::Child>() {
        Ok(mut child) => {
            return match child.wait() {
                Ok(status) => exit_parts(status),
                Err(_) => (None, None),
            }
        }
        Err(child) => child,
    };
    let mut child = child;
    match child.wait() {
        Ok(status) if status.signal().is_some() => (None, Some(-1)),
        Ok(status) => (Some(status.exit_code() as i32), None),
        Err(_) => (None, None),
    }
}

fn exit_parts(status: std::process::ExitStatus) -> (Option<i32>, Option<i32>) {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        (status.code(), status.signal())
    }
    #[cfg(windows)]
    {
        (status.code(), None)
    }
}

fn start_pipe_reader<R: Read + Send + 'static>(session: &Arc<Session>, stream: Stream, mut pipe: R) {
    let reader = Arc::clone(session);
    let spawned = std::thread::Builder::new()
        .name(format!("read-{}", session.sid))
        .spawn(move || {
            let mut chunk = vec![0u8; 32 * 1024];
            loop {
                match pipe.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(count) => reader.record_output(stream, &chunk[..count]),
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            reader.stream_done(stream);
        });
    if spawned.is_err() {
        // Without a reader the stream can never reach its end by itself;
        // marking it done still lets the exit be published when the process
        // goes, which is better than a session that never finishes.
        session.stream_done(stream);
    }
}

/// Reads a terminal until it closes or the session stops reading it. Polled,
/// so the `closing` flag is noticed even while the terminal stays silent.
#[cfg(unix)]
fn start_terminal_reader(session: &Arc<Session>, fd: std::os::fd::OwnedFd) {
    use std::os::fd::AsRawFd;
    let session = Arc::clone(session);
    let _ = std::thread::Builder::new()
        .name(format!("pty-{}", session.sid))
        .spawn(move || {
            let raw = fd.as_raw_fd();
            let mut chunk = vec![0u8; 32 * 1024];
            loop {
                if session.closing.load(Ordering::SeqCst) {
                    break;
                }
                let mut poll = libc::pollfd {
                    fd: raw,
                    events: libc::POLLIN,
                    revents: 0,
                };
                let ready = unsafe { libc::poll(&mut poll, 1, 100) };
                if ready < 0 {
                    if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    break;
                }
                if ready == 0 {
                    continue;
                }
                let count = unsafe { libc::read(raw, chunk.as_mut_ptr().cast(), chunk.len()) };
                if count > 0 {
                    session.record_output(Stream::Stdout, &chunk[..count as usize]);
                    continue;
                }
                if count < 0
                    && matches!(
                        std::io::Error::last_os_error().kind(),
                        std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                    )
                {
                    continue;
                }
                // 0, or EIO once every copy of the terminal's other end closed.
                break;
            }
            drop(fd);
            session.stream_done(Stream::Stdout);
            // Only now is nothing reading the master any more.
            drop(lock(&session.master).take());
        });
}

fn start_input_writer(
    session: &Arc<Session>,
    mut writer: Box<dyn Write + Send>,
    receiver: mpsc::Receiver<InputCommand>,
) {
    let name = format!("input-{}", session.sid);
    let _ = std::thread::Builder::new().name(name).spawn(move || {
        while let Ok(command) = receiver.recv() {
            match command {
                InputCommand::Data(bytes) => {
                    if writer.write_all(&bytes).and_then(|_| writer.flush()).is_err() {
                        // The process stopped reading; later input has nowhere
                        // to go. Drain the queue so senders never block.
                        while receiver.recv().is_ok() {}
                        break;
                    }
                }
                InputCommand::Close => break,
            }
        }
        drop(writer);
    });
}

/// The environment a session starts with: the owner's login environment, less
/// the daemon's private names and what the host removes, plus what it sets.
///
/// Windows spells a variable's name in any case and means one variable: the
/// login environment's `Path` is the `PATH` a host sets, and must be replaced
/// by it rather than sit beside it for the process to pick one of.
pub fn session_env(
    base: &[(String, String)],
    remove: &[String],
    set: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut env: BTreeMap<String, String> = if base.is_empty() {
        std::env::vars().collect()
    } else {
        base.iter().cloned().collect()
    };
    env.retain(|name, _| !PRIVATE_ENV.iter().any(|private| same_env_name(private, name)));
    for name in remove {
        env.retain(|existing, _| !same_env_name(existing, name));
    }
    for (name, value) in set {
        env.retain(|existing, _| !same_env_name(existing, name));
        env.insert(name.clone(), value.clone());
    }
    env
}

fn same_env_name(left: &str, right: &str) -> bool {
    if cfg!(windows) {
        left.eq_ignore_ascii_case(right)
    } else {
        left == right
    }
}

/// Finds `program` the way a shell would: a name with a separator is a path
/// (after `~` expansion, relative to `cwd`), anything else is looked up on the
/// session's own `PATH`.
pub fn resolve_program(
    program: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
    home: &Path,
) -> Option<PathBuf> {
    if cfg!(windows) {
        if let Some(name) = posix_interpreter(program) {
            return resolve_program(name, env, cwd, home);
        }
    }
    let has_separator = program.contains('/') || (cfg!(windows) && program.contains('\\'));
    if has_separator || program.starts_with('~') {
        let path = native_path(program, home);
        let path = if path.is_absolute() { path } else { cwd.join(path) };
        return is_executable(&path).then_some(path);
    }
    let path_var = env
        .iter()
        .find(|(name, _)| {
            if cfg!(windows) {
                name.eq_ignore_ascii_case("PATH")
            } else {
                name.as_str() == "PATH"
            }
        })
        .map(|(_, value)| value.clone())
        .unwrap_or_default();
    let extensions: Vec<String> = if cfg!(windows) {
        let pathext = env
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("PATHEXT"))
            .map(|(_, value)| value.clone())
            .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into());
        std::iter::once(String::new())
            .chain(pathext.split(';').filter(|e| !e.is_empty()).map(str::to_owned))
            .collect()
    } else {
        vec![String::new()]
    };
    for directory in std::env::split_paths(&path_var) {
        if directory.as_os_str().is_empty() || !directory.is_absolute() {
            continue;
        }
        if cfg!(windows) && is_windows_launcher_dir(&directory) && program.eq_ignore_ascii_case("bash") {
            // The WSL `bash.exe` launcher would run the command in another
            // machine's filesystem; see `run_environment::local_bash_candidates`.
            continue;
        }
        for extension in &extensions {
            let candidate = directory.join(format!("{program}{extension}"));
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    if cfg!(windows) && (program.eq_ignore_ascii_case("bash") || program.eq_ignore_ascii_case("sh")) {
        return windows_git_shell(program, &path_var);
    }
    None
}

/// The POSIX interpreters scripts name by their Unix path. On Windows they are
/// Git for Windows' own, found the way a bare `bash` or `sh` is.
fn posix_interpreter(program: &str) -> Option<&'static str> {
    match program {
        "/bin/sh" | "/usr/bin/sh" => Some("sh"),
        "/bin/bash" | "/usr/bin/bash" => Some("bash"),
        _ => None,
    }
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(windows)]
    {
        path.is_file()
    }
}

fn is_windows_launcher_dir(directory: &Path) -> bool {
    let text = directory.to_string_lossy().to_ascii_lowercase().replace('/', "\\");
    let system_root = std::env::var("SystemRoot")
        .unwrap_or_else(|_| r"C:\Windows".into())
        .to_ascii_lowercase();
    text.starts_with(&system_root) || text.ends_with(r"\microsoft\windowsapps")
}

/// Git for Windows advertises only its `cmd` directory; its `bash` and `sh`
/// sit beside it. Then the usual install locations. `bin\bash.exe` comes
/// first: it is the launcher that puts Git's own tools on the `PATH`.
fn windows_git_shell(program: &str, path_var: &str) -> Option<PathBuf> {
    let executable = format!("{}.exe", program.to_ascii_lowercase());
    for directory in std::env::split_paths(path_var) {
        if directory.join("git.exe").is_file() {
            if let Some(root) = directory.parent() {
                for candidate in [root.join("bin").join(&executable), root.join("usr").join("bin").join(&executable)] {
                    if candidate.is_file() {
                        return Some(candidate);
                    }
                }
            }
        }
    }
    for variable in ["ProgramFiles", "ProgramW6432", "LocalAppData"] {
        if let Some(value) = std::env::var_os(variable) {
            let root = PathBuf::from(value);
            for candidate in [
                root.join("Git").join("bin").join(&executable),
                root.join("Programs").join("Git").join("bin").join(&executable),
            ] {
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

fn spawn_failure(program: &Path, error: std::io::Error) -> Failure {
    let kind = if error.kind() == std::io::ErrorKind::NotFound {
        FailureKind::NotFound
    } else {
        FailureKind::Io
    };
    Failure::new(kind, format!("Cannot start {}: {error}", program.display()))
}

fn pty_size(size: TerminalSize) -> portable_pty::PtySize {
    portable_pty::PtySize {
        rows: size.rows.clamp(1, 1000),
        cols: size.cols.clamp(1, 1000),
        pixel_width: 0,
        pixel_height: 0,
    }
}

pub fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}
