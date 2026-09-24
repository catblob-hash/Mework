//! Which inherited environment variables must never reach a user's shell.
//!
//! Both spawn paths — `std::process::Command` for the shell tool and hooks, and
//! `portable_pty::CommandBuilder` for the interactive terminal — start from this
//! process's own environment. Under `npm run dev:browser` that environment
//! carries the browser-dev bridge address and its bearer token, and an E2E run
//! adds report URLs and their tokens. The backend genuinely needs those values;
//! a command the user or the model runs does not, and anything that dumps its
//! environment would print them.
//!
//! Scrubbing belongs at the spawn boundary rather than at the launcher, which
//! still has to hand the values to the backend, and rather than inside the
//! PowerShell bootstrap, which only runs after `CreateProcess` has already
//! published the whole block to the child.

use std::ffi::OsStr;
use std::ffi::OsString;

/// The variable a development launcher uses to hand this process the `PATH` it
/// should actually run with.
///
/// `scripts/windows-native-build-tools.mjs` has to put `<msys2>\mingw64\bin` and
/// `<msys2>\usr\bin` *ahead* of `System32` so `cargo` finds the native `gcc`. The
/// application inherits that order and gives it to every shell it opens, where a
/// bare `cmd` then resolves to `<msys2>\usr\bin\cmd` — a bash script — instead of
/// `System32\cmd.exe`; `usr\bin` shadows `find`, `sort`, `more`, `link` and `tar`
/// the same way. Splitting the two `PATH`s has to happen here rather than in the
/// launcher, because `cargo run` compiles and executes under one environment.
///
/// The value is the application `PATH` with the toolchain directories appended
/// rather than removed, so a GNU-target binary can still find its runtime DLLs.
pub(crate) const DEV_APPLICATION_PATH_ENVIRONMENT_NAME: &str = "MEWORK_DEV_APPLICATION_PATH";

/// What `restore_dev_application_path` should write and delete, decided without
/// touching the process environment so it can be tested directly.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DevApplicationPathHandoff {
    /// The `PATH` spellings to overwrite. Empty means leave `PATH` alone.
    pub names: Vec<OsString>,
    pub value: OsString,
    /// Every spelling of the handoff marker, which must be deleted whether or not
    /// a `PATH` is written, so it cannot reach a user's shell.
    pub markers: Vec<OsString>,
}

/// Reads the handoff out of an environment listing. Returns `None` when no
/// launcher marker is present, which is the packaged and user-shell case.
pub(crate) fn dev_application_path_handoff<I>(variables: I) -> Option<DevApplicationPathHandoff>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    let mut names = Vec::new();
    let mut markers = Vec::new();
    let mut value: Option<OsString> = None;
    for (name, variable) in variables {
        // Windows environment names are case-insensitive, so the marker and the
        // `PATH` key have to be matched that way or a different spelling escapes.
        let Some(text) = name.to_str() else { continue };
        let upper = text.to_ascii_uppercase();
        if upper == DEV_APPLICATION_PATH_ENVIRONMENT_NAME {
            markers.push(name);
            value = Some(variable);
        } else if upper == "PATH" {
            names.push(name);
        }
    }
    let value = value?;
    // An empty handoff would blank `PATH` for the whole process. Consume the
    // marker anyway; refusing the write is the safe half of the decision.
    if value.is_empty() {
        return Some(DevApplicationPathHandoff {
            names: Vec::new(),
            value,
            markers,
        });
    }
    if names.is_empty() {
        names.push(OsString::from("PATH"));
    }
    Some(DevApplicationPathHandoff {
        names,
        value,
        markers,
    })
}

/// Applies the launcher's handoff to this process. Must be called from the entry
/// point before any thread or child process exists, because mutating the
/// environment is only sound while the process is single-threaded.
pub(crate) fn restore_dev_application_path() {
    let Some(handoff) = dev_application_path_handoff(std::env::vars_os()) else {
        return;
    };
    // SAFETY: called as the first statement of `run`/`run_browser_dev`, before the
    // Tauri builder, the runtime, or any spawn.
    unsafe {
        for name in &handoff.names {
            std::env::set_var(name, &handoff.value);
        }
        for name in &handoff.markers {
            std::env::remove_var(name);
        }
    }
}

/// Markers around the login shell's environment in [`adopt_login_shell_path`],
/// so whatever its startup files print cannot be mistaken for a variable.
const LOGIN_ENVIRONMENT_BEGIN: &str = "__MEWORK_LOGIN_ENVIRONMENT_BEGIN__";
const LOGIN_ENVIRONMENT_END: &str = "__MEWORK_LOGIN_ENVIRONMENT_END__";

/// Set on the probe, so a startup file that wants to can tell it is being asked
/// for its environment rather than opening a terminal, and skip slow work.
const RESOLVING_ENVIRONMENT_NAME: &str = "MEWORK_RESOLVING_ENVIRONMENT";

/// How long the login shell may take to report its environment. Start-up waits
/// this long at most (see [`adopt_login_shell_path`]).
#[cfg(target_os = "macos")]
const LOGIN_SHELL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// What a process with no locale at all is given, and what an unrecognisable
/// system locale falls back to. The language matters far less than the
/// `UTF-8`: without one, `git`, `python` and `ls` assume ASCII and mangle every
/// non-English file name.
const FALLBACK_LOCALE: &str = "en_US.UTF-8";

/// Gives this process the environment the user's login shell ends up with
/// (macOS): its `PATH`, and everything else their startup files export.
///
/// An app started from Finder, the Dock or Spotlight inherits launchd's
/// environment, whose `PATH` is `/usr/bin:/bin:/usr/sbin:/sbin` and which has
/// nothing `~/.zprofile` or `~/.zshrc` exports: no Homebrew, no `~/.cargo/bin`,
/// no version-manager shims, but also no `https_proxy` (how most users in China
/// reach anything, through Clash), no `SSH_AUTH_SOCK` pointing at 1Password or
/// gpg-agent, no `JAVA_HOME`, `GOPATH`, `ANDROID_HOME`, `NVM_DIR` or `LANG`.
/// Every program Mework starts — the shell tools, hooks, MCP and language
/// servers, preview dev servers, Git — would then run without the setup the
/// user relies on in Terminal, and `bash` itself would always be the system's
/// 3.2 even when a newer one is installed. Windows never had the problem: a GUI
/// app there inherits the user's variables from the system. So the login shell
/// is asked once, at start, for its whole environment, and
/// [`login_environment_writes`] decides how much of it to take. `PATH` is always
/// merged: the login shell's order wins, as it does in Terminal, and entries
/// only the inherited `PATH` had (a development launcher's) are kept after it.
///
/// A probe that fails — a startup file that hangs past the deadline, exits
/// before the command runs, or breaks `PATH` — is reported on stderr and leaves
/// launchd's `PATH` extended by the well-known tool directories that exist, so
/// Homebrew and `cargo` are still found. Either way, a process that ends up with
/// no locale is given the system's, as `UTF-8`.
///
/// Must be called from the entry point before any thread exists, like
/// [`restore_dev_application_path`], because it writes the process environment.
/// That is also why start-up waits for the probe (up to
/// [`LOGIN_SHELL_TIMEOUT`]) instead of running it in the background: the writes
/// cannot happen once other threads may be reading the environment. The probe
/// itself starts no thread: its output goes to a file, so a daemon the startup
/// files leave holding the descriptor cannot keep a reader waiting.
///
/// The name predates everything but `PATH`; the entry points still call it by
/// that name.
pub(crate) fn adopt_login_shell_path() {
    #[cfg(all(target_os = "macos", not(test)))]
    {
        let inherited: Vec<(OsString, OsString)> = std::env::vars_os().collect();
        let mut writes = match login_shell_environment() {
            Ok(login) => login_environment_writes(&inherited, &login),
            Err(reason) => {
                eprintln!("无法读取登录 shell 的环境变量（{reason}），改用常见工具目录补全 PATH");
                let home = variable(&inherited, "HOME").map(std::path::PathBuf::from);
                fallback_search_path(
                    variable(&inherited, "PATH")
                        .map(|path| path.as_os_str())
                        .unwrap_or_default(),
                    home.as_deref(),
                    |directory| directory.is_dir(),
                )
                .map(|path| vec![(OsString::from("PATH"), path)])
                .unwrap_or_default()
            }
        };
        if needs_default_locale(&inherited, &writes) {
            writes.push((OsString::from("LANG"), OsString::from(system_utf8_locale())));
        }
        writes.extend(loopback_proxy_bypass_writes(&inherited, &writes));
        // SAFETY: called from the entry point before the Tauri builder, the
        // runtime or any spawn; the probes above started processes, not threads.
        unsafe {
            for (name, value) in &writes {
                std::env::set_var(name, value);
            }
        }
    }
}

/// Runs the user's login shell and returns the environment it ends up with, or
/// why it could not.
#[cfg(target_os = "macos")]
fn login_shell_environment() -> Result<Vec<(OsString, OsString)>, String> {
    use std::io::{Read, Seek};
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::process::CommandExt;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use wait_timeout::ChildExt;

    let shell = std::env::var_os("SHELL")
        .filter(|shell| Path::new(shell).is_absolute() && Path::new(shell).is_file())
        .unwrap_or_else(|| OsString::from("/bin/zsh"));
    let shell_name = Path::new(&shell).display().to_string();
    let capture_path = std::env::temp_dir().join(format!(
        "mework-login-environment-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let mut capture = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&capture_path)
        .map_err(|error| format!("无法创建临时文件：{error}"))?;
    let output = (|| {
        let mut command = Command::new(&shell);
        // `env -0` rather than `$VAR`s: it is the exported environment exactly,
        // whatever the shell's own syntax (fish joins lists with spaces), and a
        // NUL cannot occur inside a value, so a value spanning lines or holding
        // `=` still parses. The absolute path skips any `env` function or alias
        // the startup files define.
        command
            .args([
                "-i",
                "-l",
                "-c",
                &format!(
                    "echo {LOGIN_ENVIRONMENT_BEGIN}; /usr/bin/env -0; echo {LOGIN_ENVIRONMENT_END}"
                ),
            ])
            .env(RESOLVING_ENVIRONMENT_NAME, "1")
            .stdin(Stdio::null())
            .stdout(
                capture
                    .try_clone()
                    .map_err(|error| format!("无法创建临时文件：{error}"))?,
            )
            .stderr(Stdio::null());
        // The startup files are the user's code like any command they run, and
        // get the same scrub: nothing they do needs the development bridge's
        // token, and a `.zshrc` that logs its environment would record it.
        for name in private_child_environment_names() {
            command.env_remove(&name);
        }
        // A session of its own: an interactive shell must not reach for the
        // terminal a development launch was started from, and a hung one is
        // killed along with everything it started.
        // SAFETY: `setsid` is async-signal-safe and touches no Rust state.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("无法启动 {shell_name}：{error}"))?;
        match child.wait_timeout(LOGIN_SHELL_TIMEOUT) {
            Ok(Some(_)) => {}
            _ => {
                // SAFETY: the child leads its own process group.
                unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
                let _ = child.wait();
                return Err(format!(
                    "{shell_name} 在 {} 秒内没有结束",
                    LOGIN_SHELL_TIMEOUT.as_secs()
                ));
            }
        }
        let mut bytes = Vec::new();
        capture
            .rewind()
            .and_then(|()| (&capture).take(1 << 20).read_to_end(&mut bytes))
            .map_err(|error| format!("无法读取 {shell_name} 的输出：{error}"))?;
        let environment = parse_login_shell_environment(&bytes)
            .ok_or_else(|| format!("{shell_name} 没有执行到输出环境变量的那一步"))?;
        // A startup file that leaves `PATH` without a single directory broke
        // something; the fallback is a better guess than anything it exported.
        if !variable(&environment, "PATH").is_some_and(is_search_path) {
            return Err(format!("{shell_name} 给出的 PATH 无效"));
        }
        Ok(environment)
    })();
    let _ = std::fs::remove_file(&capture_path);
    output
}

/// The variables `env -0` printed between the probe's markers, or `None` when
/// the shell never got that far.
///
/// Every entry `env -0` prints ends in a NUL, and no value can contain one, so
/// the end marker directly after a NUL can only be the real one — a value that
/// happens to contain the marker's text is not preceded by a NUL there. Entries
/// that are not valid UTF-8 or not portable variable names (Bash's exported
/// `BASH_FUNC_name%%` functions) are skipped rather than failing the rest.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_login_shell_environment(output: &[u8]) -> Option<Vec<(OsString, OsString)>> {
    let begin = format!("{LOGIN_ENVIRONMENT_BEGIN}\n");
    let start = find_bytes(output, begin.as_bytes())? + begin.len();
    let dump = &output[start..];
    let length = if dump.starts_with(LOGIN_ENVIRONMENT_END.as_bytes()) {
        0
    } else {
        let end = [b"\0".as_slice(), LOGIN_ENVIRONMENT_END.as_bytes()].concat();
        find_bytes(dump, &end)? + 1
    };
    let mut variables = Vec::new();
    for entry in dump[..length].split(|byte| *byte == 0) {
        let Ok(entry) = std::str::from_utf8(entry) else {
            continue;
        };
        let Some((name, value)) = entry.split_once('=') else {
            continue;
        };
        if is_portable_name(name) {
            variables.push((OsString::from(name), OsString::from(value)));
        }
    }
    Some(variables)
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// A name every shell can export: a letter or `_`, then letters, digits, `_`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn is_portable_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|first| first == b'_' || first.is_ascii_alphabetic())
        && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}

/// Whether a value looks like a search path rather than something a broken
/// startup file left behind.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn is_search_path(value: &OsString) -> bool {
    value.to_str().is_some_and(|value| value.contains('/'))
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn variable<'a>(variables: &'a [(OsString, OsString)], name: &str) -> Option<&'a OsString> {
    variables
        .iter()
        .find(|(candidate, _)| candidate == name)
        .map(|(_, value)| value)
}

/// True for a variable that describes the shell session or terminal the login
/// shell ran in rather than configuration the user exported for their programs,
/// and that must therefore never be copied into this process.
///
/// `PWD`, `OLDPWD`, `SHLVL` and `_` are the probe's own bookkeeping. `TERM` and
/// the terminal families would tell every child that it is attached to a
/// terminal it does not have. `GPG_TTY` is the one users export from `.zshrc`
/// as `$(tty)`, which the probe — no terminal — resolves to "not a tty";
/// gpg-agent would then fail to show its passphrase prompt. `ZDOTDIR` is
/// re-derived by every zsh from the files that set it, and a terminal's shell
/// integration also sets it to point at its own scripts. `XPC_*`, `__CF*`,
/// `SECURITYSESSIONID` and `LaunchInstanceID` are launchd's per-process and
/// per-session labels. `DYLD_*` changes how every helper process this app
/// starts loads code, which a line in a shell profile has no business doing.
/// Finally the probe's own marker, the development-launcher handoffs, and every
/// harness variable [`is_private_child_environment_name`] keeps from children.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn is_session_variable(name: &str) -> bool {
    const NAMES: &[&str] = &[
        "PWD",
        "OLDPWD",
        "SHLVL",
        "_",
        "TERM",
        "COLORTERM",
        "COLUMNS",
        "LINES",
        "TMUX",
        "STY",
        "WINDOW",
        "ZDOTDIR",
        "GPG_TTY",
        "SSH_TTY",
        "SSH_CLIENT",
        "SSH_CONNECTION",
        "LC_TERMINAL",
        "LC_TERMINAL_VERSION",
        "SECURITYSESSIONID",
        "LaunchInstanceID",
        RESOLVING_ENVIRONMENT_NAME,
        DEV_APPLICATION_PATH_ENVIRONMENT_NAME,
        "MEWORK_APPLICATION_PATH",
    ];
    const PREFIXES: &[&str] = &[
        "TERM_",
        "ITERM_",
        "TMUX_",
        "KITTY_",
        "WEZTERM_",
        "ALACRITTY_",
        "GHOSTTY_",
        "VSCODE_",
        "SHELL_SESSION_",
        "XPC_",
        "__CF",
        "DYLD_",
    ];
    NAMES.contains(&name)
        || PREFIXES.iter().any(|prefix| name.starts_with(prefix))
        || is_private_child_environment_name(OsStr::new(name))
}

/// Whether this process was started from a shell, whose environment is the
/// user's live one, rather than by launchd, whose is not.
///
/// Every POSIX shell exports `SHLVL`, and every terminal sets `TERM`; launchd
/// sets neither for an application it starts, including one started with
/// `open` from a terminal.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn started_from_shell(inherited: &[(OsString, OsString)]) -> bool {
    variable(inherited, "SHLVL").is_some() || variable(inherited, "TERM").is_some()
}

/// The variables to write so this process has the login shell's environment,
/// given what it inherited and what the login shell reported.
///
/// Where the inherited environment came from decides who wins a disagreement:
///
/// - Started by launchd, the inherited values are launchd's defaults, and the
///   login shell was started from exactly them. Any value it reports differently
///   is therefore something the user's startup files set on purpose — the
///   1Password `SSH_AUTH_SOCK` replacing launchd's own agent is the common case
///   — and the login shell's value wins.
/// - Started from a shell (a development launch, or `npm run tauri dev`), the
///   inherited environment already *is* the user's, including anything they
///   changed in that terminal after it opened; a fresh login shell would undo
///   that. Only variables it lacks are filled in, which also covers a launcher
///   that sets `SHLVL` without ever reading the user's files.
///
/// Session variables ([`is_session_variable`]) are never written, and a variable
/// only the inherited environment has is never removed. `PATH` is merged in both
/// cases, the login shell's order first.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn login_environment_writes(
    inherited: &[(OsString, OsString)],
    login: &[(OsString, OsString)],
) -> Vec<(OsString, OsString)> {
    let from_shell = started_from_shell(inherited);
    let mut writes = Vec::new();
    for (name, value) in login {
        let Some(text) = name.to_str() else {
            continue;
        };
        if text == "PATH" || !is_portable_name(text) || is_session_variable(text) {
            continue;
        }
        let adopt = match variable(inherited, text) {
            None => true,
            Some(existing) => !from_shell && existing != value,
        };
        if adopt && variable(&writes, text).is_none() {
            writes.push((name.clone(), value.clone()));
        }
    }
    if let Some(login_path) = variable(login, "PATH").filter(|path| is_search_path(path)) {
        let inherited_path = variable(inherited, "PATH").cloned().unwrap_or_default();
        if let Some(merged) = merged_search_path(login_path, &inherited_path) {
            if merged != inherited_path {
                writes.push((OsString::from("PATH"), merged));
            }
        }
    }
    writes
}

/// Proxy variables whose presence makes an HTTP client send requests through a
/// proxy, in the spellings reqwest and curl read.
const PROXY_NAMES: &[&str] = &[
    "http_proxy",
    "HTTP_PROXY",
    "https_proxy",
    "HTTPS_PROXY",
    "all_proxy",
    "ALL_PROXY",
];

/// A `no_proxy` for loopback, written when adopting the login shell's proxy
/// would otherwise send this machine's own traffic through it.
///
/// Until the whole environment was adopted, an app started from Finder had no
/// proxy at all. With the user's `https_proxy` it has one, and neither reqwest
/// (the host's model, MCP and discovery requests) nor curl exempts loopback on
/// its own: a request to Ollama on `127.0.0.1` or to a local MCP server would go
/// to the proxy, which for anything but a proxy on this same machine means
/// "127.0.0.1" is the proxy's own host. So when this adoption is what brought a
/// proxy in, and the user has no `no_proxy` of their own — one they set is kept
/// exactly, even if it proxies loopback on purpose — loopback is exempted, in
/// both spellings so every reader agrees.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn loopback_proxy_bypass_writes(
    inherited: &[(OsString, OsString)],
    writes: &[(OsString, OsString)],
) -> Vec<(OsString, OsString)> {
    let adopted_proxy = PROXY_NAMES
        .iter()
        .any(|name| variable(writes, name).is_some_and(|value| !value.is_empty()));
    let configured_bypass = ["no_proxy", "NO_PROXY"]
        .iter()
        .any(|name| variable(writes, name).or_else(|| variable(inherited, name)).is_some());
    if !adopted_proxy || configured_bypass {
        return Vec::new();
    }
    ["no_proxy", "NO_PROXY"]
        .iter()
        .map(|name| (OsString::from(name), OsString::from("localhost,127.0.0.1,::1")))
        .collect()
}

/// `inherited` with the well-known tool directories that exist appended, for
/// when the login shell could not say what `PATH` should be. `None` when there
/// is nothing to add.
///
/// Appended rather than prepended: without the user's own startup files there
/// is no telling which of `/usr/bin/python3` and Homebrew's they expect to win,
/// and a guess should only ever make a missing tool appear, never change which
/// of two installed ones runs.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn fallback_search_path(
    inherited: &OsStr,
    home: Option<&std::path::Path>,
    exists: impl Fn(&std::path::Path) -> bool,
) -> Option<OsString> {
    let mut candidates: Vec<std::path::PathBuf> =
        ["/opt/homebrew/bin", "/opt/homebrew/sbin", "/usr/local/bin"]
            .into_iter()
            .map(std::path::PathBuf::from)
            .collect();
    if let Some(home) = home.filter(|home| home.is_absolute()) {
        for relative in [".local/bin", ".cargo/bin", ".bun/bin", ".volta/bin"] {
            candidates.push(home.join(relative));
        }
    }
    candidates.retain(|candidate| exists(candidate));
    let extra = std::env::join_paths(candidates).ok()?;
    let merged = merged_search_path(inherited, &extra)?;
    (merged.as_os_str() != inherited).then_some(merged)
}

/// Whether nothing among the locale variables that decide the character set is
/// set once `writes` are applied over `inherited`. An empty value counts as
/// unset, as it does to `setlocale`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn needs_default_locale(
    inherited: &[(OsString, OsString)],
    writes: &[(OsString, OsString)],
) -> bool {
    ["LC_ALL", "LC_CTYPE", "LANG"].into_iter().all(|name| {
        variable(writes, name)
            .or_else(|| variable(inherited, name))
            .is_none_or(|value| value.is_empty())
    })
}

/// The system's language and region as a `UTF-8` locale name, for a process
/// that has none. Terminal sets `LANG` this way for the shells it opens, which
/// is why a user never sees the problem there.
#[cfg(target_os = "macos")]
fn system_utf8_locale() -> String {
    let apple_locale = apple_locale().unwrap_or_default();
    utf8_locale_from_apple_locale(&apple_locale, |name| {
        std::path::Path::new("/usr/share/locale")
            .join(name)
            .is_dir()
    })
}

/// `AppleLocale` from the global preferences domain, such as `zh_CN`,
/// `zh-Hans_CN`, `zh_Hans_TW` or `en_US@rg=cnzzzz`.
#[cfg(target_os = "macos")]
fn apple_locale() -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use wait_timeout::ChildExt;

    let mut child = Command::new("/usr/bin/defaults")
        .args(["read", "-g", "AppleLocale"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // `defaults` answers from cfprefsd in milliseconds; the bound only keeps a
    // wedged preferences daemon from holding up start-up.
    match child.wait_timeout(std::time::Duration::from_secs(2)) {
        Ok(Some(status)) if status.success() => {}
        Ok(Some(_)) => return None,
        _ => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    }
    let mut text = String::new();
    child
        .stdout
        .take()?
        .take(256)
        .read_to_string(&mut text)
        .ok()?;
    Some(text.trim().to_owned())
}

/// Turns an `AppleLocale` value into a `UTF-8` locale name that `exists`
/// confirms, or [`FALLBACK_LOCALE`].
///
/// Only the language and region survive: `@rg=` and `@currency=` overrides
/// are preferences no C library understands, and a value that does not have
/// the `ll_CC` shape is not trusted to name anything. For Chinese the script
/// decides before the region does, because it is the written form a program
/// will print: `zh_Hans_TW` is a reader of Simplified Chinese who happens to be
/// in Taiwan, not a reader of Traditional.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn utf8_locale_from_apple_locale(apple_locale: &str, exists: impl Fn(&str) -> bool) -> String {
    let base = apple_locale.trim().split('@').next().unwrap_or_default();
    let parts: Vec<&str> = base.split(['_', '-']).collect();
    let language = parts[0];
    if !(2..=3).contains(&language.len()) || !language.bytes().all(|byte| byte.is_ascii_lowercase())
    {
        return FALLBACK_LOCALE.to_owned();
    }
    let script = parts[1..].iter().copied().find(|part| {
        part.len() == 4
            && part.starts_with(|first: char| first.is_ascii_uppercase())
            && part[1..].bytes().all(|byte| byte.is_ascii_lowercase())
    });
    let region = parts[1..]
        .last()
        .copied()
        .filter(|part| part.len() == 2 && part.bytes().all(|byte| byte.is_ascii_uppercase()));
    let mut candidates = Vec::new();
    match (language, script) {
        ("zh", Some("Hans")) => candidates.push("zh_CN".to_owned()),
        ("zh", Some("Hant")) => candidates.push(
            if region == Some("HK") {
                "zh_HK"
            } else {
                "zh_TW"
            }
            .to_owned(),
        ),
        _ => {}
    }
    if let Some(region) = region {
        candidates.push(format!("{language}_{region}"));
    }
    candidates
        .into_iter()
        .map(|candidate| format!("{candidate}.UTF-8"))
        .find(|candidate| exists(candidate))
        .unwrap_or_else(|| FALLBACK_LOCALE.to_owned())
}

/// `primary`'s entries in order, then the entries only `secondary` has.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn merged_search_path(primary: &OsStr, secondary: &OsStr) -> Option<OsString> {
    let mut merged: Vec<std::path::PathBuf> = Vec::new();
    for entry in std::env::split_paths(primary).chain(std::env::split_paths(secondary)) {
        if !entry.as_os_str().is_empty() && !merged.contains(&entry) {
            merged.push(entry);
        }
    }
    std::env::join_paths(merged).ok()
}

/// True for a variable that exists to wire up development and end-to-end
/// harnesses, and that a user command has no reason to inherit.
///
/// The rule is deliberately a rule and not a list: every one of these families
/// grows a new member whenever a harness gains an option, and a list silently
/// stops covering them. It stays narrow by requiring the project's own prefix
/// *and* a harness marker, so `MEWORK_TERMINAL_*` (which the terminal injects
/// immediately after this scrub), `MEWORK_HOOK_EVENT`, `PATH`, the user's API
/// keys, and ordinary `VITE_*` project settings are all kept.
pub(crate) fn is_private_child_environment_name(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    // Windows environment names are case-insensitive, so a child could otherwise
    // reintroduce the value under a different spelling.
    let name = name.to_ascii_uppercase();
    let owned_by_project = name.starts_with("MEWORK_") || name.starts_with("VITE_");
    owned_by_project && (name.contains("BROWSER_DEV") || name.contains("E2E"))
}

/// Every name in this process's environment that `is_private_child_environment_name`
/// rejects, in the spelling the environment actually uses — which is what a
/// removal has to be keyed on.
pub(crate) fn private_child_environment_names() -> Vec<std::ffi::OsString> {
    std::env::vars_os()
        .map(|(name, _)| name)
        .filter(|name| is_private_child_environment_name(name))
        .collect()
}

/// Normalize only proxy bypass variables. Configuration overrides inheritance, including
/// an explicit empty value. POSIX prefers `no_proxy`; Windows prefers `NO_PROXY`.
/// Other spellings are a deterministic fallback, never dependent on insertion order.
/// This normalizes list syntax, not client-specific matching semantics.
pub(crate) fn normalized_proxy_bypass(
    inherited: &std::collections::BTreeMap<String, String>,
    configured: &std::collections::BTreeMap<String, String>,
    windows: bool,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    let select = |values: &std::collections::BTreeMap<String, String>| {
        let preferred = if windows { "NO_PROXY" } else { "no_proxy" };
        values
            .get(preferred)
            .or_else(|| {
                values
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case("NO_PROXY"))
                    .map(|(_, value)| value)
            })
            .cloned()
    };
    let Some(value) = select(configured).or_else(|| select(inherited)) else {
        return Ok(Default::default());
    };
    let mut entries = Vec::new();
    for entry in value.split(|c: char| c == ',' || c == ';' || c.is_ascii_whitespace()) {
        if entry.is_empty() {
            continue;
        }
        if entry.contains("://")
            || entry.contains(['?', '#', '@'])
            || (entry.contains('*') && entry != "*")
            || entry.chars().any(char::is_control)
        {
            return Err(
                "Invalid NO_PROXY entry: use host, IP, CIDR or * entries, not URLs or host globs"
                    .into(),
            );
        }
        if !entries.contains(&entry) {
            entries.push(entry);
        }
    }
    let value = entries.join(",");
    let mut output = std::collections::BTreeMap::new();
    output.insert("NO_PROXY".into(), value.clone());
    if !windows {
        output.insert("no_proxy".into(), value);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn pairs(variables: &[(OsString, OsString)]) -> Vec<(&str, &str)> {
        variables
            .iter()
            .map(|(name, value)| (name.to_str().unwrap(), value.to_str().unwrap()))
            .collect()
    }

    /// What the probe writes: startup-file noise, the begin marker, `env -0`'s
    /// NUL-terminated entries, the end marker, and whatever `.zlogout` prints.
    fn probe_output(entries: &[&[u8]]) -> Vec<u8> {
        let mut output = b"Last login: today\nwelcome =^.^= NOT=A_VARIABLE\n".to_vec();
        output.extend_from_slice(LOGIN_ENVIRONMENT_BEGIN.as_bytes());
        output.push(b'\n');
        for entry in entries {
            output.extend_from_slice(entry);
            output.push(0);
        }
        output.extend_from_slice(LOGIN_ENVIRONMENT_END.as_bytes());
        output.extend_from_slice(b"\nlogout noise\0JUNK=1\n");
        output
    }

    #[test]
    fn the_login_environment_is_read_only_from_between_its_markers() {
        let multi_line_value = format!("line one\nline two with a={LOGIN_ENVIRONMENT_END}");
        let multi_line = format!("MULTI={multi_line_value}");
        let output = probe_output(&[
            b"PATH=/opt/homebrew/bin:/usr/bin",
            b"https_proxy=http://127.0.0.1:7890",
            b"EMPTY=",
            b"EQUALS=a=b=c",
            multi_line.as_bytes(),
            // Bash's exported function: not a name anything else could export.
            b"BASH_FUNC_greet%%=() {  echo hi\n}",
            b"NOT_UTF8=\xff\xfe",
            b"no equals sign",
        ]);
        let parsed = parse_login_shell_environment(&output).expect("both markers are present");
        assert_eq!(
            pairs(&parsed),
            vec![
                ("PATH", "/opt/homebrew/bin:/usr/bin"),
                ("https_proxy", "http://127.0.0.1:7890"),
                ("EMPTY", ""),
                ("EQUALS", "a=b=c"),
                ("MULTI", multi_line_value.as_str()),
            ]
        );

        // An empty environment is still a complete answer.
        let empty = format!("{LOGIN_ENVIRONMENT_BEGIN}\n{LOGIN_ENVIRONMENT_END}\n");
        assert_eq!(
            parse_login_shell_environment(empty.as_bytes()),
            Some(Vec::new())
        );
        // A shell that died before the command, or whose output was cut short,
        // did not report an environment at all.
        assert_eq!(parse_login_shell_environment(b"PATH=/usr/bin\0"), None);
        let truncated = format!("{LOGIN_ENVIRONMENT_BEGIN}\nPATH=/usr/bin\0HOME=/Us");
        assert_eq!(parse_login_shell_environment(truncated.as_bytes()), None);
    }

    /// Asks this machine's real login shell, which is the only way to know the
    /// probe survives an actual set of startup files.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "runs the user's login shell and their startup files"]
    fn a_real_login_shell_reports_its_environment() {
        let environment =
            login_shell_environment().expect("the login shell reports its environment");
        let path = variable(&environment, "PATH").expect("a PATH");
        let entries: Vec<_> = std::env::split_paths(path).collect();
        assert!(
            entries
                .iter()
                .any(|entry| entry == std::path::Path::new("/usr/bin")),
            "{path:?}"
        );
        assert_eq!(
            variable(&environment, RESOLVING_ENVIRONMENT_NAME),
            Some(&OsString::from("1"))
        );
    }

    #[test]
    fn session_and_harness_variables_are_never_adopted() {
        for name in [
            "PWD",
            "OLDPWD",
            "SHLVL",
            "_",
            "TERM",
            "TERM_PROGRAM",
            "TERM_PROGRAM_VERSION",
            "TERM_SESSION_ID",
            "COLORTERM",
            "ITERM_SESSION_ID",
            "ITERM_PROFILE",
            "LC_TERMINAL",
            "TMUX",
            "TMUX_PANE",
            "STY",
            "ZDOTDIR",
            "GPG_TTY",
            "SSH_TTY",
            "XPC_SERVICE_NAME",
            "XPC_FLAGS",
            "__CF_USER_TEXT_ENCODING",
            "__CFBundleIdentifier",
            "SECURITYSESSIONID",
            "DYLD_INSERT_LIBRARIES",
            "VSCODE_INJECTION",
            RESOLVING_ENVIRONMENT_NAME,
            DEV_APPLICATION_PATH_ENVIRONMENT_NAME,
            "MEWORK_APPLICATION_PATH",
            "MEWORK_BROWSER_DEV_TOKEN",
            "VITE_WEB_SEARCH_E2E_REPORT_TOKEN",
        ] {
            assert!(is_session_variable(name), "{name} must not be adopted");
        }
        for name in [
            "http_proxy",
            "https_proxy",
            "all_proxy",
            "no_proxy",
            "HTTPS_PROXY",
            "SSH_AUTH_SOCK",
            "JAVA_HOME",
            "GOPATH",
            "ANDROID_HOME",
            "NVM_DIR",
            "PYENV_ROOT",
            "LANG",
            "LC_ALL",
            "LC_CTYPE",
            "HOMEBREW_PREFIX",
            "MANPATH",
            "TERMINFO_DIRS",
            "MEWORK_DIR",
        ] {
            assert!(!is_session_variable(name), "{name} must be adopted");
        }
    }

    #[cfg(unix)]
    const LAUNCHD_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";
    #[cfg(unix)]
    const LOGIN_PATH: &str = "/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin";

    /// Started by launchd: the login shell began from exactly this environment,
    /// so every value it reports differently is the user's own configuration.
    #[cfg(unix)]
    #[test]
    fn a_launchd_start_takes_the_login_shells_values() {
        let inherited = variables(&[
            ("PATH", LAUNCHD_PATH),
            ("HOME", "/Users/u"),
            (
                "SSH_AUTH_SOCK",
                "/private/tmp/com.apple.launchd.x/Listeners",
            ),
            ("XPC_SERVICE_NAME", "application.app.mework"),
        ]);
        let login = variables(&[
            ("PATH", LOGIN_PATH),
            ("HOME", "/Users/u"),
            ("SSH_AUTH_SOCK", "/Users/u/.1password/agent.sock"),
            ("https_proxy", "http://127.0.0.1:7890"),
            ("JAVA_HOME", "/opt/homebrew/opt/openjdk"),
            ("XPC_SERVICE_NAME", "0"),
            ("PWD", "/Users/u"),
            ("SHLVL", "1"),
            ("TERM", "xterm-256color"),
            ("GPG_TTY", "not a tty"),
            (RESOLVING_ENVIRONMENT_NAME, "1"),
        ]);
        assert_eq!(
            pairs(&login_environment_writes(&inherited, &login)),
            vec![
                ("SSH_AUTH_SOCK", "/Users/u/.1password/agent.sock"),
                ("https_proxy", "http://127.0.0.1:7890"),
                ("JAVA_HOME", "/opt/homebrew/opt/openjdk"),
                ("PATH", LOGIN_PATH),
            ]
        );
    }

    /// Started from a shell: that environment is the user's live one, so it
    /// keeps every value it has and is only filled in; `PATH` still merges.
    #[cfg(unix)]
    #[test]
    fn a_shell_start_only_fills_in_what_is_missing() {
        let inherited = variables(&[
            ("PATH", "/repo/node_modules/.bin:/usr/bin:/bin"),
            ("SHLVL", "2"),
            ("SSH_AUTH_SOCK", "/tmp/forwarded.sock"),
            ("http_proxy", ""),
        ]);
        let login = variables(&[
            ("PATH", LOGIN_PATH),
            ("SHLVL", "3"),
            ("SSH_AUTH_SOCK", "/Users/u/.1password/agent.sock"),
            ("http_proxy", "http://127.0.0.1:7890"),
            ("GOPATH", "/Users/u/go"),
        ]);
        assert_eq!(
            pairs(&login_environment_writes(&inherited, &login)),
            vec![
                ("GOPATH", "/Users/u/go"),
                (
                    "PATH",
                    "/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:/repo/node_modules/.bin"
                ),
            ]
        );

        // A terminal with nothing to add writes nothing, not even `PATH`.
        let complete = variables(&[("PATH", LOGIN_PATH), ("TERM", "xterm-256color")]);
        let same = variables(&[("PATH", LOGIN_PATH), ("TERM", "dumb")]);
        assert!(login_environment_writes(&complete, &same).is_empty());
    }

    /// A login shell whose `PATH` names no directory broke something; its
    /// `PATH` is not merged, whatever else it reports.
    #[cfg(unix)]
    #[test]
    fn a_broken_login_path_is_not_merged() {
        let inherited = variables(&[("PATH", LAUNCHD_PATH)]);
        let login = variables(&[("PATH", "oops"), ("GOPATH", "/Users/u/go")]);
        assert_eq!(
            pairs(&login_environment_writes(&inherited, &login)),
            vec![("GOPATH", "/Users/u/go")]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_probe_appends_only_the_tool_directories_that_exist() {
        let present = [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/Users/u/.cargo/bin",
            "/Users/u/.volta/bin",
        ];
        let exists = |directory: &std::path::Path| {
            present
                .iter()
                .any(|candidate| directory == std::path::Path::new(candidate))
        };
        assert_eq!(
            fallback_search_path(
                OsStr::new("/usr/local/bin:/usr/bin:/bin"),
                Some(std::path::Path::new("/Users/u")),
                exists,
            ),
            Some(OsString::from(
                "/usr/local/bin:/usr/bin:/bin:/opt/homebrew/bin:/Users/u/.cargo/bin:/Users/u/.volta/bin"
            ))
        );
        // No home, or a relative one, adds only the system-wide directories.
        assert_eq!(
            fallback_search_path(
                OsStr::new(LAUNCHD_PATH),
                Some(std::path::Path::new("relative")),
                exists,
            ),
            Some(OsString::from(
                "/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin:/usr/local/bin"
            ))
        );
        // Nothing new to add leaves PATH alone.
        assert_eq!(
            fallback_search_path(OsStr::new(LAUNCHD_PATH), None, |_| false),
            None
        );
    }

    #[test]
    fn a_default_locale_is_needed_only_when_nothing_sets_the_character_set() {
        let none = variables(&[("HOME", "/Users/u")]);
        assert!(needs_default_locale(&none, &[]));
        assert!(needs_default_locale(&variables(&[("LANG", "")]), &[]));
        // `LC_MESSAGES` alone does not pick a character set.
        assert!(needs_default_locale(
            &variables(&[("LC_MESSAGES", "zh_CN.UTF-8")]),
            &[]
        ));
        for name in ["LANG", "LC_ALL", "LC_CTYPE"] {
            assert!(!needs_default_locale(
                &variables(&[(name, "zh_CN.UTF-8")]),
                &[]
            ));
            // Adopted from the login shell counts as set.
            assert!(!needs_default_locale(
                &none,
                &variables(&[(name, "en_GB.UTF-8")])
            ));
        }
    }

    #[test]
    fn the_system_locale_becomes_a_utf8_locale_that_exists() {
        let installed = [
            "zh_CN.UTF-8",
            "zh_TW.UTF-8",
            "zh_HK.UTF-8",
            "en_US.UTF-8",
            "de_DE.UTF-8",
        ];
        let locale =
            |value: &str| utf8_locale_from_apple_locale(value, |name| installed.contains(&name));
        assert_eq!(locale("zh_CN"), "zh_CN.UTF-8");
        assert_eq!(locale("zh-Hans_CN"), "zh_CN.UTF-8");
        // The script decides which written Chinese, not the region.
        assert_eq!(locale("zh_Hans_TW"), "zh_CN.UTF-8");
        assert_eq!(locale("zh-Hant_TW"), "zh_TW.UTF-8");
        assert_eq!(locale("zh_Hant_HK"), "zh_HK.UTF-8");
        assert_eq!(locale("de_DE\n"), "de_DE.UTF-8");
        // Region and currency overrides are preferences, not locale names.
        assert_eq!(locale("en_US@rg=cnzzzz"), "en_US.UTF-8");
        assert_eq!(locale("de_DE@currency=EUR"), "de_DE.UTF-8");
        // English with a Chinese region has no locale of its own.
        assert_eq!(locale("en_CN"), FALLBACK_LOCALE);
        // Anything not shaped like `ll_CC` is never trusted to name a locale.
        for value in [
            "",
            "es_419",
            "EN_us",
            "zh",
            "../../etc/passwd",
            "en_US.UTF-8/x",
        ] {
            assert_eq!(locale(value), FALLBACK_LOCALE, "{value:?}");
        }
    }

    /// The real `defaults` answer on this machine always becomes a UTF-8 locale.
    #[cfg(target_os = "macos")]
    #[test]
    fn this_machines_locale_is_utf8() {
        let locale = system_utf8_locale();
        assert!(locale.ends_with(".UTF-8"), "{locale}");
        assert!(
            std::path::Path::new("/usr/share/locale")
                .join(&locale)
                .is_dir(),
            "{locale}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_login_shell_order_leads_and_inherited_extras_follow() {
        let merged = merged_search_path(
            OsStr::new("/opt/homebrew/bin:/usr/bin:/bin"),
            OsStr::new("/usr/bin:/bin:/usr/sbin:/repo/node_modules/.bin::/bin"),
        )
        .unwrap();
        assert_eq!(
            merged,
            OsString::from("/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/repo/node_modules/.bin")
        );
    }

    fn private(name: &str) -> bool {
        is_private_child_environment_name(OsStr::new(name))
    }

    #[test]
    fn proxy_bypass_normalization_preserves_scope_and_explicit_precedence() {
        let map = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect()
        };
        for windows in [false, true] {
            for name in ["NO_PROXY", "no_proxy", "No_Proxy"] {
                let output =
                    normalized_proxy_bypass(&map(&[(name, "a;b a, c")]), &map(&[]), windows)
                        .unwrap();
                assert_eq!(output["NO_PROXY"], "a,b,c");
                assert_eq!(output.len(), if windows { 1 } else { 2 });
                if !windows {
                    assert_eq!(output["no_proxy"], "a,b,c");
                }
                let empty = normalized_proxy_bypass(
                    &map(&[(name, "inherited")]),
                    &map(&[("no_proxy", "")]),
                    windows,
                )
                .unwrap();
                assert_eq!(empty["NO_PROXY"], "");
            }
            let both = map(&[("NO_PROXY", "upper"), ("no_proxy", "lower")]);
            let output = normalized_proxy_bypass(&map(&[]), &both, windows).unwrap();
            assert_eq!(output["NO_PROXY"], if windows { "upper" } else { "lower" });
            let output =
                normalized_proxy_bypass(&both, &map(&[("No_Proxy", "configured")]), windows)
                    .unwrap();
            assert_eq!(output["NO_PROXY"], "configured");
        }
        for value in [
            "*",
            "10.0.0.0/8",
            "::1",
            "[::1]:8080",
            "example.com:443",
            ".example.com",
        ] {
            let result =
                normalized_proxy_bypass(&map(&[]), &map(&[("NO_PROXY", value)]), false).unwrap();
            assert_eq!(result["no_proxy"], value);
        }
        assert!(normalized_proxy_bypass(&map(&[]), &map(&[]), false)
            .unwrap()
            .is_empty());
        for value in [
            "http://localhost",
            "*.example.com",
            "user@host",
            "host?query",
        ] {
            assert!(
                normalized_proxy_bypass(&map(&[]), &map(&[("NO_PROXY", value)]), false).is_err()
            );
        }
    }

    #[test]
    fn every_harness_variable_is_private_and_nothing_else_is() {
        for name in [
            // The bridge address and the bearer token that authenticates to it.
            "MEWORK_BROWSER_DEV_TOKEN",
            "MEWORK_BROWSER_DEV_SUPPLIED_TOKEN",
            "MEWORK_BROWSER_DEV_ADDRESS",
            "MEWORK_BROWSER_DEV_ORIGIN",
            "MEWORK_BROWSER_DEV_INSTANCE_ID",
            "MEWORK_BROWSER_DEV_DATA_IDENTIFIER",
            "VITE_BROWSER_DEV_TOKEN",
            "VITE_BROWSER_DEV_BACKEND_URL",
            // E2E harness wiring, including its own report tokens.
            "MEWORK_MEMORY_E2E_RUN_ID",
            "MEWORK_IMAGE_INPUT_E2E_REPORT_PORT",
            "MEWORK_WEB_SEARCH_E2E",
            "VITE_IMAGE_INPUT_E2E_REPORT_TOKEN",
            "VITE_WEB_SEARCH_E2E_REPORT_TOKEN",
            "VITE_MEMORY_E2E_ENABLED",
            // Case-insensitive: Windows would treat these as the same variable.
            "mework_browser_dev_token",
            "Vite_Browser_Dev_Token",
        ] {
            assert!(private(name), "{name} must not reach a user command");
        }

        for name in [
            // Injected immediately after the scrub; removing it would break the
            // control handshake the terminal depends on.
            "MEWORK_TERMINAL_CONTROL_NONCE",
            "MEWORK_TERMINAL_ACK_EVENT",
            "MEWORK_TERMINAL_REJECT_EVENT",
            // Product variables a command is entitled to see.
            "MEWORK_HOOK_EVENT",
            "MEWORK_DIR",
            "MEWORK_KERNEL_SHADOW",
            "VITE_POLICY_EXPECTATION",
            // Nothing outside the project's own namespace is ever touched.
            "PATH",
            "Path",
            "USERPROFILE",
            "TERM",
            "OPENAI_API_KEY",
            "E2E_SOMETHING_ELSE",
            "BROWSER_DEV_TOKEN",
        ] {
            assert!(!private(name), "{name} must still be inherited");
        }
    }

    #[test]
    fn names_are_collected_in_the_spelling_the_environment_uses() {
        // A removal keyed on the canonical upper-case spelling would miss the
        // entry on a case-sensitive platform.
        let name = OsString::from("Mework_Browser_Dev_Probe_Name");
        // SAFETY: single-threaded assertion over a name no other test uses.
        unsafe { std::env::set_var(&name, "value") };
        let collected = private_child_environment_names();
        unsafe { std::env::remove_var(&name) };
        assert!(collected.contains(&name), "collected: {collected:?}");
    }

    fn variables(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(name, value)| (OsString::from(*name), OsString::from(*value)))
            .collect()
    }

    const BUILD_PATH: &str = "C:\\msys64\\mingw64\\bin;C:\\msys64\\usr\\bin;C:\\WINDOWS\\system32";
    const APPLICATION_PATH: &str =
        "C:\\WINDOWS\\system32;C:\\msys64\\mingw64\\bin;C:\\msys64\\usr\\bin";

    #[test]
    fn the_launcher_handoff_restores_the_application_path_and_is_consumed() {
        let handoff = dev_application_path_handoff(variables(&[
            ("Path", BUILD_PATH),
            (DEV_APPLICATION_PATH_ENVIRONMENT_NAME, APPLICATION_PATH),
            ("MEWORK_DIR", "C:\\app"),
        ]))
        .expect("a marked environment must produce a handoff");
        assert_eq!(handoff.names, vec![OsString::from("Path")]);
        assert_eq!(handoff.value, OsString::from(APPLICATION_PATH));
        assert_eq!(
            handoff.markers,
            vec![OsString::from(DEV_APPLICATION_PATH_ENVIRONMENT_NAME)]
        );

        // The marker is matched case-insensitively, and the write lands on the
        // spelling the environment actually uses.
        let handoff = dev_application_path_handoff(variables(&[
            ("PATH", BUILD_PATH),
            ("Mework_Dev_Application_Path", APPLICATION_PATH),
        ]))
        .expect("a lower-case marker is the same variable on Windows");
        assert_eq!(handoff.names, vec![OsString::from("PATH")]);
        assert_eq!(
            handoff.markers,
            vec![OsString::from("Mework_Dev_Application_Path")]
        );

        // No PATH of its own: the restore still has to publish one.
        let handoff = dev_application_path_handoff(variables(&[(
            DEV_APPLICATION_PATH_ENVIRONMENT_NAME,
            APPLICATION_PATH,
        )]))
        .expect("the handoff does not depend on an inherited PATH");
        assert_eq!(handoff.names, vec![OsString::from("PATH")]);
    }

    #[test]
    fn an_unmarked_or_empty_handoff_never_rewrites_path() {
        // The packaged application and every user shell: nothing to restore, and
        // the inherited PATH must be left exactly as it is.
        assert_eq!(
            dev_application_path_handoff(variables(&[
                ("Path", APPLICATION_PATH),
                ("MEWORK_DIR", "C:\\app"),
            ])),
            None
        );
        assert_eq!(dev_application_path_handoff(variables(&[])), None);

        // An empty marker would blank PATH for the whole process; consume it, but
        // never write it.
        let handoff = dev_application_path_handoff(variables(&[
            ("Path", BUILD_PATH),
            (DEV_APPLICATION_PATH_ENVIRONMENT_NAME, ""),
        ]))
        .expect("an empty marker still has to be deleted");
        assert!(handoff.names.is_empty());
        assert_eq!(
            handoff.markers,
            vec![OsString::from(DEV_APPLICATION_PATH_ENVIRONMENT_NAME)]
        );
    }

    #[test]
    fn the_restored_path_puts_system32_ahead_of_the_msys_shadows() {
        // The whole point of the split: `cmd`, `find`, `sort`, `link` and `tar`
        // exist under `<msys2>\usr\bin` too, so whichever directory comes first
        // decides what a shell the application opens actually runs. The handoff
        // therefore has to win over the inherited build `PATH`, not merge with it.
        let handoff = dev_application_path_handoff(variables(&[
            ("Path", BUILD_PATH),
            (DEV_APPLICATION_PATH_ENVIRONMENT_NAME, APPLICATION_PATH),
        ]))
        .expect("a marked environment must produce a handoff");
        let restored = handoff.value.to_str().expect("the handoff is UTF-8");
        assert_ne!(restored, BUILD_PATH, "恢复必须换掉构建 PATH 而不是沿用它");
        let entries: Vec<&str> = restored.split(';').collect();
        let system32 = entries
            .iter()
            .position(|entry| entry.eq_ignore_ascii_case("C:\\WINDOWS\\system32"))
            .expect("System32 must survive the restore");
        let usr_bin = entries
            .iter()
            .position(|entry| entry.eq_ignore_ascii_case("C:\\msys64\\usr\\bin"))
            .expect("the toolchain is appended, not dropped, so DLLs stay findable");
        assert!(system32 < usr_bin, "{restored}");

        let build: Vec<&str> = BUILD_PATH.split(';').collect();
        assert!(
            build
                .iter()
                .position(|entry| *entry == "C:\\msys64\\mingw64\\bin")
                < build
                    .iter()
                    .position(|entry| entry.eq_ignore_ascii_case("C:\\WINDOWS\\system32")),
            "构建 PATH 必须继续把 mingw64 排在前面，否则 gcc 会解析错"
        );
    }

    #[test]
    fn an_adopted_proxy_exempts_loopback_unless_the_user_chose_a_bypass() {
        let pairs = |items: &[(&str, &str)]| -> Vec<(OsString, OsString)> {
            items
                .iter()
                .map(|(name, value)| (OsString::from(name), OsString::from(value)))
                .collect()
        };
        let adopted = pairs(&[("https_proxy", "http://proxy.corp:8080")]);
        let exempted = loopback_proxy_bypass_writes(&[], &adopted);
        assert_eq!(
            exempted,
            pairs(&[
                ("no_proxy", "localhost,127.0.0.1,::1"),
                ("NO_PROXY", "localhost,127.0.0.1,::1"),
            ])
        );
        // A bypass the user configured, inherited or adopted, is theirs.
        assert!(loopback_proxy_bypass_writes(&pairs(&[("NO_PROXY", "")]), &adopted).is_empty());
        let with_bypass = pairs(&[
            ("https_proxy", "http://proxy.corp:8080"),
            ("no_proxy", ".corp"),
        ]);
        assert!(loopback_proxy_bypass_writes(&[], &with_bypass).is_empty());
        // A proxy that was already inherited, or none at all, changes nothing.
        let inherited = pairs(&[("https_proxy", "http://127.0.0.1:7890")]);
        assert!(loopback_proxy_bypass_writes(&inherited, &[]).is_empty());
        assert!(loopback_proxy_bypass_writes(&[], &pairs(&[("https_proxy", "")])).is_empty());
    }
}
