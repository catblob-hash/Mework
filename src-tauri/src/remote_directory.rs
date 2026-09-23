//! Reading a directory on a machine that is not this one.
//!
//! The host has a native folder dialog only for its own filesystem, so a
//! workspace on a WSL distribution or an SSH machine is chosen by browsing it
//! here: one level per call, read through that machine's own shell.
//!
//! The picker is where a machine is first met, so it assumes as little about
//! it as it can. An SSH login shell may be any Unix shell, or `cmd.exe` or
//! PowerShell on Windows, and bash may not be installed at all. It reads a
//! Unix machine with `/bin/sh` alone and a Windows machine with the
//! PowerShell every Windows ships. Which of the two a machine is comes from
//! its agent when the agent serves it ([`crate::remote_link`]) — a Windows
//! machine is then read as Windows whichever shell it logs in with — and
//! otherwise from its login shell ([`remote_shell::login_shell`]).
//!
//! This is picker code, not tool code. It never runs anything the model asked
//! for: the scripts are fixed, the only variable in them is the directory the
//! user is looking at, and that goes through the quoting of the language it is
//! spliced into.

use std::time::Duration;

use serde::Serialize;

use crate::cancel::CancelSignal;
use crate::model::{ExecutionEnvironmentAssets, RunTarget};
use crate::remote_link::{self, Route};
use crate::remote_shell::{self, LoginShell};
use crate::run_environment::{self, RemoteCommandOutput, ShellRunner};

/// How long a listing may take before the browser gives up.
///
/// A picker that hangs is worse than one that fails: the user is standing in
/// front of a dialog waiting for it. SSH's own `ConnectTimeout` covers reaching
/// the machine; this covers a machine that answers and then stalls.
const LISTING_TIMEOUT: Duration = Duration::from_secs(20);

/// Longest path the browser will carry. Matches the host's other path fields.
const MAX_PATH_CHARS: usize = 4096;

/// Longest line `cmd.exe` accepts is 8191 characters; a PowerShell line a
/// little under it is refused here rather than cut off there.
const MAX_WINDOWS_LINE: usize = 8000;

/// The pseudo-directory a Windows machine's drives are listed under, so that
/// "up" from `C:/` leads somewhere and another drive can be reached.
const WINDOWS_DRIVES: &str = "/";

/// One level of a remote filesystem.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDirectoryListing {
    /// The directory the remote shell actually resolved, with `~` expanded and
    /// `..` applied. The browser shows this rather than what it asked for: only
    /// the remote shell can say where a path really leads. A Windows path is
    /// spelled with forward slashes, `C:/Users/dev`, which both PowerShell and
    /// a Git Bash read.
    pub path: String,
    /// The directory to go up to, or `None` at the top.
    pub parent: Option<String>,
    /// Immediate subdirectories, sorted.
    pub entries: Vec<RemoteDirectoryEntry>,
}

/// One subdirectory, with the path to ask for when the user opens it. The host
/// spells the path because only it knows the machine's path rules — `/srv` is
/// joined with `/`, the drive list's `C:` opens as `C:/`.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDirectoryEntry {
    pub name: String,
    pub path: String,
}

/// Lists one directory on `machine`.
///
/// Fails rather than returning an empty listing when the machine is
/// unreachable or the directory cannot be entered: "this directory is empty"
/// and "I could not look" are different answers, and only one of them means the
/// user should pick something else.
pub fn list_directory(
    assets: &ExecutionEnvironmentAssets,
    machine: &RunTarget,
    path: &str,
) -> Result<RemoteDirectoryListing, String> {
    let runner = run_environment::resolve_shell_runner(assets, Some(machine), None)?;
    let path = checked(path)?;
    let (output, flavor) = read(
        &runner,
        &posix_listing_script(path),
        &windows_script(path, true)?,
    )?;
    parse_listing(&output, flavor)
}

/// Resolves `path` on `machine` and confirms it is a directory that can be
/// entered, returning the path the remote shell resolved.
///
/// This is the remote half of the native dialog's contract: the caller records
/// the returned path as granted, and the host refuses to save a document naming
/// a workspace no picker of its own ever returned. Resolving rather than echoing
/// matters — `~/app` and `/home/dev/app` are the same directory, and a grant
/// recorded under one spelling has to match a check made under the other.
pub fn resolve_directory(
    assets: &ExecutionEnvironmentAssets,
    machine: &RunTarget,
    path: &str,
) -> Result<String, String> {
    let runner = run_environment::resolve_shell_runner(assets, Some(machine), None)?;
    let path = checked(path)?;
    let (output, flavor) = read(
        &runner,
        &format!("cd -- {} && pwd", run_environment::quote_remote_path(path)),
        &windows_script(path, false)?,
    )?;
    let resolved = output.lines().next().unwrap_or_default().trim();
    if resolved.is_empty() {
        return Err(format!("{path} 在这台机器上解析不到目录"));
    }
    if flavor == Flavor::Windows && resolved == WINDOWS_DRIVES {
        return Err("盘符列表不是目录；请进入一个盘符再选择".into());
    }
    Ok(resolved.to_owned())
}

/// The path rules of the machine a listing came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flavor {
    Posix,
    Windows,
}

impl Flavor {
    fn parent(self, path: &str) -> Option<String> {
        match self {
            Self::Posix => posix_parent(path),
            Self::Windows => windows_parent(path),
        }
    }

    fn child(self, path: &str, name: &str) -> String {
        if self == Self::Windows && path == WINDOWS_DRIVES {
            return format!("{name}/");
        }
        if path.ends_with('/') {
            format!("{path}{name}")
        } else {
            format!("{path}/{name}")
        }
    }
}

fn posix_parent(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        _ if trimmed.is_empty() => None,
        Some(0) => Some("/".into()),
        Some(cut) => Some(trimmed[..cut].to_owned()),
        None => None,
    }
}

/// `C:/Users` → `C:/` → the drive list; a UNC share root is as far up as the
/// share lets anyone go.
fn windows_parent(path: &str) -> Option<String> {
    if path == WINDOWS_DRIVES {
        return None;
    }
    let trimmed = path.trim_end_matches('/');
    if is_drive(trimmed) {
        return Some(WINDOWS_DRIVES.into());
    }
    if let Some(unc) = trimmed.strip_prefix("//") {
        if unc.splitn(3, '/').nth(2).is_none() {
            return None;
        }
    }
    let (head, _) = trimmed.rsplit_once('/')?;
    Some(if is_drive(head) {
        format!("{head}/")
    } else {
        head.to_owned()
    })
}

fn is_drive(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// The one variable the scripts carry, refused when it could not be a path.
fn checked(path: &str) -> Result<&str, String> {
    let path = path.trim();
    if path.is_empty() {
        return Err("目录不能为空".into());
    }
    if path.chars().count() > MAX_PATH_CHARS {
        return Err(format!("目录路径不能超过 {MAX_PATH_CHARS} 个字符"));
    }
    if path.chars().any(char::is_control) {
        return Err("目录路径不能包含控制字符".into());
    }
    Ok(path)
}

/// The fixed POSIX listing script.
///
/// `ls -p` marks directories with a trailing slash and `-L` makes it mark a
/// symlink to a directory too, which is what a checked-out project often is.
/// `sed` keeps only the marked names and strips the mark, so the output is the
/// resolved path followed by one directory name per line. A name containing a
/// newline would split into two entries; the browser would then show a
/// directory that cannot be entered, which is a visible and harmless failure —
/// nothing downstream trusts these names except as the next path to try.
fn posix_listing_script(path: &str) -> String {
    format!(
        "cd -- {} && pwd && ls -A1pL . 2>/dev/null | sed -n 's:/$::p'",
        run_environment::quote_remote_path(path)
    )
}

/// The fixed PowerShell script for a Windows machine: the resolved directory,
/// then, when `list` is set, its subdirectories — the same shape the POSIX
/// script prints.
///
/// The output is written as UTF-8 bytes straight to the standard streams, not
/// through the console, whose code page is the OEM one (936 on a Chinese
/// Windows) and would reach this host as mojibake. `~` is the profile
/// directory, `/` is the drive list, and a bare `C:` is that drive's root
/// rather than PowerShell's idea of the current directory on it. Hidden
/// directories are listed like dotfiles are on Unix, except the ones that are
/// also system entries — `$Recycle.Bin`, the legacy `Application Data`
/// junctions — which no one opens a project in.
fn windows_script(path: &str, list: bool) -> Result<String, String> {
    let script = format!(
        r#"$ErrorActionPreference='Stop'
$ProgressPreference='SilentlyContinue'
$utf8=New-Object System.Text.UTF8Encoding $false
function Write-MeworkBytes($stream,[string]$text){{$bytes=$utf8.GetBytes($text);$stream.Write($bytes,0,$bytes.Length);$stream.Flush()}}
try{{
$path={path}
$list=${list}
$lines=New-Object System.Collections.Generic.List[string]
if($path -eq '/'){{
$lines.Add('/')
if($list){{foreach($drive in [System.IO.DriveInfo]::GetDrives()){{if($drive.IsReady){{$lines.Add($drive.Name.TrimEnd('\'))}}}}}}
}}else{{
if($path -eq '~' -or $path -eq '~/' -or $path -eq '~\'){{$path=$HOME}}
elseif($path.StartsWith('~/') -or $path.StartsWith('~\')){{$path=Join-Path -Path $HOME -ChildPath ($path.Substring(2))}}
if($path -match '^[A-Za-z]:$'){{$path+='\'}}
$item=Get-Item -LiteralPath $path -Force
if(-not $item.PSIsContainer){{throw ('不是目录：'+$item.FullName)}}
$lines.Add($item.FullName.Replace('\','/'))
if($list){{foreach($child in (Get-ChildItem -LiteralPath $item.FullName -Directory -Force -ErrorAction SilentlyContinue)){{
$attributes=$child.Attributes
if(($attributes -band [System.IO.FileAttributes]::Hidden) -and ($attributes -band [System.IO.FileAttributes]::System)){{continue}}
$lines.Add($child.Name)}}}}
}}
Write-MeworkBytes ([Console]::OpenStandardOutput()) (($lines -join "`n")+"`n")
exit 0
}}catch{{
Write-MeworkBytes ([Console]::OpenStandardError()) ($_.Exception.Message+"`n")
exit 1
}}"#,
        path = remote_shell::ps_single_quote(path),
        list = if list { "true" } else { "false" },
    );
    if remote_shell::powershell_line(&script).len() > MAX_WINDOWS_LINE {
        return Err("这条路径太长，经 Windows 的命令行传不过去".into());
    }
    Ok(script)
}

/// Splits a script's output into the resolved path and its subdirectories.
fn parse_listing(output: &str, flavor: Flavor) -> Result<RemoteDirectoryListing, String> {
    let mut lines = output.lines().map(|line| line.trim_end_matches('\r'));
    let path = lines.next().unwrap_or_default().trim().to_owned();
    if path.is_empty() {
        return Err("这台机器没有报告目录位置".into());
    }
    let mut names: Vec<&str> = lines
        .filter(|name| !name.is_empty() && *name != "." && *name != "..")
        .collect();
    match flavor {
        // Windows names compare without case, and so should their order.
        Flavor::Windows => names.sort_by(|a, b| {
            a.to_lowercase()
                .cmp(&b.to_lowercase())
                .then_with(|| a.cmp(b))
        }),
        Flavor::Posix => names.sort_unstable(),
    }
    names.dedup();
    let entries = names
        .into_iter()
        .map(|name| RemoteDirectoryEntry {
            path: flavor.child(&path, name),
            name: name.to_owned(),
        })
        .collect();
    Ok(RemoteDirectoryListing {
        parent: flavor.parent(&path),
        path,
        entries,
    })
}

/// Why one attempt failed, and whether the reply says the machine is not the
/// family the attempt assumed.
struct Failure {
    message: String,
    answered_by_windows: bool,
}

/// Runs whichever of the two scripts the machine's login shell needs and
/// returns its stdout with the path rules to read it by.
///
/// A remembered answer about the login shell can go stale — someone switches
/// `DefaultShell` to bash after reading the advice to — so a failure made
/// with one is followed by asking the machine again, and by one more attempt
/// if the answer changed. A POSIX attempt that `cmd.exe` or PowerShell
/// answered is retried as Windows outright: a probe the machine answered
/// oddly (an unset `%OS%`) should not keep the picker from working.
fn read(
    runner: &ShellRunner,
    posix: &str,
    windows: &str,
) -> Result<(String, Flavor), String> {
    match remote_link::route(runner, LISTING_TIMEOUT) {
        Route::Agent(link, agent) => return read_through_agent(&link, runner, &agent, posix, windows),
        Route::Unreachable(error) => return Err(error),
        Route::Legacy => {}
    }
    let (shell, remembered) = remote_shell::login_shell(runner)?;
    let failure = match attempt(runner, shell, posix, windows) {
        Ok(read) => return Ok(read),
        Err(failure) => failure,
    };
    let next = if failure.answered_by_windows && !shell.is_windows() {
        Some(LoginShell::Cmd)
    } else if remembered {
        remote_shell::forget_login_shell(runner);
        let (fresh, _) = remote_shell::login_shell(runner)?;
        (fresh != shell).then_some(fresh)
    } else {
        None
    };
    let Some(next) = next else {
        return Err(failure.message);
    };
    let read = attempt(runner, next, posix, windows).map_err(|failure| failure.message)?;
    remote_shell::remember_login_shell(runner, next);
    Ok(read)
}

/// Runs the script for the machine the agent says it is on, without a login
/// shell in between: the agent starts `/bin/sh` or PowerShell itself.
fn read_through_agent(
    link: &remote_agent::client::Link,
    runner: &ShellRunner,
    agent: &remote_agent::protocol::AgentInfo,
    posix: &str,
    windows: &str,
) -> Result<(String, Flavor), String> {
    let (argv, flavor) = if agent.os == "windows" {
        (remote_shell::powershell_argv(windows), Flavor::Windows)
    } else {
        (
            vec!["/bin/sh".to_owned(), "-c".to_owned(), posix.to_owned()],
            Flavor::Posix,
        )
    };
    let output = remote_link::run_script_on(
        link,
        runner,
        argv,
        None,
        LISTING_TIMEOUT,
        &CancelSignal::default(),
    )?;
    if output.status == Some(0) {
        return Ok((decode(&output.stdout, runner), flavor));
    }
    Err(refusal(&output))
}

fn attempt(
    runner: &ShellRunner,
    shell: LoginShell,
    posix: &str,
    windows: &str,
) -> Result<(String, Flavor), Failure> {
    let cancel = CancelSignal::default();
    let (output, flavor) = if shell.is_windows() {
        (
            run_environment::run_ssh_line(
                runner,
                &remote_shell::powershell_line(windows),
                LISTING_TIMEOUT,
                &cancel,
            ),
            Flavor::Windows,
        )
    } else {
        (
            run_environment::run_remote_sh_script(runner, posix, LISTING_TIMEOUT, &cancel),
            Flavor::Posix,
        )
    };
    let output = output.map_err(|message| Failure {
        message,
        answered_by_windows: false,
    })?;
    if output.status == Some(0) {
        return Ok((decode(&output.stdout, runner), flavor));
    }
    Err(Failure {
        answered_by_windows: flavor == Flavor::Posix
            && run_environment::answered_by_non_posix_shell(output.status, &output.stderr),
        message: refusal(&output),
    })
}

/// A failed read in the machine's own words: "No such file or directory" from
/// the remote shell tells the user more than any sentence the host could
/// invent for it. Words that did not survive decoding are not shown.
fn refusal(output: &RemoteCommandOutput) -> String {
    match run_environment::legible_remote_reply(&output.stderr) {
        Some(reply) => reply.to_owned(),
        None if output.stderr.trim().is_empty() => {
            format!("这台机器拒绝了这次目录读取（退出码 {:?}）", output.status)
        }
        None => format!(
            "这台机器的回应不是 UTF-8 文本，无法显示（退出码 {:?}）",
            output.status
        ),
    }
}

/// WSL may answer in UTF-16LE even with `WSL_UTF8=1`; everything else is UTF-8.
fn decode(bytes: &[u8], runner: &ShellRunner) -> String {
    if matches!(runner, ShellRunner::Wsl { .. }) {
        return run_environment::decode_wsl_output(bytes);
    }
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(listing: &RemoteDirectoryListing) -> Vec<&str> {
        listing
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect()
    }

    fn paths(listing: &RemoteDirectoryListing) -> Vec<&str> {
        listing
            .entries
            .iter()
            .map(|entry| entry.path.as_str())
            .collect()
    }

    #[test]
    fn a_listing_reports_the_resolved_path_and_its_subdirectories() {
        let listing =
            parse_listing("/home/dev/projects\napp\nnotes\ninfra\n", Flavor::Posix).unwrap();
        assert_eq!(listing.path, "/home/dev/projects");
        assert_eq!(names(&listing), ["app", "infra", "notes"]);
        assert_eq!(
            paths(&listing),
            [
                "/home/dev/projects/app",
                "/home/dev/projects/infra",
                "/home/dev/projects/notes"
            ]
        );
        assert_eq!(listing.parent.as_deref(), Some("/home/dev"));
    }

    #[test]
    fn the_filesystem_root_has_nowhere_to_go_up_to() {
        let listing = parse_listing("/\netc\nsrv\n", Flavor::Posix).unwrap();
        assert_eq!(listing.path, "/");
        assert_eq!(listing.parent, None);
        assert_eq!(paths(&listing), ["/etc", "/srv"]);
        assert_eq!(posix_parent("/srv"), Some("/".into()));
    }

    #[test]
    fn a_directory_with_no_subdirectories_is_not_a_failure() {
        let listing = parse_listing("/home/dev/leaf\n", Flavor::Posix).unwrap();
        assert!(listing.entries.is_empty());
        assert_eq!(listing.path, "/home/dev/leaf");
    }

    #[test]
    fn output_with_no_path_is_refused_rather_than_read_as_an_empty_directory() {
        assert!(parse_listing("", Flavor::Posix).is_err());
        assert!(parse_listing("\napp\n", Flavor::Posix).is_err());
        assert!(parse_listing("", Flavor::Windows).is_err());
    }

    /// PowerShell's answer uses forward slashes; up from a drive root is the
    /// drive list, and a drive in that list opens at its root.
    #[test]
    fn a_windows_listing_walks_drives_and_folders() {
        let listing =
            parse_listing("C:/Users/dev\r\nsource\r\nAppData\r\n中文项目\r\n", Flavor::Windows)
                .unwrap();
        assert_eq!(listing.path, "C:/Users/dev");
        assert_eq!(listing.parent.as_deref(), Some("C:/Users"));
        assert_eq!(names(&listing), ["AppData", "source", "中文项目"]);
        assert_eq!(paths(&listing)[0], "C:/Users/dev/AppData");

        let root = parse_listing("C:/\nUsers\nWindows\n", Flavor::Windows).unwrap();
        assert_eq!(root.parent.as_deref(), Some("/"));
        assert_eq!(paths(&root), ["C:/Users", "C:/Windows"]);
        assert_eq!(windows_parent("C:/Users"), Some("C:/".into()));

        let drives = parse_listing("/\nC:\nD:\n", Flavor::Windows).unwrap();
        assert_eq!(drives.parent, None);
        assert_eq!(paths(&drives), ["C:/", "D:/"]);
    }

    #[test]
    fn a_unc_share_root_is_as_far_up_as_windows_goes() {
        assert_eq!(windows_parent("//nas/share"), None);
        assert_eq!(windows_parent("//nas/share/"), None);
        assert_eq!(
            windows_parent("//nas/share/team"),
            Some("//nas/share".into())
        );
    }

    #[test]
    fn the_browsed_directory_is_the_only_variable_in_the_posix_script() {
        let command = posix_listing_script("~/my projects");
        assert!(
            command.starts_with("cd -- ~/'my projects' && pwd && ls -A1pL ."),
            "{command}"
        );
        // A quote in the name closes nothing: the fragment is single-quoted.
        let command = posix_listing_script("/srv/it's here");
        assert!(command.contains(r#"'/srv/it'\''s here'"#), "{command}");
    }

    #[test]
    fn the_browsed_directory_is_the_only_variable_in_the_windows_script() {
        let script = windows_script("C:/Users/it's here", true).unwrap();
        assert!(script.contains("$path='C:/Users/it''s here'\n"), "{script}");
        assert!(script.contains("$list=$true\n"), "{script}");
        assert!(windows_script("~", false)
            .unwrap()
            .contains("$list=$false\n"));
        // Nothing else in the script depends on what was asked for.
        let one = windows_script("C:/a", true).unwrap();
        let other = windows_script("D:/b", true).unwrap();
        assert_eq!(
            one.replace("'C:/a'", "PATH"),
            other.replace("'D:/b'", "PATH")
        );
    }

    #[test]
    fn a_path_that_could_rewrite_a_script_never_reaches_it() {
        assert!(checked("").is_err());
        assert!(checked("  ").is_err());
        assert!(checked("/srv/\nrm -rf /").is_err());
        assert!(checked(&"/".repeat(MAX_PATH_CHARS + 1)).is_err());
        assert_eq!(checked("  /srv/app "), Ok("/srv/app"));
        // `cmd.exe` would cut a line this long; it is refused here instead.
        assert!(windows_script(&"a".repeat(3000), true).is_err());
        assert!(windows_script(&"a".repeat(200), true).is_ok());
    }

    #[test]
    fn a_reply_that_did_not_survive_decoding_is_not_shown() {
        let reply = |status: i32, stderr: &str| RemoteCommandOutput {
            status: Some(status),
            stdout: Vec::new(),
            stderr: stderr.to_owned(),
        };
        assert_eq!(
            refusal(&reply(1, "cd: /srv/missing: No such file or directory\n")),
            "cd: /srv/missing: No such file or directory"
        );
        let gbk = String::from_utf8_lossy(b"\xd5\xd2\xb2\xbb\xb5\xbd\r\n").into_owned();
        assert!(refusal(&reply(1, &gbk)).contains("不是 UTF-8"));
        assert!(refusal(&reply(2, "")).contains("退出码 Some(2)"));
    }
}
