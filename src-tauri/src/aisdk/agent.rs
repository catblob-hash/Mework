//! Host side of the `claude-agent` family: where the Claude Code executable is,
//! which profile variables the CLI needs, and the per-run session lease.
//!
//! The sidecar owns the CLI process and its parked tool handlers; the host only
//! names the session, resolves the executable, and guarantees a `release` frame
//! when the run ends, whichever way it ends.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use wait_timeout::ChildExt as _;

use crate::model::{ApiProvider, FamilySetting, ProviderFamily};

use super::protocol::AgentSession;

/// Directory under the app data root that serves as the CLI's working
/// directory. Claude Code keys its transcript folder by cwd, so a fixed private
/// directory keeps Mework sessions out of the user's own project folders.
const SESSION_DIR: &str = "claude-agent";

/// Profile-location variables the CLI needs to find its configuration and
/// login (`~/.claude`). The sidecar spawns with a cleared environment and the
/// SDK replaces the CLI environment wholesale, so these must be carried
/// explicitly. Credentials are deliberately absent: this family has none — the
/// CLI authenticates with the user's own `claude auth login`.
const CLAUDE_AGENT_ENV: &[&str] = &[
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "ProgramData",
    "HOME",
    "XDG_CONFIG_HOME",
    // Honors a user who relocated their Claude Code configuration.
    "CLAUDE_CONFIG_DIR",
];

/// Executable file name per platform. Only the native build is supported: the
/// npm `claude.cmd`/`cli.js` shims need a Node runtime the single-file sidecar
/// cannot provide.
fn executable_name() -> &'static str {
    if cfg!(windows) {
        "claude.exe"
    } else {
        "claude"
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// Locations searched when the provider leaves `claude_executable` empty, in
/// order: the native installer's `~/.local/bin`, then `PATH`.
fn discovered_candidates() -> Vec<PathBuf> {
    let name = executable_name();
    let mut candidates = Vec::new();
    if let Some(home) = home_dir() {
        candidates.push(home.join(".local").join("bin").join(name));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            if dir.as_os_str().is_empty() {
                continue;
            }
            candidates.push(dir.join(name));
        }
    }
    candidates
}

/// Resolve the Claude Code executable for a provider.
///
/// A configured path must exist as a file: a stale explicit path is a
/// configuration error to surface, not something to silently paper over with
/// discovery. Discovery accepts only the native executable name.
pub(crate) fn resolve_executable(provider: &ApiProvider) -> Result<PathBuf, String> {
    let configured = provider
        .family_settings
        .get(&FamilySetting::ClaudeExecutable)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty());
    if let Some(configured) = configured {
        let path = PathBuf::from(configured);
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!(
            "提供商 {} 配置的 Claude Code 可执行文件不存在：{}",
            provider.name, configured
        ));
    }
    discovered_candidates()
        .into_iter()
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            format!(
                "未找到 Claude Code 可执行文件（{}）。请安装原生版 Claude Code（https://claude.com/claude-code），或在提供商 {} 的设置里填写 claude_executable 路径",
                executable_name(),
                provider.name
            )
        })
}

/// Working directory for the CLI. Created on demand because the CLI refuses a
/// missing cwd; an empty app data path (unit tests, ad-hoc requests) falls back
/// to a temp directory rather than the process cwd, which could be a user repo.
pub(crate) fn session_cwd(app_data_path: &str) -> Result<PathBuf, String> {
    let root = if app_data_path.trim().is_empty() {
        std::env::temp_dir().join("mework").join(SESSION_DIR)
    } else {
        Path::new(app_data_path.trim()).join(SESSION_DIR)
    };
    std::fs::create_dir_all(&root)
        .map_err(|error| format!("无法创建 Claude Agent 工作目录 {}: {error}", root.display()))?;
    Ok(root)
}

fn profile_env() -> BTreeMap<String, String> {
    CLAUDE_AGENT_ENV
        .iter()
        .filter_map(|name| std::env::var(name).ok().map(|value| ((*name).to_owned(), value)))
        .collect()
}

/// Overrides that point the CLI at a fake upstream on this machine, read only
/// in the `browser-dev` build. An end-to-end test needs the CLI to talk to a
/// local test double instead of Anthropic, and `agent.env` is the sole channel
/// the sidecar accepts for that; the shipped build has no such channel at all,
/// so nothing outside [`CLAUDE_AGENT_ENV`] can reach the CLI environment.
///
/// The address must be loopback. A remote one would hand the user's own Claude
/// Code login to whoever runs that host, which is exactly what this family's
/// "no credential of our own" position exists to prevent.
#[cfg(feature = "browser-dev")]
fn test_upstream_env() -> Result<BTreeMap<String, String>, String> {
    let mut overrides = BTreeMap::new();
    let Some(base_url) = std::env::var("MEWORK_CLAUDE_AGENT_BASE_URL")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
    else {
        return Ok(overrides);
    };
    let url = crate::http_util::normalized_base_url(&base_url)?;
    if !url.host().is_some_and(|host| match host {
        url::Host::Domain(domain) => {
            let domain = domain.trim_end_matches('.').to_ascii_lowercase();
            domain == "localhost" || domain.ends_with(".localhost")
        }
        url::Host::Ipv4(address) => address.is_loopback(),
        url::Host::Ipv6(address) => address.is_loopback(),
    }) {
        return Err(format!(
            "MEWORK_CLAUDE_AGENT_BASE_URL 只允许本机测试桩地址（当前为 {base_url}）"
        ));
    }
    overrides.insert("ANTHROPIC_BASE_URL".to_owned(), url.to_string());
    if let Some(key) = std::env::var("MEWORK_CLAUDE_AGENT_API_KEY")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
    {
        overrides.insert("ANTHROPIC_API_KEY".to_owned(), key);
    }
    Ok(overrides)
}

/// Mint a session block for one run or probe. Only the `ClaudeAgent` family
/// gets one; every other family returns `None` so callers can attach it
/// unconditionally.
pub(crate) fn session_for(
    provider: &ApiProvider,
    app_data_path: &str,
) -> Result<Option<AgentSession>, String> {
    if provider.family != ProviderFamily::ClaudeAgent {
        return Ok(None);
    }
    let executable = resolve_executable(provider)?;
    let cwd = session_cwd(app_data_path)?;
    #[cfg_attr(not(feature = "browser-dev"), allow(unused_mut))]
    let mut env = profile_env();
    #[cfg(feature = "browser-dev")]
    env.extend(test_upstream_env()?);
    Ok(Some(AgentSession {
        session: Uuid::new_v4().simple().to_string(),
        executable: executable.to_string_lossy().into_owned(),
        cwd: cwd.to_string_lossy().into_owned(),
        env,
    }))
}

/// Extra variables the login probe needs on top of [`CLAUDE_AGENT_ENV`]. The
/// probe runs with a cleared environment so nothing the user happens to export
/// — an `ANTHROPIC_API_KEY` above all — can change which account the CLI
/// reports; these are what remains necessary for a process to start at all.
const PROBE_ENV: &[&str] = &[
    // Windows: `SYSTEMROOT` must survive or the CLI cannot open a socket, and
    // `ComSpec` is what `cmd`-based launches resolve through. Both spellings are
    // listed because a cleared environment is matched case-sensitively here.
    "SYSTEMROOT",
    "SystemRoot",
    "ComSpec",
    "PATH",
    "TEMP",
    "TMP",
];

/// Values pinned for the probe: it must answer from local state only, and must
/// not be the thing that triggers an auto-update or a telemetry upload.
const PROBE_CONTROL_ENV: &[(&str, &str)] = &[
    ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
    ("DISABLE_TELEMETRY", "1"),
    ("DISABLE_AUTOUPDATER", "1"),
];

/// How long the login probe may take before it is killed. `auth status` answers
/// from local state in well under a second; a hang means the CLI is waiting on
/// something the probe must not wait on.
const LOGIN_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Characters of stderr kept in a probe failure message.
const MAX_STDERR_TAIL: usize = 400;

/// Environment for the login probe. Never contains a credential: no `ANTHROPIC_*`
/// and no `CLAUDE_CODE_OAUTH_TOKEN`, so the answer describes the user's own CLI
/// login and nothing Mework supplied.
fn probe_env() -> BTreeMap<String, String> {
    let mut env = profile_env();
    for name in PROBE_ENV {
        if let Ok(value) = std::env::var(name) {
            env.insert((*name).to_owned(), value);
        }
    }
    for (name, value) in PROBE_CONTROL_ENV {
        env.insert((*name).to_owned(), (*value).to_owned());
    }
    env
}

/// Where the CLI keeps its configuration and login, as the probe environment
/// makes it resolve: an explicit `CLAUDE_CONFIG_DIR`, else `~/.claude`.
fn config_dir() -> String {
    std::env::var("CLAUDE_CONFIG_DIR")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            home_dir().map(|home| home.join(".claude").to_string_lossy().into_owned())
        })
        .unwrap_or_default()
}

/// `claude auth status --json`, as far as the host reads it. Unknown fields are
/// ignored so a CLI release that adds one keeps working.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthStatus {
    #[serde(default)]
    logged_in: bool,
    #[serde(default)]
    auth_method: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    org_name: Option<String>,
    #[serde(default)]
    subscription_type: Option<String>,
}

/// Login state of the local Claude Code CLI, as shown in provider settings.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeAgentLoginStatus {
    pub signed_in: bool,
    /// `claude.ai`, `console`, `none`, or whatever a newer CLI reports — passed
    /// through rather than mapped, so an unknown method is visible instead of
    /// being flattened into "not signed in".
    pub auth_method: String,
    pub email: Option<String>,
    pub org_name: Option<String>,
    pub subscription_type: Option<String>,
    pub executable: String,
    pub config_dir: String,
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// Parse `auth status --json` output.
///
/// The JSON object is extracted rather than parsed from the whole of stdout: a
/// CLI release that prints a migration notice or an update banner first would
/// otherwise turn a perfectly good answer into "not signed in".
fn parse_auth_status(stdout: &str) -> Result<AuthStatus, String> {
    let trimmed = stdout.trim();
    let object = match (trimmed.find('{'), trimmed.rfind('}')) {
        (Some(start), Some(end)) if start < end => &trimmed[start..=end],
        _ => return Err("Claude Code 的登录状态输出不是 JSON".into()),
    };
    serde_json::from_str(object)
        .map_err(|error| format!("无法解析 Claude Code 的登录状态输出：{error}"))
}

/// Read the local CLI's login state.
///
/// `--setting-sources ""` keeps project and enterprise settings files out of the
/// answer, and must precede the subcommand: the CLI rejects it as an unknown
/// option afterwards.
pub(crate) fn login_status(provider: &ApiProvider) -> Result<ClaudeAgentLoginStatus, String> {
    let executable = resolve_executable(provider)?;
    let mut command = Command::new(&executable);
    command
        .args(["--setting-sources", "", "auth", "status", "--json"])
        .env_clear()
        .envs(probe_env())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // Without this the probe flashes a console window on every settings visit.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("无法启动 Claude Code 读取登录状态：{error}"))?;
    let status = child
        .wait_timeout(LOGIN_PROBE_TIMEOUT)
        .map_err(|error| format!("等待 Claude Code 登录状态失败：{error}"))?;
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
        return Err("读取 Claude Code 登录状态超时".into());
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("读取 Claude Code 登录状态失败：{error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    // The answer, not the exit code, is the judge. A logged-out CLI still prints
    // a complete `--json` object, and some releases pair it with a non-zero exit;
    // reading the code first would make "not signed in" indistinguishable from
    // "the probe broke" and hide the only state the user can act on.
    let parsed = match parse_auth_status(&stdout) {
        Ok(parsed) => parsed,
        Err(error) => {
            let error = match output.status.code() {
                Some(code) if code != 0 => format!("{error}（退出码 {code}）"),
                _ => error,
            };
            return Err(with_stderr_tail(&error, &output.stderr));
        }
    };
    Ok(ClaudeAgentLoginStatus {
        signed_in: parsed.logged_in,
        auth_method: non_empty(parsed.auth_method).unwrap_or_else(|| "none".to_owned()),
        email: non_empty(parsed.email),
        org_name: non_empty(parsed.org_name),
        subscription_type: non_empty(parsed.subscription_type),
        executable: executable.to_string_lossy().into_owned(),
        config_dir: config_dir(),
    })
}

/// Append what the CLI wrote to stderr, redacted and clipped. The tail is the
/// only thing that distinguishes "no such subcommand" from "config unreadable".
fn with_stderr_tail(message: &str, stderr: &[u8]) -> String {
    let tail = String::from_utf8_lossy(stderr);
    let tail = crate::http_util::redact_inline_encoded_data(tail.trim());
    if tail.is_empty() {
        return message.to_owned();
    }
    let clipped = tail
        .char_indices()
        .rev()
        .nth(MAX_STDERR_TAIL)
        .map_or(tail.as_str(), |(index, _)| &tail[index..]);
    format!("{message}（{clipped}）")
}

/// Authentication channels removed from the interactive login's environment.
/// The login must produce the same credential the runtime later relies on, and
/// the runtime never sees these; a key or endpoint exported in the user's shell
/// would otherwise make `auth login` look already authenticated or send the
/// exchange somewhere else.
const LOGIN_STRIPPED_ENV: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_CUSTOM_HEADERS",
    "CLAUDE_CODE_OAUTH_TOKEN",
];

/// How long a freshly started terminal or CLI gets to fail. A launcher that
/// exits non-zero inside this window never showed a window at all; one that is
/// still running, or handed off to a terminal server and exited zero, did.
const LOGIN_LAUNCH_GRACE: Duration = Duration::from_millis(600);

/// Environment for the interactive login. Unlike the probe this is the user's
/// own environment: a terminal needs the display, session bus and locale
/// variables the whitelist has no business enumerating, and the login must land
/// in the same `CLAUDE_CONFIG_DIR` the probe and the runtime read. Only the
/// authentication overrides are removed.
fn login_env() -> BTreeMap<String, String> {
    let mut env: BTreeMap<String, String> = std::env::vars().collect();
    for name in LOGIN_STRIPPED_ENV {
        env.remove(*name);
    }
    for (name, value) in PROBE_CONTROL_ENV {
        env.insert((*name).to_owned(), (*value).to_owned());
    }
    env
}

/// Waits out the launch grace window and reports a launcher that already died.
/// `Ok(true)` means the process is still alive or exited cleanly (a terminal
/// server hand-off); `Ok(false)` means it failed before it could show anything.
fn launched(child: &mut std::process::Child) -> Result<bool, String> {
    std::thread::sleep(LOGIN_LAUNCH_GRACE);
    match child.try_wait() {
        Ok(None) => Ok(true),
        Ok(Some(status)) => Ok(status.success()),
        Err(error) => Err(format!("无法确认登录终端是否启动：{error}")),
    }
}

/// POSIX shell single-quoting; the only character that needs care is the quote itself.
#[cfg(target_os = "macos")]
fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// The shell command Terminal runs for the login. `do script` starts a fresh
/// shell from Terminal's own environment, not from ours, so the config
/// directory and the stripped authentication variables have to travel inside
/// the command text: `unset` first, then the directory the probe and the
/// runtime use, then the CLI.
#[cfg(target_os = "macos")]
fn macos_login_shell_command(executable: &Path, config_dir: Option<&str>) -> String {
    let mut command = format!("unset {};", LOGIN_STRIPPED_ENV.join(" "));
    if let Some(dir) = config_dir {
        command.push_str(&format!(" CLAUDE_CONFIG_DIR={}", shell_single_quote(dir)));
    }
    command.push_str(&format!(
        " {} auth login",
        shell_single_quote(&executable.to_string_lossy())
    ));
    command
}

/// AppleScript string literal: only the backslash and the double quote need escaping.
#[cfg(target_os = "macos")]
fn applescript_literal(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Terminal emulators tried in order on Linux, with the flag each one uses to
/// take a command. `x-terminal-emulator` is the Debian alternatives entry, so it
/// is whatever terminal the user actually installed.
#[cfg(all(unix, not(target_os = "macos")))]
const LINUX_TERMINALS: &[(&str, &str)] = &[
    ("x-terminal-emulator", "-e"),
    ("gnome-terminal", "--"),
    ("konsole", "-e"),
    ("xterm", "-e"),
];

/// Run `claude auth login` in a terminal the user can see and type into.
///
/// The login flow is interactive — it prints a URL and waits for a pasted code —
/// so it cannot run headless. Mework starts the terminal and stops there: it
/// never observes the exchange, and the resulting session belongs to the CLI.
pub(crate) fn open_login(provider: &ApiProvider) -> Result<(), String> {
    let executable = resolve_executable(provider)?;
    let env = login_env();

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // The CLI is started directly in a console of its own rather than through
        // `cmd /c start`: `cmd` would re-parse the path and treat `&` or `^` in a
        // directory name as syntax.
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        let mut child = Command::new(&executable)
            .args(["auth", "login"])
            .env_clear()
            .envs(&env)
            .creation_flags(CREATE_NEW_CONSOLE)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("无法打开终端运行 claude auth login：{error}"))?;
        return if launched(&mut child)? {
            Ok(())
        } else {
            Err("claude auth login 启动后立即退出，请在终端里手动运行它".into())
        };
    }

    #[cfg(target_os = "macos")]
    {
        let config_dir = env.get("CLAUDE_CONFIG_DIR").map(String::as_str);
        let script = applescript_literal(&macos_login_shell_command(&executable, config_dir));
        let mut child = Command::new("osascript")
            .arg("-e")
            .arg(format!("tell application \"Terminal\" to do script \"{script}\""))
            .arg("-e")
            .arg("tell application \"Terminal\" to activate")
            .env_clear()
            .envs(&env)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("无法打开终端运行 claude auth login：{error}"))?;
        return if launched(&mut child)? {
            Ok(())
        } else {
            Err("无法让 Terminal 运行 claude auth login，请手动运行它".into())
        };
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        for (terminal, flag) in LINUX_TERMINALS {
            let spawned = Command::new(terminal)
                .arg(flag)
                .arg(&executable)
                .arg("auth")
                .arg("login")
                .env_clear()
                .envs(&env)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            if let Ok(mut child) = spawned {
                if launched(&mut child)? {
                    return Ok(());
                }
            }
        }
        return Err("找不到能运行 claude auth login 的终端程序，请手动运行它".into());
    }

    #[allow(unreachable_code)]
    Err("当前平台不支持自动打开终端，请手动运行 claude auth login".into())
}

/// A run's hold on a sidecar CLI session. Dropping it sends `release`, so
/// every exit path of the turn loop — completion, error, cancellation, panic
/// unwinding — tears the parked CLI query down. Releasing an unknown session
/// is a no-op on the sidecar, so a run that never reached the sidecar is fine.
pub(crate) struct SessionLease {
    session: Option<AgentSession>,
}

impl SessionLease {
    pub(crate) fn acquire(provider: &ApiProvider, app_data_path: &str) -> Result<Self, String> {
        Ok(Self {
            session: session_for(provider, app_data_path)?,
        })
    }

    /// The session block to attach to each step of this run; `None` for
    /// families without one.
    pub(crate) fn session(&self) -> Option<AgentSession> {
        self.session.clone()
    }
}

impl Drop for SessionLease {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            super::process::release_session(&session.session);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ModelProfile;

    fn provider(family: ProviderFamily, executable: &str) -> ApiProvider {
        let mut family_settings = BTreeMap::new();
        if !executable.is_empty() {
            family_settings.insert(FamilySetting::ClaudeExecutable, executable.to_owned());
        }
        ApiProvider {
            id: "claude-agent".into(),
            name: "Claude Agent".into(),
            enabled: true,
            family,
            base_url: String::new(),
            family_settings,
            endpoint_base_urls: BTreeMap::new(),
            notes: String::new(),
            models: Vec::<ModelProfile>::new(),
            active_model_id: None,
        }
    }

    #[test]
    fn other_families_get_no_session_block() {
        let session = session_for(&provider(ProviderFamily::Anthropic, ""), "").unwrap();
        assert!(session.is_none());
    }

    /// A configured path is authoritative: when it does not exist the error
    /// names it instead of falling back to discovery, which could silently run
    /// a different binary than the one the user pointed at.
    #[test]
    fn a_configured_executable_must_exist() {
        let missing = std::env::temp_dir().join("mework-no-such-claude.exe");
        let error = resolve_executable(&provider(
            ProviderFamily::ClaudeAgent,
            &missing.to_string_lossy(),
        ))
        .unwrap_err();
        assert!(error.contains("不存在"), "{error}");
        assert!(error.contains("mework-no-such-claude.exe"), "{error}");
    }

    #[test]
    fn a_configured_executable_is_used_verbatim_and_the_session_carries_profile_env() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join(executable_name());
        std::fs::write(&executable, b"").unwrap();
        let app_data = tempfile::tempdir().unwrap();
        let session = session_for(
            &provider(ProviderFamily::ClaudeAgent, &executable.to_string_lossy()),
            &app_data.path().to_string_lossy(),
        )
        .unwrap()
        .expect("Claude Agent 必须铸出会话块");
        assert_eq!(session.executable, executable.to_string_lossy());
        assert_eq!(
            Path::new(&session.cwd),
            app_data.path().join(SESSION_DIR),
            "cwd 必须是应用数据目录下的私有子目录"
        );
        assert!(Path::new(&session.cwd).is_dir(), "cwd 必须已经创建");
        assert_eq!(session.session.len(), 32, "session 是 simple uuid");
        // The profile variables present in this process must be forwarded; nothing else may be.
        for (name, value) in &session.env {
            assert!(CLAUDE_AGENT_ENV.contains(&name.as_str()), "{name} 不在白名单里");
            assert_eq!(std::env::var(name).ok().as_deref(), Some(value.as_str()));
        }
        for name in ["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_BASE_URL", "PATH"] {
            assert!(!session.env.contains_key(name), "{name} 不得出现在 agent.env");
        }
        // Two runs never share a session key.
        let second = session_for(
            &provider(ProviderFamily::ClaudeAgent, &executable.to_string_lossy()),
            &app_data.path().to_string_lossy(),
        )
        .unwrap()
        .unwrap();
        assert_ne!(session.session, second.session);
    }

    /// Discovery ignores non-native shims: a `claude.cmd` on PATH is not a match.
    #[test]
    fn discovery_only_accepts_the_native_executable_name() {
        let name = executable_name();
        assert!(name == "claude.exe" || name == "claude");
        let candidates = discovered_candidates();
        assert!(candidates
            .iter()
            .all(|candidate| candidate.file_name().and_then(|f| f.to_str()) == Some(name)));
        if let Some(home) = home_dir() {
            assert_eq!(candidates[0], home.join(".local").join("bin").join(name));
        }
    }

    #[test]
    fn the_probe_environment_carries_no_credential() {
        let env = probe_env();
        for name in env.keys() {
            assert!(
                !name.starts_with("ANTHROPIC_"),
                "{name} 会改写探测到的账号"
            );
            assert_ne!(name, "CLAUDE_CODE_OAUTH_TOKEN");
        }
        for (name, value) in PROBE_CONTROL_ENV {
            assert_eq!(env.get(*name).map(String::as_str), Some(*value));
        }
        // A cleared environment still needs the variables a process needs to run.
        if std::env::var("PATH").is_ok() {
            assert!(env.contains_key("PATH"));
        }
        if cfg!(windows) {
            assert!(
                env.contains_key("SYSTEMROOT") || env.contains_key("SystemRoot"),
                "Windows 上缺 SystemRoot 会让 CLI 连 socket 都开不了"
            );
        }
    }

    /// The login runs in the user's own environment (a terminal needs far more
    /// than the probe whitelist), minus every authentication override; the
    /// config directory the probe honours must reach it unchanged.
    #[test]
    fn the_login_environment_is_the_users_own_minus_the_authentication_overrides() {
        let env = login_env();
        for name in LOGIN_STRIPPED_ENV {
            assert!(!env.contains_key(*name), "{name} 会让 auth login 看起来已经登录");
        }
        for (name, value) in PROBE_CONTROL_ENV {
            assert_eq!(env.get(*name).map(String::as_str), Some(*value));
        }
        for (name, value) in std::env::vars() {
            if LOGIN_STRIPPED_ENV.contains(&name.as_str())
                || PROBE_CONTROL_ENV.iter().any(|(control, _)| *control == name)
            {
                continue;
            }
            assert_eq!(env.get(&name), Some(&value), "{name} 必须原样保留");
        }
    }

    /// Terminal starts its own shell, so the stripped variables and the config
    /// directory have to be spelled out in the command text, quoted for `sh`.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_macos_login_command_carries_the_environment_in_the_text() {
        let command = macos_login_shell_command(
            Path::new("/Users/me/it's here/claude"),
            Some("/Users/me/.claude-mework"),
        );
        assert_eq!(
            command,
            "unset ANTHROPIC_API_KEY ANTHROPIC_AUTH_TOKEN ANTHROPIC_BASE_URL \
             ANTHROPIC_CUSTOM_HEADERS CLAUDE_CODE_OAUTH_TOKEN; \
             CLAUDE_CONFIG_DIR='/Users/me/.claude-mework' \
             '/Users/me/it'\\''s here/claude' auth login"
        );
        let plain = macos_login_shell_command(Path::new("/usr/local/bin/claude"), None);
        assert!(plain.ends_with(" '/usr/local/bin/claude' auth login"), "{plain}");
        assert!(!plain.contains("CLAUDE_CONFIG_DIR"), "{plain}");
        assert_eq!(applescript_literal(r#"say "a\b""#), r#"say \"a\\b\""#);
    }

    #[test]
    fn a_signed_in_answer_is_read_field_by_field() {
        let parsed = parse_auth_status(
            r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"anthropic",
                "email":"user@example.com","orgName":"Example Inc","subscriptionType":"max"}"#,
        )
        .unwrap();
        assert!(parsed.logged_in);
        assert_eq!(parsed.auth_method.as_deref(), Some("claude.ai"));
        assert_eq!(parsed.email.as_deref(), Some("user@example.com"));
        assert_eq!(parsed.org_name.as_deref(), Some("Example Inc"));
        assert_eq!(parsed.subscription_type.as_deref(), Some("max"));
    }

    /// Everything but `loggedIn` is optional, and a console login has no
    /// subscription at all. Missing fields must not fail the parse, or the
    /// panel would report a broken CLI to a user who is merely logged out.
    #[test]
    fn a_minimal_answer_still_parses() {
        let parsed = parse_auth_status(r#"{"loggedIn":false}"#).unwrap();
        assert!(!parsed.logged_in);
        assert_eq!(parsed.auth_method, None);
        assert_eq!(parsed.email, None);
        assert_eq!(parsed.subscription_type, None);
    }

    /// A CLI release that prints a notice before its JSON is still answering.
    #[test]
    fn a_banner_before_the_json_does_not_hide_the_answer() {
        let parsed =
            parse_auth_status("Update available: 2.1.259\n{\"loggedIn\":true}\n").unwrap();
        assert!(parsed.logged_in);
    }

    #[test]
    fn output_without_an_object_is_an_error_not_a_logged_out_answer() {
        let error = parse_auth_status("command not found").unwrap_err();
        assert!(error.contains("JSON"), "{error}");
        parse_auth_status("{ not json }").unwrap_err();
    }

    #[test]
    fn a_failure_message_carries_a_clipped_stderr_tail() {
        let noise = "x".repeat(MAX_STDERR_TAIL * 2);
        let message = with_stderr_tail("读取失败", noise.as_bytes());
        assert!(message.starts_with("读取失败（"));
        assert!(
            message.chars().count() < noise.chars().count(),
            "尾巴必须被裁剪"
        );
        assert_eq!(with_stderr_tail("读取失败", b"   "), "读取失败");
    }
}
