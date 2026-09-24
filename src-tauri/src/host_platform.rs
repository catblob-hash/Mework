//! Which machine Mework itself is running on, asked once and answered everywhere.
//!
//! Every *other* machine Mework works with is a [`RunTarget`]: its family is
//! probed the first time it is reached and remembered for the life of the
//! process (see [`crate::remote_shell`]). The host is the one machine that
//! needs no probe — the binary was built for it — but it used to be asked about
//! wherever the answer mattered, with a two-way `cfg!` that had no macOS arm,
//! so macOS silently answered every one of them with Linux's answer, whether or
//! not Linux's answer was right for it. Path case is the plainest example: a
//! default macOS volume is case-insensitive like Windows, not case-sensitive
//! like Linux.
//!
//! So the host is resolved once, at startup, into [`HostPlatform`] — three
//! named platforms, no default arm — and every later branch reads that value.
//! Call sites should prefer the named capability over the platform itself:
//! `host_platform().paths_are_case_insensitive()` says why the code branches,
//! where `== HostPlatform::Windows` only says where it was written.
//!
//! What stays a `#[cfg]` attribute is code that cannot exist on another
//! platform — a `windows_sys` call, a `libc` termios call. Those are already
//! each platform writing its own; this module is for the decisions that are
//! made in shared code.
//!
//! [`RunTarget`]: crate::model::RunTarget

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::OnceLock,
    time::Duration,
};

use wait_timeout::ChildExt;

/// The machine Mework itself runs on.
///
/// Deliberately closed: a new platform has to be added here and then answered
/// at every `match`, which is the point — nothing gets a silent default.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostPlatform {
    Windows,
    Macos,
    Linux,
}

/// Resolved once per process. A `OnceLock` rather than a `const` so the answer
/// has one observable moment — [`resolve_at_startup`] — and one place to read.
static RESOLVED: OnceLock<HostPlatform> = OnceLock::new();

/// The host platform, resolved at startup and unchanged for the life of the
/// process.
///
/// Nothing can set it: what the host claims to be is not something a renderer,
/// a model, or a configuration file may influence.
pub fn host_platform() -> HostPlatform {
    *RESOLVED.get_or_init(HostPlatform::detect)
}

/// Performs the one resolution, before any thread or window exists.
///
/// Reading it later gives the same value; this exists so the resolution has a
/// stated place in startup instead of happening at whichever call site happened
/// to run first.
pub fn resolve_at_startup() -> HostPlatform {
    host_platform()
}

impl HostPlatform {
    /// The only place in the crate that asks the compiler what it built for.
    fn detect() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::Macos
        } else {
            Self::Linux
        }
    }

    /// The platform's name as people write it, for text shown to a person or
    /// to the model.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Windows => "Windows",
            Self::Macos => "macOS",
            Self::Linux => "Linux",
        }
    }

    pub fn is_windows(self) -> bool {
        matches!(self, Self::Windows)
    }

    pub fn is_macos(self) -> bool {
        matches!(self, Self::Macos)
    }

    /// Suffix an executable file name carries here.
    pub fn executable_suffix(self) -> &'static str {
        match self {
            Self::Windows => ".exe",
            Self::Macos | Self::Linux => "",
        }
    }

    /// Whether two spellings that differ only in case name the same file.
    ///
    /// True on Windows, and true on macOS: a default APFS (and HFS+) volume is
    /// case-insensitive and case-preserving, so `~/Code/App` and `~/code/app`
    /// are one directory there. macOS can be installed on a case-sensitive
    /// volume, and this answers for the common one — the same approximation
    /// Windows gets, where a case-sensitive directory flag also exists.
    pub fn paths_are_case_insensitive(self) -> bool {
        match self {
            Self::Windows | Self::Macos => true,
            Self::Linux => false,
        }
    }

    /// Whether WSL distributions can exist here at all.
    pub fn has_wsl(self) -> bool {
        self.is_windows()
    }

    /// The platform as Rust's own `std::env::consts::OS` spells it, which is
    /// the spelling the model's environment block has always carried.
    pub fn os_tag(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Macos => "macos",
            Self::Linux => "linux",
        }
    }

    /// The platform segment npm packages use, which is also what
    /// `build.rs::claude_code_platform_package` names.
    pub fn npm_platform_tag(self) -> &'static str {
        match self {
            Self::Windows => "win32",
            Self::Macos => "darwin",
            Self::Linux => "linux",
        }
    }

    /// The command that hands a path or URL to whatever the desktop opens it
    /// with, or `None` on Windows, which goes through `ShellExecute` instead of
    /// a child process.
    // Only the non-Windows builds of its callers ask, so a Windows build never does.
    #[cfg_attr(windows, allow(dead_code))]
    pub fn desktop_opener(self) -> Option<&'static str> {
        match self {
            Self::Windows => None,
            Self::Macos => Some("open"),
            Self::Linux => Some("xdg-open"),
        }
    }

    /// Path comparison key: the spelling that makes two paths that name one
    /// file compare equal here. Separators are normalized on every platform so
    /// one key rule serves all three.
    pub fn path_key(self, path: &str) -> String {
        let value = path.replace('\\', "/");
        if self.paths_are_case_insensitive() {
            value.to_lowercase()
        } else {
            value
        }
    }
}

/// The names macOS ships in `/usr/bin` as stand-ins for the Xcode command line
/// tools. On a Mac they are one binary under all of these names (hard links of
/// `/usr/bin/git`), plus `xcrun`, the forwarder they are built on.
const DEVELOPER_TOOL_SHIMS: &str = "\
    DeRez GetFileInfo ResMerger Rez SetFile SplitForks ar as asa bison bm4 c++ c++filt c89 c99 \
    cc clang clang++ clangd cmpdylib codesign_allocate cpp ctags ctf_insert dsymutil dwarfdump \
    dyld_info flex flex++ g++ gatherheaderdoc gcc gcov git git-receive-pack git-shell \
    git-upload-archive git-upload-pack gm4 gnumake gperf hdxml2manxml headerdoc2html indent \
    install_name_tool ld lex libtool lipo lldb llvm-g++ llvm-gcc lorder m4 make mig nm nmedit \
    objdump otool pagestuff pip3 python3 ranlib resolveLinks rpcgen segedit size sourcekit-lsp \
    strings strip swift swiftc unifdef unifdefall vtool xcrun xml2man yacc";

/// How long `xcode-select -p` may take before the tools count as absent. It
/// only reads a link, so this is a bound on something broken, not a budget.
const DEVELOPER_DIRECTORY_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Whether `path` is a macOS command-line-tools stand-in that cannot run as the
/// tool it is named for, because the tools are not installed.
///
/// Such a stand-in (`/usr/bin/git`, `/usr/bin/python3`, `/usr/bin/make`, …)
/// answers every run by opening the system's "install the command line
/// developer tools" dialog, again on each run, so a program that resolves to
/// one has to count as absent: the caller's own "not installed" answer is the
/// true one, and a background probe must never put a system dialog in front of
/// the user. Once the tools (or Xcode) are selected the same files forward to
/// the real tools and are kept. Nothing is a stand-in off macOS.
pub fn is_uninstalled_developer_tool_shim(path: &Path) -> bool {
    host_platform().is_macos()
        && is_developer_tool_shim_path(path)
        && !developer_tools_installed()
}

/// Whether `path` is one of the stand-ins, whether or not the tools behind it
/// are there. Resolved first, so a link elsewhere on PATH that points at a
/// stand-in is one too.
fn is_developer_tool_shim_path(path: &Path) -> bool {
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
    names_developer_tool_shim(&resolved)
}

/// The location-and-name half of [`is_developer_tool_shim_path`], on a path
/// that is already resolved.
fn names_developer_tool_shim(resolved: &Path) -> bool {
    // The default macOS volume is case-insensitive, so `/usr/bin/Git` runs the
    // same file.
    let in_usr_bin = resolved
        .parent()
        .is_some_and(|directory| directory.as_os_str().eq_ignore_ascii_case("/usr/bin"));
    in_usr_bin
        && resolved
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| {
                DEVELOPER_TOOL_SHIMS
                    .split_ascii_whitespace()
                    .any(|shim| shim.eq_ignore_ascii_case(name))
            })
}

/// Whether the command line tools behind the stand-ins are present: asked once
/// per process, like the platform itself.
///
/// Installing the tools while Mework runs is therefore seen after a restart;
/// asking on every lookup would put a process spawn in front of every Git call.
fn developer_tools_installed() -> bool {
    static INSTALLED: OnceLock<bool> = OnceLock::new();
    *INSTALLED.get_or_init(probe_developer_directory)
}

/// `xcode-select -p` exits 0 and names the active developer directory when
/// the tools or Xcode are selected, and that directory still has to exist.
///
/// `xcode-select` is not one of the stand-ins — it answers without a dialog
/// either way — which is why it, and never a stand-in, is what gets asked. It
/// is run by absolute path so PATH cannot substitute it.
fn probe_developer_directory() -> bool {
    let Ok(mut child) = Command::new("/usr/bin/xcode-select")
        .arg("-p")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    match child.wait_timeout(DEVELOPER_DIRECTORY_PROBE_TIMEOUT) {
        Ok(Some(status)) if status.success() => {}
        Ok(Some(_)) => return false,
        _ => {
            let _ = child.kill();
            let _ = child.wait();
            return false;
        }
    }
    let Ok(output) = child.wait_with_output() else {
        return false;
    };
    let directory = String::from_utf8_lossy(&output.stdout);
    let directory = directory.trim();
    !directory.is_empty() && Path::new(directory).is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_resolves_to_exactly_the_platform_this_build_targets() {
        let resolved = host_platform();
        assert_eq!(resolved.is_windows(), cfg!(windows));
        assert_eq!(resolved.is_macos(), cfg!(target_os = "macos"));
        assert_eq!(resolved, HostPlatform::detect());
        // The tag has to stay the spelling the rest of the host already used.
        assert_eq!(resolved.os_tag(), std::env::consts::OS);
    }

    /// The resolution happens once: every later read is the same answer, and
    /// nothing in the process can move it.
    #[test]
    fn every_read_after_the_first_is_the_same_answer() {
        let first = resolve_at_startup();
        for _ in 0..4 {
            assert_eq!(host_platform(), first);
        }
    }

    /// The regression this guard exists for: macOS used to be whatever the
    /// `else` branch of a two-way `cfg!(windows)` said, which handed it Linux's
    /// answers. Each capability names macOS on its own.
    #[test]
    fn macos_answers_for_itself_rather_than_falling_through_to_linux() {
        use HostPlatform::{Linux, Macos, Windows};

        assert!(Macos.paths_are_case_insensitive());
        assert!(!Linux.paths_are_case_insensitive());
        assert!(Windows.paths_are_case_insensitive());

        assert_eq!(Macos.desktop_opener(), Some("open"));
        assert_eq!(Linux.desktop_opener(), Some("xdg-open"));
        assert_eq!(Windows.desktop_opener(), None);

        assert_eq!(Macos.os_tag(), "macos");
        assert_eq!(Linux.os_tag(), "linux");
        assert_eq!(Windows.os_tag(), "windows");

        assert_eq!(Macos.npm_platform_tag(), "darwin");
        assert_eq!(Linux.npm_platform_tag(), "linux");
        assert_eq!(Windows.npm_platform_tag(), "win32");

        assert_eq!(Macos.display_name(), "macOS");
        assert_eq!(Macos.executable_suffix(), "");
        assert_eq!(Windows.executable_suffix(), ".exe");
        assert!(!Macos.has_wsl());
    }

    #[test]
    fn path_keys_fold_case_only_where_the_filesystem_does() {
        use HostPlatform::{Linux, Macos, Windows};

        assert_eq!(Windows.path_key("C:\\Work\\App"), "c:/work/app");
        assert_eq!(Macos.path_key("/Users/dev/Code"), "/users/dev/code");
        assert_eq!(Linux.path_key("/home/dev/Code"), "/home/dev/Code");
    }

    /// Only the `/usr/bin` stand-ins are passed over; the same tool installed
    /// anywhere else (Homebrew, a version manager) is always the tool.
    #[test]
    fn only_the_usr_bin_stand_ins_are_developer_tool_shims() {
        for name in ["git", "python3", "pip3", "make", "clang", "cc", "lldb", "xcrun"] {
            assert!(
                names_developer_tool_shim(&Path::new("/usr/bin").join(name)),
                "{name}"
            );
        }
        assert!(names_developer_tool_shim(Path::new("/usr/bin/Git")));
        assert!(!names_developer_tool_shim(Path::new("/opt/homebrew/bin/git")));
        assert!(!names_developer_tool_shim(Path::new("/usr/local/bin/python3")));
        assert!(!names_developer_tool_shim(Path::new("/usr/bin/xcode-select")));
        assert!(!names_developer_tool_shim(Path::new("/usr/bin/ssh")));
        assert!(!names_developer_tool_shim(Path::new("/bin/zsh")));
        if !host_platform().is_macos() {
            assert!(!is_uninstalled_developer_tool_shim(Path::new("/usr/bin/git")));
        }
    }
}
