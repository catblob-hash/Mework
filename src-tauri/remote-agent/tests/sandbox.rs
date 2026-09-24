//! Sandboxed sessions, end to end, with the machine's real sandbox.
//!
//! Each test starts `mework-remote serve --stdio` — the daemon Mework runs on
//! its own machine — and starts sessions in cells through it, exactly as the
//! host does. What a session tries is ordinary shell: write, read, connect,
//! signal. What the test checks is that the operating system said no.
//!
//! Skipped (with a note) where the machine has no sandbox: a Linux machine
//! without bubblewrap, or one that forbids unprivileged user namespaces.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener};
use std::path::{Path, PathBuf};
use std::time::Duration;

use remote_agent::client::{ChildLauncher, Link, LinkConfig, RemoteProcess};
#[cfg(target_os = "linux")]
use remote_agent::protocol::TerminalSize;
use remote_agent::protocol::{NetworkMode, NetworkPolicy, SandboxPolicy, SandboxSpec, SpawnSpec, StdinMode};

const CALL: Duration = Duration::from_secs(30);

fn agent() -> PathBuf {
    std::env::var_os("MEWORK_REMOTE_E2E_AGENT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_mework-remote")))
}

/// A stdio daemon, and whether this machine can sandbox at all.
fn daemon(env: &[(&str, &str)]) -> Option<Link> {
    let mut launcher = ChildLauncher::new(agent(), vec!["serve".into(), "--stdio".into(), "--tick-ms".into(), "100".into()]);
    let mut vars: BTreeMap<String, String> = std::env::vars().collect();
    for (name, value) in env {
        vars.insert((*name).into(), (*value).into());
    }
    launcher.env = Some(vars);
    let link = Link::start(LinkConfig::new("sandbox-test", "e1"), launcher);
    let agent = link.wait_ready(CALL).expect("the stdio daemon starts");
    if !agent.sandbox.available {
        eprintln!("skipped: no sandbox on this machine ({})", agent.sandbox.detail);
        link.close(true);
        return None;
    }
    Some(link)
}

fn scratch() -> tempfile::TempDir {
    // Not under the system temporary directory's usual path on macOS, which
    // is a symlink: the rules compare real paths.
    tempfile::Builder::new().prefix("mwsb").tempdir().unwrap()
}

/// The real path, in the form a program is handed it: without Windows'
/// `\\?\` prefix, which PowerShell cannot take as its location.
fn real(path: &Path) -> PathBuf {
    let real = std::fs::canonicalize(path).unwrap();
    let text = real.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => real,
    }
}

fn sandboxed(sid: &str, cell: &str, script: &str, cwd: &Path, policy: &SandboxPolicy) -> SpawnSpec {
    let argv = if cfg!(windows) {
        vec![
            "powershell.exe".into(),
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-ExecutionPolicy".into(),
            "Bypass".into(),
            "-Command".into(),
            format!("[Console]::OutputEncoding = [Text.Encoding]::UTF8\n{script}"),
        ]
    } else {
        vec!["/bin/sh".into(), "-c".into(), script.into()]
    };
    SpawnSpec {
        sid: sid.into(),
        argv,
        cwd: Some(cwd.to_string_lossy().into_owned()),
        env: BTreeMap::new(),
        env_remove: Vec::new(),
        terminal: None,
        stdin: StdinMode::Null,
        output_limit: None,
        orphan_ttl_secs: None,
        label: None,
        sandbox: Some(SandboxSpec {
            cell: cell.into(),
            policy: policy.clone(),
        }),
    }
}

/// Everything a session printed, decoded leniently: Windows PowerShell
/// writes in the console's code page.
fn output(mut process: RemoteProcess) -> (String, String, Option<i32>) {
    let mut out = process.take_stdout().unwrap();
    let mut err = process.take_stderr().unwrap();
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        err.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let mut stdout = Vec::new();
    out.read_to_end(&mut stdout).unwrap();
    let stderr = reader.join().unwrap();
    let exit = process.wait().unwrap();
    (
        String::from_utf8_lossy(&stdout).into_owned(),
        String::from_utf8_lossy(&stderr).into_owned(),
        exit.code,
    )
}

fn policy(workspace: &Path) -> SandboxPolicy {
    SandboxPolicy {
        writable: vec![workspace.to_string_lossy().into_owned()],
        ..SandboxPolicy::default()
    }
}

#[cfg(unix)]
#[test]
fn a_session_writes_its_workspace_and_nothing_else() {
    let Some(link) = daemon(&[]) else { return };
    let base = scratch();
    let base = real(base.path());
    let workspace = base.join("ws");
    std::fs::create_dir_all(workspace.join(".git/hooks")).unwrap();
    std::fs::write(workspace.join(".git/config"), "[core]\n").unwrap();
    std::fs::write(workspace.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    let outside = base.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let script = format!(
        "echo made > made.txt && echo wrote-workspace
         echo x >> .git/config 2>/dev/null || echo git-config-protected
         echo x > .git/hooks/pre-commit 2>/dev/null || echo git-hooks-protected
         mv .git .git-away 2>/dev/null || echo git-dir-pinned
         mkdir -p .mework 2>/dev/null && echo x > .mework/hooks.json 2>/dev/null || echo mework-protected
         echo x > '{outside}/file' 2>/dev/null || echo outside-protected
         echo x > \"$HOME/.mework-sandbox-test\" 2>/dev/null || echo home-protected
         echo x > \"$TMPDIR/scratch\" && echo tmp-writable
         [ -n \"$MEWORK_SANDBOX\" ] && echo knows-it-is-sandboxed
         for t in /dev/ttys*; do [ -e \"$t\" ] && head -c0 \"$t\" 2>/dev/null && echo \"TTY-OPEN $t\"; done; echo ttys-checked",
        outside = outside.display()
    );
    let process = link
        .spawn(sandboxed("w1", "conv-w", &script, &workspace, &policy(&workspace)), b"", CALL)
        .unwrap();
    let (stdout, stderr, code) = output(process);
    for expected in [
        "wrote-workspace",
        "git-config-protected",
        "git-hooks-protected",
        "git-dir-pinned",
        "mework-protected",
        "outside-protected",
        "home-protected",
        "tmp-writable",
        "knows-it-is-sandboxed",
        "ttys-checked",
    ] {
        assert!(stdout.contains(expected), "{expected} missing\nstdout: {stdout}\nstderr: {stderr}");
    }
    assert!(!stdout.contains("TTY-OPEN"), "a terminal outside the sandbox was readable: {stdout}");
    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert_eq!(std::fs::read_to_string(workspace.join("made.txt")).unwrap(), "made\n");
    assert_eq!(std::fs::read_to_string(workspace.join(".git/config")).unwrap(), "[core]\n");
    assert!(!outside.join("file").exists());
    assert!(!workspace.join(".git/hooks/pre-commit").exists());
    link.close(true);
    // A placeholder `.mework` the sandbox needed is gone again once the cell
    // is.
    std::thread::sleep(Duration::from_millis(300));
    assert!(!workspace.join(".mework").exists() || std::fs::read_dir(workspace.join(".mework")).unwrap().next().is_none());
}

#[cfg(unix)]
#[test]
fn credentials_and_secret_variables_do_not_reach_the_sandbox() {
    let Some(link) = daemon(&[("MEWORK_TEST_API_TOKEN", "hunter2"), ("MEWORK_PLAIN_SETTING", "visible")]) else {
        return;
    };
    let base = scratch();
    let base = real(base.path());
    let workspace = base.join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let vault = base.join("vault");
    std::fs::create_dir_all(&vault).unwrap();
    std::fs::write(vault.join("key"), "top secret").unwrap();
    let mut policy = policy(&workspace);
    policy.deny_read.push(vault.to_string_lossy().into_owned());
    // A link planted in the workspace does not get around the rule.
    std::os::unix::fs::symlink(vault.join("key"), workspace.join("innocent")).unwrap();
    let script = format!(
        "cat '{vault}/key' 2>/dev/null || echo vault-unreadable
         cat innocent 2>/dev/null || echo link-unreadable
         ln '{vault}/key' hard 2>/dev/null && cat hard 2>/dev/null || echo hardlink-refused
         ls \"$HOME/.ssh\" >/dev/null 2>&1 && echo SSH-LISTED || echo ssh-unreadable
         echo \"token=[$MEWORK_TEST_API_TOKEN]\"
         env | grep -c MEWORK_PLAIN_SETTING >/dev/null && echo plain-kept",
        vault = vault.display()
    );
    let process = link
        .spawn(sandboxed("c1", "conv-c", &script, &workspace, &policy), b"", CALL)
        .unwrap();
    let (stdout, stderr, _) = output(process);
    assert!(!stdout.contains("top secret"), "{stdout}");
    for expected in ["vault-unreadable", "link-unreadable", "hardlink-refused", "token=[]"] {
        assert!(stdout.contains(expected), "{expected} missing\nstdout: {stdout}\nstderr: {stderr}");
    }
    if Path::new(&std::env::var("HOME").unwrap()).join(".ssh").is_dir() {
        assert!(stdout.contains("ssh-unreadable"), "{stdout}");
    }
    link.close(true);
}

/// A tiny HTTP server that answers every request with `pong`.
fn pong_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 2 {
                    line.clear();
                }
                let mut stream = stream;
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\npong");
                let _ = stream.shutdown(Shutdown::Write);
            });
        }
    });
    port
}

#[cfg(unix)]
#[test]
fn the_network_is_the_proxy_and_the_proxy_is_the_policy() {
    let Some(link) = daemon(&[]) else { return };
    if std::process::Command::new("curl").arg("--version").output().is_err() {
        eprintln!("skipped: no curl");
        return;
    }
    let base = scratch();
    let base = real(base.path());
    let workspace = base.join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let allowed = pong_server();
    let other = pong_server();
    let mut policy = policy(&workspace);
    policy.network = NetworkPolicy {
        mode: NetworkMode::Allowlist,
        // Loopback is reachable only because the list names this address.
        allow: vec![format!("127.0.0.1:{allowed}")],
        deny: Vec::new(),
    };
    // `--noproxy ''` sends even loopback through the proxy, which a Linux
    // cell's own `NO_PROXY` would keep inside its namespace.
    let script = format!(
        "curl -sS -m 10 --noproxy '' http://127.0.0.1:{allowed}/ && echo ' via-proxy'
         curl -sS -m 10 --noproxy '' -o /dev/null -w '%{{http_code}}' http://127.0.0.1:{other}/; echo ' other-port'
         curl -sS -m 5 --noproxy '*' http://127.0.0.1:{allowed}/ >/dev/null 2>&1 || echo direct-blocked
         curl -sS -m 10 -o /dev/null -w '%{{http_code}}' https://example.com/; echo ' example'"
    );
    let process = link
        .spawn(sandboxed("n1", "conv-n", &script, &workspace, &policy), b"", CALL)
        .unwrap();
    let (stdout, stderr, _) = output(process);
    assert!(stdout.contains("pong via-proxy"), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("403 other-port"), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("direct-blocked"), "stdout: {stdout}\nstderr: {stderr}");
    // HTTPS goes through CONNECT, which the proxy refuses before any TLS.
    assert!(!stdout.contains("200 example"), "stdout: {stdout}\nstderr: {stderr}");
    link.close(true);
}

#[cfg(unix)]
#[test]
fn a_sandbox_cannot_touch_processes_outside_it() {
    let Some(link) = daemon(&[]) else { return };
    let base = scratch();
    let base = real(base.path());
    let workspace = base.join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let mut outsider = std::process::Command::new("sleep").arg("30").spawn().unwrap();
    // Another conversation's process, in a cell of its own. Its pid as the
    // machine sees it: inside a Linux cell pids are the cell's own, and a
    // small one is as likely as not one of this cell's processes too.
    let marker = format!("{}.{}", 30 + std::process::id() % 7, std::process::id() % 1000);
    let mut neighbour = link
        .spawn(
            sandboxed("p0", "conv-other", &format!("echo ready; exec sleep {marker}"), &workspace, &policy(&workspace)),
            b"",
            CALL,
        )
        .unwrap();
    let mut neighbour_out = BufReader::new(neighbour.take_stdout().unwrap());
    let mut ready = String::new();
    neighbour_out.read_line(&mut ready).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let found = std::process::Command::new("pgrep")
        .args(["-f", &format!("sleep {marker}")])
        .output()
        .unwrap();
    let neighbour_pid = String::from_utf8_lossy(&found.stdout)
        .lines()
        .next()
        .expect("the neighbour's sleep is running")
        .trim()
        .to_owned();
    let script = format!(
        "kill -0 {outsider} 2>/dev/null && echo OUTSIDER-VISIBLE || echo outsider-unreachable
         kill -0 {daemon} 2>/dev/null && echo DAEMON-VISIBLE || echo daemon-unreachable
         kill -0 {neighbour} 2>/dev/null && echo NEIGHBOUR-VISIBLE || echo neighbour-unreachable
         sleep 5 & kill -0 $! && echo own-child-visible",
        outsider = outsider.id(),
        daemon = link.wait_ready(CALL).unwrap().pid,
        neighbour = neighbour_pid.trim(),
    );
    let process = link
        .spawn(sandboxed("p1", "conv-p", &script, &workspace, &policy(&workspace)), b"", CALL)
        .unwrap();
    let (stdout, stderr, _) = output(process);
    for expected in ["outsider-unreachable", "daemon-unreachable", "neighbour-unreachable", "own-child-visible"] {
        assert!(stdout.contains(expected), "{expected} missing\nstdout: {stdout}\nstderr: {stderr}");
    }
    let _ = outsider.kill();
    let _ = outsider.wait();
    neighbour.kill();
    link.close(true);
}

/// Linux gives each sandbox a pseudo-terminal namespace of its own; macOS has
/// one for the whole account, so a cell there gets no terminals at all (see
/// `agent::sandbox::seatbelt`).
#[cfg(target_os = "linux")]
#[test]
fn a_terminal_runs_in_the_sandbox_too() {
    let Some(link) = daemon(&[]) else { return };
    let base = scratch();
    let base = real(base.path());
    let workspace = base.join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let mut spec = sandboxed(
        "t1",
        "conv-t",
        "read line; echo \"got:$line\"; echo x > /etc/mework-test 2>/dev/null || echo still-sandboxed",
        &workspace,
        &policy(&workspace),
    );
    spec.terminal = Some(TerminalSize { cols: 80, rows: 24 });
    let mut process = link.spawn(spec, b"", CALL).unwrap();
    let mut stdin = process.stdin();
    stdin.write_all(b"hello\r").unwrap();
    let mut out = String::new();
    process.take_stdout().unwrap().read_to_string(&mut out).unwrap();
    assert!(out.contains("got:hello"), "{out:?}");
    assert!(out.contains("still-sandboxed"), "{out:?}");
    link.close(true);
}

#[cfg(unix)]
#[test]
fn sessions_of_one_conversation_share_their_cell() {
    let Some(link) = daemon(&[]) else { return };
    let base = scratch();
    let base = real(base.path());
    let workspace = base.join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let mut first = link
        .spawn(sandboxed("s1", "conv-s", "echo $$; exec sleep 30", &workspace, &policy(&workspace)), b"", CALL)
        .unwrap();
    let mut out = BufReader::new(first.take_stdout().unwrap());
    let mut pid = String::new();
    out.read_line(&mut pid).unwrap();
    let second = link
        .spawn(
            sandboxed(
                "s2",
                "conv-s",
                &format!("kill -0 {} && echo sibling-visible", pid.trim()),
                &workspace,
                &policy(&workspace),
            ),
            b"",
            CALL,
        )
        .unwrap();
    let (stdout, stderr, _) = output(second);
    assert!(stdout.contains("sibling-visible"), "stdout: {stdout}\nstderr: {stderr}");
    first.kill();
    link.close(true);
}

/// Windows: the cell runs as srt-win's sandbox account. The same promises,
/// in PowerShell — except one: every cell on a machine is that one account,
/// so cells are not kept from each other.
#[cfg(windows)]
mod windows {
    use super::*;

    #[test]
    fn a_session_writes_its_workspace_and_nothing_else() {
        let Some(link) = daemon(&[]) else { return };
        let base = scratch();
        let base = real(base.path());
        let workspace = base.join("ws");
        std::fs::create_dir_all(workspace.join(".git").join("hooks")).unwrap();
        std::fs::write(workspace.join(".git").join("config"), "[core]\n").unwrap();
        let outside = base.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "top secret").unwrap();
        let script = format!(
            "$ErrorActionPreference = 'Stop'
             $null = New-PSDrive -Name MeworkWorkspace -PSProvider FileSystem -Root '{workspace}' -Scope Global
             Set-Location -LiteralPath 'MeworkWorkspace:\\'
             'location=' + (Get-Location).ProviderPath
             try {{ Set-Content -Path made.txt -Value made; 'wrote-workspace' }} catch {{ 'WORKSPACE-REFUSED ' + $_ }}
             try {{ Add-Content -Path .git\\config -Value x; 'CONFIG-WRITTEN' }} catch {{ 'git-config-protected' }}
             try {{ Set-Content -Path .git\\hooks\\pre-commit -Value x; 'HOOK-WRITTEN' }} catch {{ 'git-hooks-protected' }}
             try {{ Rename-Item -Path .git -NewName .git-away; 'GIT-MOVED' }} catch {{ 'git-dir-pinned' }}
             try {{ New-Item -ItemType Directory -Force -Path .mework | Out-Null; Set-Content -Path .mework\\hooks.json -Value x; 'MEWORK-WRITTEN' }} catch {{ 'mework-protected' }}
             try {{ Set-Content -Path '{outside}\\file' -Value x; 'OUTSIDE-WRITTEN' }} catch {{ 'outside-protected' }}
             try {{ Get-Content -Path '{outside}\\secret.txt' | Out-Null; 'SECRET-READ' }} catch {{ 'outside-unreadable' }}
             try {{ Get-ChildItem -Path '{home}\\.ssh' -ErrorAction Stop | Out-Null; 'SSH-LISTED' }} catch {{ 'home-unreadable' }}
             if ($env:MEWORK_SANDBOX) {{ 'knows-it-is-sandboxed' }}
             [Environment]::UserName",
            outside = outside.display(),
            home = std::env::var("USERPROFILE").unwrap(),
            workspace = workspace.display(),
        );
        let process = link
            .spawn(sandboxed("w1", "conv-w", &script, &workspace, &policy(&workspace)), b"", CALL)
            .unwrap();
        let (stdout, stderr, _) = output(process);
        for expected in [
            "wrote-workspace",
            "git-config-protected",
            "git-hooks-protected",
            "git-dir-pinned",
            "mework-protected",
            "outside-protected",
            "outside-unreadable",
            "home-unreadable",
            "knows-it-is-sandboxed",
        ] {
            assert!(stdout.contains(expected), "{expected} missing\nstdout: {stdout}\nstderr: {stderr}");
        }
        let user = std::env::var("USERNAME").unwrap();
        assert!(!stdout.lines().any(|line| line.trim().eq_ignore_ascii_case(&user)), "ran as the user: {stdout}");
        assert!(workspace.join("made.txt").is_file());
        assert_eq!(std::fs::read_to_string(workspace.join(".git").join("config")).unwrap(), "[core]\n");
        link.close(true);
    }

    #[test]
    fn the_network_is_the_proxy_and_the_proxy_is_the_policy() {
        let Some(link) = daemon(&[]) else { return };
        let base = scratch();
        let base = real(base.path());
        let workspace = base.join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let allowed = pong_server();
        let other = pong_server();
        let mut policy = policy(&workspace);
        policy.network = NetworkPolicy {
            mode: NetworkMode::Allowlist,
            allow: vec![format!("127.0.0.1:{allowed}")],
            deny: Vec::new(),
        };
        // curl.exe ships with Windows and reads the proxy variables;
        // `--noproxy ''` sends even loopback through the proxy.
        let script = format!(
            "$allowed = curl.exe -sS -m 10 --noproxy '\"\"' http://127.0.0.1:{allowed}/ 2>&1; \"$allowed via-proxy\"
             $code = curl.exe -sS -m 10 --noproxy '\"\"' -o NUL -w '%{{http_code}}' http://127.0.0.1:{other}/ 2>&1; \"$code other-port\"
             try {{ $direct = New-Object System.Net.Sockets.TcpClient; $direct.Connect('127.0.0.1', {allowed}); 'DIRECT-CONNECTED' }} catch {{ 'direct-blocked' }}
             try {{ $direct = New-Object System.Net.Sockets.TcpClient; $direct.Connect('1.1.1.1', 443); 'INTERNET-CONNECTED' }} catch {{ 'internet-blocked' }}"
        );
        let process = link
            .spawn(sandboxed("n1", "conv-n", &script, &workspace, &policy), b"", CALL)
            .unwrap();
        let (stdout, stderr, _) = output(process);
        for expected in ["pong via-proxy", "403 other-port", "direct-blocked", "internet-blocked"] {
            assert!(stdout.contains(expected), "{expected} missing\nstdout: {stdout}\nstderr: {stderr}");
        }
        link.close(true);
    }

    #[test]
    fn a_sandbox_cannot_touch_the_users_processes() {
        let Some(link) = daemon(&[]) else { return };
        let base = scratch();
        let base = real(base.path());
        let workspace = base.join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let mut outsider = std::process::Command::new("ping")
            .args(["-n", "60", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let script = format!(
            "try {{ Stop-Process -Id {pid} -Force -ErrorAction Stop; 'OUTSIDER-KILLED' }} catch {{ 'outsider-untouchable' }}",
            pid = outsider.id()
        );
        let process = link
            .spawn(sandboxed("p1", "conv-p", &script, &workspace, &policy(&workspace)), b"", CALL)
            .unwrap();
        let (stdout, stderr, _) = output(process);
        assert!(stdout.contains("outsider-untouchable"), "stdout: {stdout}\nstderr: {stderr}");
        assert!(outsider.try_wait().unwrap().is_none(), "the user's process was ended");
        let _ = outsider.kill();
        link.close(true);
    }

    /// A WSL 2 distribution, reached the way Mework reaches it from Windows:
    /// the Linux agent started through `wsl.exe --exec`, its protocol over
    /// `wsl.exe`'s standard input and output, its cells in bubblewrap inside
    /// the distribution. Opt-in: `MEWORK_E2E_WSL_DISTRO` names the
    /// distribution and `MEWORK_E2E_WSL_AGENT` the Linux agent's path in it
    /// (`/mnt/c/…`).
    #[test]
    fn a_wsl_distribution_sandboxes_through_wsl_exe() {
        let (Ok(distro), Ok(agent)) = (std::env::var("MEWORK_E2E_WSL_DISTRO"), std::env::var("MEWORK_E2E_WSL_AGENT")) else {
            eprintln!("skipped: MEWORK_E2E_WSL_DISTRO and MEWORK_E2E_WSL_AGENT are not set");
            return;
        };
        let launcher = ChildLauncher::new(
            "wsl.exe",
            vec!["-d".into(), distro, "--exec".into(), agent, "serve".into(), "--stdio".into()],
        );
        let link = Link::start(LinkConfig::new("sandbox-test", "e1"), launcher);
        let info = link.wait_ready(CALL).expect("the agent starts through wsl.exe");
        assert!(info.sandbox.available, "no sandbox in the distribution: {}", info.sandbox.detail);
        let shell = |sid: &str, script: &str, cwd: Option<String>, sandbox: Option<SandboxSpec>| SpawnSpec {
            sid: sid.into(),
            argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
            cwd,
            env: BTreeMap::new(),
            env_remove: Vec::new(),
            terminal: None,
            stdin: StdinMode::Null,
            output_limit: None,
            orphan_ttl_secs: None,
            label: None,
            sandbox,
        };
        // The workspace is the distribution's own, made outside the sandbox.
        let made = link
            .spawn(shell("mk", "d=$(mktemp -d) && mkdir \"$d/ws\" \"$d/outside\" && echo \"$d\"", None, None), b"", CALL)
            .unwrap();
        let (stdout, stderr, code) = output(made);
        assert_eq!(code, Some(0), "stderr: {stderr}");
        let base = stdout.trim().to_owned();
        let workspace = format!("{base}/ws");
        let policy = SandboxPolicy {
            writable: vec![workspace.clone()],
            ..SandboxPolicy::default()
        };
        let script = format!(
            "echo inside > made.txt && cat made.txt
             echo x > '{base}/outside/file' 2>/dev/null || echo outside-refused
             mkdir -p .git 2>/dev/null; echo x > .git/config 2>/dev/null || echo git-protected
             [ -n \"$MEWORK_SANDBOX\" ] && echo knows-it-is-sandboxed"
        );
        let sandbox = SandboxSpec {
            cell: "conv-wsl".into(),
            policy,
        };
        let process = link
            .spawn(shell("w1", &script, Some(workspace.clone()), Some(sandbox)), b"", CALL)
            .unwrap();
        let (stdout, stderr, _) = output(process);
        for expected in ["inside", "outside-refused", "git-protected", "knows-it-is-sandboxed"] {
            assert!(stdout.contains(expected), "{expected} missing\nstdout: {stdout}\nstderr: {stderr}");
        }
        let check = link
            .spawn(
                shell("ck", &format!("cat '{workspace}/made.txt'; ls '{base}/outside' | wc -l; rm -rf '{base}'"), None, None),
                b"",
                CALL,
            )
            .unwrap();
        let (stdout, stderr, _) = output(check);
        assert_eq!(stdout.split_whitespace().collect::<Vec<_>>(), ["inside", "0"], "stderr: {stderr}");
        link.close(true);
    }
}
