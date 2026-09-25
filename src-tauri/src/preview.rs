//! The seam between `.mework/launch.json` and the dev servers it describes.
//!
//! [`crate::preview_launch_config`] reads the file and [`crate::preview_servers`]
//! owns the processes; neither knows the other exists. This module is the only
//! place that does, so the order a start has to follow — resolve the entry, hand
//! back an attach entry before anything else looks at it, decide whether a running
//! server already answers it, reserve capacity, reserve the port, rewrite the port
//! arguments, spawn — stays in one readable function instead of being spread
//! across Tauri commands and a future tool handler.
//!
//! Two voices meet here. Anything that came out of the two ported modules is the
//! Claude Code desktop app's own English, passed through byte for byte because
//! those strings are the diagnosis; anything this layer decides for itself is in
//! the host's ordinary voice, like every other command error.
//!
//! One deliberate departure from that byte-for-byte rule: the config path reads
//! `.mework/launch.json`, not the source's `.claude/launch.json`. The source's
//! path belongs to a different product that may be installed alongside this one,
//! so sharing it would have the two fight over one file.

use std::path::Path;

use serde::Serialize;

use crate::{
    preview_launch_config::{self, LaunchConfigDiscovery, ServerConfig},
    preview_servers::{
        self, PreviewLogQuery, PreviewServerConfig, PreviewServerRegistry, PreviewServerSnapshot,
        PreviewServerStatus, PreviewStartAction,
    },
};

/// What `preview_logs` answers when the id addresses nothing.
pub const NO_SERVER_FOR_LOGS: &str =
    "No dev server is running. preview_logs takes a process serverId from preview_list.";

/// What `preview_start` says once it has pointed the preview at an attach entry's
/// url. The sentence, and the fact that it is the whole report, are the source's.
pub const ATTACHED_NOTICE: &str =
    "Attached the preview to the configured url; no process was started.";

/// What `preview_stop` answers when handed an attach entry's id.
pub fn no_stop_for_attachment(server_id: &str) -> String {
    format!(
        "\"{}\" attaches the preview to a url; no process was started for it, so there is nothing to stop.",
        preview_launch_config::sanitize_for_message(server_id)
    )
}

/// A repeated name's entry as the model addresses it: the name as the file writes it, and the
/// number its first start gave it.
fn numbered_server_id(name: &str, number: u32) -> String {
    format!("{name}-{number}")
}

/// Whether `server_id` is what `preview_start` answered for an entry of `list` that attaches
/// rather than runs: the entry's name, or — for a name the file repeats — the name numbered.
pub fn is_attach_entry(list: &PreviewConfigurationList, server_id: &str) -> bool {
    let repeated = |name: &str| {
        list.servers
            .iter()
            .filter(|server| preview_launch_config::names_match(&server.name, name))
            .count()
            > 1
    };
    list.servers
        .iter()
        .filter(|server| server.command.is_none() && server.url.is_some())
        .any(|server| {
            preview_launch_config::names_match(&server.name, server_id)
                || server_id.rsplit_once('-').is_some_and(|(base, number)| {
                    preview_launch_config::names_match(&server.name, base)
                        && number.parse::<u32>().is_ok_and(|number| number >= 1)
                        && repeated(&server.name)
                })
        })
}

/// Whether a `serverId` the model passed is `server_id`. Ids are names, and names are matched the
/// way `preview_start` matches them.
pub fn server_ids_match(server_id: &str, requested: &str) -> bool {
    preview_launch_config::names_match(server_id, requested)
}

/// The entry of `list` a server was started from: the one with its name, or — for a name the
/// file repeats — the one its number was given to in `worktree`.
pub fn configured_entry<'a>(
    registry: &PreviewServerRegistry,
    worktree: &Path,
    list: &'a PreviewConfigurationList,
    server: &PreviewServerSnapshot,
) -> Option<&'a PreviewConfiguredServer> {
    let siblings: Vec<&PreviewConfiguredServer> = list
        .servers
        .iter()
        .filter(|entry| preview_launch_config::names_match(&entry.name, &server.name))
        .collect();
    if siblings.len() < 2 {
        return siblings.first().copied();
    }
    let number = server.server_id.rsplit_once('-')?.1.parse::<usize>().ok()?;
    let occurrence = *registry
        .repeated_name_order(worktree, &siblings[0].name)
        .get(number.checked_sub(1)?)?;
    siblings.get(occurrence).copied()
}

/// What one of a conversation's workspaces configures, wherever it is. `None` when that cannot be
/// read right now: a workspace on WSL, or a machine that is away.
pub fn workspace_configurations(
    workspace: &crate::workspace_set::ResolvedWorkspace,
) -> Option<PreviewConfigurationList> {
    match &workspace.machine {
        None => Some(configurations(Path::new(&workspace.root))),
        Some(machine @ crate::model::RunTarget::Ssh { .. }) => remote_configurations(
            &crate::preview_remote::RemoteMachine::new(
                workspace.runner.clone(),
                crate::run_environment::env_key(Some(machine)),
                workspace.machine_label.clone(),
            )
            .with_patience(crate::preview_remote::POLL_PATIENCE),
            &workspace.root,
        )
        .ok(),
        Some(crate::model::RunTarget::Wsl { .. }) => None,
    }
}

/// The registry key of one of a conversation's workspaces: its directory on this computer, or,
/// for one on an SSH machine, the machine-qualified key its servers are filed under there. `None`
/// for a workspace on WSL, which has no previews.
pub fn registry_key(
    workspace: &crate::workspace_set::ResolvedWorkspace,
) -> Option<std::path::PathBuf> {
    match &workspace.machine {
        None => Some(std::path::PathBuf::from(&workspace.root)),
        Some(machine @ crate::model::RunTarget::Ssh { .. }) => Some(
            crate::preview_remote::RemoteMachine::new(
                workspace.runner.clone(),
                crate::run_environment::env_key(Some(machine)),
                workspace.machine_label.clone(),
            )
            .worktree_key(&workspace.root),
        ),
        Some(crate::model::RunTarget::Wsl { .. }) => None,
    }
}

/// One usable entry, as the pane lists it.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewConfiguredServer {
    pub name: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub cwd: String,
    /// Still the parser's `u32`: a port extracted from a command line is not
    /// range-checked, and truncating it here would hide the mistake instead of
    /// showing it.
    pub port: u32,
    pub auto_port: Option<bool>,
    pub url: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewMalformedEntry {
    pub name: String,
    pub reason: String,
}

/// Everything `.mework/launch.json` says, including what is wrong with it.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewConfigurationList {
    pub launch_json_path: String,
    pub servers: Vec<PreviewConfiguredServer>,
    pub malformed: Vec<PreviewMalformedEntry>,
    /// The full explanation of an unusable file. `None` when it is usable.
    pub problem: Option<String>,
    /// The wire reason behind `problem`.
    pub problem_reason: Option<String>,
}

/// A `.mework/launch.json` entry that names a url and no command: the server is
/// somebody else's, and the whole start is pointing the preview at it.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewAttachment {
    /// The entry's name, numbered when the file repeats it — the id a process of it would have.
    /// Nothing answers to it but the page, so `preview_stop` and `preview_logs` refuse it.
    pub server_id: String,
    pub name: String,
    /// The parser's `u32`, unchecked, because nothing binds it: the url carries
    /// the address, and the field is reported only because the entry stated it.
    pub port: u32,
    pub url: String,
}

/// What a start produced.
///
/// Untagged so the attach form is recognised by the field that is only there for
/// it, the way the source's own receipt is: an attach has no process, and giving
/// it a snapshot with a `status` would tell the pane and `preview_list` that
/// something is running when nothing was started.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum PreviewStartOutcome {
    /// A process: spawned by this call, or already running and handed back.
    Server {
        server: PreviewServerSnapshot,
        reused: bool,
    },
    /// The attach form. Nothing was spawned and no port was reserved.
    Attached { attached: PreviewAttachment },
}

impl TryFrom<&ServerConfig> for PreviewServerConfig {
    type Error = String;

    /// The parser keeps `port` as a `u32` because `extractPortFromCommand` reads
    /// whatever digits a command line holds and the source range-checks only the
    /// explicit `port` field. Five digits are not a port, and quietly binding a
    /// different one would leave the server unreachable at the address its own
    /// command line hardcodes.
    fn try_from(config: &ServerConfig) -> Result<Self, Self::Error> {
        let port = u16::try_from(config.port).map_err(|_| {
            format!(
                "launch.json 配置「{}」的端口 {} 超出 0-65535",
                config.name, config.port
            )
        })?;
        Ok(Self {
            name: config.name.clone(),
            // The name as the file writes it; a start that finds it repeated numbers it.
            server_id: config.name.clone(),
            command: config.command.clone(),
            args: config.args.clone(),
            cwd: config.cwd.clone(),
            port,
            env: config.env.clone(),
            auto_port: config.auto_port,
            url: config.url.clone(),
        })
    }
}

/// One workspace of a conversation, as the renderer's preview pages address it: by
/// the number the model addresses it with, exactly as a terminal is opened. A page
/// belongs to one workspace; its start page reads that workspace's
/// `.mework/launch.json` and the servers it runs run on that workspace's machine.
#[derive(Clone, Debug, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewTarget {
    /// The conversation; for the renderer's draft, the id it will materialize as.
    pub conversation_id: String,
    /// Only for the draft: the project it is aimed at.
    #[serde(default)]
    pub draft_workspace_id: Option<String>,
    /// 1-based. Absent is workspace 1 — or, where a command can span them, every workspace.
    #[serde(default)]
    pub workspace: Option<u32>,
}

/// Reads `.mework/launch.json` and reports what it configures.
///
/// Never fails: an absent, unreadable, or broken file is a result the pane has to
/// render, not an error it has to swallow.
pub fn configurations(workspace: &Path) -> PreviewConfigurationList {
    let discovery = LaunchConfigDiscovery::new(workspace);
    let found = discovery.discover(None);
    if let Some(warning) = preview_launch_config::discovery_warning(&found) {
        eprintln!("预览服务器配置（{}）：{warning}", workspace.display());
    }
    configuration_list(&discovery, &found, workspace)
}

/// [`configurations`] for a workspace on another machine: the file is read there,
/// and parsed here by the same parser. Only an unreachable machine is an error —
/// the file's own problems are still a result to render.
pub fn remote_configurations(
    machine: &crate::preview_remote::RemoteMachine,
    root: &str,
) -> Result<PreviewConfigurationList, String> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    /// How long a read stands in for the next. The pane asks every second and a half for as long
    /// as it is open, and each read of another machine's file is a process started there; a file
    /// edited on the machine still shows within a few seconds.
    const FRESH_FOR: Duration = Duration::from_secs(5);
    type Cache = Mutex<HashMap<String, (Instant, PreviewConfigurationList)>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = format!("{}\n{root}", machine.key());
    if let Some((read_at, list)) = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&key)
    {
        if read_at.elapsed() < FRESH_FOR {
            return Ok(list.clone());
        }
    }
    let (discovery, found) = remote_discovery(machine, root)?;
    let list = configuration_list(&discovery, &found, discovery.working_directory());
    cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(key, (Instant::now(), list.clone()));
    Ok(list)
}

/// Reads and parses a remote workspace's `launch.json`, keeping every entry.
fn remote_discovery(
    machine: &crate::preview_remote::RemoteMachine,
    root: &str,
) -> Result<(LaunchConfigDiscovery, preview_launch_config::LaunchDiscovery), String> {
    use crate::preview_remote::RemoteLaunchJsonContent;
    let read = machine.read_launch_json(root)?;
    let discovery = LaunchConfigDiscovery::new(&read.root);
    let found = match read.content {
        RemoteLaunchJsonContent::Read(source) => discovery.parse_source(&source, None),
        RemoteLaunchJsonContent::Missing => preview_launch_config::LaunchDiscovery::NotFound,
        RemoteLaunchJsonContent::Unreadable(message) => {
            preview_launch_config::LaunchDiscovery::Unreadable {
                code: "EACCES".to_owned(),
                message,
            }
        }
    };
    Ok((discovery, found))
}

fn configuration_list(
    discovery: &LaunchConfigDiscovery,
    found: &preview_launch_config::LaunchDiscovery,
    workspace: &Path,
) -> PreviewConfigurationList {
    let config = found.config();
    PreviewConfigurationList {
        launch_json_path: discovery.launch_json_path().to_string_lossy().into_owned(),
        servers: config
            .map(|config| {
                config
                    .servers
                    .iter()
                    .map(|server| PreviewConfiguredServer {
                        name: server.name.clone(),
                        command: server.command.clone(),
                        args: server.args.clone(),
                        cwd: server.cwd.to_string_lossy().into_owned(),
                        port: server.port,
                        auto_port: server.auto_port,
                        url: server.url.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        malformed: found
            .malformed()
            .iter()
            .map(|entry| PreviewMalformedEntry {
                name: entry.name.clone(),
                reason: entry.reason.clone(),
            })
            .collect(),
        problem: preview_launch_config::discovery_model_message(found, workspace),
        problem_reason: preview_launch_config::discovery_deny_reason(found)
            .map(|reason| reason.as_str().to_owned()),
    }
}

/// Starts the server `name` addresses, hands back the one already answering it, or
/// resolves the attach entry the caller then points the preview at.
///
/// Blocks for up to the registry's startup gate and spawns detached threads, so it
/// must run on a blocking worker rather than a runtime thread.
pub fn start(
    registry: &PreviewServerRegistry,
    workspace: &Path,
    requested_name: Option<&str>,
    session_id: Option<&str>,
) -> Result<PreviewStartOutcome, String> {
    // A renderer sends an empty box as an empty string. Normalising once here is
    // what keeps `select_server` (which would look for a server literally named
    // "") and `decide_start_action` (which already treats "" as unnamed) agreeing
    // on whether the caller named anything.
    let requested_name = requested_name
        .map(str::trim)
        .filter(|name| !name.is_empty());

    // Deliberately a nameless discovery: `select_server` needs every sibling entry
    // and every malformed one to explain a name it cannot find.
    let discovery = LaunchConfigDiscovery::new(workspace).discover(None);
    let configured_server_count = discovery.config().map_or(0, |config| config.servers.len());
    let running =
        preview_servers::running_for_session(&registry.servers_for_worktree(workspace), session_id);
    let (selected, server_id) = address_entry(
        registry,
        workspace,
        &discovery,
        workspace,
        requested_name,
        &running,
    )?;

    // Decided where the source decides it: before reuse, before capacity, and
    // before any port is reserved. An entry that names a url and no command
    // describes a server this application never started, so there is no process
    // to reuse and nothing to bind — the preview is simply pointed at the url.
    //
    // The two branches partition every entry the parser can produce, which only
    // keeps a command-less entry when it has a url. One with neither falls through
    // to the registry's own refusal, which is the right message for it.
    if let (None, Some(url)) = (selected.command.as_deref(), selected.url.as_deref()) {
        return attachment(server_id, &selected.name, url, selected.port)
            .map(|attached| PreviewStartOutcome::Attached { attached });
    }

    let mut config = PreviewServerConfig::try_from(&selected)?;
    config.server_id = server_id;

    if let Some(reused) = reuse(
        registry,
        &running,
        &config,
        requested_name,
        configured_server_count,
    )? {
        return Ok(reused);
    }

    registry
        .ensure_capacity(workspace, session_id)
        .map_err(|error| error.message)?;
    let port = registry
        .select_port(config.port, config.auto_port, session_id)
        .map_err(|error| error.message)?;
    if port != config.port {
        config.args = preview_servers::rewrite_port_arguments(&config.args, port);
        config.port = port;
    }
    registry
        .start_with_retries(workspace, &config, session_id)
        .map(|server| PreviewStartOutcome::Server {
            server,
            reused: false,
        })
        .map_err(|error| error.message)
}

/// The entry a start addresses, and the server id it answers to.
///
/// A name the file gives one entry is that entry, and its id is the name as the file writes it.
/// A name the file repeats — nothing stops a launch.json from doing that — addresses the first of
/// its entries that is not running yet, and once every one is, the one first started; each is
/// numbered (`dev-1`, `dev-2`) in the order its entries were first started in this worktree, so
/// another workspace's file never counts. A numbered id addresses the entry it was given to,
/// which is how a particular one is started again.
///
/// `worktree` is the registry key, `working_directory` the directory the file's paths resolve
/// against — the same directory for a workspace on this computer, not for one on another machine.
/// `running` is the caller's own, narrowed by [`preview_servers::running_for_session`].
fn address_entry(
    registry: &PreviewServerRegistry,
    worktree: &Path,
    discovery: &preview_launch_config::LaunchDiscovery,
    working_directory: &Path,
    requested_name: Option<&str>,
    running: &[PreviewServerSnapshot],
) -> Result<(ServerConfig, String), String> {
    let servers = discovery
        .config()
        .map(|config| config.servers.as_slice())
        .unwrap_or_default();
    let siblings_of = |name: &str| -> Vec<&ServerConfig> {
        servers
            .iter()
            .filter(|server| preview_launch_config::names_match(&server.name, name))
            .collect()
    };

    // A numbered id, unless an entry is literally called that.
    if let Some(requested) = requested_name.filter(|requested| siblings_of(requested).is_empty()) {
        if let Some((base, number)) = requested
            .rsplit_once('-')
            .and_then(|(base, number)| Some((base, number.parse::<u32>().ok()?)))
            .filter(|(_, number)| *number >= 1)
        {
            let siblings = siblings_of(base);
            if siblings.len() > 1 {
                let order = registry.repeated_name_order(worktree, &siblings[0].name);
                let Some(entry) = usize::try_from(number - 1)
                    .ok()
                    .and_then(|position| order.get(position))
                    .and_then(|occurrence| siblings.get(*occurrence))
                else {
                    let name = preview_launch_config::sanitize_for_message(&siblings[0].name);
                    return Err(format!(
                        "\"{}\" has not been started. .mework/launch.json has {} servers named \"{name}\"; preview_start with name \"{name}\" starts the next one that is not running, and they are numbered in the order they are first started.",
                        preview_launch_config::sanitize_for_message(requested),
                        siblings.len()
                    ));
                };
                return Ok(((*entry).clone(), numbered_server_id(&entry.name, number)));
            }
        }
    }

    let selected =
        preview_launch_config::select_server(discovery, working_directory, requested_name)
            .map_err(|denial| denial.message)?;
    let siblings = siblings_of(&selected.name);
    if siblings.len() < 2 {
        let server_id = selected.name.clone();
        return Ok((selected, server_id));
    }
    let key = &siblings[0].name;
    let order = registry.repeated_name_order(worktree, key);
    let taken = |occurrence: usize| {
        let Some(position) = order.iter().position(|started| *started == occurrence) else {
            return false;
        };
        let entry = siblings[occurrence];
        // An attach entry has no process to be running; once attached, it has had its turn.
        if entry.command.is_none() {
            return true;
        }
        let server_id =
            numbered_server_id(&entry.name, u32::try_from(position + 1).unwrap_or(u32::MAX));
        running
            .iter()
            .any(|server| server.server_id.eq_ignore_ascii_case(&server_id))
    };
    let occurrence = (0..siblings.len())
        .find(|occurrence| !taken(*occurrence))
        .or_else(|| order.first().copied())
        .unwrap_or(0);
    let number = registry.number_repeated_name(worktree, key, occurrence);
    let entry = siblings[occurrence].clone();
    let server_id = numbered_server_id(&entry.name, number);
    Ok((entry, server_id))
}

/// The running server a start hands back instead of spawning another, if there is one.
///
/// The decision is made from a snapshot. A server that died in between has to be
/// reported as gone: respawning it silently would answer a "reuse" with a different
/// process than the one the caller was told about.
fn reuse(
    registry: &PreviewServerRegistry,
    running: &[PreviewServerSnapshot],
    config: &PreviewServerConfig,
    requested_name: Option<&str>,
    configured_server_count: usize,
) -> Result<Option<PreviewStartOutcome>, String> {
    let PreviewStartAction::Reuse { server, .. } = preview_servers::decide_start_action(
        running,
        &config.server_id,
        config.port,
        requested_name,
        configured_server_count,
    ) else {
        return Ok(None);
    };
    match registry.get(&server.handle).filter(|live| {
        matches!(
            live.status,
            PreviewServerStatus::Running | PreviewServerStatus::Starting
        )
    }) {
        Some(server) => Ok(Some(PreviewStartOutcome::Server {
            server,
            reused: true,
        })),
        None => Err(preview_servers::dead_reuse_refusal_message(
            &server.server_id,
        )),
    }
}

/// [`start`] for a workspace on another machine, through its agent.
///
/// The same order, the same decisions and the same messages; what changes is where
/// each question is asked. The file is read there, the port is probed there, and
/// the server runs there. An attach entry's url is handed back as the file wrote
/// it: it names that machine's `localhost`, which is where a page of that
/// workspace reaches.
///
/// `root` is the workspace directory as the conversation records it, which is also
/// what the registry files the machine's servers under, so a listing never has to
/// ask the machine where its root resolves to.
pub fn start_remote(
    registry: &PreviewServerRegistry,
    machine: &crate::preview_remote::RemoteMachine,
    root: &str,
    requested_name: Option<&str>,
    session_id: Option<&str>,
) -> Result<PreviewStartOutcome, String> {
    let requested_name = requested_name
        .map(str::trim)
        .filter(|name| !name.is_empty());
    let (discovery, found) = remote_discovery(machine, root)?;
    let resolved_root = discovery.working_directory().to_path_buf();
    let configured_server_count = found.config().map_or(0, |config| config.servers.len());
    let worktree = machine.worktree_key(root);
    let running =
        preview_servers::running_for_session(&registry.servers_for_worktree(&worktree), session_id);
    let (selected, server_id) = address_entry(
        registry,
        &worktree,
        &found,
        &resolved_root,
        requested_name,
        &running,
    )?;
    if let (None, Some(url)) = (selected.command.as_deref(), selected.url.as_deref()) {
        return attachment(server_id, &selected.name, url, selected.port)
            .map(|attached| PreviewStartOutcome::Attached { attached });
    }

    let mut config = PreviewServerConfig::try_from(&selected)?;
    config.server_id = server_id;
    if let Some(reused) = reuse(
        registry,
        &running,
        &config,
        requested_name,
        configured_server_count,
    )? {
        return Ok(reused);
    }

    registry
        .ensure_capacity(&worktree, session_id)
        .map_err(|error| error.message)?;
    let port = registry
        .select_remote_port(machine, config.port, config.auto_port, session_id)
        .map_err(|error| error.message)?;
    if port != config.port {
        config.args = preview_servers::rewrite_port_arguments(&config.args, port);
        config.port = port;
    }
    let host = crate::preview_remote::host(machine);
    let display_root = resolved_root.to_string_lossy().into_owned();
    let mut last = None;
    for attempt in 1..=preview_servers::MAX_SPAWN_ATTEMPTS {
        match registry.start_remote(&worktree, &display_root, host.clone(), &config, session_id) {
            Ok(server) => {
                return Ok(PreviewStartOutcome::Server {
                    server,
                    reused: false,
                })
            }
            Err(error) if !error.is_retryable() || attempt == preview_servers::MAX_SPAWN_ATTEMPTS => {
                return Err(error.message)
            }
            Err(error) => last = Some(error),
        }
    }
    Err(last.map_or_else(
        || "Failed to start preview server after retries".to_owned(),
        |error| error.message,
    ))
}

/// The attach form's own checks, applied to an entry the parser already accepted.
///
/// The localhost rule is re-stated rather than assumed: it is the reason a config
/// file cannot aim the preview at an arbitrary local endpoint, and this is the
/// second module that would have to keep it. The source words the refusal as the
/// boundary violation it would be, because a file that reaches here has already
/// passed [`preview_launch_config::validate_url`].
fn attachment(
    server_id: String,
    name: &str,
    url: &str,
    port: u32,
) -> Result<PreviewAttachment, String> {
    if preview_launch_config::is_localhost_url(url)
        && !preview_launch_config::is_bare_localhost_origin(url)
    {
        return Err(format!(
            "\"{}\" has a localhost \"url\" with a path or query, which this build should have rejected at config parsing. Change \"url\" to the server's origin (for example \"https://localhost:8443/\") and report this message if the config file already looks correct.",
            preview_launch_config::sanitize_for_message(name)
        ));
    }
    Ok(PreviewAttachment {
        server_id,
        name: name.to_owned(),
        port,
        url: url.to_owned(),
    })
}

/// One server's buffered output, filtered and tail-sliced. `handle` is the registry's.
pub fn logs(registry: &PreviewServerRegistry, handle: &str, query: &PreviewLogQuery) -> String {
    if registry.get(handle).is_none() {
        return NO_SERVER_FOR_LOGS.to_owned();
    }
    preview_servers::render_preview_logs(&registry.logs(handle), query)
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, path::PathBuf};

    use super::*;

    fn entry(port: u32) -> ServerConfig {
        ServerConfig {
            name: "dev".to_owned(),
            command: Some("npm".to_owned()),
            args: vec!["run".to_owned(), "dev".to_owned()],
            cwd: PathBuf::from("/project"),
            port,
            env: BTreeMap::new(),
            auto_port: None,
            url: None,
        }
    }

    #[test]
    fn a_parsed_entry_becomes_a_registry_configuration() {
        let converted = PreviewServerConfig::try_from(&entry(5173)).unwrap();

        assert_eq!(converted.name, "dev");
        assert_eq!(converted.command.as_deref(), Some("npm"));
        assert_eq!(converted.args, vec!["run".to_owned(), "dev".to_owned()]);
        assert_eq!(converted.cwd, PathBuf::from("/project"));
        assert_eq!(converted.port, 5173);
        assert_eq!(converted.auto_port, None);
    }

    #[test]
    fn a_port_wider_than_a_port_is_refused_rather_than_truncated() {
        let error = PreviewServerConfig::try_from(&entry(99999)).unwrap_err();

        assert!(error.contains("99999"), "{error}");
        assert!(error.contains("dev"), "{error}");
        // Truncation is the failure this guards: 99999 & 0xFFFF is 34463.
        assert!(!error.contains("34463"), "{error}");
    }

    #[test]
    fn a_missing_launch_json_is_a_reported_problem_not_an_error() {
        let workspace = tempfile::tempdir().unwrap();

        let listed = configurations(workspace.path());

        assert!(listed.servers.is_empty());
        assert!(listed.malformed.is_empty());
        assert_eq!(
            listed.problem_reason.as_deref(),
            Some("launch_config_missing")
        );
        assert!(listed
            .problem
            .unwrap()
            .starts_with("No .mework/launch.json found."));
        assert!(listed.launch_json_path.ends_with("launch.json"));
    }

    #[test]
    fn a_usable_file_lists_its_servers() {
        let workspace = tempfile::tempdir().unwrap();
        write_launch_json(
            workspace.path(),
            r#"{
  "version": "0.0.1",
  "configurations": [
    { "name": "web", "runtimeExecutable": "npm", "runtimeArgs": ["run", "dev"], "port": 5173 }
  ]
}"#,
        );

        let listed = configurations(workspace.path());

        assert_eq!(listed.servers.len(), 1);
        assert_eq!(listed.servers[0].name, "web");
        assert_eq!(listed.servers[0].port, 5173);
        assert_eq!(listed.servers[0].command.as_deref(), Some("npm"));
        assert_eq!(listed.problem, None);
        assert_eq!(listed.problem_reason, None);
    }

    #[test]
    fn logs_for_an_unknown_server_name_the_tool_that_supplies_the_id() {
        let registry = PreviewServerRegistry::default();

        assert_eq!(
            logs(&registry, "missing", &PreviewLogQuery::default()),
            NO_SERVER_FOR_LOGS
        );
    }

    #[test]
    fn starting_without_a_launch_json_reports_the_configs_own_refusal() {
        let workspace = tempfile::tempdir().unwrap();
        let registry = PreviewServerRegistry::default();

        let error = start(&registry, workspace.path(), Some("  "), None).unwrap_err();

        assert!(
            error.starts_with("No .mework/launch.json found."),
            "{error}"
        );
        assert!(registry.servers().is_empty());
    }

    /// The whole chain the format notes promise: a `url` with no command parses,
    /// resolves, and starts — by attaching, without a process behind it.
    #[test]
    fn an_attach_entry_starts_by_resolving_to_its_url_and_nothing_else() {
        let workspace = tempfile::tempdir().unwrap();
        write_launch_json(
            workspace.path(),
            r#"{
  "configurations": [
    { "name": "docs", "url": "https://example.com/docs" }
  ]
}"#,
        );
        let registry = PreviewServerRegistry::default();

        let listed = configurations(workspace.path());
        assert_eq!(listed.servers[0].command, None);
        assert_eq!(
            listed.servers[0].url.as_deref(),
            Some("https://example.com/docs")
        );

        let outcome = start(&registry, workspace.path(), Some("docs"), Some("chat-1")).unwrap();

        assert_eq!(
            outcome,
            PreviewStartOutcome::Attached {
                attached: PreviewAttachment {
                    server_id: "docs".to_owned(),
                    name: "docs".to_owned(),
                    // Non-localhost, so the entry states no port and none is invented.
                    port: 0,
                    url: "https://example.com/docs".to_owned(),
                }
            }
        );
        // Decided before capacity and before the port: nothing was reserved, and
        // above all nothing was spawned.
        assert!(registry.servers().is_empty());
    }

    /// The receipt's id is the entry's name, like a process's, but no process answers to
    /// it, and every id-taking preview surface has to keep answering that way.
    #[test]
    fn an_attach_receipt_answers_to_the_entry_name_and_no_process() {
        let workspace = tempfile::tempdir().unwrap();
        write_launch_json(
            workspace.path(),
            r#"{"configurations":[{"name":"api","url":"http://localhost:8443"}]}"#,
        );
        let registry = PreviewServerRegistry::default();

        let PreviewStartOutcome::Attached { attached } =
            start(&registry, workspace.path(), Some("api"), Some("chat-2")).unwrap()
        else {
            panic!("a url with no command is an attach");
        };

        // A localhost url does supply the port, so the entry reports where it points.
        assert_eq!(attached.port, 8443);
        assert_eq!(attached.server_id, "api");
        let listed = configurations(workspace.path());
        assert!(is_attach_entry(&listed, "api"));
        assert!(is_attach_entry(&listed, "API"));
        // Only a repeated name is ever numbered.
        assert!(!is_attach_entry(&listed, "api-1"));
        assert!(!is_attach_entry(&listed, "web"));
        assert_eq!(
            logs(&registry, &attached.server_id, &PreviewLogQuery::default()),
            NO_SERVER_FOR_LOGS
        );
    }

    /// A file the parser would refuse can still be handed to the attach layer by a
    /// future caller, and the localhost rule is the one that must not be lost in
    /// the handover.
    #[test]
    fn attaching_refuses_a_localhost_url_that_carries_a_path() {
        let workspace = tempfile::tempdir().unwrap();
        write_launch_json(
            workspace.path(),
            r#"{"configurations":[{"name":"admin","url":"http://localhost:9090/admin/wipe"}]}"#,
        );

        // The parser drops it first, so the entry never reaches the attach layer.
        let listed = configurations(workspace.path());
        assert!(listed.servers.is_empty());

        let refusal = attachment(
            "admin".to_owned(),
            "admin",
            "http://localhost:9090/admin/wipe",
            9090,
        )
        .unwrap_err();

        assert!(refusal.starts_with("\"admin\" has a localhost \"url\" with a path or query,"));
        assert!(refusal.contains("should have rejected at config parsing"));
        assert!(attachment("admin".to_owned(), "admin", "http://localhost:9090", 9090).is_ok());
    }

    fn attached_id(outcome: PreviewStartOutcome) -> (String, String) {
        let PreviewStartOutcome::Attached { attached } = outcome else {
            panic!("a url with no command is an attach");
        };
        (attached.server_id, attached.url)
    }

    /// A launch.json may repeat a name. Each of its entries is numbered in the order it is first
    /// started, the name alone reaches the next one not yet started, and a numbered id reaches
    /// the entry it was given to — while another name keeps its bare id.
    #[test]
    fn a_repeated_name_numbers_its_entries_in_the_order_they_are_first_started() {
        let workspace = tempfile::tempdir().unwrap();
        write_launch_json(
            workspace.path(),
            r#"{"configurations":[
  {"name":"docs","url":"https://example.com/a"},
  {"name":"guide","url":"https://example.com/guide"},
  {"name":"Docs","url":"https://example.com/b"}
]}"#,
        );
        let registry = PreviewServerRegistry::default();
        let start_named =
            |name: &str| start(&registry, workspace.path(), Some(name), Some("chat-1"));

        assert_eq!(
            attached_id(start_named("guide").unwrap()),
            ("guide".to_owned(), "https://example.com/guide".to_owned())
        );
        // Not started yet, so there is nothing numbered 1 to address.
        let early = start_named("docs-1").unwrap_err();
        assert!(
            early.starts_with("\"docs-1\" has not been started."),
            "{early}"
        );
        assert!(early.contains("2 servers named \"docs\""), "{early}");

        assert_eq!(
            attached_id(start_named("docs").unwrap()),
            ("docs-1".to_owned(), "https://example.com/a".to_owned())
        );
        assert_eq!(
            attached_id(start_named("docs").unwrap()),
            ("Docs-2".to_owned(), "https://example.com/b".to_owned())
        );
        // Every entry has had its turn: the first one started answers again.
        assert_eq!(
            attached_id(start_named("docs").unwrap()),
            ("docs-1".to_owned(), "https://example.com/a".to_owned())
        );
        assert_eq!(
            attached_id(start_named("docs-2").unwrap()),
            ("Docs-2".to_owned(), "https://example.com/b".to_owned())
        );
        assert!(start_named("docs-3").is_err());
        assert!(is_attach_entry(&configurations(workspace.path()), "docs-2"));
        assert!(registry.servers().is_empty());
    }

    /// With processes behind them, the name starts whichever entry is not running, a running one
    /// is handed back only for its own id, and a stopped entry comes back under the id it had.
    #[cfg(unix)]
    #[test]
    fn a_repeated_name_starts_the_entry_that_is_not_running() {
        let workspace = tempfile::tempdir().unwrap();
        let entry = |label: &str| {
            format!(
                r#"{{"name":"dev","runtimeExecutable":"/bin/sh","runtimeArgs":["-c","sleep 30 # {label}"],"port":0,"autoPort":true}}"#
            )
        };
        write_launch_json(
            workspace.path(),
            &format!(r#"{{"configurations":[{},{}]}}"#, entry("a"), entry("b")),
        );
        let registry = PreviewServerRegistry::default();
        let start_named =
            |name: &str| match start(&registry, workspace.path(), Some(name), Some("chat-1"))
                .unwrap()
            {
                PreviewStartOutcome::Server { server, reused } => (server, reused),
                PreviewStartOutcome::Attached { .. } => panic!("these entries run a command"),
            };

        let (first, reused) = start_named("dev");
        assert_eq!((first.server_id.as_str(), reused), ("dev-1", false));
        let (second, reused) = start_named("dev");
        assert_eq!((second.server_id.as_str(), reused), ("dev-2", false));
        let (again, reused) = start_named("dev-2");
        assert_eq!(
            (again.handle.as_str(), reused),
            (second.handle.as_str(), true)
        );

        assert!(registry.stop(&first.handle));
        let (restarted, reused) = start_named("dev");
        assert_eq!((restarted.server_id.as_str(), reused), ("dev-1", false));
        assert_ne!(restarted.handle, first.handle);
        registry.stop_all();
    }

    fn write_launch_json(workspace: &Path, source: &str) {
        let directory = workspace.join(".mework");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("launch.json"), source).unwrap();
    }
}
