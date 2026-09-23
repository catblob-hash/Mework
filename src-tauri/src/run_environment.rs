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

use crate::host_platform::host_platform;
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
            &BTreeMap::new(),
            self.env(),
            host_platform().is_windows() && matches!(self, Self::Local { .. }),
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
    ///
    /// The working directory is deliberately absent: it is no longer a property of
    /// the machine but of the workspace the call names, so the policy fingerprint
    /// that guards a nonce folds the workspace in alongside this.
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
                ..
            } => {
                hasher.update(b"ssh:");
                hasher.update(host.as_bytes());
                hasher.update([0]);
                hasher.update(port.to_le_bytes());
                hasher.update(identity_file.as_bytes());
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

/// A machine's identity: `local`, `wsl:<distro>`, or `ssh:<machine id>`.
pub fn env_key(target: Option<&RunTarget>) -> String {
    match target {
        None => "local".into(),
        Some(RunTarget::Wsl { distro }) => format!("wsl:{distro}"),
        Some(RunTarget::Ssh { machine_id }) => format!("ssh:{machine_id}"),
    }
}

/// Key of a workspace's variable table in [`ExecutionEnvironmentAssets::env_vars`]:
/// the machine's [`env_key`] and the directory as the workspace records it,
/// joined by `|`. Variables belong to the workspace, not the machine — two
/// directories on one machine each carry their own table.
pub fn workspace_env_key(target: Option<&RunTarget>, path: &str) -> String {
    format!("{}|{path}", env_key(target))
}

/// Resolves a machine to a trusted dispatch environment.
///
/// `workspace_path` names the workspace the shell runs for, and the runner
/// carries that workspace's variable table; `None` is the machine alone, with
/// no variables, which is what browsing a machine before any of its directories
/// is a workspace wants. A missing SSH catalog entry must fail explicitly rather
/// than silently falling back to local execution, which would run
/// remote-intended commands on the wrong machine.
pub fn resolve_shell_runner(
    assets: &ExecutionEnvironmentAssets,
    target: Option<&RunTarget>,
    workspace_path: Option<&str>,
) -> Result<ShellRunner, String> {
    let env = workspace_path
        .and_then(|path| assets.env_vars.get(&workspace_env_key(target, path)))
        .cloned()
        .unwrap_or_default();
    match target {
        None => Ok(ShellRunner::Local { env }),
        Some(RunTarget::Wsl { distro }) => {
            // WSL exists on Windows and nowhere else. A document that travelled
            // between machines can still name one here, and without this the
            // call would reach `wsl.exe` and come back as a missing program —
            // a message about a file, for what is really the wrong machine.
            if !host_platform().has_wsl() {
                return Err(format!(
                    "This conversation runs in WSL ({distro}), which a {} host cannot reach; select an execution location again",
                    host_platform().display_name()
                ));
            }
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
                return Err(format!(
                    "SSH machine {} has an empty host address",
                    machine.name
                ));
            }
            Ok(ShellRunner::Ssh {
                host: machine.host.clone(),
                port: machine.port,
                identity_file: machine.identity_file.clone(),
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
        "BASH_ENV"
            | "ENV"
            | "SHELLOPTS"
            | "BASHOPTS"
            | "CDPATH"
            | "GLOBIGNORE"
            | "GIT_EXTERNAL_DIFF"
    )
}

/// Validates POSIX-shaped environment variable names up to 128 characters.
pub fn validate_env_var_name(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let valid_head = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    let valid_tail = name
        .chars()
        .skip(1)
        .all(|c| c.is_ascii_alphanumeric() || c == '_');
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

/// Quotes a remote path while leaving a `~` prefix unquoted for expansion.
pub fn quote_remote_path(cwd: &str) -> String {
    if cwd == "~" {
        return "~".into();
    }
    if let Some(rest) = cwd.strip_prefix("~/") {
        return format!("~/{}", sh_single_quote(rest));
    }
    sh_single_quote(cwd)
}

/// Builds `wsl.exe` arguments. `--cd` applies distribution automount rules to an
/// absolute Windows path and takes a Linux path as-is, which is what a workspace
/// attached on the distribution itself carries; environment variables are argv
/// entries and the command is the single `bash -c` argument passed unchanged
/// through `--exec`.
pub fn wsl_shell_args(
    distro: &str,
    workspace_root: &str,
    env: &BTreeMap<String, String>,
    command: &str,
) -> Vec<String> {
    let mut args = vec![
        "-d".into(),
        distro.to_owned(),
        "--cd".into(),
        workspace_root.to_owned(),
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

/// OpenSSH's options and destination for an unattended call — everything
/// before the remote command. `BatchMode=yes` fails explicitly when credentials
/// or host-key confirmation are unavailable.
pub fn ssh_connection_args(host: &str, port: u16, identity_file: &str) -> Vec<String> {
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
    args
}

/// Builds OpenSSH arguments that run `command` in bash on the machine.
///
/// This is the per-command transport: one SSH login per call. A machine the
/// agent serves ([`crate::remote_link`]) does not use it for tool calls,
/// scripts or terminals; it remains for machines the agent cannot serve.
///
/// The POSIX script around the command — the `cd`, the variables, the `bash`
/// it hands the command to — is composed with [`sh_single_quote`] for every
/// host-supplied fragment, as `sh` will read it. What travels is that script in
/// [`remote_shell::posix_line`] form, because the login shell reads the line
/// first and is not necessarily a POSIX shell: fish, tcsh and nushell each read
/// quotes their own way, and the neutral line reads the same to all of them.
///
/// `remote_cwd` is the root of the workspace the call named, not a property of the
/// machine: the same machine serves as many working directories as the
/// conversation has attached on it. Empty leaves the remote login shell wherever
/// it starts, which is the remote user's home.
///
/// [`remote_shell::posix_line`]: crate::remote_shell::posix_line
pub fn ssh_shell_args(
    host: &str,
    port: u16,
    identity_file: &str,
    remote_cwd: &str,
    env: &BTreeMap<String, String>,
    command: &str,
) -> Vec<String> {
    let mut args = ssh_connection_args(host, port, identity_file);
    let script = ssh_bash_script(remote_cwd, env, command);
    let line = crate::remote_shell::posix_line(&script);
    // The line is one argument to the login shell, and Linux refuses a single
    // argument over 128 KiB. A command dense enough in escaped bytes to cross
    // that goes as the script itself, which every POSIX login shell still reads
    // — what such a command loses is only fish and tcsh.
    args.push(if line.len() <= MAX_NEUTRAL_LINE_BYTES {
        line
    } else {
        script
    });
    args
}

/// Longest [`crate::remote_shell::posix_line`] sent: Linux's `MAX_ARG_STRLEN`
/// (32 pages of 4 KiB) with room for its terminating NUL.
const MAX_NEUTRAL_LINE_BYTES: usize = 128 * 1024 - 1;

/// The `sh` script [`ssh_shell_args`] sends: enter the workspace, then hand the
/// command to a bash that reads no startup files, with the variable table in
/// its environment.
fn ssh_bash_script(remote_cwd: &str, env: &BTreeMap<String, String>, command: &str) -> String {
    let mut remote = String::new();
    if !remote_cwd.is_empty() {
        remote.push_str(&format!("cd {} || exit 1; ", quote_remote_path(remote_cwd)));
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
    remote
}

/// SSH client executable candidates in priority order. On Windows, the system OpenSSH
/// binary is a fallback when PATH lacks a usable client.
pub fn ssh_client_candidates() -> Vec<String> {
    #[cfg(windows)]
    {
        vec!["ssh".into(), r"C:\Windows\System32\OpenSSH\ssh.exe".into()]
    }
    #[cfg(not(windows))]
    {
        vec!["ssh".into()]
    }
}

/// Whether a failed remote invocation was answered by `cmd.exe` or PowerShell
/// rather than a POSIX shell.
///
/// Every bash leg sends the `exec /bin/sh -c '…'` line from [`ssh_shell_args`], and
/// a Windows sshd whose `DefaultShell` is still the factory `cmd.exe` cannot
/// run a word of it. What comes back is that shell's
/// own "not recognized" complaint about `exec`, which read raw says nothing
/// about what to change on the machine. The signatures: `cmd.exe` says so in
/// the console language and exits 9009; PowerShell prints the
/// locale-independent `CommandNotFoundException` category. The exit code is
/// only a secondary signal — the Windows OpenSSH client passes it through
/// whole, a POSIX client folds it to eight bits, and a Windows sshd reached
/// from macOS has been seen to report plain 1 — so the text carries the weight.
///
/// The console language arrives in the machine's OEM code page, not UTF-8: a
/// Chinese install answers in GBK, which lossy decoding here turns into
/// replacement characters, so the localized sentences match only on a console
/// switched to UTF-8. What survives every code page is the ASCII head of
/// `cmd.exe`'s reply — it quotes the unknown command first, `'exec' …` in
/// English, Chinese, Japanese and Korean alike — and no POSIX shell ever opens a
/// complaint about its own `exec` builtin that way. An unknown language that
/// quotes differently falls through to the raw reply.
pub fn answered_by_non_posix_shell(status: Option<i32>, stderr: &str) -> bool {
    const SIGNATURES: [&str; 6] = [
        "is not recognized as an internal or external command",
        "is not recognized as the name of a cmdlet",
        "CommandNotFoundException",
        "不是内部或外部命令",
        "不是內部或外部命令",
        "内部コマンドまたは外部コマンド",
    ];
    status == Some(9009)
        || stderr.trim_start().starts_with("'exec'")
        || SIGNATURES.iter().any(|needle| stderr.contains(needle))
}

/// A remote reply fit to show someone, or `None` when there is nothing to show
/// or it did not survive decoding.
///
/// Remote stderr is decoded as UTF-8 with replacement, and a Windows console
/// answering in its OEM code page (GBK, Big5, Shift_JIS…) comes out as
/// replacement characters around the ASCII. Shown raw that is noise that buries
/// whatever was said next to it, so the caller leaves it out instead.
pub fn legible_remote_reply(stderr: &str) -> Option<&str> {
    let reply = stderr.trim();
    (!reply.is_empty() && !reply.contains(char::REPLACEMENT_CHARACTER)).then_some(reply)
}

/// What one remote invocation left behind.
///
/// `stdout` stays raw. A remote `read` carries file bytes through it, and
/// `--exec … bash` hands the child's own stdout back verbatim, so decoding it
/// here would corrupt any payload that is not text. Only `stderr` is decoded,
/// because that is where `wsl.exe` itself may answer in UTF-16LE.
pub struct RemoteCommandOutput {
    /// Exit code, or `None` when a signal ended the child.
    pub status: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

/// How often a running remote script is asked whether someone stopped it. The
/// same interval the shell leg polls at, and the bound on how long a cancelled
/// call keeps a child alive.
const REMOTE_POLL: Duration = Duration::from_millis(100);

/// Runs one host-authored `bash -c` script on the machine `runner` dispatches to,
/// feeding `stdin` if given.
///
/// The script is the only variable in the invocation and it is authored here, not
/// by the model: every fragment a caller folds into it goes through
/// [`sh_single_quote`] first. A `Local` runner is refused — this host's own
/// filesystem is reached directly, and silently running a POSIX script through
/// some local Bash would act on paths that mean something else here.
///
/// An SSH machine the agent serves runs the script through it
/// ([`crate::remote_link`]): the same `bash --noprofile --norc -c`, started by
/// the agent directly instead of by the login shell, over the machine's one
/// long-lived connection instead of a fresh SSH login.
pub fn run_remote_script(
    runner: &ShellRunner,
    script: &str,
    stdin: Option<&[u8]>,
    timeout: Duration,
    cancel: &crate::cancel::CancelSignal,
) -> Result<RemoteCommandOutput, String> {
    let argv = ["bash", "--noprofile", "--norc", "-c", script]
        .iter()
        .map(|part| (*part).to_owned())
        .collect();
    if let Some(result) = crate::remote_link::run_script(runner, argv, stdin, timeout, cancel) {
        return result;
    }
    let child = spawn_remote_script(runner, script, stdin.is_some())?;
    pump_remote_child(child, runner, stdin, timeout, cancel)
}

/// The host program and arguments that run `script` on the machine `runner`
/// dispatches to: `wsl.exe` for a distribution, one of the SSH client
/// candidates for a machine. Pure, so the invocation shape is testable without
/// a machine to reach.
pub fn remote_script_invocation(
    runner: &ShellRunner,
    script: &str,
) -> Result<(Vec<String>, Vec<String>), String> {
    let env = runner.normalized_env()?;
    match runner {
        ShellRunner::Local { .. } => {
            Err("This machine's own filesystem is not reached through a remote shell".into())
        }
        ShellRunner::Wsl { distro, .. } => {
            validate_wsl_distro_name(distro)?;
            Ok((
                vec!["wsl.exe".to_owned()],
                // `--cd /` keeps the invocation independent of wherever the
                // distribution would otherwise start; the script does its own
                // `cd` to the workspace root it was built for.
                wsl_shell_args(distro, "/", &env, script),
            ))
        }
        ShellRunner::Ssh {
            host,
            port,
            identity_file,
            ..
        } => Ok((
            ssh_client_candidates(),
            ssh_shell_args(host, *port, identity_file, "", &env, script),
        )),
    }
}

/// Runs a POSIX `sh` script on the machine `runner` dispatches to, assuming
/// nothing there but `/bin/sh`.
///
/// This is for the host's own read-only probes, such as the directory picker,
/// which should work on a machine before anything else about it is known —
/// including whether it has bash. On SSH the script is the whole
/// [`remote_shell::posix_line`] payload, so `sh` runs it directly; on WSL
/// `--exec` starts `/bin/sh` itself.
///
/// [`remote_shell::posix_line`]: crate::remote_shell::posix_line
pub fn run_remote_sh_script(
    runner: &ShellRunner,
    script: &str,
    timeout: Duration,
    cancel: &crate::cancel::CancelSignal,
) -> Result<RemoteCommandOutput, String> {
    let argv = vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()];
    if let Some(result) = crate::remote_link::run_script(runner, argv, None, timeout, cancel) {
        return result;
    }
    let (candidates, args) = match runner {
        ShellRunner::Local { .. } => {
            return Err(
                "This machine's own filesystem is not reached through a remote shell".into(),
            )
        }
        ShellRunner::Wsl { distro, .. } => {
            validate_wsl_distro_name(distro)?;
            (
                vec!["wsl.exe".to_owned()],
                vec![
                    "-d".into(),
                    distro.clone(),
                    "--cd".into(),
                    "/".into(),
                    "--exec".into(),
                    "/bin/sh".into(),
                    "-c".into(),
                    script.to_owned(),
                ],
            )
        }
        ShellRunner::Ssh {
            host,
            port,
            identity_file,
            ..
        } => {
            let mut args = ssh_connection_args(host, *port, identity_file);
            args.push(crate::remote_shell::posix_line(script));
            (ssh_client_candidates(), args)
        }
    };
    let child = spawn_invocation(runner, &candidates, &args, false)?;
    pump_remote_child(child, runner, None, timeout, cancel)
}

/// Sends one command line, verbatim, to an SSH machine's login shell.
///
/// Nothing is wrapped around `line`, so it must already be in a form the login
/// shell reads as intended: [`remote_shell::PROBE_COMMAND`], or a
/// [`remote_shell::powershell_line`] for a Windows machine. Only SSH has a login
/// shell in the way; any other runner is refused.
///
/// [`remote_shell::PROBE_COMMAND`]: crate::remote_shell::PROBE_COMMAND
/// [`remote_shell::powershell_line`]: crate::remote_shell::powershell_line
pub fn run_ssh_line(
    runner: &ShellRunner,
    line: &str,
    timeout: Duration,
    cancel: &crate::cancel::CancelSignal,
) -> Result<RemoteCommandOutput, String> {
    let ShellRunner::Ssh {
        host,
        port,
        identity_file,
        ..
    } = runner
    else {
        return Err("Only an SSH machine is reached through a login shell".into());
    };
    let mut args = ssh_connection_args(host, *port, identity_file);
    args.push(line.to_owned());
    let child = spawn_invocation(runner, &ssh_client_candidates(), &args, false)?;
    pump_remote_child(child, runner, None, timeout, cancel)
}

/// Starts `script` on the machine `runner` dispatches to, with stdout and
/// stderr piped and stdin piped only when the caller has something to feed it.
///
/// The child is handed back unwaited: [`run_remote_script`] pumps it to
/// completion, while a language server started this way stays up and speaks
/// its protocol over the same pipes for as long as the workspace needs it.
pub fn spawn_remote_script(
    runner: &ShellRunner,
    script: &str,
    pipe_stdin: bool,
) -> Result<std::process::Child, String> {
    let (candidates, args) = remote_script_invocation(runner, script)?;
    spawn_invocation(runner, &candidates, &args, pipe_stdin)
}

/// Starts the first of `candidates` that exists with `args`, piping stdout and
/// stderr, and stdin only when asked to.
fn spawn_invocation(
    runner: &ShellRunner,
    candidates: &[String],
    args: &[String],
    pipe_stdin: bool,
) -> Result<std::process::Child, String> {
    let mut last_error = None;
    for executable in candidates {
        let mut process = Command::new(executable);
        process
            .args(args)
            .stdin(if pipe_stdin {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if matches!(runner, ShellRunner::Wsl { .. }) {
            process.env("WSL_UTF8", "1");
        }
        // A tool call is not a user-initiated console session; a window flashing
        // up for every remote `ls` would read as the app doing something else.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            process.creation_flags(CREATE_NO_WINDOW);
        }
        match process.spawn() {
            Ok(child) => return Ok(child),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                last_error = Some(format!("{executable} was not found"));
                continue;
            }
            Err(error) => return Err(format!("Failed to start {executable}: {error}")),
        }
    }
    Err(last_error.unwrap_or_else(|| "No remote execution program is available".into()))
}

/// Feeds the child its input and drains both of its pipes while waiting.
///
/// All three run on their own threads: a script that fills the stdout pipe while
/// the host is still writing stdin deadlocks otherwise, and so does one that
/// writes more to stderr than the pipe holds.
fn pump_remote_child(
    mut child: std::process::Child,
    runner: &ShellRunner,
    stdin: Option<&[u8]>,
    timeout: Duration,
    cancel: &crate::cancel::CancelSignal,
) -> Result<RemoteCommandOutput, String> {
    let writer = match (child.stdin.take(), stdin) {
        (Some(mut pipe), Some(bytes)) => {
            let bytes = bytes.to_vec();
            Some(std::thread::spawn(move || {
                use std::io::Write as _;
                // A broken pipe means the script stopped reading; the exit code
                // it leaves behind is the answer, not this write's error.
                let _ = pipe.write_all(&bytes);
                let _ = pipe.flush();
            }))
        }
        (pipe, _) => {
            drop(pipe);
            None
        }
    };
    fn drain<R: std::io::Read + Send + 'static>(
        pipe: Option<R>,
    ) -> Option<std::thread::JoinHandle<Vec<u8>>> {
        pipe.map(|mut pipe| {
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let _ = pipe.read_to_end(&mut bytes);
                bytes
            })
        })
    }
    let out_reader = drain(child.stdout.take());
    let err_reader = drain(child.stderr.take());

    let deadline = Instant::now() + timeout;
    let mut ended = None;
    let mut failure = None;
    loop {
        match child.wait_timeout(REMOTE_POLL) {
            Ok(Some(status)) => {
                ended = Some(status);
                break;
            }
            Ok(None) => {
                if cancel.cancelled() {
                    let _ = child.kill();
                    let _ = child.wait();
                    failure = Some("The remote command was cancelled".to_owned());
                    break;
                }
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    failure = Some(format!(
                        "The remote command did not finish within {} seconds",
                        timeout.as_secs()
                    ));
                    break;
                }
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                failure = Some(format!("Failed to wait for the remote command: {error}"));
                break;
            }
        }
    }
    // Joined after the child is gone, so every pipe is at end of file and no
    // thread outlives the call that started it — including on the error paths.
    if let Some(writer) = writer {
        let _ = writer.join();
    }
    let stdout = out_reader
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default();
    let stderr = err_reader
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default();
    if let Some(failure) = failure {
        return Err(failure);
    }
    Ok(RemoteCommandOutput {
        status: ended.and_then(|status| status.code()),
        stdout,
        stderr: if matches!(runner, ShellRunner::Wsl { .. }) {
            decode_wsl_output(&stderr)
        } else {
            String::from_utf8_lossy(&stderr).into_owned()
        },
    })
}

/// The Windows-shaped file names this resolver looks for. A Local run always
/// targets the Windows host, so the POSIX name is never the right answer here.
const BASH_EXECUTABLE: &str = "bash.exe";
const GIT_EXECUTABLE: &str = "git.exe";

/// Bash interpreters for a Local run, in preference order.
///
/// A bare `bash` is not good enough on Windows, and it fails in two opposite
/// directions. A `bash.exe` belonging to WSL is a launcher: it would run the
/// command inside the default distribution, whose filesystem and loopback
/// namespace are not the ones this conversation targets, and it would report
/// success for a command that never touched the host — so the wrong machine
/// stays invisible in the transcript. Meanwhile a normal Git for Windows
/// install advertises only its `cmd` directory on `PATH`, which carries
/// `git.exe` but no `bash.exe`, so the bare name resolves to nothing at all and
/// the tool looks like it has no backend.
///
/// Local means this Windows host: launchers are excluded and a native Bash is
/// resolved to an absolute path. WSL stays reachable by selecting it as the
/// conversation's run target, which is the only place its semantics are honest.
pub fn local_bash_candidates() -> Vec<String> {
    #[cfg(windows)]
    {
        select_local_bash(
            std::env::var_os("PATH").as_deref(),
            &launcher_only_directories(),
            &well_known_bash_paths(),
            &|path: &Path| path.is_file(),
        )
        .map(|path| vec![path.to_string_lossy().into_owned()])
        .unwrap_or_default()
    }
    #[cfg(not(windows))]
    {
        // By path rather than by name, because the tool also hands it to the
        // command as `SHELL`, and a program that re-runs `$SHELL` or checks it
        // is executable needs a path. `PATH` order decides, so a newer bash the
        // user installed wins over macOS's `/bin/bash` 3.2, as it does in their
        // own terminal (see `child_environment::adopt_login_shell_path`).
        vec![unix_path_lookup("bash")
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| "bash".into())]
    }
}

/// The first executable file called `name` in an absolute `PATH` directory.
#[cfg(not(windows))]
fn unix_path_lookup(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(name))
        .find(|candidate| {
            std::fs::metadata(candidate).is_ok_and(|metadata| {
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
            })
        })
}

/// Picks the first native Bash from `PATH`, then from a Git installation named
/// by `PATH`, then from well-known install locations. Directories that can only
/// hold a WSL launcher are skipped, so the launcher can never win.
///
/// Compiled on every platform so the rule stays unit-testable; only the Windows
/// branch above calls it.
#[cfg_attr(not(windows), allow(dead_code))]
fn select_local_bash(
    path_var: Option<&OsStr>,
    launcher_only: &[PathBuf],
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
            !launcher_only
                .iter()
                .any(|excluded| path_is_inside(directory, excluded))
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

/// Directories that can only ever yield a WSL launcher, never a native Bash.
///
/// `%SystemRoot%` is excluded whole rather than just `System32`: no native Bash
/// installs anywhere under it, and the launcher has appeared under more than one
/// of its subdirectories across Windows releases.
///
/// `%LocalAppData%\Microsoft\WindowsApps` is the app execution alias directory,
/// and it sits on the default user `PATH`. WSL 2.x — the MSI as much as the
/// Store package — registers a `bash.exe` alias there pointing at the MSIX
/// package's `wsl.exe`. On the normal Git for Windows shape, where `PATH` names
/// Git only through its `cmd` directory, that alias is the *only* `bash.exe` on
/// `PATH`: without this exclusion it wins the first pass outright, the
/// Git-derived pass never runs, and every local `bash` call silently executes
/// inside the default distribution. Nothing but aliases lives in that
/// directory, so excluding it forfeits no real interpreter.
#[cfg(windows)]
fn launcher_only_directories() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = ["SystemRoot", "windir"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .collect();
    if roots.is_empty() {
        roots.push(PathBuf::from(r"C:\Windows"));
    }
    if let Some(value) = std::env::var_os("LocalAppData") {
        roots.push(PathBuf::from(value).join("Microsoft").join("WindowsApps"));
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
///
/// Only Windows has a local PowerShell as far as Mework is concerned. A Mac or
/// Linux host is a POSIX workspace — [`crate::workspace_set::WorkspaceOs`] says
/// so and the tool is withdrawn there — even when `pwsh` happens to be
/// installed, so no candidate is offered off Windows.
pub fn local_powershell_candidates() -> Vec<String> {
    #[cfg(windows)]
    {
        select_local_powershell(&|path: &Path| path.is_file(), &|name: &str| {
            path_lookup(name).is_some()
        })
    }
    #[cfg(not(windows))]
    {
        Vec::new()
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
            fixed.push(
                PathBuf::from(value)
                    .join("PowerShell")
                    .join("7")
                    .join("pwsh.exe"),
            );
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
        fixed.push(
            PathBuf::from(value)
                .join(".dotnet")
                .join("tools")
                .join("pwsh.exe"),
        );
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
#[cfg_attr(not(windows), allow(dead_code))]
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
#[cfg_attr(not(windows), allow(dead_code))]
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
        assert_eq!(
            chosen.as_deref().map(normalized).as_deref(),
            Some("/tools/bin/bash.exe")
        );
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

    /// Installing WSL 2.x registers a `bash.exe` app execution alias under
    /// `%LocalAppData%\Microsoft\WindowsApps`, which is on the default user
    /// `PATH`. On the normal Git for Windows shape only `Git\cmd` is advertised
    /// and it carries no `bash.exe`, so that alias is the only `bash.exe` on
    /// `PATH` — it must still lose to the Bash derived from `git.exe`, or every
    /// local `bash` call runs inside the default distribution instead.
    #[test]
    fn local_bash_never_selects_the_windows_apps_alias() {
        let path = path_var(&["/users/me/appdata/local/microsoft/windowsapps", "/git/cmd"]);
        let chosen = select_local_bash(
            Some(path.as_os_str()),
            &[
                PathBuf::from("/win"),
                PathBuf::from("/users/me/appdata/local/microsoft/windowsapps"),
            ],
            &[],
            &present(&[
                "/users/me/appdata/local/microsoft/windowsapps/bash.exe",
                "/git/cmd/git.exe",
                "/git/bin/bash.exe",
            ]),
        );
        assert_eq!(
            chosen.as_deref().map(normalized).as_deref(),
            Some("/git/bin/bash.exe")
        );
    }

    #[test]
    fn local_bash_reports_nothing_when_only_the_windows_apps_alias_exists() {
        let path = path_var(&["/users/me/appdata/local/microsoft/windowsapps"]);
        let chosen = select_local_bash(
            Some(path.as_os_str()),
            &[PathBuf::from("/users/me/appdata/local/microsoft/windowsapps")],
            &[],
            &present(&["/users/me/appdata/local/microsoft/windowsapps/bash.exe"]),
        );
        // Same refusal as the System32 launcher: an actionable error beats a
        // command that silently lands on another machine.
        assert_eq!(chosen, None);
    }

    /// The exclusions only help if the real list names those directories, and
    /// the selector tests above inject their own list, so they cannot show it.
    /// This is the regression that shipped: the list covered `%SystemRoot%`
    /// alone, and installing WSL put a `bash.exe` alias outside it.
    #[cfg(windows)]
    #[test]
    fn launcher_only_directories_cover_both_launcher_homes() {
        let excluded = launcher_only_directories();
        let local_app_data =
            std::env::var_os("LocalAppData").expect("Windows always sets LocalAppData");
        let alias = PathBuf::from(local_app_data)
            .join("Microsoft")
            .join("WindowsApps");
        assert!(
            excluded.iter().any(|root| path_is_inside(&alias, root)),
            "the WSL bash.exe alias directory must be excluded: {excluded:?}"
        );
        let system_root = std::env::var_os("SystemRoot").expect("Windows always sets SystemRoot");
        let system32 = PathBuf::from(system_root).join("System32");
        assert!(
            excluded.iter().any(|root| path_is_inside(&system32, root)),
            "the System32 launcher must stay excluded: {excluded:?}"
        );
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
        assert_eq!(
            chosen.as_deref().map(normalized).as_deref(),
            Some("/git/bin/bash.exe")
        );
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
    fn local_resolution_picks_the_workspace_env_table() {
        let assets = assets_with(
            Vec::new(),
            &[
                ("local|C:/work/app", &[("FOO", "bar")]),
                ("local|C:/work/other", &[("FOO", "other")]),
            ],
        );
        let runner = resolve_shell_runner(&assets, None, Some("C:/work/app")).unwrap();
        assert_eq!(
            runner,
            ShellRunner::Local {
                env: [("FOO".to_owned(), "bar".to_owned())].into_iter().collect()
            }
        );
    }

    #[test]
    fn machine_resolution_without_a_workspace_carries_no_variables() {
        // A machine-wide table from before variables moved onto workspaces is
        // never read: the variables are the workspace's, not the machine's.
        let assets = assets_with(Vec::new(), &[("local", &[("FOO", "bar")])]);
        assert_eq!(
            resolve_shell_runner(&assets, None, None).unwrap(),
            ShellRunner::default()
        );
        assert_eq!(
            resolve_shell_runner(&assets, None, Some("C:/work/app")).unwrap(),
            ShellRunner::default()
        );
    }

    #[test]
    fn wsl_resolution_keys_env_by_distro_and_path_and_validates_the_name() {
        if !host_platform().has_wsl() {
            return;
        }
        let assets = assets_with(
            Vec::new(),
            &[
                ("wsl:Ubuntu|/home/dev/app", &[("A", "1")]),
                ("wsl:Debian|/home/dev/app", &[("A", "2")]),
            ],
        );
        let runner = resolve_shell_runner(
            &assets,
            Some(&RunTarget::Wsl {
                distro: "Ubuntu".into(),
            }),
            Some("/home/dev/app"),
        )
        .unwrap();
        let ShellRunner::Wsl { distro, env } = runner else {
            panic!("expected wsl runner");
        };
        assert_eq!(distro, "Ubuntu");
        assert_eq!(env.get("A").map(String::as_str), Some("1"));

        let injection = RunTarget::Wsl {
            distro: "Ubuntu; rm -rf /".into(),
        };
        assert!(resolve_shell_runner(&assets, Some(&injection), None).is_err());
    }

    /// A document that travelled from a Windows machine still names WSL
    /// distributions. On a host that has none, resolution has to say that —
    /// otherwise the call reaches `wsl.exe` and the user is told a file is
    /// missing when what is missing is the machine.
    #[test]
    fn a_wsl_target_is_refused_by_a_host_that_has_no_wsl() {
        let assets = assets_with(Vec::new(), &[]);
        let resolved = resolve_shell_runner(
            &assets,
            Some(&RunTarget::Wsl {
                distro: "Ubuntu".into(),
            }),
            None,
        );
        if host_platform().has_wsl() {
            assert!(matches!(resolved, Ok(ShellRunner::Wsl { .. })));
            return;
        }
        let error = resolved.unwrap_err();
        assert!(error.contains("Ubuntu"), "{error}");
        assert!(
            error.contains(host_platform().display_name()),
            "the refusal has to name the machine it is about: {error}"
        );
    }

    #[test]
    fn deleted_ssh_machine_fails_resolution_instead_of_falling_back_to_local() {
        let assets = assets_with(Vec::new(), &[]);
        let error = resolve_shell_runner(
            &assets,
            Some(&RunTarget::Ssh {
                machine_id: "m1".into(),
            }),
            None,
        )
        .unwrap_err();
        assert!(
            error.contains("no longer in the machine catalog"),
            "{error}"
        );
    }

    #[test]
    fn ssh_resolution_copies_endpoint_fields_from_the_catalog() {
        let machine = SshMachineConfig {
            id: "m1".into(),
            name: "devbox".into(),
            host: "user@devbox.local".into(),
            port: 2222,
            identity_file: "C:/keys/id_ed25519".into(),
            ..Default::default()
        };
        let assets = assets_with(vec![machine], &[("ssh:m1|~/app", &[("K", "v")])]);
        let runner = resolve_shell_runner(
            &assets,
            Some(&RunTarget::Ssh {
                machine_id: "m1".into(),
            }),
            Some("~/app"),
        )
        .unwrap();
        let ShellRunner::Ssh {
            host,
            port,
            identity_file,
            env,
        } = runner
        else {
            panic!("expected ssh runner");
        };
        assert_eq!(host, "user@devbox.local");
        assert_eq!(port, 2222);
        assert_eq!(identity_file, "C:/keys/id_ed25519");
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
            &r"C:\proj",
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
        assert_eq!(args.len(), 11);
        assert_eq!(
            crate::remote_shell::tests::decode_posix_line(&args[10]),
            r"cd ~/'my work' || exit 1; exec env 'FOO=a'\''b' bash --noprofile --norc -c 'echo '\''hi'\'''"
        );
    }

    #[test]
    fn ssh_args_omit_port_identity_and_cd_when_unset() {
        let args = ssh_shell_args("devbox", 0, "", "", &BTreeMap::new(), "pwd");
        assert_eq!(
            args[..6],
            ["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "--", "devbox"]
        );
        assert_eq!(args.len(), 7);
        assert_eq!(
            crate::remote_shell::tests::decode_posix_line(&args[6]),
            "exec bash --noprofile --norc -c 'pwd'"
        );
    }

    /// A command whose neutral line would pass Linux's one-argument limit is
    /// sent the way it always was, rather than refused by the remote kernel.
    #[test]
    fn ssh_args_fall_back_to_the_plain_script_past_the_argument_limit() {
        let dense = "\"".repeat(40 * 1024);
        let args = ssh_shell_args("devbox", 0, "", "", &BTreeMap::new(), &dense);
        let sent = args.last().unwrap();
        assert!(sent.len() <= MAX_NEUTRAL_LINE_BYTES, "{}", sent.len());
        assert_eq!(
            *sent,
            format!("exec bash --noprofile --norc -c {}", sh_single_quote(&dense))
        );
        let ordinary = "x".repeat(40 * 1024);
        let args = ssh_shell_args("devbox", 0, "", "", &BTreeMap::new(), &ordinary);
        assert!(args.last().unwrap().starts_with("exec /bin/sh -c 'eval"));
    }

    #[test]
    fn wsl_list_output_parses_utf16_and_utf8_tables() {
        let table = "  NAME            STATE           VERSION\r\n* Ubuntu          Running         2\r\n  Debian          Stopped         1\r\n  kali-linux      Stopped         2\r\n";
        let utf16: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain(table.encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        for text in [
            decode_wsl_output(table.as_bytes()),
            decode_wsl_output(&utf16),
        ] {
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
        let table =
            "  NAME            STATE           VERSION\n  My Distro Name  Stopped         2\n";
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

        let wsl = wsl_shell_args("Ubuntu", r"C:\proj", &env, "pwd").join(" ");
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

    /// One invocation shape serves both the one-shot scripts and the language
    /// servers that stay up: the script is the only variable, the root is the
    /// script's own business, and a local runner is refused outright.
    #[test]
    fn a_remote_script_invocation_is_the_shell_legs_argv() {
        let (programs, args) = remote_script_invocation(
            &ShellRunner::Wsl {
                distro: "Ubuntu".into(),
                env: BTreeMap::new(),
            },
            "exec 'rust-analyzer'",
        )
        .unwrap();
        assert_eq!(programs, ["wsl.exe"]);
        assert_eq!(
            args,
            wsl_shell_args("Ubuntu", "/", &BTreeMap::new(), "exec 'rust-analyzer'")
        );

        let (programs, args) = remote_script_invocation(
            &ShellRunner::Ssh {
                host: "devbox".into(),
                port: 2222,
                identity_file: String::new(),
                env: BTreeMap::new(),
            },
            "exec 'gopls'",
        )
        .unwrap();
        assert_eq!(programs, ssh_client_candidates());
        assert_eq!(
            args,
            ssh_shell_args("devbox", 2222, "", "", &BTreeMap::new(), "exec 'gopls'")
        );

        assert!(remote_script_invocation(&ShellRunner::default(), "pwd").is_err());
    }

    /// The factory Windows sshd hands the POSIX line to `cmd.exe`, which
    /// complains about `exec` in the console language and exits 9009;
    /// PowerShell names the exception category regardless of locale. A POSIX
    /// shell's own failures must not be mistaken for either.
    #[test]
    fn a_cmd_or_powershell_answer_is_recognized_in_any_language() {
        assert!(answered_by_non_posix_shell(
            Some(1),
            "'exec' is not recognized as an internal or external command,\noperable program or batch file."
        ));
        assert!(answered_by_non_posix_shell(
            Some(1),
            "'exec' 不是内部或外部命令，也不是可运行的程序\n或批处理文件。"
        ));
        assert!(answered_by_non_posix_shell(Some(9009), "Der Befehl ist falsch."));
        assert!(answered_by_non_posix_shell(
            Some(1),
            "exec : The term 'exec' is not recognized as the name of a cmdlet, function, script file, or operable program."
        ));
        assert!(answered_by_non_posix_shell(
            Some(1),
            "    + CategoryInfo          : ObjectNotFound: (exec:String) [], CommandNotFoundException"
        ));
        // What a Chinese Windows actually sends: the same sentence in GBK,
        // unreadable once decoded as UTF-8, with only the quoted head intact.
        let gbk = String::from_utf8_lossy(
            b"'exec' \xb2\xbb\xca\xc7\xc4\xda\xb2\xbf\xbb\xf2\xcd\xe2\xb2\xbf\xc3\xfc\xc1\xee\xa3\xac\r\n",
        );
        assert!(answered_by_non_posix_shell(Some(1), &gbk));
        assert!(answered_by_non_posix_shell(
            Some(1),
            "'exec'은(는) 내부 또는 외부 명령, 실행할 수 있는 프로그램, 또는 배치 파일이 아닙니다."
        ));

        assert!(!answered_by_non_posix_shell(
            Some(1),
            "bash: line 1: cd: /srv/missing: No such file or directory"
        ));
        assert!(!answered_by_non_posix_shell(
            Some(255),
            "ssh: connect to host devbox port 22: Connection refused"
        ));
        assert!(!answered_by_non_posix_shell(Some(127), "bash: rg: command not found"));
        assert!(!answered_by_non_posix_shell(
            Some(127),
            "bash: line 1: exec: bash: not found"
        ));
    }

    #[test]
    fn a_reply_that_did_not_survive_decoding_is_not_shown() {
        assert_eq!(
            legible_remote_reply("  cd: /srv/missing: No such file or directory\n"),
            Some("cd: /srv/missing: No such file or directory")
        );
        assert_eq!(
            legible_remote_reply("'exec' 不是内部或外部命令"),
            Some("'exec' 不是内部或外部命令")
        );
        assert_eq!(legible_remote_reply(" \r\n"), None);
        let gbk = String::from_utf8_lossy(b"'exec' \xb2\xbb\xca\xc7\r\n");
        assert_eq!(legible_remote_reply(&gbk), None);
    }
}
