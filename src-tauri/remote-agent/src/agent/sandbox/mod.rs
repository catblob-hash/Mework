//! The operating system's own sandbox around a *cell*: the agent process that
//! runs one conversation's sessions (see [`crate::protocol::SandboxSpec`]).
//!
//! The threat is code the conversation runs — a dependency's install script,
//! a test, whatever the model was talked into — being hostile. The sandbox
//! keeps it from:
//!
//! * reading credentials: SSH keys, cloud and registry tokens, keychains,
//!   browser profiles, Mework's own vault ([`rules`]);
//! * writing anything but the workspaces, or anything in them that runs
//!   outside the sandbox later: git hooks and config, `.mework`, editor tasks
//!   ([`rules`]);
//! * reaching the network except through a proxy outside it that applies the
//!   conversation's policy, and never the machine's own services or the
//!   networks around it ([`netgate`], [`proxy`]);
//! * inspecting, signalling or debugging any process that is not its own —
//!   Mework, the agent, other conversations — or talking to the services
//!   that act on the account's behalf (session bus, keyring, SSH agent).
//!
//! The mechanisms are the systems' own, as Claude Code's are: Seatbelt on
//! macOS ([`seatbelt`]), bubblewrap and seccomp on Linux and WSL 2
//! ([`bubblewrap`], `seccomp`), and on Windows srt-win, Claude Code's own
//! Windows backend, which runs the cell as a dedicated account set up once
//! with administrator rights ([`windows`]). Where the mechanism is missing a
//! cell is refused, never started without it.

pub mod bubblewrap;
pub mod netgate;
pub mod proxy;
pub mod rules;
pub mod seatbelt;
#[cfg(target_os = "linux")]
pub mod seccomp;
pub mod windows;

use std::collections::BTreeMap;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Serialize};

use crate::protocol::{SandboxPolicy, SandboxSupport};
use proxy::{Dial, DialError, Proxy, Upstream};

/// What this machine offers. Found once where the answer cannot change while
/// the agent runs; on Windows asked again while it is no, since the sandbox
/// can be set up meanwhile.
pub fn support() -> SandboxSupport {
    static SUPPORT: OnceLock<SandboxSupport> = OnceLock::new();
    if cfg!(windows) {
        return probe();
    }
    SUPPORT.get_or_init(probe).clone()
}

fn probe() -> SandboxSupport {
    if cfg!(target_os = "macos") {
        let available = Path::new("/usr/bin/sandbox-exec").is_file();
        SandboxSupport {
            backend: "seatbelt".into(),
            available,
            detail: if available {
                String::new()
            } else {
                "/usr/bin/sandbox-exec is missing".into()
            },
            setup: false,
        }
    } else if cfg!(target_os = "linux") {
        match bubblewrap::probe() {
            Ok(path) => SandboxSupport {
                backend: "bubblewrap".into(),
                available: true,
                detail: path.display().to_string(),
                setup: false,
            },
            Err(detail) => SandboxSupport {
                backend: "bubblewrap".into(),
                available: false,
                detail,
                setup: false,
            },
        }
    } else if cfg!(windows) {
        match windows::probe() {
            Ok(helper) => SandboxSupport {
                backend: "srt-win".into(),
                available: true,
                detail: helper.exe.display().to_string(),
                setup: false,
            },
            Err(unavailable) => SandboxSupport {
                backend: "srt-win".into(),
                available: false,
                detail: unavailable.detail,
                setup: unavailable.setup,
            },
        }
    } else {
        SandboxSupport {
            backend: String::new(),
            available: false,
            detail: "This operating system has no sandbox Mework can use".into(),
            setup: false,
        }
    }
}

/// What a cell is told about itself on its command line.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellConfig {
    /// Install the seccomp filter before anything else (Linux).
    #[serde(default)]
    pub seccomp: bool,
    /// Run the proxy inside the sandbox on this loopback port, and reach
    /// the network through the link (Linux).
    #[serde(default)]
    pub proxy_port: Option<u16>,
    /// The proxy's password.
    #[serde(default)]
    pub proxy_token: String,
}

/// The machine facts a cell's rules depend on that only the agent knows.
#[derive(Clone, Debug)]
pub struct Machine {
    pub home: PathBuf,
    pub agent_root: Option<PathBuf>,
    pub run_dir: Option<PathBuf>,
    pub agent_exe: PathBuf,
}

/// Everything needed to start one cell.
pub struct Launch {
    /// The whole command line: the sandbox's own program, then the agent.
    pub argv: Vec<String>,
    /// The cell's whole environment.
    pub env: BTreeMap<String, String>,
    /// Kept until the cell is gone.
    pub resources: Resources,
    /// Whether the cell reaches the network through its link (Linux) rather
    /// than a proxy outside it.
    pub tunnelled: bool,
    /// Where the sandbox's own program starts: somewhere the sandboxed
    /// account can enter (Windows).
    pub cwd: Option<PathBuf>,
}

/// What a cell holds on the machine besides its process.
pub struct Resources {
    pub tmp: PathBuf,
    pub placeholders: bubblewrap::Placeholders,
    /// The proxy outside the sandbox, where there is one (macOS).
    pub proxy: Option<Proxy>,
    /// The cell's password on the proxy this agent shares among its cells
    /// (Windows), withdrawn with the cell.
    pub shared_proxy_token: Option<String>,
}

impl Drop for Resources {
    fn drop(&mut self) {
        self.placeholders.remove();
        if let Some(token) = &self.shared_proxy_token {
            shared_proxy::withdraw(token);
        }
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

/// Windows: one proxy for all of this agent's cells, on a port inside the
/// range srt-win's network fence lets the sandbox account reach. Every cell
/// runs as that one account, so a cell is told apart by its password alone.
mod shared_proxy {
    use std::collections::HashMap;
    use std::ops::RangeInclusive;
    use std::sync::{Arc, Mutex};

    use super::proxy::{Authorize, Dial, Proxy};

    struct Shared {
        proxy: Proxy,
        cells: Arc<Mutex<HashMap<String, Arc<dyn Dial>>>>,
    }

    static SHARED: Mutex<Option<Shared>> = Mutex::new(None);

    /// Admits `token` with `dialer`'s policy; the proxy's port, the first
    /// free one of `ports`.
    pub fn admit(token: &str, dialer: Arc<dyn Dial>, ports: &RangeInclusive<u16>) -> Result<u16, String> {
        let mut shared = SHARED.lock().unwrap_or_else(|e| e.into_inner());
        if shared.is_none() {
            let cells: Arc<Mutex<HashMap<String, Arc<dyn Dial>>>> = Arc::default();
            let lookup = Arc::clone(&cells);
            let authorize: Authorize = Arc::new(move |presented: Option<&str>| {
                let presented = presented?;
                lookup.lock().unwrap_or_else(|e| e.into_inner()).get(presented).cloned()
            });
            let listener = ports
                .clone()
                .find_map(|port| std::net::TcpListener::bind(("127.0.0.1", port)).ok())
                .ok_or_else(|| {
                    format!(
                        "Every loopback port the Windows sandbox may reach ({}-{}) is taken",
                        ports.start(),
                        ports.end()
                    )
                })?;
            let proxy = Proxy::start_authorized(listener, authorize)
                .map_err(|error| format!("Cannot start the sandbox's proxy: {error}"))?;
            *shared = Some(Shared { proxy, cells });
        }
        let shared = shared.as_ref().expect("set above");
        shared
            .cells
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(token.to_owned(), dialer);
        Ok(shared.proxy.port())
    }

    pub fn withdraw(token: &str) {
        if let Some(shared) = SHARED.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            shared.cells.lock().unwrap_or_else(|e| e.into_inner()).remove(token);
        }
    }
}

/// Dials straight from the agent, by the gate.
pub struct GateDial(pub netgate::Gate);

impl Dial for GateDial {
    fn dial(&self, host: &str, port: u16) -> Result<Upstream, DialError> {
        if let Err(message) = self.0.decide(host, port) {
            return Err(DialError { refused: true, message });
        }
        let stream = self
            .0
            .connect(host, port)
            .map_err(|message| DialError { refused: message.contains("local or private"), message })?;
        Upstream::tcp(stream).map_err(|error| DialError {
            refused: false,
            message: error.to_string(),
        })
    }
}

/// Connects for a tunnelled cell: `Ok` with the stream, or whether the policy
/// refused and why.
pub fn gate_connect(gate: &netgate::Gate, host: &str, port: u16) -> Result<TcpStream, (bool, String)> {
    gate.decide(host, port).map_err(|message| (true, message))?;
    gate.connect(host, port)
        .map_err(|message| (message.contains("local or private"), message))
}

/// Plans a cell: resolves the rules, creates its temporary directory (and on
/// Linux the placeholders its protected paths need), starts the proxy it will
/// use where that runs outside, and builds the command line and environment.
pub fn prepare(
    policy: &SandboxPolicy,
    machine: &Machine,
    base_env: &BTreeMap<String, String>,
    cell_args: &[String],
    token: &str,
) -> Result<Launch, String> {
    let support = support();
    if !support.available {
        return Err(format!("The sandbox is not available on this machine: {}", support.detail));
    }
    let tmp = make_cell_tmp()?;
    let cache_dir = cache_root(&machine.home).join(roots_digest(&policy.writable));
    std::fs::create_dir_all(&cache_dir)
        .map_err(|error| format!("Cannot create the sandbox cache {}: {error}", cache_dir.display()))?;
    let path_dirs: Vec<PathBuf> = [base_env.get("PATH").cloned(), std::env::var("PATH").ok()]
        .into_iter()
        .flatten()
        .flat_map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .collect();
    let facts = rules::Facts {
        os: rules::Os::current(),
        home: machine.home.clone(),
        agent_root: machine.agent_root.clone(),
        agent_exe: machine.agent_exe.clone(),
        run_dir: machine.run_dir.clone(),
        path_dirs,
        cell_tmp: tmp.clone(),
        cache_dir: cache_dir.clone(),
    };
    let mut resources = Resources {
        tmp: tmp.clone(),
        placeholders: bubblewrap::Placeholders::default(),
        proxy: None,
        shared_proxy_token: None,
    };
    let rules = rules::resolve(policy, &facts)?;

    let mut env: BTreeMap<String, String> = base_env
        .iter()
        .filter(|(name, _)| !rules::is_secret_env_name(name) && !is_proxy_env_name(name))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let tmp_text = tmp.to_string_lossy().into_owned();
    for name in ["TMPDIR", "TMP", "TEMP"] {
        env.insert(name.into(), tmp_text.clone());
    }
    env.extend(rules::cache_env(&cache_dir));
    env.insert("MEWORK_SANDBOX".into(), "1".into());

    let mut config = CellConfig {
        proxy_token: token.to_owned(),
        ..CellConfig::default()
    };
    let program: Vec<String> = cell_args.to_vec();
    let mut cwd = None;
    let (argv, tunnelled) = if cfg!(windows) {
        let helper = windows::probe().map_err(|unavailable| unavailable.detail)?;
        let gate = netgate::Gate::new(policy.network.clone());
        let port = shared_proxy::admit(token, Arc::new(GateDial(gate)), &helper.ports)?;
        resources.shared_proxy_token = Some(token.to_owned());
        let workspaces: Vec<PathBuf> = rules
            .writable
            .iter()
            .filter(|root| **root != rules::canonical(&tmp) && **root != rules::canonical(&cache_dir))
            .cloned()
            .collect();
        // The account reads nothing of the user's unless granted: the
        // agent itself, what the rules re-allow, and the user's own tools
        // on `PATH` — never a directory that holds credentials.
        let home = rules::canonical(&machine.home);
        let mut read: Vec<PathBuf> = rules.readable.clone();
        read.extend(
            facts
                .path_dirs
                .iter()
                .map(|dir| rules::canonical(dir))
                .filter(|dir| dir.starts_with(&home) && dir.is_dir())
                .filter(|dir| !rules.deny_read.iter().any(|denied| dir.starts_with(denied) || denied.starts_with(dir))),
        );
        read.sort();
        read.dedup();
        windows::grant(&helper, &read, &rules.writable)?;
        // Inside what it may write, what it may still not: the protected
        // paths (created as placeholders when missing), and, for a
        // workspace that is one, its `.git` pinned in place.
        let mut confinement = windows::Confinement::default();
        let inside = |path: &Path| rules.writable.iter().any(|root| path.starts_with(root));
        let granted = |path: &Path| inside(path) || read.iter().any(|root| path.starts_with(root));
        confinement.deny_read = rules
            .deny_read
            .iter()
            .filter(|path| granted(path) && path.exists())
            .cloned()
            .collect();
        let mut deny_write: Vec<String> = rules
            .deny_write
            .iter()
            .filter(|path| inside(path) && path.exists())
            .map(|path| path.to_string_lossy().into_owned())
            .collect();
        for found in rules::find_protected(&workspaces, &rules.protected_names, 3) {
            deny_write.push(found.to_string_lossy().into_owned());
        }
        // srt-win makes a missing target exist (a trailing separator makes it
        // a directory) and removes it again afterwards.
        let placeholder = |path: PathBuf, directory: bool| {
            let text = path.to_string_lossy().into_owned();
            if directory {
                format!("{text}\\")
            } else {
                text
            }
        };
        for root in &workspaces {
            for (name, directory, _) in rules::ROOT_PLACEHOLDERS {
                deny_write.push(placeholder(root.join(name), *directory));
            }
            let git = root.join(".git");
            if git.is_dir() {
                for (name, directory, _) in rules::GIT_PLACEHOLDERS {
                    deny_write.push(placeholder(git.join(name), *directory));
                }
                confinement.deny_delete.push(git);
            }
        }
        for root in rules.bare_roots.iter().filter(|root| workspaces.contains(root)) {
            deny_write.push(format!("{}\\", root.join(".git").to_string_lossy()));
            for marker in rules::BARE_REPOSITORY_MARKERS {
                deny_write.push(root.join(marker).to_string_lossy().into_owned());
            }
        }
        deny_write.sort();
        deny_write.dedup();
        confinement.deny_write = deny_write;
        // The account's own profile supplies the rest of the environment;
        // the user's `PATH` finds the user's tools.
        let mut overlay: Vec<(String, String)> = Vec::new();
        if let Some(path) = base_env.iter().find(|(name, _)| name.eq_ignore_ascii_case("PATH")) {
            overlay.push(("PATH".into(), path.1.clone()));
        }
        for (name, value) in &env {
            if matches!(name.as_str(), "TMPDIR" | "TMP" | "TEMP" | "MEWORK_SANDBOX") || rules::cache_env(&cache_dir).contains_key(name) {
                overlay.push((name.clone(), value.clone()));
            }
        }
        overlay.extend(proxy::environment(port, token, false));
        env.extend(proxy::environment(port, token, false));
        confinement.env = overlay;
        let mut program = program;
        program.push("--cell".into());
        program.push(serde_json::to_string(&config).expect("the config serializes"));
        cwd = Some(tmp.clone());
        (windows::exec_argv(&helper, &confinement, &program), false)
    } else if cfg!(target_os = "macos") {
        let listener = TcpListener::bind("127.0.0.1:0")
            .map_err(|error| format!("Cannot open the sandbox's proxy: {error}"))?;
        let gate = netgate::Gate::new(policy.network.clone());
        let proxy = Proxy::start(listener, Some(token.to_owned()), Arc::new(GateDial(gate)))
            .map_err(|error| format!("Cannot start the sandbox's proxy: {error}"))?;
        env.extend(proxy::environment(proxy.port(), token, false));
        let profile = seatbelt::profile(&rules, seatbelt::ProxyPort(proxy.port()));
        resources.proxy = Some(proxy);
        let mut program = program;
        program.push("--cell".into());
        program.push(serde_json::to_string(&config).expect("the config serializes"));
        (seatbelt::argv(&profile, &program), false)
    } else if cfg!(target_os = "linux") {
        let bwrap = bubblewrap::find().ok_or("bubblewrap is not installed")?;
        config.seccomp = true;
        config.proxy_port = Some(bubblewrap::CELL_PROXY_PORT);
        env.extend(proxy::environment(bubblewrap::CELL_PROXY_PORT, token, true));
        let mut program = program;
        program.push("--cell".into());
        program.push(serde_json::to_string(&config).expect("the config serializes"));
        let own = [rules::canonical(&tmp), rules::canonical(&cache_dir)];
        let (argv, placeholders) = bubblewrap::argv(&bwrap, &rules, &own, &program);
        resources.placeholders = placeholders;
        (argv, true)
    } else {
        return Err(support.detail);
    };
    Ok(Launch {
        argv,
        env,
        resources,
        tunnelled,
        cwd,
    })
}

/// Leaves the machine as this agent found it: on Windows, takes back the
/// account's grants.
pub fn release_machine() {
    if cfg!(windows) {
        if let Ok(helper) = windows::probe() {
            windows::revoke(&helper);
        }
    }
}

fn is_proxy_env_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "http_proxy" | "https_proxy" | "all_proxy" | "no_proxy" | "ftp_proxy" | "grpc_proxy" | "node_use_env_proxy"
    )
}

/// A fresh directory only this account can enter, for one cell's temporary
/// files.
fn make_cell_tmp() -> Result<PathBuf, String> {
    let base = std::env::temp_dir();
    let path = base.join(format!("mework-cell-{}", super::platform::random_hex(8)));
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(&path)
        .map_err(|error| format!("Cannot create the sandbox's temporary directory: {error}"))?;
    Ok(rules::canonical(&path))
}

/// Where cells keep package caches: the system's cache directory, not the
/// agent's (which the sandbox cannot write).
fn cache_root(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Caches/com.mework.sandbox")
    } else if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Local"))
            .join("mework-sandbox")
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| home.join(".cache"))
            .join("mework-sandbox")
    }
}

/// Cells working on the same directories share caches; others do not, so
/// one project's sandbox cannot plant a package another project's will use.
fn roots_digest(roots: &[String]) -> String {
    use sha2::{Digest, Sha256};
    let mut sorted: Vec<&String> = roots.iter().collect();
    sorted.sort();
    let mut hasher = Sha256::new();
    for root in sorted {
        hasher.update(root.as_bytes());
        hasher.update([0]);
    }
    super::platform::hex(&hasher.finalize())[..16].to_owned()
}
