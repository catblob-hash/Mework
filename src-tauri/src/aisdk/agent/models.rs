//! The bundled Claude Code's own model picker, asked of the CLI directly.
//!
//! The model fetch for the Claude Agent family does not go through the sidecar.
//! The host starts the bundled executable the way the login probe does, speaks
//! the CLI's stream-json control protocol (the one the Agent SDK drives) just
//! long enough to read the picker, and kills it. No prompt is ever sent, so
//! nothing is billed.
//!
//! The picker is resolved against the user's current login and the bundled
//! CLI's release, so the list drifts with both. That is the point: a model the
//! login gains shows up on the next fetch.

use std::collections::BTreeMap;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};

use super::{bundled_executable, probe_env, session_cwd, with_stderr_tail};

/// How long the whole listing may take. The CLI answers from local state in
/// under a second; the bound only covers one that hangs while starting.
const LIST_TIMEOUT: Duration = Duration::from_secs(60);

/// A CLI that only answers control requests: the SDK's stream-json transport,
/// and none of the CLI's own tools, settings files, MCP servers or transcript —
/// the switches a run is started with.
const LIST_ARGS: &[&str] = &[
    "--output-format",
    "stream-json",
    "--verbose",
    "--input-format",
    "stream-json",
    "--tools",
    "",
    "--setting-sources=",
    "--strict-mcp-config",
    "--no-session-persistence",
];

/// Pinned on top of the probe environment so the CLI resolves the login and
/// the provider a run resolves: the SDK's entrypoint label, which every run
/// carries, and first-party Anthropic rather than a cloud provider.
const LIST_CONTROL_ENV: &[(&str, &str)] = &[
    ("CLAUDE_CODE_ENTRYPOINT", "sdk-ts"),
    ("CLAUDE_CODE_USE_BEDROCK", "0"),
    ("CLAUDE_CODE_USE_VERTEX", "0"),
    ("CLAUDE_CODE_USE_FOUNDRY", "0"),
];

/// One row of the CLI's model picker, in its order. `value` may be an alias
/// (`sonnet`, `opus[1m]`, `default`); `resolved_model` is the model id it
/// stands for today. `context_window` is the CLI's window for an alias row, read
/// by switching to it; absent for a row named by an explicit id (switching to
/// one costs a request) and for a row the CLI would not switch to.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentModel {
    pub(crate) value: String,
    #[serde(default)]
    pub(crate) resolved_model: Option<String>,
    #[serde(default)]
    pub(crate) display_name: String,
    #[serde(skip)]
    pub(crate) context_window: Option<u64>,
}

/// The environment the listing CLI runs with: the login probe's, which carries
/// no credential, plus [`LIST_CONTROL_ENV`].
fn listing_env() -> BTreeMap<String, String> {
    let mut env = probe_env();
    for (name, value) in LIST_CONTROL_ENV {
        env.insert((*name).to_owned(), (*value).to_owned());
    }
    env
}

/// The bundled Claude Code's model picker under the user's current login.
pub(crate) fn list_models() -> Result<Vec<AgentModel>, String> {
    #[cfg_attr(not(feature = "browser-dev"), allow(unused_mut))]
    let mut env = listing_env();
    #[cfg(feature = "browser-dev")]
    env.extend(super::test_upstream_env()?);
    // A fetch has no app data path; the fallback directory is private to Mework,
    // and `--no-session-persistence` keeps the CLI from writing anything there.
    list_models_with(&bundled_executable()?, &session_cwd("")?, &env)
}

fn list_models_with(
    executable: &Path,
    cwd: &Path,
    env: &BTreeMap<String, String>,
) -> Result<Vec<AgentModel>, String> {
    let mut cli = ControlSession::start(executable, cwd, env)?;
    let listed = read_picker(&mut cli);
    let stderr = cli.stop();
    listed.map_err(|error| with_stderr_tail(&error, &stderr))
}

fn read_picker(cli: &mut ControlSession) -> Result<Vec<AgentModel>, String> {
    let initialized = cli
        .request(json!({ "subtype": "initialize" }))?
        .map_err(|error| format!("Claude Code 拒绝了初始化请求：{error}"))?;
    let rows = initialized
        .get("models")
        .cloned()
        .ok_or("Claude Code 的初始化回答里没有模型列表")?;
    let mut models: Vec<AgentModel> = serde_json::from_value(rows)
        .map_err(|error| format!("无法解析 Claude Code 的模型列表：{error}"))?;
    for model in &mut models {
        model.context_window = context_window_of(cli, model)?;
    }
    Ok(models)
}

/// The context window the CLI uses for one picker row: switch the idle CLI to
/// it, then read the window from a `summary` context report, which answers from
/// local state without a token-count call.
///
/// Only an alias row is switched to. Switching to an explicit model id makes the
/// CLI validate it with a real one-token `/v1/messages` request, and listing
/// must never bill; the model fetch derives those rows' windows from the id
/// instead. `None` also when the CLI refuses the switch or reports a model other
/// than the row's; the row itself is still offered. A CLI that stopped
/// answering fails the whole listing.
fn context_window_of(cli: &mut ControlSession, model: &AgentModel) -> Result<Option<u64>, String> {
    if model.value.to_ascii_lowercase().starts_with("claude-") {
        return Ok(None);
    }
    if cli
        .request(json!({ "subtype": "set_model", "model": model.value }))?
        .is_err()
    {
        return Ok(None);
    }
    let Ok(usage) = cli.request(json!({ "subtype": "get_context_usage", "detail": "summary" }))?
    else {
        return Ok(None);
    };
    if let Some(resolved) = &model.resolved_model {
        if usage.get("model").and_then(Value::as_str) != Some(resolved.as_str()) {
            return Ok(None);
        }
    }
    Ok(usage
        .get("rawMaxTokens")
        .and_then(Value::as_u64)
        .filter(|window| *window > 0))
}

/// A CLI started for control requests only. Dropping it kills the process.
struct ControlSession {
    child: Child,
    stdin: ChildStdin,
    /// Stdout lines, from a reader thread; disconnected once the CLI exits.
    lines: Receiver<String>,
    stderr: Option<JoinHandle<Vec<u8>>>,
    deadline: Instant,
    next_id: u64,
}

impl ControlSession {
    fn start(
        executable: &Path,
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<Self, String> {
        let mut command = Command::new(executable);
        command
            .args(LIST_ARGS)
            .current_dir(cwd)
            .env_clear()
            .envs(env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            // Without this every fetch flashes a console window.
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("无法启动 Claude Code 读取模型列表：{error}"))?;
        let (Some(stdin), Some(stdout), Some(mut stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Claude Code 的标准输入输出没有接上".into());
        };
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    return;
                }
            }
        });
        // Drained even though it is read only on failure: a full pipe would
        // block the CLI.
        let stderr = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stderr.read_to_end(&mut bytes);
            bytes
        });
        Ok(Self {
            child,
            stdin,
            lines,
            stderr: Some(stderr),
            deadline: Instant::now() + LIST_TIMEOUT,
            next_id: 0,
        })
    }

    /// Send one control request and wait for its response. The outer error is
    /// the transport (the CLI exited or timed out); the inner one is the CLI
    /// refusing this request.
    fn request(&mut self, request: Value) -> Result<Result<Value, String>, String> {
        self.next_id += 1;
        let id = format!("mework-{}", self.next_id);
        let frame = json!({ "type": "control_request", "request_id": id, "request": request });
        writeln!(self.stdin, "{frame}")
            .and_then(|()| self.stdin.flush())
            .map_err(|error| format!("写入 Claude Code 失败：{error}"))?;
        loop {
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            let line = match self.lines.recv_timeout(remaining) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) => {
                    return Err("读取 Claude Code 模型列表超时".into());
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("Claude Code 在回答模型列表之前退出了".into());
                }
            };
            // Anything but this request's answer — a banner, a system message —
            // is not what is being waited on.
            let Ok(frame) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let Some(response) = frame
                .get("response")
                .filter(|_| frame.get("type").and_then(Value::as_str) == Some("control_response"))
            else {
                continue;
            };
            if response.get("request_id").and_then(Value::as_str) != Some(id.as_str()) {
                continue;
            }
            return Ok(match response.get("subtype").and_then(Value::as_str) {
                Some("success") => Ok(response.get("response").cloned().unwrap_or(Value::Null)),
                _ => Err(response
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("未说明原因")
                    .to_owned()),
            });
        }
    }

    /// Kill the CLI and return what it wrote to stderr.
    fn stop(&mut self) -> Vec<u8> {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.stderr
            .take()
            .and_then(|stderr| stderr.join().ok())
            .unwrap_or_default()
    }
}

impl Drop for ControlSession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use super::*;

    /// An upstream that records each request line and answers nothing useful.
    fn recording_upstream() -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("绑定本机端口");
        let address = format!("http://{}", listener.local_addr().expect("本机地址"));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&seen);
        std::thread::spawn(move || {
            for mut stream in listener.incoming().map_while(Result::ok) {
                let mut reader = BufReader::new(&stream);
                let mut request_line = String::new();
                let _ = reader.read_line(&mut request_line);
                recorded
                    .lock()
                    .unwrap()
                    .push(request_line.trim().to_owned());
                let _ = stream.write_all(
                    b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}",
                );
            }
        });
        (address, seen)
    }

    /// The listing, end to end against the bundled CLI: the picker's rows, an
    /// alias row's window read by switching to it, and no model request. A
    /// loopback stub key keeps the developer's own login out of it, and the stub
    /// records whatever the CLI asked for.
    #[test]
    fn the_bundled_cli_answers_its_picker_without_a_model_request() {
        let (address, seen) = recording_upstream();
        let mut env = listing_env();
        env.insert("ANTHROPIC_BASE_URL".to_owned(), address);
        env.insert(
            "ANTHROPIC_API_KEY".to_owned(),
            "sk-ant-fake-listing".to_owned(),
        );
        let executable = bundled_executable().expect("Mework 必须能找到自己附带的 Claude Code");
        let cwd = session_cwd("").expect("工作目录");

        let models = list_models_with(&executable, &cwd, &env).expect("CLI 应回答模型列表");

        assert!(!models.is_empty(), "CLI 没有列出任何模型");
        assert!(
            models.iter().all(|model| !model.value.is_empty()),
            "{models:?}"
        );
        assert!(
            models.iter().any(|model| model.context_window.is_some()),
            "没有一行带上下文窗口: {models:?}"
        );
        assert!(
            models
                .iter()
                .filter(|model| model.value.starts_with("claude-"))
                .all(|model| model.context_window.is_none()),
            "显式 id 的行不该被切换过去: {models:?}"
        );
        let seen = seen.lock().unwrap();
        assert!(
            seen.iter().all(|line| !line.contains("/v1/messages")),
            "列模型发起了模型请求: {seen:?}"
        );
    }

    #[test]
    fn the_listing_environment_is_the_probes_plus_the_run_switches() {
        let env = listing_env();
        for name in env.keys() {
            assert!(!name.starts_with("ANTHROPIC_"), "{name} 会改写列出的账号");
            assert_ne!(name, "CLAUDE_CODE_OAUTH_TOKEN");
        }
        for (name, value) in LIST_CONTROL_ENV {
            assert_eq!(env.get(*name).map(String::as_str), Some(*value));
        }
        for (name, value) in probe_env() {
            assert_eq!(env.get(&name), Some(&value));
        }
    }
}
