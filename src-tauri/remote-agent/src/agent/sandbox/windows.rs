//! Windows: the cell runs through srt-win, the Windows backend of Anthropic's
//! sandbox-runtime (vendored under `src-tauri/vendor/srt-win`, Apache-2.0),
//! shipped beside the agent as `srt-win.exe`.
//!
//! Windows has no unprivileged sandbox that can both fence the network and
//! keep a process out of the account's files, so srt-win uses another
//! account: a hidden local user, created once with administrator rights
//! (`srt-win install`, one UAC prompt), whose processes it starts under a
//! restricted token — administrators and the logon session disabled, no
//! privileges, medium integrity — in a kill-on-close job on a private
//! desktop. Windows Filtering Platform filters keyed on that account's SID
//! block every connection it makes except to a few loopback ports, where the
//! agent runs the proxy that applies the network policy. Which ports is
//! chosen at install — 60080–60089 unless the installing program said
//! otherwise — so the agent asks srt-win to find them by trying them as that
//! account (`wfp ports`) rather than assume them.
//!
//! Files follow from the account: it cannot read the user's profile at all,
//! so credentials are out of reach without a rule of their own. The agent
//! grants it what the cell needs — the workspaces and the cell's own
//! directories to write, the agent and the user's tool directories on `PATH`
//! to read — and denies, per cell, what in the workspaces runs outside the
//! sandbox later.
//!
//! srt-win's install state is machine-wide and shared with any other program
//! that uses it (Claude Code does): an existing install is reused, whoever
//! made it, and Mework never uninstalls one.

use std::io::Write;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

/// The helper's file name, beside the agent's own executable.
pub const HELPER: &str = "srt-win.exe";

/// An installed, working srt-win.
#[derive(Clone, Debug)]
pub struct Helper {
    pub exe: PathBuf,
    /// The sandbox account's SID.
    pub sid: String,
    /// The loopback ports the network fence lets the sandbox account reach,
    /// where its proxy must listen.
    pub ports: RangeInclusive<u16>,
}

/// The helper beside the running agent.
pub fn find_helper() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let helper = exe.parent()?.join(HELPER);
    helper.is_file().then_some(helper)
}

fn missing_helper() -> String {
    let directory = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|directory| directory.display().to_string()))
        .unwrap_or_default();
    format!(
        "{HELPER}, the Windows sandbox, is not beside Mework's agent ({directory}); Mework installs the two together, and a development build of Mework gets it from `npm run build:remote-agents`"
    )
}

/// Why the Windows sandbox cannot run here.
#[derive(Clone, Debug)]
pub struct Unavailable {
    pub detail: String,
    /// What it lacks is the one-time setup, which would make it available.
    pub setup: bool,
}

impl From<String> for Unavailable {
    fn from(detail: String) -> Self {
        Self { detail, setup: false }
    }
}

/// `what` the setup would fix, and how to run it: from Mework's settings on
/// the computer Mework runs on, or on a machine it reaches over SSH, from an
/// administrator's shell there — a UAC prompt cannot reach anyone over SSH.
fn needs_setup(what: &str) -> Unavailable {
    let agent = std::env::current_exe()
        .map(|exe| exe.display().to_string())
        .unwrap_or_else(|_| "mework-remote.exe".into());
    Unavailable {
        detail: format!(
            "{what}. The setup needs administrator rights: Mework's sandbox settings start it for this computer; on a machine reached over SSH, run `\"{agent}\" sandbox-setup` there as an administrator"
        ),
        setup: true,
    }
}

fn command(exe: &Path) -> Command {
    let mut command = Command::new(exe);
    command.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// Checks that srt-win is installed and its network fence is in effect, and
/// finds the ports the fence leaves open. Remembered once it is; asked again
/// while it is not, so a setup made meanwhile is noticed.
pub fn probe() -> Result<Helper, Unavailable> {
    static READY: Mutex<Option<Helper>> = Mutex::new(None);
    if let Some(helper) = READY.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        return Ok(helper);
    }
    let exe = find_helper().ok_or_else(missing_helper)?;
    let output = command(&exe)
        .arg("status")
        .output()
        .map_err(|error| format!("Cannot run {HELPER}: {error}"))?;
    let status: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|_| format!("{HELPER} status answered: {}", String::from_utf8_lossy(&output.stderr).trim()))?;
    let user = &status["user"];
    let provisioned = user["user"]["exists"].as_bool() == Some(true) && user["cred_present"].as_bool() == Some(true);
    let Some(sid) = user["marker_user_sid"].as_str().filter(|sid| provisioned && sid.starts_with("S-1-")) else {
        return Err(needs_setup(NEEDS_SETUP));
    };
    let sid = sid.to_owned();
    // srt-win starts itself as the sandbox account before it starts
    // anything else, and the agent's directory — its own and the cell's
    // executable — is in the user's profile, which that account cannot
    // read unless told it may.
    let directory = exe
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("{HELPER} has no directory"))?;
    grant_as(&exe, &sid, std::slice::from_ref(&directory), &[])?;
    // The fence is checked by what it does: the sandbox account tries
    // loopback ports, and must be refused all but the few the install
    // chose. A non-elevated caller cannot read the filters themselves.
    let found = command(&exe)
        .args(["wfp", "ports"])
        .current_dir(&directory)
        .output()
        .map_err(|error| format!("Cannot run {HELPER}: {error}"))?;
    let ports = match found.status.code() {
        Some(0) => permitted_ports(&found.stdout)
            .ok_or_else(|| format!("{HELPER} wfp ports answered: {}", String::from_utf8_lossy(&found.stdout).trim()))?,
        Some(15) => return Err(needs_setup(NEEDS_SETUP)),
        Some(3) => return Err(needs_setup(FENCE_MISSING)),
        Some(4) => return Err(NO_PORTS.to_owned().into()),
        code => {
            let said = String::from_utf8_lossy(&found.stderr);
            // Windows starts another account's process for a desktop
            // session only; an SSH session is refused.
            if said.contains("CreateProcessWithLogonW") && said.contains("0x80070005") {
                return Err(OUTSIDE_DESKTOP.to_owned().into());
            }
            return Err(format!(
                "Cannot confirm the Windows sandbox's network fence (exit {code:?}): {}",
                said.trim()
            )
            .into());
        }
    };
    let helper = Helper { exe, sid, ports };
    *READY.lock().unwrap_or_else(|e| e.into_inner()) = Some(helper.clone());
    Ok(helper)
}

pub const OUTSIDE_DESKTOP: &str =
    "The Windows sandbox runs only from a desktop session: Windows refuses to start the sandbox account from an SSH session, so sandboxed commands cannot run on this machine over SSH";

const NEEDS_SETUP: &str =
    "The Windows sandbox needs a one-time setup, which creates a hidden local account for sandboxed commands";

const FENCE_MISSING: &str = "The Windows sandbox's network fence is not in effect, and the setup has to be run again";

const NO_PORTS: &str = "The Windows sandbox's network fence lets the sandbox account reach no loopback port, so sandboxed commands could not reach Mework's proxy; the program that set it up chose that";

/// The `{"permitted":[low,high]}` `wfp ports` prints.
fn permitted_ports(stdout: &[u8]) -> Option<RangeInclusive<u16>> {
    let answer: serde_json::Value = serde_json::from_slice(stdout).ok()?;
    let range = answer["permitted"].as_array()?;
    let port = |index: usize| range.get(index)?.as_u64().and_then(|port| u16::try_from(port).ok()).filter(|port| *port > 0);
    let (low, high) = (port(0)?, port(1)?);
    (low <= high).then_some(low..=high)
}

/// Lets the sandbox account read `read` and change `write`, for as long as
/// this agent runs: srt-win counts grants by the process that holds them and
/// removes them when it is gone.
///
/// Only ever the directories themselves: srt-win writes an ACL through
/// `SetNamedSecurityInfo`, which walks the directory's whole subtree, so a
/// grant on a user's profile would rewrite every file in it.
pub fn grant(helper: &Helper, read: &[PathBuf], write: &[PathBuf]) -> Result<(), String> {
    grant_as(&helper.exe, &helper.sid, read, write)
}

fn grant_as(exe: &Path, sid: &str, read: &[PathBuf], write: &[PathBuf]) -> Result<(), String> {
    let text = |paths: &[PathBuf]| paths.iter().map(|path| path.to_string_lossy().into_owned()).collect::<Vec<_>>();
    let input = serde_json::json!({ "read": text(read), "write": text(write) });
    let mut child = command(exe)
        .args([
            "acl",
            "grant",
            "--holder-pid",
            &std::process::id().to_string(),
            "--sandbox-user-sid",
            sid,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("Cannot run {HELPER}: {error}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.to_string().as_bytes());
    }
    let output = child.wait_with_output().map_err(|error| error.to_string())?;
    // 2: some paths were skipped (a missing one); the rest were granted.
    if !matches!(output.status.code(), Some(0 | 2)) {
        return Err(format!(
            "The sandbox account could not be given the workspaces: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

/// Takes back every grant this agent made. Called when it leaves; srt-win
/// also cleans up after an agent that died without it.
pub fn revoke(helper: &Helper) {
    let _ = command(&helper.exe)
        .args([
            "acl",
            "revoke",
            "--holder-pid",
            &std::process::id().to_string(),
            "--sandbox-user-sid",
            &helper.sid,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// What `srt-win exec` is told besides the program.
#[derive(Clone, Debug, Default)]
pub struct Confinement {
    pub deny_read: Vec<PathBuf>,
    /// A trailing separator makes srt-win create a missing path as a
    /// directory placeholder rather than a file.
    pub deny_write: Vec<String>,
    pub deny_delete: Vec<PathBuf>,
    pub env: Vec<(String, String)>,
}

/// The `srt-win exec` invocation that runs `program` as the sandbox account,
/// with its standard input and output kept open for the protocol.
pub fn exec_argv(helper: &Helper, confinement: &Confinement, program: &[String]) -> Vec<String> {
    let mut argv = vec![
        helper.exe.to_string_lossy().into_owned(),
        "exec".into(),
        "--quiet".into(),
        "--stdin".into(),
    ];
    for path in &confinement.deny_read {
        argv.push("--deny-read".into());
        argv.push(path.to_string_lossy().into_owned());
    }
    for path in &confinement.deny_write {
        argv.push("--deny-write".into());
        argv.push(path.clone());
    }
    for path in &confinement.deny_delete {
        argv.push("--deny-delete".into());
        argv.push(path.to_string_lossy().into_owned());
    }
    for (name, value) in &confinement.env {
        argv.push("--env".into());
        argv.push(format!("{name}={value}"));
    }
    argv.push("--".into());
    argv.extend(program.iter().cloned());
    argv
}

/// Provisions the sandbox account and the network fence, unless some program
/// already did. srt-win asks for elevation itself (one UAC prompt); over SSH,
/// where there is nobody to click it, the session must already be elevated.
pub fn setup() -> Result<String, String> {
    if probe().is_ok() {
        return Ok("The Windows sandbox is already set up".into());
    }
    let exe = find_helper().ok_or_else(missing_helper)?;
    let output = command(&exe)
        .arg("install")
        .output()
        .map_err(|error| format!("Cannot run {HELPER}: {error}"))?;
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    match output.status.code() {
        Some(0) => probe()
            .map(|_| "The Windows sandbox is set up".into())
            .map_err(|unavailable| unavailable.detail),
        Some(10) => Err("The setup was cancelled at the administrator prompt".into()),
        Some(13) => {
            // Another program installed srt-win with other settings; its
            // install serves Mework as well.
            probe()
                .map(|_| "The Windows sandbox another program set up is in use".into())
                .map_err(|unavailable| unavailable.detail)
        }
        code => Err(format!("The Windows sandbox setup failed (exit {code:?}): {}", said.trim())),
    }
}
