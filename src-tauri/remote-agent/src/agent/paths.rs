//! Where the agent keeps its files on the machine.
//!
//! Everything lives under one directory in the account's home,
//! `~/.mework/remote` (`%USERPROFILE%\.mework\remote` on Windows):
//!
//! ```text
//! bin/<tag>/mework-remote     the uploaded executable, one directory per build
//! run/<tag>.sock              the daemon's socket (Unix)
//! run/<tag>.token             the secret a proxy must present (Unix)
//! run/<tag>.json              port and secret of the daemon's socket (Windows)
//! run/<tag>.lock              held by the running daemon for its whole life
//! run/<tag>.start             held by a proxy while it starts a daemon
//! agent.log                   the daemons' log, trimmed at start
//! ```
//!
//! `<tag>` is the version and the executable's own digest, so two builds never
//! share a daemon: a host that uploads a new agent starts a new daemon beside
//! the old one, which exits once nothing uses it.
//!
//! A Unix socket's path is limited to about a hundred bytes, which a long home
//! directory can exceed. The runtime directory then moves to
//! `$XDG_RUNTIME_DIR/mework-remote` or `/tmp/mework-remote-<uid>`, each only
//! accepted when it is a real directory owned by this account and closed to
//! everyone else.

use std::path::{Path, PathBuf};

/// Overrides the root directory, for tests and for machines whose home is not
/// writable. Unset in normal operation.
pub const ROOT_ENV: &str = "MEWORK_REMOTE_ROOT";

/// Longest socket path accepted. `sun_path` holds 104 bytes on macOS and the
/// BSDs and 108 on Linux, NUL included.
#[cfg(unix)]
const MAX_SOCKET_PATH: usize = 100;

#[derive(Clone, Debug)]
pub struct Paths {
    pub home: PathBuf,
    pub root: PathBuf,
    pub run_dir: PathBuf,
    pub tag: String,
}

impl Paths {
    pub fn discover(tag: &str) -> Result<Self, String> {
        Self::discover_at(tag, None)
    }

    /// [`Self::discover`] with the root given rather than looked up: a daemon
    /// is told its proxy's root, since a daemon WMI started does not inherit
    /// the proxy's environment.
    pub fn discover_at(tag: &str, root: Option<PathBuf>) -> Result<Self, String> {
        let home = home_dir().ok_or("The account on this machine has no home directory")?;
        let root = match root.or_else(|| std::env::var_os(ROOT_ENV).filter(|value| !value.is_empty()).map(PathBuf::from)) {
            Some(root) => root,
            None => home.join(".mework").join("remote"),
        };
        create_private_dir(&root)?;
        let run_dir = choose_run_dir(&root, tag)?;
        Ok(Self {
            home,
            root,
            run_dir,
            tag: tag.to_owned(),
        })
    }

    #[cfg(unix)]
    pub fn socket(&self) -> PathBuf {
        self.run_dir.join(format!("{}.sock", self.tag))
    }

    /// Unix: the token file. Windows: the port and token file.
    pub fn endpoint_file(&self) -> PathBuf {
        if cfg!(windows) {
            self.run_dir.join(format!("{}.json", self.tag))
        } else {
            self.run_dir.join(format!("{}.token", self.tag))
        }
    }

    pub fn lifetime_lock(&self) -> PathBuf {
        self.run_dir.join(format!("{}.lock", self.tag))
    }

    pub fn startup_lock(&self) -> PathBuf {
        self.run_dir.join(format!("{}.start", self.tag))
    }

    pub fn log_file(&self) -> PathBuf {
        self.root.join("agent.log")
    }

    pub fn bin_root(&self) -> PathBuf {
        self.root.join("bin")
    }
}

pub fn home_dir() -> Option<PathBuf> {
    let variable = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(variable)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Expands a leading `~` against `home`, the only expansion the agent does:
/// everything else in a path is taken literally.
pub fn expand_home(path: &str, home: &Path) -> PathBuf {
    if path == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = path.strip_prefix("~/").or_else(|| {
        if cfg!(windows) {
            path.strip_prefix("~\\")
        } else {
            None
        }
    }) {
        return home.join(rest);
    }
    PathBuf::from(path)
}

/// A path the host sent, as this machine's own filesystem calls it: `~`
/// expanded, and on Windows the drive spellings of the POSIX layers a Windows
/// machine may have — `/c/Users` from Git Bash and MSYS2, `/cygdrive/c/Users`
/// from Cygwin — turned into the `C:/Users` they mean. A workspace chosen
/// through a Git Bash login shell was recorded in the first form, and a
/// Windows program handed it would look for `\c\Users` on the current drive.
pub fn native_path(path: &str, home: &Path) -> PathBuf {
    if cfg!(windows) {
        if let Some(drive) = posix_drive_path(path) {
            return PathBuf::from(drive);
        }
    }
    expand_home(path, home)
}

/// `/c/src` → `C:/src`, `/cygdrive/d` → `D:/`; anything else is not a drive
/// path.
pub fn posix_drive_path(path: &str) -> Option<String> {
    let rest = path
        .strip_prefix("/cygdrive/")
        .or_else(|| path.strip_prefix('/'))?;
    let mut chars = rest.chars();
    let letter = chars.next().filter(char::is_ascii_alphabetic)?;
    let after = chars.as_str();
    if !(after.is_empty() || after.starts_with('/')) {
        return None;
    }
    Some(format!(
        "{}:/{}",
        letter.to_ascii_uppercase(),
        after.trim_start_matches('/')
    ))
}

fn choose_run_dir(root: &Path, tag: &str) -> Result<PathBuf, String> {
    let preferred = root.join("run");
    #[cfg(unix)]
    {
        let fits = |dir: &Path| dir.join(format!("{tag}.sock")).as_os_str().len() <= MAX_SOCKET_PATH;
        let mut candidates = vec![preferred];
        if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
            candidates.push(PathBuf::from(runtime).join("mework-remote"));
        }
        let uid = unsafe { libc::getuid() };
        candidates.push(PathBuf::from(format!("/tmp/mework-remote-{uid}")));
        let mut last_error = String::from("no candidate directory");
        for candidate in candidates {
            if !fits(&candidate) {
                last_error = format!("{} makes the socket path too long", candidate.display());
                continue;
            }
            match create_private_dir(&candidate) {
                Ok(()) => return Ok(candidate),
                Err(error) => last_error = error,
            }
        }
        Err(format!("No usable runtime directory for the agent: {last_error}"))
    }
    #[cfg(windows)]
    {
        let _ = tag;
        create_private_dir(&preferred)?;
        Ok(preferred)
    }
}

/// Creates `dir` if needed and insists it is a directory only this account
/// can enter. A directory someone else owns, or a symlink where the directory
/// should be, is refused rather than repaired: either could hand another
/// account the socket.
pub fn create_private_dir(dir: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("Cannot create {}: {error}", parent.display()))?;
        }
        match std::fs::DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(format!("Cannot create {}: {error}", dir.display())),
        }
        let metadata = std::fs::symlink_metadata(dir)
            .map_err(|error| format!("Cannot inspect {}: {error}", dir.display()))?;
        if !metadata.is_dir() {
            return Err(format!("{} is not a directory", dir.display()));
        }
        let uid = unsafe { libc::getuid() };
        if metadata.uid() != uid {
            return Err(format!("{} belongs to another account", dir.display()));
        }
        if metadata.mode() & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|error| format!("Cannot restrict {}: {error}", dir.display()))?;
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        // A directory under the profile inherits the profile's ACL, which
        // already closes it to other accounts.
        std::fs::create_dir_all(dir)
            .map_err(|error| format!("Cannot create {}: {error}", dir.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_leading_tilde_is_expanded() {
        let home = Path::new("/home/ada");
        assert_eq!(expand_home("~", home), PathBuf::from("/home/ada"));
        assert_eq!(expand_home("~/src/app", home), PathBuf::from("/home/ada/src/app"));
        assert_eq!(expand_home("/srv/~/x", home), PathBuf::from("/srv/~/x"));
        assert_eq!(expand_home("~ada/x", home), PathBuf::from("~ada/x"));
    }

    #[test]
    fn a_posix_drive_spelling_names_the_windows_drive_path() {
        assert_eq!(posix_drive_path("/c/Users/ada").as_deref(), Some("C:/Users/ada"));
        assert_eq!(posix_drive_path("/d").as_deref(), Some("D:/"));
        assert_eq!(posix_drive_path("/c/").as_deref(), Some("C:/"));
        assert_eq!(posix_drive_path("/cygdrive/e/src").as_deref(), Some("E:/src"));
        assert_eq!(posix_drive_path("/usr/bin"), None);
        assert_eq!(posix_drive_path("/tmp"), None);
        assert_eq!(posix_drive_path("C:/Users"), None);
        assert_eq!(posix_drive_path("/"), None);
        assert_eq!(posix_drive_path("/1/x"), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_private_dir_is_created_closed_and_a_symlink_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path().join("run");
        create_private_dir(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        create_private_dir(&dir).unwrap();
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        let link = scratch.path().join("link");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        assert!(create_private_dir(&link).is_err());
    }
}
