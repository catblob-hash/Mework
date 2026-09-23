//! The proxy: what an SSH session actually runs.
//!
//! It finds the daemon of its own build — starting one if there is none —
//! introduces itself over the machine-local socket, prints the sync line the
//! host is waiting for, and from then on only copies bytes: the SSH channel's
//! stdin to the socket, the socket to the SSH channel's stdout. It never reads
//! a frame. When either side ends, so does the proxy, and nothing else on the
//! machine notices: the daemon and everything it runs carry on.

use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use super::paths::Paths;
use super::platform::{self, FileLock, LocalStream};
use crate::protocol::{self, LocalHello};

/// How long a proxy waits for a daemon it started to open its socket.
const DAEMON_START_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a proxy waits for another proxy that is starting the daemon.
const STARTUP_LOCK_TIMEOUT: Duration = Duration::from_secs(20);

pub fn run_proxy(nonce: &str, daemon_args: &[String]) -> Result<(), String> {
    let digest = platform::self_digest()?;
    let paths = Paths::discover(&platform::build_tag(&digest))?;
    let (stream, token) = connect_or_start(&paths, daemon_args)?;
    let env: Vec<(String, String)> = std::env::vars().collect();
    {
        let mut writer = &stream;
        protocol::write_frame(
            &mut writer,
            &LocalHello {
                token,
                proxy_pid: std::process::id(),
                env,
            },
            &[],
        )
        .map_err(|error| format!("Cannot introduce the proxy to the daemon: {error}"))?;
    }
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(protocol::sync_line(nonce).as_bytes())
        .and_then(|_| stdout.flush())
        .map_err(|error| format!("Cannot answer the host: {error}"))?;
    drop(stdout);
    relay(stream)
}

/// Copies both directions until one of them ends, then exits the process:
/// a half-open proxy would keep the daemon believing the host is there.
fn relay(stream: LocalStream) -> Result<(), String> {
    let mut upstream = stream
        .try_clone()
        .map_err(|error| format!("Cannot share the daemon socket: {error}"))?;
    std::thread::Builder::new()
        .name("stdin".into())
        .spawn(move || {
            let mut stdin = std::io::stdin().lock();
            let mut buffer = vec![0u8; 64 * 1024];
            loop {
                match stdin.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        if upstream.write_all(&buffer[..count]).is_err() {
                            break;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            let _ = upstream.shutdown(std::net::Shutdown::Both);
            std::process::exit(0);
        })
        .map_err(|error| format!("Cannot start the relay: {error}"))?;
    let mut downstream = stream;
    let mut stdout = std::io::stdout().lock();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        match downstream.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                if stdout
                    .write_all(&buffer[..count])
                    .and_then(|_| stdout.flush())
                    .is_err()
                {
                    break;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    let _ = downstream.shutdown(std::net::Shutdown::Both);
    std::process::exit(0)
}

/// Connects to the daemon of this build, starting it first if nothing
/// answers. Proxies that arrive together serialize on a startup lock, so one
/// of them starts the daemon and the others find it.
fn connect_or_start(paths: &Paths, daemon_args: &[String]) -> Result<(LocalStream, String), String> {
    if let Ok(connected) = platform::connect_local(paths) {
        return Ok(connected);
    }
    let _startup = FileLock::acquire(&paths.startup_lock(), STARTUP_LOCK_TIMEOUT)
        .map_err(|error| format!("Cannot take the agent's startup lock: {error}"))?
        .ok_or("Another proxy has been starting the agent for too long")?;
    if let Ok(connected) = platform::connect_local(paths) {
        return Ok(connected);
    }
    let exe = std::env::current_exe()
        .map_err(|error| format!("Cannot locate the agent executable: {error}"))?;
    platform::spawn_detached_daemon(&exe, paths, daemon_args)
        .map_err(|error| format!("Cannot start the agent daemon: {error}"))?;
    let deadline = Instant::now() + DAEMON_START_TIMEOUT;
    let mut last_error = None;
    while Instant::now() < deadline {
        match platform::connect_local(paths) {
            Ok(connected) => return Ok(connected),
            Err(error) => last_error = Some(error),
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(format!(
        "The agent daemon did not come up ({}){}",
        last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| "no answer".into()),
        log_tail(&paths.log_file())
    ))
}

/// The end of the daemon log, for an error that says why it did not start.
fn log_tail(path: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let lines: Vec<&str> = text.lines().rev().take(5).collect();
    if lines.is_empty() {
        return String::new();
    }
    let tail: Vec<&str> = lines.into_iter().rev().collect();
    format!("; agent log: {}", tail.join(" | "))
}
