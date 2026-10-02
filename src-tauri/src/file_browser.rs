//! The file pane's reads and writes, on whichever machine the reader is looking at.
//!
//! The pane is the user's own file manager: it browses this computer, a WSL
//! distribution or a registered SSH machine anywhere the account can reach, not
//! only inside a workspace. Every request names its machine; the machine is
//! looked up in the persisted catalog like every other remote operation, so the
//! renderer cannot name an endpoint that is not registered.
//!
//! The requests themselves are [`remote_agent::files`]: this computer answers
//! them in-process, another machine through its agent's `files` helper — one
//! spawn and one round trip each, the transport the remote Git helper uses
//! ([`crate::remote_git`]). A machine whose agent is not up (still installing,
//! no build for it) has no file browser yet; the reply says so instead of
//! falling back to a second implementation in shell script.
//!
//! Nothing here is reachable from the model. The transcript's path links only
//! ask the pane to *show* a file; the writes — rename, delete, a new directory —
//! are the pane's own menu items. Deleting on this computer moves to the Trash
//! (the Recycle Bin on Windows); on another machine there is no Trash to move
//! to, and the pane says before it asks that the delete is for good.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use git_core::text;
use remote_agent::files::{self as service, EntryKind, FilesOp, FilesReply, FilesRequest, Place};
use remote_agent::protocol::SELF_PROGRAM;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::cancel::CancelSignal;
use crate::model::{ExecutionEnvironmentAssets, RunTarget};
use crate::run_environment::{self, ShellRunner};

/// How long a request the reader is waiting on may wait for the machine's
/// link. The first request to a machine may install its agent there.
const LINK_PATIENCE: Duration = Duration::from_secs(45);
/// Bound on one read or write through the link.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// A probe is asked on hover and on click, to decide where a path in the
/// transcript lives: it is worth nothing late, so it neither waits for a link
/// that is not up nor for a machine that does not answer.
const PROBE_PATIENCE: Duration = Duration::from_millis(1500);
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
/// A machine that has not answered a probe this long is asked whether it is
/// there at all: a healthy one answers in tens of milliseconds.
const PROBE_CHECK_AFTER: Duration = Duration::from_millis(300);
/// How long that question may take. A machine that cannot say it is there in
/// this time is left out of the probe.
const PROBE_CHECK_WITHIN: Duration = Duration::from_millis(700);

/// The machine a request is for.
#[derive(Clone)]
pub(crate) enum Machine {
    Local,
    Remote(ShellRunner),
}

impl Machine {
    /// `target` absent is this computer.
    pub(crate) fn resolve(
        assets: &ExecutionEnvironmentAssets,
        target: Option<&RunTarget>,
    ) -> Result<Self, String> {
        match target {
            None => Ok(Machine::Local),
            Some(target) => Ok(Machine::Remote(run_environment::resolve_shell_runner(
                assets,
                Some(target),
                None,
            )?)),
        }
    }
}

fn english() -> bool {
    git_core::english()
}

/// Runs one request on `machine`.
fn call<T: DeserializeOwned>(
    machine: &Machine,
    op: FilesOp,
    timeout: Duration,
    patience: Duration,
) -> Result<T, String> {
    let request = FilesRequest {
        op,
        english: english(),
    };
    let reply = match machine {
        Machine::Local => service::handle(request),
        Machine::Remote(runner) => remote_reply(runner, &request, timeout, patience)?,
    };
    match reply {
        FilesReply::Ok(value) => serde_json::from_value(value).map_err(|error| {
            text!(
                "文件回答无法解析：{error}",
                "Could not parse the file reply: {error}"
            )
        }),
        FilesReply::Err(message) => Err(message),
    }
}

fn remote_reply(
    runner: &ShellRunner,
    request: &FilesRequest,
    timeout: Duration,
    patience: Duration,
) -> Result<FilesReply, String> {
    let Some(link) = crate::remote_link::helper_link(runner, patience)? else {
        return Err(text!(
            "这台机器上的 Mework agent 不可用（可能仍在安装，或没有适合它的构建），暂时无法浏览它的文件；稍后再试",
            "The Mework agent is not available on this machine (it may still be installing, or there \
             is no build for it), so its files cannot be browsed yet; try again later"
        ));
    };
    let body = serde_json::to_vec(request).map_err(|error| {
        text!(
            "无法编码文件请求：{error}",
            "Could not encode the file request: {error}"
        )
    })?;
    let output = crate::remote_link::run_script_on(
        &link,
        runner,
        vec![SELF_PROGRAM.to_owned(), "files".to_owned()],
        Some(&body),
        timeout,
        &CancelSignal::default(),
    )?;
    if output.status != Some(0) {
        return Err(
            match run_environment::legible_remote_reply(&output.stderr) {
                Some(reply) => text!(
                    "远端文件助手失败：{reply}",
                    "The remote file helper failed: {reply}"
                ),
                None => text!(
                    "远端文件助手失败（退出码 {:?}）",
                    "The remote file helper failed (exit code {:?})",
                    output.status
                ),
            },
        );
    }
    serde_json::from_slice(&output.stdout).map_err(|error| {
        text!(
            "远端文件助手的回答无法解析：{error}",
            "Could not parse the remote file helper's reply: {error}"
        )
    })
}

pub(crate) fn list(machine: &Machine, path: String) -> Result<service::Listing, String> {
    call(
        machine,
        FilesOp::List { path },
        REQUEST_TIMEOUT,
        LINK_PATIENCE,
    )
}

pub(crate) fn read_text(machine: &Machine, path: String) -> Result<service::TextFile, String> {
    call(
        machine,
        FilesOp::ReadText { path },
        REQUEST_TIMEOUT,
        LINK_PATIENCE,
    )
}

pub(crate) fn read_bytes(machine: &Machine, path: String) -> Result<service::FileBytes, String> {
    call(
        machine,
        FilesOp::ReadBytes { path },
        REQUEST_TIMEOUT,
        LINK_PATIENCE,
    )
}

pub(crate) fn search(
    machine: &Machine,
    root: String,
    query: String,
    limit: usize,
) -> Result<service::SearchResults, String> {
    call(
        machine,
        FilesOp::Search { root, query, limit },
        REQUEST_TIMEOUT,
        LINK_PATIENCE,
    )
}

pub(crate) fn rename(
    machine: &Machine,
    path: String,
    name: String,
) -> Result<service::Changed, String> {
    call(
        machine,
        FilesOp::Rename { path, name },
        REQUEST_TIMEOUT,
        LINK_PATIENCE,
    )
}

pub(crate) fn create_directory(
    machine: &Machine,
    parent: String,
    name: String,
) -> Result<service::Changed, String> {
    call(
        machine,
        FilesOp::CreateDirectory { parent, name },
        REQUEST_TIMEOUT,
        LINK_PATIENCE,
    )
}

/// What a delete did.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Deleted {
    /// True when the item went to the Trash and can be put back from there.
    pub trashed: bool,
}

/// Deletes `path`: to the Trash on this computer, for good on another machine.
pub(crate) fn delete(machine: &Machine, path: String) -> Result<Deleted, String> {
    match machine {
        Machine::Local => {
            let local = local_deletable(&path)?;
            move_to_trash(&local)?;
            Ok(Deleted { trashed: true })
        }
        Machine::Remote(_) => {
            call::<serde_json::Value>(
                machine,
                FilesOp::Remove { path },
                REQUEST_TIMEOUT,
                LINK_PATIENCE,
            )?;
            Ok(Deleted { trashed: false })
        }
    }
}

/// `path` on this computer, normalized the way the browser spells it, refused
/// when it is a root, a drive, the home directory or not there at all.
fn local_deletable(path: &str) -> Result<String, String> {
    let windows = cfg!(windows);
    let home = home_directory();
    let place = service::normalize(path, windows, home.as_deref(), english())?;
    let Place::Path(normalized) = &place else {
        return Err(text!(
            "不能删除驱动器列表",
            "The list of drives cannot be deleted"
        ));
    };
    let parent = service::parent_of(&place, windows);
    if parent.is_none()
        || (windows && parent.as_deref() == Some("/"))
        || home.as_deref() == Some(normalized.as_str())
    {
        return Err(text!(
            "不能删除 {normalized}",
            "{normalized} cannot be deleted"
        ));
    }
    // A link is trashed as itself, never as what it leads to.
    std::fs::symlink_metadata(normalized).map_err(|error| {
        text!(
            "无法访问 {normalized}：{error}",
            "Could not access {normalized}: {error}"
        )
    })?;
    Ok(normalized.clone())
}

fn home_directory() -> Option<String> {
    let windows = cfg!(windows);
    std::env::var_os(if windows { "USERPROFILE" } else { "HOME" })
        .filter(|home| !home.is_empty())
        .map(|home| {
            let home = home.to_string_lossy().into_owned();
            if windows {
                home.replace('\\', "/")
            } else {
                home
            }
        })
}

#[cfg(target_os = "macos")]
fn move_to_trash(path: &str) -> Result<(), String> {
    use objc2_foundation::{NSFileManager, NSString, NSURL};

    let url = NSURL::fileURLWithPath(&NSString::from_str(path));
    NSFileManager::defaultManager()
        .trashItemAtURL_resultingItemURL_error(&url, None)
        .map_err(|error| {
            let reason = error.localizedDescription().to_string();
            text!(
                "无法把 {path} 移到废纸篓：{reason}",
                "Could not move {path} to the Trash: {reason}"
            )
        })
}

#[cfg(windows)]
fn move_to_trash(path: &str) -> Result<(), String> {
    use windows_sys::Win32::UI::Shell::{
        SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FO_DELETE,
        SHFILEOPSTRUCTW,
    };

    // The shell wants backslashes and a list ended by two NULs.
    let mut from: Vec<u16> = path.replace('/', "\\").encode_utf16().collect();
    from.extend([0, 0]);
    let mut operation = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: from.as_ptr(),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT) as u16,
        ..Default::default()
    };
    // SAFETY: `from` is double-NUL-terminated and outlives the call; the
    // structure's other pointers are null, which the call documents as unused.
    let status = unsafe { SHFileOperationW(&mut operation) };
    if status != 0 || operation.fAnyOperationsAborted != 0 {
        return Err(text!(
            "无法把 {path} 移到回收站（错误 {status:#x}）",
            "Could not move {path} to the Recycle Bin (error {status:#x})"
        ));
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", windows)))]
fn move_to_trash(path: &str) -> Result<(), String> {
    // The freedesktop Trash, through the tool every GLib desktop ships.
    let output = std::process::Command::new("gio")
        .arg("trash")
        .arg("--")
        .arg(path)
        .output()
        .map_err(|error| {
            text!(
                "这台电脑没有可用的回收站（gio：{error}）",
                "This computer has no Trash to move to (gio: {error})"
            )
        })?;
    if !output.status.success() {
        let reason = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(text!(
            "无法把 {path} 移到回收站：{reason}",
            "Could not move {path} to the Trash: {reason}"
        ));
    }
    Ok(())
}

/// One path a probe asks about.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProbeTarget {
    #[serde(default)]
    pub machine: Option<RunTarget>,
    pub path: String,
}

/// What a probe found at one target.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProbeResult {
    /// The path normalized on its machine.
    pub path: String,
    pub kind: Option<EntryKind>,
    /// False when the machine could not be asked in time; `kind` is then unknown.
    pub reached: bool,
}

/// The most targets one probe may carry: a path in a conversation with the
/// most workspaces it can have, twice over.
const MAX_PROBE_TARGETS: usize = 64;

/// What a probe hears from one machine's threads, by the machine's place in
/// the probe.
enum Heard {
    Answer(usize, Result<service::Stats, String>),
    /// Whether the machine said it is there when asked.
    Checked(usize, bool),
}

/// Asks every machine named in `targets` what is at its paths, all machines at
/// once and each machine in one request, answering in the order asked.
///
/// `patient` waits for a machine's link the way a listing does: the reader
/// typed the address and is waiting for it, so a first connection is worth it.
/// Neither kind waits on a machine that is gone — switched off, or never
/// there: its targets come back unreached as soon as it fails to say it is
/// there (see [`crate::remote_link::machine_is_there`]).
pub(crate) fn probe(
    assets: &ExecutionEnvironmentAssets,
    targets: Vec<ProbeTarget>,
    patient: bool,
) -> Result<Vec<ProbeResult>, String> {
    let (timeout, patience) = if patient {
        (REQUEST_TIMEOUT, LINK_PATIENCE)
    } else {
        (PROBE_TIMEOUT, PROBE_PATIENCE)
    };
    if targets.len() > MAX_PROBE_TARGETS {
        return Err(text!(
            "一次最多查询 {MAX_PROBE_TARGETS} 个路径",
            "At most {MAX_PROBE_TARGETS} paths per probe"
        ));
    }
    let mut groups: Vec<(Option<RunTarget>, Vec<usize>)> = Vec::new();
    for (index, target) in targets.iter().enumerate() {
        let key = run_environment::env_key(target.machine.as_ref());
        match groups
            .iter_mut()
            .find(|(machine, _)| run_environment::env_key(machine.as_ref()) == key)
        {
            Some((_, indices)) => indices.push(index),
            None => groups.push((target.machine.clone(), vec![index])),
        }
    }
    let mut results: Vec<ProbeResult> = targets
        .iter()
        .map(|target| ProbeResult {
            path: target.path.clone(),
            kind: None,
            reached: false,
        })
        .collect();
    // Each machine is asked on its own thread, and none is waited on once it
    // is known to be gone: a machine still silent after `PROBE_CHECK_AFTER` is
    // asked whether it is there at all, and left out unless it says so in
    // time. The threads of the ones left out end on their own bounds.
    let (sender, heard) = mpsc::channel::<Heard>();
    let mut waiting: HashMap<usize, Machine> = HashMap::new();
    for (group, (machine, indices)) in groups.iter().enumerate() {
        // A machine that is not in the catalog any more is not asked.
        let Ok(machine) = Machine::resolve(assets, machine.as_ref()) else {
            continue;
        };
        let paths: Vec<String> = indices
            .iter()
            .map(|&index| targets[index].path.clone())
            .collect();
        let asked = machine.clone();
        let sender = sender.clone();
        std::thread::spawn(move || {
            let answer = call::<service::Stats>(&asked, FilesOp::Stat { paths }, timeout, patience);
            let _ = sender.send(Heard::Answer(group, answer));
        });
        waiting.insert(group, machine);
    }
    let started = Instant::now();
    let check_at = started + PROBE_CHECK_AFTER;
    let checked_by = check_at + PROBE_CHECK_WITHIN;
    let deadline = started + patience + timeout;
    let mut checking = false;
    let mut there: HashSet<usize> = HashSet::new();
    while !waiting.is_empty() {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        if !checking && now >= check_at {
            checking = true;
            for (&group, machine) in &waiting {
                let Machine::Remote(runner) = machine else {
                    there.insert(group);
                    continue;
                };
                let runner = runner.clone();
                let sender = sender.clone();
                std::thread::spawn(move || {
                    let answers = crate::remote_link::machine_is_there(&runner, PROBE_CHECK_WITHIN);
                    let _ = sender.send(Heard::Checked(group, answers));
                });
            }
        }
        if checking && now >= checked_by {
            waiting.retain(|group, _| there.contains(group));
            if waiting.is_empty() {
                break;
            }
        }
        let next = if !checking {
            check_at
        } else if now < checked_by {
            checked_by
        } else {
            deadline
        };
        match heard.recv_timeout(next.min(deadline) - now) {
            Ok(Heard::Answer(group, answer)) => {
                waiting.remove(&group);
                // A machine that could not be asked leaves its targets
                // unreached rather than failing the whole probe: the others
                // still say where the path is.
                let Ok(stats) = answer else { continue };
                for (&index, stat) in groups[group].1.iter().zip(stats.stats) {
                    results[index] = ProbeResult {
                        path: stat.path,
                        kind: stat.kind,
                        reached: true,
                    };
                }
            }
            Ok(Heard::Checked(group, true)) => {
                there.insert(group);
            }
            Ok(Heard::Checked(group, false)) => {
                waiting.remove(&group);
            }
            Err(_) => {}
        }
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_requests_are_answered_in_process() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().to_string_lossy().replace('\\', "/");
        std::fs::write(directory.path().join("note.md"), "# hi\n").expect("write");
        let listing = list(&Machine::Local, root.clone()).expect("listing");
        assert_eq!(listing.entries.len(), 1);
        assert_eq!(listing.entries[0].path, format!("{root}/note.md"));
        let text = read_text(&Machine::Local, format!("{root}/note.md")).expect("text");
        assert_eq!(text.content, "# hi\n");
    }

    #[test]
    fn a_probe_answers_in_the_order_asked() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().to_string_lossy().replace('\\', "/");
        std::fs::create_dir(directory.path().join("src")).expect("mkdir");
        let results = probe(
            &ExecutionEnvironmentAssets::default(),
            vec![
                ProbeTarget {
                    machine: None,
                    path: format!("{root}/missing"),
                },
                ProbeTarget {
                    machine: None,
                    path: format!("{root}/src"),
                },
            ],
            false,
        )
        .expect("probe");
        assert_eq!(
            results
                .iter()
                .map(|result| (result.kind, result.reached))
                .collect::<Vec<_>>(),
            [(None, true), (Some(EntryKind::Directory), true)]
        );
    }

    #[test]
    fn a_probe_of_an_unregistered_machine_leaves_it_unreached() {
        let results = probe(
            &ExecutionEnvironmentAssets::default(),
            vec![ProbeTarget {
                machine: Some(RunTarget::Ssh {
                    machine_id: "gone".into(),
                }),
                path: "/srv".into(),
            }],
            false,
        )
        .expect("probe");
        assert_eq!(
            results[0],
            ProbeResult {
                path: "/srv".into(),
                kind: None,
                reached: false
            }
        );
    }

    /// Against real machines over SSH: set `MEWORK_E2E_SSH_HOST` to one that is
    /// up and `MEWORK_E2E_SSH_OFFLINE_HOST` to a registered one that is powered
    /// off, and run with `--ignored`.
    #[test]
    #[ignore]
    fn over_real_ssh_a_probe_does_not_wait_on_a_machine_that_is_gone() {
        let online = std::env::var("MEWORK_E2E_SSH_HOST").expect("MEWORK_E2E_SSH_HOST");
        let offline =
            std::env::var("MEWORK_E2E_SSH_OFFLINE_HOST").expect("MEWORK_E2E_SSH_OFFLINE_HOST");
        let app_data = tempfile::tempdir().expect("temporary directory");
        crate::remote_link::install(app_data.path(), Vec::new(), None);
        let machine = |id: &str, host: &str| crate::model::SshMachineConfig {
            id: id.into(),
            name: id.into(),
            host: host.into(),
            port: 0,
            identity_file: String::new(),
            agent_shell: None,
            created_at: String::new(),
            updated_at: String::new(),
        };
        let assets = ExecutionEnvironmentAssets {
            ssh_machines: vec![
                machine("online", &online),
                machine("offline", &offline),
                // TEST-NET-3: routed nowhere, so a connection neither succeeds nor is refused.
                machine("nowhere", "203.0.113.1"),
            ],
            ..ExecutionEnvironmentAssets::default()
        };
        let target = |id: &str| ProbeTarget {
            machine: Some(RunTarget::Ssh {
                machine_id: id.into(),
            }),
            path: "~".into(),
        };
        let runner = |id: &str| match Machine::resolve(&assets, target(id).machine.as_ref()) {
            Ok(Machine::Remote(runner)) => runner,
            _ => panic!("{id} is a registered SSH machine"),
        };
        // Before any link: whatever accepts a connection where ssh makes one.
        let there =
            |id: &str| crate::remote_link::machine_is_there(&runner(id), PROBE_CHECK_WITHIN);
        assert_eq!(
            (there("online"), there("offline"), there("nowhere")),
            (true, false, false)
        );

        // The way a reader's first look at the machine connects it.
        let warm = probe(&assets, vec![target("online")], true).expect("probe");
        // A link that is up: its agent answers.
        assert!(there("online"));
        assert!(warm[0].reached, "{warm:?}");

        let started = Instant::now();
        let results = probe(
            &assets,
            vec![target("online"), target("offline"), target("nowhere")],
            false,
        )
        .expect("probe");
        let elapsed = started.elapsed();
        eprintln!("online, offline and nowhere took {elapsed:?}: {results:?}");
        assert_eq!(
            results
                .iter()
                .map(|result| result.reached)
                .collect::<Vec<_>>(),
            [true, false, false]
        );
        let gone_elapsed = elapsed;

        // A machine switched off while connected: its link still says it is up,
        // and nothing it was sent is answered. A stopped ssh is exactly that.
        let pid = std::process::id().to_string();
        let stopped = std::process::Command::new("pkill")
            .args(["-STOP", "-P", &pid, "-x", "ssh"])
            .status()
            .expect("pkill");
        assert!(stopped.success(), "the link's ssh was running");
        let started = Instant::now();
        let results = probe(&assets, vec![target("online")], false).expect("probe");
        let elapsed = started.elapsed();
        let _ = std::process::Command::new("pkill")
            .args(["-CONT", "-P", &pid, "-x", "ssh"])
            .status();
        eprintln!("a stalled link took {elapsed:?}: {results:?}");
        assert!(!results[0].reached, "{results:?}");
        crate::remote_link::shutdown();
        let bound = PROBE_CHECK_AFTER + PROBE_CHECK_WITHIN + Duration::from_millis(500);
        assert!(gone_elapsed < bound, "{gone_elapsed:?}");
        assert!(elapsed < bound, "{elapsed:?}");
    }

    #[test]
    fn the_roots_and_the_home_are_never_deleted() {
        assert!(local_deletable("/").is_err());
        if let Some(home) = home_directory() {
            assert!(local_deletable(&home).is_err());
        }
        assert!(local_deletable("relative/path").is_err());
    }
}
