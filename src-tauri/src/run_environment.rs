//! Execution environments: where a conversation's shell commands run.
//!
//! A target is local, a dynamically enumerated WSL distribution, or a catalogued SSH
//! machine. WSL output may be UTF-8 or UTF-16LE even with `WSL_UTF8=1`.
//!
//! This module resolves persisted [`RunTarget`] values into [`ShellRunner`] instances,
//! enumerates WSL distributions, and provides safe remote command construction.
//! Process creation remains in `tool_executor::spawn_shell_process`.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde::Serialize;
use sha2::{Digest, Sha256};
use wait_timeout::ChildExt;

use crate::model::{ExecutionEnvironmentAssets, RunTarget};

/// Trusted shell environment resolved by the host. Only [`resolve_shell_runner`] may
/// construct it from persisted data; neither renderer nor model input may do so.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellRunner {
    Local {
        env: BTreeMap<String, String>,
    },
    Wsl {
        distro: String,
        env: BTreeMap<String, String>,
    },
    Ssh {
        /// `user@hostname`, `hostname`, or a `~/.ssh/config` Host alias.
        host: String,
        /// Zero uses the default port, 22.
        port: u16,
        /// Empty delegates to OpenSSH's default resolution.
        identity_file: String,
        /// Empty uses the remote user's home directory; `~` prefixes are supported.
        remote_cwd: String,
        env: BTreeMap<String, String>,
    },
}

impl Default for ShellRunner {
    fn default() -> Self {
        Self::Local {
            env: BTreeMap::new(),
        }
    }
}

impl ShellRunner {
    /// Target configuration only: Windows host inheritance must never leak into WSL/SSH.
    pub fn normalized_env(&self) -> Result<BTreeMap<String, String>, String> {
        let proxy = crate::child_environment::normalized_proxy_bypass(
            &BTreeMap::new(), self.env(), cfg!(windows) && matches!(self, Self::Local { .. }),
        )?;
        let mut env = self.env().clone();
        env.retain(|name, _| !name.eq_ignore_ascii_case("NO_PROXY"));
        env.extend(proxy);
        Ok(env)
    }

    pub fn env(&self) -> &BTreeMap<String, String> {
        match self {
            Self::Local { env } | Self::Wsl { env, .. } | Self::Ssh { env, .. } => env,
        }
    }

    /// Approval fingerprint over environment identity and variables. A manual-execution
    /// nonce must expire when either changes, preventing local approval from running
    /// the command on a different target.
    pub fn fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        match self {
            Self::Local { .. } => hasher.update(b"local"),
            Self::Wsl { distro, .. } => {
                hasher.update(b"wsl:");
                hasher.update(distro.as_bytes());
            }
            Self::Ssh {
                host,
                port,
                identity_file,
                remote_cwd,
                ..
            } => {
                hasher.update(b"ssh:");
                hasher.update(host.as_bytes());
                hasher.update([0]);
                hasher.update(port.to_le_bytes());
                hasher.update(identity_file.as_bytes());
                hasher.update([0]);
                hasher.update(remote_cwd.as_bytes());
            }
        }
        for (key, value) in self.env() {
            hasher.update([0]);
            hasher.update(key.as_bytes());
            hasher.update([1]);
            hasher.update(value.as_bytes());
        }
        format!("{:x}", hasher.finalize())
    }
}

/// Key for the environment-variable map in [`ExecutionEnvironmentAssets::env_vars`].
pub fn env_key(target: Option<&RunTarget>) -> String {
    match target {
        None => "local".into(),
        Some(RunTarget::Wsl { distro }) => format!("wsl:{distro}"),
        Some(RunTarget::Ssh { machine_id }) => format!("ssh:{machine_id}"),
    }
}

/// Resolves a persisted conversation target to a trusted dispatch environment.
/// A missing SSH catalog entry must fail explicitly rather than silently falling back
/// to local execution, which would run remote-intended commands on the wrong machine.
pub fn resolve_shell_runner(
    assets: &ExecutionEnvironmentAssets,
    target: Option<&RunTarget>,
) -> Result<ShellRunner, String> {
    let env = assets
        .env_vars
        .get(&env_key(target))
        .cloned()
        .unwrap_or_default();
    match target {
        None => Ok(ShellRunner::Local { env }),
        Some(RunTarget::Wsl { distro }) => {
            validate_wsl_distro_name(distro)?;
            Ok(ShellRunner::Wsl {
                distro: distro.clone(),
                env,
            })
        }
        Some(RunTarget::Ssh { machine_id }) => {
            let machine = assets
                .ssh_machines
                .iter()
                .find(|machine| machine.id == *machine_id)
                .ok_or_else(|| {
                    format!(
                        "The SSH machine ({machine_id}) bound to this conversation is no longer in the machine catalog; select an execution location again"
                    )
                })?;
            if machine.host.trim().is_empty() {
                return Err(format!("SSH machine {} has an empty host address", machine.name));
            }
            Ok(ShellRunner::Ssh {
                host: machine.host.clone(),
                port: machine.port,
                identity_file: machine.identity_file.clone(),
                remote_cwd: machine.remote_cwd.clone(),
                env,
            })
        }
    }
}

/// Validates WSL distribution names: alphanumeric leading character, then only
/// alphanumeric characters or `._ -`, no trailing space, and at most 64 characters.
/// This excludes path separators, NUL, and argument-injection forms.
pub fn validate_wsl_distro_name(name: &str) -> Result<(), String> {
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    let pattern = PATTERN.get_or_init(|| {
        regex::Regex::new(r"^[\p{L}\p{N}](?:[\p{L}\p{N}._ -]{0,62}[\p{L}\p{N}._-])?$")
            .expect("distro name pattern compiles")
    });
    if pattern.is_match(name) {
        Ok(())
    } else {
        Err(format!("Invalid WSL distribution name: {name:?}"))
    }
}

/// Startup-pollution variables stripped by remote wrappers. A configured `BASH_ENV`
/// could run an unapproved script before every remote command, so storage rejects
/// these names and command construction removes them again for legacy data.
pub fn is_shell_startup_env_name(name: &str) -> bool {
    matches!(
        name,
        "BASH_ENV" | "ENV" | "SHELLOPTS" | "BASHOPTS" | "CDPATH" | "GLOBIGNORE"
            | "GIT_EXTERNAL_DIFF"
    )
}

/// Validates POSIX-shaped environment variable names up to 128 characters.
pub fn validate_env_var_name(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let valid_head = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    let valid_tail = name.chars().skip(1).all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !valid_head || !valid_tail || name.len() > 128 {
        return Err(format!("Invalid environment variable name: {name:?}"));
    }
    Ok(())
}

/// POSIX single-quote escaping for all host-supplied SSH command fragments.
pub fn sh_single_quote(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('\'');
    for c in text.chars() {
        if c == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(c);
        }
    }
    quoted.push('\'');
    quoted
}

/// Quotes a remote working directory while leaving a `~` prefix unquoted for expansion.
fn quote_remote_cwd(cwd: &str) -> String {
    if cwd == "~" {
        return "~".into();
    }
    if let Some(rest) = cwd.strip_prefix("~/") {
        return format!("~/{}", sh_single_quote(rest));
    }
    sh_single_quote(cwd)
}

/// Builds `wsl.exe` arguments. `--cd` accepts an absolute Windows path and applies
/// distribution automount rules; environment variables are argv entries and the
/// command is the single `bash -c` argument passed unchanged through `--exec`.
pub fn wsl_shell_args(
    distro: &str,
    workspace: &Path,
    env: &BTreeMap<String, String>,
    command: &str,
) -> Vec<String> {
    let mut args = vec![
        "-d".into(),
        distro.to_owned(),
        "--cd".into(),
        workspace.to_string_lossy().into_owned(),
        "--exec".into(),
        "/usr/bin/env".into(),
    ];
    args.extend(
        env.iter()
            .filter(|(key, _)| !is_shell_startup_env_name(key))
            .map(|(key, value)| format!("{key}={value}")),
    );
    args.extend([
        "bash".into(),
        "--noprofile".into(),
        "--norc".into(),
        "-c".into(),
        command.to_owned(),
    ]);
    args
}

/// Builds OpenSSH arguments. The remote login shell parses one command string, so all
/// host-supplied fragments use [`sh_single_quote`]. `BatchMode=yes` fails explicitly
/// when credentials or host-key confirmation are unavailable.
pub fn ssh_shell_args(
    host: &str,
    port: u16,
    identity_file: &str,
    remote_cwd: &str,
    env: &BTreeMap<String, String>,
    command: &str,
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "ConnectTimeout=10".into(),
    ];
    if port != 0 {
        args.extend(["-p".into(), port.to_string()]);
    }
    if !identity_file.is_empty() {
        args.extend(["-i".into(), identity_file.to_owned()]);
    }
    args.push("--".into());
    args.push(host.to_owned());

    let mut remote = String::new();
    if !remote_cwd.is_empty() {
        remote.push_str(&format!("cd {} || exit 1; ", quote_remote_cwd(remote_cwd)));
    }
    remote.push_str("exec ");
    let injected: Vec<_> = env
        .iter()
        .filter(|(key, _)| !is_shell_startup_env_name(key))
        .collect();
    if !injected.is_empty() {
        remote.push_str("env ");
        for (key, value) in injected {
            remote.push_str(&sh_single_quote(&format!("{key}={value}")));
            remote.push(' ');
        }
    }
    remote.push_str(&format!(
        "bash --noprofile --norc -c {}",
        sh_single_quote(command)
    ));
    args.push(remote);
    args
}

/// SSH client executable candidates in priority order. On Windows, the system OpenSSH
/// binary is a fallback when PATH lacks a usable client.
pub fn ssh_client_candidates() -> Vec<String> {
    #[cfg(windows)]
    {
        vec![
            "ssh".into(),
            r"C:\Windows\System32\OpenSSH\ssh.exe".into(),
        ]
    }
    #[cfg(not(windows))]
    {
        vec!["ssh".into()]
    }
}

/// The Windows-shaped file names this resolver looks for. A Local run always
/// targets the Windows host, so the POSIX name is never the right answer here.
const BASH_EXECUTABLE: &str = "bash.exe";
const GIT_EXECUTABLE: &str = "git.exe";

/// Bash interpreters for a Local run, in preference order.
///
/// A bare `bash` is not good enough on Windows, and it fails in two opposite
/// directions. `%SystemRoot%\System32\bash.exe` is the WSL launcher: it would
/// run the command inside the default distribution, whose filesystem and
/// loopback namespace are not the ones this conversation targets, and it would
/// report success for a command that never touched the host — so the wrong
/// machine stays invisible in the transcript. Meanwhile a normal Git for Windows
/// install advertises only its `cmd` directory on `PATH`, which carries
/// `git.exe` but no `bash.exe`, so the bare name resolves to nothing at all and
/// the tool looks like it has no backend.
///
/// Local means this Windows host: the launcher is excluded and a native Bash is
/// resolved to an absolute path. WSL stays reachable by selecting it as the
/// conversation's run target, which is the only place its semantics are honest.
pub fn local_bash_candidates() -> Vec<String> {
    #[cfg(windows)]
    {
        select_local_bash(
            std::env::var_os("PATH").as_deref(),
            &windows_system_directories(),
            &well_known_bash_paths(),
            &|path: &Path| path.is_file(),
        )
        .map(|path| vec![path.to_string_lossy().into_owned()])
        .unwrap_or_default()
    }
    #[cfg(not(windows))]
    {
        vec!["bash".into()]
    }
}

/// Picks the first native Bash from `PATH`, then from a Git installation named
/// by `PATH`, then from well-known install locations. Directories inside a
/// Windows system directory are skipped, so the WSL launcher can never win.
///
/// Compiled on every platform so the rule stays unit-testable; only the Windows
/// branch above calls it.
#[cfg_attr(not(windows), allow(dead_code))]
fn select_local_bash(
    path_var: Option<&OsStr>,
    system_directories: &[PathBuf],
    well_known: &[PathBuf],
    is_file: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let directories: Vec<PathBuf> = path_var
        .map(|value| std::env::split_paths(value).collect())
        .unwrap_or_default();
    let usable: Vec<&PathBuf> = directories
        .iter()
        .filter(|directory| !directory.as_os_str().is_empty())
        .filter(|directory| {
            !system_directories
                .iter()
                .any(|system| path_is_inside(directory, system))
        })
        .collect();

    if let Some(found) = usable
        .iter()
        .map(|directory| directory.join(BASH_EXECUTABLE))
        .find(|candidate| is_file(candidate))
    {
        return Some(found);
    }
    // Git for Windows advertises only its `cmd` directory, and Bash sits beside
    // it inside the same installation. A resolvable `git.exe` therefore names a
    // usable Bash that `PATH` alone never mentions.
    usable
        .iter()
        .filter(|directory| is_file(&directory.join(GIT_EXECUTABLE)))
        .filter_map(|directory| directory.parent())
        .flat_map(|root| {
            [
                root.join("bin").join(BASH_EXECUTABLE),
                root.join("usr").join("bin").join(BASH_EXECUTABLE),
            ]
        })
        .find(|candidate| is_file(candidate))
        .or_else(|| {
            well_known
                .iter()
                .find(|candidate| is_file(candidate))
                .cloned()
        })
}

/// Case-insensitive containment for Windows paths, tolerant of either separator.
#[cfg_attr(not(windows), allow(dead_code))]
fn path_is_inside(directory: &Path, root: &Path) -> bool {
    let normalize = |path: &Path| {
        path.to_string_lossy()
            .to_ascii_lowercase()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_owned()
    };
    let directory = normalize(directory);
    let root = normalize(root);
    !root.is_empty() && (directory == root || directory.starts_with(&format!("{root}\\")))
}

/// The whole Windows directory is excluded rather than just `System32`: no
/// native Bash installs there, and the launcher has appeared under more than one
/// of its subdirectories across Windows releases.
#[cfg(windows)]
fn windows_system_directories() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = ["SystemRoot", "windir"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .collect();
    if roots.is_empty() {
        roots.push(PathBuf::from(r"C:\Windows"));
    }
    roots
}

/// Install locations to try once `PATH` has produced nothing.
#[cfg(windows)]
fn well_known_bash_paths() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    for variable in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"] {
        if let Some(value) = std::env::var_os(variable) {
            roots.push(PathBuf::from(value).join("Git"));
        }
    }
    if let Some(value) = std::env::var_os("LocalAppData") {
        roots.push(PathBuf::from(value).join("Programs").join("Git"));
    }
    let drive = std::env::var_os("SystemDrive")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:"));
    roots.push(drive.join("msys64"));
    roots.push(drive.join("msys32"));
    roots
        .into_iter()
        .flat_map(|root| {
            [
                root.join("bin").join(BASH_EXECUTABLE),
                root.join("usr").join("bin").join(BASH_EXECUTABLE),
            ]
        })
        .collect()
}

/// Local PowerShell interpreters, in Claude Code's exact preference order.
///
/// PowerShell 7 first and Windows PowerShell 5.1 last, because 5.1 is the one
/// that decodes BOM-less files with the ANSI code page. The three fixed paths
/// between them are the install locations `PATH` routinely fails to mention: the
/// MSI's own directory, the Microsoft Store alias, and a per-user `dotnet tool`
/// install.
///
/// A bare name is returned rather than an absolute path when `PATH` resolves it,
/// matching Claude Code; unlike Bash there is no launcher-shaped impostor on
/// Windows for a bare `pwsh` to hit.
pub fn local_powershell_candidates() -> Vec<String> {
    #[cfg(windows)]
    {
        select_local_powershell(&|path: &Path| path.is_file(), &|name: &str| {
            path_lookup(name).is_some()
        })
    }
    #[cfg(not(windows))]
    {
        vec!["pwsh".into()]
    }
}

/// Whether a bare executable name resolves on `PATH`. A hit inside the current
/// directory is ignored: resolving an interpreter out of the workspace would let
/// a checked-in `pwsh.exe` run instead of the real one.
#[cfg(windows)]
fn path_lookup(name: &str) -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok();
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .filter(|directory| !directory.as_os_str().is_empty())
        .filter(|directory| cwd.as_deref().is_none_or(|cwd| directory != cwd))
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}

/// Compiled on every platform so the order stays unit-testable; only the Windows
/// branch above calls it.
#[cfg_attr(not(windows), allow(dead_code))]
fn select_local_powershell(
    is_file: &dyn Fn(&Path) -> bool,
    on_path: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let mut candidates: Vec<String> = Vec::new();
    if on_path("pwsh") {
        candidates.push("pwsh".into());
    }
    let mut fixed: Vec<PathBuf> = Vec::new();
    for variable in ["ProgramFiles", "ProgramW6432"] {
        if let Some(value) = std::env::var_os(variable) {
            fixed.push(PathBuf::from(value).join("PowerShell").join("7").join("pwsh.exe"));
        }
    }
    if let Some(value) = std::env::var_os("LocalAppData") {
        fixed.push(
            PathBuf::from(value)
                .join("Microsoft")
                .join("WindowsApps")
                .join("pwsh.exe"),
        );
    }
    if let Some(value) = std::env::var_os("UserProfile") {
        fixed.push(PathBuf::from(value).join(".dotnet").join("tools").join("pwsh.exe"));
    }
    for candidate in fixed {
        if is_file(&candidate) {
            candidates.push(candidate.to_string_lossy().into_owned());
        }
    }
    if on_path("powershell") {
        candidates.push("powershell".into());
    }
    let system_root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let windows_powershell = system_root
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    if is_file(&windows_powershell) {
        candidates.push(windows_powershell.to_string_lossy().into_owned());
    }
    candidates.dedup();
    candidates
}

/// An installed WSL distribution from one `wsl.exe --list --verbose` row.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WslDistro {
    pub name: String,
    pub version: u8,
    pub is_default: bool,
}

/// Hard timeout for enumeration so a stuck WSL service cannot block IPC.
const WSL_LIST_TIMEOUT: Duration = Duration::from_secs(10);

/// Enumerates installed WSL distributions. Missing WSL, spawn failures, timeouts, and
/// non-zero exits yield an empty list; an unavailable distribution is normal. Non-Windows
/// platforms always return an empty list.
pub fn list_wsl_distros() -> Vec<WslDistro> {
    #[cfg(not(windows))]
    {
        Vec::new()
    }
    #[cfg(windows)]
    {
        let mut command = Command::new("wsl.exe");
        command
            .args(["--list", "--verbose"])
            .env("WSL_UTF8", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let Ok(mut child) = command.spawn() else {
            return Vec::new();
        };
        // Consume stdout while waiting: a large distribution table can fill the pipe
        // and block the child writer. The reader runs to EOF while this loop handles
        // timeouts and termination.
        let reader = child.stdout.take().map(|mut stdout| {
            std::thread::spawn(move || {
                use std::io::Read;
                let mut bytes = Vec::new();
                let _ = stdout.read_to_end(&mut bytes);
                bytes
            })
        });
        let deadline = Instant::now() + WSL_LIST_TIMEOUT;
        let status = loop {
            match child.wait_timeout(Duration::from_millis(100)) {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Vec::new();
                    }
                }
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Vec::new();
                }
            }
        };
        if !status.success() {
            return Vec::new();
        }
        let bytes = reader
            .and_then(|handle| handle.join().ok())
            .unwrap_or_default();
        parse_wsl_list_output(&decode_wsl_output(&bytes))
    }
}

/// Decodes `wsl.exe` output as UTF-16LE when it has a BOM or NUL bytes, otherwise as
/// UTF-8. Both paths remove a leading BOM because older versions can ignore `WSL_UTF8=1`.
pub fn decode_wsl_output(bytes: &[u8]) -> String {
    let utf16 = bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE || bytes.contains(&0);
    let text = if utf16 {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    };
    text.trim_start_matches('\u{feff}').to_owned()
}

/// Parses a `--list --verbose` table. Distribution names may contain spaces, so split
/// columns on two or more spaces and discard rows with invalid names.
pub fn parse_wsl_list_output(text: &str) -> Vec<WslDistro> {
    static ROW: OnceLock<regex::Regex> = OnceLock::new();
    let row = ROW.get_or_init(|| {
        regex::Regex::new(r"^(\*?)\s*(.+?)\s{2,}\S+(?: \S+)*\s{2,}([12])$")
            .expect("wsl list row pattern compiles")
    });
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .skip(1)
        .filter_map(|line| {
            let captures = row.captures(line)?;
            let name = captures[2].to_owned();
            validate_wsl_distro_name(&name).ok()?;
            Some(WslDistro {
                name,
                version: if &captures[3] == "2" { 2 } else { 1 },
                is_default: &captures[1] == "*",
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SshMachineConfig;

    /// Separator-insensitive so one expectation reads the same on either platform:
    /// `Path::join` emits `\` on Windows and `/` elsewhere.
    fn normalized(path: &Path) -> String {
        path.to_string_lossy()
            .to_ascii_lowercase()
            .replace('\\', "/")
    }

    /// Builds a `PATH` value from separator-free segments so the fixture round-trips
    /// through `split_paths` on every platform.
    fn path_var(entries: &[&str]) -> std::ffi::OsString {
        std::env::join_paths(entries.iter().map(Path::new)).expect("fixture PATH joins")
    }

    fn present(existing: &'static [&'static str]) -> impl Fn(&Path) -> bool {
        move |path: &Path| existing.contains(&normalized(path).as_str())
    }

    #[test]
    fn local_bash_never_selects_the_windows_launcher() {
        let path = path_var(&["/win/system32", "/tools/bin"]);
        let chosen = select_local_bash(
            Some(path.as_os_str()),
            &[PathBuf::from("/win")],
            &[],
            &present(&["/win/system32/bash.exe", "/tools/bin/bash.exe"]),
        );
        // The launcher comes first on PATH and still loses: running there would put the
        // command on another machine's filesystem and loopback namespace.
        assert_eq!(chosen.as_deref().map(normalized).as_deref(), Some("/tools/bin/bash.exe"));
    }

    #[test]
    fn windows_directory_exclusion_ignores_case_and_separators() {
        let path = path_var(&["/WIN/System32"]);
        let chosen = select_local_bash(
            Some(path.as_os_str()),
            &[PathBuf::from("/win/")],
            &[],
            &present(&["/win/system32/bash.exe"]),
        );
        assert_eq!(chosen, None);
    }

    #[test]
    fn local_bash_derives_from_a_git_installation_on_path() {
        // The shape of a normal Git for Windows install: only `cmd` is advertised,
        // and it carries no bash.exe at all.
        let path = path_var(&["/git/cmd"]);
        let chosen = select_local_bash(
            Some(path.as_os_str()),
            &[PathBuf::from("/win")],
            &[],
            &present(&["/git/cmd/git.exe", "/git/bin/bash.exe"]),
        );
        assert_eq!(chosen.as_deref().map(normalized).as_deref(), Some("/git/bin/bash.exe"));
    }

    #[test]
    fn local_bash_falls_back_to_well_known_locations() {
        let path = path_var(&["/empty"]);
        let chosen = select_local_bash(
            Some(path.as_os_str()),
            &[PathBuf::from("/win")],
            &[PathBuf::from("/msys64/usr/bin/bash.exe")],
            &present(&["/msys64/usr/bin/bash.exe"]),
        );
        assert_eq!(
            chosen.as_deref().map(normalized).as_deref(),
            Some("/msys64/usr/bin/bash.exe")
        );
    }

    #[test]
    fn local_bash_reports_nothing_when_only_the_launcher_exists() {
        let path = path_var(&["/win/system32"]);
        let chosen = select_local_bash(
            Some(path.as_os_str()),
            &[PathBuf::from("/win")],
            &[PathBuf::from("/git/bin/bash.exe")],
            &present(&["/win/system32/bash.exe"]),
        );
        // Nothing found is a refusal the caller turns into an actionable message,
        // never a silent fallback to the launcher.
        assert_eq!(chosen, None);
    }

    fn assets_with(
        machines: Vec<SshMachineConfig>,
        env_vars: &[(&str, &[(&str, &str)])],
    ) -> ExecutionEnvironmentAssets {
        ExecutionEnvironmentAssets {
            ssh_machines: machines,
            env_vars: env_vars
                .iter()
                .map(|(key, pairs)| {
                    (
                        (*key).to_owned(),
                        pairs
                            .iter()
                            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                            .collect(),
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn local_resolution_picks_the_local_env_table() {
        let assets = assets_with(Vec::new(), &[("local", &[("FOO", "bar")])]);
        let runner = resolve_shell_runner(&assets, None).unwrap();
        assert_eq!(
            runner,
            ShellRunner::Local {
                env: [("FOO".to_owned(), "bar".to_owned())].into_iter().collect()
            }
        );
    }

    #[test]
    fn wsl_resolution_keys_env_by_distro_and_validates_the_name() {
        let assets = assets_with(Vec::new(), &[("wsl:Ubuntu", &[("A", "1")])]);
        let runner =
            resolve_shell_runner(&assets, Some(&RunTarget::Wsl { distro: "Ubuntu".into() }))
                .unwrap();
        let ShellRunner::Wsl { distro, env } = runner else {
            panic!("expected wsl runner");
        };
        assert_eq!(distro, "Ubuntu");
        assert_eq!(env.get("A").map(String::as_str), Some("1"));

        let injection = RunTarget::Wsl {
            distro: "Ubuntu; rm -rf /".into(),
        };
        assert!(resolve_shell_runner(&assets, Some(&injection)).is_err());
    }

    #[test]
    fn deleted_ssh_machine_fails_resolution_instead_of_falling_back_to_local() {
        let assets = assets_with(Vec::new(), &[]);
        let error = resolve_shell_runner(
            &assets,
            Some(&RunTarget::Ssh {
                machine_id: "m1".into(),
            }),
        )
        .unwrap_err();
        assert!(error.contains("no longer in the machine catalog"), "{error}");
    }

    #[test]
    fn ssh_resolution_copies_endpoint_fields_from_the_catalog() {
        let machine = SshMachineConfig {
            id: "m1".into(),
            name: "devbox".into(),
            host: "user@devbox.local".into(),
            port: 2222,
            identity_file: "C:/keys/id_ed25519".into(),
            remote_cwd: "~/work".into(),
            ..Default::default()
        };
        let assets = assets_with(vec![machine], &[("ssh:m1", &[("K", "v")])]);
        let runner = resolve_shell_runner(
            &assets,
            Some(&RunTarget::Ssh {
                machine_id: "m1".into(),
            }),
        )
        .unwrap();
        let ShellRunner::Ssh {
            host,
            port,
            identity_file,
            remote_cwd,
            env,
        } = runner
        else {
            panic!("expected ssh runner");
        };
        assert_eq!(host, "user@devbox.local");
        assert_eq!(port, 2222);
        assert_eq!(identity_file, "C:/keys/id_ed25519");
        assert_eq!(remote_cwd, "~/work");
        assert_eq!(env.get("K").map(String::as_str), Some("v"));
    }

    #[test]
    fn fingerprint_changes_with_identity_and_env() {
        let base = ShellRunner::Wsl {
            distro: "Ubuntu".into(),
            env: BTreeMap::new(),
        };
        let other_distro = ShellRunner::Wsl {
            distro: "Debian".into(),
            env: BTreeMap::new(),
        };
        let with_env = ShellRunner::Wsl {
            distro: "Ubuntu".into(),
            env: [("A".to_owned(), "1".to_owned())].into_iter().collect(),
        };
        assert_ne!(base.fingerprint(), other_distro.fingerprint());
        assert_ne!(base.fingerprint(), with_env.fingerprint());
        assert_eq!(base.fingerprint(), base.clone().fingerprint());
    }

    #[test]
    fn sh_single_quote_survives_embedded_quotes() {
        assert_eq!(sh_single_quote("plain"), "'plain'");
        assert_eq!(sh_single_quote("a'b"), r"'a'\''b'");
        assert_eq!(sh_single_quote(""), "''");
    }

    #[test]
    fn wsl_args_pass_command_and_env_as_verbatim_argv() {
        let env = [
            ("BASH_ENV".to_owned(), "/tmp/pwn".to_owned()),
            ("FOO".to_owned(), "a b'c".to_owned()),
        ]
        .into_iter()
        .collect();
        let args = wsl_shell_args(
            "Ubuntu",
            Path::new(r"C:\proj"),
            &env,
            "echo \"hello world\"",
        );
        assert_eq!(
            args,
            vec![
                "-d",
                "Ubuntu",
                "--cd",
                r"C:\proj",
                "--exec",
                "/usr/bin/env",
                "FOO=a b'c",
                "bash",
                "--noprofile",
                "--norc",
                "-c",
                "echo \"hello world\"",
            ]
        );
    }

    #[test]
    fn ssh_args_quote_every_host_supplied_fragment() {
        // Strip `BASH_ENV` in the wrapper as well as validation.
        let env = [
            ("BASH_ENV".to_owned(), "/tmp/pwn".to_owned()),
            ("FOO".to_owned(), "a'b".to_owned()),
        ]
        .into_iter()
        .collect();
        let args = ssh_shell_args(
            "user@devbox",
            2222,
            "C:/keys/id",
            "~/my work",
            &env,
            "echo 'hi'",
        );
        assert_eq!(
            &args[..8],
            &[
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                "-p",
                "2222",
                "-i",
                "C:/keys/id",
            ]
        );
        assert_eq!(&args[8..10], &["--", "user@devbox"]);
        assert_eq!(
            args[10],
            r"cd ~/'my work' || exit 1; exec env 'FOO=a'\''b' bash --noprofile --norc -c 'echo '\''hi'\'''"
        );
    }

    #[test]
    fn ssh_args_omit_port_identity_and_cd_when_unset() {
        let args = ssh_shell_args("devbox", 0, "", "", &BTreeMap::new(), "pwd");
        assert_eq!(
            args,
            vec![
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                "--",
                "devbox",
                "exec bash --noprofile --norc -c 'pwd'",
            ]
        );
    }

    #[test]
    fn wsl_list_output_parses_utf16_and_utf8_tables() {
        let table = "  NAME            STATE           VERSION\r\n* Ubuntu          Running         2\r\n  Debian          Stopped         1\r\n  kali-linux      Stopped         2\r\n";
        let utf16: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain(table.encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        for text in [decode_wsl_output(table.as_bytes()), decode_wsl_output(&utf16)] {
            let distros = parse_wsl_list_output(&text);
            assert_eq!(
                distros,
                vec![
                    WslDistro {
                        name: "Ubuntu".into(),
                        version: 2,
                        is_default: true
                    },
                    WslDistro {
                        name: "Debian".into(),
                        version: 1,
                        is_default: false
                    },
                    WslDistro {
                        name: "kali-linux".into(),
                        version: 2,
                        is_default: false
                    },
                ]
            );
        }
    }

    #[test]
    fn wsl_list_output_keeps_names_with_single_spaces() {
        let table = "  NAME            STATE           VERSION\n  My Distro Name  Stopped         2\n";
        let distros = parse_wsl_list_output(table);
        assert_eq!(distros.len(), 1);
        assert_eq!(distros[0].name, "My Distro Name");
    }

    /// Startup-pollution variables must be blocked by both validation and command
    /// construction, so legacy or bypassed data cannot run scripts before visible,
    /// approved remote commands.
    #[test]
    fn startup_pollution_variables_never_reach_a_remote_command() {
        let env: BTreeMap<String, String> = [
            ("BASH_ENV", "/tmp/pwn"),
            ("ENV", "/tmp/pwn"),
            ("SHELLOPTS", "xtrace"),
            ("FOO", "kept"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();

        let wsl = wsl_shell_args("Ubuntu", Path::new(r"C:\proj"), &env, "pwd").join(" ");
        assert!(wsl.contains("FOO=kept"), "{wsl}");
        for name in ["BASH_ENV", "ENV=", "SHELLOPTS"] {
            assert!(!wsl.contains(name), "{name} 不得进入 WSL argv: {wsl}");
        }

        let ssh = ssh_shell_args("devbox", 0, "", "", &env, "pwd").join(" ");
        assert!(ssh.contains("FOO=kept"), "{ssh}");
        for name in ["BASH_ENV", "SHELLOPTS"] {
            assert!(!ssh.contains(name), "{name} 不得进入 SSH 远端命令串: {ssh}");
        }
    }

    #[test]
    fn env_var_names_are_posix_shaped() {
        for name in ["FOO", "_bar", "A1_b2"] {
            assert!(validate_env_var_name(name).is_ok(), "{name}");
        }
        for name in ["", "1ABC", "A-B", "A B", "A=B", "名字"] {
            assert!(validate_env_var_name(name).is_err(), "{name}");
        }
    }
}
