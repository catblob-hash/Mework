use std::{
    fs::{self, File},
    io,
    path::{Component, Path, PathBuf},
};

/// Filesystem boundary applied by a prepared tool execution.
///
/// Restricted roots are canonicalized again immediately before use. This keeps
/// path checks authoritative even when callers constructed the scope from
/// persisted strings rather than already-canonical paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecutionScope {
    Restricted {
        roots: Vec<PathBuf>,
    },
    RestrictedExcept {
        roots: Vec<PathBuf>,
        denied_roots: Vec<PathBuf>,
    },
    Unrestricted,
    UnrestrictedExcept {
        denied_roots: Vec<PathBuf>,
    },
}

impl ExecutionScope {
    pub fn restricted(roots: impl IntoIterator<Item = PathBuf>) -> Self {
        Self::Restricted {
            roots: roots.into_iter().collect(),
        }
    }

    pub fn workspace_only(workspace: &Path) -> Self {
        Self::restricted([workspace.to_path_buf()])
    }

    /// Adds host-owned paths that model-visible filesystem tools must never
    /// access, even when the conversation otherwise has unrestricted access.
    ///
    /// Denied roots are resolved again at the point of use so symlinks,
    /// junctions, case aliases, and other canonical path aliases cannot turn a
    /// stale classification decision into an access grant.
    pub fn denying(self, roots: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut additional = roots.into_iter().collect::<Vec<_>>();
        match self {
            Self::Restricted { roots } => Self::RestrictedExcept {
                roots,
                denied_roots: deduplicate_boundary_roots(additional),
            },
            Self::RestrictedExcept {
                roots,
                mut denied_roots,
            } => {
                denied_roots.append(&mut additional);
                Self::RestrictedExcept {
                    roots,
                    denied_roots: deduplicate_boundary_roots(denied_roots),
                }
            }
            Self::Unrestricted => Self::UnrestrictedExcept {
                denied_roots: deduplicate_boundary_roots(additional),
            },
            Self::UnrestrictedExcept { mut denied_roots } => {
                denied_roots.append(&mut additional);
                Self::UnrestrictedExcept {
                    denied_roots: deduplicate_boundary_roots(denied_roots),
                }
            }
        }
    }
}

pub fn canonical_workspace(path: &Path) -> Result<PathBuf, String> {
    let canonical = fs::canonicalize(path)
        .map_err(|error| format!("Could not access workspace {}: {error}", path.display()))?;
    if !canonical.is_dir() {
        return Err(format!(
            "Workspace is not a directory: {}",
            canonical.display()
        ));
    }
    Ok(canonical)
}

pub fn resolve_existing(workspace: &Path, requested: &str) -> Result<PathBuf, String> {
    resolve_existing_with_scope(
        workspace,
        requested,
        &ExecutionScope::workspace_only(workspace),
    )
}

/// Resolves an existing target. Relative requests are always based on the
/// workspace, including under an unrestricted approval.
pub fn resolve_existing_with_scope(
    workspace: &Path,
    requested: &str,
    scope: &ExecutionScope,
) -> Result<PathBuf, String> {
    let workspace = canonical_workspace(workspace)?;
    let candidate = candidate_path(&workspace, requested)?;
    let canonical = fs::canonicalize(&candidate)
        .map_err(|error| format!("Could not access path {requested}: {error}"))?;
    ensure_allowed(scope, &canonical)?;
    Ok(canonical)
}

/// Resolves and opens an existing regular file while binding the scope check to
/// the opened handle. The final handle path must still be the canonical path
/// that was authorized, so swapping a parent directory for a symlink/junction
/// between resolution and `open` fails closed.
pub fn secure_open_existing_file_with_scope(
    workspace: &Path,
    requested: &str,
    scope: &ExecutionScope,
) -> Result<(File, PathBuf), String> {
    let canonical = resolve_existing_with_scope(workspace, requested, scope)?;
    let file = open_verified_scoped_file(&canonical, scope)?;
    Ok((file, canonical))
}

#[cfg(windows)]
fn open_verified_scoped_file(expected: &Path, scope: &ExecutionScope) -> Result<File, String> {
    use std::os::windows::{
        ffi::OsStringExt,
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        GetFinalPathNameByHandleW, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_SHARE_READ, FILE_SHARE_WRITE, VOLUME_NAME_DOS,
    };

    let file = fs::OpenOptions::new()
        .read(true)
        // Keep the pathname from being removed/replaced while this verified
        // handle is being consumed, but remain compatible with editors that
        // already hold a writable handle.
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        // Open a final reparse point itself instead of following it.
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(expected)
        .map_err(|error| {
            format!(
                "Could not securely open file {}: {error}",
                expected.display()
            )
        })?;
    let metadata = file.metadata().map_err(|error| {
        format!(
            "Could not inspect file handle {}: {error}",
            expected.display()
        )
    })?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err("read target must be a regular file without a reparse point".into());
    }

    let mut buffer = vec![0_u16; 512];
    let actual = loop {
        // SAFETY: the handle remains owned by `file`, and `buffer` is writable
        // for the exact length supplied to Win32.
        let length = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle().cast(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                VOLUME_NAME_DOS,
            )
        };
        if length == 0 {
            return Err(format!(
                "Could not verify file handle {}: {}",
                expected.display(),
                io::Error::last_os_error()
            ));
        }
        if (length as usize) < buffer.len() {
            break PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length as usize]));
        }
        buffer.resize(length as usize + 1, 0);
    };
    ensure_allowed(scope, &actual)?;
    if !same_path_identity(&actual, expected) {
        return Err(format!(
            "read path passed through a symlink, directory junction, or replacement while opening: {}",
            expected.display()
        ));
    }
    Ok(file)
}

#[cfg(unix)]
fn open_verified_scoped_file(expected: &Path, scope: &ExecutionScope) -> Result<File, String> {
    use std::os::unix::{fs::OpenOptionsExt, io::AsRawFd};

    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(expected)
        .map_err(|error| {
            format!(
                "Could not securely open file {}: {error}",
                expected.display()
            )
        })?;
    let metadata = file.metadata().map_err(|error| {
        format!(
            "Could not inspect file handle {}: {error}",
            expected.display()
        )
    })?;
    if !metadata.is_file() {
        return Err("read target must be a regular file without symlinks".into());
    }

    #[cfg(target_os = "linux")]
    let actual = fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).map_err(|error| {
        format!(
            "Could not verify file handle {}: {error}",
            expected.display()
        )
    })?;

    #[cfg(target_os = "macos")]
    let actual = {
        use std::{ffi::CStr, os::unix::ffi::OsStrExt};
        let mut buffer = vec![0_i8; libc::PATH_MAX as usize];
        // SAFETY: F_GETPATH writes a NUL-terminated path into the supplied
        // PATH_MAX-sized buffer while `file` keeps the descriptor live.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, buffer.as_mut_ptr()) } == -1 {
            return Err(format!(
                "Could not verify file handle {}: {}",
                expected.display(),
                io::Error::last_os_error()
            ));
        }
        let bytes = unsafe { CStr::from_ptr(buffer.as_ptr()) }.to_bytes();
        PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
    };

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let actual = fs::canonicalize(expected)
        .map_err(|error| format!("Could not verify file path {}: {error}", expected.display()))?;

    ensure_allowed(scope, &actual)?;
    if actual != expected {
        return Err(format!(
            "read path passed through a symlink or replacement while opening: {}",
            expected.display()
        ));
    }
    Ok(file)
}

#[cfg(windows)]
fn same_path_identity(left: &Path, right: &Path) -> bool {
    let normalize = |value: &Path| {
        value
            .to_string_lossy()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_ascii_lowercase()
    };
    normalize(left) == normalize(right)
}

/// Resolves an existing or new write target. For a new path the nearest
/// existing ancestor is canonicalized before the boundary check, preventing a
/// symlink or junction ancestor from escaping a restricted root.
pub fn resolve_for_write_with_scope(
    workspace: &Path,
    requested: &str,
    scope: &ExecutionScope,
) -> Result<PathBuf, String> {
    let workspace = canonical_workspace(workspace)?;
    let candidate = candidate_path(&workspace, requested)?;

    match fs::symlink_metadata(&candidate) {
        Ok(_) => {
            // `symlink_metadata` deliberately treats a broken symlink as an
            // existing entry. Canonicalization then fails closed instead of
            // returning the lexical alias and letting the eventual write
            // follow it to an unverified target.
            let canonical = fs::canonicalize(&candidate).map_err(|error| {
                format!(
                    "Could not access write target {}: {error}",
                    candidate.display()
                )
            })?;
            ensure_allowed(scope, &canonical)?;
            return Ok(canonical);
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "Could not inspect write target {}: {error}",
                candidate.display()
            ))
        }
    }

    let mut ancestor = candidate.parent();
    let existing_ancestor = loop {
        let Some(path) = ancestor else {
            return Err("Write target has no verifiable parent directory".into());
        };
        match fs::symlink_metadata(path) {
            Ok(_) => break path,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                ancestor = path.parent();
            }
            Err(error) => {
                return Err(format!(
                    "Could not inspect write-target parent path {}: {error}",
                    path.display()
                ))
            }
        }
    };
    let canonical_ancestor = fs::canonicalize(existing_ancestor).map_err(|error| {
        format!(
            "Could not verify write-target parent directory {}: {error}",
            existing_ancestor.display()
        )
    })?;
    let unresolved_suffix = candidate.strip_prefix(existing_ancestor).map_err(|_| {
        format!(
            "Could not resolve the relationship between write target {} and existing parent directory {}",
            candidate.display(),
            existing_ancestor.display()
        )
    })?;
    let prospective_target = canonical_ancestor.join(unresolved_suffix);
    ensure_allowed(scope, &prospective_target)?;
    Ok(prospective_target)
}

pub fn relative_display<'a>(workspace: &'a Path, path: &'a Path) -> &'a Path {
    path.strip_prefix(workspace).unwrap_or(path)
}

/// Checks a path yielded during recursive traversal against the same
/// canonical scope used for a direct request.
///
/// Recursive tools may start at an allowed ancestor such as `app_data` and
/// encounter a denied descendant later. They must call this for every entry
/// before displaying or opening it, and before descending into a directory.
pub fn existing_path_is_allowed(scope: &ExecutionScope, path: &Path) -> bool {
    fs::canonicalize(path)
        .map_err(|_| ())
        .and_then(|canonical| ensure_allowed(scope, &canonical).map_err(|_| ()))
        .is_ok()
}

fn candidate_path(workspace: &Path, requested: &str) -> Result<PathBuf, String> {
    let requested = requested.trim();
    if requested.is_empty() {
        return Err("Path must not be empty".into());
    }
    if requested.contains('\0') {
        return Err("Path contains an invalid character".into());
    }
    let requested = Path::new(requested);
    let joined = if requested.is_absolute() {
        requested.to_owned()
    } else {
        workspace.join(requested)
    };
    normalize_absolute(&joined)
        .map_err(|error| format!("Invalid path {}: {error}", joined.display()))
}

fn normalize_absolute(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Path must resolve to an absolute path",
        ));
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "Path escapes the filesystem root",
                    ));
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    Ok(normalized)
}

fn ensure_allowed(scope: &ExecutionScope, candidate: &Path) -> Result<(), String> {
    let denied_roots = match scope {
        ExecutionScope::RestrictedExcept { denied_roots, .. }
        | ExecutionScope::UnrestrictedExcept { denied_roots } => denied_roots.as_slice(),
        ExecutionScope::Restricted { .. } | ExecutionScope::Unrestricted => &[],
    };
    for root in denied_roots {
        let canonical = canonical_boundary_root(root)?;
        if path_is_within(candidate, &canonical) {
            return Err("Access to protected application-data path is denied".into());
        }
    }

    match scope {
        ExecutionScope::Unrestricted | ExecutionScope::UnrestrictedExcept { .. } => Ok(()),
        ExecutionScope::Restricted { roots } | ExecutionScope::RestrictedExcept { roots, .. } => {
            if roots.is_empty() {
                return Err("Restricted execution scope has no trusted root".into());
            }
            for root in roots {
                let canonical = canonical_workspace(root).map_err(|error| {
                    format!(
                        "Could not verify restricted execution root {}: {error}",
                        root.display()
                    )
                })?;
                if path_is_within(candidate, &canonical) {
                    return Ok(());
                }
            }
            Err(format!(
                "Access to a path outside the trusted roots is denied: {}",
                candidate.display()
            ))
        }
    }
}

fn canonical_boundary_root(path: &Path) -> Result<PathBuf, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            return fs::canonicalize(path).map_err(|error| {
                format!(
                    "Could not verify protected path {}: {error}",
                    path.display()
                )
            })
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "Could not inspect protected path {}: {error}",
                path.display()
            ))
        }
    }

    let mut ancestor = path.parent();
    let existing_ancestor = loop {
        let Some(candidate) = ancestor else {
            return Err(format!(
                "Protected path has no verifiable parent directory: {}",
                path.display()
            ));
        };
        match fs::symlink_metadata(candidate) {
            Ok(_) => break candidate,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                ancestor = candidate.parent();
            }
            Err(error) => {
                return Err(format!(
                    "Could not inspect protected path parent directory {}: {error}",
                    candidate.display()
                ))
            }
        }
    };
    let canonical_ancestor = fs::canonicalize(existing_ancestor).map_err(|error| {
        format!(
            "Could not verify protected path parent directory {}: {error}",
            existing_ancestor.display()
        )
    })?;
    let suffix = path.strip_prefix(existing_ancestor).map_err(|_| {
        format!(
            "Could not resolve the relationship between protected path and parent directory: {}",
            path.display()
        )
    })?;
    Ok(canonical_ancestor.join(suffix))
}

fn deduplicate_boundary_roots(roots: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut deduplicated = Vec::<(PathBuf, PathBuf)>::new();
    for root in roots {
        let identity = canonical_boundary_root(&root).unwrap_or_else(|_| root.clone());
        if deduplicated.iter().any(|(_, existing)| {
            path_is_within(&identity, existing) && path_is_within(existing, &identity)
        }) {
            continue;
        }
        deduplicated.push((root, identity));
    }
    deduplicated
        .into_iter()
        .map(|(original, _)| original)
        .collect()
}

fn path_is_within(candidate: &Path, root: &Path) -> bool {
    if candidate.starts_with(root) {
        return true;
    }
    #[cfg(windows)]
    {
        let candidate = candidate.to_string_lossy().replace('/', "\\");
        let root = root.to_string_lossy().replace('/', "\\");
        let root = root.trim_end_matches('\\');
        return candidate.eq_ignore_ascii_case(root)
            || candidate
                .get(..root.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(root))
                && candidate.as_bytes().get(root.len()) == Some(&b'\\');
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn link_directory(target: &Path, link: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn link_directory(target: &Path, link: &Path) -> io::Result<()> {
        std::os::windows::fs::symlink_dir(target, link)
    }

    #[cfg(unix)]
    fn link_file(target: &Path, link: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn link_file(target: &Path, link: &Path) -> io::Result<()> {
        std::os::windows::fs::symlink_file(target, link)
    }

    #[test]
    fn rejects_parent_traversal_and_absolute_escape() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let outside = root.path().join("outside.txt");
        fs::create_dir(&workspace).unwrap();
        fs::write(&outside, "secret").unwrap();

        assert!(resolve_existing(&workspace, "../outside.txt").is_err());
        assert!(resolve_existing(&workspace, outside.to_str().unwrap()).is_err());
        assert!(resolve_for_write_with_scope(
            &workspace,
            "../new-outside.txt",
            &ExecutionScope::workspace_only(&workspace)
        )
        .is_err());
    }

    #[test]
    fn accepts_existing_and_new_paths_inside_workspace() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        fs::write(workspace.join("readme.md"), "hello").unwrap();

        assert!(resolve_existing(&workspace, "./readme.md").is_ok());
        let new_path = resolve_for_write_with_scope(
            &workspace,
            "src/new.rs",
            &ExecutionScope::workspace_only(&workspace),
        )
        .unwrap();
        assert!(new_path.starts_with(fs::canonicalize(&workspace).unwrap()));
    }

    #[test]
    fn restricted_scope_accepts_each_root_and_rejects_a_sibling() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let app_data = root.path().join("app-data");
        let outside = root.path().join("outside");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&app_data).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(workspace.join("workspace.txt"), "workspace").unwrap();
        fs::write(app_data.join("app.txt"), "app").unwrap();
        fs::write(outside.join("outside.txt"), "outside").unwrap();
        let scope = ExecutionScope::restricted([workspace.clone(), app_data.clone()]);

        assert!(resolve_existing_with_scope(&workspace, "workspace.txt", &scope).is_ok());
        assert!(resolve_existing_with_scope(
            &workspace,
            app_data.join("app.txt").to_str().unwrap(),
            &scope
        )
        .is_ok());
        assert!(resolve_existing_with_scope(
            &workspace,
            outside.join("outside.txt").to_str().unwrap(),
            &scope
        )
        .is_err());
    }

    #[test]
    fn unrestricted_scope_allows_absolute_outside_targets() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let outside = root.path().join("outside");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let existing = outside.join("existing.txt");
        fs::write(&existing, "outside").unwrap();

        assert_eq!(
            resolve_existing_with_scope(
                &workspace,
                existing.to_str().unwrap(),
                &ExecutionScope::Unrestricted
            )
            .unwrap(),
            fs::canonicalize(&existing).unwrap()
        );
        // The write path canonicalizes the nearest existing ancestor before
        // rejoining the unresolved suffix, so the result carries whatever prefix
        // canonicalization produces for that directory.
        assert_eq!(
            resolve_for_write_with_scope(
                &workspace,
                outside.join("new.txt").to_str().unwrap(),
                &ExecutionScope::Unrestricted
            )
            .unwrap(),
            fs::canonicalize(&outside).unwrap().join("new.txt")
        );
    }

    #[test]
    fn denied_root_overrides_unrestricted_reads_and_writes() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let app_data = root.path().join("app-data");
        let memory = app_data.join("memory");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&memory).unwrap();
        fs::write(memory.join("memory.v1.sqlite3"), "private").unwrap();
        fs::write(app_data.join("ordinary.txt"), "public").unwrap();
        let scope = ExecutionScope::Unrestricted.denying([app_data.join("memory")]);

        assert!(resolve_existing_with_scope(
            &workspace,
            memory.join("memory.v1.sqlite3").to_str().unwrap(),
            &scope
        )
        .is_err());
        assert!(resolve_for_write_with_scope(
            &workspace,
            memory.join("memory.v1.sqlite3-wal").to_str().unwrap(),
            &scope
        )
        .is_err());
        assert!(resolve_existing_with_scope(
            &workspace,
            app_data.join("ordinary.txt").to_str().unwrap(),
            &scope
        )
        .is_ok());
        assert!(existing_path_is_allowed(&scope, &app_data));
        assert!(!existing_path_is_allowed(&scope, &memory));
    }

    #[test]
    fn denied_root_blocks_new_root_and_descendants_before_the_root_exists() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let app_data = root.path().join("app-data");
        let memory = app_data.join("memory");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&app_data).unwrap();
        assert!(!memory.exists());
        let scope = ExecutionScope::Unrestricted.denying([memory.clone()]);

        assert!(
            resolve_for_write_with_scope(&workspace, memory.to_str().unwrap(), &scope).is_err()
        );
        assert!(resolve_for_write_with_scope(
            &workspace,
            memory.join("memory.v1.sqlite3").to_str().unwrap(),
            &scope
        )
        .is_err());
        assert!(resolve_for_write_with_scope(
            &workspace,
            memory.join("nested").join("MEMORY.md").to_str().unwrap(),
            &scope
        )
        .is_err());
        assert!(!memory.exists());
    }

    #[test]
    fn denied_nonexistent_root_is_not_bypassed_through_an_existing_alias() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let app_data = root.path().join("app-data");
        let memory = app_data.join("memory");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&app_data).unwrap();
        let app_data_alias = workspace.join("app-data-alias");
        if link_directory(&app_data, &app_data_alias).is_err() {
            return;
        }
        assert!(!memory.exists());
        let scope = ExecutionScope::Unrestricted.denying([memory]);

        assert!(resolve_for_write_with_scope(
            &workspace,
            "app-data-alias/memory/memory.v1.sqlite3",
            &scope
        )
        .is_err());
        assert!(!app_data.join("memory").exists());
    }

    #[test]
    fn denied_root_follows_a_directory_alias_before_comparison() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let memory = root.path().join("app-data").join("memory");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&memory).unwrap();
        fs::write(memory.join("memory.v1.sqlite3"), "private").unwrap();
        let alias = workspace.join("memory-alias");
        if link_directory(&memory, &alias).is_err() {
            return;
        }
        let scope = ExecutionScope::Unrestricted.denying([memory]);

        assert!(
            resolve_existing_with_scope(&workspace, "memory-alias/memory.v1.sqlite3", &scope)
                .is_err()
        );
        assert!(!existing_path_is_allowed(&scope, &alias));
    }

    #[test]
    fn adding_denied_roots_preserves_existing_entries_and_deduplicates_aliases() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first");
        let second = root.path().join("second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        let duplicate = root.path().join("first-alias");
        let has_alias = link_directory(&first, &duplicate).is_ok();

        let mut scope = ExecutionScope::Unrestricted
            .denying([first.clone()])
            .denying([second.clone()]);
        if has_alias {
            scope = scope.denying([duplicate]);
        }
        let ExecutionScope::UnrestrictedExcept { denied_roots } = scope else {
            panic!("denying unrestricted access must keep an except scope");
        };
        assert_eq!(denied_roots.len(), 2);
        assert!(denied_roots.contains(&first));
        assert!(denied_roots.contains(&second));
    }

    #[test]
    fn restricted_scope_rejects_a_symlink_ancestor_escape() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let outside = root.path().join("outside");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.txt"), "secret").unwrap();
        let link = workspace.join("escape");
        // Windows may disallow symlink creation when Developer Mode is off.
        // The production canonicalization is shared across platforms; skip only
        // this platform capability-dependent assertion when creation is denied.
        if link_directory(&outside, &link).is_err() {
            return;
        }
        let scope = ExecutionScope::workspace_only(&workspace);

        assert!(resolve_existing_with_scope(&workspace, "escape/secret.txt", &scope).is_err());
        assert!(resolve_for_write_with_scope(&workspace, "escape/new.txt", &scope).is_err());
        assert!(resolve_existing_with_scope(
            &workspace,
            "escape/secret.txt",
            &ExecutionScope::Unrestricted
        )
        .is_ok());
    }

    #[test]
    fn secure_open_rejects_a_parent_swapped_after_resolution() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let original = workspace.join("safe");
        let moved = workspace.join("safe-original");
        let outside = root.path().join("outside");
        fs::create_dir_all(&original).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(original.join("image.png"), b"approved").unwrap();
        fs::write(outside.join("image.png"), b"secret").unwrap();
        let scope = ExecutionScope::workspace_only(&workspace);
        let expected = resolve_existing_with_scope(&workspace, "safe/image.png", &scope).unwrap();

        fs::rename(&original, &moved).unwrap();
        if link_directory(&outside, &original).is_err() {
            return;
        }

        assert!(open_verified_scoped_file(&expected, &scope).is_err());
    }

    #[test]
    fn secure_open_returns_the_authorized_regular_file_handle() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(workspace.join("image.png"), b"approved").unwrap();
        let scope = ExecutionScope::workspace_only(&workspace);

        let (mut file, canonical) =
            secure_open_existing_file_with_scope(&workspace, "image.png", &scope).unwrap();
        let mut bytes = Vec::new();
        use std::io::Read as _;
        file.read_to_end(&mut bytes).unwrap();

        assert_eq!(bytes, b"approved");
        assert_eq!(
            canonical,
            fs::canonicalize(workspace.join("image.png")).unwrap()
        );
    }

    #[test]
    fn write_resolution_rejects_broken_symlinks_instead_of_treating_them_as_missing() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let outside = root.path().join("outside");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let scope = ExecutionScope::workspace_only(&workspace);

        let missing_file = outside.join("future-secret.txt");
        let file_alias = workspace.join("broken-file");
        if link_file(&missing_file, &file_alias).is_ok() {
            assert!(fs::symlink_metadata(&file_alias).is_ok());
            assert!(!file_alias.exists(), "the test alias must remain broken");
            assert!(resolve_for_write_with_scope(&workspace, "broken-file", &scope).is_err());
            assert!(!missing_file.exists());
        }

        let missing_directory = outside.join("future-directory");
        let directory_alias = workspace.join("broken-directory");
        if link_directory(&missing_directory, &directory_alias).is_ok() {
            assert!(fs::symlink_metadata(&directory_alias).is_ok());
            assert!(
                !directory_alias.exists(),
                "the test directory alias must remain broken"
            );
            assert!(resolve_for_write_with_scope(
                &workspace,
                "broken-directory/new-secret.txt",
                &scope
            )
            .is_err());
            assert!(!missing_directory.exists());
        }
    }
}
