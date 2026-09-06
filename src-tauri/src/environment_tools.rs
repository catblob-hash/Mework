//! Probes environment dependencies.
//!
//! Resolves executables on PATH, runs their version arguments, and reports results.
//! It never downloads or installs tools, or modifies PATH.
//!
//! Version probes (`<exe> --version`) are short-lived and have no child process
//! trees, so timeouts kill them directly.

use std::{
    path::PathBuf,
    process::{Command, Stdio},
    time::Duration,
};

use wait_timeout::ChildExt;

use crate::model::EnvironmentToolDefinition;

/// Wall-clock limit for one version probe. A broken installed executable must not
/// block the page refresh.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// Maximum amount of version output to read.
const MAX_VERSION_BYTES: usize = 8 * 1024;

/// A built-in environment dependency preset. This code constant is not persisted.
pub struct EnvironmentToolPreset {
    pub name: &'static str,
    pub executable: &'static str,
    pub description: &'static str,
    pub repo_url: &'static str,
    pub homepage: &'static str,
}

/// Built-in presets.
pub const ENVIRONMENT_TOOL_PRESETS: &[EnvironmentToolPreset] = &[
    EnvironmentToolPreset {
        name: "uv",
        executable: "uv",
        description: "Python 包与虚拟环境管理器；许多 MCP 服务器用 uvx 启动。",
        repo_url: "https://github.com/astral-sh/uv",
        homepage: "https://docs.astral.sh/uv/",
    },
    EnvironmentToolPreset {
        name: "Bun",
        executable: "bun",
        description: "JavaScript 运行时与包管理器；部分 MCP 服务器用 bunx 启动。",
        repo_url: "https://github.com/oven-sh/bun",
        homepage: "https://bun.sh",
    },
    EnvironmentToolPreset {
        name: "Node.js",
        executable: "node",
        description: "JavaScript 运行时；npx 启动的 MCP 服务器需要它。",
        repo_url: "https://github.com/nodejs/node",
        homepage: "https://nodejs.org",
    },
    EnvironmentToolPreset {
        name: "Python",
        executable: "python",
        description: "Python 解释器；pipx/uvx 之外的 Python MCP 服务器需要它。",
        repo_url: "https://github.com/python/cpython",
        homepage: "https://www.python.org",
    },
    EnvironmentToolPreset {
        name: "Git",
        executable: "git",
        description: "版本控制；工作区的 Git 面板与差异审阅依赖它。",
        repo_url: "https://github.com/git/git",
        homepage: "https://git-scm.com",
    },
    EnvironmentToolPreset {
        name: "ripgrep",
        executable: "rg",
        description: "快速全文搜索；装了之后内容搜索会明显更快。",
        repo_url: "https://github.com/BurntSushi/ripgrep",
        homepage: "https://github.com/BurntSushi/ripgrep",
    },
    EnvironmentToolPreset {
        name: "fd",
        executable: "fd",
        description: "快速文件查找。",
        repo_url: "https://github.com/sharkdp/fd",
        homepage: "https://github.com/sharkdp/fd",
    },
    EnvironmentToolPreset {
        name: "GitHub CLI",
        executable: "gh",
        description: "GitHub 命令行；用于仓库、PR 与 Issue 操作。",
        repo_url: "https://github.com/cli/cli",
        homepage: "https://cli.github.com",
    },
];

/// Result of one probe. Computed live and never persisted.
///
/// Display metadata travels with the snapshot to keep the Rust and renderer
/// catalogs synchronized.
#[derive(Clone, Debug, Default, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentToolSnapshot {
    pub name: String,
    pub executable: String,
    /// Resolved absolute path; empty when absent.
    pub path: String,
    /// Detected version; empty when parsing fails.
    pub version: String,
    /// Probe failure reason; empty on success.
    pub error: String,
    pub description: String,
    pub repo_url: String,
    pub homepage: String,
    /// Built-in presets cannot be deleted; custom entries can.
    pub builtin: bool,
}

/// Probes environment dependencies, listing built-in presets before custom ones.
pub fn probe(custom: &[EnvironmentToolDefinition]) -> Vec<EnvironmentToolSnapshot> {
    let mut snapshots = ENVIRONMENT_TOOL_PRESETS
        .iter()
        .map(|preset| {
            let mut snapshot =
                probe_one(preset.name, preset.executable, &["--version".to_owned()]);
            snapshot.description = preset.description.to_owned();
            snapshot.repo_url = preset.repo_url.to_owned();
            snapshot.homepage = preset.homepage.to_owned();
            snapshot.builtin = true;
            snapshot
        })
        .collect::<Vec<_>>();
    let builtin_executables = ENVIRONMENT_TOOL_PRESETS
        .iter()
        .map(|preset| preset.executable.to_lowercase())
        .collect::<Vec<_>>();
    for tool in custom {
        // Do not probe duplicate built-in executables; identical cards make
        // ownership of the remove action ambiguous.
        if builtin_executables.contains(&tool.executable.to_lowercase()) {
            continue;
        }
        let arguments = if tool.version_args.is_empty() {
            vec!["--version".to_owned()]
        } else {
            tool.version_args.clone()
        };
        snapshots.push(probe_one(&tool.name, &tool.executable, &arguments));
    }
    snapshots
}

/// Returns whether a probeable executable belongs to a built-in or custom entry.
/// The renderer cannot start a process or open a directory for an arbitrary name.
pub fn is_known_executable(executable: &str, custom: &[EnvironmentToolDefinition]) -> bool {
    let lowered = executable.trim().to_lowercase();
    if lowered.is_empty() {
        return false;
    }
    ENVIRONMENT_TOOL_PRESETS
        .iter()
        .any(|preset| preset.executable.eq_ignore_ascii_case(&lowered))
        || custom
            .iter()
            .any(|tool| tool.executable.to_lowercase() == lowered)
}

fn probe_one(name: &str, executable: &str, version_args: &[String]) -> EnvironmentToolSnapshot {
    let mut snapshot = EnvironmentToolSnapshot {
        name: name.to_owned(),
        executable: executable.to_owned(),
        ..Default::default()
    };
    let Some(resolved) = resolve_on_path(executable) else {
        return snapshot;
    };
    snapshot.path = resolved.to_string_lossy().into_owned();
    match run_version(&resolved, version_args) {
        Ok(version) => snapshot.version = version,
        Err(error) => snapshot.error = error,
    }
    snapshot
}

/// Resolves a bare executable name on PATH.
///
/// Do not use a shell: `where` / `which` delegate resolution to an external
/// program that PATH can replace. This must report the first PATH match.
pub fn resolve_on_path(executable: &str) -> Option<PathBuf> {
    let name = executable.trim();
    if name.is_empty() || name.contains('/') || name.contains('\\') {
        return None;
    }
    let extensions = executable_extensions();
    for directory in std::env::split_paths(&std::env::var_os("PATH")?) {
        if directory.as_os_str().is_empty() {
            continue;
        }
        for extension in &extensions {
            let candidate = directory.join(format!("{name}{extension}"));
            if matches!(std::fs::metadata(&candidate), Ok(metadata) if metadata.is_file()) {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(windows)]
fn executable_extensions() -> Vec<String> {
    // PATHEXT determines which suffixes Windows adds to bare names. Fall back to
    // the three guaranteed extensions; `.CMD` is required for npm/npx/bun shims.
    let raw = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned());
    let mut extensions = raw
        .split(';')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| entry.to_lowercase())
        .collect::<Vec<_>>();
    // Allow extensionless PATH entries, such as an MSYS layout.
    extensions.push(String::new());
    extensions
}

#[cfg(not(windows))]
fn executable_extensions() -> Vec<String> {
    vec![String::new()]
}

fn run_version(executable: &std::path::Path, arguments: &[String]) -> Result<String, String> {
    let mut command = Command::new(executable);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("无法启动：{error}"))?;
    let status = child
        .wait_timeout(PROBE_TIMEOUT)
        .map_err(|error| format!("等待版本输出失败：{error}"))?;
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
        return Err("版本探测超时".into());
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("读取版本输出失败：{error}"))?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    if text.trim().is_empty() {
        // Some tools write their version to stderr, including Python before 3.4.
        text = String::from_utf8_lossy(&output.stderr).into_owned();
    }
    if text.len() > MAX_VERSION_BYTES {
        text.truncate(MAX_VERSION_BYTES);
    }
    Ok(first_version_token(&text))
}

/// Extracts a version number from version output. Displaying a whole line would
/// put prefixes such as `git version 2.51.0.windows.1` in the badge.
fn first_version_token(output: &str) -> String {
    let line = output.lines().find(|line| !line.trim().is_empty()).unwrap_or("");
    for token in line.split_whitespace() {
        let candidate = token.trim_start_matches('v');
        if candidate
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_digit())
        {
            return candidate.trim_end_matches(&[',', ';'][..]).to_owned();
        }
    }
    line.trim().chars().take(64).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_token_survives_a_prefixed_line() {
        assert_eq!(first_version_token("git version 2.51.0.windows.1"), "2.51.0.windows.1");
        assert_eq!(first_version_token("v22.14.0"), "22.14.0");
        assert_eq!(first_version_token("Python 3.13.1"), "3.13.1");
        assert_eq!(first_version_token("uv 0.5.11 (abcdef 2026-01-01)"), "0.5.11");
    }

    #[test]
    fn version_token_falls_back_to_the_first_line() {
        assert_eq!(first_version_token("no numbers here\nsecond"), "no numbers here");
        assert_eq!(first_version_token(""), "");
    }

    #[test]
    fn path_resolution_refuses_anything_that_is_not_a_bare_name() {
        assert!(resolve_on_path("C:/Windows/System32/cmd.exe").is_none());
        assert!(resolve_on_path("../cmd").is_none());
        assert!(resolve_on_path("").is_none());
    }

    #[test]
    fn probe_reports_an_absent_tool_without_an_error() {
        let snapshots = probe(&[EnvironmentToolDefinition {
            name: "definitely-not-installed-xyzzy".into(),
            executable: "definitely-not-installed-xyzzy".into(),
            version_args: Vec::new(),
        }]);
        let custom = snapshots.last().expect("custom tool is probed");
        assert_eq!(custom.path, "");
        assert_eq!(custom.error, "");
    }
}
