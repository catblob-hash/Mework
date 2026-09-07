//! Reveal a local filesystem path in the operating system's file manager.
//!
//! Model output reaches this boundary, so a path is validated and proven to
//! exist before it is handed to the shell. Files are *selected* in their
//! containing folder rather than opened, so a path that names an executable
//! cannot start a process.

use std::path::{Component, Path, PathBuf, Prefix};

/// Maximum path length accepted from the renderer.
const MAX_REVEAL_PATH_LENGTH: usize = 4096;

/// Validate a path, resolve it against a base directory, and prove it exists.
///
/// Callers must pass the returned path to the system rather than the raw input:
/// canonicalization resolves `..` and symbolic links, and the verbatim prefix it
/// produces on Windows is stripped here.
pub fn resolve_reveal_path(raw: &str, base: Option<&str>) -> Result<PathBuf, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("文件路径为空".to_owned());
    }
    if trimmed.len() > MAX_REVEAL_PATH_LENGTH {
        return Err(format!(
            "文件路径超过 {MAX_REVEAL_PATH_LENGTH} 字节上限，拒绝交给系统打开"
        ));
    }
    if trimmed.chars().any(char::is_control) {
        return Err("文件路径包含控制字符，拒绝交给系统打开".to_owned());
    }
    // A network or device path must never reach the shell: revealing
    // `\\host\share\x` makes Windows authenticate against `host`.
    if is_unc_or_device(trimmed) {
        return Err("拒绝打开网络或设备路径".to_owned());
    }

    let joined = if is_absolute(trimmed) {
        PathBuf::from(trimmed)
    } else {
        // A leading separator is drive-relative on Windows and would silently
        // escape the base directory instead of resolving under it.
        if trimmed.starts_with('\\') {
            return Err("拒绝打开驱动器相对路径".to_owned());
        }
        let base = base
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "相对路径缺少工作目录".to_owned())?;
        if base.chars().any(char::is_control) {
            return Err("工作目录包含控制字符，拒绝交给系统打开".to_owned());
        }
        plain_base(base)?.join(trimmed)
    };

    let canonical = std::fs::canonicalize(&joined)
        .map_err(|error| format!("路径不存在或无法访问: {error}"))?;
    plain_path(canonical)
}

/// Reduce a working directory to the plain local form it must have.
///
/// The renderer takes this value from the conversation's resolved working
/// directory, which the host canonicalized and therefore hands over in `\\?\`
/// verbatim form. Rejecting that outright would leave every relative path
/// unopenable, so it is reduced exactly the way results are.
fn plain_base(base: &str) -> Result<PathBuf, String> {
    let reduced =
        plain_path(PathBuf::from(base)).map_err(|_| "工作目录不是本机绝对路径".to_owned())?;
    if is_unc_or_device(&reduced.to_string_lossy()) || !is_absolute(&reduced.to_string_lossy()) {
        return Err("工作目录不是本机绝对路径".to_owned());
    }
    Ok(reduced)
}

/// Whether the string names a UNC share or a device namespace.
///
/// This also covers the `\\?\` and `\\.\` prefixes.
fn is_unc_or_device(value: &str) -> bool {
    value.starts_with("\\\\") || value.starts_with("//")
}

/// Whether the string is an absolute path in either platform's syntax.
///
/// `Path::is_absolute` answers only for the host platform, so drive-letter and
/// POSIX forms are recognized explicitly and validation behaves the same
/// wherever the tests run.
fn is_absolute(value: &str) -> bool {
    if value.starts_with('/') {
        return true;
    }
    let mut characters = value.chars();
    matches!(
        (characters.next(), characters.next(), characters.next()),
        (Some(letter), Some(':'), Some('\\' | '/')) if letter.is_ascii_alphabetic()
    )
}

/// Convert a canonicalized path into the plain form the shell accepts.
///
/// `std::fs::canonicalize` returns `\\?\C:\…` on Windows, which `explorer.exe`
/// rejects. Components are rebuilt rather than the string edited so that paths
/// which are not valid UTF-8 survive unchanged.
fn plain_path(path: PathBuf) -> Result<PathBuf, String> {
    let Some(Component::Prefix(prefix)) = path.components().next() else {
        return Ok(path);
    };
    match prefix.kind() {
        Prefix::Disk(_) => Ok(path),
        Prefix::VerbatimDisk(letter) => {
            let mut rebuilt = PathBuf::from(format!("{}:\\", char::from(letter)));
            rebuilt.extend(
                path.components()
                    .skip(1)
                    .filter(|component| !matches!(component, Component::RootDir)),
            );
            Ok(rebuilt)
        }
        // A junction or symbolic link can resolve onto a share or device even
        // when the input looked local, so the check is repeated after
        // canonicalization rather than only before it.
        _ => Err("拒绝打开网络或设备路径".to_owned()),
    }
}

/// Show a validated path in the file manager.
///
/// Success means the operating system accepted the launch request, not that a
/// window appeared.
#[cfg(windows)]
pub fn reveal(path: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    // `/select,` and the path must form a single argument; passing them
    // separately makes Explorer ignore both and open the default folder.
    // A directory is opened directly, because selecting it would show its
    // parent instead of the place the user clicked.
    let argument = if path.is_dir() {
        path.as_os_str().to_owned()
    } else {
        let mut argument = std::ffi::OsString::from("/select,");
        argument.push(path.as_os_str());
        argument
    };

    // explorer.exe returns nonzero when the window already exists; only
    // process-start success matters.
    std::process::Command::new("explorer.exe")
        .arg(argument)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("无法打开文件位置: {error}"))
}

/// Show a validated path in the file manager.
#[cfg(not(windows))]
pub fn reveal(path: &Path) -> Result<(), String> {
    let mut command = if cfg!(target_os = "macos") {
        let mut command = std::process::Command::new("open");
        command.arg("-R").arg(path);
        command
    } else {
        // No portable select verb exists, so open the containing folder.
        let target = if path.is_dir() {
            path
        } else {
            path.parent().unwrap_or(path)
        };
        let mut command = std::process::Command::new("xdg-open");
        command.arg(target);
        command
    };
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("无法打开文件位置: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An existing directory every platform provides, without creating fixtures.
    ///
    /// Canonicalization yields a verbatim path on Windows, which is exactly what
    /// the validator refuses as input, so the helper reduces it the same way
    /// `resolve_reveal_path` reduces its own result.
    fn existing_directory() -> PathBuf {
        let canonical =
            std::fs::canonicalize(std::env::temp_dir()).expect("the temporary directory exists");
        plain_path(canonical).expect("the temporary directory is local")
    }

    #[test]
    fn rejects_empty_and_oversized_input() {
        assert!(resolve_reveal_path("", None).is_err());
        assert!(resolve_reveal_path("   ", None).is_err());
        let long = "a".repeat(MAX_REVEAL_PATH_LENGTH + 1);
        assert!(resolve_reveal_path(&long, None).is_err());
    }

    #[test]
    fn rejects_control_characters() {
        for candidate in ["/tmp/\u{0}x", "/tmp/\nx", "/tmp/\tx"] {
            assert!(
                resolve_reveal_path(candidate, None).is_err(),
                "{candidate:?} must not reach the system"
            );
        }
        assert!(resolve_reveal_path("x", Some("/tmp/\u{0}base")).is_err());
    }

    #[test]
    fn rejects_network_and_device_paths() {
        for candidate in [
            r"\\server\share\file.txt",
            r"\\?\C:\Windows",
            r"\\.\PIPE\mework",
            "//server/share/file.txt",
        ] {
            assert!(
                resolve_reveal_path(candidate, None).is_err(),
                "{candidate} must not reach the system"
            );
        }
    }

    #[test]
    fn rejects_paths_that_do_not_exist() {
        let base = existing_directory();
        assert!(resolve_reveal_path(
            "mework-reveal-path-does-not-exist/nested",
            base.to_str()
        )
        .is_err());
    }

    #[test]
    fn rejects_relative_input_without_a_usable_base() {
        assert!(resolve_reveal_path("src/lib.rs", None).is_err());
        assert!(resolve_reveal_path("src/lib.rs", Some("   ")).is_err());
        assert!(resolve_reveal_path("src/lib.rs", Some("relative/base")).is_err());
        assert!(resolve_reveal_path("src/lib.rs", Some(r"\\server\share")).is_err());
        // Drive-relative input would escape the base directory.
        let base = existing_directory();
        assert!(resolve_reveal_path(r"\Windows", base.to_str()).is_err());
    }

    #[test]
    fn accepts_an_existing_absolute_path() {
        let directory = existing_directory();
        let resolved = resolve_reveal_path(
            directory.to_str().expect("temporary path is UTF-8"),
            None,
        )
        .expect("the temporary directory is revealable");
        assert_eq!(resolved, directory);
    }

    #[test]
    fn resolves_a_relative_path_against_the_base_directory() {
        let directory = existing_directory();
        let base = directory.parent().expect("the temporary directory has a parent");
        let name = directory
            .file_name()
            .expect("the temporary directory has a name")
            .to_str()
            .expect("temporary path is UTF-8");
        let resolved = resolve_reveal_path(name, base.to_str())
            .expect("a name under an existing base resolves");
        assert_eq!(resolved, directory);
    }

    /// The renderer reads the conversation's working directory from a value the
    /// host canonicalized, so on Windows it arrives in `\\?\` form. Rejecting
    /// that would leave every relative path unopenable.
    #[test]
    fn resolves_against_a_canonicalized_verbatim_base() {
        let directory = existing_directory();
        let base = directory.parent().expect("the temporary directory has a parent");
        let name = directory
            .file_name()
            .expect("the temporary directory has a name")
            .to_str()
            .expect("temporary path is UTF-8");
        let verbatim = if cfg!(windows) {
            format!("\\\\?\\{}", base.display())
        } else {
            base.display().to_string()
        };
        let resolved = resolve_reveal_path(name, Some(&verbatim))
            .expect("a verbatim working directory still resolves");
        assert_eq!(resolved, directory);
    }

    /// `explorer.exe` does not accept the verbatim prefix that canonicalization
    /// produces, so the resolved path must never carry it.
    #[test]
    fn resolved_paths_carry_no_verbatim_prefix() {
        let directory = existing_directory();
        let resolved =
            resolve_reveal_path(directory.to_str().expect("temporary path is UTF-8"), None)
                .expect("the temporary directory is revealable");
        assert!(!resolved.to_string_lossy().starts_with(r"\\?\"));
        assert!(!is_unc_or_device(&resolved.to_string_lossy()));
    }

    #[test]
    fn recognizes_both_absolute_syntaxes() {
        for candidate in ["/usr/bin", r"C:\Windows", "D:/projects", "/"] {
            assert!(is_absolute(candidate), "{candidate} is absolute");
        }
        for candidate in ["src/lib.rs", "C:", "CD:\\x", "1:\\x", r".\x", "x"] {
            assert!(!is_absolute(candidate), "{candidate} is not absolute");
        }
    }
}
