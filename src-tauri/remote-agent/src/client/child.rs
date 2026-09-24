//! A link to an agent that is a child process speaking the protocol on its own
//! standard input and output: `mework-remote serve --stdio`, started by the
//! host for its own machine (directly, or inside WSL through `wsl.exe`), or by
//! a daemon for a sandboxed cell with the sandbox's program in front.
//!
//! The child prints the sync line for the link's nonce once it is ready. Until
//! then anything it prints is its own business; if it exits first, what it
//! said on standard error is the reason the link reports, and the link does
//! not try again — a program that cannot start now will not start in a
//! moment either.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::{lock, LaunchError, Launcher, Transport};
use crate::protocol;

/// Called with each line the child writes to standard error.
pub type StderrSink = Arc<dyn Fn(&str) + Send + Sync>;

pub struct ChildLauncher {
    pub program: PathBuf,
    /// Arguments; the launcher appends `--sync <nonce>`.
    pub args: Vec<String>,
    /// The child's whole environment, or `None` to inherit this process's.
    pub env: Option<BTreeMap<String, String>>,
    /// Removed from an inherited environment.
    pub env_remove: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub stderr: Option<StderrSink>,
}

impl ChildLauncher {
    pub fn new(program: impl Into<PathBuf>, args: Vec<String>) -> Self {
        Self {
            program: program.into(),
            args,
            env: None,
            env_remove: Vec::new(),
            cwd: None,
            stderr: None,
        }
    }
}

impl Launcher for ChildLauncher {
    fn launch(&self, nonce: &str) -> Result<Transport, LaunchError> {
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .args(["--sync", nonce])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(env) = &self.env {
            command.env_clear().envs(env);
        }
        for name in &self.env_remove {
            command.env_remove(name);
        }
        if let Some(cwd) = &self.cwd {
            command.current_dir(cwd);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child: Child = command.spawn().map_err(|error| {
            LaunchError::Unavailable(format!("Cannot start {}: {error}", self.program.display()))
        })?;
        let stdin = child.stdin.take().expect("piped");
        let mut stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");
        let said = Arc::new(Mutex::new(String::new()));
        {
            let said = Arc::clone(&said);
            let sink = self.stderr.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if let Some(sink) = &sink {
                        sink(&line);
                    }
                    let mut text = lock(&said);
                    if text.len() < 4096 {
                        text.push_str(line.trim());
                        text.push('\n');
                    }
                }
            });
        }
        match protocol::read_preamble(&mut stdout, nonce) {
            Ok(protocol::Preamble::Ready) => {}
            other => {
                let _ = child.kill();
                let _ = child.wait();
                // The reader thread needs a moment to collect the last lines.
                std::thread::sleep(Duration::from_millis(100));
                let said = lock(&said).trim().to_owned();
                let reason = if !said.is_empty() {
                    said
                } else {
                    match other {
                        Err(error) => error.to_string(),
                        Ok(_) => "it did not answer".into(),
                    }
                };
                return Err(LaunchError::Unavailable(format!(
                    "{} did not start: {reason}",
                    self.program.display()
                )));
            }
        }
        let child = Arc::new(Mutex::new(child));
        Ok(Transport {
            reader: Box::new(ChildOutput(stdout)),
            writer: Box::new(stdin),
            closer: Box::new(move || {
                let mut child = lock(&child);
                let _ = child.kill();
                let _ = child.wait();
            }),
        })
    }
}

struct ChildOutput(std::process::ChildStdout);

impl Read for ChildOutput {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buffer)
    }
}
