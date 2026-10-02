//! Reaching an SSH machine through the agent Mework keeps running there.
//!
//! Every SSH machine a conversation uses is served by one long-lived link
//! ([`remote_agent::client::Link`]): a single `ssh` process whose channel
//! carries every command, terminal, file operation and heartbeat for that
//! machine, to a daemon that outlives the connection. Before this, each of
//! those was its own `ssh` process, run by whatever login shell the account
//! had, and ended by any hiccup in the network.
//!
//! This module is the host's half of that arrangement:
//!
//! * [`SshLauncher`] starts the transport. The login shell is only asked to
//!   start the agent's proxy — one fixed line every Unix shell, Git Bash
//!   included, reads the same way ([`crate::remote_shell::posix_line`]), or on
//!   a Windows machine whose login shell is `cmd.exe` or PowerShell, a
//!   PowerShell script sent encoded ([`crate::remote_shell::powershell_line`])
//!   — and when the agent is not there yet, the launcher uploads the build for
//!   the machine's platform and checks that it runs before using it.
//! * [`link_for`] hands out the link for a runner, starting it on first use and
//!   remembering machines the agent cannot serve (a platform with no build, a
//!   home it cannot write to), which keep using the per-command transport in
//!   [`crate::run_environment`].
//! * [`run_script`], [`spawn`] and friends are the shapes the rest of the host
//!   already speaks — a script's output, a child process — so callers change
//!   where a process runs, not how they talk to it.
//!
//! The daemon reclaims what a vanished host leaves behind; the host, for its
//! part, ends its links with a release on a normal exit ([`shutdown`]), so a
//! quit application leaves nothing running on any machine.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use remote_agent::client::{CallError, LaunchError, Launcher, Link, LinkConfig, LinkStatus, RemoteProcess, Transport};
use remote_agent::protocol::{self, AgentInfo, Preamble, SpawnSpec, StdinMode, TerminalSize};

use crate::cancel::CancelSignal;
use crate::remote_shell::{self, LoginShell};
use crate::run_environment::{self, RemoteCommandOutput, ShellRunner};

/// Set to `off` to keep every SSH machine on the per-command transport.
pub const DISABLE_ENV: &str = "MEWORK_REMOTE_AGENT";
/// A directory of agent builds, `<dir>/<target-triple>/mework-remote`, searched
/// before the bundled ones. For developing the agent itself.
pub const AGENT_DIR_ENV: &str = "MEWORK_REMOTE_AGENT_DIR";

/// How long a first connection may take, upload included, before the caller
/// hears about it. A machine that is merely slow keeps connecting behind it.
const FIRST_CONNECT_WAIT: Duration = Duration::from_secs(90);
/// How long a machine the agent cannot serve stays on the per-command path
/// before the agent is tried again.
const UNAVAILABLE_RETRY: Duration = Duration::from_secs(10 * 60);
/// How long the login's first line may take.
const PREAMBLE_TIMEOUT: Duration = Duration::from_secs(40);
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(180);
/// A link that has carried nothing for this long is closed; the next
/// operation on the machine reconnects, which takes a fraction of a second
/// once the agent is installed. The daemon, left without connections or
/// sessions, then leaves the machine by itself.
const IDLE_LINK_CLOSE: Duration = Duration::from_secs(15 * 60);
/// Spawn and control requests; the process itself may run as long as it likes.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a terminal outlives its host's link before the agent reclaims it.
/// Longer than a command's: a laptop closed over lunch should come back to its
/// shells, while one that never comes back should not leave them forever.
const TERMINAL_ORPHAN_TTL: Duration = Duration::from_secs(2 * 60 * 60);
/// What a script's output may be kept in on the machine while the link is
/// down. A script's answer is used whole or not at all, so this is sized to
/// make a gap improbable rather than to bound memory tightly.
const SCRIPT_OUTPUT_LIMIT: u64 = 48 << 20;

// ---------------------------------------------------------------------------
// The hub
// ---------------------------------------------------------------------------

/// Told about every link's state changes, by the machine's host name.
pub type StatusObserver = Box<dyn Fn(&str, &LinkStatus) + Send + Sync>;

/// Called, on a thread of its own, the first time this process reaches an SSH
/// endpoint — its link connected, or the agent turned out not to serve it and
/// the per-command transport took over — with the runner that reached it.
/// This is when a machine's shell backends are probed each session.
pub type FirstReachHook = Box<dyn Fn(ShellRunner) + Send + Sync>;

static FIRST_REACH: OnceLock<FirstReachHook> = OnceLock::new();

/// Installs the [`FirstReachHook`]. Once per process; later calls are ignored.
pub fn on_first_reach(hook: FirstReachHook) {
    let _ = FIRST_REACH.set(hook);
}

/// Hands `runner` to the hook unless its endpoint was already announced this
/// process. On a fresh thread before anything is locked: link observers run
/// inside the link's own machinery, some of them while the hub is locked.
fn announce_reached(key: String, runner: ShellRunner) {
    if FIRST_REACH.get().is_none() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("remote-first-reach".into())
        .spawn(move || {
            let Some(hub) = HUB.get() else {
                return;
            };
            if !lock(&hub.state).announced.insert(key) {
                return;
            }
            if let Some(hook) = FIRST_REACH.get() {
                hook(runner);
            }
        });
}

struct Hub {
    observer: Option<StatusObserver>,
    client_id: String,
    epoch: String,
    /// Searched as `<dir>/<target-triple>/mework-remote`.
    agent_dirs: Vec<PathBuf>,
    /// Builds for this host's own triple laid out without a triple directory,
    /// as `cargo build` leaves them.
    native_builds: Vec<PathBuf>,
    /// Found on first use, and again after a build is made here.
    catalog: Mutex<Option<Arc<Catalog>>>,
    /// One per platform a build is being made for, so machines of one platform connecting
    /// together wait for a single build instead of each starting one.
    building: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    state: Mutex<HubState>,
}

impl Hub {
    fn catalog(&self) -> Arc<Catalog> {
        lock(&self.catalog)
            .get_or_insert_with(|| Arc::new(Catalog::discover(&self.agent_dirs, &self.native_builds)))
            .clone()
    }

    fn rediscover(&self) -> Arc<Catalog> {
        let fresh = Arc::new(Catalog::discover(&self.agent_dirs, &self.native_builds));
        *lock(&self.catalog) = Some(fresh.clone());
        fresh
    }

    fn building(&self, platform: &str) -> Arc<Mutex<()>> {
        lock(&self.building).entry(platform.to_owned()).or_default().clone()
    }
}

#[derive(Default)]
struct HubState {
    links: HashMap<String, Link>,
    /// When each link was last handed to a caller.
    last_used: HashMap<String, Instant>,
    /// Endpoints the agent cannot serve, with why and since when.
    unavailable: HashMap<String, (String, Instant)>,
    /// Endpoints handed to the [`FirstReachHook`] this process.
    announced: std::collections::HashSet<String>,
}

static HUB: OnceLock<Hub> = OnceLock::new();

/// Makes the agent available to the rest of the host. Called once at startup
/// with the application's data directory (where the installation's client id
/// lives) and the directories bundled agent builds may be found in. Until it
/// is called — in tests, for one — every SSH machine uses the per-command
/// transport.
pub fn install(app_data: &Path, bundled_dirs: Vec<PathBuf>, observer: Option<StatusObserver>) {
    let client_id = load_client_id(app_data);
    let mut agent_dirs = Vec::new();
    if let Some(dir) = std::env::var_os(AGENT_DIR_ENV).filter(|value| !value.is_empty()) {
        agent_dirs.push(PathBuf::from(dir));
    }
    agent_dirs.extend(bundled_dirs);
    let mut native_builds = Vec::new();
    if cfg!(debug_assertions) {
        let (dirs, native) = source_tree_agents();
        agent_dirs.extend(dirs);
        native_builds = native;
    }
    let installed = HUB.set(Hub {
        observer,
        client_id,
        epoch: uuid::Uuid::new_v4().simple().to_string(),
        agent_dirs,
        native_builds,
        catalog: Mutex::new(None),
        building: Mutex::new(HashMap::new()),
        state: Mutex::new(HubState::default()),
    });
    if installed.is_ok() {
        std::thread::Builder::new()
            .name("remote-link-janitor".into())
            .spawn(close_idle_links)
            .expect("the remote link janitor starts");
    }
}

/// Closes links nobody has used for a while and that carry nothing: an open
/// SSH connection and a resident daemon are only worth keeping while a
/// conversation is actually working on the machine.
fn close_idle_links() {
    loop {
        std::thread::sleep(Duration::from_secs(60));
        let Some(hub) = HUB.get() else {
            return;
        };
        let idle: Vec<Link> = {
            let mut state = lock(&hub.state);
            let keys: Vec<String> = state
                .links
                .iter()
                .filter(|(key, link)| {
                    link.is_idle()
                        && state
                            .last_used
                            .get(*key)
                            .map_or(true, |used| used.elapsed() >= IDLE_LINK_CLOSE)
                })
                .map(|(key, _)| key.clone())
                .collect();
            keys.into_iter()
                .filter_map(|key| {
                    state.last_used.remove(&key);
                    state.links.remove(&key)
                })
                .collect()
        };
        for link in idle {
            std::thread::spawn(move || link.close(true));
        }
    }
}

/// Ends every link, releasing what each started: the host is quitting on
/// purpose, so nothing it ran should wait out an orphan time.
pub fn shutdown() {
    let Some(hub) = HUB.get() else {
        return;
    };
    let links: Vec<Link> = lock(&hub.state).links.drain().map(|(_, link)| link).collect();
    let closers: Vec<_> = links
        .into_iter()
        .map(|link| std::thread::spawn(move || link.close(true)))
        .collect();
    for closer in closers {
        let _ = closer.join();
    }
}

/// The installation's stable client id, created on first use.
fn load_client_id(app_data: &Path) -> String {
    let path = app_data.join("remote-agent-client-id");
    if let Ok(text) = std::fs::read_to_string(&path) {
        let text = text.trim();
        if !text.is_empty() && text.len() <= 64 && text.chars().all(|c| c.is_ascii_alphanumeric()) {
            return text.to_owned();
        }
    }
    let id = uuid::Uuid::new_v4().simple().to_string();
    let _ = std::fs::create_dir_all(app_data);
    let _ = std::fs::write(&path, &id);
    id
}

fn endpoint_key(host: &str, port: u16, identity_file: &str) -> String {
    format!("{host}\u{0}{port}\u{0}{identity_file}")
}

/// Whether the agent is switched off for this process.
fn disabled() -> bool {
    std::env::var(DISABLE_ENV).is_ok_and(|value| value.eq_ignore_ascii_case("off") || value == "0")
}

/// Where a remote operation should go.
pub enum Route {
    /// Through the agent, over this connected link, to the agent described:
    /// its operating system decides which programs a caller asks it for.
    Agent(Link, AgentInfo),
    /// Through the per-command transport in [`crate::run_environment`]: the
    /// agent does not serve this machine, or is still being installed there.
    Legacy,
    /// Nowhere: the machine could not be reached at all, which the
    /// per-command transport would only discover again.
    Unreachable(String),
}

/// Decides where an operation on the machine `runner` reaches goes, waiting
/// up to `patience` for the machine's link to be ready.
///
/// A link that is still connecting when the patience runs out — the first
/// connection installs the agent, which takes a moment on a slow network —
/// sends this one operation the per-command way and keeps connecting behind
/// it, so nobody waits on an installation they did not ask for.
pub fn route(runner: &ShellRunner, patience: Duration) -> Route {
    let ShellRunner::Ssh {
        host,
        port,
        identity_file,
        ..
    } = runner
    else {
        return Route::Legacy;
    };
    let Some(hub) = HUB.get() else {
        return Route::Legacy;
    };
    if disabled() {
        return Route::Legacy;
    }
    let key = endpoint_key(host, *port, identity_file);
    let link = {
        let mut state = lock(&hub.state);
        if let Some((_, since)) = state.unavailable.get(&key) {
            if since.elapsed() < UNAVAILABLE_RETRY {
                return Route::Legacy;
            }
            state.unavailable.remove(&key);
        }
        state.last_used.insert(key.clone(), Instant::now());
        match state.links.get(&key) {
            Some(link) => link.clone(),
            None => {
                let launcher = SshLauncher {
                    runner: runner.clone(),
                    label: host.clone(),
                };
                let config = LinkConfig::new(hub.client_id.clone(), hub.epoch.clone());
                let link = Link::start(config, launcher);
                let observed_host = host.clone();
                let reached = (key.clone(), runner.clone());
                link.set_observer(move |status| {
                    report_status(&observed_host, status);
                    if matches!(status, LinkStatus::Connected { .. }) {
                        announce_reached(reached.0.clone(), reached.1.clone());
                    }
                });
                state.links.insert(key.clone(), link.clone());
                link
            }
        }
    };
    match link.wait_ready(patience) {
        Ok(agent) => Route::Agent(link, agent),
        Err(CallError::Timeout) => Route::Legacy,
        Err(error) => match link.status() {
            LinkStatus::Unavailable { error } => {
                let mut state = lock(&hub.state);
                state.links.remove(&key);
                state.unavailable.insert(key.clone(), (error.clone(), Instant::now()));
                drop(state);
                eprintln!("[remote-agent] {host}: using per-command SSH ({error})");
                announce_reached(key, runner.clone());
                Route::Legacy
            }
            LinkStatus::Closed => Route::Legacy,
            _ => Route::Unreachable(format!("Cannot reach the SSH machine {host}: {error}")),
        },
    }
}

/// [`route`] for callers that only distinguish "use this link" from "use the
/// per-command transport" and report an unreachable machine as an error.
pub fn link_for(runner: &ShellRunner, patience: Duration) -> Result<Option<Link>, String> {
    match route(runner, patience) {
        Route::Agent(link, _) => Ok(Some(link)),
        Route::Legacy => Ok(None),
        Route::Unreachable(error) => Err(error),
    }
}

/// The agent that runs the host's helpers (`mework-remote git`) on the machine
/// `runner` reaches: the SSH machine's daemon, or the agent Mework starts
/// inside a WSL distribution. `Ok(None)` when there is none to ask — the
/// machine is served per command, or this computer's own, which the host
/// reaches directly — and the caller takes its per-command path.
pub fn helper_link(runner: &ShellRunner, patience: Duration) -> Result<Option<Link>, String> {
    match runner {
        ShellRunner::Ssh { .. } => link_for(runner, patience),
        // Not remembered as unavailable, as `local_link` explains; the
        // per-command path serves this call.
        ShellRunner::Wsl { .. } => Ok(local_link(runner, patience).ok().map(|(link, _)| link)),
        ShellRunner::Local { .. } => Ok(None),
    }
}

/// Whether the machine `runner` reaches is there to answer right now, found
/// out in at most `within`.
///
/// For a caller that has already waited on a machine longer than a healthy one
/// takes and must decide whether to keep waiting. A link's status cannot say:
/// one that reads connected may be talking to a machine switched off a moment
/// ago — its heartbeat notices only after [`LinkConfig::dead_after`] — and one
/// still connecting may be dialing a machine that is not on. So a link that is
/// up is asked for the cheapest reply its agent gives, and a machine without
/// one is checked for anything accepting a connection where `ssh` would make
/// it. What that cannot check — a connection through a proxy, a configuration
/// `ssh` cannot print, a WSL distribution still starting — counts as there,
/// and the caller's own bounds decide.
pub fn machine_is_there(runner: &ShellRunner, within: Duration) -> bool {
    let key = match runner {
        ShellRunner::Local { .. } => return true,
        ShellRunner::Wsl { distro, .. } => format!("wsl\u{0}{distro}"),
        ShellRunner::Ssh {
            host,
            port,
            identity_file,
            ..
        } => endpoint_key(host, *port, identity_file),
    };
    let link = HUB.get().and_then(|hub| lock(&hub.state).links.get(&key).cloned());
    if let Some(link) = link.filter(|link| matches!(link.status(), LinkStatus::Connected { .. })) {
        // A failure is an answer too: only silence means the machine is gone.
        let reply = link.call(protocol::Op::Which { names: Vec::new() }, &[], within);
        return !matches!(reply, Err(CallError::Timeout | CallError::Link(_)));
    }
    match runner {
        ShellRunner::Ssh { host, port, .. } => endpoint_accepts(host, *port, within),
        _ => true,
    }
}

/// Whether anything accepts a TCP connection where `ssh` connects for `host`,
/// decided in `within`. Resolving a name has no timeout of its own, so the
/// check runs where it can be left behind; every address is tried at once.
fn endpoint_accepts(host: &str, port: u16, within: Duration) -> bool {
    let (sender, verdict) = std::sync::mpsc::channel();
    let host = host.to_owned();
    std::thread::spawn(move || {
        let Some((name, port)) = ssh_endpoint(&host, port) else {
            let _ = sender.send(true);
            return;
        };
        // A name that does not resolve is a machine that is not there; every
        // address refusing or timing out drops the last sender, which ends the
        // wait at once.
        let addresses = std::net::ToSocketAddrs::to_socket_addrs(&(name.as_str(), port))
            .map(Iterator::collect::<Vec<_>>)
            .unwrap_or_default();
        for address in addresses {
            let sender = sender.clone();
            std::thread::spawn(move || {
                if std::net::TcpStream::connect_timeout(&address, within).is_ok() {
                    let _ = sender.send(true);
                }
            });
        }
    });
    verdict.recv_timeout(within).unwrap_or(false)
}

/// Where `ssh` connects for `host` once the user's configuration has had its
/// say — `HostName`, `Port` — as `ssh -G` prints it. None when `ssh` does not
/// make that connection itself (`ProxyJump`, `ProxyCommand`) or cannot say.
fn ssh_endpoint(host: &str, port: u16) -> Option<(String, u16)> {
    let mut args = vec!["-G".to_owned()];
    if port != 0 {
        args.extend(["-p".to_owned(), port.to_string()]);
    }
    args.extend(["--".to_owned(), host.to_owned()]);
    let output = run_environment::ssh_client_candidates()
        .into_iter()
        .find_map(|program| {
            let mut command = Command::new(program);
            command
                .args(&args)
                .stdin(Stdio::null())
                .stderr(Stdio::null());
            for name in crate::child_environment::private_child_environment_names() {
                command.env_remove(&name);
            }
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt as _;
                command.creation_flags(0x0800_0000);
            }
            command.output().ok()
        })?;
    if !output.status.success() {
        return None;
    }
    parse_ssh_endpoint(&String::from_utf8_lossy(&output.stdout))
}

fn parse_ssh_endpoint(config: &str) -> Option<(String, u16)> {
    let (mut name, mut port) = (None, None);
    for line in config.lines() {
        let Some((key, value)) = line.trim().split_once(' ') else {
            continue;
        };
        let value = value.trim();
        match key.to_ascii_lowercase().as_str() {
            "hostname" => name = Some(value.to_owned()),
            "port" => port = value.parse().ok(),
            "proxyjump" | "proxycommand" if !value.eq_ignore_ascii_case("none") => return None,
            _ => {}
        }
    }
    Some((name?, port?))
}

fn report_status(host: &str, status: &LinkStatus) {
    if let Some(observer) = HUB.get().and_then(|hub| hub.observer.as_ref()) {
        observer(host, status);
    }
    match status {
        LinkStatus::Connected { agent } => {
            eprintln!(
                "[remote-agent] {host}: connected to agent {} (pid {}, {}/{})",
                agent.version, agent.pid, agent.os, agent.arch
            );
            // Only builds of this host's own source are installed, so this is a daemon the host
            // did not start from its catalog — worth a line when something misbehaves.
            if agent.source.as_deref() != Some(remote_agent::SOURCE_ID) {
                eprintln!(
                    "[remote-agent] {host}: warning: the agent was built from other source ({}) than this Mework ({})",
                    agent.source.as_deref().map_or("unknown", |source| &source[..source.len().min(12)]),
                    &remote_agent::SOURCE_ID[..12]
                );
            }
        }
        LinkStatus::Reconnecting { attempt, error } if *attempt > 0 || !error.is_empty() => {
            eprintln!("[remote-agent] {host}: reconnecting (attempt {attempt}): {error}")
        }
        LinkStatus::Lost { error } => eprintln!("[remote-agent] {host}: link lost: {error}"),
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// What callers use
// ---------------------------------------------------------------------------

/// The variables a runner hands every process, less the names that would let
/// a startup file run before the command.
fn process_env(runner: &ShellRunner) -> Result<std::collections::BTreeMap<String, String>, String> {
    let mut env = runner.normalized_env()?;
    env.retain(|name, _| !run_environment::is_shell_startup_env_name(name));
    Ok(env)
}

fn call_error(host: &str, error: CallError) -> String {
    match error {
        CallError::Failed(failure) => failure.message,
        CallError::Timeout => format!("The SSH machine {host} did not answer in time"),
        CallError::Link(reason) => format!("Lost the link to the SSH machine {host}: {reason}"),
    }
}

/// [`call_error`] for a spawn of `program`. The agent's own helpers are asked for by
/// [`protocol::SELF_PROGRAM`]; an agent that answers that it cannot find such a program on `PATH`
/// is a build from before it knew the name, which a host only reaches through a daemon it did not
/// install itself — and saying so is the only useful thing to say.
fn spawn_error(host: &str, program: &str, error: CallError) -> String {
    match error {
        CallError::Failed(failure)
            if failure.kind == protocol::FailureKind::NotFound
                && program == protocol::SELF_PROGRAM =>
        {
            format!(
                "the Mework agent on {host} is an older build that lacks the helpers this needs; \
                 restart Mework to have it replaced with this version's agent"
            )
        }
        error => call_error(host, error),
    }
}

fn host_label(runner: &ShellRunner) -> &str {
    match runner {
        ShellRunner::Ssh { host, .. } => host,
        _ => "the remote machine",
    }
}

/// Runs a host-authored script through the agent and collects its output,
/// the agent-backed twin of [`run_environment::run_remote_script`]. `None`
/// means the agent does not serve this runner.
pub fn run_script(
    runner: &ShellRunner,
    argv: Vec<String>,
    stdin: Option<&[u8]>,
    timeout: Duration,
    cancel: &CancelSignal,
) -> Option<Result<RemoteCommandOutput, String>> {
    // A script's own time limit is also as long as it is worth waiting for
    // the link; past that the per-command transport runs it instead.
    let link = match link_for(runner, timeout.min(FIRST_CONNECT_WAIT)) {
        Ok(Some(link)) => link,
        Ok(None) => return None,
        Err(error) => return Some(Err(error)),
    };
    Some(run_script_on(&link, runner, argv, stdin, timeout, cancel))
}

/// [`run_script`] over a link the caller already holds, for a caller that
/// chose the program by the agent's operating system.
pub fn run_script_on(
    link: &Link,
    runner: &ShellRunner,
    argv: Vec<String>,
    stdin: Option<&[u8]>,
    timeout: Duration,
    cancel: &CancelSignal,
) -> Result<RemoteCommandOutput, String> {
    if cancel.cancelled() {
        return Err("The remote command was cancelled".into());
    }
    let host = host_label(runner);
    let spec = SpawnSpec {
        sid: link.new_sid("script"),
        argv,
        cwd: None,
        env: process_env(runner)?,
        env_remove: Vec::new(),
        terminal: None,
        stdin: if stdin.is_some() {
            StdinMode::Body
        } else {
            StdinMode::Null
        },
        output_limit: Some(SCRIPT_OUTPUT_LIMIT),
        orphan_ttl_secs: Some(timeout.as_secs().max(60)),
        label: Some("script".into()),
        sandbox: None,
    };
    let program = spec.argv.first().cloned().unwrap_or_default();
    let mut process = link
        .spawn(spec, stdin.unwrap_or_default(), REQUEST_TIMEOUT)
        .map_err(|error| spawn_error(host, &program, error))?;
    let stdout = drain(process.take_stdout());
    let stderr = drain(process.take_stderr());
    let deadline = Instant::now() + timeout;
    let exit = loop {
        match process.wait_timeout(Duration::from_millis(100)) {
            Ok(Some(exit)) => break exit,
            Ok(None) => {
                if cancel.cancelled() {
                    process.kill();
                    return Err("The remote command was cancelled".into());
                }
                if Instant::now() >= deadline {
                    process.kill();
                    return Err(format!(
                        "The remote command did not finish within {} seconds",
                        timeout.as_secs()
                    ));
                }
            }
            Err(error) => return Err(format!("Lost the link to the SSH machine {host}: {error}")),
        }
    };
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    // A script's answer is parsed whole — a file's bytes, a listing — so a
    // part of it is worse than none.
    if process.lost_bytes() > 0 {
        return Err(format!(
            "Part of the remote command's output was lost while the link to {host} was down; run it again"
        ));
    }
    Ok(RemoteCommandOutput {
        status: exit.code,
        stdout,
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

fn drain(reader: Option<remote_agent::client::SessionReader>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut reader) = reader {
            let _ = reader.read_to_end(&mut bytes);
        }
        bytes
    })
}

/// A process started through the agent, for callers that hold it the way they
/// would hold a local child: the shell tool, a language server, a terminal.
pub struct AgentChild {
    pub process: RemoteProcess,
}

/// Starts `argv` on the machine `runner` reaches, in `cwd` there. `None`
/// means the agent does not serve this runner.
pub fn spawn(
    runner: &ShellRunner,
    argv: Vec<String>,
    cwd: Option<&str>,
    stdin: StdinMode,
    label: &str,
) -> Option<Result<AgentChild, String>> {
    let link = match link_for(runner, FIRST_CONNECT_WAIT) {
        Ok(Some(link)) => link,
        Ok(None) => return None,
        Err(error) => return Some(Err(error)),
    };
    let host = host_label(runner).to_owned();
    Some((|| {
        let spec = SpawnSpec {
            sid: link.new_sid(label),
            argv,
            cwd: cwd.filter(|cwd| !cwd.trim().is_empty()).map(str::to_owned),
            env: process_env(runner)?,
            env_remove: Vec::new(),
            terminal: None,
            stdin,
            output_limit: None,
            orphan_ttl_secs: None,
            label: Some(label.to_owned()),
            sandbox: None,
        };
        let process = link
            .spawn(spec, b"", REQUEST_TIMEOUT)
            .map_err(|error| call_error(&host, error))?;
        Ok(AgentChild { process })
    })())
}

/// Starts `argv` in `cwd` over `link` as a long-lived service — a dev server — that outlives
/// its link for `orphan_ttl` the way a terminal does, with `extra_env` on top of the runner's
/// variables.
pub fn spawn_service(
    link: &Link,
    runner: &ShellRunner,
    argv: Vec<String>,
    cwd: Option<&str>,
    extra_env: &[(String, String)],
    orphan_ttl: Duration,
    label: &str,
) -> Result<RemoteProcess, String> {
    let host = host_label(runner).to_owned();
    let mut env = process_env(runner)?;
    for (name, value) in extra_env {
        env.insert(name.clone(), value.clone());
    }
    let spec = SpawnSpec {
        sid: link.new_sid(label),
        argv,
        cwd: cwd.filter(|cwd| !cwd.trim().is_empty()).map(str::to_owned),
        env,
        env_remove: Vec::new(),
        terminal: None,
        stdin: StdinMode::Null,
        output_limit: None,
        orphan_ttl_secs: Some(orphan_ttl.as_secs()),
        label: Some(label.to_owned()),
        sandbox: None,
    };
    link.spawn(spec, b"", REQUEST_TIMEOUT)
        .map_err(|error| call_error(&host, error))
}

/// Output a relayed connection may keep on the machine while the link is down: enough for a
/// dev server's biggest bundle, so a short drop does not cut a page load in half.
const RELAY_OUTPUT_LIMIT: u64 = 32 << 20;
/// How long a relayed connection outlives a dropped link. A browser gives up on a request far
/// sooner; the connection only has to last as long as a reconnect plausibly takes.
const RELAY_ORPHAN_TTL: Duration = Duration::from_secs(5 * 60);

/// Starts one of the agent's own relays over `link` — a byte stream on standard input and output
/// — with room for its output to wait out a short drop of the link.
pub fn spawn_relay(link: &Link, runner: &ShellRunner, argv: Vec<String>) -> Result<RemoteProcess, String> {
    let host = host_label(runner).to_owned();
    let spec = SpawnSpec {
        sid: link.new_sid("net"),
        argv,
        cwd: None,
        env: process_env(runner)?,
        env_remove: Vec::new(),
        terminal: None,
        stdin: StdinMode::Pipe,
        output_limit: Some(RELAY_OUTPUT_LIMIT),
        orphan_ttl_secs: Some(RELAY_ORPHAN_TTL.as_secs()),
        label: Some("net".into()),
        sandbox: None,
    };
    let program = spec.argv.first().cloned().unwrap_or_default();
    link.spawn(spec, b"", REQUEST_TIMEOUT)
        .map_err(|error| spawn_error(&host, &program, error))
}

/// Starts an interactive terminal over `link`: `argv` on a pseudo terminal of
/// `size`, in `cwd`, with the runner's variables plus `extra_env`.
pub fn spawn_terminal(
    link: &Link,
    runner: &ShellRunner,
    argv: Vec<String>,
    cwd: &str,
    extra_env: &[(String, String)],
    size: TerminalSize,
) -> Result<AgentChild, String> {
    let host = host_label(runner).to_owned();
    let mut env = process_env(runner)?;
    for (name, value) in extra_env {
        env.insert(name.clone(), value.clone());
    }
    let spec = SpawnSpec {
        sid: link.new_sid("terminal"),
        argv,
        cwd: Some(cwd.to_owned()).filter(|cwd| !cwd.trim().is_empty()),
        env,
        env_remove: Vec::new(),
        terminal: Some(size),
        stdin: StdinMode::Pipe,
        output_limit: None,
        orphan_ttl_secs: Some(TERMINAL_ORPHAN_TTL.as_secs()),
        label: Some("terminal".into()),
        sandbox: None,
    };
    let process = link
        .spawn(spec, b"", REQUEST_TIMEOUT)
        .map_err(|error| call_error(&host, error))?;
    Ok(AgentChild { process })
}

/// How long a terminal waits for its machine's link before it falls back to
/// an interactive `ssh` session.
pub const TERMINAL_CONNECT_WAIT: Duration = FIRST_CONNECT_WAIT;

/// What a sandboxed process is: its command line, where it starts, and the
/// variables it gets on top of the machine's.
pub struct SandboxedCommand {
    pub argv: Vec<String>,
    pub cwd: Option<String>,
    pub env: std::collections::BTreeMap<String, String>,
    pub env_remove: Vec<String>,
    pub label: String,
}

/// Starts `command` in the conversation's sandbox on the machine `runner`
/// reaches: through Mework's agent on this machine or in the WSL
/// distribution, or through the agent on the SSH machine.
///
/// Refuses rather than run anything unsandboxed: a machine whose agent cannot
/// sandbox, or an SSH machine the agent does not serve, says why.
pub fn spawn_in_sandbox(
    runner: &ShellRunner,
    sandbox: &protocol::SandboxSpec,
    command: SandboxedCommand,
) -> Result<AgentChild, String> {
    let (link, agent, place) = match runner {
        ShellRunner::Ssh { host, .. } => match route(runner, FIRST_CONNECT_WAIT) {
            Route::Agent(link, agent) => (link, agent, format!("the SSH machine {host}")),
            Route::Legacy => {
                return Err(format!("{}; the command was not run", no_agent_for_sandbox(host)));
            }
            Route::Unreachable(error) => return Err(error),
        },
        _ => {
            let (link, agent) = local_link(runner, FIRST_CONNECT_WAIT)?;
            let place = match runner {
                ShellRunner::Wsl { distro, .. } => format!("the WSL distribution {distro}"),
                _ => "this computer".into(),
            };
            (link, agent, place)
        }
    };
    if !agent.sandbox.available {
        return Err(format!(
            "The sandbox is not available on {place}: {}; the command was not run",
            if agent.sandbox.detail.is_empty() {
                "the machine has no sandbox Mework can use"
            } else {
                agent.sandbox.detail.as_str()
            }
        ));
    }
    let spec = SpawnSpec {
        sid: link.new_sid(&command.label),
        argv: command.argv,
        cwd: command.cwd.filter(|cwd| !cwd.trim().is_empty()),
        env: command.env,
        env_remove: command.env_remove,
        terminal: None,
        stdin: StdinMode::Null,
        output_limit: None,
        orphan_ttl_secs: None,
        label: Some(command.label.clone()),
        sandbox: Some(sandbox.clone()),
    };
    let process = link
        .spawn(spec, b"", FIRST_CONNECT_WAIT)
        .map_err(|error| match error {
            CallError::Failed(failure) => format!("{}; the command was not run", failure.message),
            error => format!("The sandbox on {place} did not answer: {error}"),
        })?;
    Ok(AgentChild { process })
}

// ---------------------------------------------------------------------------
// This computer and its WSL distributions, through the agent
// ---------------------------------------------------------------------------

/// Why an SSH machine the agent does not serve cannot sandbox.
fn no_agent_for_sandbox(host: &str) -> String {
    let why = HUB
        .get()
        .and_then(|hub| lock(&hub.state).unavailable.values().next().map(|(why, _)| why.clone()))
        .unwrap_or_else(|| "it is still being installed there, or it is switched off".into());
    format!("The sandbox needs Mework's agent on the SSH machine {host}, which is not running there ({why})")
}

/// What the agent on the machine `runner` reaches says about sandboxing
/// there: this computer's, a WSL distribution's, or an SSH machine's — the
/// same agent [`spawn_in_sandbox`] would start the cell through. An SSH
/// machine the agent does not serve cannot sandbox at all, and says why.
pub fn sandbox_support(runner: &ShellRunner) -> Result<protocol::SandboxSupport, String> {
    match runner {
        ShellRunner::Ssh { host, .. } => match route(runner, FIRST_CONNECT_WAIT) {
            Route::Agent(_, agent) => Ok(agent.sandbox),
            Route::Legacy => Ok(protocol::SandboxSupport {
                detail: no_agent_for_sandbox(host),
                ..Default::default()
            }),
            Route::Unreachable(error) => Err(error),
        },
        _ => local_link(runner, FIRST_CONNECT_WAIT).map(|(_, agent)| agent.sandbox),
    }
}

/// The link to the agent Mework runs on this computer — or inside a WSL
/// distribution — for what has to run through one: sandboxed commands. The
/// agent is a child of this process, speaking over its standard input and
/// output, and ends with it.
fn local_link(runner: &ShellRunner, patience: Duration) -> Result<(Link, AgentInfo), String> {
    let hub = HUB.get().ok_or("Mework's agent is not available in this process")?;
    let (key, place) = match runner {
        ShellRunner::Local { .. } => ("local".to_owned(), "this computer".to_owned()),
        ShellRunner::Wsl { distro, .. } => {
            run_environment::validate_wsl_distro_name(distro)?;
            (format!("wsl\u{0}{distro}"), format!("the WSL distribution {distro}"))
        }
        ShellRunner::Ssh { .. } => return Err("an SSH machine has no local agent".into()),
    };
    let link = {
        let mut state = lock(&hub.state);
        state.last_used.insert(key.clone(), Instant::now());
        match state.links.get(&key) {
            Some(link) if !matches!(link.status(), LinkStatus::Unavailable { .. } | LinkStatus::Closed) => link.clone(),
            _ => {
                let config = LinkConfig::new(hub.client_id.clone(), hub.epoch.clone());
                let link = Link::start(config, LocalLauncher { runner: runner.clone() });
                let label = place.clone();
                link.set_observer(move |status| report_status(&label, status));
                state.links.insert(key.clone(), link.clone());
                link
            }
        }
    };
    match link.wait_ready(patience) {
        Ok(agent) => Ok((link, agent)),
        Err(CallError::Timeout) => Err(format!("Mework's agent on {place} did not start in time")),
        Err(error) => {
            // Not remembered: a build made meanwhile, or a WSL distribution
            // started, is worth another try on the next command.
            lock(&hub.state).links.remove(&key);
            Err(format!("Mework's agent could not start on {place}: {error}"))
        }
    }
}

/// Starts `mework-remote serve --stdio`: the build for this computer, or the
/// Linux build inside a WSL distribution through `wsl.exe`.
struct LocalLauncher {
    runner: ShellRunner,
}

impl Launcher for LocalLauncher {
    fn launch(&self, nonce: &str) -> Result<Transport, LaunchError> {
        let (os, arch) = match &self.runner {
            ShellRunner::Wsl { .. } => ("Linux", std::env::consts::ARCH),
            _ => (
                match std::env::consts::OS {
                    "macos" => "Darwin",
                    "windows" => "Windows",
                    _ => "Linux",
                },
                std::env::consts::ARCH,
            ),
        };
        let executable = local_agent(os, arch)?;
        let mut launcher = match &self.runner {
            ShellRunner::Wsl { distro, .. } => {
                let inside = wsl_path(&executable).ok_or_else(|| {
                    LaunchError::Unavailable(format!(
                        "the agent at {} is not on a drive WSL can see",
                        executable.display()
                    ))
                })?;
                remote_agent::client::ChildLauncher::new(
                    "wsl.exe",
                    vec![
                        "-d".into(),
                        distro.clone(),
                        "--exec".into(),
                        inside,
                        "serve".into(),
                        "--stdio".into(),
                    ],
                )
            }
            _ => remote_agent::client::ChildLauncher::new(&executable, vec!["serve".into(), "--stdio".into()]),
        };
        launcher.env_remove = crate::child_environment::private_child_environment_names()
            .into_iter()
            .map(|name| name.to_string_lossy().into_owned())
            .collect();
        launcher.stderr = Some(Arc::new(|line: &str| eprintln!("[local-agent] {line}")));
        launcher.launch(nonce)
    }
}

/// The agent build for `os`/`arch` — this computer's own, or the Linux one for WSL — as a file
/// this computer can run; in a development build, built here if there is none.
fn local_agent(os: &str, arch: &str) -> Result<PathBuf, LaunchError> {
    let hub = HUB
        .get()
        .ok_or_else(|| LaunchError::Unavailable("the agent is not installed in this host".into()))?;
    let catalog = hub.catalog();
    let catalog = match catalog.for_machine(os, arch) {
        Some(_) => catalog,
        None => build_locally(os, arch)?,
    };
    let build = catalog
        .for_machine(os, arch)
        .ok_or_else(|| LaunchError::Unavailable(format!("no agent build for {os}/{arch}")))?;
    local_executable(build)
}

/// Sets this computer up for the sandbox, once. Only Windows needs it: srt-win's hidden account
/// and network fence, which the agent's `sandbox-setup` provisions after asking for
/// administrator rights (one UAC prompt). Returns what the agent reports afterwards.
pub fn setup_local_sandbox() -> Result<protocol::SandboxSupport, String> {
    if !cfg!(windows) {
        return Err("Only Windows needs the sandbox set up".into());
    }
    let executable = local_agent("Windows", std::env::consts::ARCH).map_err(|error| match error {
        LaunchError::Unavailable(why) | LaunchError::Unreachable(why) => why,
    })?;
    let mut command = Command::new(&executable);
    command.arg("sandbox-setup").stdin(Stdio::null());
    for name in crate::child_environment::private_child_environment_names() {
        command.env_remove(&name);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x0800_0000);
    }
    let output = command
        .output()
        .map_err(|error| format!("Cannot run Mework's agent ({}): {error}", executable.display()))?;
    if !output.status.success() {
        let said = String::from_utf8_lossy(&output.stderr);
        let said = said.trim();
        return Err(said.strip_prefix("mework-remote: ").unwrap_or(said).to_owned());
    }
    // The running agent said what it could do when it started; a new one looks again.
    let stale = HUB.get().and_then(|hub| lock(&hub.state).links.remove("local"));
    if let Some(link) = stale {
        link.close(true);
    }
    sandbox_support(&ShellRunner::default())
}

/// The build as a file this computer can execute. A bundled build may have
/// lost its executable bit on the way into the application's resources; such
/// a build is copied, once per build, to a directory of the user's own.
fn local_executable(build: &Build) -> Result<PathBuf, LaunchError> {
    // Windows has no executable bit, and a Linux build on a Windows drive is
    // executable to WSL as it is.
    if cfg!(windows) || is_executable(&build.path) {
        return Ok(build.path.clone());
    }
    let base = dirs::cache_dir()
        .or_else(dirs::data_local_dir)
        .ok_or_else(|| LaunchError::Unavailable("this account has no cache directory".into()))?;
    let directory = base.join("com.mework.app").join("agents").join(&build.tag);
    let target = directory.join(agent_binary(&build.triple));
    if !target.is_file() {
        let bytes = std::fs::read(&build.path)
            .map_err(|error| LaunchError::Unavailable(format!("cannot read {}: {error}", build.path.display())))?;
        std::fs::create_dir_all(&directory)
            .map_err(|error| LaunchError::Unavailable(format!("cannot create {}: {error}", directory.display())))?;
        let partial = directory.join(format!(".partial-{}", std::process::id()));
        std::fs::write(&partial, &bytes)
            .map_err(|error| LaunchError::Unavailable(format!("cannot write {}: {error}", partial.display())))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o755));
        }
        std::fs::rename(&partial, &target)
            .map_err(|error| LaunchError::Unavailable(format!("cannot place {}: {error}", target.display())))?;
    }
    Ok(target)
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// `C:\Users\…` as WSL mounts it by default: `/mnt/c/Users/…`.
fn wsl_path(path: &Path) -> Option<String> {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut chars = text.chars();
    let drive = chars.next()?;
    if !drive.is_ascii_alphabetic() || chars.next()? != ':' {
        return None;
    }
    Some(format!("/mnt/{}{}", drive.to_ascii_lowercase(), chars.as_str()))
}

/// Turns an agent exit into the `ExitStatus` local callers already handle.
pub fn exit_status(exit: &protocol::ExitInfo) -> std::process::ExitStatus {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        match (exit.code, exit.signal) {
            (Some(code), _) => std::process::ExitStatus::from_raw((code & 0xff) << 8),
            (None, Some(signal)) if signal > 0 => std::process::ExitStatus::from_raw(signal & 0x7f),
            // Killed with no signal the agent could name: SIGKILL.
            _ => std::process::ExitStatus::from_raw(9),
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(exit.code.map(|code| code as u32).unwrap_or(1))
    }
}

// ---------------------------------------------------------------------------
// The SSH launcher
// ---------------------------------------------------------------------------

struct SshLauncher {
    runner: ShellRunner,
    label: String,
}

/// How the login shell is asked to start the proxy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dialect {
    /// Every Unix shell, and Git Bash, MSYS2 or Cygwin as a Windows
    /// machine's `DefaultShell`: the POSIX line.
    Posix,
    /// `cmd.exe` or PowerShell as a Windows machine's `DefaultShell`: a
    /// PowerShell script, encoded so neither shell reads any of it.
    PowerShell,
}

impl Dialect {
    fn of(shell: LoginShell) -> Self {
        if shell.is_windows() {
            Self::PowerShell
        } else {
            Self::Posix
        }
    }

    fn other(self) -> Self {
        match self {
            Self::Posix => Self::PowerShell,
            Self::PowerShell => Self::Posix,
        }
    }

    /// The login shell to remember for the endpoint once this dialect
    /// worked. `cmd.exe` stands for both Windows shells: they are sent the
    /// same line.
    fn login_shell(self) -> LoginShell {
        match self {
            Self::Posix => LoginShell::Posix,
            Self::PowerShell => LoginShell::Cmd,
        }
    }

    fn line(self, script: &str) -> String {
        match self {
            Self::Posix => remote_shell::posix_line(script),
            Self::PowerShell => remote_shell::powershell_line(script),
        }
    }
}

/// A login that ended before the proxy answered.
struct LoginFailure {
    error: LaunchError,
    /// The reply came from the other family of shell than the one addressed.
    wrong_dialect: bool,
}

impl SshLauncher {
    fn connection_args(&self) -> Vec<String> {
        let ShellRunner::Ssh {
            host,
            port,
            identity_file,
            ..
        } = &self.runner
        else {
            unreachable!("an SSH launcher is only built for an SSH runner");
        };
        // `-T` and `-e none`: no terminal and no escape character, so the
        // channel carries bytes exactly. The keepalives let SSH itself give up
        // on a dead network, and the connection is kept out of any
        // ControlMaster: a multiplexed master that went stale with the network
        // would hang every reconnect behind it.
        let mut args: Vec<String> = [
            "-T",
            "-e",
            "none",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=3",
            "-o",
            "ControlMaster=no",
            "-o",
            "ControlPath=none",
        ]
        .iter()
        .map(|arg| (*arg).to_owned())
        .collect();
        args.extend(run_environment::ssh_connection_args(host, *port, identity_file));
        args
    }

    fn catalog(&self) -> Option<Arc<Catalog>> {
        HUB.get().map(Hub::catalog)
    }

    /// One login that either becomes the proxy or reports what is missing.
    fn bootstrap(
        &self,
        nonce: &str,
        catalog: &Catalog,
        dialect: Dialect,
    ) -> Result<(SshProcess, Preamble), LoginFailure> {
        let script = match dialect {
            Dialect::Posix => bootstrap_script(nonce, catalog),
            Dialect::PowerShell => powershell_bootstrap_script(nonce, catalog),
        };
        let mut args = self.connection_args();
        args.push(dialect.line(&script));
        let mut process = SshProcess::start(&args).map_err(|error| LoginFailure {
            error: LaunchError::Unavailable(format!("cannot run ssh: {error}")),
            wrong_dialect: false,
        })?;
        let mut stdout = process.stdout.take().expect("piped");
        let watchdog = process.watchdog(PREAMBLE_TIMEOUT);
        let preamble = protocol::read_preamble(&mut stdout, nonce);
        watchdog.disarm();
        process.stdout = Some(stdout);
        match preamble {
            Ok(preamble) => Ok((process, preamble)),
            Err(error) => {
                let status = process.finish(Duration::from_secs(5));
                let said = process.stderr_text();
                Err(classify_login_failure(status, &said, &error.to_string(), dialect))
            }
        }
    }

    /// [`Self::bootstrap`] in the dialect the login shell was found to speak,
    /// and in the other one if the machine turns out to speak that: someone
    /// switched its `DefaultShell` since the probe, which is remembered from
    /// then on.
    ///
    /// The reply can say so outright. When it does not, a dialect that was
    /// only remembered is checked by probing again: a shell handed a line it
    /// cannot read may answer anything at all — `cmd.exe` given the POSIX
    /// line acts on the redirections in it and can exit with 255, which reads
    /// like `ssh` failing to reach the machine.
    fn start_proxy(
        &self,
        nonce: &str,
        catalog: &Catalog,
        dialect: &mut Dialect,
        remembered: bool,
    ) -> Result<(SshProcess, Preamble), LaunchError> {
        let failure = match self.bootstrap(nonce, catalog, *dialect) {
            Ok(started) => return Ok(started),
            Err(failure) => failure,
        };
        let switch = failure.wrong_dialect || {
            remembered && {
                remote_shell::forget_login_shell(&self.runner);
                remote_shell::login_shell(&self.runner)
                    .is_ok_and(|(fresh, _)| Dialect::of(fresh) != *dialect)
            }
        };
        if !switch {
            return Err(failure.error);
        }
        *dialect = dialect.other();
        let started = self
            .bootstrap(nonce, catalog, *dialect)
            .map_err(|failure| failure.error)?;
        remote_shell::remember_login_shell(&self.runner, dialect.login_shell());
        Ok(started)
    }

    fn upload(&self, target: &Build, dialect: Dialect) -> Result<(), LaunchError> {
        let read = |path: &Path| {
            std::fs::read(path).map_err(|error| {
                LaunchError::Unavailable(format!("cannot read the agent build {}: {error}", path.display()))
            })
        };
        let bytes = read(&target.path)?;
        let helper = target.helper.as_deref().map(read).transpose()?;
        let sizes = UploadSizes {
            agent: bytes.len() as u64,
            helper: helper.as_ref().map(|helper| helper.len() as u64),
        };
        let script = match dialect {
            Dialect::Posix => upload_script(&target.tag, sizes),
            Dialect::PowerShell => powershell_upload_script(&target.tag, sizes),
        };
        let mut args = self.connection_args();
        args.push(dialect.line(&script));
        let mut process = SshProcess::start(&args)
            .map_err(|error| LaunchError::Unavailable(format!("cannot run ssh: {error}")))?;
        let mut stdin = process.stdin.take().expect("piped");
        let writer = std::thread::spawn(move || {
            let _ = stdin.write_all(&bytes);
            if let Some(helper) = helper {
                let _ = stdin.write_all(&helper);
            }
            drop(stdin);
        });
        let mut stdout = process.stdout.take().expect("piped");
        let watchdog = process.watchdog(UPLOAD_TIMEOUT);
        let mut reply = String::new();
        let _ = stdout.read_to_string(&mut reply);
        watchdog.disarm();
        let _ = writer.join();
        let status = process.finish(Duration::from_secs(10));
        let said = process.stderr_text();
        if status == Some(255) {
            return Err(LaunchError::Unreachable(if said.is_empty() {
                "ssh could not reach the machine".into()
            } else {
                said
            }));
        }
        if status != Some(0) {
            return Err(LaunchError::Unavailable(format!(
                "could not install the agent on the machine (exit {status:?}){}",
                if said.is_empty() { String::new() } else { format!(": {said}") }
            )));
        }
        // The upload script ran the new binary once; what it reported must be
        // the build that was sent.
        let reported: serde_json::Value = reply
            .lines()
            .rev()
            .find_map(|line| serde_json::from_str(line.trim()).ok())
            .unwrap_or(serde_json::Value::Null);
        if reported["build"].as_str() != Some(target.digest.as_str()) {
            return Err(LaunchError::Unavailable(format!(
                "the uploaded agent did not identify itself as the build that was sent ({})",
                reply.trim()
            )));
        }
        Ok(())
    }
}

impl Launcher for SshLauncher {
    fn launch(&self, nonce: &str) -> Result<Transport, LaunchError> {
        let (mut dialect, remembered) = match remote_shell::login_shell(&self.runner) {
            Ok((shell, remembered)) => (Dialect::of(shell), remembered),
            Err(error) => return Err(LaunchError::Unreachable(error)),
        };
        let Some(mut catalog) = self.catalog() else {
            return Err(LaunchError::Unavailable("the agent is not installed in this host".into()));
        };
        if catalog.builds.is_empty() && local_agent_builder().is_none() {
            return Err(LaunchError::Unavailable(
                "this Mework has no agent builds to install on remote machines".into(),
            ));
        }
        let (mut process, preamble) = self.start_proxy(nonce, &catalog, &mut dialect, remembered)?;
        let (process, preamble) = match preamble {
            Preamble::Ready => (process, Preamble::Ready),
            // The machine has no agent of this host's build: none at all, or another Mework's —
            // older or newer. Either way it gets this host's own, which is the whole of updating
            // it and of rolling it back.
            Preamble::Missing { os, arch } => {
                process.finish(Duration::from_secs(5));
                let build = match catalog.for_machine(&os, &arch) {
                    Some(build) => build.clone(),
                    None => {
                        catalog = build_locally(&os, &arch)?;
                        catalog.for_machine(&os, &arch).cloned().ok_or_else(|| {
                            LaunchError::Unavailable(format!("there is no agent build for {os}/{arch}"))
                        })?
                    }
                };
                eprintln!(
                    "[remote-agent] {}: installing agent {} for {os}/{arch}",
                    self.label, build.tag
                );
                self.upload(&build, dialect)?;
                self.bootstrap(nonce, &catalog, dialect)
                    .map_err(|failure| failure.error)?
            }
        };
        let Preamble::Ready = preamble else {
            return Err(LaunchError::Unavailable(
                "the agent was installed but did not start".into(),
            ));
        };
        Ok(process.into_transport())
    }
}

/// What a login that ended before the proxy answered means.
fn classify_login_failure(status: Option<i32>, said: &str, read_error: &str, dialect: Dialect) -> LoginFailure {
    let detail = if said.is_empty() {
        read_error.to_owned()
    } else {
        said.to_owned()
    };
    if status == Some(255) || status.is_none() {
        // OpenSSH's own failures (and a login the watchdog cut off) are about
        // reaching the machine, which a later attempt may do better.
        return LoginFailure {
            error: LaunchError::Unreachable(detail),
            wrong_dialect: false,
        };
    }
    let wrong_dialect = match dialect {
        Dialect::Posix => run_environment::answered_by_non_posix_shell(status, said),
        // A Unix shell that was handed the PowerShell line has no
        // `powershell` to run.
        Dialect::PowerShell => status == Some(127) || said.contains("powershell: command not found") || said.contains("powershell: not found"),
    };
    if wrong_dialect {
        return LoginFailure {
            error: LaunchError::Unavailable(match dialect {
                Dialect::Posix => "the login shell is not a POSIX shell".into(),
                Dialect::PowerShell => "the login shell is not cmd.exe or PowerShell".into(),
            }),
            wrong_dialect: true,
        };
    }
    // The login ran and the agent refused to start: a home that cannot be
    // written, a filesystem mounted noexec. The per-command path may still
    // work there.
    LoginFailure {
        error: LaunchError::Unavailable(format!("the agent could not start (exit {status:?}): {detail}")),
        wrong_dialect: false,
    }
}

/// Which machine `sh` is on: `$S` and `$M` as `uname` spells them, except
/// that the POSIX layers of Windows — Git Bash and MSYS2 (`MINGW64_NT-10.0…`,
/// `MSYS_NT-…`) and Cygwin — are all `Windows`: the agent they start is the
/// Windows one, whichever layer started it.
const MACHINE_SH: &str = "S=$(uname -s 2>/dev/null); M=$(uname -m 2>/dev/null)\n\
    case \"$S\" in MINGW*|MSYS*|CYGWIN*) S=Windows ;; esac\n";

/// Where the agent lives, as `sh` finds it: `$R`, and `$X`, the suffix of its
/// executable. On Windows that is where the agent itself looks — under the
/// Windows profile, which need not be the POSIX layer's `$HOME` — spelled the
/// way the POSIX layer reads a path.
const ROOT_SH: &str = "if [ \"$S\" = Windows ]; then\n\
    X=.exe\n\
    R=\"${MEWORK_REMOTE_ROOT:-$USERPROFILE/.mework/remote}\"\n\
    R=$(cygpath -u \"$R\" 2>/dev/null || printf '%s' \"$R\")\n\
    else\n\
    X=\n\
    R=\"${MEWORK_REMOTE_ROOT:-$HOME/.mework/remote}\"\n\
    fi\n";

/// The login script: exec the proxy of the build that matches this machine,
/// or say which machine it is so the right build can be uploaded. Only `sh`
/// reads it (see [`remote_shell::posix_line`]).
fn bootstrap_script(nonce: &str, catalog: &Catalog) -> String {
    let mut cases = String::new();
    for (pattern, tag) in catalog.uname_cases() {
        cases.push_str(&format!("{pattern}) T={} ;;\n", run_environment::sh_single_quote(&tag)));
    }
    format!(
        "{MACHINE_SH}\
         case \"$S/$M\" in\n{cases}*) T= ;;\nesac\n\
         {ROOT_SH}\
         if [ -n \"$T\" ] && [ -x \"$R/bin/$T/mework-remote$X\" ]; then\n\
         exec \"$R/bin/$T/mework-remote$X\" proxy --sync {nonce}\n\
         fi\n\
         printf '\\n{marker} {nonce} %s %s\\n' \"$S\" \"$M\"\n",
        marker = protocol::MISSING_MARKER,
    )
}

/// What an upload sends on standard input, in order: the agent, then (Windows) the sandbox
/// helper.
#[derive(Clone, Copy)]
struct UploadSizes {
    agent: u64,
    helper: Option<u64>,
}

/// Receives a build on stdin, proves it runs here, and moves it into place.
/// Other builds beyond the newest few are removed; one a daemon still runs
/// from keeps running, since a Unix file outlives its name (and Windows
/// refuses to remove it, which leaves it for a later upload).
///
/// A Windows build (through Git Bash) brings the sandbox helper behind the
/// agent. The two arrive as one stream, kept whole in a file and split there,
/// and the helper is in place before the agent is: an agent in its directory
/// means the build is complete.
fn upload_script(tag: &str, sizes: UploadSizes) -> String {
    let tag = run_environment::sh_single_quote(tag);
    let receive = match sizes.helper {
        None => "trap 'rm -f \"$U\"' EXIT\n\
                 cat > \"$U\"\n\
                 chmod 700 \"$U\"\n\
                 \"$U\" version --json\n"
            .to_owned(),
        Some(helper) => format!(
            "H=\"$D/.upload.$$.{SANDBOX_HELPER}\"\n\
             P=\"$D/.upload.$$.part\"\n\
             trap 'rm -f \"$U\" \"$H\" \"$P\"' EXIT\n\
             cat > \"$P\"\n\
             head -c {agent} \"$P\" > \"$U\"\n\
             tail -c +{after} \"$P\" > \"$H\"\n\
             rm -f \"$P\"\n\
             [ \"$(wc -c < \"$H\")\" -eq {helper} ] || {{ echo 'the sandbox helper arrived incomplete' >&2; exit 1; }}\n\
             chmod 700 \"$U\" \"$H\"\n\
             \"$U\" version --json\n\
             [ -e \"$D/{SANDBOX_HELPER}\" ] || mv -f \"$H\" \"$D/{SANDBOX_HELPER}\"\n",
            agent = sizes.agent,
            after = sizes.agent + 1,
        ),
    };
    format!(
        "set -e\n\
         umask 077\n\
         {MACHINE_SH}\
         {ROOT_SH}\
         D=\"$R/bin/\"{tag}\n\
         mkdir -p \"$D\"\n\
         U=\"$D/.upload.$$$X\"\n\
         {receive}\
         mv -f \"$U\" \"$D/mework-remote$X\"\n\
         trap - EXIT\n\
         (cd \"$R/bin\" && ls -1t | sed -n '4,$p' | while IFS= read -r old; do\n\
         case \"$old\" in *-*) [ \"$old\" = {tag} ] || rm -rf -- \"$old\" ;; esac\n\
         done) || true\n"
    )
}

/// `$R`, the agent's directory, as PowerShell finds it: where the agent
/// itself looks.
const ROOT_PS: &str = "$R = $env:MEWORK_REMOTE_ROOT\n\
    if (-not $R) { $R = Join-Path $env:USERPROFILE '.mework\\remote' }\n";

/// [`bootstrap_script`] for a Windows login shell, in PowerShell, which every
/// Windows since 7 has whether `cmd.exe` or PowerShell is the login shell.
/// The processor comes from the environment Windows sets for every process;
/// `PROCESSOR_ARCHITEW6432` is the machine's own when a 32-bit PowerShell
/// runs on a 64-bit Windows.
///
/// The proxy is started by PowerShell, not in its place — Windows has no
/// `exec` — and inherits the SSH channel's standard handles directly: a
/// native program that is the last thing on its line is not piped through
/// PowerShell, so the stream stays byte for byte what the agent wrote.
fn powershell_bootstrap_script(nonce: &str, catalog: &Catalog) -> String {
    let mut cases = String::new();
    for (arch, tag) in catalog.windows_arch_cases() {
        cases.push_str(&format!(
            "{} {{ {} }}\n",
            remote_shell::ps_single_quote(arch),
            remote_shell::ps_single_quote(&tag)
        ));
    }
    format!(
        "$ProgressPreference = 'SilentlyContinue'\n\
         {ROOT_PS}\
         $A = $env:PROCESSOR_ARCHITEW6432\n\
         if (-not $A) {{ $A = $env:PROCESSOR_ARCHITECTURE }}\n\
         $T = switch ($A) {{\n{cases}default {{ '' }}\n}}\n\
         if ($T) {{\n\
         $E = Join-Path $R \"bin\\$T\\mework-remote.exe\"\n\
         if (Test-Path -LiteralPath $E -PathType Leaf) {{ & $E proxy --sync {nonce}; exit $LASTEXITCODE }}\n\
         }}\n\
         [Console]::Out.Write(\"`n{marker} {nonce} Windows $A`n\")\n",
        marker = protocol::MISSING_MARKER,
    )
}

/// [`upload_script`] in PowerShell. A build already in place is the same
/// bytes — its directory is named by their digest — and may be the one a
/// daemon runs from, which Windows will not replace, so it is kept; so is a
/// sandbox helper already there. The helper, when the build brings one, is in
/// place before the agent is.
///
/// The build arrives on standard input, read as bytes through a stream of the
/// script's own on the input handle, the agent's and then the helper's exact
/// sizes. Windows PowerShell's `[Console]::OpenStandardInput()` never returns
/// when the input is already waiting in the pipe as it starts to read — which
/// it is, for a build sent right behind the command. The handle comes from the
/// runtime's own `GetStdHandle`, or where that is not to be found, from a
/// declaration compiled on the spot.
fn powershell_upload_script(tag: &str, sizes: UploadSizes) -> String {
    let tag = remote_shell::ps_single_quote(tag);
    let (receive_helper, place_helper) = match sizes.helper {
        Some(helper) => (
            format!("Receive $V {helper}\n"),
            format!(
                "$H = Join-Path $D '{SANDBOX_HELPER}'\n\
                 if (-not (Test-Path -LiteralPath $H)) {{ Move-Item -LiteralPath $V -Destination $H }}\n"
            ),
        ),
        None => (String::new(), String::new()),
    };
    format!(
        "$ErrorActionPreference = 'Stop'\n\
         $ProgressPreference = 'SilentlyContinue'\n\
         {ROOT_PS}\
         $B = Join-Path $R 'bin'\n\
         $D = Join-Path $B {tag}\n\
         New-Item -ItemType Directory -Force -Path $D | Out-Null\n\
         $F = Join-Path $D 'mework-remote.exe'\n\
         $U = Join-Path $D ('.upload.' + $PID + '.exe')\n\
         $V = Join-Path $D ('.upload.' + $PID + '.{SANDBOX_HELPER}')\n\
         $native = [Console].Assembly.GetType('Microsoft.Win32.Win32Native')\n\
         $get = if ($native) {{ $native.GetMethod('GetStdHandle', [Reflection.BindingFlags]'NonPublic, Static') }}\n\
         if ($get) {{ $handle = $get.Invoke($null, @([int]-10)) }} else {{\n\
         Add-Type -Namespace MeworkUpload -Name Native -MemberDefinition '[DllImport(\"kernel32.dll\")] public static extern IntPtr GetStdHandle(int n);'\n\
         $handle = [MeworkUpload.Native]::GetStdHandle(-10)\n\
         }}\n\
         $buffer = New-Object byte[] 65536\n\
         function Receive($path, [long]$left) {{\n\
         $out = [IO.File]::Create($path)\n\
         try {{ while ($left -gt 0) {{\n\
         $n = $in.Read($buffer, 0, [Math]::Min(65536, $left))\n\
         if ($n -le 0) {{ throw 'the upload ended early' }}\n\
         $out.Write($buffer, 0, $n); $left -= $n\n\
         }} }} finally {{ $out.Close() }}\n\
         }}\n\
         try {{\n\
         $in = New-Object IO.FileStream((New-Object Microsoft.Win32.SafeHandles.SafeFileHandle($handle, $false)), ([IO.FileAccess]::Read))\n\
         Receive $U {agent}\n\
         {receive_helper}\
         $reply = & $U version --json\n\
         if ($LASTEXITCODE -ne 0) {{ throw \"the uploaded agent does not run on this machine (exit $LASTEXITCODE)\" }}\n\
         [Console]::Out.Write((($reply | Out-String).Trim()) + \"`n\")\n\
         {place_helper}\
         if (-not (Test-Path -LiteralPath $F)) {{ Move-Item -LiteralPath $U -Destination $F }}\n\
         }} finally {{\n\
         foreach ($P in $U, $V) {{ if (Test-Path -LiteralPath $P) {{ Remove-Item -LiteralPath $P -Force -ErrorAction SilentlyContinue }} }}\n\
         }}\n\
         Get-ChildItem -LiteralPath $B -Directory | Where-Object {{ $_.Name -like '*-*' -and $_.Name -ne {tag} }} | \
         Sort-Object LastWriteTime -Descending | Select-Object -Skip 2 | \
         ForEach-Object {{ Remove-Item -LiteralPath $_.FullName -Recurse -Force -ErrorAction SilentlyContinue }}\n",
        agent = sizes.agent,
    )
}

// ---------------------------------------------------------------------------
// Agent builds
// ---------------------------------------------------------------------------

/// The Windows sandbox's helper, which travels with a Windows build and lives beside it: the
/// agent finds it next to its own executable (`build-remote-agents.mjs` stages the two
/// together).
const SANDBOX_HELPER: &str = "srt-win.exe";

/// One agent executable this host can install, with the sandbox helper beside it on Windows.
#[derive(Clone)]
struct Build {
    triple: String,
    path: PathBuf,
    /// The executable's SHA-256, which the agent reports as its `build`.
    digest: String,
    /// The directory a machine keeps the build in: the version and a digest of everything
    /// installed there, so a build whose helper changed is installed anew.
    tag: String,
    helper: Option<PathBuf>,
}

impl Build {
    /// The build at `path` for `triple`, if it was made from this host's agent source.
    fn read(triple: &str, path: PathBuf) -> Option<Self> {
        let bytes = std::fs::read(&path).ok()?;
        match remote_agent::source_of_executable(&bytes) {
            Some(source) if source == remote_agent::SOURCE_ID => {}
            other => {
                eprintln!(
                    "[remote-agent] not using the {triple} build at {}: {}",
                    path.display(),
                    match other {
                        Some(source) => format!(
                            "it was built from other agent source ({} where this Mework has {})",
                            &source[..12],
                            &remote_agent::SOURCE_ID[..12]
                        ),
                        None => "it predates agent source identities".to_owned(),
                    }
                );
                return None;
            }
        }
        use sha2::{Digest, Sha256};
        let digest = format!("{:x}", Sha256::digest(&bytes));
        let helper = Some(path.with_file_name(SANDBOX_HELPER))
            .filter(|helper| triple.contains("-windows-") && helper.is_file());
        let installed = match helper.as_ref().and_then(|helper| std::fs::read(helper).ok()) {
            Some(helper) => format!("{:x}", Sha256::new().chain_update(&bytes).chain_update(&helper).finalize()),
            None => digest.clone(),
        };
        Some(Self {
            triple: triple.to_owned(),
            tag: format!("{}-{}", remote_agent::AGENT_VERSION, &installed[..12]),
            path,
            digest,
            helper,
        })
    }
}

/// Every agent build found, by target triple.
struct Catalog {
    builds: Vec<Build>,
}

/// The triples a machine reporting its system and processor can run, best
/// first: `uname -s`/`uname -m` from `sh`, or `Windows` and
/// `PROCESSOR_ARCHITECTURE` from PowerShell. Linux prefers the static musl
/// build, which runs on any distribution. Windows prefers the MSVC build,
/// which is what a Windows machine builds; a Windows on Arm without an Arm
/// build runs the x64 one emulated.
fn triples_for(os: &str, arch: &str) -> &'static [&'static str] {
    const WINDOWS_X64: &[&str] = &["x86_64-pc-windows-msvc", "x86_64-pc-windows-gnullvm", "x86_64-pc-windows-gnu"];
    const WINDOWS_ARM64: &[&str] = &[
        "aarch64-pc-windows-msvc",
        "aarch64-pc-windows-gnullvm",
        "x86_64-pc-windows-msvc",
        "x86_64-pc-windows-gnullvm",
        "x86_64-pc-windows-gnu",
    ];
    match (os, arch) {
        ("Linux", "x86_64" | "amd64") => &["x86_64-unknown-linux-musl", "x86_64-unknown-linux-gnu"],
        ("Linux", "aarch64" | "arm64") => &["aarch64-unknown-linux-musl", "aarch64-unknown-linux-gnu"],
        ("Darwin", "arm64" | "aarch64") => &["aarch64-apple-darwin"],
        ("Darwin", "x86_64") => &["x86_64-apple-darwin"],
        ("Windows", "x86_64" | "amd64" | "AMD64") => WINDOWS_X64,
        ("Windows", "aarch64" | "arm64" | "ARM64") => WINDOWS_ARM64,
        _ => &[],
    }
}

/// `case` patterns for the POSIX bootstrap, one per platform a build exists
/// for. Windows is what [`MACHINE_SH`] calls every POSIX layer of it.
const PLATFORMS: &[(&str, &str, &str)] = &[
    ("Linux/x86_64|Linux/amd64", "Linux", "x86_64"),
    ("Linux/aarch64|Linux/arm64", "Linux", "aarch64"),
    ("Darwin/arm64|Darwin/aarch64", "Darwin", "arm64"),
    ("Darwin/x86_64", "Darwin", "x86_64"),
    ("Windows/x86_64|Windows/amd64", "Windows", "x86_64"),
    ("Windows/aarch64|Windows/arm64", "Windows", "aarch64"),
];

/// The `PROCESSOR_ARCHITECTURE` values the PowerShell bootstrap tells apart.
const WINDOWS_ARCHES: &[&str] = &["AMD64", "ARM64"];

/// The agent's executable name in a build for `triple`.
fn agent_binary(triple: &str) -> String {
    if triple.contains("-windows-") {
        format!("{}.exe", remote_agent::AGENT_BINARY)
    } else {
        remote_agent::AGENT_BINARY.to_owned()
    }
}

impl Catalog {
    /// Every build under `dirs` (and, for this host's own triple, `native`) that was made from
    /// the agent source this host was compiled against ([`remote_agent::SOURCE_ID`]).
    ///
    /// Anything else is left out, however it got there — a build staged before the agent's
    /// source last changed, one copied in from another checkout, one from before source
    /// identities. A machine is only ever given the agent this host speaks for, so a machine
    /// running any other one is moved to this one on its next connection, forward or back.
    fn discover(dirs: &[PathBuf], native: &[PathBuf]) -> Self {
        let mut builds = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for (_, os, arch) in PLATFORMS {
            for triple in triples_for(os, arch) {
                if !seen.insert(*triple) {
                    continue;
                }
                let staged = dirs
                    .iter()
                    .map(|dir| dir.join(triple).join(agent_binary(triple)))
                    .filter(|path| path.is_file());
                // This machine's own triple may also have a build straight out of `cargo build`.
                let own = native
                    .iter()
                    .filter(|path| *triple == env!("MEWORK_TARGET_TRIPLE") && path.is_file())
                    .cloned();
                let mut candidates: Vec<Build> = staged
                    .chain(own)
                    .filter_map(|path| Build::read(triple, path))
                    .collect();
                // Of several builds from this source, the newest is the one meant — after one
                // with the Windows sandbox helper beside it, which a bare `cargo build` lacks.
                candidates.sort_by_key(|build| {
                    (
                        std::cmp::Reverse(build.helper.is_some()),
                        std::cmp::Reverse(modified(&build.path)),
                    )
                });
                if let Some(build) = candidates.into_iter().next() {
                    builds.push(build);
                }
            }
        }
        for build in &builds {
            eprintln!(
                "[remote-agent] agent build {} for {} at {}",
                build.tag,
                build.triple,
                build.path.display()
            );
        }
        Self { builds }
    }

    fn for_machine(&self, os: &str, arch: &str) -> Option<&Build> {
        triples_for(os, arch)
            .iter()
            .find_map(|triple| self.builds.iter().find(|build| build.triple == *triple))
    }

    fn uname_cases(&self) -> Vec<(&'static str, String)> {
        PLATFORMS
            .iter()
            .filter_map(|(pattern, os, arch)| {
                self.for_machine(os, arch).map(|build| (*pattern, build.tag.clone()))
            })
            .collect()
    }

    fn windows_arch_cases(&self) -> Vec<(&'static str, String)> {
        WINDOWS_ARCHES
            .iter()
            .filter_map(|arch| {
                self.for_machine("Windows", arch)
                    .map(|build| (*arch, build.tag.clone()))
            })
            .collect()
    }
}

fn modified(path: &Path) -> std::time::SystemTime {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
}

/// Where a development build finds agents built from this source tree:
/// `npm run build:remote-agents` stages every target it can build under
/// `src-tauri/remote-agents/`, and a plain `cargo build -p mework-remote-agent`
/// leaves this machine's own build in the target directory.
fn source_tree_agents() -> (Vec<PathBuf>, Vec<PathBuf>) {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("target"));
    let native = ["release", "debug"]
        .iter()
        .map(|profile| {
            target
                .join(profile)
                .join(format!("{}{}", remote_agent::AGENT_BINARY, std::env::consts::EXE_SUFFIX))
        })
        .collect();
    (vec![manifest.join("remote-agents")], native)
}

/// How `build-remote-agents.mjs --only <triple>` says this computer has no way to build that
/// triple at all, as opposed to trying and failing.
const CANNOT_BUILD_HERE: i32 = 3;

/// The repository root and the script that builds agents from it, in a development build. A
/// release has neither, and needs neither: every build it carries was made from its own source,
/// for every platform its release was built with.
fn local_agent_builder() -> Option<(PathBuf, PathBuf)> {
    if !cfg!(debug_assertions) {
        return None;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent()?.to_path_buf();
    let script = root.join("scripts").join("build-remote-agents.mjs");
    script.is_file().then_some((root, script))
}

/// Builds the agent for a machine reporting `os`/`arch` on this computer, from the source this
/// host was compiled from, and returns the catalog that has it.
///
/// The machine takes no part beyond saying what it is. The build is made here, with this
/// computer's toolchain and network — an SSH machine being reachable says nothing about what it
/// can reach — and gets to the machine the way every build does, over SSH.
fn build_locally(os: &str, arch: &str) -> Result<Arc<Catalog>, LaunchError> {
    let hub = HUB
        .get()
        .ok_or_else(|| LaunchError::Unavailable("the agent is not installed in this host".into()))?;
    let missing = format!("this Mework has no agent build for {os}/{arch} made from its own source");
    let Some((root, script)) = local_agent_builder() else {
        return Err(LaunchError::Unavailable(missing));
    };
    let gate = hub.building(&format!("{os}/{arch}"));
    let _building = lock(&gate);
    // Another machine of this platform may have had one built while this one waited.
    let catalog = hub.rediscover();
    if catalog.for_machine(os, arch).is_some() {
        return Ok(catalog);
    }
    let mut reasons = Vec::new();
    for triple in triples_for(os, arch) {
        eprintln!("[remote-agent] building the {triple} agent here for a {os}/{arch} machine");
        let output = Command::new("node")
            .arg(&script)
            .args(["--only", triple])
            .current_dir(&root)
            .stdin(Stdio::null())
            .stderr(Stdio::inherit())
            .output();
        let output = match output {
            Ok(output) => output,
            Err(error) => {
                reasons.push(format!("cannot run node to build it: {error}"));
                break;
            }
        };
        let said = String::from_utf8_lossy(&output.stdout);
        eprint!("{said}");
        match output.status.code() {
            Some(0) => {
                let catalog = hub.rediscover();
                if catalog.for_machine(os, arch).is_some() {
                    return Ok(catalog);
                }
                reasons.push(format!("the {triple} build made here is not usable"));
            }
            Some(CANNOT_BUILD_HERE) => reasons.extend(
                said.lines()
                    .filter_map(|line| line.strip_prefix("[remote-agents] skip "))
                    .map(str::to_owned),
            ),
            status => reasons.push(format!("building {triple} failed (exit {status:?})")),
        }
    }
    Err(LaunchError::Unavailable(format!(
        "{missing}, and this computer could not build one: {}",
        reasons.join("; ")
    )))
}

// ---------------------------------------------------------------------------
// One ssh process
// ---------------------------------------------------------------------------

struct SshProcess {
    child: Arc<Mutex<Option<Child>>>,
    stdin: Option<std::process::ChildStdin>,
    stdout: Option<std::process::ChildStdout>,
    stderr: Arc<Mutex<Vec<u8>>>,
}

struct Watchdog {
    armed: Arc<std::sync::atomic::AtomicBool>,
}

impl Watchdog {
    fn disarm(self) {
        self.armed.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

impl SshProcess {
    fn start(args: &[String]) -> std::io::Result<Self> {
        let mut last = None;
        for program in run_environment::ssh_client_candidates() {
            let mut command = Command::new(&program);
            command
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            for name in crate::child_environment::private_child_environment_names() {
                command.env_remove(&name);
            }
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt as _;
                command.creation_flags(0x0800_0000);
            }
            match command.spawn() {
                Ok(mut child) => {
                    let stderr = Arc::new(Mutex::new(Vec::new()));
                    if let Some(mut pipe) = child.stderr.take() {
                        let sink = Arc::clone(&stderr);
                        std::thread::spawn(move || {
                            let mut chunk = [0u8; 4096];
                            while let Ok(count) = pipe.read(&mut chunk) {
                                if count == 0 {
                                    break;
                                }
                                let mut kept = lock(&sink);
                                kept.extend_from_slice(&chunk[..count]);
                                let excess = kept.len().saturating_sub(8192);
                                kept.drain(..excess);
                            }
                        });
                    }
                    return Ok(Self {
                        stdin: child.stdin.take(),
                        stdout: child.stdout.take(),
                        child: Arc::new(Mutex::new(Some(child))),
                        stderr,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => last = Some(error),
                Err(error) => return Err(error),
            }
        }
        Err(last.unwrap_or_else(|| std::io::Error::other("no ssh client")))
    }

    /// Kills the process if it is still running after `timeout`, unless
    /// disarmed first.
    fn watchdog(&self, timeout: Duration) -> Watchdog {
        let armed = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let child = Arc::clone(&self.child);
        let flag = Arc::clone(&armed);
        std::thread::spawn(move || {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                if !flag.load(std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            if flag.load(std::sync::atomic::Ordering::SeqCst) {
                if let Some(child) = lock(&child).as_mut() {
                    let _ = child.kill();
                }
            }
        });
        Watchdog { armed }
    }

    /// Waits up to `timeout` for the process to end, killing it after that,
    /// and returns its exit code.
    fn finish(&mut self, timeout: Duration) -> Option<i32> {
        use wait_timeout::ChildExt;
        drop(self.stdin.take());
        let mut child = lock(&self.child).take()?;
        let status = match child.wait_timeout(timeout) {
            Ok(Some(status)) => Some(status),
            _ => {
                let _ = child.kill();
                child.wait().ok()
            }
        };
        // The stderr drain ends with the process; let it catch up.
        std::thread::sleep(Duration::from_millis(50));
        status.and_then(|status| status.code())
    }

    fn stderr_text(&self) -> String {
        let bytes = lock(&self.stderr).clone();
        String::from_utf8_lossy(&bytes).trim().to_owned()
    }

    fn into_transport(mut self) -> Transport {
        let reader = self.stdout.take().expect("the proxy's stdout");
        let writer = self.stdin.take().expect("the proxy's stdin");
        let child = Arc::clone(&self.child);
        Transport {
            reader: Box::new(reader),
            writer: Box::new(writer),
            closer: Box::new(move || {
                if let Some(mut child) = lock(&child).take() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }),
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog_with(triples: &[&str]) -> Catalog {
        Catalog {
            builds: triples
                .iter()
                .enumerate()
                .map(|(index, triple)| Build {
                    triple: (*triple).to_owned(),
                    path: PathBuf::from(format!("/builds/{triple}")),
                    digest: format!("{index:0>64}"),
                    tag: format!("0.1.0-{index:0>12}"),
                    helper: None,
                })
                .collect(),
        }
    }

    fn sizes(agent: &[u8], helper: Option<&[u8]>) -> UploadSizes {
        UploadSizes {
            agent: agent.len() as u64,
            helper: helper.map(|helper| helper.len() as u64),
        }
    }

    /// A Windows build and its sandbox helper are installed as one: the directory a machine keeps
    /// them in is named by both, so a changed helper is a new build there.
    #[test]
    fn a_windows_build_carries_the_sandbox_helper_beside_it() {
        let root = tempfile::tempdir().unwrap();
        let agent = format!("MZ fake agent mework-remote-source:{}", remote_agent::SOURCE_ID);
        for triple in ["x86_64-pc-windows-msvc", "x86_64-unknown-linux-musl"] {
            let directory = root.path().join(triple);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join(agent_binary(triple)), &agent).unwrap();
        }
        let windows = root.path().join("x86_64-pc-windows-msvc/mework-remote.exe");
        let bare = Build::read("x86_64-pc-windows-msvc", windows.clone()).unwrap();
        assert!(bare.helper.is_none());

        std::fs::write(windows.with_file_name(SANDBOX_HELPER), "srt-win one").unwrap();
        let with_helper = Build::read("x86_64-pc-windows-msvc", windows.clone()).unwrap();
        assert_eq!(with_helper.helper.as_deref(), Some(windows.with_file_name(SANDBOX_HELPER).as_path()));
        assert_eq!(with_helper.digest, bare.digest, "the agent reports the same build");
        assert_ne!(with_helper.tag, bare.tag);
        std::fs::write(windows.with_file_name(SANDBOX_HELPER), "srt-win two").unwrap();
        assert_ne!(Build::read("x86_64-pc-windows-msvc", windows).unwrap().tag, with_helper.tag);

        // Only Windows has one.
        let linux = root.path().join("x86_64-unknown-linux-musl/mework-remote");
        std::fs::write(linux.with_file_name(SANDBOX_HELPER), "stray").unwrap();
        assert!(Build::read("x86_64-unknown-linux-musl", linux).unwrap().helper.is_none());
    }

    #[test]
    fn a_machine_is_checked_where_ssh_connects_unless_a_proxy_does() {
        assert_eq!(
            parse_ssh_endpoint("user holycat\nhostname desktop.example.ts.net\nport 2222\nconnecttimeout none\n"),
            Some(("desktop.example.ts.net".to_owned(), 2222))
        );
        assert_eq!(parse_ssh_endpoint("hostname 10.0.0.2\nport 22\nproxycommand none\n"), Some(("10.0.0.2".to_owned(), 22)));
        assert_eq!(parse_ssh_endpoint("hostname 10.0.0.2\nport 22\nproxyjump bastion\n"), None);
        assert_eq!(parse_ssh_endpoint("hostname 10.0.0.2\nport 22\nproxycommand nc %h %p\n"), None);
        assert_eq!(parse_ssh_endpoint("port 22\n"), None);
    }

    #[test]
    fn a_machine_nothing_answers_for_is_not_there() {
        let runner = |host: &str, port: u16| ShellRunner::Ssh {
            agent_shell: Default::default(),
            host: host.to_owned(),
            port,
            identity_file: String::new(),
            env: Default::default(),
        };
        let within = Duration::from_millis(500);
        // A port nothing listens on is refused at once; a reserved name never resolves.
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        let started = Instant::now();
        assert!(!machine_is_there(&runner("127.0.0.1", port), within));
        assert!(!machine_is_there(&runner("there-is-no-such-machine.invalid", 0), within));
        assert!(started.elapsed() < within * 2, "{:?}", started.elapsed());
        let open = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        assert!(machine_is_there(&runner("127.0.0.1", open.local_addr().unwrap().port()), within));
    }

    #[test]
    fn a_machine_gets_the_best_build_it_can_run() {
        let catalog = catalog_with(&["x86_64-unknown-linux-gnu", "x86_64-unknown-linux-musl", "aarch64-apple-darwin"]);
        assert_eq!(
            catalog.for_machine("Linux", "x86_64").map(|build| build.triple.as_str()),
            Some("x86_64-unknown-linux-musl")
        );
        assert_eq!(
            catalog.for_machine("Linux", "amd64").map(|build| build.triple.as_str()),
            Some("x86_64-unknown-linux-musl")
        );
        assert_eq!(
            catalog.for_machine("Darwin", "arm64").map(|build| build.triple.as_str()),
            Some("aarch64-apple-darwin")
        );
        assert!(catalog.for_machine("Linux", "riscv64").is_none());
        assert!(catalog.for_machine("FreeBSD", "amd64").is_none());
    }

    /// The bootstrap is only ever read by `sh`, but it has to survive the
    /// neutral line every login shell passes on, and name only the builds
    /// this host can actually install.
    #[cfg(unix)]
    #[test]
    fn the_bootstrap_reports_a_machine_without_the_agent() {
        let catalog = catalog_with(&["aarch64-apple-darwin", "x86_64-unknown-linux-musl"]);
        let script = bootstrap_script("n0nce", &catalog);
        assert!(script.contains("Linux/x86_64|Linux/amd64) T='0.1.0-000000000001'"), "{script}");
        assert!(!script.contains("Linux/aarch64"), "no build, no case: {script}");
        let root = tempfile::tempdir().unwrap();
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(remote_shell::posix_line(&script))
            .env("MEWORK_REMOTE_ROOT", root.path())
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        let reported = protocol::read_preamble(&mut text.as_bytes(), "n0nce").unwrap();
        let Preamble::Missing { os, arch } = reported else {
            panic!("expected a missing report: {text}")
        };
        assert_eq!(os, String::from_utf8_lossy(&std::process::Command::new("uname").arg("-s").output().unwrap().stdout).trim());
        assert!(!arch.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn the_upload_script_installs_only_a_build_that_runs() {
        let root = tempfile::tempdir().unwrap();
        let fake = "#!/bin/sh\nprintf '{\"build\":\"abc\"}\\n'\n";
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(remote_shell::posix_line(&upload_script("0.1.0-abc", sizes(fake.as_bytes(), None))))
            .env("MEWORK_REMOTE_ROOT", root.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                child.stdin.take().unwrap().write_all(fake.as_bytes())?;
                child.wait_with_output()
            })
            .unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("\"build\":\"abc\""));
        let installed = root.path().join("bin/0.1.0-abc/mework-remote");
        assert_eq!(std::fs::read_to_string(&installed).unwrap(), fake);

        // A build that cannot run here is never put in place.
        let broken = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(remote_shell::posix_line(&upload_script("0.1.0-bad", sizes(b"\x7fELF not really", None))))
            .env("MEWORK_REMOTE_ROOT", root.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                child.stdin.take().unwrap().write_all(b"\x7fELF not really")?;
                child.wait_with_output()
            })
            .unwrap();
        assert!(!broken.status.success());
        assert!(!root.path().join("bin/0.1.0-bad/mework-remote").exists());
        let leftovers: Vec<_> = std::fs::read_dir(root.path().join("bin/0.1.0-bad")).unwrap().collect();
        assert!(leftovers.is_empty(), "the partial upload is removed");
    }

    /// Against a real sshd, which this suite cannot assume: set
    /// `MEWORK_E2E_SSH_HOST` (and `MEWORK_E2E_SSH_PORT`, `MEWORK_E2E_SSH_KEY`
    /// as needed) and run with `--ignored`. The agent is installed from this
    /// source tree's own build (`cargo build -p mework-remote-agent`).
    #[test]
    #[ignore]
    fn over_real_ssh_scripts_commands_and_terminals_survive_a_dropped_link() {
        use std::io::BufRead;
        let host = std::env::var("MEWORK_E2E_SSH_HOST").expect("MEWORK_E2E_SSH_HOST");
        let port = std::env::var("MEWORK_E2E_SSH_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
            .unwrap_or(0);
        let identity_file = std::env::var("MEWORK_E2E_SSH_KEY").unwrap_or_default();
        let app_data = tempfile::tempdir().unwrap();
        install(app_data.path(), Vec::new(), None);
        let runner = ShellRunner::Ssh {
            agent_shell: Default::default(),
            host,
            port,
            identity_file,
            env: [("MEWORK_E2E".to_owned(), "it's set".to_owned())].into_iter().collect(),
        };

        // A script, the way the file tools run one, goes through the agent.
        let started = Instant::now();
        let output = run_environment::run_remote_script(
            &runner,
            "echo hi; printf '%s\\n' \"$MEWORK_E2E\"; echo \"$0\"; cat",
            Some(b"from stdin\n"),
            Duration::from_secs(120),
            &CancelSignal::default(),
        )
        .unwrap();
        eprintln!("first script (connect + install) took {:?}", started.elapsed());
        assert_eq!(output.status, Some(0), "{}", output.stderr);
        assert_eq!(String::from_utf8_lossy(&output.stdout), "hi\nit's set\nbash\nfrom stdin\n");
        let Route::Agent(link, _) = route(&runner, Duration::from_secs(5)) else {
            panic!("the machine should be served by the agent")
        };
        let first_agent = link.wait_ready(Duration::from_secs(5)).unwrap();

        // Warm calls cost one round trip, not a login.
        let started = Instant::now();
        for _ in 0..10 {
            let output = run_environment::run_remote_script(
                &runner,
                "true",
                None,
                Duration::from_secs(30),
                &CancelSignal::default(),
            )
            .unwrap();
            assert_eq!(output.status, Some(0));
        }
        eprintln!("ten warm scripts took {:?}", started.elapsed());

        // A command keeps running, and keeps every line, through a drop.
        let mut child = spawn(
            &runner,
            vec![
                "bash".into(),
                "-c".into(),
                "for i in $(seq 1 30); do echo line$i; sleep 0.1; done".into(),
            ],
            Some("~"),
            StdinMode::Null,
            "e2e",
        )
        .unwrap()
        .unwrap();
        let mut stdout = std::io::BufReader::new(child.process.take_stdout().unwrap());
        let mut lines = Vec::new();
        let mut line = String::new();
        for _ in 0..5 {
            line.clear();
            stdout.read_line(&mut line).unwrap();
            lines.push(line.trim().to_owned());
        }
        // Killing the link's ssh is what a dropped network looks like to sshd, whether the
        // machine is this one or another: the session ends and its proxy with it.
        let killed = std::process::Command::new("pkill")
            .args(["-KILL", "-P", &std::process::id().to_string(), "-x", "ssh"])
            .status()
            .unwrap();
        assert!(killed.success(), "the link's ssh was running");
        loop {
            line.clear();
            if stdout.read_line(&mut line).unwrap() == 0 {
                break;
            }
            lines.push(line.trim().to_owned());
        }
        assert_eq!(lines, (1..=30).map(|i| format!("line{i}")).collect::<Vec<_>>());
        assert_eq!(child.process.wait().unwrap().code, Some(0));
        let after = link.wait_ready(Duration::from_secs(30)).unwrap();
        assert_eq!(after.pid, first_agent.pid, "the same daemon served both connections");

        // A terminal, through the same front the terminal panel uses.
        let spec = crate::remote_terminal::AgentTerminalSpec {
            runner: runner.clone(),
            argv: vec!["/bin/sh".into(), "-c".into(), "echo term-ok; exit 3".into()],
            windows_shell: None,
            cwd: "~".into(),
            env: vec![("TERM".into(), "xterm-256color".into())],
            machine_label: "e2e".into(),
        };
        let (master, mut terminal) = crate::remote_terminal::start(
            spec,
            portable_pty::CommandBuilder::new("false"),
            portable_pty::PtySize::default(),
        );
        let mut reader = master.try_clone_reader().unwrap();
        let status = terminal.wait().unwrap();
        assert_eq!(status.exit_code(), 3);
        let mut text = Vec::new();
        let _ = reader.read_to_end(&mut text);
        assert!(String::from_utf8_lossy(&text).contains("term-ok"));

        drop(child);
        shutdown();
    }

    /// Through this computer's own agent, the way the shell tool starts a
    /// sandboxed command: the agent is started on first use, starts the
    /// conversation's cell, and what runs in it is confined. Run with
    /// `--ignored` (it installs the process-wide hub) after
    /// `cargo build -p mework-remote-agent`, on a machine that can sandbox.
    #[test]
    #[ignore]
    fn a_sandboxed_command_runs_through_this_computers_agent() {
        let app_data = tempfile::tempdir().unwrap();
        install(app_data.path(), Vec::new(), None);
        let support = sandbox_support(&ShellRunner::default()).expect("the local agent starts");
        if !support.available {
            eprintln!("skipped: {}", support.detail);
            return;
        }
        let base = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(base.path()).unwrap();
        let workspace = base.join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let outside = base.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let root = workspace.to_string_lossy().into_owned();
        let mut assets = crate::model::ExecutionEnvironmentAssets::default();
        assets.sandboxes.insert(
            crate::run_environment::workspace_env_key(None, &root),
            crate::model::SandboxSettings {
                enabled: true,
                ..Default::default()
            },
        );
        let set = crate::workspace_set::WorkspaceSet::local_root(root).sandboxed(&assets, "conv-e2e");
        let sandbox = set.primary().unwrap().sandbox.clone().expect("sandboxed");
        let script = format!(
            "echo inside > made.txt && echo wrote; echo x > '{}/escape' 2>/dev/null || echo outside-refused; echo \"[$MEWORK_SANDBOX]\"",
            outside.display()
        );
        let mut child = spawn_in_sandbox(
            &ShellRunner::default(),
            &sandbox,
            SandboxedCommand {
                argv: vec!["/bin/sh".into(), "-c".into(), script],
                cwd: Some(workspace.to_string_lossy().into_owned()),
                env: Default::default(),
                env_remove: Vec::new(),
                label: "e2e".into(),
            },
        )
        .unwrap();
        let mut stdout = String::new();
        child.process.take_stdout().unwrap().read_to_string(&mut stdout).unwrap();
        assert_eq!(child.process.wait().unwrap().code, Some(0));
        assert!(stdout.contains("wrote") && stdout.contains("outside-refused") && stdout.contains("[1]"), "{stdout}");
        assert!(workspace.join("made.txt").is_file());
        assert!(!outside.join("escape").exists());
        drop(child);
        shutdown();
    }

    /// The sandbox on a real Windows machine over SSH: the agent arrives with `srt-win.exe`
    /// beside it, and a sandboxed command runs as the sandbox account, writing its workspace and
    /// nothing else. Set `MEWORK_E2E_SSH_WINDOWS_HOST` (and `MEWORK_E2E_SSH_PORT`,
    /// `MEWORK_E2E_SSH_KEY` as needed) and run with `--ignored`. The machine needs Git for
    /// Windows and the sandbox set up (`mework-remote.exe sandbox-setup` as an administrator);
    /// the agent is installed from `src-tauri/remote-agents/<windows triple>/`, or built there.
    #[test]
    #[ignore]
    fn over_real_ssh_a_windows_machine_sandboxes_commands() {
        let host = std::env::var("MEWORK_E2E_SSH_WINDOWS_HOST").expect("MEWORK_E2E_SSH_WINDOWS_HOST");
        let port = std::env::var("MEWORK_E2E_SSH_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
            .unwrap_or(0);
        let identity_file = std::env::var("MEWORK_E2E_SSH_KEY").unwrap_or_default();
        let app_data = tempfile::tempdir().unwrap();
        install(app_data.path(), Vec::new(), None);
        let runner = ShellRunner::Ssh {
            agent_shell: Default::default(),
            host,
            port,
            identity_file,
            env: Default::default(),
        };
        let script = |script: &str| {
            let output =
                run_environment::run_remote_script(&runner, script, None, Duration::from_secs(180), &CancelSignal::default())
                    .unwrap();
            assert_eq!(output.status, Some(0), "{}", output.stderr);
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        };
        // A workspace on the machine, made outside the sandbox.
        let base = script("d=$(mktemp -d) && mkdir \"$d/ws\" \"$d/outside\" && echo \"$d\"");
        let windows_base = script(&format!("cygpath -w '{base}'"));
        let Route::Agent(_, agent) = route(&runner, Duration::from_secs(5)) else {
            panic!("the machine should be served by the agent")
        };
        assert!(agent.sandbox.available, "no sandbox on the machine: {}", agent.sandbox.detail);
        assert!(agent.sandbox.detail.ends_with(SANDBOX_HELPER), "{}", agent.sandbox.detail);

        let workspace = format!("{windows_base}\\ws");
        let sandbox = protocol::SandboxSpec {
            cell: "conv-ssh-windows".into(),
            policy: protocol::SandboxPolicy {
                writable: vec![workspace.clone()],
                ..Default::default()
            },
        };
        let command = format!(
            "$ErrorActionPreference = 'Stop'
             $null = New-PSDrive -Name MeworkWorkspace -PSProvider FileSystem -Root '{workspace}' -Scope Global
             Set-Location -LiteralPath 'MeworkWorkspace:\\'
             Set-Content -Path made.txt -Value inside; 'wrote'
             try {{ Set-Content -Path '{windows_base}\\outside\\file' -Value x; 'OUTSIDE-WRITTEN' }} catch {{ 'outside-refused' }}
             if ($env:MEWORK_SANDBOX) {{ 'knows-it-is-sandboxed' }}
             'user=' + [Environment]::UserName"
        );
        let mut child = spawn_in_sandbox(
            &runner,
            &sandbox,
            SandboxedCommand {
                argv: remote_shell::powershell_argv(&command),
                cwd: Some(workspace),
                env: Default::default(),
                env_remove: Vec::new(),
                label: "e2e".into(),
            },
        )
        .unwrap();
        // Windows PowerShell writes errors in the console's code page.
        let mut stdout = Vec::new();
        child.process.take_stdout().unwrap().read_to_end(&mut stdout).unwrap();
        let mut stderr = Vec::new();
        child.process.take_stderr().unwrap().read_to_end(&mut stderr).unwrap();
        let (stdout, stderr) = (String::from_utf8_lossy(&stdout), String::from_utf8_lossy(&stderr));
        assert_eq!(child.process.wait().unwrap().code, Some(0), "{stdout}{stderr}");
        for expected in ["wrote", "outside-refused", "knows-it-is-sandboxed"] {
            assert!(stdout.contains(expected), "{expected} missing\nstdout: {stdout}\nstderr: {stderr}");
        }
        let login = script("whoami");
        let login = login.rsplit('\\').next().unwrap_or_default();
        let user = stdout.lines().find_map(|line| line.trim().strip_prefix("user=")).unwrap_or_default();
        assert!(!user.is_empty() && !user.eq_ignore_ascii_case(login), "ran as {user}, the login is {login}");
        drop(child);

        let left = script(&format!("cat '{base}/ws/made.txt'; ls '{base}/outside' | wc -l; rm -rf '{base}'"));
        assert_eq!(left.split_whitespace().collect::<Vec<_>>(), ["inside", "0"]);
        shutdown();
    }

    /// Against a real Windows machine over SSH, whatever its login shell:
    /// set `MEWORK_E2E_SSH_WINDOWS_HOST` (and `MEWORK_E2E_SSH_PORT`,
    /// `MEWORK_E2E_SSH_KEY` as needed) and run with `--ignored`. The machine
    /// needs Git for Windows, whose bash runs the host's scripts. The agent is
    /// installed from `src-tauri/remote-agents/<windows triple>/`.
    #[test]
    #[ignore]
    fn over_real_ssh_a_windows_machine_is_served_by_the_agent() {
        use std::io::BufRead;
        let host = std::env::var("MEWORK_E2E_SSH_WINDOWS_HOST").expect("MEWORK_E2E_SSH_WINDOWS_HOST");
        let port = std::env::var("MEWORK_E2E_SSH_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
            .unwrap_or(0);
        let identity_file = std::env::var("MEWORK_E2E_SSH_KEY").unwrap_or_default();
        let app_data = tempfile::tempdir().unwrap();
        install(app_data.path(), Vec::new(), None);
        let runner = ShellRunner::Ssh {
            agent_shell: Default::default(),
            host: host.clone(),
            port,
            identity_file: identity_file.clone(),
            env: [("MEWORK_E2E".to_owned(), "it's set".to_owned())].into_iter().collect(),
        };

        // A script the way the file tools run one: Git Bash, started by the
        // agent, whatever shell the account logs in with.
        let started = Instant::now();
        let output = run_environment::run_remote_script(
            &runner,
            "echo hi; printf '%s\\n' \"$MEWORK_E2E\"; echo \"$0\"; cat",
            Some(b"from stdin\n"),
            Duration::from_secs(180),
            &CancelSignal::default(),
        )
        .unwrap();
        eprintln!("first script (connect + install) took {:?}", started.elapsed());
        assert_eq!(output.status, Some(0), "{}", output.stderr);
        // Git's `bin\bash.exe` starts its `/usr/bin/bash` under that name.
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!((lines[0], lines[1], lines[3]), ("hi", "it's set", "from stdin"), "{text}");
        assert!(lines[2].ends_with("bash"), "{text}");
        let Route::Agent(link, agent) = route(&runner, Duration::from_secs(5)) else {
            panic!("the machine should be served by the agent")
        };
        assert_eq!(agent.os, "windows");
        eprintln!("agent {} (pid {}) on {}/{}, login shell {:?}", agent.version, agent.pid, agent.os, agent.arch, agent.shell);

        let started = Instant::now();
        for _ in 0..10 {
            let output = run_environment::run_remote_script(
                &runner,
                "true",
                None,
                Duration::from_secs(30),
                &CancelSignal::default(),
            )
            .unwrap();
            assert_eq!(output.status, Some(0));
        }
        eprintln!("ten warm scripts took {:?}", started.elapsed());

        // A workspace root in either spelling a Windows machine gets.
        for cwd in ["C:/Windows", "/c/Windows"] {
            let mut child = spawn(&runner, vec!["bash".into(), "-c".into(), "pwd".into()], Some(cwd), StdinMode::Null, "e2e")
                .unwrap()
                .unwrap();
            let mut text = String::new();
            child.process.take_stdout().unwrap().read_to_string(&mut text).unwrap();
            assert_eq!(text.trim().to_ascii_lowercase(), "/c/windows", "{cwd}");
            // The machine timed it, so the task row need not use this host's span.
            let exit = child.process.wait().unwrap();
            assert!(exit.runtime_ms.is_some(), "{exit:?}");
        }

        // A command keeps running, and keeps every line, through a dropped
        // connection. Killing the local ssh client is what a network drop
        // looks like to sshd: it ends the session and kills the session's
        // job, which the daemon must not be in.
        let mut child = spawn(
            &runner,
            vec![
                "bash".into(),
                "-c".into(),
                "for i in $(seq 1 30); do echo line$i; sleep 0.1; done".into(),
            ],
            Some("~"),
            StdinMode::Null,
            "e2e",
        )
        .unwrap()
        .unwrap();
        let mut stdout = std::io::BufReader::new(child.process.take_stdout().unwrap());
        let mut lines = Vec::new();
        let mut line = String::new();
        for _ in 0..5 {
            line.clear();
            stdout.read_line(&mut line).unwrap();
            lines.push(line.trim().to_owned());
        }
        let killed = std::process::Command::new("pkill")
            .args(["-KILL", "-P", &std::process::id().to_string(), "-x", "ssh"])
            .status()
            .unwrap();
        assert!(killed.success(), "the link's ssh was running");
        loop {
            line.clear();
            if stdout.read_line(&mut line).unwrap() == 0 {
                break;
            }
            lines.push(line.trim().to_owned());
        }
        assert_eq!(lines, (1..=30).map(|i| format!("line{i}")).collect::<Vec<_>>());
        assert_eq!(child.process.wait().unwrap().code, Some(0));
        let after = link.wait_ready(Duration::from_secs(60)).unwrap();
        assert_eq!(after.pid, agent.pid, "the same daemon served both connections");
        drop(child);

        // The directory picker reads the machine as Windows through the
        // agent, whatever the login shell.
        let assets = crate::model::ExecutionEnvironmentAssets {
            ssh_machines: vec![crate::model::SshMachineConfig {
                id: "win".into(),
                name: "win".into(),
                host: host.clone(),
                port,
                identity_file: identity_file.clone(),
                created_at: String::new(),
                updated_at: String::new(),
                agent_shell: None,
            }],
            ..Default::default()
        };
        let machine = crate::model::RunTarget::Ssh {
            machine_id: "win".into(),
        };
        let home = crate::remote_directory::list_directory(&assets, &machine, "~").unwrap();
        assert!(home.path.to_ascii_lowercase().starts_with("c:/users/"), "{home:?}");
        assert!(home.entries.iter().any(|entry| entry.name == "Desktop"), "{home:?}");
        let drives = crate::remote_directory::list_directory(&assets, &machine, "/").unwrap();
        assert!(drives.entries.iter().any(|entry| entry.path == "C:/"), "{drives:?}");

        // Terminals: PowerShell by default, bash when chosen and present.
        for (shell, input, marker, code) in [
            (None, "'term-' + 'ok'; exit 3\r", "term-ok", 3),
            (Some("bash"), "echo term-$((6*7)); exit 4\r", "term-42", 4),
        ] {
            let spec = crate::remote_terminal::AgentTerminalSpec {
                runner: runner.clone(),
                argv: vec!["/bin/sh".into()],
                windows_shell: shell.map(str::to_owned),
                cwd: "~".into(),
                env: vec![("TERM".into(), "xterm-256color".into())],
                machine_label: "e2e".into(),
            };
            let (master, mut terminal) = crate::remote_terminal::start(
                spec,
                portable_pty::CommandBuilder::new("false"),
                portable_pty::PtySize::default(),
            );
            let output = Arc::new(Mutex::new(Vec::<u8>::new()));
            let mut reader = master.try_clone_reader().unwrap();
            {
                let output = Arc::clone(&output);
                std::thread::spawn(move || {
                    let mut chunk = [0u8; 4096];
                    while let Ok(count) = reader.read(&mut chunk) {
                        if count == 0 {
                            break;
                        }
                        lock(&output).extend_from_slice(&chunk[..count]);
                    }
                });
            }
            let seen = |needle: &str| String::from_utf8_lossy(&lock(&output)).contains(needle);
            let deadline = Instant::now() + Duration::from_secs(30);
            // The pseudo console asks where the cursor is, as xterm.js answers.
            while !seen("\x1b[6n") && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
            let mut writer = master.take_writer().unwrap();
            writer.write_all(b"\x1b[1;1R").unwrap();
            writer.write_all(input.as_bytes()).unwrap();
            let status = terminal.wait().unwrap();
            std::thread::sleep(Duration::from_millis(300));
            let text = String::from_utf8_lossy(&lock(&output)).into_owned();
            assert!(text.contains(marker), "{shell:?}: {text}");
            assert_eq!(status.exit_code(), code, "{shell:?}: {text}");
        }

        shutdown();
    }

    /// A machine whose login shell changed since it was probed — here, one
    /// remembered as POSIX that answers as `cmd.exe` — is asked again in the
    /// other dialect, and the answer remembered. Same variables as
    /// [`over_real_ssh_a_windows_machine_is_served_by_the_agent`].
    #[test]
    #[ignore]
    fn over_real_ssh_a_changed_login_shell_is_followed() {
        let host = std::env::var("MEWORK_E2E_SSH_WINDOWS_HOST").expect("MEWORK_E2E_SSH_WINDOWS_HOST");
        let app_data = tempfile::tempdir().unwrap();
        install(app_data.path(), Vec::new(), None);
        let runner = ShellRunner::Ssh {
            agent_shell: Default::default(),
            host,
            port: 0,
            identity_file: String::new(),
            env: Default::default(),
        };
        remote_shell::remember_login_shell(&runner, LoginShell::Posix);
        let output = run_environment::run_remote_script(
            &runner,
            "echo ok",
            None,
            Duration::from_secs(120),
            &CancelSignal::default(),
        )
        .unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout), "ok\n", "{}", output.stderr);
        assert_eq!(remote_shell::login_shell(&runner), Ok((LoginShell::Cmd, true)));
        shutdown();
    }

    #[test]
    fn a_login_failure_is_retried_only_when_it_is_about_reaching_the_machine() {
        let unreachable = classify_login_failure(
            Some(255),
            "ssh: connect to host x port 22: Connection refused",
            "",
            Dialect::Posix,
        );
        assert!(matches!(unreachable.error, LaunchError::Unreachable(_)));
        assert!(!unreachable.wrong_dialect);
        let refused = classify_login_failure(
            Some(1),
            "mework-remote: Cannot create /home/x/.mework: Read-only file system",
            "",
            Dialect::Posix,
        );
        assert!(matches!(refused.error, LaunchError::Unavailable(_)));
        assert!(!refused.wrong_dialect);
        // The POSIX line reached cmd.exe: the machine is asked again in
        // PowerShell rather than given up on.
        let cmd = classify_login_failure(
            Some(9009),
            "'exec' is not recognized as an internal or external command",
            "",
            Dialect::Posix,
        );
        assert!(cmd.wrong_dialect);
        // And the PowerShell line reached a Unix shell.
        let unix = classify_login_failure(Some(127), "bash: powershell: command not found", "", Dialect::PowerShell);
        assert!(unix.wrong_dialect);
        let failed = classify_login_failure(
            Some(1),
            "mework-remote: Cannot create C:\\Users\\x\\.mework: Access is denied.",
            "",
            Dialect::PowerShell,
        );
        assert!(!failed.wrong_dialect);
    }

    #[test]
    fn a_windows_machine_gets_a_windows_build_whichever_shell_reports_it() {
        let catalog = catalog_with(&["x86_64-pc-windows-gnu", "x86_64-pc-windows-msvc", "x86_64-unknown-linux-musl"]);
        // PowerShell reports PROCESSOR_ARCHITECTURE, Git Bash `uname -m`.
        for arch in ["AMD64", "x86_64"] {
            assert_eq!(
                catalog.for_machine("Windows", arch).map(|build| build.triple.as_str()),
                Some("x86_64-pc-windows-msvc")
            );
        }
        // Windows on Arm without an Arm build runs the x64 one.
        assert_eq!(
            catalog.for_machine("Windows", "ARM64").map(|build| build.triple.as_str()),
            Some("x86_64-pc-windows-msvc")
        );
        assert_eq!(agent_binary("x86_64-pc-windows-msvc"), "mework-remote.exe");
        assert_eq!(agent_binary("aarch64-apple-darwin"), "mework-remote");
        let cases = catalog.windows_arch_cases();
        assert_eq!(cases.len(), 2, "{cases:?}");
        assert!(catalog.uname_cases().iter().any(|(pattern, _)| pattern.starts_with("Windows/")));
    }

    /// Only builds made from this host's own agent source are offered, whatever else is staged
    /// and however new it is: a build from other source, older or newer, never reaches a
    /// machine, so a machine running one is given this host's instead.
    #[test]
    fn only_builds_of_this_hosts_agent_source_are_offered() {
        let staged = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let write = |dir: &Path, triple: &str, contents: &[u8]| {
            let path = dir.join(triple).join(agent_binary(triple));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, contents).unwrap();
            path
        };
        let ours = |salt: &str| [b"\x7fELF".as_slice(), remote_agent::SOURCE_MARKER.as_bytes(), salt.as_bytes()].concat();
        let foreign = format!("mework-remote-source:{}", "0".repeat(64)).into_bytes();

        let linux = write(staged.path(), "x86_64-unknown-linux-musl", &ours("linux"));
        write(staged.path(), "aarch64-apple-darwin", &foreign);
        write(staged.path(), "x86_64-pc-windows-msvc", b"MZ an agent from before source identities");
        // A newer build from other source does not displace an older one from this source.
        let windows = write(other.path(), "x86_64-pc-windows-msvc", &ours("windows"));
        std::thread::sleep(Duration::from_millis(20));
        write(staged.path(), "x86_64-pc-windows-gnu", &foreign);

        let catalog = Catalog::discover(&[staged.path().to_path_buf(), other.path().to_path_buf()], &[]);
        let offered: Vec<(&str, &Path)> = catalog
            .builds
            .iter()
            .map(|build| (build.triple.as_str(), build.path.as_path()))
            .collect();
        assert_eq!(
            offered,
            vec![
                ("x86_64-unknown-linux-musl", linux.as_path()),
                ("x86_64-pc-windows-msvc", windows.as_path()),
            ]
        );
        assert!(catalog.for_machine("Darwin", "arm64").is_none());
        assert_eq!(
            catalog.for_machine("Windows", "AMD64").map(|build| build.triple.as_str()),
            Some("x86_64-pc-windows-msvc")
        );
    }

    /// An agent that looks its own name up on `PATH` predates its helpers; any other missing
    /// program is the command's own problem and keeps the agent's words.
    #[test]
    fn a_self_spawn_the_agent_cannot_find_says_the_agent_is_stale() {
        let not_found = |program: &str| {
            CallError::Failed(protocol::Failure::new(
                protocol::FailureKind::NotFound,
                format!("{program} was not found on this machine's PATH"),
            ))
        };
        let stale = spawn_error("windows", protocol::SELF_PROGRAM, not_found(protocol::SELF_PROGRAM));
        assert!(stale.contains("older build"), "{stale}");
        assert!(stale.contains("restart Mework"), "{stale}");
        assert_eq!(
            spawn_error("windows", "node", not_found("node")),
            "node was not found on this machine's PATH"
        );
    }

    /// The PowerShell scripts cannot run here; what can be checked is that
    /// they name only the builds this host has, carry the nonce, and fit on
    /// the command line `cmd.exe` hands them to.
    #[test]
    fn the_powershell_bootstrap_and_upload_fit_a_windows_command_line() {
        let catalog = catalog_with(&["x86_64-pc-windows-msvc", "x86_64-unknown-linux-musl"]);
        let script = powershell_bootstrap_script("n0nce", &catalog);
        assert!(script.contains("'AMD64' { '0.1.0-000000000000' }"), "{script}");
        assert!(script.contains("'ARM64' { '0.1.0-000000000000' }"), "{script}");
        assert!(script.contains("proxy --sync n0nce"), "{script}");
        assert!(script.contains(&format!("{} n0nce Windows $A", protocol::MISSING_MARKER)), "{script}");
        assert!(script.contains("Join-Path $env:USERPROFILE '.mework\\remote'"), "{script}");
        let upload = powershell_upload_script("0.1.0-abc", UploadSizes { agent: 2_107_904, helper: None });
        assert!(upload.contains("$D = Join-Path $B '0.1.0-abc'"), "{upload}");
        assert!(upload.contains("GetStdHandle"), "{upload}");
        assert!(!upload.contains("OpenStandardInput"), "{upload}");
        assert!(upload.contains("Receive $U 2107904"), "{upload}");
        assert!(!upload.contains("Receive $V"), "{upload}");
        let with_helper = powershell_upload_script(
            "0.1.0-abc",
            UploadSizes {
                agent: 2_107_904,
                helper: Some(3_240_960),
            },
        );
        assert!(with_helper.contains("Receive $V 3240960"), "{with_helper}");
        // The helper is in place before the agent that says the build is complete.
        assert!(with_helper.find("Destination $H").unwrap() < with_helper.find("Destination $F").unwrap());
        for line in [
            remote_shell::powershell_line(&script),
            remote_shell::powershell_line(&upload),
            remote_shell::powershell_line(&with_helper),
        ] {
            assert!(line.len() < 8000, "{} characters", line.len());
        }
    }

    /// A Windows machine logging in through Git Bash runs the POSIX line; the
    /// bootstrap has to call it Windows, look where the Windows agent keeps
    /// its builds, and run the `.exe`. Here `uname` answers as Git Bash does.
    #[cfg(unix)]
    #[test]
    fn the_posix_bootstrap_and_upload_serve_git_bash_on_windows() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = tempfile::tempdir().unwrap();
        let tools = scratch.path().join("tools");
        std::fs::create_dir_all(&tools).unwrap();
        let uname = tools.join("uname");
        std::fs::write(
            &uname,
            "#!/bin/sh\ncase \"$1\" in -s) echo MINGW64_NT-10.0-26100 ;; -m) echo x86_64 ;; esac\n",
        )
        .unwrap();
        std::fs::set_permissions(&uname, std::fs::Permissions::from_mode(0o755)).unwrap();
        let profile = scratch.path().join("profile");
        std::fs::create_dir_all(&profile).unwrap();
        let path = format!("{}:{}", tools.display(), std::env::var("PATH").unwrap());
        let run = |script: &str, stdin: &[u8]| {
            let mut child = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(remote_shell::posix_line(script))
                .env("PATH", &path)
                .env("USERPROFILE", &profile)
                .env_remove("MEWORK_REMOTE_ROOT")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(stdin).unwrap();
            child.wait_with_output().unwrap()
        };
        let catalog = catalog_with(&["x86_64-pc-windows-msvc"]);
        let tag = catalog.builds[0].tag.clone();

        let output = run(&bootstrap_script("n0nce", &catalog), b"");
        let text = String::from_utf8_lossy(&output.stdout);
        let Preamble::Missing { os, arch } = protocol::read_preamble(&mut text.as_bytes(), "n0nce").unwrap() else {
            panic!("expected a missing report: {text}")
        };
        assert_eq!((os.as_str(), arch.as_str()), ("Windows", "x86_64"));
        assert!(catalog.for_machine(&os, &arch).is_some());

        // The upload lands where the Windows agent looks, as an `.exe`.
        let fake = "#!/bin/sh\nif [ \"$1\" = version ]; then printf '{\"build\":\"abc\"}\\n'; else echo \"ran $*\"; fi\n";
        // With the sandbox helper behind it on the same stream, which lands beside it.
        let helper = b"srt-win helper bytes\n\0\xff".repeat(1000);
        let stream = [fake.as_bytes(), &helper].concat();
        let uploaded = run(&upload_script(&tag, sizes(fake.as_bytes(), Some(&helper))), &stream);
        assert!(uploaded.status.success(), "{}", String::from_utf8_lossy(&uploaded.stderr));
        let installed = profile.join(".mework/remote/bin").join(&tag).join("mework-remote.exe");
        assert_eq!(std::fs::read_to_string(&installed).unwrap(), fake);
        assert_eq!(std::fs::read(installed.with_file_name(SANDBOX_HELPER)).unwrap(), helper);
        let leftovers: Vec<_> = std::fs::read_dir(installed.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".upload"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");

        // A helper cut short installs nothing.
        let short = run(
            &upload_script("0.1.0-short", sizes(fake.as_bytes(), Some(&helper))),
            &stream[..stream.len() - 10],
        );
        assert!(!short.status.success());
        assert!(!profile.join(".mework/remote/bin/0.1.0-short/mework-remote.exe").exists());

        // And the next login runs it.
        let output = run(&bootstrap_script("n0nce", &catalog), b"");
        assert_eq!(String::from_utf8_lossy(&output.stdout), "ran proxy --sync n0nce\n");
    }

    #[cfg(unix)]
    #[test]
    fn an_agent_exit_reads_like_a_local_one() {
        let exited = protocol::ExitInfo {
            code: Some(3),
            signal: None,
            reason: protocol::ExitReason::Exited,
            ends: Default::default(),
            runtime_ms: None,
        };
        assert_eq!(exit_status(&exited).code(), Some(3));
        let killed = protocol::ExitInfo {
            code: None,
            signal: Some(9),
            reason: protocol::ExitReason::Signalled,
            ends: Default::default(),
            runtime_ms: None,
        };
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(exit_status(&killed).signal(), Some(9));
        assert!(!exit_status(&killed).success());
    }
}
