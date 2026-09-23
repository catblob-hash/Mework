//! The agent and the link, end to end, on this machine.
//!
//! Each test starts the real `mework-remote proxy` as its transport — exactly
//! what SSH runs on a remote machine, minus SSH — against a daemon rooted in a
//! private temporary directory. Killing that proxy is what a dropped network
//! looks like from both ends, so the reconnect paths here are the real ones.
//!
//! The same suite runs on Unix and on Windows. Where a test needs a program of
//! the machine's own, the Windows version speaks `cmd.exe` or PowerShell —
//! nothing here assumes Git for Windows except the checks that say so — and a
//! few tests exist only on Windows: its pseudo console, its job objects, and
//! the daemon leaving a job the way it must leave sshd's.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use remote_agent::client::{LaunchError, Launcher, Link, LinkConfig, LinkStatus, Transport};
use remote_agent::protocol::{
    self, ExitReason, Message, Op, Policy, Preamble, Reply, SpawnSpec, StdinMode, TerminalSize,
};

const AGENT: &str = env!("CARGO_BIN_EXE_mework-remote");

/// A root short enough for a Unix socket path, removed with the test.
#[cfg(unix)]
fn scratch_root() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("mwr")
        .tempdir_in("/tmp")
        .expect("a scratch directory under /tmp")
}

#[cfg(windows)]
fn scratch_root() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("mwr")
        .tempdir()
        .expect("a scratch directory")
}

/// A program that runs for about `seconds`, whose pid is the session's.
fn long_running(seconds: u32) -> Vec<String> {
    if cfg!(windows) {
        ["ping", "-n", &(seconds + 1).to_string(), "127.0.0.1"].map(str::to_owned).to_vec()
    } else {
        vec!["sleep".to_owned(), seconds.to_string()]
    }
}

/// Runs the proxy directly, the way an SSH session would, and remembers the
/// running one so a test can kill it to simulate a dropped link.
struct DirectLauncher {
    root: PathBuf,
    idle_exit: u64,
    current: Arc<Mutex<Option<Child>>>,
    launches: Arc<Mutex<u32>>,
    /// While set, every launch fails as an unreachable machine would.
    blocked: Arc<std::sync::atomic::AtomicBool>,
}

impl DirectLauncher {
    fn new(root: &Path, idle_exit: u64) -> Self {
        Self {
            root: root.to_path_buf(),
            idle_exit,
            current: Arc::new(Mutex::new(None)),
            launches: Arc::new(Mutex::new(0)),
            blocked: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    fn blocker(&self) -> Arc<std::sync::atomic::AtomicBool> {
        Arc::clone(&self.blocked)
    }

    fn handle(&self) -> (Arc<Mutex<Option<Child>>>, Arc<Mutex<u32>>) {
        (Arc::clone(&self.current), Arc::clone(&self.launches))
    }
}

impl Launcher for DirectLauncher {
    fn launch(&self, nonce: &str) -> Result<Transport, LaunchError> {
        if self.blocked.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(LaunchError::Unreachable("ssh: Network is unreachable".into()));
        }
        let mut child = Command::new(AGENT)
            .args([
                "proxy",
                "--sync",
                nonce,
                "--idle-exit",
                &self.idle_exit.to_string(),
                "--tick-ms",
                "100",
            ])
            .env("MEWORK_REMOTE_ROOT", &self.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| LaunchError::Unreachable(error.to_string()))?;
        let mut reader = child.stdout.take().unwrap();
        let writer = child.stdin.take().unwrap();
        match protocol::read_preamble(&mut reader, nonce) {
            Ok(Preamble::Ready) => {}
            Ok(other) => return Err(LaunchError::Unavailable(format!("{other:?}"))),
            Err(error) => return Err(LaunchError::Unreachable(error.to_string())),
        }
        *self.launches.lock().unwrap() += 1;
        *self.current.lock().unwrap() = Some(child);
        let current = Arc::clone(&self.current);
        Ok(Transport {
            reader: Box::new(reader),
            writer: Box::new(writer),
            closer: Box::new(move || {
                if let Some(mut child) = current.lock().unwrap().take() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }),
        })
    }
}

fn kill_transport(current: &Arc<Mutex<Option<Child>>>) {
    if let Some(child) = current.lock().unwrap().as_mut() {
        let _ = child.kill();
    }
}

fn config(client: &str, epoch: &str) -> LinkConfig {
    let mut config = LinkConfig::new(client, epoch);
    config.ping_interval = Duration::from_millis(300);
    config.dead_after = Duration::from_secs(3);
    config.backoff_initial = Duration::from_millis(50);
    config.backoff_max = Duration::from_millis(400);
    config.give_up_after = Duration::from_secs(10);
    config
}

fn spec(sid: &str, argv: &[&str]) -> SpawnSpec {
    SpawnSpec {
        sid: sid.into(),
        argv: argv.iter().map(|arg| (*arg).to_owned()).collect(),
        cwd: None,
        env: Default::default(),
        env_remove: Vec::new(),
        terminal: None,
        stdin: StdinMode::Null,
        output_limit: None,
        orphan_ttl_secs: None,
        label: None,
    }
}

fn spec_of(sid: &str, argv: Vec<String>) -> SpawnSpec {
    let mut spec = spec(sid, &[]);
    spec.argv = argv;
    spec
}

fn read_all(reader: &mut impl Read) -> String {
    let mut text = String::new();
    reader.read_to_string(&mut text).unwrap();
    text
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return false;
    }
    let mut code = 0u32;
    let queried = unsafe { GetExitCodeProcess(handle, &mut code) } != 0;
    unsafe { CloseHandle(handle) };
    queried && code == STILL_ACTIVE as u32
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    condition()
}

const CALL: Duration = Duration::from_secs(20);

#[cfg(unix)]
#[test]
fn a_command_runs_with_separate_streams_and_its_exit_code() {
    let root = scratch_root();
    let link = Link::start(config("host-a", "e1"), DirectLauncher::new(root.path(), 3));
    link.wait_connected(CALL).unwrap();
    let mut process = link
        .spawn(
            spec("s1", &["sh", "-c", "echo out; echo err >&2; printf '中文'; exit 3"]),
            b"",
            CALL,
        )
        .unwrap();
    let mut stdout = process.take_stdout().unwrap();
    let mut stderr = process.take_stderr().unwrap();
    assert_eq!(read_all(&mut stdout), "out\n中文");
    assert_eq!(read_all(&mut stderr), "err\n");
    let exit = process.wait().unwrap();
    assert_eq!(exit.code, Some(3));
    assert_eq!(exit.reason, ExitReason::Exited);
    link.close(true);
}

#[cfg(unix)]
#[test]
fn a_spawn_carries_its_stdin_body_cwd_and_environment() {
    let root = scratch_root();
    let link = Link::start(config("host-a", "e1"), DirectLauncher::new(root.path(), 3));
    let mut with_input = spec("s1", &["cat"]);
    with_input.stdin = StdinMode::Body;
    let mut process = link.spawn(with_input, b"payload \x00\xff bytes", CALL).unwrap();
    let mut out = Vec::new();
    process.take_stdout().unwrap().read_to_end(&mut out).unwrap();
    assert_eq!(out, b"payload \x00\xff bytes");

    let mut placed = spec("s2", &["sh", "-c", "pwd; echo \"$MEWORK_TEST_VALUE\"; echo \"${HOME+set}\""]);
    placed.cwd = Some("/tmp".into());
    placed.env.insert("MEWORK_TEST_VALUE".into(), "it's here".into());
    let mut process = link.spawn(placed, b"", CALL).unwrap();
    let text = read_all(&mut process.take_stdout().unwrap());
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].ends_with("/tmp"), "{text}");
    assert_eq!(lines[1], "it's here");
    assert_eq!(lines[2], "set", "the login environment is inherited");

    let missing = link.spawn(spec("s3", &["mework-no-such-program"]), b"", CALL);
    let error = missing.unwrap_err().to_string();
    assert!(error.contains("was not found"), "{error}");
    let mut bad_cwd = spec("s4", &["true"]);
    bad_cwd.cwd = Some("/definitely/not/here".into());
    assert!(link.spawn(bad_cwd, b"", CALL).unwrap_err().to_string().contains("does not exist"));
    link.close(true);
}

/// The whole point: the link drops in the middle of a command, and the
/// command neither dies nor loses a line.
#[cfg(unix)]
#[test]
fn output_resumes_exactly_after_the_transport_is_killed() {
    let root = scratch_root();
    let launcher = DirectLauncher::new(root.path(), 3);
    let (current, launches) = launcher.handle();
    let link = Link::start(config("host-a", "e1"), launcher);
    let mut process = link
        .spawn(
            spec("s1", &["sh", "-c", "i=1; while [ $i -le 40 ]; do echo line$i; i=$((i+1)); sleep 0.05; done"]),
            b"",
            CALL,
        )
        .unwrap();
    let pid = process.pid();
    let mut stdout = BufReader::new(process.take_stdout().unwrap());
    let mut lines = Vec::new();
    let mut line = String::new();
    for _ in 0..5 {
        line.clear();
        stdout.read_line(&mut line).unwrap();
        lines.push(line.trim().to_owned());
    }
    // Two drops, one of them while the process keeps printing.
    kill_transport(&current);
    std::thread::sleep(Duration::from_millis(300));
    kill_transport(&current);
    loop {
        line.clear();
        if stdout.read_line(&mut line).unwrap() == 0 {
            break;
        }
        lines.push(line.trim().to_owned());
    }
    let expected: Vec<String> = (1..=40).map(|i| format!("line{i}")).collect();
    assert_eq!(lines, expected);
    let exit = process.wait().unwrap();
    assert_eq!(exit.code, Some(0));
    assert!(*launches.lock().unwrap() >= 2, "the link reconnected");
    assert!(!process_alive(pid) || wait_until(Duration::from_secs(2), || !process_alive(pid)));
    link.close(true);
}

#[cfg(unix)]
#[test]
fn input_typed_while_the_link_is_down_arrives_in_order_once() {
    let root = scratch_root();
    let launcher = DirectLauncher::new(root.path(), 3);
    let (current, _) = launcher.handle();
    let link = Link::start(config("host-a", "e1"), launcher);
    let mut cat = spec("s1", &["cat"]);
    cat.stdin = StdinMode::Pipe;
    let mut process = link.spawn(cat, b"", CALL).unwrap();
    let mut stdin = process.stdin();
    let mut stdout = BufReader::new(process.take_stdout().unwrap());
    stdin.write_all(b"first\n").unwrap();
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    assert_eq!(line, "first\n");

    kill_transport(&current);
    // Written into a dead link: kept, then delivered after the reconnect.
    stdin.write_all(b"second\n").unwrap();
    stdin.write_all(b"third\n").unwrap();
    line.clear();
    stdout.read_line(&mut line).unwrap();
    assert_eq!(line, "second\n");
    line.clear();
    stdout.read_line(&mut line).unwrap();
    assert_eq!(line, "third\n");
    process.close_stdin();
    line.clear();
    assert_eq!(stdout.read_line(&mut line).unwrap(), 0);
    assert_eq!(process.wait().unwrap().code, Some(0));
    link.close(true);
}

#[cfg(unix)]
#[test]
fn a_terminal_session_echoes_resizes_and_exits() {
    let root = scratch_root();
    let link = Link::start(config("host-a", "e1"), DirectLauncher::new(root.path(), 3));
    let mut terminal = spec("t1", &["/bin/sh"]);
    terminal.terminal = Some(TerminalSize { cols: 80, rows: 24 });
    terminal.stdin = StdinMode::Pipe;
    terminal.env.insert("PS1".into(), "$ ".into());
    let mut process = link.spawn(terminal, b"", CALL).unwrap();
    let output = Arc::new(Mutex::new(Vec::<u8>::new()));
    {
        let output = Arc::clone(&output);
        let mut reader = process.take_stdout().unwrap();
        std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            while let Ok(count) = reader.read(&mut chunk) {
                if count == 0 {
                    break;
                }
                output.lock().unwrap().extend_from_slice(&chunk[..count]);
            }
        });
    }
    process.resize(TerminalSize { cols: 132, rows: 40 });
    let mut stdin = process.stdin();
    stdin.write_all(b"stty size; echo mark-$((6*7))\n").unwrap();
    assert!(
        wait_until(Duration::from_secs(10), || {
            let text = String::from_utf8_lossy(&output.lock().unwrap()).into_owned();
            text.contains("40 132") && text.contains("mark-42")
        }),
        "{}",
        String::from_utf8_lossy(&output.lock().unwrap())
    );
    stdin.write_all(b"exit 5\n").unwrap();
    let exit = process.wait().unwrap();
    assert_eq!(exit.code, Some(5));
    link.close(true);
}

/// A host that vanishes without saying goodbye leaves its sessions behind for
/// exactly the orphan time it asked for.
#[test]
fn an_orphaned_session_is_reclaimed_after_its_ttl() {
    let root = scratch_root();
    let mut short = config("host-a", "e1");
    // Wide enough that a loaded machine running the whole suite in parallel
    // cannot blur "a short absence" into "the orphan time".
    short.policy = Policy {
        orphan_ttl_secs: 4,
        silence_timeout_secs: 5,
        finished_ttl_secs: 60,
    };
    let link = Link::start(short.clone(), DirectLauncher::new(root.path(), 3));
    let process = link.spawn(spec_of("s1", long_running(60)), b"", CALL).unwrap();
    let pid = process.pid();
    std::mem::forget(process);
    link.close(false);
    assert!(process_alive(pid), "a dropped host's session outlives the link");
    std::thread::sleep(Duration::from_millis(1200));
    assert!(process_alive(pid), "and survives a short absence");
    assert!(
        wait_until(Duration::from_secs(10), || !process_alive(pid)),
        "but not its orphan time"
    );

    // The same host process coming back finds it ended, and why.
    let again = Link::start(short, DirectLauncher::new(root.path(), 3));
    let Reply::Sessions { sessions } = again.call(Op::Sessions, b"", CALL).unwrap() else {
        panic!("expected sessions")
    };
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].exit.as_ref().unwrap().reason, ExitReason::Reclaimed);
    again.close(true);
}

/// A host process that restarts cannot reach its predecessor's sessions, so
/// they end at once rather than waiting out their orphan time.
#[test]
fn a_new_host_epoch_reclaims_the_previous_epochs_sessions() {
    let root = scratch_root();
    let first = Link::start(config("host-a", "e1"), DirectLauncher::new(root.path(), 3));
    let process = first.spawn(spec_of("s1", long_running(60)), b"", CALL).unwrap();
    let pid = process.pid();
    std::mem::forget(process);
    first.close(false);

    // Another installation on the same machine is someone else entirely.
    let neighbour = Link::start(config("host-b", "x1"), DirectLauncher::new(root.path(), 3));
    neighbour.wait_connected(CALL).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert!(process_alive(pid));

    let restarted = Link::start(config("host-a", "e2"), DirectLauncher::new(root.path(), 3));
    restarted.wait_connected(CALL).unwrap();
    assert!(wait_until(Duration::from_secs(3), || !process_alive(pid)));
    let Reply::Sessions { sessions } = restarted.call(Op::Sessions, b"", CALL).unwrap() else {
        panic!("expected sessions")
    };
    assert!(sessions.is_empty());
    neighbour.close(true);
    restarted.close(true);
}

/// Releasing kills a session's whole process group, not only its root.
#[cfg(unix)]
#[test]
fn releasing_a_session_ends_its_whole_process_tree() {
    let root = scratch_root();
    let link = Link::start(config("host-a", "e1"), DirectLauncher::new(root.path(), 3));
    let mut process = link
        .spawn(
            spec("s1", &["sh", "-c", "sleep 60 & echo $!; wait"]),
            b"",
            CALL,
        )
        .unwrap();
    let mut stdout = BufReader::new(process.take_stdout().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let grandchild: u32 = line.trim().parse().unwrap();
    assert!(process_alive(grandchild));
    process.release();
    assert!(wait_until(Duration::from_secs(3), || !process_alive(grandchild)));
    link.close(true);
}

/// Output the agent could not hold while the host was away is reported as a
/// gap, never silently spliced.
#[cfg(unix)]
#[test]
fn output_dropped_while_disconnected_is_counted_not_spliced() {
    let root = scratch_root();
    let launcher = DirectLauncher::new(root.path(), 3);
    let (current, _) = launcher.handle();
    let blocked = launcher.blocker();
    let link = Link::start(config("host-a", "e1"), launcher);
    let mut noisy = spec(
        "s1",
        &["sh", "-c", "sleep 0.5; i=0; while [ $i -lt 2000 ]; do echo 0123456789abcdefghijklmnopqrstuvwxyz; i=$((i+1)); done"],
    );
    noisy.output_limit = Some(4096);
    let mut process = link.spawn(noisy, b"", CALL).unwrap();
    let mut stdout = process.take_stdout().unwrap();
    // The link goes down before the command prints anything and stays down
    // until it has finished: 74 000 bytes into a 4 KiB ring.
    blocked.store(true, std::sync::atomic::Ordering::SeqCst);
    kill_transport(&current);
    std::thread::sleep(Duration::from_millis(2500));
    assert!(matches!(link.status(), LinkStatus::Reconnecting { .. }), "{:?}", link.status());
    blocked.store(false, std::sync::atomic::Ordering::SeqCst);
    let mut received = Vec::new();
    stdout.read_to_end(&mut received).unwrap();
    let exit = process.wait().unwrap();
    assert_eq!(exit.code, Some(0));
    let total = 2000 * 37u64;
    assert_eq!(exit.ends.stdout, total);
    assert!(process.lost_bytes() > 0, "the ring must have overflowed");
    assert!(received.len() <= 4096, "only what the ring still held: {}", received.len());
    assert_eq!(received.len() as u64 + process.lost_bytes(), total);
    assert!(received.ends_with(b"xyz\n"));
    link.close(true);
}

/// A link that is down when a request is made delivers it once it is back.
#[test]
fn a_request_made_while_the_link_is_down_is_delivered_after_it_returns() {
    let root = scratch_root();
    let launcher = DirectLauncher::new(root.path(), 3);
    let (current, _) = launcher.handle();
    let blocked = launcher.blocker();
    let link = Link::start(config("host-a", "e1"), launcher);
    link.wait_connected(CALL).unwrap();
    blocked.store(true, std::sync::atomic::Ordering::SeqCst);
    kill_transport(&current);
    let unblock = {
        let blocked = Arc::clone(&blocked);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(800));
            blocked.store(false, std::sync::atomic::Ordering::SeqCst);
        })
    };
    let started = Instant::now();
    let echo = if cfg!(windows) {
        spec("s1", &["cmd.exe", "/d", "/c", "echo late"])
    } else {
        spec("s1", &["echo", "late"])
    };
    let mut process = link.spawn(echo, b"", CALL).unwrap();
    assert!(started.elapsed() >= Duration::from_millis(700));
    let expected = if cfg!(windows) { "late\r\n" } else { "late\n" };
    assert_eq!(read_all(&mut process.take_stdout().unwrap()), expected);
    unblock.join().unwrap();
    link.close(true);
}

/// The agent drops a connection that stops sending, even though its socket
/// never closed; the link notices and replaces it.
#[test]
fn a_silent_connection_is_dropped_by_the_agent_and_replaced() {
    let root = scratch_root();
    let launcher = DirectLauncher::new(root.path(), 3);
    let (_, launches) = launcher.handle();
    let mut quiet = config("host-a", "e1");
    // The host never pings within the agent's silence limit.
    quiet.ping_interval = Duration::from_secs(60);
    quiet.dead_after = Duration::from_secs(120);
    quiet.policy.silence_timeout_secs = 5;
    let link = Link::start(quiet, launcher);
    link.wait_connected(CALL).unwrap();
    assert!(wait_until(Duration::from_secs(12), || *launches.lock().unwrap() >= 2));
    link.wait_connected(CALL).unwrap();
    link.close(true);
}

#[cfg(unix)]
#[test]
fn which_resolves_programs_on_the_machine() {
    let root = scratch_root();
    let link = Link::start(config("host-a", "e1"), DirectLauncher::new(root.path(), 3));
    let Reply::Which { found } = link
        .call(
            Op::Which {
                names: vec!["sh".into(), "mework-no-such-program".into()],
            },
            b"",
            CALL,
        )
        .unwrap()
    else {
        panic!("expected which")
    };
    assert!(found["sh"].as_deref().is_some_and(|path| path.ends_with("/sh")));
    assert_eq!(found["mework-no-such-program"], None);
    link.close(true);
}

/// A daemon with nothing to do leaves, and takes its socket with it.
#[test]
fn an_idle_daemon_exits_and_removes_its_socket() {
    let root = scratch_root();
    let link = Link::start(config("host-a", "e1"), DirectLauncher::new(root.path(), 1));
    let agent = link.wait_connected(CALL).unwrap();
    let run = root.path().join("run");
    // Unix: the socket itself. Windows: the file naming its port.
    let endpoint = if cfg!(windows) { "json" } else { "sock" };
    let socket = std::fs::read_dir(&run)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == endpoint))
        .expect("the daemon's socket");
    link.close(true);
    assert!(wait_until(Duration::from_secs(6), || !socket.exists()));
    assert!(wait_until(Duration::from_secs(2), || !process_alive(agent.pid)));
}

/// Retransmission is harmless by construction: the same spawn twice is one
/// process.
#[test]
fn a_retransmitted_spawn_starts_one_process() {
    let root = scratch_root();
    let nonce = "abc123";
    let mut child = Command::new(AGENT)
        .args(["proxy", "--sync", nonce, "--idle-exit", "2", "--tick-ms", "100"])
        .env("MEWORK_REMOTE_ROOT", root.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut reader = child.stdout.take().unwrap();
    let mut writer = child.stdin.take().unwrap();
    assert_eq!(protocol::read_preamble(&mut reader, nonce).unwrap(), Preamble::Ready);
    protocol::write_frame(
        &mut writer,
        &Message::Hello(protocol::Hello {
            protocol: protocol::PROTOCOL_VERSION,
            client: "raw".into(),
            epoch: "1".into(),
            policy: Policy::default(),
            resume: Vec::new(),
        }),
        &[],
    )
    .unwrap();
    let request = Message::Request {
        id: 1,
        op: Op::Spawn(spec_of("dup", long_running(30))),
    };
    protocol::write_frame(&mut writer, &request, b"").unwrap();
    protocol::write_frame(&mut writer, &request, b"").unwrap();
    let mut pids = Vec::new();
    while pids.len() < 2 {
        let frame = protocol::read_frame(&mut reader).unwrap().unwrap();
        if let Message::Response { id: 1, outcome } = frame.message {
            let protocol::Outcome::Ok {
                reply: Reply::Spawned { pid },
            } = outcome
            else {
                panic!("{outcome:?}")
            };
            pids.push(pid);
        }
    }
    assert_eq!(pids[0], pids[1]);
    protocol::write_frame(&mut writer, &Message::Bye { release: true }, &[]).unwrap();
    assert!(wait_until(Duration::from_secs(3), || !process_alive(pids[0])));
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn a_link_that_cannot_launch_gives_up_and_says_why() {
    struct Nowhere;
    impl Launcher for Nowhere {
        fn launch(&self, _: &str) -> Result<Transport, LaunchError> {
            Err(LaunchError::Unreachable("ssh: connect to host nowhere: Connection refused".into()))
        }
    }
    let mut quick = config("host-a", "e1");
    quick.give_up_after = Duration::from_millis(600);
    let link = Link::start(quick, Nowhere);
    let error = link.wait_connected(Duration::from_secs(10)).unwrap_err().to_string();
    assert!(error.contains("Connection refused"), "{error}");
    assert!(matches!(link.status(), LinkStatus::Lost { .. }));
    // A new request tries again, and fails again with the same words.
    let error = link.call(Op::Sessions, b"", Duration::from_secs(10)).unwrap_err().to_string();
    assert!(error.contains("Connection refused"), "{error}");
    link.close(false);
}

/// The Windows half: the same promises, kept with `cmd.exe`, PowerShell, a
/// pseudo console and job objects.
#[cfg(windows)]
mod windows {
    use super::*;

    /// PowerShell running `script`, passed encoded so no quoting of the
    /// command line can reach it.
    fn powershell(script: &str) -> Vec<String> {
        let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
        [
            "powershell.exe",
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
            &base64(&utf16),
        ]
        .map(str::to_owned)
        .to_vec()
    }

    fn base64(bytes: &[u8]) -> String {
        const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let word = (u32::from(chunk[0]) << 16)
                | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
                | u32::from(*chunk.get(2).unwrap_or(&0));
            for (index, shift) in [18, 12, 6, 0].into_iter().enumerate() {
                if index <= chunk.len() {
                    out.push(ALPHABET[(word >> shift) as usize & 63] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    /// Copies standard input to standard output byte for byte, as it arrives.
    const COPY_STDIN: &str =
        "[Console]::OpenStandardInput().CopyTo([Console]::OpenStandardOutput())";

    #[test]
    fn a_command_runs_with_separate_streams_and_its_exit_code() {
        let root = scratch_root();
        let link = Link::start(config("host-a", "e1"), DirectLauncher::new(root.path(), 3));
        let mut process = link
            .spawn(spec("s1", &["cmd.exe", "/d", "/c", "echo out& echo err>&2& exit /b 3"]), b"", CALL)
            .unwrap();
        let mut stdout = process.take_stdout().unwrap();
        let mut stderr = process.take_stderr().unwrap();
        assert_eq!(read_all(&mut stdout), "out\r\n");
        assert_eq!(read_all(&mut stderr), "err\r\n");
        let exit = process.wait().unwrap();
        assert_eq!(exit.code, Some(3));
        assert_eq!(exit.reason, ExitReason::Exited);
        link.close(true);
    }

    #[test]
    fn a_spawn_carries_its_stdin_body_cwd_and_environment() {
        let root = scratch_root();
        let link = Link::start(config("host-a", "e1"), DirectLauncher::new(root.path(), 3));
        let mut with_input = spec_of("s1", powershell(COPY_STDIN));
        with_input.stdin = StdinMode::Body;
        let payload: Vec<u8> = (0..=255u8).cycle().take(70_000).collect();
        let mut process = link.spawn(with_input, &payload, CALL).unwrap();
        let mut out = Vec::new();
        process.take_stdout().unwrap().read_to_end(&mut out).unwrap();
        assert!(out == payload, "binary stdin came back changed ({} bytes)", out.len());

        // The working directory in the spelling a Git Bash login gave it, and
        // a `PATH` that must replace the login's `Path`, not sit beside it.
        let windows = std::env::var("SystemRoot").unwrap();
        let (drive, rest) = windows.split_once(":\\").unwrap();
        let mut placed = spec("s2", &["cmd.exe", "/d", "/c", "cd& echo %MEWORK_TEST_VALUE%& echo %Path%"]);
        placed.cwd = Some(format!("/{}/{}", drive.to_ascii_lowercase(), rest.replace('\\', "/")));
        placed.env.insert("MEWORK_TEST_VALUE".into(), "it's here".into());
        placed.env.insert(
            "PATH".into(),
            format!("C:\\mework-marker;{}", std::env::var("PATH").unwrap()),
        );
        let mut process = link.spawn(placed, b"", CALL).unwrap();
        let text = read_all(&mut process.take_stdout().unwrap());
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].eq_ignore_ascii_case(&windows), "{text}");
        assert_eq!(lines[1], "it's here");
        assert!(lines[2].starts_with("C:\\mework-marker;"), "{text}");

        let missing = link.spawn(spec("s3", &["mework-no-such-program"]), b"", CALL);
        let error = missing.unwrap_err().to_string();
        assert!(error.contains("was not found"), "{error}");
        let mut bad_cwd = spec("s4", &["cmd.exe", "/d", "/c", "exit 0"]);
        bad_cwd.cwd = Some("C:/definitely/not/here".into());
        assert!(link.spawn(bad_cwd, b"", CALL).unwrap_err().to_string().contains("does not exist"));
        link.close(true);
    }

    #[test]
    fn output_resumes_exactly_after_the_transport_is_killed() {
        let root = scratch_root();
        let launcher = DirectLauncher::new(root.path(), 3);
        let (current, launches) = launcher.handle();
        let link = Link::start(config("host-a", "e1"), launcher);
        let mut process = link
            .spawn(
                spec_of(
                    "s1",
                    powershell("1..40 | ForEach-Object { [Console]::Out.WriteLine(\"line$_\"); Start-Sleep -Milliseconds 50 }"),
                ),
                b"",
                CALL,
            )
            .unwrap();
        let pid = process.pid();
        let mut stdout = BufReader::new(process.take_stdout().unwrap());
        let mut lines = Vec::new();
        let mut line = String::new();
        for _ in 0..5 {
            line.clear();
            stdout.read_line(&mut line).unwrap();
            lines.push(line.trim().to_owned());
        }
        kill_transport(&current);
        std::thread::sleep(Duration::from_millis(300));
        kill_transport(&current);
        loop {
            line.clear();
            if stdout.read_line(&mut line).unwrap() == 0 {
                break;
            }
            lines.push(line.trim().to_owned());
        }
        let expected: Vec<String> = (1..=40).map(|i| format!("line{i}")).collect();
        assert_eq!(lines, expected);
        assert_eq!(process.wait().unwrap().code, Some(0));
        assert!(*launches.lock().unwrap() >= 2, "the link reconnected");
        assert!(wait_until(Duration::from_secs(2), || !process_alive(pid)));
        link.close(true);
    }

    #[test]
    fn input_typed_while_the_link_is_down_arrives_in_order_once() {
        let root = scratch_root();
        let launcher = DirectLauncher::new(root.path(), 3);
        let (current, _) = launcher.handle();
        let link = Link::start(config("host-a", "e1"), launcher);
        let mut cat = spec_of("s1", powershell(COPY_STDIN));
        cat.stdin = StdinMode::Pipe;
        let mut process = link.spawn(cat, b"", CALL).unwrap();
        let mut stdin = process.stdin();
        let mut stdout = BufReader::new(process.take_stdout().unwrap());
        stdin.write_all(b"first\n").unwrap();
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        assert_eq!(line, "first\n");

        kill_transport(&current);
        stdin.write_all(b"second\n").unwrap();
        stdin.write_all(b"third\n").unwrap();
        line.clear();
        stdout.read_line(&mut line).unwrap();
        assert_eq!(line, "second\n");
        line.clear();
        stdout.read_line(&mut line).unwrap();
        assert_eq!(line, "third\n");
        process.close_stdin();
        line.clear();
        assert_eq!(stdout.read_line(&mut line).unwrap(), 0);
        assert_eq!(process.wait().unwrap().code, Some(0));
        link.close(true);
    }

    /// A pseudo console: what is typed runs, a resize is what the shell then
    /// sees, and the shell's exit code is the session's. The pseudo console
    /// starts by asking where the cursor is and waits for the answer, which a
    /// terminal — xterm.js in the app, this test here — gives.
    #[test]
    fn a_terminal_session_echoes_resizes_and_exits() {
        let root = scratch_root();
        let link = Link::start(config("host-a", "e1"), DirectLauncher::new(root.path(), 3));
        let mut terminal = spec("t1", &["powershell.exe", "-NoLogo", "-NoProfile"]);
        terminal.terminal = Some(TerminalSize { cols: 80, rows: 24 });
        terminal.stdin = StdinMode::Pipe;
        let mut process = link.spawn(terminal, b"", CALL).unwrap();
        let output = Arc::new(Mutex::new(Vec::<u8>::new()));
        {
            let output = Arc::clone(&output);
            let mut reader = process.take_stdout().unwrap();
            std::thread::spawn(move || {
                let mut chunk = [0u8; 4096];
                while let Ok(count) = reader.read(&mut chunk) {
                    if count == 0 {
                        break;
                    }
                    output.lock().unwrap().extend_from_slice(&chunk[..count]);
                }
            });
        }
        let mut stdin = process.stdin();
        assert!(
            wait_until(Duration::from_secs(20), || {
                String::from_utf8_lossy(&output.lock().unwrap()).contains("\x1b[6n")
            }),
            "the pseudo console never asked for the cursor"
        );
        stdin.write_all(b"\x1b[1;1R").unwrap();
        process.resize(TerminalSize { cols: 132, rows: 40 });
        stdin
            .write_all(b"'size=' + $Host.UI.RawUI.WindowSize.Width + 'x' + $Host.UI.RawUI.WindowSize.Height; 'mark-' + (6*7)\r")
            .unwrap();
        assert!(
            wait_until(Duration::from_secs(20), || {
                let text = String::from_utf8_lossy(&output.lock().unwrap()).into_owned();
                text.contains("size=132x40") && text.contains("mark-42")
            }),
            "{}",
            String::from_utf8_lossy(&output.lock().unwrap())
        );
        stdin.write_all(b"exit 5\r").unwrap();
        let exit = process.wait().unwrap();
        assert_eq!(exit.code, Some(5));
        link.close(true);
    }

    /// Releasing ends everything the session started, not only its root:
    /// the job holds the grandchild too.
    #[test]
    fn releasing_a_session_ends_its_whole_process_tree() {
        let root = scratch_root();
        let link = Link::start(config("host-a", "e1"), DirectLauncher::new(root.path(), 3));
        let mut process = link
            .spawn(
                spec_of(
                    "s1",
                    powershell(
                        "$p = Start-Process -FilePath ping.exe -ArgumentList '-n','61','127.0.0.1' -WindowStyle Hidden -PassThru; [Console]::Out.WriteLine($p.Id); $p.WaitForExit()",
                    ),
                ),
                b"",
                CALL,
            )
            .unwrap();
        let root_pid = process.pid();
        let mut stdout = BufReader::new(process.take_stdout().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let grandchild: u32 = line.trim().parse().unwrap_or_else(|_| panic!("{line:?}"));
        assert!(process_alive(grandchild));
        process.release();
        assert!(wait_until(Duration::from_secs(5), || !process_alive(grandchild)));
        assert!(wait_until(Duration::from_secs(5), || !process_alive(root_pid)));
        link.close(true);
    }

    #[test]
    fn output_dropped_while_disconnected_is_counted_not_spliced() {
        let root = scratch_root();
        let launcher = DirectLauncher::new(root.path(), 3);
        let (current, _) = launcher.handle();
        let blocked = launcher.blocker();
        let link = Link::start(config("host-a", "e1"), launcher);
        // PowerShell takes a moment to start, which is the head start the
        // link needs to go down before the first byte.
        let mut noisy = spec_of(
            "s1",
            powershell(
                "Start-Sleep -Milliseconds 700; $out = [Console]::OpenStandardOutput(); $line = [Text.Encoding]::ASCII.GetBytes(\"0123456789abcdefghijklmnopqrstuvwxyz`n\"); for ($i = 0; $i -lt 2000; $i++) { $out.Write($line, 0, $line.Length) }",
            ),
        );
        noisy.output_limit = Some(4096);
        let mut process = link.spawn(noisy, b"", CALL).unwrap();
        let mut stdout = process.take_stdout().unwrap();
        let mut stderr = process.take_stderr().unwrap();
        let stderr = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stderr.read_to_end(&mut bytes);
            String::from_utf8_lossy(&bytes).into_owned()
        });
        blocked.store(true, std::sync::atomic::Ordering::SeqCst);
        kill_transport(&current);
        std::thread::sleep(Duration::from_millis(4000));
        assert!(matches!(link.status(), LinkStatus::Reconnecting { .. }), "{:?}", link.status());
        blocked.store(false, std::sync::atomic::Ordering::SeqCst);
        let mut received = Vec::new();
        stdout.read_to_end(&mut received).unwrap();
        let exit = process.wait().unwrap();
        assert_eq!(exit.code, Some(0));
        let total = 2000 * 37u64;
        let stderr = stderr.join().unwrap();
        assert_eq!(exit.ends.stdout, total, "{stderr}");
        assert!(process.lost_bytes() > 0, "the ring must have overflowed");
        assert!(received.len() <= 4096, "only what the ring still held: {}", received.len());
        assert_eq!(received.len() as u64 + process.lost_bytes(), total);
        assert!(received.ends_with(b"xyz\n"));
        link.close(true);
    }

    /// Programs resolve on the login's `PATH` with `PATHEXT`; `bash` and `sh`
    /// — bare or by their Unix paths — are Git for Windows' own when it is
    /// installed, never the WSL launcher in System32.
    #[test]
    fn which_resolves_programs_on_the_machine() {
        let root = scratch_root();
        let link = Link::start(config("host-a", "e1"), DirectLauncher::new(root.path(), 3));
        let names = ["cmd", "mework-no-such-program", "bash", "sh", "/bin/sh", "/bin/bash"];
        let Reply::Which { found } = link
            .call(
                Op::Which {
                    names: names.iter().map(|name| (*name).to_owned()).collect(),
                },
                b"",
                CALL,
            )
            .unwrap()
        else {
            panic!("expected which")
        };
        let cmd = found["cmd"].clone().unwrap().to_ascii_lowercase();
        assert!(cmd.ends_with("\\cmd.exe"), "{cmd}");
        assert_eq!(found["mework-no-such-program"], None);
        match found["bash"].as_deref() {
            Some(bash) => {
                let lower = bash.to_ascii_lowercase();
                assert!(lower.ends_with("bash.exe") && !lower.contains("system32") && !lower.contains("windowsapps"), "{bash}");
                assert_eq!(found["/bin/bash"], found["bash"]);
                let sh = found["sh"].clone().expect("Git for Windows has sh beside bash");
                assert!(sh.to_ascii_lowercase().ends_with("sh.exe"), "{sh}");
                assert_eq!(found["/bin/sh"].as_deref(), Some(sh.as_str()));
            }
            None => eprintln!("Git for Windows is not installed; its shells are not checked"),
        }
        link.close(true);
    }

    /// What Windows' sshd does to a session: its processes live in a job
    /// that is killed whole when the connection ends. The daemon a proxy
    /// starts from inside such a job must not be in it.
    fn the_daemon_outlives_a_killed_session_job(allow_breakaway: bool) {
        use std::os::windows::io::AsRawHandle;
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        use windows_sys::Win32::System::Threading::CREATE_SUSPENDED;

        let root = scratch_root();
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        assert!(!job.is_null());
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | if allow_breakaway { JOB_OBJECT_LIMIT_BREAKAWAY_OK } else { 0 };
        unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                std::ptr::addr_of!(limits).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
        }
        let nonce = "job1";
        let mut proxy = Command::new(AGENT)
            .args(["proxy", "--sync", nonce, "--idle-exit", "3", "--tick-ms", "100"])
            .env("MEWORK_REMOTE_ROOT", root.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .creation_flags(CREATE_SUSPENDED)
            .spawn()
            .unwrap();
        assert_ne!(unsafe { AssignProcessToJobObject(job, proxy.as_raw_handle() as _) }, 0);
        remote_agent::agent::platform::resume_suspended(proxy.id()).unwrap();
        let mut reader = proxy.stdout.take().unwrap();
        let mut writer = proxy.stdin.take().unwrap();
        assert_eq!(protocol::read_preamble(&mut reader, nonce).unwrap(), Preamble::Ready);
        protocol::write_frame(
            &mut writer,
            &Message::Hello(protocol::Hello {
                protocol: protocol::PROTOCOL_VERSION,
                client: "job".into(),
                epoch: "1".into(),
                policy: Policy::default(),
                resume: Vec::new(),
            }),
            &[],
        )
        .unwrap();
        let daemon = loop {
            let frame = protocol::read_frame(&mut reader).unwrap().unwrap();
            if let Message::Welcome(welcome) = frame.message {
                break welcome.agent.pid;
            }
        };
        assert!(process_alive(daemon));
        // The connection ends: sshd closes the session's job. The channel
        // reaches its end at once — the daemon holds no copy of it.
        let ended = std::thread::spawn(move || {
            let mut rest = Vec::new();
            let _ = reader.read_to_end(&mut rest);
        });
        unsafe { CloseHandle(job) };
        assert!(wait_until(Duration::from_secs(5), || !process_alive(proxy.id())));
        let _ = proxy.wait();
        assert!(
            wait_until(Duration::from_secs(2), || ended.is_finished()),
            "the proxy's output stayed open after it died"
        );
        std::thread::sleep(Duration::from_millis(500));
        assert!(process_alive(daemon), "the daemon went down with the session's job");

        // The next connection finds the same daemon.
        let link = Link::start(config("job", "1"), DirectLauncher::new(root.path(), 3));
        assert_eq!(link.wait_connected(CALL).unwrap().pid, daemon);
        link.close(true);
        assert!(wait_until(Duration::from_secs(10), || !process_alive(daemon)));
    }

    #[test]
    fn the_daemon_breaks_away_from_the_session_job() {
        the_daemon_outlives_a_killed_session_job(true);
    }

    /// A job that forbids breaking away leaves WMI, whose service no session
    /// job contains, to start the daemon.
    #[test]
    fn the_daemon_is_started_through_wmi_when_the_job_forbids_breaking_away() {
        the_daemon_outlives_a_killed_session_job(false);
    }
}
