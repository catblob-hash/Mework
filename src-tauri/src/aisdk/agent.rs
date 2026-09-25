//! Host side of the `claude-agent` family: where the bundled Claude Code
//! executable is, which profile variables the CLI needs, and the per-run session
//! lease.
//!
//! The sidecar owns the CLI process and its parked tool handlers; the host only
//! names the session, resolves the executable, and guarantees a `release` frame
//! when the run ends, whichever way it ends.
//!
//! The executable is Mework's own: the CLI out of the Agent SDK's platform
//! package, pinned with the SDK and shipped beside the application. The login is
//! not — it stays the user's own `claude auth login` in `~/.claude`, and this
//! family has no credential of its own. See [`bundled_executable`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use wait_timeout::ChildExt as _;

use crate::host_platform::host_platform;
use crate::model::{ApiProvider, ProviderFamily};

use super::protocol::AgentSession;

/// Directory under the app data root that serves as the CLI's working
/// directory. Claude Code keys its transcript folder by cwd, so a fixed private
/// directory keeps Mework sessions out of the user's own project folders.
const SESSION_DIR: &str = "claude-agent";

/// Profile-location variables the CLI needs to find its configuration and
/// login (`~/.claude`). The sidecar spawns with a cleared environment and the
/// SDK replaces the CLI environment wholesale, so these must be carried
/// explicitly. Credentials are deliberately absent: this family has none — the
/// CLI authenticates with the user's own `claude auth login`. Process-level
/// variables (`PATH`, the temp directories, the locale) are not repeated here:
/// the sidecar copies its own inherited environment (`process::INHERITED_ENV`)
/// into the CLI's, so they arrive by that route.
const CLAUDE_AGENT_ENV: &[&str] = &[
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "ProgramData",
    "HOME",
    // macOS keeps the login in the Keychain under the account `$USER`. Without
    // the variable the bundled CLI looks up a different account, and a
    // signed-in user reads as "Not logged in".
    "USER",
    "XDG_CONFIG_HOME",
    // Honors a user who relocated their Claude Code configuration.
    "CLAUDE_CONFIG_DIR",
];

/// Executable file name per platform, for the copy Mework ships and for the
/// staged artifact `build.rs` produces. Only the native build is ever named: the
/// npm `claude.cmd`/`cli.js` shims need a Node runtime the single-file sidecar
/// cannot provide.
fn executable_name() -> String {
    format!("claude{}", host_platform().executable_suffix())
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// Overrides the bundled CLI in development, for driving another build against
/// the fixtures. Debug builds only: a shipped Mework must run the version it
/// shipped with, which is the whole point of bundling one.
#[cfg(debug_assertions)]
const EXECUTABLE_ENV: &str = "MEWORK_CLAUDE_BIN";

/// Where the bundled Claude Code is — and the only place it is looked for.
///
/// Mework ships the CLI out of the Agent SDK's own platform package, so the SDK
/// and the executable it drives cannot disagree: `build.rs` stages it as
/// `binaries/claude-<target-triple>` and `tauri.conf.json` declares it an
/// `externalBin`, which Tauri installs beside the application executable with the
/// triple stripped. That adjacency is the same runtime contract the AI SDK sidecar
/// relies on (`aisdk/process.rs`), and `scripts/package-portable.mjs` carries both
/// files for the same reason.
///
/// The user's own Claude Code install is deliberately not consulted. It drifts
/// with their updates, and CLI releases change behaviour this family depends on —
/// 2.1.278, for one, dropped the switch that kept the CLI from attaching an
/// `# Environment` block of its own, and the replacement (a hooks module that
/// leaves the block out) needs a CLI that loads function hooks. What is *not*
/// bundled is the login: that
/// lives in the user's `~/.claude`, written by their own `claude auth login`, and
/// the bundled CLI reads it from there like any other.
///
/// Development has no installed layout, so debug builds fall back to the source
/// tree: the SDK's platform package under `aisdk-service/node_modules`, then the
/// copy `build.rs` staged.
pub(crate) fn bundled_executable() -> Result<PathBuf, String> {
    #[cfg(debug_assertions)]
    if let Ok(value) = std::env::var(EXECUTABLE_ENV) {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!(
            "{EXECUTABLE_ENV} 指向的 Claude Code 不存在：{}",
            path.display()
        ));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(candidate) = exe.parent().map(|dir| dir.join(executable_name())) {
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    #[cfg(debug_assertions)]
    for candidate in source_tree_candidates() {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(format!(
        "找不到 Mework 附带的 Claude Code（{}）。{}",
        executable_name(),
        if cfg!(debug_assertions) {
            "开发时先在 aisdk-service/ 里 `npm install` 装上 Agent SDK 的平台包，再重新 `cargo build` 让 build.rs 把它 staged 出来。"
        } else {
            "本次安装不完整，请重新安装 Mework。"
        }
    ))
}

/// The bundled CLI as it can be found before the application is installed: the
/// Agent SDK's platform package first, because it is by definition the version the
/// pinned SDK expects, then the artifact `build.rs` staged from it.
///
/// This exists only in debug builds because it embeds `CARGO_MANIFEST_DIR`;
/// release builds use the adjacent `externalBin`.
#[cfg(debug_assertions)]
fn source_tree_candidates() -> Vec<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let Some(repo) = manifest.parent() else {
        return Vec::new();
    };
    let name = executable_name();
    let mut candidates = Vec::new();
    let modules = repo.join("aisdk-service").join("node_modules");
    // Mirrors `build.rs::claude_code_platform_package`, which is the authority on
    // the naming; a mismatch here only costs the fallback, not the release.
    let platform = host_platform().npm_platform_tag();
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x64"
    };
    let libc = if cfg!(target_env = "musl") { "-musl" } else { "" };
    candidates.push(
        modules
            .join("@anthropic-ai")
            .join(format!("claude-agent-sdk-{platform}-{arch}{libc}"))
            .join(name),
    );
    // `build.rs` derives the triple from Cargo's `TARGET`; do not recompute it here.
    candidates.push(manifest.join("binaries").join(format!(
        "claude-{}{}",
        env!("MEWORK_TARGET_TRIPLE"),
        host_platform().executable_suffix()
    )));
    candidates
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
        .filter_map(|name| {
            std::env::var(name)
                .ok()
                .map(|value| ((*name).to_owned(), value))
        })
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
    let executable = bundled_executable()?;
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
    // The POSIX spelling. On macOS it names a per-user private directory under
    // /var/folders; without it the CLI falls back to the shared, world-writable
    // /tmp for whatever it stages while answering.
    "TMPDIR",
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
        .or_else(|| home_dir().map(|home| home.join(".claude").to_string_lossy().into_owned()))
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

/// Login state of the Claude Code CLI, as shown in provider settings. The
/// executable is Mework's own; the login it reports is the user's.
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

/// Read the login state the CLI keeps on this machine.
///
/// The probe runs the bundled CLI, but the state it reads is the user's own: the
/// login lives in `~/.claude`, written by their `claude auth login`, and nothing
/// about bundling an executable changes whose account answers.
///
/// `--setting-sources ""` keeps project and enterprise settings files out of the
/// answer, and must precede the subcommand: the CLI rejects it as an unknown
/// option afterwards.
pub(crate) fn login_status() -> Result<ClaudeAgentLoginStatus, String> {
    let executable = bundled_executable()?;
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
#[cfg(any(windows, all(unix, not(target_os = "macos"))))]
const LOGIN_LAUNCH_GRACE: Duration = Duration::from_millis(600);

/// How long `open` may take to hand the login script to Terminal. It exits as
/// soon as LaunchServices has delivered the document, launching Terminal first
/// if needed, which is seconds even on a cold start; past this it is stuck.
#[cfg(target_os = "macos")]
const LOGIN_OPEN_TIMEOUT: Duration = Duration::from_secs(15);

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
#[cfg(any(windows, all(unix, not(target_os = "macos"))))]
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

/// The shell command Terminal runs for the login. Terminal starts the script
/// from a fresh shell with its own environment, not ours, so the config
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

/// The `.command` file Terminal runs for the login. It deletes itself and its
/// private directory before anything else: `sh` keeps reading from the open
/// descriptor, and the executable and config paths it names do not outlive the
/// start. `rmdir` only ever removes an empty directory, so a `$0` that is not
/// the path Mework wrote cannot take anything else with it.
#[cfg(target_os = "macos")]
fn macos_login_script(executable: &Path, config_dir: Option<&str>) -> String {
    format!(
        "#!/bin/sh\nrm -f \"$0\"\nrmdir \"${{0%/*}}\" 2>/dev/null\n{}\n",
        macos_login_shell_command(executable, config_dir)
    )
}

/// Write the login script into a fresh directory of its own under the user's
/// temp directory (`$TMPDIR`, per-user and private on macOS). The directory is
/// created 0700 without `-p`, and the file with `create_new`, so neither can be
/// a name someone planted in advance, even if the temp directory falls back to
/// the shared /tmp.
#[cfg(target_os = "macos")]
fn write_macos_login_script(contents: &str) -> Result<PathBuf, String> {
    use std::io::Write as _;
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};

    let dir = std::env::temp_dir().join(format!(
        "mework-claude-login-{}",
        Uuid::new_v4().simple()
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&dir)
        .map_err(|error| {
            format!("无法创建 claude auth login 的临时目录 {}：{error}", dir.display())
        })?;
    let script = dir.join("claude-auth-login.command");
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .open(&script)
        .and_then(|mut file| {
            // `mode` above passes through the umask; the execute bit is what lets
            // Terminal run the file at all, so it is set outright.
            file.set_permissions(std::fs::Permissions::from_mode(0o700))?;
            file.write_all(contents.as_bytes())
        });
    if let Err(error) = written {
        remove_macos_login_script(&script);
        return Err(format!("无法写入 claude auth login 的临时脚本：{error}"));
    }
    Ok(script)
}

/// Best-effort removal of a login script Terminal never started, and of its
/// directory. Either may already be gone: the script removes both itself.
#[cfg(target_os = "macos")]
fn remove_macos_login_script(script: &Path) {
    let _ = std::fs::remove_file(script);
    if let Some(dir) = script.parent() {
        let _ = std::fs::remove_dir(dir);
    }
}

/// Hand the login script to Terminal through LaunchServices. Opening a
/// `.command` document is not an Apple Event, unlike `osascript … do script`:
/// no Automation prompt, no -1743 under the hardened runtime, and no "Don't
/// Allow" that sticks until the user digs through System Settings. `-a` pins
/// Terminal even where another app is the `.command` handler. `open` exits once
/// the document is delivered, so its status is the verdict; no grace window.
#[cfg(target_os = "macos")]
fn open_macos_login_script(script: &Path, env: &BTreeMap<String, String>) -> Result<(), String> {
    let mut child = Command::new("/usr/bin/open")
        .args(["-a", "Terminal"])
        .arg(script)
        .env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("无法打开终端运行 claude auth login：{error}"))?;
    let status = child
        .wait_timeout(LOGIN_OPEN_TIMEOUT)
        .map_err(|error| format!("无法确认登录终端是否启动：{error}"))?;
    let Some(status) = status else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("等待 Terminal 打开 claude auth login 超时，请手动运行它".into());
    };
    if status.success() {
        return Ok(());
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("无法确认登录终端是否启动：{error}"))?;
    let message = match status.code() {
        Some(code) => format!("无法让 Terminal 运行 claude auth login，请手动运行它（退出码 {code}）"),
        None => "无法让 Terminal 运行 claude auth login，请手动运行它".to_owned(),
    };
    Err(with_stderr_tail(&message, &output.stderr))
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
pub(crate) fn open_login() -> Result<(), String> {
    let executable = bundled_executable()?;
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
        let script = write_macos_login_script(&macos_login_script(&executable, config_dir))?;
        let opened = open_macos_login_script(&script, &env);
        if opened.is_err() {
            // The script only deletes itself once Terminal runs it, which a failed
            // (or timed-out) hand-off gives no reason to expect; one Terminal did
            // start is already gone, and the removal is then a no-op.
            remove_macos_login_script(&script);
        }
        return opened;
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

    fn provider(family: ProviderFamily) -> ApiProvider {
        ApiProvider {
            id: "claude-agent".into(),
            name: "Claude Agent".into(),
            enabled: true,
            family,
            base_url: String::new(),
            family_settings: BTreeMap::new(),
            endpoint_base_urls: BTreeMap::new(),
            notes: String::new(),
            models: Vec::<ModelProfile>::new(),
            active_model_id: None,
        }
    }

    #[test]
    fn other_families_get_no_session_block() {
        let session = session_for(&provider(ProviderFamily::Anthropic), "").unwrap();
        assert!(session.is_none());
    }

    /// The version lock, end to end: whatever this build resolves as *the*
    /// Claude Code must report the number `build.rs` pinned. A drifted
    /// `node_modules`, a staged copy left over from an earlier pin, or an
    /// `externalBin` that picked up something else all fail here rather than
    /// silently changing CLI behaviour the family depends on.
    #[test]
    fn the_bundled_executable_reports_the_pinned_claude_code_version() {
        let executable = bundled_executable().expect("Mework 必须能找到自己附带的 Claude Code");
        let mut command = Command::new(&executable);
        command
            .arg("--version")
            .env_clear()
            .envs(probe_env())
            .stdin(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let output = command.output().expect("run the bundled Claude Code");
        let reported = String::from_utf8_lossy(&output.stdout);
        assert!(
            reported.trim().starts_with(env!("MEWORK_CLAUDE_CODE_VERSION")),
            "附带的 Claude Code 报告的是 {reported:?}，而 build.rs 钉的是 {}",
            env!("MEWORK_CLAUDE_CODE_VERSION"),
        );
    }

    /// The development fallback mirrors `build.rs::claude_code_platform_package`.
    /// The SDK's own package comes first: it is by definition the build the pinned
    /// SDK expects, while the staged copy can lag a `npm install`.
    #[cfg(debug_assertions)]
    #[test]
    fn the_source_tree_fallback_names_the_sdks_platform_package_first() {
        let candidates = source_tree_candidates();
        assert_eq!(candidates.len(), 2, "{candidates:?}");
        let first = candidates[0].to_string_lossy().replace('\\', "/");
        assert!(
            first.contains("aisdk-service/node_modules/@anthropic-ai/claude-agent-sdk-"),
            "{first}"
        );
        assert!(first.ends_with(&executable_name()), "{first}");
        let staged = candidates[1].to_string_lossy().replace('\\', "/");
        assert!(
            staged.contains(&format!("binaries/claude-{}", env!("MEWORK_TARGET_TRIPLE"))),
            "{staged}"
        );
    }

    /// The session names the bundled executable — the provider has no say in it —
    /// and carries the profile variables the CLI needs to find its own login.
    #[test]
    fn the_session_carries_the_bundled_executable_and_the_profile_env() {
        let executable = bundled_executable().expect("Mework 必须能找到自己附带的 Claude Code");
        let app_data = tempfile::tempdir().unwrap();
        let session = session_for(
            &provider(ProviderFamily::ClaudeAgent),
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
            assert!(
                CLAUDE_AGENT_ENV.contains(&name.as_str()),
                "{name} 不在白名单里"
            );
            assert_eq!(std::env::var(name).ok().as_deref(), Some(value.as_str()));
        }
        for name in [
            "ANTHROPIC_API_KEY",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "ANTHROPIC_BASE_URL",
            "PATH",
        ] {
            assert!(
                !session.env.contains_key(name),
                "{name} 不得出现在 agent.env"
            );
        }
        // Two runs never share a session key.
        let second = session_for(
            &provider(ProviderFamily::ClaudeAgent),
            &app_data.path().to_string_lossy(),
        )
        .unwrap()
        .unwrap();
        assert_ne!(session.session, second.session);
    }

    #[test]
    fn the_probe_environment_carries_no_credential() {
        let env = probe_env();
        for name in env.keys() {
            assert!(!name.starts_with("ANTHROPIC_"), "{name} 会改写探测到的账号");
            assert_ne!(name, "CLAUDE_CODE_OAUTH_TOKEN");
        }
        for (name, value) in PROBE_CONTROL_ENV {
            assert_eq!(env.get(*name).map(String::as_str), Some(*value));
        }
        // A cleared environment still needs the variables a process needs to run.
        if std::env::var("PATH").is_ok() {
            assert!(env.contains_key("PATH"));
        }
        // macOS's per-user temp directory, not the shared /tmp.
        if let Ok(tmpdir) = std::env::var("TMPDIR") {
            assert_eq!(env.get("TMPDIR"), Some(&tmpdir));
        }
        if host_platform().is_windows() {
            assert!(
                env.contains_key("SYSTEMROOT") || env.contains_key("SystemRoot"),
                "Windows 上缺 SystemRoot 会让 CLI 连 socket 都开不了"
            );
        }
    }

    /// The Keychain account the CLI reads its macOS login from is `$USER`, so
    /// both the probe and every run must carry it; a probe without it reports a
    /// signed-in user as signed out.
    #[test]
    fn the_keychain_account_reaches_the_probe_and_the_run() {
        let Ok(user) = std::env::var("USER") else {
            return;
        };
        assert_eq!(probe_env().get("USER"), Some(&user));
        assert_eq!(profile_env().get("USER"), Some(&user));
    }

    /// The login runs in the user's own environment (a terminal needs far more
    /// than the probe whitelist), minus every authentication override; the
    /// config directory the probe honours must reach it unchanged.
    #[test]
    fn the_login_environment_is_the_users_own_minus_the_authentication_overrides() {
        let env = login_env();
        for name in LOGIN_STRIPPED_ENV {
            assert!(
                !env.contains_key(*name),
                "{name} 会让 auth login 看起来已经登录"
            );
        }
        for (name, value) in PROBE_CONTROL_ENV {
            assert_eq!(env.get(*name).map(String::as_str), Some(*value));
        }
        for (name, value) in std::env::vars() {
            if LOGIN_STRIPPED_ENV.contains(&name.as_str())
                || PROBE_CONTROL_ENV
                    .iter()
                    .any(|(control, _)| *control == name)
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
        assert!(
            plain.ends_with(" '/usr/local/bin/claude' auth login"),
            "{plain}"
        );
        assert!(!plain.contains("CLAUDE_CONFIG_DIR"), "{plain}");
    }

    /// The `.command` file is the same command behind a self-removing prologue.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_macos_login_script_removes_itself_before_the_command() {
        let executable = Path::new("/Applications/Mework.app/Contents/MacOS/claude");
        let script = macos_login_script(executable, Some("/Users/me/.claude"));
        assert_eq!(
            script,
            format!(
                "#!/bin/sh\nrm -f \"$0\"\nrmdir \"${{0%/*}}\" 2>/dev/null\n{}\n",
                macos_login_shell_command(executable, Some("/Users/me/.claude"))
            )
        );
    }

    /// End to end through `sh`, as Terminal runs it: hostile characters in both
    /// interpolated paths stay data, the authentication overrides are gone, the
    /// CLI gets `auth login`, and nothing is left on disk afterwards.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_macos_login_script_is_private_quoted_and_self_removing() {
        use std::os::unix::fs::PermissionsExt as _;

        let bin = tempfile::tempdir().unwrap();
        let fake = bin.path().join("it's a \"claude\" $HOME `id`");
        std::fs::write(
            &fake,
            "#!/bin/sh\nprintf '%s|%s|%s' \"$CLAUDE_CONFIG_DIR\" \"$*\" \"${ANTHROPIC_API_KEY-unset}\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
        let config_dir = "/tmp/it's $(id) `id` \\ \"dir\"";

        let script = write_macos_login_script(&macos_login_script(&fake, Some(config_dir))).unwrap();
        let dir = script.parent().unwrap().to_owned();
        assert_eq!(script.extension().and_then(|value| value.to_str()), Some("command"));
        for path in [&dir, &script] {
            let mode = std::fs::metadata(path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "{} 必须只有属主可访问", path.display());
        }

        let output = Command::new(&script)
            .env("ANTHROPIC_API_KEY", "sk-ant-should-never-reach-login")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            format!("{config_dir}|auth login|unset")
        );
        assert!(!script.exists(), "脚本启动后必须删掉自己");
        assert!(!dir.exists(), "脚本的私有目录也必须一起删掉");
    }

    /// The fallback cleanup for a hand-off that failed removes both the script
    /// and its directory.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_unopened_macos_login_script_is_cleaned_up() {
        let script = write_macos_login_script("#!/bin/sh\n").unwrap();
        let dir = script.parent().unwrap().to_owned();
        remove_macos_login_script(&script);
        assert!(!script.exists());
        assert!(!dir.exists());
        // Already gone: still quiet.
        remove_macos_login_script(&script);
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
        let parsed = parse_auth_status("Update available: 2.1.259\n{\"loggedIn\":true}\n").unwrap();
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
