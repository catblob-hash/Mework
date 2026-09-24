//! The Git status of a workspace that lives on another machine.
//!
//! The host reads a local checkout's status with its own Git
//! ([`crate::git::workspace_summary`]). A workspace on a WSL distribution or an
//! SSH machine has no checkout here, so the same reads — `rev-parse`, `status
//! --porcelain=v2`, the line counts, the tracked diffs, the local branches'
//! upstreams, the remotes and the operation in progress — run there instead,
//! all in one script through that machine's own shell, the transport the
//! remote file tools use ([`crate::run_environment::run_remote_script`]). One
//! script rather than one call per read, because a machine the agent does not
//! serve pays a whole SSH login for each call, and the Git pane asks every few
//! seconds.
//!
//! The script's answer is framed rather than parsed in place: a magic line,
//! then per read `name exit stdout-length stderr-length` and the two streams'
//! bytes. `status -z` output holds NULs and paths may hold newlines, so no
//! separator would survive; lengths do. The host assembles the snapshot from
//! the frames with the parsers the local leg uses
//! ([`crate::git::remote_workspace_snapshot`]).
//!
//! Nothing in the script comes from the model or the renderer: the only
//! variable is the workspace root the host recorded, and it goes through the
//! quoting of the language it is spliced into.

use std::collections::HashMap;
use std::time::Duration;

use crate::cancel::CancelSignal;
use crate::git::{
    self, GitWorkspaceSnapshot, GitWorkspaceSummaryResult, RemoteGitOutput, RemoteGitProbe,
};
use crate::remote_files::RemoteShell;
use crate::run_environment;
use crate::shell_backend::ScriptDialect;

/// How long one probe may take. Generous next to the local reads' own limits
/// because it carries all of them and a link that is not fast; the Git pane
/// does not ask again while one is still out.
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);

/// The first line of every answer, so a login shell that printed a banner
/// before the script ran is skipped instead of misread as a frame.
pub(crate) const PROBE_MAGIC: &str = "mework-git-status 1";

/// The root could not be entered — the same code the remote file tools use.
const EXIT_ROOT_MISSING: i32 = 64;
/// No `git` on the machine's `PATH`.
const EXIT_NO_GIT: i32 = 127;

/// The variables that would point Git somewhere other than the workspace or
/// change what it prints, cleared as the local leg clears them
/// (`configure_cli_environment`).
pub(crate) const GIT_ENVIRONMENT: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_SHALLOW_FILE",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    "GIT_CONFIG",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_NOSYSTEM",
    "GIT_EXEC_PATH",
    "GIT_TEMPLATE_DIR",
    "GIT_ATTR_NOSYSTEM",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_LITERAL_PATHSPECS",
    "GIT_GLOB_PATHSPECS",
    "GIT_NOGLOB_PATHSPECS",
    "GIT_ICASE_PATHSPECS",
    "GIT_EXTERNAL_DIFF",
    "GIT_TRACE",
    "GIT_TRACE2",
    "GIT_TRACE2_EVENT",
    "GIT_TRACE_PACKET",
];

/// The options every read runs Git with: the local leg's passive prefix
/// (`git_command_prefix`), which keeps a read from taking locks, starting
/// maintenance, recursing into submodules or colouring and quoting paths.
pub(crate) const GIT_PREFIX: &str = "--no-optional-locks -c core.fsmonitor=false -c gc.auto=0 \
     -c maintenance.auto=false -c submodule.recurse=false -c color.ui=false -c core.quotepath=false";

/// `for-each-ref`'s format for the local branches: the current one marked by
/// `%(HEAD)`, then the fields `read_upstream_atoms` reads. Listing every local
/// branch rather than naming the current one keeps each read's arguments
/// fixed — nothing Git reported is ever spliced back into a command line.
pub(crate) const BRANCHES_FORMAT: &str = "--format=%(HEAD)%00%(refname)%00%(objectname)%00%(upstream)%00%(upstream:short)%00%(upstream:remotename)%00%(upstream:remoteref)";

/// `diff`'s options for the tracked-content digests, as `tracked_diff_digest`
/// has them.
pub(crate) const DIGEST_DIFF: &str =
    "diff --binary --full-index --no-ext-diff --no-textconv --no-color --no-renames";

/// The state files each operation's revision is taken over, by the label
/// `repository_operation_label` gives it, in the order the local leg detects
/// them (`repository_operation_state`).
pub(crate) const OPERATIONS: &[(&str, &[&str], &[&str])] = &[
    (
        "merge",
        &["MERGE_HEAD"],
        &[
            "MERGE_HEAD",
            "MERGE_MODE",
            "MERGE_MSG",
            "AUTO_MERGE",
            "MERGE_RR",
        ],
    ),
    (
        "rebase",
        &["rebase-merge", "rebase-apply"],
        &[
            "REBASE_HEAD",
            "rebase-merge/head-name",
            "rebase-merge/onto",
            "rebase-merge/msgnum",
            "rebase-merge/end",
            "rebase-merge/done",
            "rebase-merge/stopped-sha",
            "rebase-apply/head-name",
            "rebase-apply/onto",
            "rebase-apply/next",
            "rebase-apply/last",
        ],
    ),
    (
        "cherry-pick",
        &["CHERRY_PICK_HEAD"],
        &[
            "CHERRY_PICK_HEAD",
            "MERGE_MSG",
            "sequencer/head",
            "sequencer/todo",
        ],
    ),
    (
        "revert",
        &["REVERT_HEAD"],
        &[
            "REVERT_HEAD",
            "MERGE_MSG",
            "sequencer/head",
            "sequencer/todo",
        ],
    ),
    (
        "bisect",
        &["BISECT_LOG"],
        &[
            "BISECT_LOG",
            "BISECT_START",
            "BISECT_TERMS",
            "BISECT_EXPECTED_REV",
        ],
    ),
];

/// [`crate::git::workspace_summary`] for a workspace on the machine `shell`
/// reaches.
pub(crate) fn workspace_summary(
    shell: &dyn RemoteShell,
    machine_key: &str,
    root: &str,
    known_revision: Option<String>,
) -> Result<GitWorkspaceSummaryResult, String> {
    match workspace_snapshot(shell, machine_key, root)? {
        Some(snapshot) => git::summary_result(&snapshot, known_revision),
        None => Ok(GitWorkspaceSummaryResult::NotRepository),
    }
}

/// The Git status snapshot of a workspace on the machine `shell` reaches:
/// `None` when the root is not a repository's root.
pub(crate) fn workspace_snapshot(
    shell: &dyn RemoteShell,
    machine_key: &str,
    root: &str,
) -> Result<Option<GitWorkspaceSnapshot>, String> {
    let root = root.trim();
    if root.is_empty() || root.chars().any(char::is_control) {
        return Err("工作区路径为空或含有控制字符，无法读取它的 Git 状态".into());
    }
    let script = match shell.dialect() {
        ScriptDialect::PowerShell => crate::remote_powershell::git_status_probe(root),
        ScriptDialect::Posix => posix_probe(root),
    };
    let output = shell.run(&script, None, PROBE_TIMEOUT, &CancelSignal::default())?;
    match output.status {
        Some(0) => {}
        Some(EXIT_ROOT_MISSING) => {
            return Err(format!("工作区目录 {root} 在这台机器上不存在或无法进入"))
        }
        Some(EXIT_NO_GIT) => return Err("这台机器上没有找到 Git CLI".into()),
        status => {
            return Err(
                match run_environment::legible_remote_reply(&output.stderr) {
                    Some(reply) => format!("读取远端 Git 状态失败：{reply}"),
                    None => format!("读取远端 Git 状态失败（退出码 {status:?}）"),
                },
            )
        }
    }
    let sections = parse_frames(&output.stdout)?;
    git::remote_workspace_snapshot(&RemoteGitProbe {
        machine_key: machine_key.to_owned(),
        sections,
    })
}

/// The probe in POSIX `sh`, for bash, zsh (in `sh` emulation) and sh alike.
///
/// Each read's streams go to files in a private temporary directory so their
/// lengths are known before they are written out; a tracked diff is not kept
/// at all, only its digest, which Git itself computes (`hash-object --stdin`)
/// so the machine needs no checksum tool of its own.
fn posix_probe(root: &str) -> String {
    let mut script = format!(
        "cd -- {} || exit {EXIT_ROOT_MISSING}\n\
         command -v git >/dev/null 2>&1 || exit {EXIT_NO_GIT}\n\
         unset {}\n\
         GIT_OPTIONAL_LOCKS=0 GIT_TERMINAL_PROMPT=0 GIT_PAGER=cat PAGER=cat LC_ALL=C LANG=C\n\
         export GIT_OPTIONAL_LOCKS GIT_TERMINAL_PROMPT GIT_PAGER PAGER LC_ALL LANG\n",
        run_environment::quote_remote_path(root),
        GIT_ENVIRONMENT.join(" "),
    );
    script.push_str(&format!(
        r#"T=$(mktemp -d 2>/dev/null) || T=
if [ -z "$T" ]; then T="${{TMPDIR:-/tmp}}/mework-git.$$"; mkdir -m 700 "$T" || exit 71; fi
trap 'rm -rf "$T"' EXIT
trap 'exit 130' HUP INT TERM
g() {{ git {GIT_PREFIX} "$@"; }}
emit() {{ printf '%s %s %s %s\n' "$1" "$2" $(wc -c <"$T/o") $(wc -c <"$T/e"); cat "$T/o" "$T/e"; }}
run() {{ n=$1; shift; g "$@" >"$T/o" 2>"$T/e"; emit "$n" $?; }}
digest() {{ n=$1; shift; {{ g "$@"; echo $? >"$T/x"; }} 2>"$T/e" | git hash-object --stdin >"$T/o"; emit "$n" "$(cat "$T/x")"; }}
printf '%s\n' '{PROBE_MAGIC}'
g rev-parse --show-prefix --show-toplevel --absolute-git-dir --git-common-dir >"$T/o" 2>"$T/e"; s=$?
emit rev-parse $s
[ $s -eq 0 ] || exit 0
[ -z "$(sed -n 1p "$T/o")" ] || exit 0
D=$(sed -n 3p "$T/o")
run version --version
run status status --porcelain=v2 -z --branch --show-stash --untracked-files=all
if g rev-parse -q --verify HEAD >/dev/null 2>&1; then
  run numstat diff --no-ext-diff --no-textconv --numstat -z HEAD
else
  run numstat diff --no-ext-diff --no-textconv --numstat -z --cached
fi
digest staged-digest {DIGEST_DIFF} --cached --
digest unstaged-digest {DIGEST_DIFF} --
run branches for-each-ref '{BRANCHES_FORMAT}' refs/heads
run upstream-oid rev-parse -q --verify '@{{upstream}}^{{commit}}'
run remotes remote -v
op=
set --
"#
    ));
    for (index, (label, markers, files)) in OPERATIONS.iter().enumerate() {
        let test = markers
            .iter()
            .map(|marker| format!("[ -e \"$D/{marker}\" ]"))
            .collect::<Vec<_>>()
            .join(" || ");
        script.push_str(if index == 0 { "if " } else { "elif " });
        script.push_str(&format!(
            "{test}; then op={label}; set -- {}\n",
            files.join(" ")
        ));
    }
    script.push_str(
        r#"fi
{ printf '%s\n' "$op"; for f in "$@"; do if [ -f "$D/$f" ]; then printf '%s ' "$f"; cksum <"$D/$f"; fi; done; } >"$T/o" 2>"$T/e"
emit operation 0
exit 0
"#,
    );
    script
}

/// Splits the probe's answer into its reads, by name.
fn parse_frames(bytes: &[u8]) -> Result<HashMap<String, RemoteGitOutput>, String> {
    let magic = format!("{PROBE_MAGIC}\n");
    let start = if bytes.starts_with(magic.as_bytes()) {
        0
    } else {
        let marker = format!("\n{magic}");
        bytes
            .windows(marker.len())
            .position(|window| window == marker.as_bytes())
            .map(|position| position + 1)
            .ok_or_else(|| "远端 Git 状态探测没有给出可识别的回答".to_owned())?
    };
    let mut rest = &bytes[start + magic.len()..];
    let mut sections = HashMap::new();
    while !rest.is_empty() {
        let newline = rest
            .iter()
            .position(|byte| *byte == b'\n')
            .ok_or_else(|| "远端 Git 状态探测的回答被截断".to_owned())?;
        let header = std::str::from_utf8(&rest[..newline])
            .map_err(|_| "远端 Git 状态探测的段头不是 UTF-8".to_owned())?;
        let fields = header.split_whitespace().collect::<Vec<_>>();
        let [name, status, stdout_len, stderr_len] = fields[..] else {
            return Err(format!("远端 Git 状态探测的段头无效：{header}"));
        };
        let number = |text: &str| {
            text.parse::<usize>()
                .map_err(|_| format!("远端 Git 状态探测的段头无效：{header}"))
        };
        let status = status
            .parse::<i32>()
            .map_err(|_| format!("远端 Git 状态探测的段头无效：{header}"))?;
        let (stdout_len, stderr_len) = (number(stdout_len)?, number(stderr_len)?);
        rest = &rest[newline + 1..];
        let body = stdout_len
            .checked_add(stderr_len)
            .filter(|length| *length <= rest.len())
            .ok_or_else(|| "远端 Git 状态探测的回答被截断".to_owned())?;
        sections.insert(
            name.to_owned(),
            RemoteGitOutput {
                status,
                stdout: rest[..stdout_len].to_vec(),
                stderr: rest[stdout_len..body].to_vec(),
            },
        );
        rest = &rest[body..];
    }
    Ok(sections)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote_files::tests::LocalBash;
    use std::path::Path;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?}");
    }

    fn repository() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(dir.path().join("tracked.txt"), "one\ntwo\n").unwrap();
        git(dir.path(), &["add", "tracked.txt"]);
        git(dir.path(), &["commit", "-q", "-m", "first"]);
        dir
    }

    fn root_of(dir: &tempfile::TempDir) -> String {
        std::fs::canonicalize(dir.path())
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn frames_carry_nul_and_newline_bytes_by_length() {
        let mut answer = b"welcome to the machine\n".to_vec();
        answer.extend_from_slice(format!("{PROBE_MAGIC}\n").as_bytes());
        answer.extend_from_slice(b"status 0 7 0\n1 a\0\nb\0");
        answer.extend_from_slice(b"rev-parse 128 0 5\nfatal");
        let sections = parse_frames(&answer).unwrap();
        assert_eq!(sections["status"].stdout, b"1 a\0\nb\0");
        assert_eq!(sections["rev-parse"].status, 128);
        assert_eq!(sections["rev-parse"].stderr, b"fatal");
    }

    #[test]
    fn a_truncated_or_unframed_answer_is_refused() {
        assert!(parse_frames(b"nothing here").is_err());
        let truncated = format!("{PROBE_MAGIC}\nstatus 0 9 0\nshort");
        assert!(parse_frames(truncated.as_bytes()).is_err());
    }

    #[test]
    fn the_root_is_the_only_variable_in_the_script() {
        let script = posix_probe("/srv/it's here");
        assert!(
            script.starts_with("cd -- '/srv/it'\\''s here' || exit 64\n"),
            "{script}"
        );
        let other = posix_probe("/srv/elsewhere");
        assert_eq!(
            script.lines().skip(1).collect::<Vec<_>>(),
            other.lines().skip(1).collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_powershell_probe_passes_git_only_fixed_arguments() {
        // Windows splits a command line on spaces and quotes; none of the
        // fixed arguments may carry either, so none needs quoting there.
        for argument in GIT_PREFIX
            .split_whitespace()
            .chain(BRANCHES_FORMAT.split_whitespace())
            .chain(DIGEST_DIFF.split_whitespace())
        {
            assert!(
                !argument.contains('"') && !argument.contains('\''),
                "{argument}"
            );
        }
        assert_eq!(BRANCHES_FORMAT.split_whitespace().count(), 1);
    }

    #[test]
    fn a_remote_checkout_reads_as_the_local_leg_reads_it() {
        let Some(shell) = LocalBash::find() else {
            return;
        };
        let dir = repository();
        std::fs::write(dir.path().join("tracked.txt"), "one\n2\nthree\n").unwrap();
        std::fs::write(dir.path().join("new file.txt"), "fresh\n").unwrap();
        let root = root_of(&dir);

        let remote = workspace_snapshot(&shell, "ssh:test", &root)
            .unwrap()
            .expect("a repository");
        let local = git::workspace_snapshot(dir.path())
            .unwrap()
            .expect("a repository");
        assert_eq!(remote.branch.as_deref(), Some("main"));
        assert_eq!(remote.head, local.head);
        assert_eq!(
            (remote.additions, remote.deletions),
            (local.additions, local.deletions)
        );
        assert_eq!((remote.additions, remote.deletions), (2, 1));
        assert_eq!(
            (remote.staged, remote.unstaged, remote.untracked),
            (local.staged, local.unstaged, local.untracked)
        );
        assert_eq!(remote.files, local.files);
        assert_eq!(remote.worktree_root, root);
        assert_eq!(remote.operation, None);
        assert!(remote.remote.is_none());
        assert!(remote.warnings.is_empty(), "{:?}", remote.warnings);
        assert!(!remote.git_version.is_empty());
        // The machine is part of the identity.
        let elsewhere = workspace_snapshot(&shell, "ssh:other", &root)
            .unwrap()
            .unwrap();
        assert_ne!(remote.repository_id, elsewhere.repository_id);
        assert_ne!(remote.worktree_id, elsewhere.worktree_id);
    }

    #[test]
    fn the_summary_revision_follows_the_content_and_holds_still_without_it() {
        let Some(shell) = LocalBash::find() else {
            return;
        };
        let dir = repository();
        let root = root_of(&dir);
        let first = workspace_snapshot(&shell, "ssh:test", &root)
            .unwrap()
            .unwrap();
        let again = workspace_snapshot(&shell, "ssh:test", &root)
            .unwrap()
            .unwrap();
        assert_eq!(first.summary_revision, again.summary_revision);
        assert!(matches!(
            workspace_summary(
                &shell,
                "ssh:test",
                &root,
                Some(first.summary_revision.clone())
            ),
            Ok(GitWorkspaceSummaryResult::Unchanged { .. })
        ));
        // Same paths and statuses, different bytes: only the digests can tell.
        std::fs::write(dir.path().join("tracked.txt"), "one\nTWO\n").unwrap();
        let edited = workspace_snapshot(&shell, "ssh:test", &root)
            .unwrap()
            .unwrap();
        std::fs::write(dir.path().join("tracked.txt"), "one\nTwo\n").unwrap();
        let edited_again = workspace_snapshot(&shell, "ssh:test", &root)
            .unwrap()
            .unwrap();
        assert_ne!(first.summary_revision, edited.summary_revision);
        assert_ne!(edited.summary_revision, edited_again.summary_revision);
    }

    #[test]
    fn upstream_remotes_and_operations_come_through() {
        let Some(shell) = LocalBash::find() else {
            return;
        };
        let origin = repository();
        let dir = tempfile::tempdir().unwrap();
        let clone = dir.path().join("clone");
        git(
            dir.path(),
            &["clone", "-q", &origin.path().to_string_lossy(), "clone"],
        );
        git(&clone, &["commit", "-q", "--allow-empty", "-m", "ahead"]);
        let root = std::fs::canonicalize(&clone)
            .unwrap()
            .to_string_lossy()
            .into_owned();

        let snapshot = workspace_snapshot(&shell, "ssh:test", &root)
            .unwrap()
            .unwrap();
        assert_eq!(snapshot.upstream.as_deref(), Some("origin/main"));
        assert_eq!((snapshot.ahead, snapshot.behind), (1, 0));
        assert_eq!(
            snapshot.remote.as_ref().map(|remote| remote.name.as_str()),
            Some("origin")
        );
        let upstream = snapshot.upstream_target.clone().expect("an upstream");
        assert_eq!(upstream.remote_name, "origin");
        assert_eq!(upstream.merge_ref, "refs/heads/main");
        assert!(upstream.tracking_oid.is_some());
        assert!(snapshot.remotes.iter().all(|remote| remote.url.is_none()));

        let head = snapshot.head.clone().expect("a commit");
        std::fs::write(clone.join(".git").join("MERGE_HEAD"), format!("{head}\n")).unwrap();
        let merging = workspace_snapshot(&shell, "ssh:test", &root)
            .unwrap()
            .unwrap();
        assert_eq!(merging.operation, Some(git::GitRepositoryOperation::Merge));
        assert!(merging.operation_revision.is_some());
    }

    /// The probe against a real Windows machine over SSH, through the agent,
    /// in both agent shells it can have there: Git Bash, which runs the POSIX
    /// probe, and PowerShell, which runs its own. Set
    /// `MEWORK_E2E_SSH_WINDOWS_HOST` (and `MEWORK_E2E_SSH_PORT`,
    /// `MEWORK_E2E_SSH_KEY` as needed) and run with `--ignored`. The machine
    /// needs Git for Windows; the scratch repository goes in its home.
    #[test]
    #[ignore]
    fn over_real_ssh_both_windows_agent_shells_read_the_same_status() {
        use crate::run_environment::ShellRunner;
        use crate::shell_backend::{AgentShell, ShellBackend};
        let host =
            std::env::var("MEWORK_E2E_SSH_WINDOWS_HOST").expect("MEWORK_E2E_SSH_WINDOWS_HOST");
        let port = std::env::var("MEWORK_E2E_SSH_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
            .unwrap_or(0);
        let identity_file = std::env::var("MEWORK_E2E_SSH_KEY").unwrap_or_default();
        let app_data = tempfile::tempdir().unwrap();
        crate::remote_link::install(app_data.path(), Vec::new(), None);
        let runner = |agent_shell| ShellRunner::Ssh {
            agent_shell,
            host: host.clone(),
            port,
            identity_file: identity_file.clone(),
            env: Default::default(),
        };
        let bash = runner(AgentShell::default());
        let setup = bash
            .run(
                "R=~/mework-e2e-git && rm -rf \"$R\" && mkdir -p \"$R\" && cd \"$R\" \\
                 && git init -q -b main && printf 'one\\n' >a.txt && git add a.txt \\
                 && git -c user.name=e2e -c user.email=e2e@example.com commit -q -m first \\
                 && printf 'two\\n' >>a.txt && printf 'new\\n' >b.txt && cygpath -m \"$R\"",
                None,
                PROBE_TIMEOUT,
                &CancelSignal::default(),
            )
            .unwrap();
        assert_eq!(setup.status, Some(0), "{}", setup.stderr);
        let root = String::from_utf8_lossy(&setup.stdout).trim().to_owned();
        let powershell = runner(AgentShell::new(ShellBackend::PowerShell, "powershell.exe"));

        let through_bash = workspace_snapshot(&bash, "ssh:e2e", &root)
            .unwrap()
            .expect("a repository");
        let through_powershell = workspace_snapshot(&powershell, "ssh:e2e", &root)
            .unwrap()
            .expect("a repository");
        for snapshot in [&through_bash, &through_powershell] {
            assert_eq!(snapshot.branch.as_deref(), Some("main"));
            // An untracked file is unstaged too: a.txt's edit and b.txt.
            assert_eq!(
                (snapshot.additions, snapshot.unstaged, snapshot.untracked),
                (1, 2, 1)
            );
            assert!(snapshot.warnings.is_empty(), "{:?}", snapshot.warnings);
        }
        assert_eq!(through_bash.files, through_powershell.files);
        assert_eq!(through_bash.repository_id, through_powershell.repository_id);
        crate::remote_link::shutdown();
    }

    #[test]
    fn outside_a_repository_root_there_is_no_repository() {
        let Some(shell) = LocalBash::find() else {
            return;
        };
        let plain = tempfile::tempdir().unwrap();
        assert_eq!(
            workspace_snapshot(&shell, "ssh:test", &root_of(&plain)).unwrap(),
            None
        );
        let dir = repository();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let sub = format!("{}/sub", root_of(&dir));
        assert_eq!(workspace_snapshot(&shell, "ssh:test", &sub).unwrap(), None);
        assert!(matches!(
            workspace_summary(&shell, "ssh:test", &sub, None),
            Ok(GitWorkspaceSummaryResult::NotRepository)
        ));
        let missing = format!("{}/missing", root_of(&plain));
        assert!(workspace_snapshot(&shell, "ssh:test", &missing)
            .unwrap_err()
            .contains("不存在"));
    }
}
