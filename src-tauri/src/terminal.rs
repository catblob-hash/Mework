use std::{
    collections::{HashMap, HashSet, VecDeque},
    env,
    ffi::OsString,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    thread,
    time::Duration,
};

use base64::Engine as _;
use portable_pty::{
    native_pty_system, Child as PtyChild, ChildKiller, CommandBuilder, MasterPty, PtySize,
};
use serde::Serialize;
use tauri::ipc::Channel;
use uuid::Uuid;

mod bash_control;
#[cfg(unix)]
mod posix_control;
mod reply_files;

const MAX_BUFFER_BYTES: usize = 2 * 1024 * 1024;
const MAX_INPUT_BYTES: usize = 64 * 1024;
const MAX_TERMINAL_SESSIONS: usize = 64;
const MAX_COLS: u16 = 500;
const MAX_ROWS: u16 = 300;
const CONTROL_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const CONTROL_PREFIX: &[u8] = b"\x1b]633;Mework;v1;";
const CONTROL_TERMINATOR: u8 = 0x07;
/// How long a zsh, bash or fish hook waits for the host to answer `start`
/// before refusing the line itself, matching the PowerShell hook.
const DECISION_TIMEOUT_SECONDS: u32 = 30;
/// The message a refused line prints in zsh, bash and fish, where the line
/// stays in the editor; PowerShell's own says to recall it instead.
const REFUSED_MESSAGE: &str =
    "Mework 未执行该命令：工作区正在进行 Git、模型或其他写操作；请稍后按回车重试。";

pub type TerminalCommandLease = Box<dyn Send + 'static>;
pub type TerminalCommandLeaseFactory =
    Arc<dyn Fn() -> Result<TerminalCommandLease, String> + Send + Sync + 'static>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalCommandStatus {
    Idle,
    Running,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalCommandState {
    pub revision: u64,
    pub status: TerminalCommandStatus,
    pub command_id: Option<String>,
    pub command_count: u64,
}

impl Default for TerminalCommandState {
    fn default() -> Self {
        Self {
            revision: 0,
            status: TerminalCommandStatus::Idle,
            command_id: None,
            command_count: 0,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TerminalEvent {
    Output {
        #[serde(rename = "sessionId")]
        session_id: String,
        data: Vec<u8>,
    },
    Exit {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "exitCode")]
        exit_code: Option<u32>,
    },
    Error {
        #[serde(rename = "sessionId")]
        session_id: String,
        message: String,
    },
    CommandState {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "commandState")]
        command_state: TerminalCommandState,
    },
    Ready {
        #[serde(rename = "sessionId")]
        session_id: String,
    },
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalOpenResponse {
    pub created: bool,
    pub running: bool,
    pub ready: bool,
    pub session_id: String,
    pub snapshot: Vec<u8>,
    pub cwd: String,
    pub shell: String,
    pub command_state: TerminalCommandState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShellControl {
    /// PowerShell with the PSReadLine hook from [`configure_powershell_control`].
    #[cfg(windows)]
    PowerShell,
    /// zsh with the startup files from [`posix_control::ControlFifo`]: the same
    /// frames, answered through a FIFO instead of named events.
    #[cfg(unix)]
    Zsh,
    /// bash with the rcfile from [`bash_control`], answered through a FIFO on a
    /// Mac or Linux host and through [`reply_files`] under Git Bash on Windows.
    Bash,
    /// fish with the script from [`posix_control::ControlFifo`].
    #[cfg(unix)]
    Fish,
    /// A shell on another machine, reached through `wsl.exe` or `ssh`. It has no
    /// command-state frames, and needs none: the frames exist to hold the host
    /// checkout's Git mutex while a command runs, and a remote workspace has no
    /// host checkout to protect.
    Remote,
    Unsupported,
}

/// How the host answers a shell's `start` frame.
///
/// The shell does not run the command until it hears back, which is what makes
/// the command lease a barrier rather than a record. PowerShell waits on two
/// named Win32 events; zsh, bash and fish read a line naming the generation
/// from a FIFO; Git Bash looks for a reply file naming it.
enum ControlReply {
    /// A remote shell sends no frames, so it is never answered.
    None,
    #[cfg(windows)]
    Events { ack: String, reject: String },
    #[cfg(unix)]
    Fifo(posix_control::ControlFifo),
    #[cfg(windows)]
    ReplyFiles(reply_files::ControlReplyFiles),
}

impl ControlReply {
    fn ack(&self, generation: u64) -> Result<(), String> {
        self.send(true, generation)
    }

    fn reject(&self, generation: u64) -> Result<(), String> {
        self.send(false, generation)
    }

    fn send(&self, accepted: bool, generation: u64) -> Result<(), String> {
        let _ = (accepted, generation);
        match self {
            Self::None => Err("该终端没有命令确认通道".into()),
            #[cfg(windows)]
            Self::Events { ack, reject } => {
                signal_control_event(if accepted { ack } else { reject })
            }
            #[cfg(unix)]
            Self::Fifo(fifo) => fifo.send(accepted, generation),
            #[cfg(windows)]
            Self::ReplyFiles(files) => files.send(accepted, generation),
        }
    }
}

/// The shell a terminal runs, as the renderer's shell menu names it.
///
/// The renderer offers the shells the machine's probe found
/// ([`crate::machine_shells`]) that a terminal there can start: a Windows host
/// runs PowerShell and Git Bash; a Mac or Linux host zsh, bash and fish, never
/// `sh`, whose line editor cannot hold the Git mutex; a WSL distribution and an
/// SSH machine any of zsh, bash, fish and sh, and an SSH machine running
/// Windows PowerShell too. `None` wherever an `Option<TerminalShell>` is taken
/// means that machine's default: PowerShell on a Windows host, zsh everywhere
/// else.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalShell {
    PowerShell,
    Bash,
    Zsh,
    Fish,
    Sh,
}

impl TerminalShell {
    /// The name the shell is started by, and the label a tab shows for it.
    pub fn program_name(self) -> &'static str {
        match self {
            Self::PowerShell => "pwsh",
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Fish => "fish",
            Self::Sh => "sh",
        }
    }
}

pub struct TerminalLaunch {
    program: OsString,
    args: Vec<OsString>,
    cwd: PathBuf,
    display_cwd: String,
    shell: String,
    binding: String,
    control: ShellControl,
    /// For an SSH machine: the terminal the agent runs there. The program and
    /// arguments above are then the interactive `ssh` it falls back to when
    /// the agent does not serve the machine ([`crate::remote_terminal`]).
    agent: Option<crate::remote_terminal::AgentTerminalSpec>,
}

impl TerminalLaunch {
    /// A shell on this machine: `shell`, or the machine's default when `None`.
    /// A shell the machine does not have is refused rather than replaced, and
    /// so is one this host cannot run with the command barrier.
    pub fn host(cwd: &Path, shell: Option<TerminalShell>) -> Result<Self, String> {
        let cwd = canonical_directory(cwd)?;
        let (program, args, shell, control) = host_shell(shell)?;
        Ok(Self::new(
            program,
            args,
            cwd.clone(),
            cwd.to_string_lossy().into_owned(),
            shell,
            "host",
            control,
        ))
    }

    /// An interactive shell in `remote_cwd` on the machine `runner` dispatches
    /// to. `local_cwd` is only where the client process starts; it is the
    /// conversation's host-side anchor, never the directory the user sees.
    ///
    /// WSL is entered with `--cd` so the distribution applies its own path
    /// rules. SSH asks for a terminal (`-t`) and runs the user's login shell in
    /// the directory; unlike the tool leg it does not set `BatchMode`, because a
    /// person is sitting in front of this one and may answer a prompt. The
    /// machine's variable table is applied the way the tool leg applies it.
    ///
    /// A chosen `shell` is started as a login shell by `/bin/sh`, which falls
    /// back to the user's own login shell, with a note, on a machine without
    /// it. Over SSH the whole remote line reaches `/bin/sh` in
    /// [`crate::remote_shell::posix_line`] form, because the login shell reads
    /// it first and may be fish or tcsh, which read neither `${SHELL:-…}` nor
    /// `if … fi` nor POSIX quoting.
    /// A remote shell has no command barrier: there is no host checkout behind
    /// it to protect.
    ///
    /// On an SSH machine the launch also carries the same shell choice for the
    /// agent to run ([`crate::remote_terminal`]): a terminal owned by the
    /// machine's agent survives the network dropping, and the `ssh -t` above is
    /// what it falls back to where the agent does not serve the machine.
    pub fn remote(
        runner: &crate::run_environment::ShellRunner,
        remote_cwd: &str,
        local_cwd: &Path,
        shell: Option<TerminalShell>,
    ) -> Result<Self, String> {
        use crate::run_environment::{self as run_env, ShellRunner};
        let chosen = match shell {
            None => None,
            // PowerShell is what an SSH machine's agent starts on Windows when
            // nothing is chosen, and there is no POSIX login line for it.
            Some(TerminalShell::PowerShell) if matches!(runner, ShellRunner::Ssh { .. }) => None,
            Some(TerminalShell::PowerShell) => {
                return Err("WSL 终端不提供 PowerShell；请选择 zsh、bash、fish 或 sh".into());
            }
            Some(shell) => Some(shell.program_name()),
        };
        let local_cwd = canonical_directory(local_cwd)?;
        let env = runner.normalized_env()?;
        let injected: Vec<String> = env
            .iter()
            .filter(|(key, _)| !run_env::is_shell_startup_env_name(key))
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        let labelled = |machine: String| match chosen {
            Some(name) => format!("{machine} · {name}"),
            None => machine,
        };
        let (program, args, shell, scope) = match runner {
            ShellRunner::Local { .. } => {
                return Err("本机工作区的终端不经远端启动".into());
            }
            ShellRunner::Wsl { distro, .. } => {
                run_env::validate_wsl_distro_name(distro)?;
                let mut args = vec![
                    OsString::from("-d"),
                    OsString::from(distro),
                    OsString::from("--cd"),
                    OsString::from(remote_cwd),
                ];
                if let Some(name) = chosen {
                    args.push(OsString::from("--exec"));
                    if !injected.is_empty() {
                        args.push(OsString::from("/usr/bin/env"));
                        args.extend(injected.iter().map(OsString::from));
                    }
                    args.extend([
                        OsString::from("/bin/sh"),
                        OsString::from("-c"),
                        OsString::from(remote_shell_script(name)),
                    ]);
                } else if !injected.is_empty() {
                    args.extend([OsString::from("--exec"), OsString::from("/usr/bin/env")]);
                    args.extend(injected.iter().map(OsString::from));
                    args.extend([OsString::from("bash"), OsString::from("-l")]);
                }
                (
                    OsString::from("wsl.exe"),
                    args,
                    labelled(format!("WSL: {distro}")),
                    "wsl",
                )
            }
            ShellRunner::Ssh {
                host,
                port,
                identity_file,
                ..
            } => {
                let mut args = vec![
                    OsString::from("-t"),
                    OsString::from("-o"),
                    OsString::from("ConnectTimeout=10"),
                ];
                if *port != 0 {
                    args.extend([OsString::from("-p"), OsString::from(port.to_string())]);
                }
                if !identity_file.is_empty() {
                    args.extend([OsString::from("-i"), OsString::from(identity_file)]);
                }
                args.extend([OsString::from("--"), OsString::from(host)]);
                let mut remote = format!("cd {} && ", run_env::quote_remote_path(remote_cwd));
                if !injected.is_empty() {
                    remote.push_str("export ");
                    for pair in &injected {
                        remote.push_str(&run_env::sh_single_quote(pair));
                        remote.push(' ');
                    }
                    remote.push_str("&& ");
                }
                match chosen {
                    Some(name) => {
                        remote.push_str("exec /bin/sh -c ");
                        remote.push_str(&run_env::sh_single_quote(&remote_shell_script(name)));
                    }
                    None => remote.push_str(r#"exec "${SHELL:-/bin/sh}" -l"#),
                }
                args.push(OsString::from(crate::remote_shell::posix_line(&remote)));
                let program = run_env::ssh_client_candidates()
                    .into_iter()
                    .find(|candidate| candidate == "ssh" || Path::new(candidate).is_file())
                    .unwrap_or_else(|| "ssh".into());
                (
                    OsString::from(program),
                    args,
                    labelled(format!("SSH: {host}")),
                    "ssh",
                )
            }
        };
        let mut launch = Self::new(
            program,
            args,
            local_cwd,
            remote_cwd.to_owned(),
            shell,
            scope,
            ShellControl::Remote,
        );
        // On an SSH machine the agent runs the same shell choice itself, on a
        // pseudo terminal of its own that outlives any one connection. Its
        // variables come from the runner the way every agent process's do.
        if let ShellRunner::Ssh { host, .. } = runner {
            let script = match chosen {
                Some(name) => remote_shell_script(name),
                None => r#"exec "${SHELL:-/bin/sh}" -l"#.to_owned(),
            };
            launch.agent = Some(crate::remote_terminal::AgentTerminalSpec {
                runner: runner.clone(),
                argv: vec!["/bin/sh".into(), "-c".into(), script],
                windows_shell: chosen.map(str::to_owned),
                cwd: remote_cwd.to_owned(),
                env: vec![
                    ("TERM".into(), "xterm-256color".into()),
                    ("COLORTERM".into(), "truecolor".into()),
                ],
                machine_label: host.clone(),
            });
        }
        Ok(launch)
    }

    fn new(
        program: OsString,
        args: Vec<OsString>,
        cwd: PathBuf,
        display_cwd: String,
        shell: String,
        scope: &str,
        control: ShellControl,
    ) -> Self {
        let binding = std::iter::once(program.as_os_str())
            .chain(args.iter().map(OsString::as_os_str))
            .map(|part| part.to_string_lossy())
            .collect::<Vec<_>>()
            .join("\u{0}");
        let binding = format!("{scope}\u{0}{}\u{0}{binding}", cwd.to_string_lossy());
        Self {
            program,
            args,
            cwd,
            display_cwd,
            shell,
            binding,
            control,
            agent: None,
        }
    }
}

/// The `/bin/sh` script that starts `name` as a login shell on another
/// machine, or that user's own login shell when the machine lacks it. `name`
/// is one of [`TerminalShell::program_name`]'s, never text from the user, and
/// the script has neither a single quote nor a backslash, so it reads the same
/// to a POSIX shell and to fish once single-quoted.
fn remote_shell_script(name: &str) -> String {
    format!(
        "if command -v {name} >/dev/null 2>&1; then exec {name} -l; fi; \
         echo \"Mework：这台机器上没有 {name}，改用 ${{SHELL:-/bin/sh}}。\" >&2; \
         exec \"${{SHELL:-/bin/sh}}\" -l"
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ControlHandshake {
    Pending,
    Ready,
    Failed,
}

struct TerminalControlParser {
    prefix: Vec<u8>,
    pending: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TerminalControlFrameKind {
    Ready,
    Start,
    End,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TerminalControlFrame {
    kind: TerminalControlFrameKind,
    generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TerminalControlToken {
    Visible(Vec<u8>),
    Frame(TerminalControlFrame),
}

enum TerminalControlUpdate {
    Ready,
    CommandState(TerminalCommandState),
}

impl TerminalControlParser {
    fn new(nonce: &str) -> Self {
        let mut prefix = Vec::with_capacity(CONTROL_PREFIX.len() + nonce.len() + 1);
        prefix.extend_from_slice(CONTROL_PREFIX);
        prefix.extend_from_slice(nonce.as_bytes());
        prefix.push(b';');
        Self {
            prefix,
            pending: Vec::new(),
        }
    }

    fn filter(&mut self, data: &[u8]) -> Vec<TerminalControlToken> {
        self.pending.extend_from_slice(data);
        let mut tokens = Vec::new();
        let mut visible = Vec::with_capacity(self.pending.len());
        let mut cursor = 0;

        while cursor < self.pending.len() {
            let remaining = &self.pending[cursor..];
            if remaining[0] != self.prefix[0] {
                let next = remaining
                    .iter()
                    .position(|byte| *byte == self.prefix[0])
                    .unwrap_or(remaining.len());
                visible.extend_from_slice(&remaining[..next]);
                cursor += next;
                continue;
            }

            if remaining.len() < self.prefix.len() {
                if self.prefix.starts_with(remaining) {
                    break;
                }
                visible.push(remaining[0]);
                cursor += 1;
                continue;
            }
            if !remaining.starts_with(&self.prefix) {
                visible.push(remaining[0]);
                cursor += 1;
                continue;
            }

            let kind_start = cursor + self.prefix.len();
            let kind_bytes = &self.pending[kind_start..];
            let Some(kind_end) = kind_bytes.iter().take(6).position(|byte| *byte == b';') else {
                if kind_bytes.len() <= 5 {
                    break;
                }
                visible.push(self.pending[cursor]);
                cursor += 1;
                continue;
            };
            let kind = match &kind_bytes[..kind_end] {
                b"ready" => TerminalControlFrameKind::Ready,
                b"start" => TerminalControlFrameKind::Start,
                b"end" => TerminalControlFrameKind::End,
                _ => {
                    visible.push(self.pending[cursor]);
                    cursor += 1;
                    continue;
                }
            };
            let generation_start = kind_start + kind_end + 1;
            let generation_bytes = &self.pending[generation_start..];
            let Some(terminator_offset) = generation_bytes
                .iter()
                .take(21)
                .position(|byte| *byte == CONTROL_TERMINATOR)
            else {
                if generation_bytes.len() <= 20 {
                    break;
                }
                visible.push(self.pending[cursor]);
                cursor += 1;
                continue;
            };
            let generation = &generation_bytes[..terminator_offset];
            if generation.is_empty() || !generation.iter().all(u8::is_ascii_digit) {
                visible.push(self.pending[cursor]);
                cursor += 1;
                continue;
            }
            let Ok(generation) = std::str::from_utf8(generation)
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or(())
            else {
                visible.push(self.pending[cursor]);
                cursor += 1;
                continue;
            };
            if !visible.is_empty() {
                tokens.push(TerminalControlToken::Visible(std::mem::take(&mut visible)));
            }
            tokens.push(TerminalControlToken::Frame(TerminalControlFrame {
                kind,
                generation,
            }));
            cursor = generation_start + terminator_offset + 1;
        }

        self.pending.drain(..cursor);
        if !visible.is_empty() {
            tokens.push(TerminalControlToken::Visible(visible));
        }
        tokens
    }

    fn finish(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending)
    }
}

struct TerminalOutputState {
    buffer: VecDeque<u8>,
    sink: Option<Channel<TerminalEvent>>,
    running: bool,
    closed: bool,
    control: Option<TerminalControlParser>,
    control_ready: bool,
    last_control_generation: u64,
    active_control_generation: Option<u64>,
    command_state: TerminalCommandState,
    command_lease: Option<TerminalCommandLease>,
    startup_lease: Option<TerminalCommandLease>,
    command_lease_factory: TerminalCommandLeaseFactory,
    control_reply: ControlReply,
}

struct TerminalSession {
    session_id: String,
    binding: String,
    cwd: String,
    shell: String,
    master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>,
    writer: Arc<Mutex<Option<Box<dyn Write + Send>>>>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    process_id: Option<u32>,
    process_group_id: Option<i32>,
    output: Arc<Mutex<TerminalOutputState>>,
    control_handshake: Arc<(Mutex<ControlHandshake>, Condvar)>,
}

/// A terminal is addressed by the conversation that owns it and the id that
/// conversation chose for it. Ids are only unique within a conversation: the
/// composer drawer of every conversation uses the same one.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct TerminalKey {
    conversation_id: String,
    terminal_id: String,
}

impl TerminalKey {
    fn new(conversation_id: &str, terminal_id: &str) -> Self {
        Self {
            conversation_id: conversation_id.to_owned(),
            terminal_id: terminal_id.to_owned(),
        }
    }
}

impl TerminalSession {
    /// Ends the shell without holding up the caller, which for a closed tab is
    /// the UI thread. The sink stays attached so the waiter can still report
    /// the exit: a panel that did not ask for this close — the task list did,
    /// or a workspace closing — has no other way to learn its shell is gone.
    ///
    /// On Windows the tree is killed at once, as it always was.
    #[cfg(windows)]
    fn terminate(mut self) {
        self.stop_answering();
        self.kill_now();
    }

    /// Ends the shell without holding up the caller, which for a closed tab is
    /// the UI thread. The sink stays attached so the waiter can still report
    /// the exit: a panel that did not ask for this close — the task list did,
    /// or a workspace closing — has no other way to learn its shell is gone.
    ///
    /// The shell is asked to hang up first, the way closing a Terminal window
    /// asks it: zsh, bash and fish write `$HISTFILE` on SIGHUP, where SIGKILL
    /// lost every command typed in the tab. Whatever is still in the group
    /// after a short grace is killed as before, on a thread of its own.
    #[cfg(unix)]
    fn terminate(mut self) {
        self.stop_answering();
        match self.hang_up() {
            Ok(shell) => shell.finish_in_background(),
            Err(session) => session.kill_now(),
        }
    }

    /// The part of a close that is the same however the shell is then ended:
    /// nothing it prints is forwarded and nothing it asks is answered.
    fn stop_answering(&mut self) {
        {
            let mut output = lock(&self.output);
            output.closed = true;
            // The shell is about to die, and nothing it asks from here on will be
            // answered. Dropping the channel now removes its private directory
            // even when the process exits before the waiter thread finishes.
            output.control_reply = ControlReply::None;
        }
        mark_handshake(&self.control_handshake, ControlHandshake::Failed);
        if let Some(mut writer) = lock(&self.writer).take() {
            let _ = writer.flush();
        }
    }

    /// Kills the shell's tree now and closes the pseudo console.
    fn kill_now(mut self) {
        if !kill_process_tree(self.process_id, self.process_group_id) {
            if let Err(error) = self.killer.kill() {
                eprintln!("关闭终端 {} 的 shell 失败：{error}", self.session_id);
            }
        }
        // Closing the pseudo console after terminating the shell wakes the blocking reader. The
        // waiter thread owns and reaps the child handle.
        drop(lock(&self.master).take());
    }

    /// Sends SIGHUP to the shell's process group. The session comes back when
    /// there is no group to signal, for the caller to kill it the old way.
    #[cfg(unix)]
    fn hang_up(self) -> Result<HangingUpShell, Self> {
        match hang_up_process_group(self.process_group_id) {
            Some(process_group_id) => Ok(HangingUpShell {
                process_group_id,
                master: self.master.clone(),
            }),
            None => Err(self),
        }
    }

    /// Ends every shell in `sessions` before returning, for teardown, where a
    /// thread left to finish the job would not outlive the process. They are
    /// all asked to hang up at once and share one grace period, so closing many
    /// costs no longer than closing one.
    fn terminate_before_returning(sessions: impl IntoIterator<Item = Self>) {
        #[cfg(windows)]
        sessions.into_iter().for_each(Self::terminate);
        #[cfg(unix)]
        {
            let hanging_up = sessions
                .into_iter()
                .filter_map(|mut session| {
                    session.stop_answering();
                    match session.hang_up() {
                        Ok(shell) => Some(shell),
                        Err(session) => {
                            session.kill_now();
                            None
                        }
                    }
                })
                .collect::<Vec<_>>();
            let deadline = std::time::Instant::now() + SHELL_HANGUP_GRACE;
            for shell in hanging_up {
                shell.finish_by(deadline);
            }
        }
    }
}

/// How long a closed shell has to exit on its own after SIGHUP — ample for
/// zsh, bash or fish to write its history file — before its group is killed.
#[cfg(unix)]
const SHELL_HANGUP_GRACE: Duration = Duration::from_millis(300);
#[cfg(unix)]
const SHELL_HANGUP_POLL: Duration = Duration::from_millis(10);

/// A closed shell that has been sent SIGHUP and is being given a moment to
/// exit before its process group is killed.
#[cfg(unix)]
struct HangingUpShell {
    process_group_id: i32,
    master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>,
}

#[cfg(unix)]
impl HangingUpShell {
    /// Waits out the grace on a thread of its own, so the close that asked for
    /// it returns at once.
    fn finish_in_background(self) {
        let process_group_id = self.process_group_id;
        let master = self.master.clone();
        let spawned = thread::Builder::new()
            .name(format!("terminal-hangup-{process_group_id}"))
            .spawn(move || self.finish_by(std::time::Instant::now() + SHELL_HANGUP_GRACE));
        if spawned.is_err() {
            // With nothing to wait on the shell, end it the way a close always
            // did rather than block the caller for the grace.
            kill_process_tree(None, Some(process_group_id));
            drop(lock(&master).take());
        }
    }

    /// Waits until the group is empty or `deadline` passes, then kills
    /// whatever is left so nothing in it outlives the terminal.
    fn finish_by(self, deadline: std::time::Instant) {
        while process_group_exists(self.process_group_id) {
            if std::time::Instant::now() >= deadline {
                kill_process_tree(None, Some(self.process_group_id));
                break;
            }
            thread::sleep(SHELL_HANGUP_POLL);
        }
        // The pseudo console stays open until now so the reader drains what the
        // shell prints on its way out instead of the shell writing into a
        // hung-up terminal. The waiter thread owns and reaps the child handle,
        // and usually has closed this already.
        drop(lock(&self.master).take());
    }
}

fn abort_unmanaged_terminal(
    child: &mut (dyn PtyChild + Send + Sync),
    killer: &mut (dyn ChildKiller + Send + Sync),
    process_id: Option<u32>,
    process_group_id: Option<i32>,
    output: &Arc<Mutex<TerminalOutputState>>,
    writer: &Arc<Mutex<Option<Box<dyn Write + Send>>>>,
    master: &Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>,
) -> Result<(), String> {
    {
        let mut state = lock(output);
        state.closed = true;
        state.sink = None;
    }
    let terminated = if kill_process_tree(process_id, process_group_id) {
        true
    } else {
        match killer.kill() {
            Ok(()) => true,
            Err(error) => {
                if let Some(mut writer) = lock(writer).take() {
                    let _ = writer.flush();
                }
                drop(lock(master).take());
                // There is no confirmed process exit, so intentionally retain
                // one Arc containing the startup/command lease.
                std::mem::forget(output.clone());
                return Err(format!(
                    "无法终止 shell：{error}；工作区租约将保持到应用重启"
                ));
            }
        }
    };
    if !terminated {
        std::mem::forget(output.clone());
        return Err("未能请求终止 shell；工作区租约将保持到应用重启".into());
    }

    let wait_result = child.wait();
    if let Some(mut writer) = lock(writer).take() {
        let _ = writer.flush();
    }
    drop(lock(master).take());
    match wait_result {
        Ok(_) => {
            let mut state = lock(output);
            state.running = false;
            state.active_control_generation = None;
            state.command_state.status = TerminalCommandStatus::Idle;
            state.command_state.command_id = None;
            drop(state.command_lease.take());
            drop(state.startup_lease.take());
            Ok(())
        }
        Err(error) => {
            // Releasing without a confirmed child exit would reopen the exact
            // Git/terminal race this integration is designed to prevent.
            std::mem::forget(output.clone());
            Err(format!(
                "等待 shell 退出失败：{error}；工作区租约将保持到应用重启"
            ))
        }
    }
}

fn wait_for_confirmed_terminal_exit(
    child: &mut (dyn PtyChild + Send + Sync),
    process_id: Option<u32>,
    process_group_id: Option<i32>,
) -> Result<u32, String> {
    match child.wait() {
        Ok(status) => Ok(status.exit_code()),
        Err(first_error) => {
            let killed = kill_process_tree(process_id, process_group_id) || child.kill().is_ok();
            if !killed {
                return Err(format!(
                    "等待 shell 退出失败：{first_error}；随后也无法终止该进程"
                ));
            }
            child.wait().map(|status| status.exit_code()).map_err(
                |second_error| {
                    format!(
                        "等待 shell 退出失败：{first_error}；终止进程后再次等待仍失败：{second_error}"
                    )
                },
            )
        }
    }
}

#[derive(Default)]
pub struct TerminalManager {
    sessions: Mutex<HashMap<TerminalKey, TerminalSession>>,
    /// Terminal owners that are drafts rather than conversations, each mapped to
    /// the id of the project (workspace row) its shells were opened in.
    ///
    /// A draft is the renderer's new task before it has a row in the document.
    /// Its shells are opened under the id it will materialize as, so becoming a
    /// real conversation hands them over without moving anything; until then no
    /// conversation in the document owns them, and this map is what tells a save
    /// they are not strays. Always locked after `sessions`.
    drafts: Mutex<HashMap<String, String>>,
}

/// One terminal as the task tools see it. Read-only: `task_wait` and
/// `task_list` observe terminals, they never write to one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalTaskSnapshot {
    pub terminal_id: String,
    pub cwd: String,
    pub shell: String,
    /// The shell process is alive. A closed terminal is dropped from the map,
    /// so this is false only for the brief window before cleanup.
    pub alive: bool,
    /// A command is executing right now, as reported by the control frames the
    /// PowerShell bootstrap emits. This is what `task_wait` waits to clear.
    pub busy: bool,
    pub command_count: u64,
}

impl TerminalManager {
    /// Every live terminal of one conversation, ordered by id so two calls in a
    /// row cannot reshuffle the list under the model.
    pub fn task_snapshots(&self, conversation_id: &str) -> Vec<TerminalTaskSnapshot> {
        let sessions = lock(&self.sessions);
        let mut snapshots = sessions
            .iter()
            .filter(|(key, _)| key.conversation_id == conversation_id)
            .map(|(key, session)| {
                let output = lock(&session.output);
                TerminalTaskSnapshot {
                    terminal_id: key.terminal_id.clone(),
                    cwd: session.cwd.clone(),
                    shell: session.shell.clone(),
                    alive: output.running && !output.closed,
                    busy: output.command_state.status == TerminalCommandStatus::Running,
                    command_count: output.command_state.command_count,
                }
            })
            .collect::<Vec<_>>();
        snapshots.sort_by(|left, right| left.terminal_id.cmp(&right.terminal_id));
        snapshots
    }

    pub fn task_snapshot(
        &self,
        conversation_id: &str,
        terminal_id: &str,
    ) -> Option<TerminalTaskSnapshot> {
        self.task_snapshots(conversation_id)
            .into_iter()
            .find(|snapshot| snapshot.terminal_id == terminal_id)
    }

    pub fn open(
        &self,
        conversation_id: &str,
        terminal_id: &str,
        launch: TerminalLaunch,
        cols: u16,
        rows: u16,
        sink: Channel<TerminalEvent>,
        startup_lease: TerminalCommandLease,
        command_lease_factory: TerminalCommandLeaseFactory,
    ) -> Result<TerminalOpenResponse, String> {
        validate_conversation_id(conversation_id)?;
        validate_terminal_id(terminal_id)?;
        let key = TerminalKey::new(conversation_id, terminal_id);
        let size = terminal_size(cols, rows);
        let mut sessions = lock(&self.sessions);

        let reusable = sessions.get(&key).is_some_and(|session| {
            session.binding == launch.binding && lock(&session.output).running
        });
        if reusable {
            let session = sessions
                .get_mut(&key)
                .expect("reusable terminal session disappeared while locked");
            lock(&session.master)
                .as_ref()
                .ok_or_else(|| "终端 shell 已退出".to_owned())?
                .resize(size)
                .map_err(|error| format!("无法调整终端尺寸：{error}"))?;
            let mut output = lock(&session.output);
            output.sink = Some(sink);
            return Ok(TerminalOpenResponse {
                created: false,
                running: output.running,
                ready: output.control_ready,
                session_id: session.session_id.clone(),
                snapshot: output.buffer.iter().copied().collect(),
                cwd: session.cwd.clone(),
                shell: session.shell.clone(),
                command_state: output.command_state.clone(),
            });
        }

        if let Some(stale) = sessions.remove(&key) {
            stale.terminate();
        }
        if sessions.len() >= MAX_TERMINAL_SESSIONS {
            return Err(format!(
                "同时运行的终端不能超过 {MAX_TERMINAL_SESSIONS} 个，请先关闭不再使用的终端"
            ));
        }

        let remote = launch.control == ShellControl::Remote;
        let control_nonce = Uuid::new_v4().simple().to_string();
        let mut launch_args = launch.args.clone();
        // The startup files live in the session's private directory, so the
        // arguments naming them are added here rather than in the launch, whose
        // binding must stay the same from one open to the next.
        let (control_reply, control_env, leading_args): ControlSetup = match launch.control {
            ShellControl::Remote => (ControlReply::None, Vec::new(), Vec::new()),
            ShellControl::Unsupported => return Err(UNSUPPORTED_SHELL_MESSAGE.into()),
            #[cfg(windows)]
            ShellControl::PowerShell => {
                configure_powershell_control(&mut launch_args);
                let ack = format!("MeworkTerminalAck_{}", Uuid::new_v4().simple());
                let reject = format!("MeworkTerminalReject_{}", Uuid::new_v4().simple());
                let environment = vec![
                    (
                        "MEWORK_TERMINAL_CONTROL_NONCE",
                        OsString::from(&control_nonce),
                    ),
                    ("MEWORK_TERMINAL_ACK_EVENT", OsString::from(&ack)),
                    ("MEWORK_TERMINAL_REJECT_EVENT", OsString::from(&reject)),
                ];
                (
                    ControlReply::Events { ack, reject },
                    environment,
                    Vec::new(),
                )
            }
            #[cfg(unix)]
            ShellControl::Zsh => fifo_control(posix_control::PosixShell::Zsh, &control_nonce)?,
            #[cfg(unix)]
            ShellControl::Bash => fifo_control(posix_control::PosixShell::Bash, &control_nonce)?,
            #[cfg(unix)]
            ShellControl::Fish => fifo_control(posix_control::PosixShell::Fish, &control_nonce)?,
            #[cfg(windows)]
            ShellControl::Bash => {
                let files = reply_files::ControlReplyFiles::create()?;
                let mut environment = files.environment(&control_nonce);
                // Unlike PowerShell, Git Bash is a terminal program: without a
                // terminal type readline assumes one that cannot clear a line,
                // and every `bind -x` hook would leave a copy of the line behind.
                environment.push(("TERM", OsString::from("xterm-256color")));
                // An MSYS2 profile changes to $HOME unless told the shell was
                // started where it should stay; Git's own ignores it.
                environment.push(("CHERE_INVOKING", OsString::from("1")));
                let leading_args = files.leading_args();
                (ControlReply::ReplyFiles(files), environment, leading_args)
            }
        };
        launch_args.splice(0..0, leading_args);
        let mut command = CommandBuilder::new(&launch.program);
        command.args(&launch_args);
        command.cwd(&launch.cwd);
        // `CommandBuilder::new` copies this process's whole environment, so the
        // development harnesses' addresses and bearer tokens would otherwise be
        // readable from the user's own shell.
        for name in crate::child_environment::private_child_environment_names() {
            command.env_remove(&name);
        }
        // A Windows shell inherits neither of these from a real terminal, and
        // programs that branch on them would behave differently here than in the
        // user's own console. Removing rather than merely not setting them
        // matters: the value may already be in the inherited block, as it is
        // whenever the app itself was started from a Unix-style shell.
        #[cfg(windows)]
        for name in ["TERM", "COLORTERM"] {
            command.env_remove(name);
        }
        #[cfg(not(windows))]
        {
            command.env("TERM", "xterm-256color");
            command.env("COLORTERM", "truecolor");
            // Inherited from whatever terminal started the app. macOS's
            // `/etc/zshrc` sources `/etc/zshrc_$TERM_PROGRAM`, and Apple
            // Terminal's copy installs session-restore hooks keyed on a session
            // this shell does not belong to.
            for name in ["TERM_PROGRAM", "TERM_PROGRAM_VERSION", "TERM_SESSION_ID"] {
                command.env_remove(name);
            }
        }
        for (name, value) in &control_env {
            command.env(name, value);
        }
        let (pty_master, mut reader, writer, mut child) = match launch.agent.clone() {
            // An SSH machine: the agent's terminal when it serves the machine,
            // and this very command in a local pseudo terminal when it does not.
            // Either attaches behind a front that answers at once, because
            // reaching the agent may first mean installing it.
            Some(agent) => {
                let (master, child) = crate::remote_terminal::start(agent, command, size);
                let reader = master
                    .try_clone_reader()
                    .map_err(|error| format!("无法读取伪终端：{error}"))?;
                let writer = master
                    .take_writer()
                    .map_err(|error| format!("无法写入伪终端：{error}"))?;
                (master, reader, writer, child)
            }
            None => {
                let pair = native_pty_system()
                    .openpty(size)
                    .map_err(|error| format!("无法创建伪终端：{error}"))?;
                let reader = pair
                    .master
                    .try_clone_reader()
                    .map_err(|error| format!("无法读取伪终端：{error}"))?;
                let writer = pair
                    .master
                    .take_writer()
                    .map_err(|error| format!("无法写入伪终端：{error}"))?;
                let child = pair
                    .slave
                    .spawn_command(command)
                    .map_err(|error| format!("无法启动终端 shell：{error}"))?;
                drop(pair.slave);
                (pair.master, reader, writer, child)
            }
        };

        let session_id = Uuid::new_v4().to_string();
        // A remote shell sends no frames, so it is ready the moment it starts:
        // there is no handshake for the watchdog to wait on and no startup
        // lease to hold, because nothing on this machine is being edited.
        let control_handshake = Arc::new((
            Mutex::new(if remote {
                ControlHandshake::Ready
            } else {
                ControlHandshake::Pending
            }),
            Condvar::new(),
        ));
        let output = Arc::new(Mutex::new(TerminalOutputState {
            buffer: VecDeque::new(),
            sink: Some(sink),
            running: true,
            closed: false,
            control: (!remote).then(|| TerminalControlParser::new(&control_nonce)),
            control_ready: remote,
            last_control_generation: 0,
            active_control_generation: None,
            command_state: TerminalCommandState::default(),
            command_lease: None,
            startup_lease: Some(startup_lease),
            command_lease_factory,
            control_reply,
        }));
        if remote {
            lock(&output).startup_lease = None;
        }
        let process_id = child.process_id();
        #[cfg(unix)]
        let process_group_id = pty_master.process_group_leader();
        #[cfg(not(unix))]
        let process_group_id = None;
        let mut killer = child.clone_killer();
        let master = Arc::new(Mutex::new(Some(pty_master)));
        let writer = Arc::new(Mutex::new(Some(writer)));
        let reader_output = output.clone();
        let reader_session_id = session_id.clone();
        let reader_handshake = control_handshake.clone();
        let reader_thread = thread::Builder::new()
            .name(format!("terminal-reader-{terminal_id}"))
            .spawn(move || {
                let mut chunk = [0_u8; 8192];
                loop {
                    match reader.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(count) => {
                            let (events, sink) = {
                                let mut state = lock(&reader_output);
                                if state.closed {
                                    break;
                                }
                                let tokens = state.control.as_mut().map_or_else(
                                    || vec![TerminalControlToken::Visible(chunk[..count].to_vec())],
                                    |control| control.filter(&chunk[..count]),
                                );
                                let mut events = Vec::with_capacity(tokens.len());
                                for token in tokens {
                                    match token {
                                        TerminalControlToken::Visible(data) => {
                                            append_buffer(&mut state.buffer, &data);
                                            events.push(TerminalEvent::Output {
                                                session_id: reader_session_id.clone(),
                                                data,
                                            });
                                        }
                                        TerminalControlToken::Frame(frame) => {
                                            if let Some(update) = apply_control_frame(
                                                &mut state,
                                                &reader_handshake,
                                                frame,
                                            ) {
                                                events.push(match update {
                                                    TerminalControlUpdate::Ready => {
                                                        TerminalEvent::Ready {
                                                            session_id: reader_session_id.clone(),
                                                        }
                                                    }
                                                    TerminalControlUpdate::CommandState(
                                                        command_state,
                                                    ) => TerminalEvent::CommandState {
                                                        session_id: reader_session_id.clone(),
                                                        command_state,
                                                    },
                                                });
                                            }
                                        }
                                    }
                                }
                                (events, state.sink.clone())
                            };
                            if let Some(sink) = sink {
                                for event in events {
                                    let _ = sink.send(event);
                                }
                            }
                        }
                        Err(error) => {
                            let sink = {
                                let state = lock(&reader_output);
                                (!state.closed).then(|| state.sink.clone()).flatten()
                            };
                            if let Some(sink) = sink {
                                let _ = sink.send(TerminalEvent::Error {
                                    session_id: reader_session_id.clone(),
                                    message: format!("读取终端输出失败：{error}"),
                                });
                            }
                            break;
                        }
                    }
                }
                let trailing = {
                    let mut state = lock(&reader_output);
                    let trailing = state
                        .control
                        .as_mut()
                        .map(TerminalControlParser::finish)
                        .unwrap_or_default();
                    if !state.closed {
                        append_buffer(&mut state.buffer, &trailing);
                    }
                    let sink = (!state.closed).then(|| state.sink.clone()).flatten();
                    (trailing, sink)
                };
                if !trailing.0.is_empty() {
                    if let Some(sink) = trailing.1 {
                        let _ = sink.send(TerminalEvent::Output {
                            session_id: reader_session_id,
                            data: trailing.0,
                        });
                    }
                }
            });
        let reader_thread = match reader_thread {
            Ok(reader_thread) => reader_thread,
            Err(error) => {
                mark_handshake(&control_handshake, ControlHandshake::Failed);
                let cleanup_error = abort_unmanaged_terminal(
                    child.as_mut(),
                    killer.as_mut(),
                    process_id,
                    process_group_id,
                    &output,
                    &writer,
                    &master,
                )
                .err();
                return Err(match cleanup_error {
                    Some(cleanup_error) => format!(
                        "无法启动终端输出线程：{error}；清理未托管 shell 时失败：{cleanup_error}"
                    ),
                    None => format!("无法启动终端输出线程：{error}"),
                });
            }
        };

        let wait_output = output.clone();
        let wait_session_id = session_id.clone();
        let wait_master = master.clone();
        let wait_writer = writer.clone();
        let wait_handshake = control_handshake.clone();
        let wait_process_id = process_id;
        let wait_process_group_id = process_group_id;
        // Keep ownership recoverable until the waiter thread is known to have
        // started. If thread creation fails, the open path can still kill and
        // reap the child before allowing the startup lease to disappear.
        let wait_child = Arc::new(Mutex::new(Some(child)));
        let wait_reader = Arc::new(Mutex::new(Some(reader_thread)));
        let waiter_child = wait_child.clone();
        let waiter_reader = wait_reader.clone();
        let waiter = thread::Builder::new()
            .name(format!("terminal-wait-{terminal_id}"))
            .spawn(move || {
                let mut child = lock(&waiter_child)
                    .take()
                    .expect("terminal child must be owned by exactly one waiter");
                let status = wait_for_confirmed_terminal_exit(
                    child.as_mut(),
                    wait_process_id,
                    wait_process_group_id,
                );
                // ConPTY keeps its output pipe alive until both the input writer and pseudo
                // console are closed. Release them only after the child has stopped so the
                // reader can drain its final output and then observe EOF.
                if let Some(mut writer) = lock(&wait_writer).take() {
                    let _ = writer.flush();
                }
                drop(lock(&wait_master).take());
                if let Some(reader_thread) = lock(&waiter_reader).take() {
                    let _ = reader_thread.join();
                }
                mark_handshake(&wait_handshake, ControlHandshake::Failed);
                let (sink, command_state, exit_code, error) = {
                    let mut state = lock(&wait_output);
                    state.running = false;
                    state.control_reply = ControlReply::None;
                    let command_state = status.as_ref().ok().and_then(|_| {
                        drop(state.startup_lease.take());
                        (state.command_state.status == TerminalCommandStatus::Running).then(|| {
                            state.active_control_generation = None;
                            state.command_state.revision =
                                state.command_state.revision.saturating_add(1);
                            state.command_state.status = TerminalCommandStatus::Idle;
                            state.command_state.command_id = None;
                            drop(state.command_lease.take());
                            state.command_state.clone()
                        })
                    });
                    // A closed session still reports its exit: `closed` only
                    // silences output, which is noise once the kill is on its way.
                    match status.as_ref() {
                        Ok(exit_code) => {
                            (state.sink.clone(), command_state, Some(*exit_code), None)
                        }
                        Err(error) => {
                            (state.sink.clone(), command_state, None, Some(error.clone()))
                        }
                    }
                };
                if status.is_err() {
                    // No future owner can prove process exit after the waiter
                    // gives up. Retain one output Arc so its workspace lease is
                    // fail-closed until application restart.
                    std::mem::forget(wait_output.clone());
                }
                if let Some(sink) = sink {
                    if let Some(state) = command_state {
                        let _ = sink.send(TerminalEvent::CommandState {
                            session_id: wait_session_id.clone(),
                            command_state: state,
                        });
                    }
                    let event = error.map_or_else(
                        || TerminalEvent::Exit {
                            session_id: wait_session_id.clone(),
                            exit_code,
                        },
                        |message| TerminalEvent::Error {
                            session_id: wait_session_id.clone(),
                            message,
                        },
                    );
                    let _ = sink.send(event);
                }
            });
        if let Err(error) = waiter {
            mark_handshake(&control_handshake, ControlHandshake::Failed);
            let cleanup_error = if let Some(mut child) = lock(&wait_child).take() {
                abort_unmanaged_terminal(
                    child.as_mut(),
                    killer.as_mut(),
                    process_id,
                    process_group_id,
                    &output,
                    &writer,
                    &master,
                )
                .err()
            } else {
                Some("终端子进程句柄在回收线程启动失败后丢失".into())
            };
            if let Some(reader_thread) = lock(&wait_reader).take() {
                let _ = reader_thread.join();
            }
            return Err(match cleanup_error {
                Some(cleanup_error) => format!(
                    "无法启动终端回收线程：{error}；清理未托管 shell 时失败：{cleanup_error}"
                ),
                None => format!("无法启动终端回收线程：{error}"),
            });
        }

        let watchdog_output = output.clone();
        let watchdog_handshake = control_handshake.clone();
        let watchdog_session_id = session_id.clone();
        let watchdog_process_id = process_id;
        let watchdog_process_group_id = process_group_id;
        let mut watchdog_killer = killer.clone_killer();
        let watchdog = thread::Builder::new()
            .name(format!("terminal-ready-{terminal_id}"))
            .spawn(move || {
                if wait_for_control_handshake(
                    &watchdog_handshake,
                    CONTROL_HANDSHAKE_TIMEOUT,
                ) != ControlHandshake::Pending
                {
                    return;
                }
                // Claim the timeout while holding the handshake mutex. If
                // Ready won the boundary race, do not kill a valid shell.
                if !mark_handshake(&watchdog_handshake, ControlHandshake::Failed) {
                    return;
                }
                let sink = {
                    let state = lock(&watchdog_output);
                    (state.running && !state.closed)
                        .then(|| state.sink.clone())
                        .flatten()
                };
                if let Some(sink) = sink.as_ref() {
                    let _ = sink.send(TerminalEvent::Error {
                        session_id: watchdog_session_id.clone(),
                        message: "终端 shell 未完成可信命令状态初始化；为避免 Git 与终端命令并发，已关闭该终端".into(),
                    });
                }
                let killed = kill_process_tree(
                    watchdog_process_id,
                    watchdog_process_group_id,
                ) || watchdog_killer.kill().is_ok();
                if !killed {
                    if let Some(sink) = sink {
                        let _ = sink.send(TerminalEvent::Error {
                            session_id: watchdog_session_id,
                            message: "无法终止未完成初始化的终端 shell；工作区租约会保持到进程实际退出".into(),
                        });
                    }
                }
            });
        if let Err(error) = watchdog {
            mark_handshake(&control_handshake, ControlHandshake::Failed);
            {
                let mut state = lock(&output);
                state.closed = true;
                state.sink = None;
            }
            if !kill_process_tree(process_id, process_group_id) {
                let _ = killer.kill();
            }
            return Err(format!("无法启动终端初始化监控线程：{error}"));
        }

        sessions.insert(
            key.clone(),
            TerminalSession {
                session_id: session_id.clone(),
                binding: launch.binding,
                cwd: launch.display_cwd.clone(),
                shell: launch.shell.clone(),
                master,
                writer,
                killer,
                process_id,
                process_group_id,
                output,
                control_handshake: control_handshake.clone(),
            },
        );

        let (ready, command_state) = sessions
            .get(&key)
            .map(|session| {
                let output = lock(&session.output);
                (output.control_ready, output.command_state.clone())
            })
            .unwrap_or_default();

        Ok(TerminalOpenResponse {
            created: true,
            running: true,
            ready,
            session_id,
            snapshot: Vec::new(),
            cwd: launch.display_cwd,
            shell: launch.shell,
            command_state,
        })
    }

    pub fn write(
        &self,
        conversation_id: &str,
        terminal_id: &str,
        session_id: &str,
        data: &str,
    ) -> Result<(), String> {
        if data.len() > MAX_INPUT_BYTES {
            return Err(format!("单次终端输入不能超过 {MAX_INPUT_BYTES} 字节"));
        }
        let writer = {
            let mut sessions = lock(&self.sessions);
            let session =
                matching_session_mut(&mut sessions, conversation_id, terminal_id, session_id)?;
            if !lock(&session.output).running {
                return Err("终端 shell 已退出".into());
            }
            session.writer.clone()
        };
        let mut writer = lock(&writer);
        let writer = writer
            .as_mut()
            .ok_or_else(|| "终端 shell 已退出".to_owned())?;
        writer
            .write_all(data.as_bytes())
            .and_then(|_| writer.flush())
            .map_err(|error| format!("写入终端失败：{error}"))
    }

    pub fn resize(
        &self,
        conversation_id: &str,
        terminal_id: &str,
        session_id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<(), String> {
        let mut sessions = lock(&self.sessions);
        let session =
            matching_session_mut(&mut sessions, conversation_id, terminal_id, session_id)?;
        let result = lock(&session.master)
            .as_ref()
            .ok_or_else(|| "终端 shell 已退出".to_owned())?
            .resize(terminal_size(cols, rows))
            .map_err(|error| format!("无法调整终端尺寸：{error}"));
        result
    }

    pub fn detach(&self, conversation_id: &str, terminal_id: &str, session_id: &str) -> bool {
        let sessions = lock(&self.sessions);
        let Some(session) = sessions.get(&TerminalKey::new(conversation_id, terminal_id)) else {
            return false;
        };
        if session.session_id != session_id {
            return false;
        }
        lock(&session.output).sink = None;
        true
    }

    pub fn close(&self, conversation_id: &str, terminal_id: &str) -> bool {
        let session = {
            let mut sessions = lock(&self.sessions);
            let session = sessions.remove(&TerminalKey::new(conversation_id, terminal_id));
            forget_idle_draft(&sessions, &mut lock(&self.drafts), conversation_id);
            session
        };
        if let Some(session) = session {
            session.terminate();
            true
        } else {
            false
        }
    }

    pub fn close_missing<'a>(&self, retained: impl IntoIterator<Item = &'a str>) {
        let retained = retained
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        let removed = {
            let mut sessions = lock(&self.sessions);
            lock(&self.drafts).retain(|owner, _| retained.contains(owner.as_str()));
            remove_matching_sessions(&mut sessions, |key| {
                !retained.contains(key.conversation_id.as_str())
            })
        };
        removed.into_iter().for_each(TerminalSession::terminate);
    }

    pub fn close_conversations<'a>(&self, conversations: impl IntoIterator<Item = &'a str>) {
        let conversations = conversations.into_iter().collect::<HashSet<_>>();
        if conversations.is_empty() {
            return;
        }
        let removed = {
            let mut sessions = lock(&self.sessions);
            lock(&self.drafts).retain(|owner, _| !conversations.contains(owner.as_str()));
            remove_matching_sessions(&mut sessions, |key| {
                conversations.contains(key.conversation_id.as_str())
            })
        };
        removed.into_iter().for_each(TerminalSession::terminate);
    }

    pub fn close_all(&self) {
        let removed = {
            let mut sessions = lock(&self.sessions);
            lock(&self.drafts).clear();
            sessions
                .drain()
                .map(|(_, session)| session)
                .collect::<Vec<_>>()
        };
        TerminalSession::terminate_before_returning(removed);
    }

    /// Records that `owner` — the id a renderer draft will materialize as — is
    /// opening a shell in the project `workspace_id`.
    ///
    /// Call it before `open`, under the same storage lock the launch was
    /// resolved under, so no save can see the shell before it sees the binding.
    /// A draft aimed somewhere else now is a different task as far as its shells
    /// are concerned: the ones it opened for the other project are closed here
    /// rather than handed to this one.
    pub fn bind_draft(&self, owner: &str, workspace_id: &str) {
        let removed = {
            let mut sessions = lock(&self.sessions);
            let mut drafts = lock(&self.drafts);
            let removed = match drafts.get(owner) {
                Some(bound) if bound != workspace_id => {
                    remove_matching_sessions(&mut sessions, |key| key.conversation_id == owner)
                }
                _ => Vec::new(),
            };
            drafts.insert(owner.to_owned(), workspace_id.to_owned());
            removed
        };
        removed.into_iter().for_each(TerminalSession::terminate);
    }

    /// Every draft that has a shell open, mapped to its project.
    pub fn draft_bindings(&self) -> HashMap<String, String> {
        lock(&self.drafts).clone()
    }

    /// Stops tracking `owner` as a draft when it has no shell left, as after an
    /// open that failed before its shell existed.
    pub fn release_idle_draft(&self, owner: &str) {
        let sessions = lock(&self.sessions);
        forget_idle_draft(&sessions, &mut lock(&self.drafts), owner);
    }

    /// Brings the draft bindings up to date with a document that is about to
    /// become the authority, and returns the ones still standing.
    ///
    /// An owner the document now holds as a conversation has materialized: from
    /// here on its conversation's binding governs its shells, so it stops being
    /// a draft. An owner whose project `project_gone` reports removed, or moved
    /// to another kind or path, loses its shells with it — the same rule a
    /// conversation's shells follow.
    pub fn settle_drafts(
        &self,
        is_conversation: impl Fn(&str) -> bool,
        project_gone: impl Fn(&str) -> bool,
    ) -> HashMap<String, String> {
        let (removed, standing) = {
            let mut sessions = lock(&self.sessions);
            let mut drafts = lock(&self.drafts);
            drafts.retain(|owner, _| !is_conversation(owner));
            let gone = drafts
                .iter()
                .filter(|(_, workspace_id)| project_gone(workspace_id))
                .map(|(owner, _)| owner.clone())
                .collect::<HashSet<_>>();
            drafts.retain(|owner, _| !gone.contains(owner));
            let removed =
                remove_matching_sessions(&mut sessions, |key| gone.contains(&key.conversation_id));
            (removed, drafts.clone())
        };
        removed.into_iter().for_each(TerminalSession::terminate);
        standing
    }
}

/// Drops `owner`'s draft binding once none of its shells is left.
fn forget_idle_draft<T>(
    sessions: &HashMap<TerminalKey, T>,
    drafts: &mut HashMap<String, String>,
    owner: &str,
) {
    if !sessions.keys().any(|key| key.conversation_id == owner) {
        drafts.remove(owner);
    }
}

/// How a host shell's command barrier is wired up: the reply channel, the
/// variables its startup reads (and removes) before any user file runs, and the
/// arguments that go ahead of its own to point it at its startup files.
type ControlSetup = (ControlReply, Vec<(&'static str, OsString)>, Vec<OsString>);

/// The FIFO route of a zsh, bash or fish host terminal.
#[cfg(unix)]
fn fifo_control(shell: posix_control::PosixShell, nonce: &str) -> Result<ControlSetup, String> {
    let fifo = posix_control::ControlFifo::create(shell, nonce)?;
    let environment = fifo.environment();
    let leading_args = fifo.leading_args();
    Ok((ControlReply::Fifo(fifo), environment, leading_args))
}

fn remove_matching_sessions<K: Clone + Eq + std::hash::Hash, T>(
    sessions: &mut HashMap<K, T>,
    mut should_remove: impl FnMut(&K) -> bool,
) -> Vec<T> {
    let removed_keys = sessions
        .keys()
        .filter(|key| should_remove(key))
        .cloned()
        .collect::<Vec<_>>();
    removed_keys
        .into_iter()
        .filter_map(|key| sessions.remove(&key))
        .collect()
}

impl Drop for TerminalManager {
    fn drop(&mut self) {
        let sessions = self
            .sessions
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        TerminalSession::terminate_before_returning(sessions.drain().map(|(_, session)| session));
    }
}

fn matching_session_mut<'a>(
    sessions: &'a mut HashMap<TerminalKey, TerminalSession>,
    conversation_id: &str,
    terminal_id: &str,
    session_id: &str,
) -> Result<&'a mut TerminalSession, String> {
    let session = sessions
        .get_mut(&TerminalKey::new(conversation_id, terminal_id))
        .ok_or_else(|| "终端会话不存在".to_owned())?;
    if session.session_id != session_id {
        return Err("终端会话已更新，请使用当前会话".into());
    }
    Ok(session)
}

fn validate_terminal_id(terminal_id: &str) -> Result<(), String> {
    if terminal_id.trim().is_empty() {
        Err("终端 ID 不能为空".into())
    } else if terminal_id.len() > 256 {
        Err("终端 ID 过长".into())
    } else {
        Ok(())
    }
}

fn validate_conversation_id(conversation_id: &str) -> Result<(), String> {
    if conversation_id.trim().is_empty() {
        Err("终端的对话 ID 不能为空".into())
    } else if conversation_id.len() > 256 {
        Err("终端的对话 ID 过长".into())
    } else {
        Ok(())
    }
}

fn terminal_size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        cols: cols.clamp(2, MAX_COLS),
        rows: rows.clamp(1, MAX_ROWS),
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn append_buffer(buffer: &mut VecDeque<u8>, data: &[u8]) {
    buffer.extend(data);
    if buffer.len() > MAX_BUFFER_BYTES {
        let mut excess = buffer.len() - MAX_BUFFER_BYTES;
        // Finish the character the byte cut split, so a replayed history
        // does not open with U+FFFD.
        while buffer.get(excess).is_some_and(|byte| (byte & 0xC0) == 0x80) {
            excess += 1;
        }
        buffer.drain(..excess);
    }
}

fn canonical_directory(path: &Path) -> Result<PathBuf, String> {
    let canonical = std::fs::canonicalize(path)
        .map_err(|error| format!("无法解析终端工作目录 {}：{error}", path.display()))?;
    if !canonical.is_dir() {
        return Err(format!("终端工作目录不是文件夹：{}", canonical.display()));
    }
    Ok(plain_directory(&canonical))
}

/// Drop the verbatim (`\\?\`) prefix Windows canonicalization adds.
///
/// The prefix is invisible to `CreateProcessW` but not to the shell that runs
/// inside the pseudo console. PowerShell cannot express a verbatim path as an
/// ordinary drive location, so it falls back to the provider-qualified form:
/// `$PWD` becomes `Microsoft.PowerShell.Core\FileSystem::\\?\C:\…` and the
/// default prompt grows past eighty columns. Once the prompt wraps, conhost
/// rewrites every following line with absolute cursor moves, which is what a
/// user sees as unexplained indentation — and the location no longer matches
/// the one their own terminal reports. `git::git_cli_environment_path` strips
/// the same prefix for the same class of reason.
#[cfg(windows)]
fn plain_directory(path: &Path) -> PathBuf {
    let value = path.to_string_lossy();
    if let Some(unc) = value.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{unc}"));
    }
    // Only a drive-letter path survives losing the prefix. A volume GUID path
    // (`\\?\Volume{…}\`, produced for a directory mounted without a letter)
    // has no ordinary form, so it is left verbatim rather than turned into a
    // relative path that would not resolve.
    let stripped = value.strip_prefix(r"\\?\").filter(|stripped| {
        let mut bytes = stripped.bytes();
        matches!(
            (bytes.next(), bytes.next(), bytes.next()),
            (Some(drive), Some(b':'), Some(b'\\')) if drive.is_ascii_alphabetic()
        )
    });
    stripped.map_or_else(|| path.to_path_buf(), PathBuf::from)
}

#[cfg(not(windows))]
fn plain_directory(path: &Path) -> PathBuf {
    path.to_path_buf()
}

fn apply_control_frame(
    state: &mut TerminalOutputState,
    handshake: &Arc<(Mutex<ControlHandshake>, Condvar)>,
    frame: TerminalControlFrame,
) -> Option<TerminalControlUpdate> {
    match frame.kind {
        TerminalControlFrameKind::Ready => {
            if frame.generation == 0
                && !state.control_ready
                && mark_handshake(handshake, ControlHandshake::Ready)
            {
                state.control_ready = true;
                drop(state.startup_lease.take());
                return Some(TerminalControlUpdate::Ready);
            }
            None
        }
        TerminalControlFrameKind::Start => {
            if !state.control_ready
                || state.last_control_generation.checked_add(1) != Some(frame.generation)
                || state.command_state.status == TerminalCommandStatus::Running
            {
                let _ = state.control_reply.reject(frame.generation);
                return None;
            }
            state.last_control_generation = frame.generation;
            let command_lease = match (state.command_lease_factory)() {
                Ok(command_lease) => command_lease,
                Err(error) => {
                    eprintln!("终端命令因工作区操作冲突被拒绝：{error}");
                    let _ = state.control_reply.reject(frame.generation);
                    state.command_state.revision = state.command_state.revision.saturating_add(1);
                    return Some(TerminalControlUpdate::CommandState(
                        state.command_state.clone(),
                    ));
                }
            };
            state.command_lease = Some(command_lease);
            state.active_control_generation = Some(frame.generation);
            if let Err(error) = state.control_reply.ack(frame.generation) {
                eprintln!("无法确认终端命令租约：{error}");
                state.active_control_generation = None;
                drop(state.command_lease.take());
                let _ = state.control_reply.reject(frame.generation);
                state.command_state.revision = state.command_state.revision.saturating_add(1);
                return Some(TerminalControlUpdate::CommandState(
                    state.command_state.clone(),
                ));
            }
            state.command_state.revision = state.command_state.revision.saturating_add(1);
            state.command_state.status = TerminalCommandStatus::Running;
            state.command_state.command_id = Some(Uuid::new_v4().to_string());
            state.command_state.command_count = state.command_state.command_count.saturating_add(1);
            Some(TerminalControlUpdate::CommandState(
                state.command_state.clone(),
            ))
        }
        TerminalControlFrameKind::End => {
            if state.active_control_generation != Some(frame.generation)
                || state.command_state.status != TerminalCommandStatus::Running
            {
                return None;
            }
            state.active_control_generation = None;
            state.command_state.revision = state.command_state.revision.saturating_add(1);
            state.command_state.status = TerminalCommandStatus::Idle;
            state.command_state.command_id = None;
            drop(state.command_lease.take());
            Some(TerminalControlUpdate::CommandState(
                state.command_state.clone(),
            ))
        }
    }
}

fn mark_handshake(
    handshake: &Arc<(Mutex<ControlHandshake>, Condvar)>,
    next: ControlHandshake,
) -> bool {
    let (state, signal) = &**handshake;
    let mut state = lock(state);
    if *state == ControlHandshake::Pending {
        *state = next;
        signal.notify_all();
        true
    } else {
        false
    }
}

fn wait_for_control_handshake(
    handshake: &Arc<(Mutex<ControlHandshake>, Condvar)>,
    timeout: Duration,
) -> ControlHandshake {
    let (state, signal) = &**handshake;
    let state = lock(state);
    let (state, _) = signal
        .wait_timeout_while(state, timeout, |state| *state == ControlHandshake::Pending)
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *state
}

#[cfg(windows)]
fn signal_control_event(name: &str) -> Result<(), String> {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{OpenEventW, SetEvent, EVENT_MODIFY_STATE},
    };

    let wide = name
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let handle = unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, wide.as_ptr()) };
    if handle.is_null() {
        return Err(format!(
            "无法打开命令确认事件：{}",
            std::io::Error::last_os_error()
        ));
    }
    let signaled = unsafe { SetEvent(handle) } != 0;
    let signal_error = (!signaled).then(std::io::Error::last_os_error);
    unsafe {
        let _ = CloseHandle(handle);
    }
    signal_error.map_or(Ok(()), |error| {
        Err(format!("无法设置命令确认事件：{error}"))
    })
}

#[cfg_attr(not(windows), allow(dead_code))]
fn configure_powershell_control(args: &mut Vec<OsString>) {
    // PowerShell is launched with -NoProfile so the control nonce and event names can be removed
    // from the process environment before profiles (or any child they start) run. The standard
    // profiles are then dot-sourced in PowerShell's documented order.
    const SCRIPT_HEAD: &str = r#"
$script:__MeworkTerminalControlNonce = [Environment]::GetEnvironmentVariable('MEWORK_TERMINAL_CONTROL_NONCE', 'Process')
$meworkAckEventName = [Environment]::GetEnvironmentVariable('MEWORK_TERMINAL_ACK_EVENT', 'Process')
$meworkRejectEventName = [Environment]::GetEnvironmentVariable('MEWORK_TERMINAL_REJECT_EVENT', 'Process')
[Environment]::SetEnvironmentVariable('MEWORK_TERMINAL_CONTROL_NONCE', $null, 'Process')
[Environment]::SetEnvironmentVariable('MEWORK_TERMINAL_ACK_EVENT', $null, 'Process')
[Environment]::SetEnvironmentVariable('MEWORK_TERMINAL_REJECT_EVENT', $null, 'Process')
"#;
    // A pseudo console starts on the OEM code page like any other console, so
    // a program that writes UTF-8 bytes straight to it — git, most MSYS tools —
    // shows up as mojibake, and Windows PowerShell reads a BOM-less UTF-8 file
    // as ANSI. The same defaults the `powershell` tool runs under apply here,
    // ahead of the profiles so a profile that wants something else still wins.
    const SCRIPT_TAIL: &str = r#"
$meworkProfiles = @(
  $PROFILE.AllUsersAllHosts,
  $PROFILE.AllUsersCurrentHost,
  $PROFILE.CurrentUserAllHosts,
  $PROFILE.CurrentUserCurrentHost
) | Where-Object { $_ } | Select-Object -Unique
foreach ($meworkProfile in $meworkProfiles) {
  if (Microsoft.PowerShell.Management\Test-Path -LiteralPath $meworkProfile -PathType Leaf) {
    . $meworkProfile
  }
}

if (-not (Get-Command PSConsoleHostReadLine -CommandType Function -ErrorAction SilentlyContinue)) {
  Import-Module PSReadLine -ErrorAction Stop
}
$script:__MeworkTerminalOriginalReadLine = (Get-Command PSConsoleHostReadLine -CommandType Function -ErrorAction Stop).ScriptBlock
$meworkCreated = $false
$script:__MeworkTerminalAckEvent = [System.Threading.EventWaitHandle]::new(
  $false,
  [System.Threading.EventResetMode]::AutoReset,
  $meworkAckEventName,
  [ref]$meworkCreated
)
$meworkCreated = $false
$script:__MeworkTerminalRejectEvent = [System.Threading.EventWaitHandle]::new(
  $false,
  [System.Threading.EventResetMode]::AutoReset,
  $meworkRejectEventName,
  [ref]$meworkCreated
)
$script:__MeworkTerminalControlGeneration = [uint64]0
$script:__MeworkTerminalActiveGeneration = [uint64]0

function script:__MeworkTerminalWriteControl([string]$kind, [uint64]$generation) {
  [Console]::Write(("{0}]633;Mework;v1;{1};{2};{3}{4}" -f [char]27, $script:__MeworkTerminalControlNonce, $kind, $generation, [char]7))
}

function global:PSConsoleHostReadLine {
  $meworkTopLevel = $NestedPromptLevel -eq 0 -and -not (Microsoft.PowerShell.Management\Test-Path Variable:/PSDebugContext)
  if ($meworkTopLevel) {
    if ($script:__MeworkTerminalActiveGeneration -ne 0) {
      __MeworkTerminalWriteControl 'end' $script:__MeworkTerminalActiveGeneration
      $script:__MeworkTerminalActiveGeneration = [uint64]0
    } else {
      __MeworkTerminalWriteControl 'ready' 0
    }
  }

  $meworkLine = & $script:__MeworkTerminalOriginalReadLine
  if (-not $meworkTopLevel) {
    return $meworkLine
  }

  $script:__MeworkTerminalControlGeneration++
  $meworkGeneration = $script:__MeworkTerminalControlGeneration
  __MeworkTerminalWriteControl 'start' $meworkGeneration
  $meworkDecision = [System.Threading.WaitHandle]::WaitAny(
    [System.Threading.WaitHandle[]]@(
      $script:__MeworkTerminalAckEvent,
      $script:__MeworkTerminalRejectEvent
    ),
    30000
  )
  if ($meworkDecision -eq 0) {
    $script:__MeworkTerminalActiveGeneration = $meworkGeneration
    return $meworkLine
  }
  return "Microsoft.PowerShell.Utility\Write-Error 'Mework 未执行该命令：工作区正在进行 Git、模型或其他写操作；请稍后按上箭头重试。'"
}
"#;
    let script = powershell_bootstrap_script(SCRIPT_HEAD, SCRIPT_TAIL);
    let encoded_script = base64::engine::general_purpose::STANDARD.encode(
        script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    args.extend([
        OsString::from("-NoProfile"),
        OsString::from("-NoExit"),
        OsString::from("-EncodedCommand"),
        OsString::from(encoded_script),
    ]);
}

/// The bootstrap in the order it runs: control-secret scrubbing, the strict
/// UTF-8 defaults this surface needs, then profiles and the PSReadLine hook.
///
/// These are deliberately not the `powershell` tool's preamble: that one matches
/// Claude Code byte for byte and is weaker. See `powershell_host`.
#[cfg_attr(not(windows), allow(dead_code))]
fn powershell_bootstrap_script(head: &str, tail: &str) -> String {
    let defaults = crate::powershell_host::strict_text_defaults().join("\n");
    format!("{head}\n{defaults}\n{tail}")
}

#[cfg(windows)]
fn kill_process_tree(process_id: Option<u32>, _process_group_id: Option<i32>) -> bool {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};

    let Some(process_id) = process_id else {
        return false;
    };
    let process_id = process_id.to_string();
    Command::new("taskkill.exe")
        .args(["/PID", process_id.as_str(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(0x0800_0000)
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(not(windows))]
fn kill_process_tree(_process_id: Option<u32>, process_group_id: Option<i32>) -> bool {
    if let Some(process_group_id) =
        process_group_id.filter(|process_group_id| *process_group_id > 0)
    {
        // portable-pty creates a dedicated session/process group for the shell. Targeting the
        // negative group ID prevents background children from surviving application teardown.
        return unsafe { libc::kill(-process_group_id, libc::SIGKILL) == 0 };
    }
    false
}

/// Asks the shell's process group to hang up, as closing a terminal window
/// does. The group comes back when the signal went out.
#[cfg(unix)]
fn hang_up_process_group(process_group_id: Option<i32>) -> Option<i32> {
    let process_group_id = process_group_id.filter(|process_group_id| *process_group_id > 0)?;
    (unsafe { libc::kill(-process_group_id, libc::SIGHUP) } == 0).then_some(process_group_id)
}

/// Whether anything is left in the process group. Signal 0 only checks; EPERM
/// means a member exists that Mework may not signal (a `sudo`), which counts.
#[cfg(unix)]
fn process_group_exists(process_group_id: i32) -> bool {
    let signalled = unsafe { libc::kill(-process_group_id, 0) } == 0;
    signalled || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// What a host terminal starts: the program, its arguments, the label a tab
/// shows, and how its command barrier is answered.
type HostShell = (OsString, Vec<OsString>, String, ShellControl);

/// A Windows host offers PowerShell, its default, and Git Bash.
#[cfg(windows)]
fn host_shell(shell: Option<TerminalShell>) -> Result<HostShell, String> {
    match shell {
        None | Some(TerminalShell::PowerShell) => Ok(powershell()),
        Some(TerminalShell::Bash) => git_bash(),
        Some(other @ (TerminalShell::Zsh | TerminalShell::Fish | TerminalShell::Sh)) => Err(format!(
            "Windows 本机终端不提供 {}；请选择 PowerShell 或 Git Bash",
            other.program_name()
        )),
    }
}

#[cfg(windows)]
fn powershell() -> HostShell {
    for (name, label) in [
        ("pwsh.exe", "PowerShell"),
        ("powershell.exe", "Windows PowerShell"),
    ] {
        if let Some(program) = executable_in_path(name) {
            return (
                program.into_os_string(),
                vec![OsString::from("-NoLogo")],
                label.into(),
                ShellControl::PowerShell,
            );
        }
    }
    (
        OsString::from("cmd.exe"),
        vec![OsString::from("/Q")],
        "命令提示符".into(),
        ShellControl::Unsupported,
    )
}

/// Git Bash: the native bash [`crate::run_environment::local_bash_candidates`]
/// resolves first, which is never the WSL launcher that also answers to
/// `bash.exe`. It runs interactive with the session's rcfile, which starts up
/// the way Git Bash's own login shell does.
#[cfg(windows)]
fn git_bash() -> Result<HostShell, String> {
    let program = crate::run_environment::local_bash_candidates()
        .into_iter()
        .next()
        .ok_or_else(|| {
            "未找到 Git Bash；请先安装 Git for Windows，或在终端菜单中选择 PowerShell".to_owned()
        })?;
    Ok((
        OsString::from(program),
        vec![OsString::from("-i")],
        "bash".into(),
        ShellControl::Bash,
    ))
}

/// Why a host terminal was refused: it could not have held the Git mutex.
#[cfg(windows)]
const UNSUPPORTED_SHELL_MESSAGE: &str =
    "当前系统没有可用的 PowerShell；为保证 Git 与终端命令严格互斥，未启动不受支持的 shell";
#[cfg(not(windows))]
const UNSUPPORTED_SHELL_MESSAGE: &str =
    "当前系统没有可用的 zsh；为保证 Git 与终端命令严格互斥，未启动不受支持的 shell";

/// A Mac or Linux host offers zsh, its default, bash and fish, each run the way
/// macOS's Terminal runs a login shell, so `/etc/zprofile`'s `path_helper`, the
/// system profile and the user's own startup files all apply.
///
/// Only a shell whose line editor can be told to wait for the host before
/// running a line is offered — that is what holds the Git mutex — and a shell
/// the machine does not have is refused rather than replaced by another. The
/// default is the one exception, as it always was: without zsh it resolves to
/// an unsupported `sh`, which `open` then refuses with the general message.
#[cfg(unix)]
fn host_shell(shell: Option<TerminalShell>) -> Result<HostShell, String> {
    let chosen = shell.unwrap_or(TerminalShell::Zsh);
    let (control, args): (ShellControl, &[&str]) = match chosen {
        TerminalShell::PowerShell => {
            return Err("本机终端不提供 PowerShell；请选择 zsh、bash 或 fish".into());
        }
        TerminalShell::Sh => {
            return Err(
                "本机终端不提供 sh：它无法与 Git 操作严格互斥；请选择 zsh、bash 或 fish".into(),
            );
        }
        TerminalShell::Zsh => (ShellControl::Zsh, &["-l", "-i"]),
        // `-l` is emulated by the rcfile: a login bash reads no rcfile.
        TerminalShell::Bash => (ShellControl::Bash, &["-i"]),
        TerminalShell::Fish => (ShellControl::Fish, &["-l", "-i"]),
    };
    let name = chosen.program_name();
    match posix_shell_program(chosen) {
        Some(program) => Ok((
            program,
            args.iter().map(OsString::from).collect(),
            name.into(),
            control,
        )),
        None if shell.is_some() => Err(format!(
            "未找到 {name}；请先安装 {name}，或在终端菜单中选择其他 shell"
        )),
        None => Ok((
            OsString::from("/bin/sh"),
            Vec::new(),
            "sh".into(),
            ShellControl::Unsupported,
        )),
    }
}

/// Where a shell is: `$SHELL` when it names that shell (a Homebrew build the
/// user logs in with, say), then the places systems and package managers put
/// it. zsh keeps the system build first, as it always has; for bash a newer
/// build wins over macOS's `/bin/bash` 3.2, as it would on the user's `PATH`.
#[cfg(unix)]
fn posix_shell_program(shell: TerminalShell) -> Option<OsString> {
    let well_known: &[&str] = match shell {
        TerminalShell::Zsh => &[
            "/bin/zsh",
            "/usr/bin/zsh",
            "/opt/homebrew/bin/zsh",
            "/usr/local/bin/zsh",
        ],
        TerminalShell::Bash => &[
            "/opt/homebrew/bin/bash",
            "/usr/local/bin/bash",
            "/bin/bash",
            "/usr/bin/bash",
        ],
        TerminalShell::Fish => &[
            "/opt/homebrew/bin/fish",
            "/usr/local/bin/fish",
            "/usr/bin/fish",
            "/bin/fish",
        ],
        TerminalShell::PowerShell | TerminalShell::Sh => &[],
    };
    let name = shell.program_name();
    let is_shell = |path: &Path| {
        path.file_name().and_then(std::ffi::OsStr::to_str) == Some(name) && path.is_file()
    };
    env::var_os("SHELL")
        .filter(|value| is_shell(Path::new(value)))
        .or_else(|| {
            well_known
                .iter()
                .find(|path| is_shell(Path::new(path)))
                .map(OsString::from)
        })
}

#[cfg(windows)]
fn executable_in_path(name: &str) -> Option<PathBuf> {
    env::var_os("PATH")
        .into_iter()
        .flat_map(|path| env::split_paths(&path).collect::<Vec<_>>())
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    fn control_frame(nonce: &str, kind: TerminalControlFrameKind, generation: u64) -> Vec<u8> {
        let kind = match kind {
            TerminalControlFrameKind::Ready => "ready",
            TerminalControlFrameKind::Start => "start",
            TerminalControlFrameKind::End => "end",
        };
        format!("\x1b]633;Mework;v1;{nonce};{kind};{generation}\x07").into_bytes()
    }

    struct DropProbe(Arc<AtomicUsize>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }

    struct ExclusiveProbe {
        active: Arc<AtomicBool>,
        drops: Arc<AtomicUsize>,
    }

    impl Drop for ExclusiveProbe {
        fn drop(&mut self) {
            self.active.store(false, Ordering::Release);
            self.drops.fetch_add(1, Ordering::AcqRel);
        }
    }

    fn unpack_tokens(
        tokens: impl IntoIterator<Item = TerminalControlToken>,
    ) -> (Vec<u8>, Vec<TerminalControlFrame>) {
        let mut visible = Vec::new();
        let mut frames = Vec::new();
        for token in tokens {
            match token {
                TerminalControlToken::Visible(data) => visible.extend(data),
                TerminalControlToken::Frame(frame) => frames.push(frame),
            }
        }
        (visible, frames)
    }

    #[test]
    fn terminal_dimensions_are_bounded() {
        assert_eq!(terminal_size(0, 0).cols, 2);
        assert_eq!(terminal_size(0, 0).rows, 1);
        assert_eq!(terminal_size(u16::MAX, u16::MAX).cols, MAX_COLS);
        assert_eq!(terminal_size(u16::MAX, u16::MAX).rows, MAX_ROWS);
    }

    #[test]
    fn terminal_launches_in_the_directory_the_user_would_type() {
        let launch = TerminalLaunch::host(&std::env::current_dir().unwrap(), None).unwrap();
        assert!(
            !launch.display_cwd.starts_with(r"\\?\"),
            "a verbatim working directory turns $PWD into a provider-qualified \
             location and wraps the prompt: {}",
            launch.display_cwd
        );
        assert!(launch.cwd.is_dir());
        assert_eq!(launch.cwd.to_string_lossy(), launch.display_cwd);
        // Session reuse is keyed on the binding, so the launched and displayed
        // directory must be the same one recorded there.
        assert!(launch.binding.contains(&launch.display_cwd));
    }

    #[cfg(unix)]
    #[test]
    fn a_host_terminal_runs_the_shell_asked_for_or_refuses() {
        let cwd = std::env::current_dir().unwrap();
        let error = TerminalLaunch::host(&cwd, Some(TerminalShell::PowerShell))
            .err()
            .expect("no PowerShell on a Mac or Linux host");
        assert!(error.contains("PowerShell"), "{error}");
        // sh has no line editor to hold a line for the Git mutex.
        let error = TerminalLaunch::host(&cwd, Some(TerminalShell::Sh))
            .err()
            .expect("no sh terminal on the host");
        assert!(error.contains("sh"), "{error}");

        let default = TerminalLaunch::host(&cwd, None).unwrap();
        if default.control != ShellControl::Unsupported {
            assert_eq!(default.shell, "zsh");
            assert_eq!(default.args, ["-l", "-i"]);
            let zsh = TerminalLaunch::host(&cwd, Some(TerminalShell::Zsh)).unwrap();
            assert_eq!(zsh.binding, default.binding, "zsh is the default");
        }
        match TerminalLaunch::host(&cwd, Some(TerminalShell::Bash)) {
            Ok(bash) => {
                assert_eq!(bash.shell, "bash");
                assert_eq!(bash.control, ShellControl::Bash);
                assert_eq!(bash.args, ["-i"]);
                assert_eq!(
                    Path::new(&bash.program).file_name(),
                    Some(std::ffi::OsStr::new("bash"))
                );
                // Switching shells must not reattach the old session.
                assert_ne!(bash.binding, default.binding);
            }
            Err(error) => assert!(error.contains("未找到 bash"), "{error}"),
        }
        match TerminalLaunch::host(&cwd, Some(TerminalShell::Fish)) {
            Ok(fish) => {
                assert_eq!(fish.shell, "fish");
                assert_eq!(fish.control, ShellControl::Fish);
                assert_eq!(fish.args, ["-l", "-i"]);
            }
            Err(error) => assert!(error.contains("未找到 fish"), "{error}"),
        }
    }

    /// Closing a shell hangs it up before anything is killed, so one that
    /// handles SIGHUP — zsh, bash and fish write their history there — gets to
    /// finish, and one that ignores it is still killed once the grace is over.
    #[cfg(unix)]
    #[test]
    fn a_closed_shell_is_hung_up_before_its_group_is_killed() {
        use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
        use std::time::Instant;

        let scratch = tempfile::tempdir().unwrap();
        let spawn_group = |trap: &str, marker: &Path| {
            let script = format!("{trap}; : > \"$1.ready\"; while :; do sleep 0.05; done");
            let mut child = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(script)
                .arg("sh")
                .arg(marker)
                .process_group(0)
                .spawn()
                .unwrap();
            let ready = marker.with_extension("ready");
            let deadline = Instant::now() + Duration::from_secs(5);
            while !ready.exists() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            assert!(ready.exists(), "the script never installed its trap");
            let process_group_id = i32::try_from(child.id()).unwrap();
            // A real shell is reaped by its terminal's waiter thread.
            (process_group_id, thread::spawn(move || child.wait().unwrap()))
        };
        let close = |process_group_id: i32| {
            assert_eq!(
                hang_up_process_group(Some(process_group_id)),
                Some(process_group_id)
            );
            HangingUpShell {
                process_group_id,
                master: Arc::new(Mutex::new(None)),
            }
            .finish_by(Instant::now() + SHELL_HANGUP_GRACE);
        };

        let written = scratch.path().join("history");
        let (group, reaper) = spawn_group("trap 'echo saved > \"$1\"; exit 0' HUP", &written);
        close(group);
        assert!(reaper.join().unwrap().success());
        assert_eq!(std::fs::read_to_string(&written).unwrap().trim(), "saved");

        let stubborn = scratch.path().join("stubborn");
        let (group, reaper) = spawn_group("trap '' HUP", &stubborn);
        let started = Instant::now();
        close(group);
        assert!(started.elapsed() >= SHELL_HANGUP_GRACE);
        assert_eq!(reaper.join().unwrap().signal(), Some(libc::SIGKILL));
        assert!(!stubborn.exists());
    }

    #[cfg(windows)]
    #[test]
    fn a_windows_host_offers_powershell_and_git_bash() {
        let cwd = std::env::current_dir().unwrap();
        for shell in [TerminalShell::Zsh, TerminalShell::Fish, TerminalShell::Sh] {
            let error = TerminalLaunch::host(&cwd, Some(shell)).err().unwrap();
            assert!(error.contains(shell.program_name()), "{error}");
        }
        let default = TerminalLaunch::host(&cwd, None).unwrap();
        match TerminalLaunch::host(&cwd, Some(TerminalShell::Bash)) {
            Ok(bash) => {
                assert_eq!(bash.shell, "bash");
                assert_eq!(bash.control, ShellControl::Bash);
                assert_eq!(bash.args, ["-i"]);
                let program = bash.program.to_string_lossy().to_ascii_lowercase();
                assert!(!program.contains(r"\system32\"), "{program}");
                assert_ne!(bash.binding, default.binding);
            }
            Err(error) => assert!(error.contains("Git Bash"), "{error}"),
        }
    }

    fn wsl_runner(env: &[(&str, &str)]) -> crate::run_environment::ShellRunner {
        crate::run_environment::ShellRunner::Wsl {
            agent_shell: Default::default(),
            distro: "Ubuntu".into(),
            env: env
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
        }
    }

    fn ssh_runner(env: &[(&str, &str)]) -> crate::run_environment::ShellRunner {
        crate::run_environment::ShellRunner::Ssh {
            agent_shell: Default::default(),
            host: "ada@build".into(),
            port: 2222,
            identity_file: String::new(),
            env: env
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
        }
    }

    const REMOTE_SHELLS: [TerminalShell; 4] = [
        TerminalShell::Zsh,
        TerminalShell::Bash,
        TerminalShell::Fish,
        TerminalShell::Sh,
    ];

    #[test]
    fn a_wsl_terminal_starts_the_chosen_shell_through_sh() {
        let local = std::env::current_dir().unwrap();
        let args = |launch: &TerminalLaunch| {
            launch
                .args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        // No choice keeps the distribution's default shell, exactly as before.
        let plain =
            TerminalLaunch::remote(&wsl_runner(&[]), "/home/ada/project", &local, None).unwrap();
        assert_eq!(plain.shell, "WSL: Ubuntu");
        assert_eq!(args(&plain), ["-d", "Ubuntu", "--cd", "/home/ada/project"]);
        let plain_env = TerminalLaunch::remote(
            &wsl_runner(&[("FOO", "bar baz")]),
            "/home/ada/project",
            &local,
            None,
        )
        .unwrap();
        assert_eq!(
            args(&plain_env)[4..],
            ["--exec", "/usr/bin/env", "FOO=bar baz", "bash", "-l"]
        );

        for shell in REMOTE_SHELLS {
            let name = shell.program_name();
            let launch =
                TerminalLaunch::remote(&wsl_runner(&[]), "/home/ada/project", &local, Some(shell))
                    .unwrap();
            assert_eq!(launch.program, OsString::from("wsl.exe"));
            assert_eq!(launch.shell, format!("WSL: Ubuntu · {name}"));
            assert_eq!(launch.control, ShellControl::Remote);
            assert_eq!(
                args(&launch),
                [
                    "-d",
                    "Ubuntu",
                    "--cd",
                    "/home/ada/project",
                    "--exec",
                    "/bin/sh",
                    "-c",
                    &remote_shell_script(name),
                ]
            );
            assert_ne!(launch.binding, plain.binding);

            let with_env = TerminalLaunch::remote(
                &wsl_runner(&[("FOO", "bar baz"), ("BASH_ENV", "/tmp/evil")]),
                "/home/ada/project",
                &local,
                Some(shell),
            )
            .unwrap();
            assert_eq!(
                args(&with_env)[4..],
                [
                    "--exec",
                    "/usr/bin/env",
                    "FOO=bar baz",
                    "/bin/sh",
                    "-c",
                    &remote_shell_script(name),
                ]
            );
        }
        let error = TerminalLaunch::remote(
            &wsl_runner(&[]),
            "/home/ada/project",
            &local,
            Some(TerminalShell::PowerShell),
        )
        .err()
        .unwrap();
        assert!(error.contains("PowerShell"), "{error}");
    }

    #[test]
    fn an_ssh_terminal_quotes_the_chosen_shell_into_its_remote_command() {
        use crate::run_environment::sh_single_quote;
        let local = std::env::current_dir().unwrap();
        // The login shell is sent the neutral line; what `sh` then runs is the
        // command below.
        let remote_command = |launch: &TerminalLaunch| {
            crate::remote_shell::tests::decode_posix_line(
                &launch.args.last().unwrap().to_string_lossy(),
            )
        };
        let plain = TerminalLaunch::remote(&ssh_runner(&[]), "~/project", &local, None).unwrap();
        assert_eq!(plain.shell, "SSH: ada@build");
        assert_eq!(
            remote_command(&plain),
            r#"cd ~/'project' && exec "${SHELL:-/bin/sh}" -l"#
        );
        for shell in REMOTE_SHELLS {
            let name = shell.program_name();
            let launch =
                TerminalLaunch::remote(&ssh_runner(&[]), "~/project", &local, Some(shell)).unwrap();
            assert_eq!(launch.shell, format!("SSH: ada@build · {name}"));
            assert_eq!(launch.control, ShellControl::Remote);
            assert_eq!(
                launch.args[..6],
                ["-t", "-o", "ConnectTimeout=10", "-p", "2222", "--"].map(OsString::from)
            );
            assert_eq!(
                remote_command(&launch),
                format!(
                    "cd ~/'project' && exec /bin/sh -c {}",
                    sh_single_quote(&remote_shell_script(name))
                )
            );
            assert_ne!(launch.binding, plain.binding);
            let with_env = TerminalLaunch::remote(
                &ssh_runner(&[("FOO", "it's")]),
                "~/project",
                &local,
                Some(shell),
            )
            .unwrap();
            assert_eq!(
                remote_command(&with_env),
                format!(
                    "cd ~/'project' && export 'FOO=it'\\''s' && exec /bin/sh -c {}",
                    sh_single_quote(&remote_shell_script(name))
                )
            );
        }
        // PowerShell is the Windows agent's own default, so it asks for no
        // shell: the fallback line is the plain one, and the agent is told
        // nothing to look for.
        let powershell = TerminalLaunch::remote(
            &ssh_runner(&[]),
            "~/project",
            &local,
            Some(TerminalShell::PowerShell),
        )
        .unwrap();
        assert_eq!(remote_command(&powershell), remote_command(&plain));
        assert_eq!(
            powershell.agent.as_ref().unwrap().windows_shell,
            None::<String>
        );
    }

    /// The remote command as a remote login shell would run it, here, with a
    /// shell this machine has and one it lacks: the first is exec'd as a login
    /// shell, the second falls back to `$SHELL` with a note.
    #[cfg(unix)]
    #[test]
    fn a_remote_shell_that_is_missing_falls_back_to_the_login_shell() {
        use std::process::{Command, Stdio};
        let local = std::env::current_dir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let workspace_path = workspace.path().to_string_lossy().into_owned();
        std::fs::write(home.path().join(".bash_profile"), "echo BASH-LOGIN:$PWD\n").unwrap();
        // `exec` needs the shell on PATH; an empty directory makes "fish" missing
        // whether or not this machine has one.
        let empty = tempfile::tempdir().unwrap();
        let run = |shell: TerminalShell, path: &str| {
            let launch =
                TerminalLaunch::remote(&ssh_runner(&[]), &workspace_path, &local, Some(shell))
                    .unwrap();
            let command = launch.args.last().unwrap().clone();
            Command::new("/bin/sh")
                .arg("-c")
                .arg(command)
                .env_clear()
                .env("HOME", home.path())
                .env("PATH", path)
                .env("SHELL", "/bin/echo")
                .stdin(Stdio::null())
                .output()
                .unwrap()
        };
        let missing = run(TerminalShell::Fish, &empty.path().to_string_lossy());
        assert_eq!(String::from_utf8_lossy(&missing.stdout), "-l\n");
        let note = String::from_utf8_lossy(&missing.stderr);
        assert!(
            note.contains("没有 fish") && note.contains("/bin/echo"),
            "{note}"
        );
        if Path::new("/bin/bash").is_file() {
            let found = run(TerminalShell::Bash, "/bin:/usr/bin");
            let stdout = String::from_utf8_lossy(&found.stdout);
            let canonical = std::fs::canonicalize(workspace.path()).unwrap();
            assert!(
                stdout.contains(&format!("BASH-LOGIN:{}", canonical.display()))
                    || stdout.contains(&format!("BASH-LOGIN:{workspace_path}")),
                "{stdout:?} {:?}",
                String::from_utf8_lossy(&found.stderr)
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn only_a_prefix_an_ordinary_path_can_lose_is_dropped() {
        assert_eq!(
            plain_directory(Path::new(r"\\?\C:\Users\example\project")),
            PathBuf::from(r"C:\Users\example\project")
        );
        assert_eq!(
            plain_directory(Path::new(r"\\?\UNC\server\share\project")),
            PathBuf::from(r"\\server\share\project")
        );
        assert_eq!(
            plain_directory(Path::new(r"C:\Users\example\project")),
            PathBuf::from(r"C:\Users\example\project")
        );
        // A volume mounted without a drive letter has no ordinary form. Losing
        // the prefix there would produce a path that no longer resolves.
        let volume = r"\\?\Volume{2eca078d-5cbc-43d2-8ef5-c546a1c37b3c}\project";
        assert_eq!(plain_directory(Path::new(volume)), PathBuf::from(volume));
    }

    #[test]
    fn replay_buffer_retains_only_the_latest_bytes() {
        let mut buffer = VecDeque::new();
        append_buffer(&mut buffer, &vec![b'a'; MAX_BUFFER_BYTES]);
        append_buffer(&mut buffer, b"tail");
        assert_eq!(buffer.len(), MAX_BUFFER_BYTES);
        assert_eq!(
            buffer.iter().rev().take(4).copied().collect::<Vec<_>>(),
            b"liat"
        );
    }

    #[test]
    fn a_draft_binding_lasts_while_its_owner_has_a_shell() {
        let sessions = HashMap::from([(TerminalKey::new("conv-draft", "terminal-1"), ())]);
        let mut drafts = HashMap::from([
            ("conv-draft".to_owned(), "ws-a".to_owned()),
            ("conv-gone".to_owned(), "ws-a".to_owned()),
        ]);

        forget_idle_draft(&sessions, &mut drafts, "conv-draft");
        forget_idle_draft(&sessions, &mut drafts, "conv-gone");

        assert_eq!(
            drafts,
            HashMap::from([("conv-draft".to_owned(), "ws-a".to_owned())])
        );
    }

    #[test]
    fn settling_drafts_drops_materialized_owners_and_those_whose_project_went() {
        let manager = TerminalManager::default();
        manager.bind_draft("conv-materialized", "ws-a");
        manager.bind_draft("conv-orphaned", "ws-removed");
        manager.bind_draft("conv-waiting", "ws-a");

        let standing = manager.settle_drafts(
            |owner| owner == "conv-materialized",
            |workspace_id| workspace_id == "ws-removed",
        );

        assert_eq!(
            standing,
            HashMap::from([("conv-waiting".to_owned(), "ws-a".to_owned())])
        );
        assert_eq!(*lock(&manager.drafts), standing);
    }

    #[test]
    fn rebinding_a_draft_moves_it_and_closing_every_owner_forgets_it() {
        let manager = TerminalManager::default();
        manager.bind_draft("conv-draft", "ws-a");
        manager.bind_draft("conv-draft", "ws-b");
        assert_eq!(
            *lock(&manager.drafts),
            HashMap::from([("conv-draft".to_owned(), "ws-b".to_owned())])
        );

        manager.close_missing(["conv-draft"]);
        assert_eq!(lock(&manager.drafts).len(), 1);
        manager.close_missing(std::iter::empty());
        assert!(lock(&manager.drafts).is_empty());

        manager.bind_draft("conv-draft", "ws-a");
        manager.release_idle_draft("conv-draft");
        assert!(lock(&manager.drafts).is_empty());
    }

    #[test]
    fn session_removal_targets_every_terminal_owned_by_invalidated_conversations() {
        let mut sessions = HashMap::from([
            (TerminalKey::new("conversation-a", "terminal-a"), "a"),
            (TerminalKey::new("conversation-b", "terminal-b"), "b"),
            (TerminalKey::new("conversation-a", "terminal-c"), "c"),
        ]);

        let mut removed =
            remove_matching_sessions(&mut sessions, |key| key.conversation_id == "conversation-a");
        removed.sort();

        assert_eq!(removed, vec!["a", "c"]);
        assert_eq!(
            sessions,
            HashMap::from([(TerminalKey::new("conversation-b", "terminal-b"), "b")])
        );
    }

    /// Every conversation's composer drawer asks for the same terminal id, so
    /// the id alone cannot name a session: the second conversation used to be
    /// refused with "terminal belongs to another task".
    #[test]
    fn the_same_terminal_id_names_a_different_session_in_each_conversation() {
        let mut sessions = HashMap::from([
            (TerminalKey::new("conversation-a", "composer"), "a"),
            (TerminalKey::new("conversation-b", "composer"), "b"),
        ]);
        assert_eq!(sessions.len(), 2);
        assert_eq!(
            sessions.get(&TerminalKey::new("conversation-b", "composer")),
            Some(&"b")
        );
        assert_eq!(
            sessions.get(&TerminalKey::new("conversation-c", "composer")),
            None
        );

        let removed =
            remove_matching_sessions(&mut sessions, |key| key.conversation_id == "conversation-a");
        assert_eq!(removed, vec!["a"]);
        assert_eq!(
            sessions.get(&TerminalKey::new("conversation-b", "composer")),
            Some(&"b")
        );
    }

    #[test]
    fn control_parser_strips_only_authenticated_frames_across_every_split() {
        let nonce = "0123456789abcdef0123456789abcdef";
        let frame = control_frame(nonce, TerminalControlFrameKind::Start, 42);
        let mut payload = b"before".to_vec();
        payload.extend_from_slice(&frame);
        payload.extend_from_slice(b"after");

        for split in 0..=payload.len() {
            let mut parser = TerminalControlParser::new(nonce);
            let first = parser.filter(&payload[..split]);
            let second = parser.filter(&payload[split..]);
            let (mut visible, frames) = unpack_tokens(first.into_iter().chain(second));
            visible.extend(parser.finish());
            assert_eq!(visible, b"beforeafter", "split at byte {split}");
            assert_eq!(
                frames,
                vec![TerminalControlFrame {
                    kind: TerminalControlFrameKind::Start,
                    generation: 42,
                }],
                "split at byte {split}"
            );
        }
    }

    #[test]
    fn control_parser_preserves_spoofed_malformed_and_incomplete_sequences() {
        let nonce = "trusted";
        let cases = [
            b"\x1b]633;Mework;v1;other;start;1\x07".as_slice(),
            b"\x1b]633;Mework;v2;trusted;start;1\x07".as_slice(),
            b"\x1b]633;Mework;v1;trusted;other;1\x07".as_slice(),
            b"\x1b]633;Mework;v1;trusted;start;not-a-number\x07".as_slice(),
            b"\x1b]633;Mework;v1;trusted;start;18446744073709551616\x07".as_slice(),
            b"\x1b]633;Mework;v1;trusted;start;12".as_slice(),
            b"ordinary\x1b[31mred".as_slice(),
        ];
        for input in cases {
            let mut parser = TerminalControlParser::new(nonce);
            let (mut visible, frames) = unpack_tokens(parser.filter(input));
            visible.extend(parser.finish());
            assert_eq!(visible, input);
            assert!(frames.is_empty());
        }
    }

    #[test]
    fn control_parser_preserves_visible_and_lifecycle_token_order_in_one_chunk() {
        let nonce = "trusted";
        let mut payload = b"first".to_vec();
        payload.extend(control_frame(nonce, TerminalControlFrameKind::End, 4));
        payload.extend_from_slice(b"between");
        payload.extend(control_frame(nonce, TerminalControlFrameKind::Start, 5));
        payload.extend_from_slice(b"last");
        let mut parser = TerminalControlParser::new(nonce);
        assert_eq!(
            parser.filter(&payload),
            vec![
                TerminalControlToken::Visible(b"first".to_vec()),
                TerminalControlToken::Frame(TerminalControlFrame {
                    kind: TerminalControlFrameKind::End,
                    generation: 4,
                }),
                TerminalControlToken::Visible(b"between".to_vec()),
                TerminalControlToken::Frame(TerminalControlFrame {
                    kind: TerminalControlFrameKind::Start,
                    generation: 5,
                }),
                TerminalControlToken::Visible(b"last".to_vec()),
            ]
        );
        assert!(parser.finish().is_empty());
    }

    #[test]
    fn only_matching_end_frame_releases_exactly_one_running_command_lease() {
        let drops = Arc::new(AtomicUsize::new(0));
        let handshake = Arc::new((Mutex::new(ControlHandshake::Pending), Condvar::new()));
        let mut state = TerminalOutputState {
            buffer: VecDeque::new(),
            sink: None,
            running: true,
            closed: false,
            control: None,
            control_ready: false,
            last_control_generation: 1,
            active_control_generation: Some(1),
            command_state: TerminalCommandState {
                revision: 7,
                status: TerminalCommandStatus::Running,
                command_id: Some("command-1".into()),
                command_count: 3,
            },
            command_lease: Some(Box::new(DropProbe(drops.clone()))),
            startup_lease: None,
            command_lease_factory: Arc::new(|| Err("not used".into())),
            control_reply: ControlReply::None,
        };

        assert!(matches!(
            apply_control_frame(
                &mut state,
                &handshake,
                TerminalControlFrame {
                    kind: TerminalControlFrameKind::Ready,
                    generation: 0,
                },
            ),
            Some(TerminalControlUpdate::Ready)
        ));
        assert_eq!(*lock(&handshake.0), ControlHandshake::Ready);
        assert!(state.control_ready);
        assert!(apply_control_frame(
            &mut state,
            &handshake,
            TerminalControlFrame {
                kind: TerminalControlFrameKind::End,
                generation: 2,
            },
        )
        .is_none());
        assert_eq!(drops.load(Ordering::Acquire), 0);
        let Some(TerminalControlUpdate::CommandState(idle)) = apply_control_frame(
            &mut state,
            &handshake,
            TerminalControlFrame {
                kind: TerminalControlFrameKind::End,
                generation: 1,
            },
        ) else {
            panic!("matching end must emit an authoritative command state");
        };
        assert_eq!(idle.revision, 8);
        assert_eq!(idle.status, TerminalCommandStatus::Idle);
        assert_eq!(idle.command_id, None);
        assert_eq!(idle.command_count, 3);
        assert_eq!(drops.load(Ordering::Acquire), 1);
        assert!(apply_control_frame(
            &mut state,
            &handshake,
            TerminalControlFrame {
                kind: TerminalControlFrameKind::End,
                generation: 1,
            },
        )
        .is_none());
        assert_eq!(drops.load(Ordering::Acquire), 1);
        assert_eq!(state.command_state.revision, 8);
    }

    #[test]
    fn rejected_start_advances_generation_and_emits_idle_revision() {
        let handshake = Arc::new((Mutex::new(ControlHandshake::Ready), Condvar::new()));
        let mut state = TerminalOutputState {
            buffer: VecDeque::new(),
            sink: None,
            running: true,
            closed: false,
            control: None,
            control_ready: true,
            last_control_generation: 0,
            active_control_generation: None,
            command_state: TerminalCommandState::default(),
            command_lease: None,
            startup_lease: None,
            command_lease_factory: Arc::new(|| Err("workspace writer active".into())),
            control_reply: ControlReply::None,
        };

        let Some(TerminalControlUpdate::CommandState(rejected)) = apply_control_frame(
            &mut state,
            &handshake,
            TerminalControlFrame {
                kind: TerminalControlFrameKind::Start,
                generation: 1,
            },
        ) else {
            panic!("rejected start must clear the renderer's optimistic busy state");
        };
        assert_eq!(state.last_control_generation, 1);
        assert_eq!(rejected.revision, 1);
        assert_eq!(rejected.status, TerminalCommandStatus::Idle);
        assert_eq!(rejected.command_id, None);
        assert_eq!(rejected.command_count, 0);
        assert!(state.command_lease.is_none());
        assert!(state.active_control_generation.is_none());
    }

    #[test]
    fn terminal_command_wire_state_is_a_complete_nested_snapshot() {
        let value = serde_json::to_value(TerminalEvent::CommandState {
            session_id: "session-1".into(),
            command_state: TerminalCommandState {
                revision: 5,
                status: TerminalCommandStatus::Running,
                command_id: Some("command-5".into()),
                command_count: 2,
            },
        })
        .unwrap();
        assert_eq!(value["type"], "command_state");
        assert_eq!(value["sessionId"], "session-1");
        assert_eq!(value["commandState"]["revision"], 5);
        assert_eq!(value["commandState"]["status"], "running");
        assert_eq!(value["commandState"]["commandId"], "command-5");
        assert_eq!(value["commandState"]["commandCount"], 2);

        let ready = serde_json::to_value(TerminalEvent::Ready {
            session_id: "session-1".into(),
        })
        .unwrap();
        assert_eq!(ready["type"], "ready");
        assert_eq!(ready["sessionId"], "session-1");
    }

    #[test]
    fn powershell_bootstrap_hides_control_secrets_before_loading_profiles() {
        let mut args = Vec::new();
        configure_powershell_control(&mut args);
        assert_eq!(args.first(), Some(&OsString::from("-NoProfile")));
        assert_eq!(args.get(2), Some(&OsString::from("-EncodedCommand")));
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(args.last().unwrap().to_string_lossy().as_bytes())
            .unwrap();
        let utf16 = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        let script = String::from_utf16(&utf16).unwrap();
        let clear_nonce = script
            .find("SetEnvironmentVariable('MEWORK_TERMINAL_CONTROL_NONCE'")
            .unwrap();
        let load_profiles = script.find("$meworkProfiles = @(").unwrap();
        assert!(clear_nonce < load_profiles);
        // The console leaves the OEM code page before any profile runs, and a
        // profile that sets its own encoding still has the last word.
        let output_utf8 = script
            .find("[Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false)")
            .expect("pseudo console switched to UTF-8");
        assert!(clear_nonce < output_utf8 && output_utf8 < load_profiles);
        assert!(script.contains("[Console]::InputEncoding=[System.Text.UTF8Encoding]::new($false)"));
        assert!(script.contains("$OutputEncoding=[Console]::OutputEncoding"));
        assert!(script.contains("'Get-Content','Set-Content'"));
        assert!(script.contains("$PSDefaultParameterValues[\"${_}:Encoding\"]='utf8'"));
        // Interactive-only behaviour is not inherited from the tool preamble: a
        // person's terminal keeps its progress bars and its real width.
        assert!(!script.contains("$ProgressPreference"));
        assert!(!script.contains("BufferSize"));
        assert!(script.contains("function global:PSConsoleHostReadLine"));
        assert!(script.contains("'start' $meworkGeneration"));
        assert!(script.contains("'end' $script:__MeworkTerminalActiveGeneration"));
        assert!(!script.contains("function global:prompt"));
    }

    /// The whole barrier against the real host shell: PowerShell under ConPTY on
    /// Windows, a login zsh under a pty elsewhere, with the user's own profile.
    #[test]
    #[ignore = "spawns the real host shell under a pseudo terminal"]
    fn real_shell_barrier_survives_prompt_calls_fast_queue_and_detach() {
        exercise_real_shell_barrier(None);
    }

    /// The same against bash: Git Bash on Windows, and elsewhere the bash the
    /// menu would start — macOS's 3.2 when nothing newer is installed.
    #[test]
    #[ignore = "spawns a real bash under a pseudo terminal"]
    fn real_bash_barrier_survives_prompt_calls_fast_queue_and_detach() {
        exercise_real_shell_barrier(Some(TerminalShell::Bash));
    }

    /// And against fish, where it is installed.
    #[cfg(unix)]
    #[test]
    #[ignore = "spawns a real fish under a pseudo terminal"]
    fn real_fish_barrier_survives_prompt_calls_fast_queue_and_detach() {
        exercise_real_shell_barrier(Some(TerminalShell::Fish));
    }

    fn exercise_real_shell_barrier(shell: Option<TerminalShell>) {
        use std::{
            sync::mpsc,
            time::{Duration, Instant},
        };
        use tauri::ipc::InvokeResponseBody;

        fn event_channel() -> (Channel<TerminalEvent>, mpsc::Receiver<serde_json::Value>) {
            let (sender, receiver) = mpsc::channel();
            let channel = Channel::new(move |body| {
                let value = match body {
                    InvokeResponseBody::Json(json) => serde_json::from_str(&json)?,
                    InvokeResponseBody::Raw(bytes) => serde_json::to_value(bytes)?,
                };
                let _ = sender.send(value);
                Ok(())
            });
            (channel, receiver)
        }

        fn wait_for_status(
            receiver: &mpsc::Receiver<serde_json::Value>,
            status: &str,
            timeout: Duration,
        ) -> serde_json::Value {
            let deadline = Instant::now() + timeout;
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                let event = receiver
                    .recv_timeout(remaining)
                    .unwrap_or_else(|error| panic!("did not receive {status}: {error}"));
                if event["type"] == "command_state" && event["commandState"]["status"] == status {
                    return event;
                }
            }
        }

        fn assert_no_idle_for(receiver: &mpsc::Receiver<serde_json::Value>, duration: Duration) {
            let deadline = Instant::now() + duration;
            while Instant::now() < deadline {
                let remaining = deadline.saturating_duration_since(Instant::now());
                match receiver.recv_timeout(remaining) {
                    Ok(event)
                        if event["type"] == "command_state"
                            && event["commandState"]["status"] == "idle" =>
                    {
                        panic!("command became idle before the top-level command returned")
                    }
                    Ok(_) => {}
                    Err(mpsc::RecvTimeoutError::Timeout) => break,
                    Err(error) => panic!("terminal event channel disconnected: {error}"),
                }
            }
        }

        fn complete_terminal_handshake(
            manager: &TerminalManager,
            receiver: &mpsc::Receiver<serde_json::Value>,
            session_id: &str,
        ) {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut pending = Vec::new();
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                let event = receiver
                    .recv_timeout(remaining)
                    .unwrap_or_else(|error| panic!("terminal did not become ready: {error}"));
                if event["type"] == "ready" {
                    return;
                }
                if event["type"] == "error" {
                    panic!("terminal initialization failed: {}", event["message"]);
                }
                if event["type"] != "output" {
                    continue;
                }
                pending.extend(
                    event["data"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(serde_json::Value::as_u64)
                        .map(|byte| byte as u8),
                );
                while let Some(offset) = pending.windows(4).position(|bytes| bytes == b"\x1b[6n") {
                    manager
                        .write(
                            "conversation-real",
                            "terminal-real",
                            session_id,
                            "\x1b[1;1R",
                        )
                        .unwrap();
                    pending.drain(..offset + 4);
                }
                if pending.len() > 32 {
                    pending.drain(..pending.len() - 32);
                }
            }
        }

        let manager = TerminalManager::default();
        let (sink, receiver) = event_channel();
        let active = Arc::new(AtomicBool::new(false));
        let drops = Arc::new(AtomicUsize::new(0));
        let factory_active = active.clone();
        let factory_drops = drops.clone();
        let factory: TerminalCommandLeaseFactory = Arc::new(move || {
            factory_active
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .map_err(|_| "command lease already active".to_owned())?;
            Ok(Box::new(ExclusiveProbe {
                active: factory_active.clone(),
                drops: factory_drops.clone(),
            }))
        });
        let startup_drops = Arc::new(AtomicUsize::new(0));
        let launch = match TerminalLaunch::host(&std::env::current_dir().unwrap(), shell) {
            Ok(launch) => launch,
            // A chosen shell the machine does not have is refused up front.
            Err(error) if shell.is_some() => {
                eprintln!("skipped: {error}");
                return;
            }
            Err(error) => panic!("{error}"),
        };
        // The first command touches the shell's own prompt machinery, which must
        // not be taken for the command ending.
        let (slow_then_print, two_queued_lines, short_sleep) = match launch.control {
            #[cfg(windows)]
            ShellControl::PowerShell => (
                "prompt; Start-Sleep -Milliseconds 600; Write-Output mework-finished\r",
                "Write-Output first\rWrite-Output second\r",
                "Start-Sleep -Milliseconds 350\r",
            ),
            #[cfg(unix)]
            ShellControl::Zsh => (
                "print -P '%~' >/dev/null; sleep 0.6; echo mework-finished\r",
                "echo first\recho second\r",
                "sleep 0.35\r",
            ),
            ShellControl::Bash => (
                "printf '%s' \"${PS1-}\" >/dev/null; sleep 0.6; echo mework-finished\r",
                "echo first\recho second\r",
                "sleep 0.35\r",
            ),
            #[cfg(unix)]
            ShellControl::Fish => (
                "fish_prompt >/dev/null; sleep 0.6; echo mework-finished\r",
                "echo first\recho second\r",
                "sleep 0.35\r",
            ),
            // A host without its default shell refuses to open a terminal at
            // all; there is no barrier to exercise.
            ShellControl::Unsupported => return,
            ShellControl::Remote => unreachable!("a host launch is never remote"),
        };
        let opened = manager
            .open(
                "conversation-real",
                "terminal-real",
                launch,
                100,
                30,
                sink,
                Box::new(DropProbe(startup_drops.clone())),
                factory.clone(),
            )
            .unwrap();
        assert_eq!(opened.command_state.status, TerminalCommandStatus::Idle);
        if !opened.ready {
            complete_terminal_handshake(&manager, &receiver, &opened.session_id);
        }
        assert_eq!(startup_drops.load(Ordering::Acquire), 1);

        manager
            .write(
                "conversation-real",
                "terminal-real",
                &opened.session_id,
                slow_then_print,
            )
            .unwrap();
        let running = wait_for_status(&receiver, "running", Duration::from_secs(5));
        assert_eq!(running["commandState"]["commandCount"], 1);
        assert!(active.load(Ordering::Acquire));
        assert_no_idle_for(&receiver, Duration::from_millis(250));
        let idle = wait_for_status(&receiver, "idle", Duration::from_secs(5));
        assert_eq!(idle["commandState"]["revision"], 2);
        assert!(!active.load(Ordering::Acquire));
        assert_eq!(drops.load(Ordering::Acquire), 1);

        manager
            .write(
                "conversation-real",
                "terminal-real",
                &opened.session_id,
                two_queued_lines,
            )
            .unwrap();
        for expected in [2_u64, 3] {
            let running = wait_for_status(&receiver, "running", Duration::from_secs(5));
            assert_eq!(running["commandState"]["commandCount"], expected);
            let _ = wait_for_status(&receiver, "idle", Duration::from_secs(5));
        }
        assert_eq!(drops.load(Ordering::Acquire), 3);

        manager
            .write(
                "conversation-real",
                "terminal-real",
                &opened.session_id,
                short_sleep,
            )
            .unwrap();
        let _ = wait_for_status(&receiver, "running", Duration::from_secs(5));
        assert!(manager.detach("conversation-real", "terminal-real", &opened.session_id));
        let deadline = Instant::now() + Duration::from_secs(5);
        while drops.load(Ordering::Acquire) != 4 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(drops.load(Ordering::Acquire), 4);
        assert!(!active.load(Ordering::Acquire));

        let (reattach_sink, _reattach_receiver) = event_channel();
        let reattached = manager
            .open(
                "conversation-real",
                "terminal-real",
                TerminalLaunch::host(&std::env::current_dir().unwrap(), shell).unwrap(),
                100,
                30,
                reattach_sink,
                Box::new(DropProbe(startup_drops.clone())),
                factory,
            )
            .unwrap();
        assert!(!reattached.created);
        assert_eq!(reattached.session_id, opened.session_id);
        assert_eq!(reattached.command_state.status, TerminalCommandStatus::Idle);
        assert_eq!(reattached.command_state.command_count, 4);
        assert!(manager.close("conversation-real", "terminal-real"));
    }
}
