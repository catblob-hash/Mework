//! Which shell backends each machine has, found by probing it.
//!
//! A machine's shells are machine state, like its WSL distributions: they
//! change when someone installs or removes one, so they are asked of the
//! machine rather than configured. What the renderer and the model are told is
//! the last answer, kept here and in `machine-shells.json` beside the document
//! so a conversation opened before the machine has been reached this session
//! still lists the right tools.
//!
//! A probe only looks for the backends the combination table registers on the
//! machine's OS ([`crate::shell_backend::backends_for`]); the rest are never
//! asked about. When a machine is probed:
//!
//! - **This machine**: at startup, and again from its settings.
//! - **An SSH machine**: the first time this process connects to it (the link
//!   hub calls back on the first connection, or on falling back to per-command
//!   SSH), and from its settings — including right after it is added, which is
//!   when the renderer records its agent shell.
//! - **A WSL distribution**: the first time this process runs something for a
//!   workspace on it, and from its settings.
//!
//! Entries are keyed by [`crate::run_environment::env_key`]: `local`,
//! `wsl:<distro>`, `ssh:<machine id>`. An SSH machine keeps its id when its
//! address is edited, so its answer also records the [`Endpoint`] it was
//! taken from, and is used only while the machine still has that endpoint.
//! Otherwise a probe of the new address that fails (the machine is offline,
//! or the app restarts) would leave the old machine's OS and shell paths in
//! force for the new one.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::cancel::CancelSignal;
use crate::model::{ExecutionEnvironmentAssets, RunTarget, SshMachineConfig};
use crate::run_environment::{self, ShellRunner};
use crate::shell_backend::{backends_for, probe_names, MachineOs, ShellBackend};

/// One backend a probe found, and where.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectedShell {
    pub backend: ShellBackend,
    /// The program as the machine names it: an absolute path wherever the
    /// probe could resolve one.
    pub path: String,
}

/// What a probe learned about one machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineShells {
    pub os: MachineOs,
    /// Registered backends the machine has, in the OS's table order.
    pub shells: Vec<DetectedShell>,
    /// RFC 3339.
    pub probed_at: String,
}

impl MachineShells {
    pub fn backends(&self) -> Vec<ShellBackend> {
        self.shells.iter().map(|shell| shell.backend).collect()
    }

    pub fn get(&self, backend: ShellBackend) -> Option<&DetectedShell> {
        self.shells.iter().find(|shell| shell.backend == backend)
    }

    fn from_found(os: MachineOs, found: impl Fn(&str) -> Option<String>) -> Self {
        let shells = backends_for(os)
            .iter()
            .filter_map(|backend| {
                probe_names(os, *backend).iter().find_map(|name| {
                    found(name).map(|path| DetectedShell {
                        backend: *backend,
                        path,
                    })
                })
            })
            .collect();
        Self {
            os,
            shells,
            probed_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}

/// The account and address an SSH machine's answer came from: what the probe
/// logged in with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    pub identity_file: String,
}

impl Endpoint {
    /// The endpoint `runner` logs in to. `None` for this machine and a WSL
    /// distribution, which their keys already name.
    pub fn of(runner: &ShellRunner) -> Option<Self> {
        match runner {
            ShellRunner::Ssh {
                host,
                port,
                identity_file,
                ..
            } => Some(Self {
                host: host.clone(),
                port: *port,
                identity_file: identity_file.clone(),
            }),
            ShellRunner::Local { .. } | ShellRunner::Wsl { .. } => None,
        }
    }

    /// The endpoint the catalog has `machine` at.
    pub fn of_machine(machine: &SshMachineConfig) -> Self {
        Self {
            host: machine.host.clone(),
            port: machine.port,
            identity_file: machine.identity_file.clone(),
        }
    }
}

/// One kept answer and where it came from, as `machine-shells.json` holds it.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Recorded {
    #[serde(flatten)]
    shells: MachineShells,
    /// `None` for this machine and WSL. An SSH answer written before answers
    /// recorded their endpoint has none either, and describes no endpoint
    /// anyone can vouch for, so it is never used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    endpoint: Option<Endpoint>,
}

/// Told about every recorded probe, with the machine's key and the endpoint
/// the answer came from.
pub type Observer = Box<dyn Fn(&str, Option<&Endpoint>, &MachineShells) + Send + Sync>;

#[derive(Default)]
struct Store {
    /// `machine-shells.json`, once [`install`] has named it.
    file: Option<PathBuf>,
    entries: BTreeMap<String, Recorded>,
    /// Machines probed since this process started. A persisted answer from an
    /// earlier session is used, and refreshed once.
    fresh: HashSet<String>,
    /// Machines with a probe running, so a burst of triggers probes once.
    running: HashSet<String>,
}

static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
static OBSERVER: OnceLock<Observer> = OnceLock::new();

fn store() -> std::sync::MutexGuard<'static, Store> {
    STORE
        .get_or_init(|| Mutex::new(Store::default()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Loads the persisted answers and names where new ones go. Called once at
/// startup; until then — in tests — answers live in memory only.
pub fn install(app_data: &Path, observer: Option<Observer>) {
    let file = app_data.join("machine-shells.json");
    let entries = std::fs::read(&file)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<BTreeMap<String, Recorded>>(&bytes).ok())
        .unwrap_or_default();
    {
        let mut store = store();
        store.file = Some(file);
        for (key, shells) in entries {
            store.entries.entry(key).or_insert(shells);
        }
    }
    if let Some(observer) = observer {
        let _ = OBSERVER.set(observer);
    }
}

/// The last answer for a machine, from this session or an earlier one, when it
/// was taken at `endpoint`: the one the machine has now ([`Endpoint::of`] its
/// runner).
pub fn get(key: &str, endpoint: Option<&Endpoint>) -> Option<MachineShells> {
    store()
        .entries
        .get(key)
        .filter(|recorded| recorded.endpoint.as_ref() == endpoint)
        .map(|recorded| recorded.shells.clone())
}

/// Every machine's last answer that still [`describes`] it.
pub fn all(assets: &ExecutionEnvironmentAssets) -> BTreeMap<String, MachineShells> {
    store()
        .entries
        .iter()
        .filter(|(key, recorded)| describes(assets, key, recorded.endpoint.as_ref()))
        .map(|(key, recorded)| (key.clone(), recorded.shells.clone()))
        .collect()
}

/// Whether an answer taken at `endpoint` describes the machine `key` names in
/// `assets`: this machine's and a WSL distribution's always, an SSH machine's
/// while the catalog has the machine at that endpoint.
pub fn describes(assets: &ExecutionEnvironmentAssets, key: &str, endpoint: Option<&Endpoint>) -> bool {
    match key.strip_prefix("ssh:") {
        Some(id) => endpoint.is_some_and(|endpoint| {
            assets
                .ssh_machines
                .iter()
                .any(|machine| machine.id == id && Endpoint::of_machine(machine) == *endpoint)
        }),
        None => endpoint.is_none(),
    }
}

/// Whether the machine has been probed since this process started.
pub fn fresh(key: &str) -> bool {
    store().fresh.contains(key)
}

/// Keeps an answer taken at `endpoint`, writes the file, and tells the
/// observer.
fn record(key: &str, endpoint: Option<Endpoint>, shells: MachineShells) {
    let (file, snapshot) = {
        let mut store = store();
        store.entries.insert(
            key.to_owned(),
            Recorded {
                shells: shells.clone(),
                endpoint: endpoint.clone(),
            },
        );
        store.fresh.insert(key.to_owned());
        (store.file.clone(), store.entries.clone())
    };
    if let Some(file) = file {
        if let Ok(bytes) = serde_json::to_vec_pretty(&snapshot) {
            let temporary = file.with_extension("json.tmp");
            if std::fs::write(&temporary, bytes).is_ok() {
                let _ = std::fs::rename(&temporary, &file);
            }
        }
    }
    if let Some(observer) = OBSERVER.get() {
        observer(key, endpoint.as_ref(), &shells);
    }
}

/// This machine's shells, probed now if nothing has asked yet. Looking up a
/// few names on `PATH` is cheap enough to do inline.
pub fn local() -> MachineShells {
    let key = run_environment::env_key(None);
    if let Some(shells) = get(&key, None) {
        if fresh(&key) {
            return shells;
        }
    }
    let shells = probe_local();
    record(&key, None, shells.clone());
    shells
}

/// What is assumed about a machine that has never been probed: bash, which is
/// what every remote leg ran before machines had backends, on an OS that is
/// known only for WSL.
pub fn assumed(target: Option<&RunTarget>) -> (Option<MachineOs>, Vec<DetectedShell>) {
    let bash = vec![DetectedShell {
        backend: ShellBackend::Bash,
        path: "bash".into(),
    }];
    match target {
        None => {
            let local = local();
            (Some(local.os), local.shells)
        }
        Some(RunTarget::Wsl { .. }) => (Some(MachineOs::Wsl), bash),
        Some(RunTarget::Ssh { .. }) => (None, bash),
    }
}

/// A machine's OS and shells as far as they are known: the last probe of it at
/// the endpoint `runner` (its own, resolved from `target`) logs in to, or
/// [`assumed`] when there has been none.
pub fn known(target: Option<&RunTarget>, runner: &ShellRunner) -> (Option<MachineOs>, Vec<DetectedShell>) {
    if target.is_none() {
        return assumed(None);
    }
    match get(&run_environment::env_key(target), Endpoint::of(runner).as_ref()) {
        Some(shells) => (Some(shells.os), shells.shells),
        None => assumed(target),
    }
}

// ---------------------------------------------------------------------------
// Probing
// ---------------------------------------------------------------------------

/// How long a remote probe may take, the connection included: the first
/// connection to an SSH machine installs its agent.
const REMOTE_PROBE_TIMEOUT: Duration = Duration::from_secs(90);

/// Probes a machine and records the answer. `runner` must be the machine's
/// own, resolved from `target`.
pub fn probe(target: Option<&RunTarget>, runner: &ShellRunner) -> Result<MachineShells, String> {
    let key = run_environment::env_key(target);
    let shells = match runner {
        ShellRunner::Local { .. } => probe_local(),
        _ => probe_remote(runner, &CancelSignal::default())?,
    };
    record(&key, Endpoint::of(runner), shells.clone());
    Ok(shells)
}

/// Probes the machine on a thread of its own unless it has been probed this
/// session or a probe is already running. Errors are logged: a trigger in the
/// background has nobody to tell, and the last answer stays in place.
pub fn refresh_in_background(target: Option<RunTarget>, runner: ShellRunner) {
    let key = run_environment::env_key(target.as_ref());
    {
        let mut store = store();
        if store.fresh.contains(&key) || !store.running.insert(key.clone()) {
            return;
        }
    }
    let running = key.clone();
    let spawned = std::thread::Builder::new()
        .name("machine-shells-probe".into())
        .spawn(move || {
            if let Err(error) = probe(target.as_ref(), &runner) {
                eprintln!("[machine-shells] {running}: probe failed: {error}");
            }
            store().running.remove(&running);
        });
    if spawned.is_err() {
        store().running.remove(&key);
    }
}

/// This machine's registered backends, from `PATH` and the resolvers the
/// shell tool itself uses — so what is listed is what would run.
pub fn probe_local() -> MachineShells {
    let os = MachineOs::host();
    MachineShells::from_found(os, |name| local_program(os, name))
}

fn local_program(os: MachineOs, name: &str) -> Option<String> {
    match (os, name) {
        (MachineOs::Windows, "pwsh" | "powershell") => {
            // One answer for both names: the tool's own resolver already
            // prefers PowerShell 7 and falls back to 5.1.
            if name == "powershell" {
                return None;
            }
            run_environment::local_powershell_candidates().into_iter().next()
        }
        (MachineOs::Windows, "bash") => run_environment::local_bash_candidates().into_iter().next(),
        (MachineOs::Windows, _) => None,
        _ => run_environment::local_program_path(name),
    }
}

/// The script a POSIX machine is probed with: its `uname -s` on the first line,
/// then one `name<TAB>path` line per program found.
fn posix_probe_script(names: &[&str]) -> String {
    let names = names
        .iter()
        .map(|name| run_environment::sh_single_quote(name))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "uname -s 2>/dev/null || echo unknown\n\
         for s in {names}; do p=$(command -v \"$s\" 2>/dev/null) && printf '%s\\t%s\\n' \"$s\" \"$p\"; done\n\
         exit 0\n"
    )
}

/// A Windows machine reached through a `cmd.exe` or PowerShell login shell
/// with no agent: PowerShell finds what is installed. Bash is not asked for —
/// a per-command SSH call through such a login shell cannot start it.
const WINDOWS_PROBE_PS: &str = "Write-Output 'windows'\n\
    foreach ($n in 'pwsh','powershell') { $c = Get-Command $n -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1; if ($c) { Write-Output ($n + \"`t\" + $c.Source) } }\n";

/// Reads a probe's `name<TAB>path` lines.
fn parse_found(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| line.trim_end_matches('\r').split_once('\t'))
        .filter(|(_, path)| !path.trim().is_empty())
        .map(|(name, path)| (name.trim().to_owned(), path.trim().to_owned()))
        .collect()
}

/// Every name the table probes for on any POSIX OS, and on Windows.
fn names_for(os: MachineOs) -> Vec<&'static str> {
    let mut names: Vec<&'static str> = Vec::new();
    for backend in backends_for(os) {
        for name in probe_names(os, *backend) {
            if !names.contains(name) {
                names.push(name);
            }
        }
    }
    names
}

fn probe_remote(runner: &ShellRunner, cancel: &CancelSignal) -> Result<MachineShells, String> {
    match runner {
        ShellRunner::Local { .. } => Ok(probe_local()),
        ShellRunner::Wsl { .. } => {
            let output = run_environment::run_remote_sh_script(
                runner,
                &posix_probe_script(&names_for(MachineOs::Wsl)),
                REMOTE_PROBE_TIMEOUT,
                cancel,
            )?;
            if output.status != Some(0) {
                return Err(failure_text(&output));
            }
            let found = parse_found(&String::from_utf8_lossy(&output.stdout));
            Ok(MachineShells::from_found(MachineOs::Wsl, |name| found.get(name).cloned()))
        }
        ShellRunner::Ssh { .. } => match crate::remote_link::route(runner, REMOTE_PROBE_TIMEOUT) {
            crate::remote_link::Route::Agent(link, agent) => {
                let os = MachineOs::from_agent_os(&agent.os);
                let names: Vec<String> = names_for(os).into_iter().map(str::to_owned).collect();
                let reply = link
                    .call(
                        remote_agent::protocol::Op::Which { names },
                        b"",
                        Duration::from_secs(30),
                    )
                    .map_err(|error| format!("The machine did not answer the shell probe: {error:?}"))?;
                let remote_agent::protocol::Reply::Which { found } = reply else {
                    return Err("The machine answered the shell probe with something else".into());
                };
                Ok(MachineShells::from_found(os, |name| {
                    found.get(name).cloned().flatten()
                }))
            }
            crate::remote_link::Route::Legacy => probe_ssh_without_agent(runner, cancel),
            crate::remote_link::Route::Unreachable(error) => Err(error),
        },
    }
}

/// An SSH machine the agent does not serve, probed through its login shell.
fn probe_ssh_without_agent(runner: &ShellRunner, cancel: &CancelSignal) -> Result<MachineShells, String> {
    let (login, _) = crate::remote_shell::login_shell(runner)?;
    if login.is_windows() {
        let output = run_environment::run_ssh_line(
            runner,
            &crate::remote_shell::powershell_line(WINDOWS_PROBE_PS),
            REMOTE_PROBE_TIMEOUT,
            cancel,
        )?;
        if output.status != Some(0) {
            return Err(failure_text(&output));
        }
        let found = parse_found(&String::from_utf8_lossy(&output.stdout));
        return Ok(MachineShells::from_found(MachineOs::Windows, |name| {
            found.get(name).cloned()
        }));
    }
    let mut names = names_for(MachineOs::Linux);
    names.extend(["pwsh.exe", "powershell.exe"]);
    let output = run_environment::run_remote_sh_script(
        runner,
        &posix_probe_script(&names),
        REMOTE_PROBE_TIMEOUT,
        cancel,
    )?;
    if output.status != Some(0) {
        return Err(failure_text(&output));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let os = MachineOs::from_uname(text.lines().next().unwrap_or_default());
    let found = parse_found(&text);
    Ok(MachineShells::from_found(os, |name| {
        found
            .get(name)
            .or_else(|| found.get(&format!("{name}.exe")))
            .cloned()
    }))
}

fn failure_text(output: &run_environment::RemoteCommandOutput) -> String {
    run_environment::legible_remote_reply(&output.stderr)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("The shell probe failed (exit code {:?})", output.status))
}

#[cfg(test)]
pub(crate) fn seed_for_test(key: &str, endpoint: Option<Endpoint>, shells: MachineShells) {
    let mut store = store();
    store.entries.insert(key.to_owned(), Recorded { shells, endpoint });
    store.fresh.insert(key.to_owned());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_probe_keeps_only_registered_backends_in_table_order() {
        let found: BTreeMap<&str, &str> = [
            ("sh", "/bin/sh"),
            ("bash", "/usr/bin/bash"),
            ("fish", "/usr/bin/fish"),
            ("pwsh", "/usr/bin/pwsh"),
        ]
        .into_iter()
        .collect();
        let shells = MachineShells::from_found(MachineOs::Linux, |name| {
            found.get(name).map(|path| (*path).to_owned())
        });
        assert_eq!(shells.backends(), vec![ShellBackend::Bash, ShellBackend::Sh]);
        assert_eq!(shells.get(ShellBackend::Sh).unwrap().path, "/bin/sh");
    }

    #[test]
    fn powershell_on_windows_prefers_pwsh() {
        let found: BTreeMap<&str, &str> = [
            ("powershell", r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"),
            ("pwsh", r"C:\Program Files\PowerShell\7\pwsh.exe"),
        ]
        .into_iter()
        .collect();
        let shells = MachineShells::from_found(MachineOs::Windows, |name| {
            found.get(name).map(|path| (*path).to_owned())
        });
        assert_eq!(shells.backends(), vec![ShellBackend::PowerShell]);
        assert!(shells.get(ShellBackend::PowerShell).unwrap().path.ends_with("pwsh.exe"));
    }

    #[test]
    fn probe_output_is_read_line_by_line() {
        let found = parse_found("Linux\nbash\t/usr/bin/bash\r\nzsh\t\nsh\t/bin/sh\n");
        assert_eq!(found.get("bash").unwrap(), "/usr/bin/bash");
        assert!(!found.contains_key("zsh"));
        assert_eq!(found.get("sh").unwrap(), "/bin/sh");
        let script = posix_probe_script(&["bash", "zsh"]);
        assert!(script.starts_with("uname -s"));
        assert!(script.contains("for s in 'bash' 'zsh'"));
    }

    fn bash_on_linux() -> MachineShells {
        MachineShells {
            os: MachineOs::Linux,
            shells: vec![DetectedShell {
                backend: ShellBackend::Bash,
                path: "/usr/bin/bash".into(),
            }],
            probed_at: String::new(),
        }
    }

    /// A machine edited to another address keeps its id, and the old
    /// address's answer must not describe the new one: not to the host, which
    /// would run the old machine's shell paths there, and not to the renderer.
    /// That holds until the new address answers, however long its probe fails.
    #[test]
    fn an_ssh_answer_describes_only_the_endpoint_it_came_from() {
        let id = "endpoint-moved";
        let key = format!("ssh:{id}");
        let target = RunTarget::Ssh {
            machine_id: id.into(),
        };
        let catalog = |host: &str| ExecutionEnvironmentAssets {
            ssh_machines: vec![SshMachineConfig {
                id: id.into(),
                name: "office".into(),
                host: host.into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let before = catalog("dev@linux-box");
        seed_for_test(
            &key,
            Some(Endpoint::of_machine(&before.ssh_machines[0])),
            bash_on_linux(),
        );
        let runner = run_environment::resolve_shell_runner(&before, Some(&target), None).unwrap();
        assert_eq!(known(Some(&target), &runner).0, Some(MachineOs::Linux));
        assert_eq!(runner.agent_shell().unwrap().program, "/usr/bin/bash");
        assert_eq!(all(&before).get(&key), Some(&bash_on_linux()));

        let after = catalog("dev@windows-box");
        let runner = run_environment::resolve_shell_runner(&after, Some(&target), None).unwrap();
        assert_eq!(known(Some(&target), &runner), assumed(Some(&target)));
        assert_eq!(runner.agent_shell().unwrap(), &crate::shell_backend::AgentShell::default());
        assert!(!all(&after).contains_key(&key));
        // A deleted machine's answer is not listed either.
        assert!(!all(&ExecutionEnvironmentAssets::default()).contains_key(&key));
    }

    /// `machine-shells.json` from before answers recorded their endpoint
    /// still loads; its SSH answers are simply never used, and the next probe
    /// replaces them.
    #[test]
    fn an_ssh_answer_kept_without_an_endpoint_is_not_used() {
        let file: BTreeMap<String, Recorded> = serde_json::from_str(
            r#"{
                "local": {"os": "macos", "shells": [{"backend": "zsh", "path": "/bin/zsh"}], "probedAt": "2026-09-28T13:22:54Z"},
                "ssh:endpoint-legacy": {"os": "linux", "shells": [{"backend": "bash", "path": "/usr/bin/bash"}], "probedAt": "2026-09-28T13:22:54Z"}
            }"#,
        )
        .unwrap();
        let legacy = file["ssh:endpoint-legacy"].clone();
        assert!(file["local"].endpoint.is_none() && legacy.endpoint.is_none());
        seed_for_test("ssh:endpoint-legacy", legacy.endpoint, legacy.shells);
        let endpoint = Endpoint {
            host: "dev@box".into(),
            port: 0,
            identity_file: String::new(),
        };
        assert_eq!(get("ssh:endpoint-legacy", Some(&endpoint)), None);

        // What is written now carries the endpoint beside the answer's own fields.
        let json = serde_json::to_value(Recorded {
            shells: bash_on_linux(),
            endpoint: Some(endpoint),
        })
        .unwrap();
        assert_eq!(json["os"], "linux");
        assert_eq!(json["endpoint"]["host"], "dev@box");
        assert_eq!(json["endpoint"]["identityFile"], "");
        let local = serde_json::to_value(file["local"].clone()).unwrap();
        assert!(local.get("endpoint").is_none(), "{local}");
    }

    /// The probe script is itself POSIX, and every shell the table registers
    /// on a Unix runs it.
    #[cfg(unix)]
    #[test]
    fn the_posix_probe_runs_in_sh_and_finds_this_machines_shells() {
        let output = std::process::Command::new("/bin/sh")
            .args(["-c", &posix_probe_script(&names_for(MachineOs::Linux))])
            .output()
            .unwrap();
        assert!(output.status.success());
        let text = String::from_utf8_lossy(&output.stdout);
        let found = parse_found(&text);
        assert!(found.contains_key("sh"), "{text}");
    }

    /// A real Windows machine over SSH, through the agent: the probe finds its
    /// OS and PowerShell; the `powershell` tool's own invocation runs there
    /// with UTF-8 output and its exit status intact; and a language server
    /// started by the PowerShell launch script owns the agent's standard
    /// streams. Set `MEWORK_E2E_SSH_WINDOWS_HOST` and run with `--ignored`.
    #[test]
    #[ignore]
    fn over_real_ssh_a_windows_machine_is_probed_and_runs_powershell() {
        use std::io::Read as _;
        let host = std::env::var("MEWORK_E2E_SSH_WINDOWS_HOST").expect("MEWORK_E2E_SSH_WINDOWS_HOST");
        let app_data = tempfile::tempdir().unwrap();
        crate::remote_link::install(app_data.path(), Vec::new(), None);
        let runner = ShellRunner::Ssh {
            host,
            port: 0,
            identity_file: String::new(),
            env: [("MEWORK_E2E_VALUE".to_owned(), "值".to_owned())].into_iter().collect(),
            agent_shell: Default::default(),
        };
        let shells = probe_remote(&runner, &CancelSignal::default()).unwrap();
        eprintln!("[e2e] probed: {shells:?}");
        assert_eq!(shells.os, MachineOs::Windows);
        let powershell = shells.get(ShellBackend::PowerShell).expect("PowerShell is on every Windows");
        assert!(powershell.path.to_ascii_lowercase().ends_with(".exe"), "{}", powershell.path);

        // The tool: Chinese output survives, the variable table arrives, and the
        // exit status is the command's.
        let argv = crate::shell_backend::remote_command_argv(
            ShellBackend::PowerShell,
            &powershell.path,
            "Write-Output \"中文 $env:MEWORK_E2E_VALUE\"; cmd /c exit 3",
        );
        let child = crate::remote_link::spawn(
            &runner,
            argv,
            Some("~"),
            remote_agent::protocol::StdinMode::Null,
            "powershell",
        )
        .expect("the agent serves the machine")
        .unwrap();
        let mut process = child.process;
        let mut stdout = Vec::new();
        process.take_stdout().unwrap().read_to_end(&mut stdout).unwrap();
        let exit = loop {
            if let Some(exit) = process.wait_timeout(Duration::from_millis(100)).unwrap() {
                break exit;
            }
        };
        let text = String::from_utf8(stdout).expect("UTF-8 output");
        assert!(text.contains("中文 值"), "{text:?}");
        assert_eq!(exit.code, Some(3), "{text:?}");

        // The launch script: the server reads the agent's stdin and writes its
        // stdout directly. `findstr` echoes the matching lines of its input.
        let script = crate::remote_powershell::lsp_launch(
            "~",
            "findstr",
            &["mework".to_owned()],
            &[("MEWORK_LSP".to_owned(), "1".to_owned())],
        );
        let argv = crate::shell_backend::script_argv(ShellBackend::PowerShell, &powershell.path, &script);
        let child = crate::remote_link::spawn(
            &runner,
            argv,
            None,
            remote_agent::protocol::StdinMode::Pipe,
            "lsp",
        )
        .expect("the agent serves the machine")
        .unwrap();
        let mut process = child.process;
        {
            use std::io::Write as _;
            let mut stdin = process.stdin();
            stdin.write_all(b"mework one\r\nother\r\nmework two\r\n").unwrap();
            stdin.flush().unwrap();
        }
        process.close_stdin();
        let mut stdout = Vec::new();
        process.take_stdout().unwrap().read_to_end(&mut stdout).unwrap();
        assert_eq!(String::from_utf8_lossy(&stdout), "mework one\r\nmework two\r\n");

        // A server that is not installed is reported by name, as on POSIX.
        let script = crate::remote_powershell::lsp_launch("~", "no-such-language-server", &[], &[]);
        let output = crate::remote_link::run_script(
            &runner,
            crate::shell_backend::script_argv(ShellBackend::PowerShell, &powershell.path, &script),
            None,
            Duration::from_secs(60),
            &CancelSignal::default(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(output.status, Some(127), "{}", output.stderr);
        crate::remote_link::shutdown();
    }

    #[test]
    fn this_machine_lists_what_the_shell_tool_would_run() {
        let shells = probe_local();
        assert_eq!(shells.os, MachineOs::host());
        for shell in &shells.shells {
            assert!(crate::shell_backend::is_registered(shells.os, shell.backend));
            assert!(!shell.path.is_empty());
        }
        #[cfg(unix)]
        assert!(shells.get(ShellBackend::Sh).is_some());
    }
}
