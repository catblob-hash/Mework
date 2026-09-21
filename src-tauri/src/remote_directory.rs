//! Reading a directory on a machine that is not this one.
//!
//! The host has a native folder dialog only for its own filesystem, so a
//! workspace on a WSL distribution or an SSH machine is chosen by browsing it
//! here: one level per call, read through that machine's own shell — the same
//! transport the `bash` tool dispatches through, so a machine the tools can
//! reach is exactly a machine the picker can browse.
//!
//! This is picker code, not tool code. It never runs anything the model asked
//! for: the command is fixed, the only variable in it is the directory the user
//! is looking at, and that goes through POSIX single-quoting like every other
//! host-supplied fragment.

use std::time::Duration;

use serde::Serialize;

use crate::cancel::CancelSignal;
use crate::model::{ExecutionEnvironmentAssets, RunTarget};
use crate::run_environment::{self, ShellRunner};

/// How long a listing may take before the browser gives up.
///
/// A picker that hangs is worse than one that fails: the user is standing in
/// front of a dialog waiting for it. SSH's own `ConnectTimeout` covers reaching
/// the machine; this covers a machine that answers and then stalls.
const LISTING_TIMEOUT: Duration = Duration::from_secs(20);

/// Longest path the browser will carry. Matches the host's other path fields.
const MAX_PATH_CHARS: usize = 4096;

/// One level of a remote filesystem.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDirectoryListing {
    /// The directory the remote shell actually resolved, with `~` expanded and
    /// `..` applied. The browser shows this rather than what it asked for: only
    /// the remote shell can say where a path really leads.
    pub path: String,
    /// Immediate subdirectory names, sorted, without their parent.
    pub directories: Vec<String>,
    /// Whether `path` has a parent to go up to.
    pub has_parent: bool,
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
    let runner = run_environment::resolve_shell_runner(assets, Some(machine))?;
    let output = run(&runner, &listing_command(path)?)?;
    parse_listing(&output)
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
    let runner = run_environment::resolve_shell_runner(assets, Some(machine))?;
    let command = format!("cd -- {} && pwd", quote(path)?);
    let resolved = run(&runner, &command)?;
    let resolved = resolved.lines().next().unwrap_or_default().trim();
    if resolved.is_empty() {
        return Err(format!("{path} 在这台机器上解析不到目录"));
    }
    Ok(resolved.to_owned())
}

/// The fixed listing command.
///
/// `ls -p` marks directories with a trailing slash and `-L` makes it mark a
/// symlink to a directory too, which is what a checked-out project often is.
/// `sed` keeps only the marked names and strips the mark, so the output is the
/// resolved path followed by one directory name per line. A name containing a
/// newline would split into two entries; the browser would then show a
/// directory that cannot be entered, which is a visible and harmless failure —
/// nothing downstream trusts these names except as the next path to try.
fn listing_command(path: &str) -> Result<String, String> {
    Ok(format!(
        "cd -- {} && pwd && ls -A1pL . 2>/dev/null | sed -n 's:/$::p'",
        quote(path)?
    ))
}

/// POSIX single-quoting, with a bare `~` prefix left outside the quotes so the
/// remote shell still expands it. Mirrors the shell leg's own rule.
fn quote(path: &str) -> Result<String, String> {
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
    Ok(run_environment::quote_remote_path(path))
}

/// Splits the command's output into the resolved path and its subdirectories.
fn parse_listing(output: &str) -> Result<RemoteDirectoryListing, String> {
    let mut lines = output.lines();
    let path = lines.next().unwrap_or_default().trim().to_owned();
    if path.is_empty() {
        return Err("这台机器没有报告目录位置".into());
    }
    let mut directories: Vec<String> = lines
        .map(str::trim_end)
        .filter(|name| !name.is_empty() && *name != "." && *name != "..")
        .map(str::to_owned)
        .collect();
    directories.sort();
    directories.dedup();
    let has_parent = path != "/";
    Ok(RemoteDirectoryListing {
        path,
        directories,
        has_parent,
    })
}

/// Runs one fixed command on the machine and returns its stdout.
///
/// The transport is the shared one every remote leg dispatches through; only the
/// picker's own refusals stay here, because they are written for someone
/// standing in front of a dialog. A non-zero exit is reported with the machine's
/// own stderr: "No such file or directory" from the remote shell tells the user
/// more than any sentence the host could invent for it.
fn run(runner: &ShellRunner, command: &str) -> Result<String, String> {
    if matches!(runner, ShellRunner::Local { .. }) {
        return Err("本机目录请用系统目录选择器".into());
    }
    let output = run_environment::run_remote_script(
        runner,
        command,
        None,
        LISTING_TIMEOUT,
        &CancelSignal::default(),
    )?;
    if output.status == Some(0) {
        return Ok(decode(&output.stdout, runner));
    }
    let detail = output.stderr.trim();
    Err(if detail.is_empty() {
        format!("这台机器拒绝了这次目录读取（退出码 {:?}）", output.status)
    } else {
        detail.to_owned()
    })
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

    #[test]
    fn a_listing_reports_the_resolved_path_and_its_subdirectories() {
        let listing = parse_listing("/home/dev/projects\napp\nnotes\ninfra\n").unwrap();
        assert_eq!(listing.path, "/home/dev/projects");
        assert_eq!(listing.directories, vec!["app", "infra", "notes"]);
        assert!(listing.has_parent);
    }

    #[test]
    fn the_filesystem_root_has_nowhere_to_go_up_to() {
        let listing = parse_listing("/\netc\nsrv\n").unwrap();
        assert_eq!(listing.path, "/");
        assert!(!listing.has_parent);
    }

    #[test]
    fn a_directory_with_no_subdirectories_is_not_a_failure() {
        let listing = parse_listing("/home/dev/leaf\n").unwrap();
        assert!(listing.directories.is_empty());
        assert_eq!(listing.path, "/home/dev/leaf");
    }

    #[test]
    fn output_with_no_path_is_refused_rather_than_read_as_an_empty_directory() {
        assert!(parse_listing("").is_err());
        assert!(parse_listing("\napp\n").is_err());
    }

    #[test]
    fn the_browsed_directory_is_the_only_variable_in_the_command() {
        let command = listing_command("~/my projects").unwrap();
        assert!(
            command.starts_with("cd -- ~/'my projects' && pwd && ls -A1pL ."),
            "{command}"
        );
        // A quote in the name closes nothing: the fragment is single-quoted.
        let command = listing_command("/srv/it's here").unwrap();
        assert!(command.contains(r#"'/srv/it'\''s here'"#), "{command}");
    }

    #[test]
    fn a_path_that_could_rewrite_the_command_never_reaches_it() {
        assert!(listing_command("").is_err());
        assert!(listing_command("/srv/\nrm -rf /").is_err());
        assert!(listing_command(&"/".repeat(MAX_PATH_CHARS + 1)).is_err());
    }
}
