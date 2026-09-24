//! Dev servers of a workspace on an SSH machine, run there.
//!
//! A remote workspace's `.mework/launch.json` describes a server that belongs on
//! that machine: its command is on that machine's `PATH`, its `cwd` is a
//! directory there, and its port is one of that machine's ports. So all of it
//! happens there, through the machine's agent ([`crate::remote_link`]):
//!
//! * the file is read there, and parsed here by the same parser a local one is;
//! * the port is probed and, under `autoPort`, allocated there
//!   (`mework-remote net probe` / `free-port`);
//! * the server is started there as one of the agent's sessions, which is what
//!   lets it survive a dropped link — its output keeps accumulating on the
//!   machine and arrives, in order, when the link is back;
//! * readiness is waited out there (`net wait`), so a slow link costs one round
//!   trip rather than one per probe.
//!
//! The registry ([`crate::preview_servers`]) keeps the books for these servers
//! exactly as it does for local ones; this module is the machine it asks.
//!
//! A machine the agent does not serve — no build for its platform, a home it
//! cannot write to — has no preview: every other remote operation can fall back
//! to one `ssh` command per call, but a server has to outlive the call that
//! started it, and only the agent gives it a place to live.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use remote_agent::client::{Link, RemoteProcess};
use remote_agent::protocol::SELF_PROGRAM;

use crate::cancel::CancelSignal;
use crate::preview_servers::{
    PreviewServerConfig, PreviewStartError, PreviewStartErrorKind, RemoteServerHost,
    REMOTE_EXIT_NO_CWD,
};
use crate::remote_link::{self, Route};
use crate::remote_shell;
use crate::run_environment::{quote_remote_path, sh_single_quote, ShellRunner};
use crate::shell_backend::ScriptDialect;

/// How long an operation waits for the machine's link before it gives up. Long
/// enough for a first connection that installs the agent.
const LINK_PATIENCE: Duration = Duration::from_secs(90);
/// How long reading `launch.json` may take.
const READ_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a port probe may take, the machine's own 500 ms connect probes included.
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a dev server outlives a link that dropped: as long as a terminal does,
/// so a laptop closed over lunch comes back to its servers.
const SERVER_ORPHAN_TTL: Duration = Duration::from_secs(2 * 60 * 60);
/// The largest `launch.json` read from a machine. Real ones are a few hundred bytes.
const MAX_LAUNCH_JSON_BYTES: u64 = 1 << 20;

/// Exit code of the read script when the file is not there.
const EXIT_MISSING: i32 = 3;
/// Exit code of the read script when the file is there but cannot be read.
const EXIT_UNREADABLE: i32 = 4;
/// Exit code of the read script when the file is over [`MAX_LAUNCH_JSON_BYTES`].
const EXIT_TOO_LARGE: i32 = 5;

/// One SSH machine, as the preview reaches it.
#[derive(Clone, Debug)]
pub struct RemoteMachine {
    runner: ShellRunner,
    /// The machine's environment key ([`crate::run_environment::env_key`]).
    key: String,
    /// The machine's name as the user knows it.
    label: String,
    /// How long an operation waits for the link.
    patience: Duration,
}

/// How long a read the pane repeats every second or two waits for a link that is down. The
/// pane says the machine is away and asks again on its next tick; waiting out a reconnect
/// here would only stack its polls up behind one another.
pub const POLL_PATIENCE: Duration = Duration::from_secs(4);

/// What reading a remote workspace's `launch.json` found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteLaunchJson {
    /// The workspace root as the machine resolved it: absolute, `~` expanded, links followed.
    /// `${workspaceFolder}` and a relative `cwd` resolve against this.
    pub root: String,
    pub content: RemoteLaunchJsonContent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteLaunchJsonContent {
    Read(String),
    Missing,
    Unreadable(String),
}

impl RemoteMachine {
    pub fn new(runner: ShellRunner, key: String, label: String) -> Self {
        Self {
            runner,
            key,
            label,
            patience: LINK_PATIENCE,
        }
    }

    /// The same machine, for an operation that should not wait long for its link.
    pub fn with_patience(mut self, patience: Duration) -> Self {
        self.patience = patience;
        self
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn runner(&self) -> &ShellRunner {
        &self.runner
    }

    /// The registry's key for a directory on this machine. It names no path on this computer —
    /// the machine's key comes first — so it can never collide with a local worktree.
    pub fn worktree_key(&self, root: &str) -> PathBuf {
        PathBuf::from(format!("{}::{root}", self.key))
    }

    /// The machine's link to its agent, waiting for it to come up if it is still connecting.
    pub fn link(&self) -> Result<Link, String> {
        match remote_link::route(&self.runner, self.patience) {
            Route::Agent(link, _) => Ok(link),
            Route::Unreachable(error) => Err(error),
            Route::Legacy => Err(format!(
                "Previews on {} need Mework's agent on that machine, and it is not answering yet — it may still be connecting or installing, or it cannot run there. Terminals and file tools still work over plain SSH.",
                self.label
            )),
        }
    }

    fn script_argv(&self, script: &str) -> Vec<String> {
        self.runner
            .agent_shell()
            .cloned()
            .unwrap_or_default()
            .script_argv(script)
    }

    fn dialect(&self) -> ScriptDialect {
        self.runner.script_dialect()
    }

    /// Runs one of the agent's own `net` verbs and returns what it printed.
    fn net(&self, arguments: &[String], timeout: Duration) -> Result<(Option<i32>, String), String> {
        let link = self.link()?;
        let mut argv = vec![SELF_PROGRAM.to_owned(), "net".to_owned()];
        argv.extend(arguments.iter().cloned());
        let output = remote_link::run_script_on(
            &link,
            &self.runner,
            argv,
            None,
            timeout,
            &CancelSignal::default(),
        )?;
        Ok((
            output.status,
            String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        ))
    }

    /// Reads `<root>/.mework/launch.json` on the machine.
    pub fn read_launch_json(&self, root: &str) -> Result<RemoteLaunchJson, String> {
        check_text(root, "workspace root")?;
        let script = match self.dialect() {
            ScriptDialect::Posix => posix_read_script(root),
            ScriptDialect::PowerShell => powershell_read_script(root),
        };
        let link = self.link()?;
        let output = remote_link::run_script_on(
            &link,
            &self.runner,
            self.script_argv(&script),
            None,
            READ_TIMEOUT,
            &CancelSignal::default(),
        )?;
        let text = String::from_utf8_lossy(&output.stdout);
        let (resolved, rest) = text.split_once('\n').unwrap_or((text.as_ref(), ""));
        let resolved = resolved.trim_end_matches('\r').to_owned();
        let content = match output.status {
            Some(0) => RemoteLaunchJsonContent::Read(rest.to_owned()),
            Some(EXIT_MISSING) => RemoteLaunchJsonContent::Missing,
            Some(EXIT_UNREADABLE) => RemoteLaunchJsonContent::Unreadable(
                output.stderr.trim().to_owned(),
            ),
            Some(EXIT_TOO_LARGE) => RemoteLaunchJsonContent::Unreadable(format!(
                "the file is larger than {MAX_LAUNCH_JSON_BYTES} bytes"
            )),
            Some(REMOTE_EXIT_NO_CWD) => {
                return Err(format!(
                    "The workspace directory {root} does not exist on {}",
                    self.label
                ))
            }
            status => {
                return Err(format!(
                    "Could not read .mework/launch.json on {} (exit {status:?}): {}",
                    self.label,
                    output.stderr.trim()
                ))
            }
        };
        Ok(RemoteLaunchJson {
            root: if resolved.is_empty() { root.to_owned() } else { resolved },
            content,
        })
    }
}

impl RemoteServerHost for RemoteMachine {
    fn machine_key(&self) -> &str {
        &self.key
    }

    fn machine_label(&self) -> &str {
        &self.label
    }

    fn spawn(&self, config: &PreviewServerConfig) -> Result<RemoteProcess, PreviewStartError> {
        let failed = |message: String| PreviewStartError {
            message,
            kind: PreviewStartErrorKind::SpawnError,
            code: None,
            exit_code: None,
            output: None,
        };
        let command = config
            .command
            .as_deref()
            .ok_or_else(|| failed(crate::preview_servers::NO_COMMAND_MESSAGE.to_owned()))?;
        let cwd = remote_path(&config.cwd);
        let mut env: Vec<(String, String)> = config
            .env
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        // Colour, as a local server gets it; then the port the machine reserved, which wins over
        // anything the entry configured, the same order a local start applies them in.
        env.insert(0, ("FORCE_COLOR".to_owned(), "1".to_owned()));
        env.push(("PORT".to_owned(), config.port.to_string()));
        let script = match self.dialect() {
            ScriptDialect::Posix => posix_launch_script(&cwd, command, &config.args, &env),
            ScriptDialect::PowerShell => {
                powershell_launch_script(&cwd, command, &config.args, &env)
            }
        }
        .map_err(failed)?;
        let unreachable = |message: String| PreviewStartError {
            message,
            kind: PreviewStartErrorKind::Unreachable,
            code: None,
            exit_code: None,
            output: None,
        };
        let link = self.link().map_err(unreachable)?;
        remote_link::spawn_service(
            &link,
            &self.runner,
            self.script_argv(&script),
            None,
            &[],
            SERVER_ORPHAN_TTL,
            "preview",
        )
        .map_err(unreachable)
    }

    fn port_state(&self, port: u16) -> Result<(bool, bool), String> {
        let (status, answer) = self.net(&["probe".into(), port.to_string()], PROBE_TIMEOUT)?;
        if status != Some(0) {
            return Err(format!("the port probe failed (exit {status:?})"));
        }
        let parsed: serde_json::Value =
            serde_json::from_str(&answer).map_err(|_| format!("unexpected probe answer: {answer}"))?;
        Ok((
            parsed["bindable"].as_bool().unwrap_or(false),
            parsed["listening"].as_bool().unwrap_or(false),
        ))
    }

    fn free_port(&self) -> Result<u16, String> {
        let (status, answer) = self.net(&["free-port".into()], PROBE_TIMEOUT)?;
        if status != Some(0) {
            return Err(format!("no free port (exit {status:?})"));
        }
        let parsed: serde_json::Value =
            serde_json::from_str(&answer).map_err(|_| format!("unexpected answer: {answer}"))?;
        parsed["port"]
            .as_u64()
            .and_then(|port| u16::try_from(port).ok())
            .filter(|port| *port != 0)
            .ok_or_else(|| format!("unexpected answer: {answer}"))
    }

    fn wait_ready(&self, port: u16, timeout: Duration, https: bool) -> bool {
        let mut arguments = vec![
            "wait".to_owned(),
            port.to_string(),
            timeout.as_millis().to_string(),
        ];
        if https {
            arguments.push("--https".into());
        }
        // A little longer than the machine's own deadline, so the answer is the machine's.
        matches!(
            self.net(&arguments, timeout + Duration::from_secs(15)),
            Ok((Some(0), _))
        )
    }
}

/// A resolved configuration path as the machine spells it. The parser joins with this
/// computer's separator; a remote root is POSIX (or a Windows path PowerShell accepts either
/// way), so the join is normalised back to forward slashes.
fn remote_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    if text.contains(":\\") || text.starts_with("\\\\") {
        // A Windows machine's own spelling: leave it as the machine wrote it.
        return text.into_owned();
    }
    text.replace('\\', "/")
}

fn check_text(text: &str, label: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err(format!("The {label} is empty"));
    }
    if text.chars().any(char::is_control) {
        return Err(format!("The {label} contains control characters"));
    }
    Ok(())
}

/// Prints the resolved root on the first line, then the file's bytes.
fn posix_read_script(root: &str) -> String {
    format!(
        "cd -- {root} 2>/dev/null || exit {REMOTE_EXIT_NO_CWD}\n\
         pwd -P\n\
         f=.mework/launch.json\n\
         [ -e \"$f\" ] || exit {EXIT_MISSING}\n\
         if [ -d \"$f\" ] || [ ! -r \"$f\" ]; then printf '%s\\n' 'the file cannot be read' >&2; exit {EXIT_UNREADABLE}; fi\n\
         size=$(wc -c < \"$f\" 2>/dev/null | tr -d ' ')\n\
         [ \"${{size:-0}}\" -le {MAX_LAUNCH_JSON_BYTES} ] || exit {EXIT_TOO_LARGE}\n\
         cat -- \"$f\" || exit {EXIT_UNREADABLE}\n",
        root = quote_remote_path(root.trim()),
    )
}

/// [`posix_read_script`] for a machine whose agent shell is PowerShell.
fn powershell_read_script(root: &str) -> String {
    format!(
        "$ErrorActionPreference = 'Stop'\n$ProgressPreference = 'SilentlyContinue'\n\
         {helper}\
         try {{ Set-Location -LiteralPath (Native-Path {root}) }} catch {{ exit {REMOTE_EXIT_NO_CWD} }}\n\
         $R = (Get-Location).ProviderPath\n\
         $o = [Console]::OpenStandardOutput()\n\
         $h = [System.Text.Encoding]::UTF8.GetBytes($R + \"`n\")\n\
         $o.Write($h, 0, $h.Length)\n\
         $f = [System.IO.Path]::Combine($R, '.mework', 'launch.json')\n\
         if (-not [System.IO.File]::Exists($f)) {{ $o.Flush(); exit {EXIT_MISSING} }}\n\
         try {{ $b = [System.IO.File]::ReadAllBytes($f) }} catch {{ [Console]::Error.WriteLine($_.Exception.Message); $o.Flush(); exit {EXIT_UNREADABLE} }}\n\
         if ($b.Length -gt {MAX_LAUNCH_JSON_BYTES}) {{ $o.Flush(); exit {EXIT_TOO_LARGE} }}\n\
         $o.Write($b, 0, $b.Length)\n\
         $o.Flush()\n\
         exit 0\n",
        helper = POWERSHELL_NATIVE_PATH,
        root = remote_shell::ps_single_quote(root.trim()),
    )
}

/// Maps the POSIX spellings a Windows machine's workspace may carry onto its own paths.
const POWERSHELL_NATIVE_PATH: &str = "function Native-Path([string]$P) {\n\
     if ($P -eq '~') { return $HOME }\n\
     if ($P.StartsWith('~/') -or $P.StartsWith('~\\')) { return [System.IO.Path]::Combine($HOME, $P.Substring(2)) }\n\
     if ($P -match '^/cygdrive/([A-Za-z])(/.*)?$' -or $P -match '^/([A-Za-z])(/.*)?$') { $rest = if ($Matches[2]) { $Matches[2] } else { '/' }; return $Matches[1].ToUpper() + ':' + $rest }\n\
     return $P\n\
     }\n";

fn check_launch(cwd: &str, command: &str, args: &[String], env: &[(String, String)]) -> Result<(), String> {
    check_text(cwd, "server's working directory")?;
    check_text(command, "server's command")?;
    if args.iter().any(|argument| argument.chars().any(char::is_control)) {
        return Err("The server's arguments contain control characters".into());
    }
    for (name, value) in env {
        crate::run_environment::validate_env_var_name(name)?;
        if value.chars().any(char::is_control) {
            return Err(format!(
                "The server's environment variable {name} contains control characters"
            ));
        }
    }
    Ok(())
}

/// The script that starts one configured server on a POSIX machine.
///
/// Every fragment the file supplied is single-quoted: the command, its arguments and its
/// variables come from a `launch.json` a repository may have shipped, and none of them may
/// rewrite the script. The server is `exec`ed, so the agent's session *is* the server and a stop
/// signals its whole group.
///
/// A command the non-interactive `PATH` does not have is tried once more through the account's
/// own shell, interactive and login: version managers (nvm, asdf, pyenv) commonly extend `PATH`
/// only there, and that is the `PATH` the user's own terminal on the machine has. Only a shell
/// that reads `-c` the POSIX way is used; anything else reports the command missing.
fn posix_launch_script(
    cwd: &str,
    command: &str,
    args: &[String],
    env: &[(String, String)],
) -> Result<String, String> {
    check_launch(cwd, command, args, env)?;
    let mut exports = String::new();
    for (name, value) in env {
        exports.push_str(&format!("export {name}={}\n", sh_single_quote(value)));
    }
    let mut argv = sh_single_quote(command);
    for argument in args {
        argv.push(' ');
        argv.push_str(&sh_single_quote(argument));
    }
    let missing = sh_single_quote(&format!(
        "{command}: command not found on the remote machine's PATH"
    ));
    Ok(format!(
        "cd -- {cwd} 2>/dev/null || {{ printf '%s\\n' {no_cwd} >&2; exit {REMOTE_EXIT_NO_CWD}; }}\n\
         {exports}\
         if command -v {cmd} >/dev/null 2>&1; then exec {argv}; fi\n\
         case \"${{SHELL:-}}\" in\n\
         */bash|*/zsh|*/ksh|*/sh|*/dash)\n\
         exec \"$SHELL\" -lic 'command -v \"$0\" >/dev/null 2>&1 || {{ printf \"%s\\n\" \"$0: command not found on the remote machine'\"'\"'s PATH\" >&2; exit 127; }}; exec \"$0\" \"$@\"' {argv} ;;\n\
         esac\n\
         printf '%s\\n' {missing} >&2\n\
         exit 127\n",
        cwd = quote_remote_path(cwd.trim()),
        no_cwd = sh_single_quote(&format!("The working directory does not exist: {cwd}")),
        cmd = sh_single_quote(command),
    ))
}

/// [`posix_launch_script`] for a Windows machine whose agent shell is PowerShell: the entry's
/// variables set, the command found as an application (127 when it is not), then run as the last
/// thing on the line so its streams are the agent's own.
fn powershell_launch_script(
    cwd: &str,
    command: &str,
    args: &[String],
    env: &[(String, String)],
) -> Result<String, String> {
    check_launch(cwd, command, args, env)?;
    let mut script = String::from(
        "$ErrorActionPreference = 'Stop'\n$ProgressPreference = 'SilentlyContinue'\n",
    );
    script.push_str(POWERSHELL_NATIVE_PATH);
    script.push_str(&format!(
        "try {{ Set-Location -LiteralPath (Native-Path {}) }} catch {{ [Console]::Error.WriteLine({}); exit {REMOTE_EXIT_NO_CWD} }}\n\
         [System.Environment]::CurrentDirectory = (Get-Location).ProviderPath\n",
        remote_shell::ps_single_quote(cwd.trim()),
        remote_shell::ps_single_quote(&format!("The working directory does not exist: {cwd}")),
    ));
    for (name, value) in env {
        script.push_str(&format!(
            "[System.Environment]::SetEnvironmentVariable({}, {})\n",
            remote_shell::ps_single_quote(name),
            remote_shell::ps_single_quote(value)
        ));
    }
    script.push_str(&format!(
        "$server = Get-Command -Name {} -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1\n\
         if ($null -eq $server) {{ [Console]::Error.WriteLine({}); exit 127 }}\n\
         & $server.Source",
        remote_shell::ps_single_quote(command),
        remote_shell::ps_single_quote(&format!(
            "{command}: command not found on the remote machine's PATH"
        )),
    ));
    for argument in args {
        script.push(' ');
        script.push_str(&remote_shell::ps_single_quote(argument));
    }
    script.push_str("\nexit $LASTEXITCODE\n");
    Ok(script)
}

/// Shared handle the registry keeps for a machine's servers.
pub fn host(machine: &RemoteMachine) -> Arc<dyn RemoteServerHost> {
    Arc::new(machine.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_launch_script_quotes_everything_the_file_supplied() {
        let script = posix_launch_script(
            "~/app/web",
            "npm",
            &["run".into(), "dev; rm -rf /".into()],
            &[("PORT".into(), "5173".into()), ("NAME".into(), "it's".into())],
        )
        .unwrap();
        assert!(script.starts_with("cd -- ~/'app/web' 2>/dev/null"), "{script}");
        assert!(script.contains("export PORT='5173'\n"), "{script}");
        assert!(script.contains("export NAME='it'\\''s'\n"), "{script}");
        assert!(script.contains("exec 'npm' 'run' 'dev; rm -rf /'; fi"), "{script}");
        assert!(script.contains("-lic"), "the interactive fallback is there: {script}");
        assert!(posix_launch_script("/app", "npm", &[], &[("BAD NAME".into(), "x".into())]).is_err());
        assert!(posix_launch_script("/app", "npm\n", &[], &[]).is_err());
    }

    #[test]
    fn the_launch_script_runs_under_sh() {
        let directory = tempfile::tempdir().unwrap();
        let script = posix_launch_script(
            &directory.path().to_string_lossy(),
            "sh",
            &["-c".into(), "printf '%s %s' \"$PORT\" \"$(pwd -P)\"".into()],
            &[("PORT".into(), "4321".into())],
        )
        .unwrap();
        let output = std::process::Command::new("sh").arg("-c").arg(&script).output().unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        let expected = std::fs::canonicalize(directory.path()).unwrap();
        assert_eq!(text, format!("4321 {}", expected.display()));

        let missing = posix_launch_script("/definitely/not/here", "sh", &[], &[]).unwrap();
        let output = std::process::Command::new("sh").arg("-c").arg(&missing).output().unwrap();
        assert_eq!(output.status.code(), Some(REMOTE_EXIT_NO_CWD));
    }

    #[test]
    fn the_read_script_reports_the_root_and_the_file() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().to_string_lossy().into_owned();
        let run = |root: &str| {
            std::process::Command::new("sh")
                .arg("-c")
                .arg(posix_read_script(root))
                .output()
                .unwrap()
        };
        let output = run(&root);
        assert_eq!(output.status.code(), Some(EXIT_MISSING));
        std::fs::create_dir_all(directory.path().join(".mework")).unwrap();
        std::fs::write(directory.path().join(".mework/launch.json"), "{\"configurations\":[]}").unwrap();
        let output = run(&root);
        assert_eq!(output.status.code(), Some(0));
        let text = String::from_utf8_lossy(&output.stdout);
        let (first, rest) = text.split_once('\n').unwrap();
        assert_eq!(Path::new(first), std::fs::canonicalize(directory.path()).unwrap());
        assert_eq!(rest, "{\"configurations\":[]}");
    }

    #[test]
    fn a_joined_configuration_path_reads_the_machines_way() {
        assert_eq!(remote_path(Path::new("/home/u/app/web")), "/home/u/app/web");
        assert_eq!(remote_path(Path::new("/home/u/app\\web")), "/home/u/app/web");
        assert_eq!(remote_path(Path::new("C:\\Users\\u\\app")), "C:\\Users\\u\\app");
    }

    /// A server for the end-to-end tests, on the machine's IPv4 loopback only — the way Flask,
    /// Django and most servers that name `127.0.0.1` listen — so a page asking for `localhost`
    /// only reaches it when the machine tries each address `localhost` has. `GET /big` answers a
    /// mebibyte, `GET /page` a page that fetches `/data` once it has loaded; anything else
    /// answers the entry's name, the machine's and the path.
    const E2E_PYTHON_SERVER: &str = r#"import http.server, os, socket
PAGE = '<!doctype html><title>e2e</title><p id="out">waiting</p><script>fetch("/data").then(r => r.text()).then(t => { document.getElementById("out").textContent = "fetched: " + t; })</script>'
class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def do_GET(self):
        kind = "text/plain"
        if self.path == "/big":
            body = b"x" * 1048576
        elif self.path == "/page":
            body = PAGE.encode()
            kind = "text/html"
        else:
            body = ("mework-e2e %s %s %s" % (os.environ["E2E_NAME"], socket.gethostname(), self.path)).encode()
        self.send_response(200)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, format, *args):
        print("served %s" % self.path, flush=True)
server = http.server.ThreadingHTTPServer(("127.0.0.1", int(os.environ["PORT"])), Handler)
print("listening on %s" % os.environ["PORT"], flush=True)
server.serve_forever()
"#;

    /// [`E2E_PYTHON_SERVER`] in the Windows PowerShell every Windows machine has.
    const E2E_POWERSHELL_SERVER: &str = r#"$ErrorActionPreference = 'Stop'
$port = [int]$env:PORT
$listener = New-Object System.Net.Sockets.TcpListener([System.Net.IPAddress]::Loopback, $port)
$listener.Start()
[Console]::Out.WriteLine("listening on $port")
while ($true) {
  $client = $listener.AcceptTcpClient()
  try {
    $stream = $client.GetStream()
    $reader = New-Object System.IO.StreamReader($stream, [System.Text.Encoding]::ASCII)
    $request = $reader.ReadLine()
    while ($true) { $header = $reader.ReadLine(); if ([string]::IsNullOrEmpty($header)) { break } }
    $path = ($request -split ' ')[1]
    $kind = 'text/plain'
    if ($path -eq '/big') { $text = 'x' * 1048576 }
    elseif ($path -eq '/page') { $text = '<!doctype html><title>e2e</title><p id="out">waiting</p><script>fetch("/data").then(r => r.text()).then(t => { document.getElementById("out").textContent = "fetched: " + t; })</script>'; $kind = 'text/html' }
    else { $text = "mework-e2e $env:E2E_NAME $env:COMPUTERNAME $path" }
    $body = [System.Text.Encoding]::UTF8.GetBytes($text)
    $head = [System.Text.Encoding]::ASCII.GetBytes("HTTP/1.1 200 OK`r`nContent-Type: $kind`r`nContent-Length: $($body.Length)`r`nConnection: close`r`n`r`n")
    $stream.Write($head, 0, $head.Length)
    if (-not $request.StartsWith('HEAD ')) { $stream.Write($body, 0, $body.Length) }
    $stream.Flush()
    [Console]::Out.WriteLine("served $path")
  } catch {
    [Console]::Error.WriteLine($_.Exception.Message)
  } finally {
    $client.Close()
  }
}
"#;

    /// Connects through the machine's tunnel the way a page's engine does, by name.
    fn e2e_socks_connect(proxy_port: u16, host: &str, port: u16) -> (std::net::TcpStream, u8) {
        use std::io::{Read, Write};
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", proxy_port)).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
        stream.write_all(&[5, 1, 0]).unwrap();
        let mut chosen = [0u8; 2];
        stream.read_exact(&mut chosen).unwrap();
        assert_eq!(chosen, [5, 0]);
        let mut request = vec![5, 1, 0, 3, host.len() as u8];
        request.extend_from_slice(host.as_bytes());
        request.extend_from_slice(&port.to_be_bytes());
        stream.write_all(&request).unwrap();
        let mut reply = [0u8; 10];
        stream.read_exact(&mut reply).unwrap();
        (stream, reply[1])
    }

    /// `GET path` through the tunnel; the body, and how long the whole exchange took.
    fn e2e_fetch(proxy_port: u16, host: &str, port: u16, path: &str) -> (String, Duration) {
        use std::io::{Read, Write};
        let started = std::time::Instant::now();
        let (mut stream, reply) = e2e_socks_connect(proxy_port, host, port);
        assert_eq!(reply, 0, "the tunnel refused {host}:{port}");
        write!(stream, "GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n").unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        let text = String::from_utf8_lossy(&response).into_owned();
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        (body.to_owned(), started.elapsed())
    }

    /// A Chromium for the page leg of the end-to-end tests: `MEWORK_CHROME_PATH`, or Chrome or
    /// Chromium where they install themselves.
    fn e2e_chrome() -> Option<PathBuf> {
        std::env::var_os("MEWORK_CHROME_PATH")
            .map(PathBuf::from)
            .into_iter()
            .chain(
                [
                    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
                    "/usr/bin/google-chrome",
                    "/usr/bin/chromium",
                    "/usr/bin/chromium-browser",
                    "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
                ]
                .map(PathBuf::from),
            )
            .find(|path| path.is_file())
    }

    /// The page at `url` as Chromium renders it with every connection — loopback included — going
    /// through `proxy`, the way the preview pane sets up a page of a remote workspace
    /// (`crate::browser`). Its DOM once the page and what it fetched have loaded.
    ///
    /// Headless Chrome prints the DOM and then, on macOS at least, never exits; the DOM is read up
    /// to its end and the browser ended.
    fn e2e_render(chrome: &Path, proxy: &str, url: &str) -> String {
        use std::io::Read;
        let profile = tempfile::tempdir().unwrap();
        let mut browser = std::process::Command::new(chrome)
            .args([
                "--headless=new",
                "--disable-gpu",
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-background-networking",
                "--disable-component-update",
                "--disable-sync",
                &format!("--user-data-dir={}", profile.path().display()),
                &format!("--proxy-server={proxy}"),
                "--proxy-bypass-list=<-loopback>",
                "--virtual-time-budget=15000",
                "--dump-dom",
                url,
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut stdout = browser.stdout.take().unwrap();
        let (sender, dumped) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut dom = Vec::new();
            let mut chunk = [0u8; 4096];
            while !dom.windows(7).any(|window| window == b"</html>") {
                match stdout.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => dom.extend_from_slice(&chunk[..count]),
                }
            }
            let _ = sender.send(dom);
        });
        let dom = dumped.recv_timeout(Duration::from_secs(90)).unwrap_or_default();
        let _ = browser.kill();
        let _ = browser.wait();
        String::from_utf8_lossy(&dom).into_owned()
    }

    fn e2e_wait_for(what: &str, timeout: Duration, mut condition: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + timeout;
        while !condition() {
            assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// A workspace on a real SSH machine, previewed end to end the way the pane does it, for each
    /// agent shell in `shells`: the machine's `.mework/launch.json` read there, its servers
    /// started there — the second on a port the machine hands out (`autoPort`) — their output
    /// streamed here, each page reached through the machine's tunnel by the name `localhost` has
    /// there, all of it through a dropped link, and a stop that frees the port there.
    fn preview_over_real_ssh(host: &str, key: &str, windows: bool, shells: &[crate::shell_backend::AgentShell]) {
        use crate::preview_servers::{PreviewServerRegistry, PreviewServerStatus, RemoteServerHost};
        use crate::run_environment::run_remote_script;
        let port = std::env::var("MEWORK_E2E_SSH_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
            .unwrap_or(0);
        let identity_file = std::env::var("MEWORK_E2E_SSH_KEY").unwrap_or_default();
        let app_data = tempfile::tempdir().unwrap();
        remote_link::install(app_data.path(), Vec::new(), None);
        let runner_with = |agent_shell| ShellRunner::Ssh {
            agent_shell,
            host: host.to_owned(),
            port,
            identity_file: identity_file.clone(),
            env: Default::default(),
        };

        // The workspace, written through the POSIX agent shell (Git Bash on Windows).
        let setup = RemoteMachine::new(runner_with(Default::default()), key.to_owned(), host.to_owned());
        let started = std::time::Instant::now();
        let server_port = setup.free_port().unwrap();
        eprintln!("[e2e] link up and a free port ({server_port}) in {:?}", started.elapsed());
        let (program, arguments) = if windows {
            ("powershell", r#"["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "server.ps1"]"#)
        } else {
            ("python3", r#"["-u", "server.py"]"#)
        };
        let launch_json = format!(
            r#"{{
  "version": "0.0.1",
  "configurations": [
    {{ "name": "web", "runtimeExecutable": "{program}", "runtimeArgs": {arguments}, "port": {server_port}, "env": {{ "E2E_NAME": "web" }} }},
    {{ "name": "api", "runtimeExecutable": "{program}", "runtimeArgs": {arguments}, "port": {server_port}, "autoPort": true, "env": {{ "E2E_NAME": "api" }} }}
  ]
}}
"#
        );
        let script = format!(
            "set -e\nR=~/mework-e2e-preview\nrm -rf \"$R\"\nmkdir -p \"$R/.mework\"\ncd \"$R\"\n\
             cat > server.py <<'MEWORK_E2E_EOF'\n{E2E_PYTHON_SERVER}MEWORK_E2E_EOF\n\
             cat > server.ps1 <<'MEWORK_E2E_EOF'\n{E2E_POWERSHELL_SERVER}MEWORK_E2E_EOF\n\
             cat > .mework/launch.json <<'MEWORK_E2E_EOF'\n{launch_json}MEWORK_E2E_EOF\n\
             hostname\n\
             if command -v cygpath >/dev/null 2>&1; then cygpath -m \"$R\"; else pwd -P; fi\n"
        );
        let output = run_remote_script(setup.runner(), &script, None, Duration::from_secs(60), &CancelSignal::default()).unwrap();
        assert_eq!(output.status, Some(0), "{}", output.stderr);
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let mut lines = text.lines().map(str::trim);
        let machine_name = lines.next().unwrap().to_ascii_lowercase();
        let root = lines.next().unwrap().to_owned();
        eprintln!("[e2e] workspace {root} on {machine_name}");

        for shell in shells {
            eprintln!("[e2e] agent shell {shell:?}");
            let machine = RemoteMachine::new(runner_with(shell.clone()), key.to_owned(), host.to_owned());
            let registry = PreviewServerRegistry::default();
            let session = Some("e2e-session");

            // What the pane lists, read there.
            let list = crate::preview::remote_configurations(&machine, &root).unwrap();
            assert_eq!(list.problem, None, "{list:?}");
            let names: Vec<&str> = list.servers.iter().map(|server| server.name.as_str()).collect();
            assert_eq!(names, ["web", "api"], "{list:?}");

            // Started there, on the configured port, and running once it answers there.
            let started = std::time::Instant::now();
            let start = |name: &str| match crate::preview::start_remote(&registry, &machine, &root, Some(name), session).unwrap() {
                crate::preview::PreviewStartOutcome::Server { server, reused } => (server, reused),
                other => panic!("expected a server: {other:?}"),
            };
            let (web, reused) = start("web");
            assert!(!reused);
            assert_eq!(web.port, server_port);
            assert_eq!(web.machine.as_deref(), Some(host));
            let running = |id: &str| registry.get(id).is_some_and(|server| server.status == PreviewServerStatus::Running);
            e2e_wait_for("web to be running", Duration::from_secs(60), || running(&web.server_id));
            eprintln!("[e2e] web running on {} in {:?}", web.port, started.elapsed());
            let logs = |id: &str| registry.logs(id).iter().map(|entry| entry.line.clone()).collect::<String>();
            e2e_wait_for("web's first line", Duration::from_secs(10), || logs(&web.server_id).contains(&format!("listening on {server_port}")));

            // A second entry on the same port moves to one the machine hands out.
            let (api, _) = start("api");
            assert_ne!(api.port, web.port, "autoPort moves api off web's port");
            e2e_wait_for("api to be running", Duration::from_secs(60), || running(&api.server_id));
            // And asking for web again hands back the one running.
            let (again, reused) = start("web");
            assert!(reused);
            assert_eq!(again.server_id, web.server_id);

            // The page's side: through the machine's tunnel, by the machine's own `localhost`.
            let proxy = crate::preview_tunnel::proxy_for(&machine).unwrap();
            let proxy_port: u16 = proxy.rsplit(':').next().unwrap().parse().unwrap();
            for (name, server) in [("web", &web), ("api", &api)] {
                for target in ["localhost", "127.0.0.1"] {
                    let (body, took) = e2e_fetch(proxy_port, target, server.port, "/hello");
                    eprintln!("[e2e] GET {target}:{}/hello took {took:?}", server.port);
                    let expected = format!("mework-e2e {name} ");
                    assert!(body.starts_with(&expected), "{body}");
                    assert!(body.to_ascii_lowercase().contains(&machine_name), "{body} is not from {machine_name}");
                    assert!(body.ends_with(" /hello"), "{body}");
                }
            }
            let (body, took) = e2e_fetch(proxy_port, "localhost", web.port, "/big");
            assert_eq!(body.len(), 1 << 20);
            assert!(body.bytes().all(|byte| byte == b'x'));
            eprintln!("[e2e] 1 MiB through the tunnel in {took:?}");
            let warm = std::time::Instant::now();
            for _ in 0..10 {
                e2e_fetch(proxy_port, "localhost", web.port, "/warm");
            }
            eprintln!("[e2e] ten page requests took {:?}", warm.elapsed());
            e2e_wait_for("web's request log", Duration::from_secs(10), || logs(&web.server_id).contains("served /warm"));

            // The page in Chromium, the engine the pane is, by the machine's own `localhost`.
            match e2e_chrome() {
                Some(chrome) => {
                    let started = std::time::Instant::now();
                    let dom = e2e_render(&chrome, &proxy, &format!("http://localhost:{}/page", web.port));
                    eprintln!("[e2e] Chromium rendered the page in {:?}", started.elapsed());
                    assert!(dom.contains("fetched: mework-e2e web "), "{dom}");
                    assert!(dom.contains(" /data"), "{dom}");
                }
                None => eprintln!("[e2e] no Chromium found (MEWORK_CHROME_PATH); the page leg is skipped"),
            }

            // A dropped link: killing this process's ssh is what a network drop looks like to
            // sshd. The servers keep running there, and the next page request waits out the
            // reconnect instead of failing.
            let killed = std::process::Command::new("pkill")
                .args(["-KILL", "-P", &std::process::id().to_string(), "-x", "ssh"])
                .status()
                .unwrap();
            assert!(killed.success(), "the link's ssh was running");
            let (body, took) = e2e_fetch(proxy_port, "localhost", web.port, "/after-drop");
            assert!(body.ends_with(" /after-drop"), "{body}");
            eprintln!("[e2e] first request after the drop took {took:?}");
            assert!(running(&web.server_id) && running(&api.server_id), "both servers outlived the drop");
            e2e_wait_for("the log after the drop", Duration::from_secs(20), || logs(&web.server_id).contains("served /after-drop"));

            // A stop ends the server there: its port is free again, and a page asking for it is
            // refused as a port nothing listens on.
            for server in [&web, &api] {
                assert!(registry.stop(&server.server_id));
                e2e_wait_for("the port to be free", Duration::from_secs(20), || {
                    machine.port_state(server.port).is_ok_and(|state| state == (true, false))
                });
                let (_, reply) = e2e_socks_connect(proxy_port, "localhost", server.port);
                assert_eq!(reply, 5, "a stopped server's port is refused");
            }
            assert!(registry.servers().is_empty());
        }
        remote_link::shutdown();
    }

    /// [`preview_over_real_ssh`] against a real Unix machine, which needs `python3` for its
    /// server: set `MEWORK_E2E_SSH_HOST` (and `MEWORK_E2E_SSH_PORT`, `MEWORK_E2E_SSH_KEY` as
    /// needed) and run with `--ignored`. The agent is installed from `src-tauri/remote-agents/`
    /// (`npm run build:remote-agents`), or built for the machine on the spot.
    #[test]
    #[ignore]
    fn over_real_ssh_a_unix_workspace_is_previewed_through_its_machine() {
        let host = std::env::var("MEWORK_E2E_SSH_HOST").expect("MEWORK_E2E_SSH_HOST");
        preview_over_real_ssh(&host, "ssh:e2e-unix", false, &[Default::default()]);
    }

    /// The same against a real Windows machine, whatever its login shell, in both agent shells it
    /// can have: `MEWORK_E2E_SSH_WINDOWS_HOST`. The machine needs Git for Windows for the setup
    /// and the Bash leg; the server is Windows PowerShell's.
    #[test]
    #[ignore]
    fn over_real_ssh_a_windows_workspace_is_previewed_through_its_machine() {
        use crate::shell_backend::{AgentShell, ShellBackend};
        let host = std::env::var("MEWORK_E2E_SSH_WINDOWS_HOST").expect("MEWORK_E2E_SSH_WINDOWS_HOST");
        preview_over_real_ssh(
            &host,
            "ssh:e2e-windows",
            true,
            &[AgentShell::new(ShellBackend::PowerShell, "powershell.exe"), AgentShell::default()],
        );
    }
}
