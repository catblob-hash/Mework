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

const MAX_BUFFER_BYTES: usize = 2 * 1024 * 1024;
const MAX_INPUT_BYTES: usize = 64 * 1024;
const MAX_TERMINAL_SESSIONS: usize = 64;
const MAX_COLS: u16 = 500;
const MAX_ROWS: u16 = 300;
const CONTROL_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const CONTROL_PREFIX: &[u8] = b"\x1b]633;Mework;v1;";
const CONTROL_TERMINATOR: u8 = 0x07;

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
    PowerShell,
    Unsupported,
}

pub struct TerminalLaunch {
    program: OsString,
    args: Vec<OsString>,
    cwd: PathBuf,
    display_cwd: String,
    shell: String,
    binding: String,
    control: ShellControl,
}

impl TerminalLaunch {
    pub fn host(cwd: &Path) -> Result<Self, String> {
        let cwd = canonical_directory(cwd)?;
        let (program, args, shell, control) = host_shell();
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
        }
    }
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
    control_ack_event: String,
    control_reject_event: String,
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
    /// Kills the shell. The sink stays attached so the waiter can still report
    /// the exit: a panel that did not ask for this close — the task list did,
    /// or a workspace closing — has no other way to learn its shell is gone.
    fn terminate(mut self) {
        lock(&self.output).closed = true;
        mark_handshake(&self.control_handshake, ControlHandshake::Failed);
        if let Some(mut writer) = lock(&self.writer).take() {
            let _ = writer.flush();
        }
        if !kill_process_tree(self.process_id, self.process_group_id) {
            if let Err(error) = self.killer.kill() {
                eprintln!("关闭终端 {} 的 shell 失败：{error}", self.session_id);
            }
        }
        // Closing the pseudo console after terminating the shell wakes the blocking reader. The
        // waiter thread owns and reaps the child handle.
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

        let pair = native_pty_system()
            .openpty(size)
            .map_err(|error| format!("无法创建伪终端：{error}"))?;
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| format!("无法读取伪终端：{error}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| format!("无法写入伪终端：{error}"))?;
        if launch.control != ShellControl::PowerShell {
            return Err(
                "当前系统没有可用的 PowerShell；为保证 Git 与终端命令严格互斥，未启动不受支持的 shell"
                    .into(),
            );
        }
        let control_nonce = Uuid::new_v4().simple().to_string();
        let control_ack_event = format!("MeworkTerminalAck_{}", Uuid::new_v4().simple());
        let control_reject_event = format!("MeworkTerminalReject_{}", Uuid::new_v4().simple());
        let mut launch_args = launch.args.clone();
        configure_powershell_control(&mut launch_args);
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
        }
        command.env("MEWORK_TERMINAL_CONTROL_NONCE", &control_nonce);
        command.env("MEWORK_TERMINAL_ACK_EVENT", &control_ack_event);
        command.env("MEWORK_TERMINAL_REJECT_EVENT", &control_reject_event);
        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| format!("无法启动终端 shell：{error}"))?;
        drop(pair.slave);

        let session_id = Uuid::new_v4().to_string();
        let control_handshake = Arc::new((Mutex::new(ControlHandshake::Pending), Condvar::new()));
        let output = Arc::new(Mutex::new(TerminalOutputState {
            buffer: VecDeque::new(),
            sink: Some(sink),
            running: true,
            closed: false,
            control: Some(TerminalControlParser::new(&control_nonce)),
            control_ready: false,
            last_control_generation: 0,
            active_control_generation: None,
            command_state: TerminalCommandState::default(),
            command_lease: None,
            startup_lease: Some(startup_lease),
            command_lease_factory,
            control_ack_event,
            control_reject_event,
        }));
        let process_id = child.process_id();
        #[cfg(unix)]
        let process_group_id = pair.master.process_group_leader();
        #[cfg(not(unix))]
        let process_group_id = None;
        let mut killer = child.clone_killer();
        let master = Arc::new(Mutex::new(Some(pair.master)));
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
        let session = lock(&self.sessions).remove(&TerminalKey::new(conversation_id, terminal_id));
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
            remove_matching_sessions(&mut sessions, |key| {
                conversations.contains(key.conversation_id.as_str())
            })
        };
        removed.into_iter().for_each(TerminalSession::terminate);
    }

    pub fn close_all(&self) {
        let removed = {
            let mut sessions = lock(&self.sessions);
            sessions
                .drain()
                .map(|(_, session)| session)
                .collect::<Vec<_>>()
        };
        removed.into_iter().for_each(TerminalSession::terminate);
    }
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
        sessions
            .drain()
            .for_each(|(_, session)| session.terminate());
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
        while buffer
            .get(excess)
            .is_some_and(|byte| (byte & 0xC0) == 0x80)
        {
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
                let _ = signal_control_event(&state.control_reject_event);
                return None;
            }
            state.last_control_generation = frame.generation;
            let command_lease = match (state.command_lease_factory)() {
                Ok(command_lease) => command_lease,
                Err(error) => {
                    eprintln!("终端命令因工作区操作冲突被拒绝：{error}");
                    let _ = signal_control_event(&state.control_reject_event);
                    state.command_state.revision = state.command_state.revision.saturating_add(1);
                    return Some(TerminalControlUpdate::CommandState(
                        state.command_state.clone(),
                    ));
                }
            };
            state.command_lease = Some(command_lease);
            state.active_control_generation = Some(frame.generation);
            if let Err(error) = signal_control_event(&state.control_ack_event) {
                eprintln!("无法确认终端命令租约：{error}");
                state.active_control_generation = None;
                drop(state.command_lease.take());
                let _ = signal_control_event(&state.control_reject_event);
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

#[cfg(not(windows))]
fn signal_control_event(_name: &str) -> Result<(), String> {
    Err("当前平台不支持终端命令确认事件".into())
}

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

#[cfg(windows)]
fn host_shell() -> (OsString, Vec<OsString>, String, ShellControl) {
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

#[cfg(not(windows))]
fn host_shell() -> (OsString, Vec<OsString>, String, ShellControl) {
    let program = env::var_os("SHELL")
        .filter(|value| Path::new(value).is_file())
        .or_else(|| {
            ["/bin/zsh", "/bin/bash", "/bin/sh"]
                .into_iter()
                .find(|path| Path::new(path).is_file())
                .map(OsString::from)
        })
        .unwrap_or_else(|| OsString::from("/bin/sh"));
    let label = Path::new(&program)
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("shell")
        .to_owned();
    (program, Vec::new(), label, ShellControl::Unsupported)
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
        let launch = TerminalLaunch::host(&std::env::current_dir().unwrap()).unwrap();
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
    fn session_removal_targets_every_terminal_owned_by_invalidated_conversations() {
        let mut sessions = HashMap::from([
            (TerminalKey::new("conversation-a", "terminal-a"), "a"),
            (TerminalKey::new("conversation-b", "terminal-b"), "b"),
            (TerminalKey::new("conversation-a", "terminal-c"), "c"),
        ]);

        let mut removed = remove_matching_sessions(&mut sessions, |key| {
            key.conversation_id == "conversation-a"
        });
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

        let removed = remove_matching_sessions(&mut sessions, |key| {
            key.conversation_id == "conversation-a"
        });
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
            control_ack_event: "unused-ack".into(),
            control_reject_event: "unused-reject".into(),
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
            control_ack_event: "unused-ack".into(),
            control_reject_event: "unused-reject".into(),
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

    #[cfg(windows)]
    #[test]
    #[ignore = "spawns the real PowerShell console host and ConPTY"]
    fn real_powershell_barrier_survives_prompt_calls_fast_queue_and_detach() {
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
        let launch = TerminalLaunch::host(&std::env::current_dir().unwrap()).unwrap();
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
                "prompt; Start-Sleep -Milliseconds 600; Write-Output mework-finished\r",
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
                "Write-Output first\rWrite-Output second\r",
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
                "Start-Sleep -Milliseconds 350\r",
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
                TerminalLaunch::host(&std::env::current_dir().unwrap()).unwrap(),
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
