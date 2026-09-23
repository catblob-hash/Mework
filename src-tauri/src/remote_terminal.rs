//! A terminal on an SSH machine that opens at once and attaches when it can.
//!
//! The terminal manager speaks `portable_pty`: a master to read, write and
//! resize, a child to wait on and kill. A terminal served by the agent needs
//! a connected link first, and the first connection to a machine installs the
//! agent there — seconds on a good network, longer on a poor one — while
//! opening a terminal is a synchronous call the window is waiting on. So the
//! pair returned here is a front: it answers at once, queues what is typed,
//! remembers the size, and a background thread attaches the real terminal
//! behind it:
//!
//! * the agent's pseudo terminal on the machine, when the link is ready — a
//!   shell that survives the network dropping, because it belongs to the
//!   daemon, not to an SSH session;
//! * otherwise an interactive `ssh -t` in a local pseudo terminal, exactly
//!   what the terminal was before the agent existed, for machines the agent
//!   does not serve;
//! * or, when the machine cannot be reached at all, a line saying so and the
//!   exit code `ssh` itself would have ended with.
//!
//! Nothing here ever holds a local process id for a remote process: the pair
//! reports none, so the manager's process-tree kill can never be pointed at an
//! unrelated local process that happens to share a remote pid.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use portable_pty::{Child, ChildKiller, CommandBuilder, ExitStatus, MasterPty, PtySize};
use remote_agent::client::{Link, RemoteProcess};
use remote_agent::protocol::{Op, Reply, SignalKind, TerminalSize};

use crate::remote_link::{self, Route};
use crate::run_environment::ShellRunner;

/// What the agent runs for the terminal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentTerminalSpec {
    pub runner: ShellRunner,
    /// What a Unix machine runs.
    pub argv: Vec<String>,
    /// The shell chosen by name, for a Windows machine, which has no
    /// `/bin/sh` to make the choice with: run where the machine has it — Git
    /// for Windows brings `bash` — and PowerShell otherwise. `None` is
    /// PowerShell, the shell every Windows has.
    pub windows_shell: Option<String>,
    pub cwd: String,
    pub env: Vec<(String, String)>,
    /// Shown while the terminal waits for the link.
    pub machine_label: String,
}

/// How long the terminal stays silent before it says it is still connecting.
const CONNECTING_NOTICE_DELAY: Duration = Duration::from_millis(1500);
/// Most output held for a reader that has not taken it.
const MAX_QUEUED_OUTPUT: usize = 4 << 20;
/// What `ssh` exits with when it cannot reach a machine; the terminal ends the
/// same way when the agent's link cannot.
const UNREACHABLE_EXIT: u32 = 255;

enum Backend {
    Connecting,
    Agent(Arc<RemoteProcess>),
    Local {
        master: Box<dyn MasterPty + Send>,
        killer: Box<dyn ChildKiller + Send + Sync>,
    },
    Ended,
}

struct State {
    backend: Backend,
    size: PtySize,
    /// Typed before a backend was attached.
    pending_input: Vec<u8>,
    writer: Option<Box<dyn Write + Send>>,
    exit: Option<ExitStatus>,
    kill_requested: bool,
    output: VecDeque<u8>,
    output_closed: bool,
}

struct Core {
    state: Mutex<State>,
    cond: Condvar,
}

impl Core {
    fn push_output(&self, bytes: &[u8]) {
        let mut state = lock(&self.state);
        state.output.extend(bytes);
        let excess = state.output.len().saturating_sub(MAX_QUEUED_OUTPUT);
        state.output.drain(..excess);
        drop(state);
        self.cond.notify_all();
    }

    fn close_output(&self) {
        lock(&self.state).output_closed = true;
        self.cond.notify_all();
    }

    fn finish(&self, status: ExitStatus) {
        let mut state = lock(&self.state);
        if state.exit.is_none() {
            state.exit = Some(status);
        }
        state.backend = Backend::Ended;
        state.writer = None;
        drop(state);
        self.cond.notify_all();
    }

    /// Copies a backend's output into the queue the terminal reads.
    fn pump(self: &Arc<Self>, mut reader: Box<dyn Read + Send>) {
        let core = Arc::clone(self);
        std::thread::spawn(move || {
            let mut chunk = [0u8; 16 * 1024];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(count) => core.push_output(&chunk[..count]),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => {
                        core.push_output(format!("\r\n[Mework] {error}\r\n").as_bytes());
                        break;
                    }
                }
            }
            core.close_output();
        });
    }
}

/// Opens the terminal: returns at once, attaching the agent's terminal — or,
/// when the agent does not serve the machine, `fallback` in a local pseudo
/// terminal — in the background.
pub fn start(
    spec: AgentTerminalSpec,
    fallback: CommandBuilder,
    size: PtySize,
) -> (Box<dyn MasterPty + Send>, Box<dyn Child + Send + Sync>) {
    let core = Arc::new(Core {
        state: Mutex::new(State {
            backend: Backend::Connecting,
            size,
            pending_input: Vec::new(),
            writer: None,
            exit: None,
            kill_requested: false,
            output: VecDeque::new(),
            output_closed: false,
        }),
        cond: Condvar::new(),
    });
    {
        let core = Arc::clone(&core);
        let label = spec.machine_label.clone();
        std::thread::spawn(move || {
            std::thread::sleep(CONNECTING_NOTICE_DELAY);
            let connecting = {
                let state = lock(&core.state);
                matches!(state.backend, Backend::Connecting) && state.output.is_empty()
            };
            if connecting {
                core.push_output(
                    format!("Mework：正在连接 {label}（首次连接会在那台机器上安装 Mework agent）…\r\n")
                        .as_bytes(),
                );
            }
        });
    }
    {
        let core = Arc::clone(&core);
        std::thread::Builder::new()
            .name("remote-terminal-attach".into())
            .spawn(move || attach(&core, spec, fallback))
            .expect("the terminal's attach thread starts");
    }
    (
        Box::new(DeferredMaster {
            core: Arc::clone(&core),
        }),
        Box::new(DeferredChild { core }),
    )
}

fn attach(core: &Arc<Core>, spec: AgentTerminalSpec, fallback: CommandBuilder) {
    match remote_link::route(&spec.runner, remote_link::TERMINAL_CONNECT_WAIT) {
        Route::Agent(link, agent) => {
            let argv = if agent.os == "windows" {
                windows_argv(core, &link, spec.windows_shell.as_deref())
            } else {
                spec.argv.clone()
            };
            let size = lock(&core.state).size;
            let started = remote_link::spawn_terminal(
                &link,
                &spec.runner,
                argv,
                &spec.cwd,
                &spec.env,
                TerminalSize {
                    cols: size.cols,
                    rows: size.rows,
                },
            );
            match started {
                Ok(child) => attach_agent(core, child.process),
                Err(error) => fail(core, &error),
            }
        }
        Route::Legacy => attach_local(core, fallback),
        Route::Unreachable(error) => fail(core, &error),
    }
}

/// A Windows machine's terminal: the chosen shell if the machine has it,
/// PowerShell otherwise — with a note, as a Unix machine without the chosen
/// shell says it fell back to the login shell.
fn windows_argv(core: &Arc<Core>, link: &Link, chosen: Option<&str>) -> Vec<String> {
    if let Some(name) = chosen {
        let found = match link.call(
            Op::Which {
                names: vec![name.to_owned()],
            },
            b"",
            Duration::from_secs(30),
        ) {
            Ok(Reply::Which { found }) => found.get(name).cloned().flatten(),
            _ => None,
        };
        if found.is_some() {
            return vec![name.to_owned(), "-l".to_owned()];
        }
        core.push_output(format!("Mework：这台机器上没有 {name}，改用 PowerShell。\r\n").as_bytes());
    }
    vec!["powershell.exe".to_owned(), "-NoLogo".to_owned()]
}

fn attach_agent(core: &Arc<Core>, mut process: RemoteProcess) {
    let reader = process.take_stdout();
    let process = Arc::new(process);
    {
        let mut state = lock(&core.state);
        if state.kill_requested {
            drop(state);
            process.kill();
            return;
        }
        let mut writer: Box<dyn Write + Send> = Box::new(process.stdin());
        let pending = std::mem::take(&mut state.pending_input);
        if !pending.is_empty() {
            let _ = writer.write_all(&pending);
        }
        let size = state.size;
        state.writer = Some(writer);
        state.backend = Backend::Agent(Arc::clone(&process));
        // A resize that arrived while connecting was remembered; the agent
        // started the terminal at the size it had then.
        process.resize(TerminalSize {
            cols: size.cols,
            rows: size.rows,
        });
    }
    if let Some(reader) = reader {
        core.pump(Box::new(reader));
    }
    let core = Arc::clone(core);
    std::thread::spawn(move || {
        let status = match process.wait() {
            Ok(exit) => match (exit.code, exit.signal) {
                (Some(code), _) => ExitStatus::with_exit_code(code as u32),
                (None, _) => ExitStatus::with_signal("Hangup"),
            },
            Err(error) => {
                core.push_output(format!("\r\n[Mework] {error}\r\n").as_bytes());
                ExitStatus::with_exit_code(UNREACHABLE_EXIT)
            }
        };
        core.finish(status);
    });
}

fn attach_local(core: &Arc<Core>, fallback: CommandBuilder) {
    let size = lock(&core.state).size;
    let pair = match portable_pty::native_pty_system().openpty(size) {
        Ok(pair) => pair,
        Err(error) => return fail(core, &format!("无法创建伪终端：{error}")),
    };
    let reader = match pair.master.try_clone_reader() {
        Ok(reader) => reader,
        Err(error) => return fail(core, &format!("无法读取伪终端：{error}")),
    };
    let mut writer = match pair.master.take_writer() {
        Ok(writer) => writer,
        Err(error) => return fail(core, &format!("无法写入伪终端：{error}")),
    };
    let mut child = match pair.slave.spawn_command(fallback) {
        Ok(child) => child,
        Err(error) => return fail(core, &format!("无法启动终端 shell：{error}")),
    };
    drop(pair.slave);
    let killer = child.clone_killer();
    {
        let mut state = lock(&core.state);
        if state.kill_requested {
            drop(state);
            let _ = child.kill();
            return;
        }
        let pending = std::mem::take(&mut state.pending_input);
        if !pending.is_empty() {
            let _ = writer.write_all(&pending);
        }
        if state.size != size {
            let _ = pair.master.resize(state.size);
        }
        state.writer = Some(writer);
        state.backend = Backend::Local {
            master: pair.master,
            killer,
        };
    }
    core.pump(reader);
    let core = Arc::clone(core);
    std::thread::spawn(move || {
        let status = child
            .wait()
            .unwrap_or_else(|_| ExitStatus::with_exit_code(UNREACHABLE_EXIT));
        core.finish(status);
    });
}

fn fail(core: &Arc<Core>, error: &str) {
    core.push_output(format!("\r\n[Mework] {error}\r\n").as_bytes());
    core.close_output();
    core.finish(ExitStatus::with_exit_code(UNREACHABLE_EXIT));
}

struct DeferredMaster {
    core: Arc<Core>,
}

impl MasterPty for DeferredMaster {
    fn resize(&self, size: PtySize) -> Result<(), anyhow::Error> {
        let mut state = lock(&self.core.state);
        state.size = size;
        match &state.backend {
            Backend::Agent(process) => process.resize(TerminalSize {
                cols: size.cols,
                rows: size.rows,
            }),
            Backend::Local { master, .. } => master.resize(size)?,
            Backend::Connecting | Backend::Ended => {}
        }
        Ok(())
    }

    fn get_size(&self) -> Result<PtySize, anyhow::Error> {
        Ok(lock(&self.core.state).size)
    }

    fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>, anyhow::Error> {
        Ok(Box::new(DeferredReader {
            core: Arc::clone(&self.core),
        }))
    }

    fn take_writer(&self) -> Result<Box<dyn Write + Send>, anyhow::Error> {
        Ok(Box::new(DeferredWriter {
            core: Arc::clone(&self.core),
        }))
    }

    #[cfg(unix)]
    fn process_group_leader(&self) -> Option<libc::pid_t> {
        None
    }

    #[cfg(unix)]
    fn as_raw_fd(&self) -> Option<std::os::unix::io::RawFd> {
        None
    }

    #[cfg(unix)]
    fn tty_name(&self) -> Option<std::path::PathBuf> {
        None
    }
}

struct DeferredReader {
    core: Arc<Core>,
}

impl Read for DeferredReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let mut state = lock(&self.core.state);
        loop {
            if !state.output.is_empty() {
                let count = buffer.len().min(state.output.len());
                for (slot, byte) in buffer.iter_mut().zip(state.output.drain(..count)) {
                    *slot = byte;
                }
                return Ok(count);
            }
            if state.output_closed {
                return Ok(0);
            }
            state = self
                .core
                .cond
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }
}

struct DeferredWriter {
    core: Arc<Core>,
}

impl Write for DeferredWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let mut state = lock(&self.core.state);
        match state.backend {
            Backend::Connecting => {
                state.pending_input.extend_from_slice(buffer);
                Ok(buffer.len())
            }
            Backend::Ended => Err(io::Error::new(io::ErrorKind::BrokenPipe, "终端 shell 已退出")),
            Backend::Agent(_) | Backend::Local { .. } => match state.writer.as_mut() {
                Some(writer) => writer.write(buffer),
                None => Err(io::Error::new(io::ErrorKind::BrokenPipe, "终端 shell 已退出")),
            },
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match lock(&self.core.state).writer.as_mut() {
            Some(writer) => writer.flush(),
            None => Ok(()),
        }
    }
}

#[derive(Clone)]
struct DeferredChild {
    core: Arc<Core>,
}

impl std::fmt::Debug for DeferredChild {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DeferredChild")
    }
}

impl ChildKiller for DeferredChild {
    /// Hangs up the terminal, as closing its window would. A terminal still
    /// connecting ends at once; whatever the connection produces afterwards is
    /// ended as soon as it is attached.
    fn kill(&mut self) -> io::Result<()> {
        let still_connecting = {
            let mut state = lock(&self.core.state);
            state.kill_requested = true;
            match &mut state.backend {
                Backend::Agent(process) => {
                    process.signal(SignalKind::Hangup);
                    false
                }
                Backend::Local { killer, .. } => {
                    let _ = killer.kill();
                    false
                }
                Backend::Connecting => true,
                Backend::Ended => false,
            }
        };
        if still_connecting {
            self.core.close_output();
            self.core.finish(ExitStatus::with_signal("Hangup"));
        }
        Ok(())
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(self.clone())
    }
}

impl Child for DeferredChild {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        Ok(lock(&self.core.state).exit.clone())
    }

    fn wait(&mut self) -> io::Result<ExitStatus> {
        let mut state = lock(&self.core.state);
        loop {
            if let Some(status) = state.exit.clone() {
                return Ok(status);
            }
            state = self
                .core
                .cond
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    /// None on purpose: the process, if any, is on another machine, and a
    /// local process-tree kill must never be aimed at its number.
    fn process_id(&self) -> Option<u32> {
        None
    }

    #[cfg(windows)]
    fn as_raw_handle(&self) -> Option<std::os::windows::io::RawHandle> {
        None
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With no agent installed (as in every unit test), the terminal falls
    /// back to the command it was given, in a local pseudo terminal, and
    /// behaves as that command does — including input typed before it was
    /// attached.
    #[cfg(unix)]
    #[test]
    fn without_the_agent_the_fallback_runs_and_gets_early_input() {
        let spec = AgentTerminalSpec {
            runner: ShellRunner::Ssh {
                host: "fallback.invalid".into(),
                port: 0,
                identity_file: String::new(),
                env: Default::default(),
            },
            argv: vec!["/bin/sh".into()],
            windows_shell: None,
            cwd: "~".into(),
            env: Vec::new(),
            machine_label: "fallback.invalid".into(),
        };
        let mut fallback = CommandBuilder::new("/bin/sh");
        fallback.args(["-c", "read line; echo got:$line; exit 4"]);
        let (master, mut child) = start(
            spec,
            fallback,
            PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            },
        );
        let mut writer = master.take_writer().unwrap();
        writer.write_all(b"early\n").unwrap();
        let mut reader = master.try_clone_reader().unwrap();
        let status = child.wait().unwrap();
        assert_eq!(status.exit_code(), 4);
        assert_eq!(child.process_id(), None);
        let mut output = Vec::new();
        let collector = std::thread::spawn(move || {
            let _ = reader.read_to_end(&mut output);
            output
        });
        let output = collector.join().unwrap();
        let text = String::from_utf8_lossy(&output);
        assert!(text.contains("got:early"), "{text}");
    }

    #[test]
    fn killing_a_terminal_that_is_still_connecting_ends_it_at_once() {
        let core = Arc::new(Core {
            state: Mutex::new(State {
                backend: Backend::Connecting,
                size: PtySize::default(),
                pending_input: Vec::new(),
                writer: None,
                exit: None,
                kill_requested: false,
                output: VecDeque::new(),
                output_closed: false,
            }),
            cond: Condvar::new(),
        });
        let mut child = DeferredChild {
            core: Arc::clone(&core),
        };
        child.kill().unwrap();
        assert!(child.wait().unwrap().signal().is_some());
        let mut reader = DeferredReader { core };
        assert_eq!(reader.read(&mut [0u8; 8]).unwrap(), 0);
    }
}
