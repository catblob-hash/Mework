//! The macOS command-line-tools stand-ins, which must never be run.
//!
//! Lives here rather than in the host's `host_platform` because Git discovery
//! needs it on every machine Git runs on — the host and, through the remote
//! agent, a Mac reached over SSH — and this crate is what both of them link.
//! The host re-exports it.

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::OnceLock,
    time::Duration,
};

use wait_timeout::ChildExt;

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
    cfg!(target_os = "macos") && is_developer_tool_shim_path(path) && !developer_tools_installed()
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
/// per process.
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
        if !cfg!(target_os = "macos") {
            assert!(!is_uninstalled_developer_tool_shim(Path::new("/usr/bin/git")));
        }
    }
}
