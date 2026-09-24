//! Linux and WSL 2: the cell runs under bubblewrap, the unprivileged sandbox
//! Flatpak is built on, in new user, PID, IPC, UTS, cgroup and network
//! namespaces with every capability dropped.
//!
//! * **Files**: the root file system is mounted read-only; the workspaces and
//!   the cell's own directories are bound writable over it; credential
//!   directories are covered by empty file systems; protected paths inside
//!   the workspaces are bound read-only again, and the directories above them
//!   are pinned (made mount points), so they can be neither renamed away nor
//!   replaced.
//! * **Processes**: the cell sees only its own processes.
//! * **Network**: the new namespace has a loopback interface and nothing else.
//!   The cell listens on it and hands each connection to the agent outside
//!   over its standard input and output (see `agent::tunnel`), which applies
//!   the network policy. Abstract Unix sockets — D-Bus, X11 — belong to the
//!   namespace too, so they are out of reach as well.
//! * **System calls**: the cell installs a seccomp filter on itself before it
//!   runs anything (see [`super::seccomp`]).
//!
//! bubblewrap is the machine's own `/usr/bin/bwrap`: distributions that
//! restrict unprivileged user namespaces (Ubuntu 24.04 and later) let the
//! packaged binary through their AppArmor policy and nothing else, so a copy
//! shipped with the agent would not run there.

use std::path::{Path, PathBuf};

use super::rules::{self, Rules};

/// Where the cell's proxy listens inside the sandbox's own network namespace.
pub const CELL_PROXY_PORT: u16 = 3128;

/// Paths the parent created so that a missing protected path could be bound
/// read-only; removed again, if still empty, when the cell is gone.
#[derive(Clone, Debug, Default)]
pub struct Placeholders {
    /// Each with what it was made holding.
    pub files: Vec<(PathBuf, String)>,
    pub directories: Vec<PathBuf>,
}

impl Placeholders {
    /// Makes `path` if it is missing, and records it. Whether it was made.
    pub fn make(&mut self, path: &Path, directory: bool, contents: &str) -> bool {
        if std::fs::symlink_metadata(path).is_ok() {
            return false;
        }
        if directory {
            if std::fs::create_dir(path).is_ok() {
                self.directories.push(path.to_path_buf());
                return true;
            }
        } else if std::fs::write(path, contents).is_ok() {
            self.files.push((path.to_path_buf(), contents.to_owned()));
            return true;
        }
        false
    }

    /// Removes what is still as it was made: a file with its contents
    /// unchanged, an empty directory. Something the user put there meanwhile
    /// stays.
    pub fn remove(&self) {
        for (file, contents) in &self.files {
            if std::fs::read(file).is_ok_and(|now| now == contents.as_bytes()) {
                let _ = std::fs::remove_file(file);
            }
        }
        for directory in self.directories.iter().rev() {
            // `remove_dir` refuses a directory that is not empty.
            let _ = std::fs::remove_dir(directory);
        }
    }
}

/// The bubblewrap invocation that runs `program` in the sandbox `rules`
/// describe, and the placeholders it needed.
pub fn argv(bwrap: &Path, rules: &Rules, own: &[PathBuf], program: &[String]) -> (Vec<String>, Placeholders) {
    let mut placeholders = Placeholders::default();
    let mut args: Vec<String> = vec![bwrap.to_string_lossy().into_owned()];
    let mut push = |parts: &[&str]| args.extend(parts.iter().map(|part| (*part).to_owned()));
    push(&[
        "--new-session",
        "--die-with-parent",
        "--unshare-user",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--unshare-cgroup-try",
        "--unshare-net",
        "--ro-bind",
        "/",
        "/",
        "--dev",
        "/dev",
        "--proc",
        "/proc",
        // A private /tmp: the machine's is shared with every other program and
        // account, and holds their sockets and lock files.
        "--tmpfs",
        "/tmp",
    ]);
    let mut args = args;
    let text = |path: &Path| path.to_string_lossy().into_owned();

    // The account's runtime directory holds its session bus, keyring and
    // agent sockets.
    if let Some(runtime) = runtime_dir() {
        args.extend(["--tmpfs".into(), text(&runtime)]);
    }

    for path in &rules.writable {
        args.extend(["--bind".into(), text(path), text(path)]);
    }

    for path in &rules.deny_read {
        match std::fs::metadata(path) {
            Ok(metadata) if metadata.is_dir() => args.extend(["--tmpfs".into(), text(path)]),
            Ok(_) => args.extend(["--ro-bind".into(), "/dev/null".into(), text(path)]),
            Err(_) => {}
        }
    }
    // What a denied directory must still show: bound back from the real one.
    for path in &rules.readable {
        if path.exists() && rules.deny_read.iter().any(|denied| path.starts_with(denied)) {
            args.extend(["--ro-bind".into(), text(path), text(path)]);
        }
    }
    for path in &rules.writable {
        if rules.deny_read.iter().any(|denied| path.starts_with(denied) && path != denied) {
            args.extend(["--bind".into(), text(path), text(path)]);
        }
    }

    // Protected paths inside writable directories. Outside them everything is
    // read-only already.
    let inside_writable =
        |path: &Path| rules.writable.iter().any(|root| path.starts_with(root)) && !own.iter().any(|own| path.starts_with(own));
    let workspace_roots: Vec<PathBuf> = rules
        .writable
        .iter()
        .filter(|root| !own.contains(root))
        .cloned()
        .collect();
    let mut protected: Vec<PathBuf> = rules
        .deny_write
        .iter()
        .filter(|path| inside_writable(path) && std::fs::symlink_metadata(path).is_ok())
        .cloned()
        .collect();
    protected.extend(rules::find_protected(&workspace_roots, &rules.protected_names, 3));
    // What does not exist cannot be bound read-only, so the names that would
    // run outside the sandbox are made to exist at each workspace's root, and
    // inside each `.git` there is.
    for root in &workspace_roots {
        for (name, directory, contents) in rules::ROOT_PLACEHOLDERS {
            let path = root.join(name);
            if placeholders.make(&path, *directory, contents) {
                protected.push(path);
            }
        }
    }
    let git_dirs: Vec<PathBuf> = workspace_roots
        .iter()
        .map(|root| root.join(".git"))
        .chain(protected.iter().filter_map(|path| {
            path.ancestors()
                .find(|ancestor| ancestor.file_name().is_some_and(|name| name == ".git"))
                .map(Path::to_path_buf)
        }))
        .filter(|git| git.is_dir())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    for git in git_dirs {
        for (name, directory, contents) in rules::GIT_PLACEHOLDERS {
            let path = git.join(name);
            if placeholders.make(&path, *directory, contents) {
                protected.push(path);
            }
        }
    }
    for root in &rules.bare_roots {
        if own.contains(root) {
            continue;
        }
        // Not in a repository: neither `.git` nor a bare repository's `HEAD`
        // may appear.
        let git = root.join(".git");
        if placeholders.make(&git, true, "") {
            protected.push(git);
        }
        for marker in rules::BARE_REPOSITORY_MARKERS {
            let path = root.join(marker);
            if placeholders.make(&path, false, "") {
                protected.push(path);
            }
        }
    }
    // A `.git` directory is pinned as a whole, so its protected contents
    // cannot be moved out with it.
    let mut pins: Vec<PathBuf> = Vec::new();
    for path in &protected {
        for ancestor in path.ancestors().skip(1) {
            if workspace_roots.iter().any(|root| ancestor == root) || !inside_writable(ancestor) {
                break;
            }
            pins.push(ancestor.to_path_buf());
        }
    }
    pins.sort();
    pins.dedup();
    protected.sort();
    protected.dedup();
    // Pins first, shallowest first: binding a directory over itself keeps
    // what is already mounted below it only if the lower mounts come after.
    pins.sort_by_key(|path| path.components().count());
    for pin in &pins {
        if !protected.contains(pin) {
            args.extend(["--bind".into(), text(pin), text(pin)]);
        }
    }
    for path in &protected {
        args.extend(["--ro-bind".into(), text(path), text(path)]);
    }

    let start = workspace_roots.first().cloned().unwrap_or_else(|| PathBuf::from("/"));
    args.extend(["--cap-drop".into(), "ALL".into(), "--chdir".into(), text(&start), "--".into()]);
    args.extend(program.iter().cloned());
    (args, placeholders)
}

fn runtime_dir() -> Option<PathBuf> {
    let from_env = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    #[cfg(unix)]
    let by_uid = Some(PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })));
    #[cfg(not(unix))]
    let by_uid: Option<PathBuf> = None;
    from_env
        .into_iter()
        .chain(by_uid)
        .find(|path| path.is_absolute() && path.is_dir())
}

/// Finds the machine's bubblewrap.
pub fn find() -> Option<PathBuf> {
    ["/usr/bin/bwrap", "/usr/local/bin/bwrap", "/bin/bwrap"]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
}

/// Checks that bubblewrap can create the namespaces a cell needs here: it may
/// be missing, the kernel may not allow unprivileged user namespaces, or this
/// may be WSL 1, which has no namespaces at all.
pub fn probe() -> Result<PathBuf, String> {
    if let Ok(version) = std::fs::read_to_string("/proc/version") {
        let lower = version.to_ascii_lowercase();
        if lower.contains("microsoft") && !lower.contains("wsl2") {
            return Err("WSL 1 has no Linux namespaces; convert the distribution to WSL 2 (wsl --set-version <distro> 2) to use the sandbox".into());
        }
    }
    if !cfg!(any(target_arch = "x86_64", target_arch = "aarch64")) {
        return Err("The sandbox needs a 64-bit x86 or Arm machine".into());
    }
    let Some(bwrap) = find() else {
        return Err("bubblewrap is not installed; install it with the system's package manager (apt install bubblewrap, dnf install bubblewrap, pacman -S bubblewrap)".into());
    };
    let output = std::process::Command::new(&bwrap)
        .args([
            "--new-session",
            "--die-with-parent",
            "--unshare-user",
            "--unshare-pid",
            "--unshare-net",
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
            "--cap-drop",
            "ALL",
            "--",
            "/bin/sh",
            "-c",
            "exit 0",
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| format!("Cannot run {}: {error}", bwrap.display()))?;
    if output.status.success() {
        return Ok(bwrap);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let hint = if std::fs::read_to_string("/proc/sys/kernel/apparmor_restrict_unprivileged_userns")
        .is_ok_and(|value| value.trim() == "1")
    {
        " AppArmor restricts unprivileged user namespaces on this machine; install the distribution's bubblewrap package (whose profile allows them) or allow them for bwrap"
    } else if std::fs::read_to_string("/proc/sys/kernel/unprivileged_userns_clone").is_ok_and(|value| value.trim() == "0") {
        " Unprivileged user namespaces are disabled (sysctl kernel.unprivileged_userns_clone=1 enables them)"
    } else {
        ""
    };
    Err(format!("bubblewrap cannot create a sandbox here: {stderr}.{hint}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspaces_are_writable_credentials_hidden_and_protected_paths_pinned() {
        let base = tempfile::tempdir().unwrap();
        let base = rules::canonical(base.path());
        let workspace = base.join("ws");
        std::fs::create_dir_all(rules::under(&workspace, ".git/hooks")).unwrap();
        std::fs::write(rules::under(&workspace, ".git/config"), "").unwrap();
        let secret = base.join("secret");
        std::fs::create_dir_all(&secret).unwrap();
        let tmp = base.join("cell");
        std::fs::create_dir_all(&tmp).unwrap();
        let rules = Rules {
            writable: vec![tmp.clone(), workspace.clone()],
            deny_read: vec![secret.clone()],
            readable: Vec::new(),
            deny_write: Vec::new(),
            protected_names: rules::PROTECTED_NAMES.to_vec(),
            bare_roots: Vec::new(),
        };
        let (argv, placeholders) = argv(Path::new("/usr/bin/bwrap"), &rules, &[tmp.clone()], &["agent".into()]);
        let joined = argv.join(" ");
        let at = |needle: &str| joined.find(needle).unwrap_or_else(|| panic!("{needle} missing in {joined}"));
        assert!(joined.contains("--unshare-net"));
        assert!(joined.contains(&format!("--bind {0} {0}", workspace.display())));
        assert!(joined.contains(&format!("--tmpfs {}", secret.display())));
        // The `.git` directory is pinned before its hooks and config are
        // made read-only.
        let pin = at(&format!("--bind {0} {0}", workspace.join(".git").display()));
        let hooks = at(&format!("--ro-bind {0} {0}", rules::under(&workspace, ".git/hooks").display()));
        let config = at(&format!("--ro-bind {0} {0}", rules::under(&workspace, ".git/config").display()));
        assert!(pin < hooks && pin < config);
        // A project without `.mework` gets an empty, read-only one, which is
        // removed again afterwards.
        assert!(joined.contains(&format!("--ro-bind {0} {0}", workspace.join(".mework").display())));
        assert!(placeholders.directories.contains(&workspace.join(".mework")));
        // So is every other name that would run outside, file or directory,
        // and what a repository's `.git` could be pointed elsewhere with.
        for name in [".envrc", ".mcp.json", ".vscode", ".git/commondir", ".git/config.worktree"] {
            assert!(joined.contains(&format!("--ro-bind {0} {0}", rules::under(&workspace, name).display())), "{name}");
        }
        assert!(argv.ends_with(&["--".into(), "agent".into()]));
        // The user's own file, written meanwhile, stays.
        std::fs::write(workspace.join(".envrc"), "export MINE=1").unwrap();
        placeholders.remove();
        assert!(!workspace.join(".mework").exists());
        assert!(!workspace.join(".mcp.json").exists());
        assert!(!rules::under(&workspace, ".git/commondir").exists());
        assert_eq!(std::fs::read_to_string(workspace.join(".envrc")).unwrap(), "export MINE=1");
    }

    #[test]
    fn a_directory_outside_any_repository_gets_placeholders_for_git() {
        let base = tempfile::tempdir().unwrap();
        let base = rules::canonical(base.path());
        let workspace = base.join("scratch");
        std::fs::create_dir_all(&workspace).unwrap();
        let rules = Rules {
            writable: vec![workspace.clone()],
            bare_roots: vec![workspace.clone()],
            protected_names: rules::PROTECTED_NAMES.to_vec(),
            ..Rules::default()
        };
        let (argv, placeholders) = argv(Path::new("/usr/bin/bwrap"), &rules, &[], &["agent".into()]);
        let joined = argv.join(" ");
        assert!(joined.contains(&format!("--ro-bind {0} {0}", workspace.join(".git").display())));
        assert!(joined.contains(&format!("--ro-bind {0} {0}", workspace.join("HEAD").display())));
        assert!(placeholders.files.iter().any(|(path, _)| *path == workspace.join("HEAD")));
        placeholders.remove();
        assert!(!workspace.join("HEAD").exists() && !workspace.join(".git").exists());
    }
}
