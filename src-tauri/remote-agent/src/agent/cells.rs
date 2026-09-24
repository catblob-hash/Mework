//! The cells a daemon keeps: one sandboxed agent process per conversation,
//! which starts that conversation's sessions inside the sandbox.
//!
//! A cell is this same executable run as `serve --stdio` under the machine's
//! sandbox (see [`super::sandbox`]), and the daemon talks to it the way the
//! host talks to a daemon: through a [`Link`] whose transport is the cell's
//! standard input and output. A session the host starts in a cell is an
//! ordinary session of the daemon's — its output kept in the daemon's rings,
//! resumed across the host's reconnects, reclaimed by the same rules — whose
//! process happens to be a [`RemoteProcess`] of the cell's rather than a
//! child of the daemon's.
//!
//! The daemon stays outside the sandbox and is where the network policy is
//! applied: a macOS cell's processes use a proxy the daemon runs; a Linux
//! cell, which has no network at all, forwards each connection over its link
//! and the daemon dials it ([`crate::tunnel`]).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::sandbox::{self, netgate::Gate, Launch, Machine, Resources};
use super::session::lock;
use crate::client::{CallError, ChildLauncher, DialHandler, Link, LinkConfig, RemoteProcess};
use crate::protocol::{Failure, FailureKind, Policy, SandboxSpec, SpawnSpec};

/// How long a cell with no sessions is kept for the next command.
const IDLE_CELL: Duration = Duration::from_secs(10 * 60);
/// How long a cell has to start and answer.
const CELL_START: Duration = Duration::from_secs(20);
/// Most cells one daemon keeps at once.
const MAX_CELLS: usize = 64;

struct Cell {
    link: Link,
    /// The temporary directory, placeholders and proxy the cell uses; dropped
    /// (cleaned up) with the cell.
    resources: Mutex<Option<Resources>>,
    idle_since: Mutex<Option<Instant>>,
}

pub struct Cells {
    machine: Machine,
    cells: Mutex<HashMap<String, Arc<Cell>>>,
}

impl Cells {
    pub fn new(machine: Machine) -> Self {
        Self {
            machine,
            cells: Mutex::new(HashMap::new()),
        }
    }

    /// Starts `spec` in the cell it names, starting the cell first if it is
    /// not running. `base_env` is the environment the daemon would give an
    /// unsandboxed session.
    pub fn spawn(
        &self,
        client: &str,
        spec: &SpawnSpec,
        sandbox: &SandboxSpec,
        body: &[u8],
        base_env: &BTreeMap<String, String>,
    ) -> Result<RemoteProcess, Failure> {
        let cell = self.cell(client, sandbox, base_env)?;
        let mut forwarded = spec.clone();
        forwarded.sandbox = None;
        // The cell's own proxy settings and temporary directory are what its
        // processes must use; a host that passes the account's own would
        // point them at something they cannot reach.
        forwarded
            .env
            .retain(|name, _| !is_managed_env_name(name));
        forwarded.env_remove.retain(|name| !is_managed_env_name(name));
        match cell.link.spawn(forwarded, body, CELL_START) {
            Ok(process) => {
                *lock(&cell.idle_since) = None;
                Ok(process)
            }
            Err(CallError::Failed(failure)) => Err(failure),
            Err(error) => Err(Failure::new(FailureKind::Unsupported, format!("The sandbox did not start: {error}"))),
        }
    }

    fn cell(&self, client: &str, sandbox: &SandboxSpec, base_env: &BTreeMap<String, String>) -> Result<Arc<Cell>, Failure> {
        if sandbox.cell.is_empty() || sandbox.cell.len() > 256 {
            return Err(Failure::new(FailureKind::Invalid, "Invalid sandbox cell name"));
        }
        let policy = serde_json::to_vec(&sandbox.policy).unwrap_or_default();
        let key = format!("{client}\u{0}{}\u{0}{}", sandbox.cell, digest(&policy));
        let mut cells = lock(&self.cells);
        if let Some(cell) = cells.get(&key) {
            if !matches!(cell.link.status(), crate::client::LinkStatus::Unavailable { .. } | crate::client::LinkStatus::Closed) {
                return Ok(Arc::clone(cell));
            }
            cells.remove(&key);
        }
        if cells.len() >= MAX_CELLS {
            return Err(Failure::new(FailureKind::Limit, "Too many sandboxes are running on this machine"));
        }
        let token = super::platform::random_hex(16);
        let exe = std::env::current_exe()
            .map_err(|error| Failure::new(FailureKind::Io, format!("The agent cannot find itself: {error}")))?;
        let cell_args = vec![exe.to_string_lossy().into_owned(), "serve".into(), "--stdio".into()];
        let Launch {
            argv,
            env,
            resources,
            tunnelled,
            cwd,
        } = sandbox::prepare(&sandbox.policy, &self.machine, base_env, &cell_args, &token)
            .map_err(|error| Failure::new(FailureKind::Unsupported, error))?;
        let mut config = LinkConfig::new(format!("cell-parent-{}", std::process::id()), super::platform::random_hex(8));
        config.policy = Policy {
            // The daemon's link to a cell is a pipe; it only ends with one of
            // them, and then everything in the cell ends too.
            orphan_ttl_secs: 60,
            silence_timeout_secs: 60,
            finished_ttl_secs: 60,
        };
        config.dead_after = Duration::from_secs(60);
        config.handshake_timeout = CELL_START;
        config.give_up_after = Duration::from_secs(15);
        config.backoff_initial = Duration::from_millis(200);
        config.backoff_max = Duration::from_secs(2);
        if tunnelled {
            let gate = Gate::new(sandbox.policy.network.clone());
            config.dialer = Some(DialHandler::new(move |host, port| sandbox::gate_connect(&gate, host, port)));
        }
        let mut launcher = ChildLauncher::new(&argv[0], argv[1..].to_vec());
        launcher.env = Some(env);
        launcher.cwd = cwd;
        // What the sandbox or the cell says goes to the daemon's log.
        launcher.stderr = Some(Arc::new(|line: &str| super::log(&format!("cell: {line}"))));
        let link = Link::start(config, launcher);
        let cell = Arc::new(Cell {
            link,
            resources: Mutex::new(Some(resources)),
            idle_since: Mutex::new(None),
        });
        cells.insert(key, Arc::clone(&cell));
        drop(cells);
        if let Err(error) = cell.link.wait_ready(CELL_START) {
            self.close(&cell);
            lock(&self.cells).retain(|_, other| !Arc::ptr_eq(other, &cell));
            return Err(Failure::new(FailureKind::Unsupported, format!("The sandbox did not start: {error}")));
        }
        Ok(cell)
    }

    /// Ends cells that have had no session for a while. Called by the
    /// daemon's janitor.
    pub fn sweep(&self) {
        let now = Instant::now();
        let mut ended = Vec::new();
        {
            let mut cells = lock(&self.cells);
            cells.retain(|_, cell| {
                let dead = matches!(
                    cell.link.status(),
                    crate::client::LinkStatus::Unavailable { .. } | crate::client::LinkStatus::Closed | crate::client::LinkStatus::Lost { .. }
                );
                let idle = cell.link.is_idle();
                let mut since = lock(&cell.idle_since);
                if !idle {
                    *since = None;
                } else if since.is_none() {
                    *since = Some(now);
                }
                let expired = since.is_some_and(|since| now.duration_since(since) >= IDLE_CELL);
                if dead || expired {
                    ended.push(Arc::clone(cell));
                    false
                } else {
                    true
                }
            });
        }
        for cell in ended {
            self.close(&cell);
        }
    }

    /// Ends every cell and everything in them, and gives back what they held
    /// on the machine.
    pub fn shutdown(&self) {
        let cells: Vec<Arc<Cell>> = lock(&self.cells).drain().map(|(_, cell)| cell).collect();
        let had_cells = !cells.is_empty();
        for cell in cells {
            self.close(&cell);
        }
        if had_cells {
            sandbox::release_machine();
        }
    }

    pub fn count(&self) -> usize {
        lock(&self.cells).len()
    }

    fn close(&self, cell: &Cell) {
        cell.link.close(true);
        // The link's closer ended the process; only then may its directories
        // go.
        drop(lock(&cell.resources).take());
    }
}

/// Names a cell sets for itself.
fn is_managed_env_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "http_proxy"
            | "https_proxy"
            | "all_proxy"
            | "no_proxy"
            | "ftp_proxy"
            | "grpc_proxy"
            | "node_use_env_proxy"
            | "tmpdir"
            | "tmp"
            | "temp"
            | "mework_sandbox"
    )
}

fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    super::platform::hex(&Sha256::digest(bytes))[..16].to_owned()
}

/// The paths of a daemon on this machine, for [`Cells::new`].
pub fn machine(home: PathBuf, agent_root: Option<PathBuf>, run_dir: Option<PathBuf>) -> Machine {
    Machine {
        home,
        agent_root,
        run_dir,
        agent_exe: std::env::current_exe().unwrap_or_default(),
    }
}
