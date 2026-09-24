//! `mework-remote`: the agent Mework uploads to a remote machine.
//!
//! ```text
//! mework-remote proxy --sync <nonce>   what an SSH session runs
//! mework-remote daemon                 started by the first proxy, detached
//! mework-remote version [--json]       what the host checks after an upload
//! mework-remote net …                  the machine's loopback, for the host (see `agent::net`)
//! mework-remote sandbox-setup          Windows: provisions the sandbox account (one UAC prompt)
//! mework-remote serve --stdio          a daemon whose one connection is its own
//!                                      standard input and output: Mework's agent on
//!                                      its own machine, or with `--cell <config>` a
//!                                      sandboxed cell (see `agent::cells`)
//! ```
//!
//! `--idle-exit <seconds>` and `--tick-ms <milliseconds>` tune the daemon; a
//! proxy passes them on to a daemon it starts, along with `--root <dir>`, the
//! agent's directory as the proxy found it. `--own-log` makes the daemon open
//! its log itself, for a daemon started without standard handles.

use std::time::Duration;

use remote_agent::agent::{self, platform, DaemonOptions};

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let result = match arguments.first().map(String::as_str) {
        Some("proxy") => proxy(&arguments[1..]),
        Some("daemon") => daemon(&arguments[1..]),
        Some("version") => version(&arguments[1..]),
        Some("net") => agent::net::run(&arguments[1..]),
        Some("serve") => serve(&arguments[1..]),
        Some("sandbox-setup") => agent::sandbox::windows::setup().map(|said| println!("{said}")),
        _ => Err(
            "usage: mework-remote proxy --sync <nonce> | daemon | serve --stdio | version [--json] | net …".to_owned(),
        ),
    };
    if let Err(message) = result {
        eprintln!("mework-remote: {message}");
        std::process::exit(1);
    }
}

fn proxy(arguments: &[String]) -> Result<(), String> {
    let mut nonce = None;
    let mut daemon_args = Vec::new();
    let mut iter = arguments.iter();
    while let Some(argument) = iter.next() {
        match argument.as_str() {
            "--sync" => nonce = iter.next().cloned(),
            "--idle-exit" | "--tick-ms" => {
                daemon_args.push(argument.clone());
                daemon_args.push(iter.next().cloned().ok_or("missing value")?);
            }
            other => return Err(format!("unknown proxy argument {other}")),
        }
    }
    let nonce = nonce.ok_or("proxy needs --sync <nonce>")?;
    if nonce.is_empty() || !nonce.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err("the sync nonce must be alphanumeric".into());
    }
    agent::proxy::run_proxy(&nonce, &daemon_args)
}

fn daemon(arguments: &[String]) -> Result<(), String> {
    let mut options = DaemonOptions::default();
    let mut iter = arguments.iter();
    while let Some(argument) = iter.next() {
        match argument.as_str() {
            "--idle-exit" => {
                options.idle_exit = Duration::from_secs(number(iter.next())?);
            }
            "--tick-ms" => {
                options.tick = Duration::from_millis(number(iter.next())?.max(10));
            }
            platform::OWN_LOG_FLAG => options.own_log = true,
            "--root" => {
                let root = iter.next().ok_or("missing value")?;
                options.root = Some(std::path::PathBuf::from(root));
            }
            other => return Err(format!("unknown daemon argument {other}")),
        }
    }
    agent::run_daemon(options)
}

fn serve(arguments: &[String]) -> Result<(), String> {
    let mut stdio = false;
    let mut cell = None;
    let mut sync = None;
    let mut options = DaemonOptions::default();
    let mut iter = arguments.iter();
    while let Some(argument) = iter.next() {
        match argument.as_str() {
            "--stdio" => stdio = true,
            "--cell" => {
                let config = iter.next().ok_or("missing value")?;
                cell = Some(
                    serde_json::from_str::<agent::sandbox::CellConfig>(config)
                        .map_err(|error| format!("unreadable cell configuration: {error}"))?,
                );
            }
            "--sync" => {
                let nonce = iter.next().ok_or("missing value")?;
                if nonce.is_empty() || !nonce.chars().all(|c| c.is_ascii_alphanumeric()) {
                    return Err("the sync nonce must be alphanumeric".into());
                }
                sync = Some(nonce.clone());
            }
            "--tick-ms" => {
                options.tick = Duration::from_millis(number(iter.next())?.max(10));
            }
            other => return Err(format!("unknown serve argument {other}")),
        }
    }
    if !stdio {
        return Err("serve needs --stdio".into());
    }
    agent::run_stdio(options, cell, sync)
}

fn number(value: Option<&String>) -> Result<u64, String> {
    value
        .ok_or("missing value")?
        .parse()
        .map_err(|_| "not a number".to_owned())
}

fn version(arguments: &[String]) -> Result<(), String> {
    let digest = platform::self_digest()?;
    if arguments.iter().any(|argument| argument == "--json") {
        println!(
            "{}",
            serde_json::json!({
                "version": remote_agent::AGENT_VERSION,
                "build": digest,
                "source": remote_agent::marked_source_id(),
                "tag": platform::build_tag(&digest),
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
            })
        );
    } else {
        println!(
            "mework-remote {} ({}) {}/{}",
            remote_agent::AGENT_VERSION,
            &digest[..12],
            std::env::consts::OS,
            std::env::consts::ARCH
        );
    }
    Ok(())
}
