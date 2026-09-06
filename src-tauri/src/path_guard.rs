use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Component, Path, PathBuf},
};
use uuid::Uuid;

struct DirectoryAuthority {
    path: PathBuf,
    handle: File,
}

/// A write target whose directory ancestry and optional existing target are
/// bound to live OS handles.
///
/// On Windows every directory from the selected execution root through the
/// final parent is opened without `FILE_SHARE_DELETE`. That prevents a
/// symlink/junction or ordinary-directory swap for the whole lifetime of this
/// value. An existing target is likewise held without delete sharing; a new
/// target is installed with no-replace semantics so a late symlink/reparse
/// insertion fails closed.
///
/// Unix builds keep no-follow handles and revalidate their live identities
/// before temporary-file creation and final installation. Those handles do not
/// provide Windows-style rename exclusion, so the final no-replace operation
/// remains the security boundary for a new target there.
pub struct SecureWriteAuthority {
    target: PathBuf,
    parent: PathBuf,
    scope: ExecutionScope,
    directories: Vec<DirectoryAuthority>,
    existing_target: Option<File>,
    #[cfg(test)]
    injected_temporary_write_failure_after: Option<usize>,
}

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

    #[allow(dead_code)] // Kept for the legacy workspace-only executor entry point.
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

    /// Narrows the scope to the workspace while keeping every denied root.
    ///
    /// Tool inputs whose published contract is workspace-relative must stay
    /// inside the workspace even when the conversation was granted unrestricted
    /// filesystem access, so an approval meant for local editing cannot be
    /// turned into an upload of arbitrary host files.
    // Not yet wired: kept as the capability-narrowing seam for
    // workspace-relative tool contracts.
    #[allow(dead_code)]
    pub fn confined_to(&self, workspace: &Path) -> Self {
        let roots = vec![workspace.to_path_buf()];
        match self {
            Self::Restricted { .. } | Self::Unrestricted => Self::Restricted { roots },
            Self::RestrictedExcept { denied_roots, .. }
            | Self::UnrestrictedExcept { denied_roots } => Self::RestrictedExcept {
                roots,
                denied_roots: denied_roots.clone(),
            },
        }
    }
}

pub fn canonical_workspace(path: &Path) -> Result<PathBuf, String> {
    let canonical = fs::canonicalize(path)
        .map_err(|error| format!("Could not access workspace {}: {error}", path.display()))?;
    if !canonical.is_dir() {
        return Err(format!("Workspace is not a directory: {}", canonical.display()));
    }
    Ok(canonical)
}

#[allow(dead_code)] // Backward-compatible workspace-only path API.
pub fn resolve_existing(workspace: &Path, requested: &str) -> Result<PathBuf, String> {
    resolve_existing_with_scope(
        workspace,
        requested,
        &ExecutionScope::workspace_only(workspace),
    )
}

#[allow(dead_code)] // Backward-compatible workspace-only path API.
pub fn resolve_for_write(workspace: &Path, requested: &str) -> Result<PathBuf, String> {
    resolve_for_write_with_scope(
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
        .map_err(|error| format!("Could not securely open file {}: {error}", expected.display()))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("Could not inspect file handle {}: {error}", expected.display()))?;
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
        .map_err(|error| format!("Could not securely open file {}: {error}", expected.display()))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("Could not inspect file handle {}: {error}", expected.display()))?;
    if !metadata.is_file() {
        return Err("read target must be a regular file without symlinks".into());
    }

    #[cfg(target_os = "linux")]
    let actual = fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
        .map_err(|error| format!("Could not verify file handle {}: {error}", expected.display()))?;

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
            let canonical = fs::canonicalize(&candidate)
                .map_err(|error| format!("Could not access write target {}: {error}", candidate.display()))?;
            ensure_allowed(scope, &canonical)?;
            return Ok(canonical);
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("Could not inspect write target {}: {error}", candidate.display())),
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

/// Prepares a handle-bound authority for a later file installation.
///
/// Callers may perform bounded work such as importing a content-addressed
/// sidecar while holding the returned value. They must call
/// [`SecureWriteAuthority::install`] for the final write instead of reopening
/// [`SecureWriteAuthority::target`] by path.
pub fn prepare_secure_write_with_scope(
    workspace: &Path,
    requested: &str,
    scope: &ExecutionScope,
) -> Result<SecureWriteAuthority, String> {
    let workspace = canonical_workspace(workspace)?;
    let candidate = candidate_path(&workspace, requested)?;
    reject_reparse_components(&candidate)?;

    let prospective_target = resolve_for_write_with_scope(&workspace, requested, scope)?;
    let prospective_parent = prospective_target
        .parent()
        .ok_or_else(|| "Secure write target has no parent directory".to_owned())?
        .to_path_buf();
    let authority_root = authority_root_for(scope, &prospective_target)?;
    let directories =
        open_or_create_directory_authority(&authority_root, &prospective_parent, scope)?;

    // Re-resolve only after the complete parent chain is held. On Windows the
    // handles opened above exclude delete sharing, so this identity remains
    // stable across all caller work and the final installation.
    reject_reparse_components(&candidate)?;
    let target = resolve_for_write_with_scope(&workspace, requested, scope)?;
    if !same_path_identity_portable(&target, &prospective_target) {
        return Err("Secure write path changed while directory authority was established".into());
    }
    let parent = target
        .parent()
        .ok_or_else(|| "Secure write target has no parent directory".to_owned())?
        .to_path_buf();
    if !same_path_identity_portable(&parent, &prospective_parent) {
        return Err("Secure write parent directory changed while directory authority was established".into());
    }

    let existing_target = match fs::symlink_metadata(&target) {
        Ok(metadata) => {
            if metadata_is_reparse(&metadata) {
                return Err("Secure write rejects a symlink or reparse-point target".into());
            }
            Some(open_verified_scoped_write_file(&target, scope)?)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(format!(
                "Could not inspect secure write target {}: {error}",
                target.display()
            ))
        }
    };

    Ok(SecureWriteAuthority {
        target,
        parent,
        scope: scope.clone(),
        directories,
        existing_target,
        #[cfg(test)]
        injected_temporary_write_failure_after: None,
    })
}

impl SecureWriteAuthority {
    pub fn target(&self) -> &Path {
        &self.target
    }

    /// Installs bytes without ever writing through the existing target handle.
    ///
    /// All bytes are first written and synced in a private, create-new file
    /// under the held parent. Existing regular files remain handle-bound until
    /// that staging succeeds, then one filesystem rename operation atomically
    /// replaces the directory entry. A crash or write/flush/sync failure before
    /// that commit therefore leaves the old file intact. New targets are
    /// created by linking the staged file with no-replace semantics, so a late
    /// symlink/reparse or ordinary target causes an error instead of being
    /// followed or overwritten.
    pub fn install(mut self, bytes: &[u8]) -> Result<(), String> {
        self.verify_live_authority()?;
        let parent = self
            .directories
            .last()
            .ok_or_else(|| "Secure write lacks the final parent-directory handle".to_owned())?;
        let (mut temporary, temporary_path) =
            create_secure_temporary_file(&self.parent, &parent.handle, &self.scope)?;
        let temporary_write = (|| {
            #[cfg(test)]
            if let Some(limit) = self.injected_temporary_write_failure_after {
                let written = limit.min(bytes.len());
                temporary
                    .write_all(&bytes[..written])
                    .map_err(|error| format!("Could not write secure temporary file: {error}"))?;
                temporary
                    .flush()
                    .map_err(|error| format!("Could not flush secure temporary file: {error}"))?;
                temporary
                    .sync_all()
                    .map_err(|error| format!("Could not sync secure temporary file: {error}"))?;
                return Err(format!("Test injection: secure temporary file write failed after {written} bytes"));
            }
            temporary
                .write_all(bytes)
                .map_err(|error| format!("Could not write secure temporary file: {error}"))?;
            temporary
                .flush()
                .map_err(|error| format!("Could not flush secure temporary file: {error}"))?;
            temporary
                .sync_all()
                .map_err(|error| format!("Could not sync secure temporary file: {error}"))?;
            verify_open_handle(&temporary, &temporary_path, &self.scope, "secure temporary file")
        })();
        if let Err(error) = temporary_write {
            drop(temporary);
            self.remove_temporary_file_best_effort(&temporary_path);
            return Err(error);
        }

        let result = (|| -> Result<bool, String> {
            self.verify_live_authority()?;
            if let Some(target) = self.existing_target.take() {
                verify_open_handle(&target, &self.target, &self.scope, "secure write target")?;
                let metadata = target
                    .metadata()
                    .map_err(|error| format!("Could not inspect secure write target handle: {error}"))?;
                if !metadata.is_file() || metadata_is_reparse(&metadata) {
                    return Err("Secure write target is no longer a regular file without a reparse point".into());
                }
                reject_reparse_components(&self.target)?;
                // Windows keeps this handle without FILE_SHARE_DELETE for the
                // complete capture/import/staging interval. Release it only
                // after the last identity check so the single atomic rename can
                // replace the entry. The held parent chain prevents an ancestor
                // escape, and rename replaces a late final symlink as an entry
                // rather than following it.
                drop(target);
                let parent = self
                    .directories
                    .last()
                    .ok_or_else(|| "Secure write lacks the final parent-directory handle".to_owned())?;
                atomic_replace_existing(&temporary, &temporary_path, &self.target, &parent.handle)?;
                return Ok(true);
            }

            match fs::symlink_metadata(&self.target) {
                Ok(metadata) if metadata_is_reparse(&metadata) => {
                    return Err("Secure write refuses a late symlink or reparse-point target".into())
                }
                Ok(_) => return Err("Secure write target was created by another process before installation".into()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!(
                        "Could not inspect secure write target before installation {}: {error}",
                        self.target.display()
                    ))
                }
            }
            verify_open_handle(&temporary, &temporary_path, &self.scope, "secure temporary file")?;
            let parent = self
                .directories
                .last()
                .ok_or_else(|| "Secure write lacks the final parent-directory handle".to_owned())?;
            install_new_no_replace(&temporary_path, &self.target, &parent.handle)?;
            Ok(false)
        })();

        drop(temporary);
        match result {
            Err(error) => {
                self.remove_temporary_file_best_effort(&temporary_path);
                Err(error)
            }
            Ok(replaced_existing) => {
                if !replaced_existing {
                    // The hard link is already the committed target. Cleanup
                    // cannot turn that successful installation into a failure
                    // or cause browser state to disagree with the filesystem.
                    self.remove_temporary_file_best_effort(&temporary_path);
                }
                self.sync_parent_directory_best_effort();
                Ok(())
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn inject_temporary_write_failure_after(&mut self, bytes: usize) {
        self.injected_temporary_write_failure_after = Some(bytes);
    }

    fn remove_temporary_file_best_effort(&self, temporary_path: &Path) {
        let Some(parent) = self.directories.last() else {
            return;
        };
        remove_temporary_at(&parent.handle, temporary_path);
    }

    fn sync_parent_directory_best_effort(&self) {
        if let Some(parent) = self.directories.last() {
            // The directory entry has already committed atomically. A parent
            // sync improves rename durability on filesystems that support it,
            // but reporting a post-commit sync error would falsely describe a
            // successful install as a failure and cannot restore the old name.
            let _ = parent.handle.sync_all();
        }
    }

    fn verify_live_authority(&self) -> Result<(), String> {
        ensure_allowed(&self.scope, &self.target)?;
        for directory in &self.directories {
            let metadata = directory
                .handle
                .metadata()
                .map_err(|error| format!("Could not inspect directory authority handle: {error}"))?;
            if !metadata.is_dir() || metadata_is_reparse(&metadata) {
                return Err("Secure write directory authority no longer points to a regular directory".into());
            }
            verify_open_handle(
                &directory.handle,
                &directory.path,
                &self.scope,
                "secure write directory",
            )?;
        }
        let actual_parent = fs::canonicalize(&self.parent)
            .map_err(|error| format!("Could not reverify secure write parent directory: {error}"))?;
        if !same_path_identity_portable(&actual_parent, &self.parent) {
            return Err("Secure write parent directory changed while authority was held".into());
        }
        Ok(())
    }
}

fn authority_root_for(scope: &ExecutionScope, target: &Path) -> Result<PathBuf, String> {
    ensure_allowed(scope, target)?;
    match scope {
        ExecutionScope::Restricted { roots } | ExecutionScope::RestrictedExcept { roots, .. } => {
            roots
                .iter()
                .filter_map(|root| canonical_workspace(root).ok())
                .filter(|root| path_is_within(target, root))
                .max_by_key(|root| root.components().count())
                .ok_or_else(|| "Secure write target has no matching trusted root".to_owned())
        }
        ExecutionScope::Unrestricted | ExecutionScope::UnrestrictedExcept { .. } => {
            filesystem_root(target)
        }
    }
}

fn filesystem_root(path: &Path) -> Result<PathBuf, String> {
    let mut root = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => root.push(prefix.as_os_str()),
            Component::RootDir => root.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir | Component::Normal(_) => break,
        }
    }
    if root.as_os_str().is_empty() {
        Err(format!(
            "secure write targethas no filesystem root: {}",
            path.display()
        ))
    } else {
        Ok(root)
    }
}

fn open_or_create_directory_authority(
    root: &Path,
    parent: &Path,
    scope: &ExecutionScope,
) -> Result<Vec<DirectoryAuthority>, String> {
    let suffix = parent.strip_prefix(root).map_err(|_| {
        format!(
            "Secure write parent directory {} is not under authority root {}",
            parent.display(),
            root.display()
        )
    })?;
    let mut directories = Vec::new();
    let mut current = root.to_path_buf();
    directories.push(DirectoryAuthority {
        path: current.clone(),
        handle: open_verified_directory(&current, scope)?,
    });

    for component in suffix.components() {
        let Component::Normal(name) = component else {
            return Err(format!(
                "Secure write parent directory contains non-canonical components: {}",
                parent.display()
            ));
        };
        if let Some(authority) = directories.last() {
            verify_open_handle(&authority.handle, &authority.path, scope, "secure write parent directory")?;
        }
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata_is_reparse(&metadata) || !metadata.is_dir() {
                    return Err(format!(
                        "Secure write directory must be a regular directory without a reparse point: {}",
                        current.display()
                    ));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match fs::create_dir(&current) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => {
                        return Err(format!(
                            "Could not create secure write directory {}: {error}",
                            current.display()
                        ))
                    }
                }
            }
            Err(error) => {
                return Err(format!(
                    "Could not inspect secure write directory {}: {error}",
                    current.display()
                ))
            }
        }
        let metadata = fs::symlink_metadata(&current)
            .map_err(|error| format!("Could not recheck secure write directory {}: {error}", current.display()))?;
        if metadata_is_reparse(&metadata) || !metadata.is_dir() {
            return Err(format!(
                "Secure write directory changed during creation: {}",
                current.display()
            ));
        }
        directories.push(DirectoryAuthority {
            path: current.clone(),
            handle: open_verified_directory(&current, scope)?,
        });
    }
    Ok(directories)
}

fn reject_reparse_components(path: &Path) -> Result<(), String> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                current.push(prefix.as_os_str());
                continue;
            }
            Component::RootDir | Component::Normal(_) => current.push(component.as_os_str()),
            Component::CurDir => continue,
            Component::ParentDir => {
                return Err(format!("Secure write path contains a parent-directory component: {}", path.display()))
            }
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata_is_reparse(&metadata) => {
                return Err(format!(
                    "Secure write path rejects symlink or reparse-point components: {}",
                    current.display()
                ))
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "Could not inspect secure write path component {}: {error}",
                    current.display()
                ))
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
fn metadata_is_reparse(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn metadata_is_reparse(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
fn open_verified_directory(path: &Path, scope: &ExecutionScope) -> Result<File, String> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    let directory = fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|error| format!("Could not lock secure write directory {}: {error}", path.display()))?;
    let metadata = directory
        .metadata()
        .map_err(|error| format!("Could not inspect secure write directory handle {}: {error}", path.display()))?;
    if !metadata.is_dir() || metadata_is_reparse(&metadata) {
        return Err(format!(
            "Secure write directory must be a regular directory without a reparse point: {}",
            path.display()
        ));
    }
    verify_open_handle(&directory, path, scope, "secure write directory")?;
    Ok(directory)
}

#[cfg(unix)]
fn open_verified_directory(path: &Path, scope: &ExecutionScope) -> Result<File, String> {
    use std::os::unix::fs::OpenOptionsExt;

    let directory = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| format!("Could not open secure write directory {}: {error}", path.display()))?;
    let metadata = directory
        .metadata()
        .map_err(|error| format!("Could not inspect secure write directory handle {}: {error}", path.display()))?;
    if !metadata.is_dir() {
        return Err(format!("Secure write directory is not a regular directory: {}", path.display()));
    }
    verify_open_handle(&directory, path, scope, "secure write directory")?;
    Ok(directory)
}

#[cfg(not(any(windows, unix)))]
fn open_verified_directory(path: &Path, scope: &ExecutionScope) -> Result<File, String> {
    let directory = File::open(path)
        .map_err(|error| format!("Could not open secure write directory {}: {error}", path.display()))?;
    if !directory
        .metadata()
        .map_err(|error| format!("Could not inspect secure write directory handle: {error}"))?
        .is_dir()
    {
        return Err(format!("Secure write directory is not a regular directory: {}", path.display()));
    }
    verify_open_handle(&directory, path, scope, "secure write directory")?;
    Ok(directory)
}

#[cfg(windows)]
fn open_verified_scoped_write_file(path: &Path, scope: &ExecutionScope) -> Result<File, String> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ};

    let file = fs::OpenOptions::new()
        .read(true)
        // Keep both content and pathname stable until the final identity check.
        // The handle is released immediately before the one atomic rename.
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|error| format!("Could not lock secure write target {}: {error}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("Could not inspect secure write target handle {}: {error}", path.display()))?;
    if !metadata.is_file() || metadata_is_reparse(&metadata) {
        return Err("Secure write target must be a regular file without a reparse point".into());
    }
    verify_open_handle(&file, path, scope, "secure write target")?;
    Ok(file)
}

#[cfg(unix)]
fn open_verified_scoped_write_file(path: &Path, scope: &ExecutionScope) -> Result<File, String> {
    use std::os::unix::fs::OpenOptionsExt;

    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| format!("Could not open secure write target {}: {error}", path.display()))?;
    if !file
        .metadata()
        .map_err(|error| format!("Could not inspect secure write target handle: {error}"))?
        .is_file()
    {
        return Err("Secure write target must be a regular file without symlinks".into());
    }
    verify_open_handle(&file, path, scope, "secure write target")?;
    Ok(file)
}

#[cfg(not(any(windows, unix)))]
fn open_verified_scoped_write_file(path: &Path, scope: &ExecutionScope) -> Result<File, String> {
    let file = fs::OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|error| format!("Could not open secure write target {}: {error}", path.display()))?;
    if !file
        .metadata()
        .map_err(|error| format!("Could not inspect secure write target handle: {error}"))?
        .is_file()
    {
        return Err("Secure write target must be a regular file".into());
    }
    verify_open_handle(&file, path, scope, "secure write target")?;
    Ok(file)
}

fn create_secure_temporary_file(
    parent: &Path,
    parent_handle: &File,
    scope: &ExecutionScope,
) -> Result<(File, PathBuf), String> {
    for _ in 0..8 {
        let path = parent.join(format!(
            ".mework-secure-write-{}.tmp",
            Uuid::new_v4().simple()
        ));
        ensure_allowed(scope, &path)?;
        match create_new_no_follow_at(parent_handle, &path) {
            Ok(file) => {
                if let Err(error) = verify_open_handle(&file, &path, scope, "secure temporary file") {
                    drop(file);
                    remove_temporary_at(parent_handle, &path);
                    return Err(error);
                }
                return Ok((file, path));
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("Could not create secure temporary file: {error}")),
        }
    }
    Err("Could not allocate a unique secure temporary filename".into())
}

#[cfg(windows)]
fn create_new_no_follow_at(_parent: &File, path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::{
        Foundation::{GENERIC_READ, GENERIC_WRITE},
        Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ},
    };
    // DELETE is the Win32 standard access right required to rename through an
    // existing handle. windows-sys exposes it only behind a feature Mework
    // otherwise does not need.
    const DELETE_ACCESS: u32 = 0x0001_0000;
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        // DELETE access allows SetFileInformationByHandle to rename this exact
        // verified handle. Delete sharing is required for the rename while
        // write sharing remains closed to other processes.
        .access_mode(GENERIC_READ | GENERIC_WRITE | DELETE_ACCESS)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(unix)]
fn create_new_no_follow_at(parent: &File, path: &Path) -> io::Result<File> {
    use std::os::unix::{
        ffi::OsStrExt,
        io::{AsRawFd, FromRawFd},
    };

    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Secure temporary file has no filename"))?;
    let name = std::ffi::CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Secure temporary filename contains a NUL byte"))?;
    // SAFETY: `name` is one NUL-terminated component, `parent` owns a live
    // directory descriptor, and a successful descriptor is transferred
    // exactly once into `File`.
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600 as libc::mode_t,
        )
    };
    if descriptor == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `openat` returned a fresh owned descriptor which has not been
    // wrapped or closed elsewhere.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

#[cfg(not(any(windows, unix)))]
fn create_new_no_follow_at(_parent: &File, path: &Path) -> io::Result<File> {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
}

#[cfg(unix)]
fn unix_file_name(path: &Path, label: &str) -> Result<std::ffi::CString, String> {
    use std::os::unix::ffi::OsStrExt;

    let name = path
        .file_name()
        .ok_or_else(|| format!("{label} has no filename"))?;
    std::ffi::CString::new(name.as_bytes()).map_err(|_| format!("{label} contains a NUL byte"))
}

#[cfg(unix)]
fn install_new_no_replace(
    temporary_path: &Path,
    target: &Path,
    parent: &File,
) -> Result<(), String> {
    use std::os::unix::io::AsRawFd;

    let temporary_name = unix_file_name(temporary_path, "secure temporary filename")?;
    let target_name = unix_file_name(target, "secure target filename")?;
    // SAFETY: both names are NUL-terminated single path components. The same
    // live parent descriptor binds source and destination to the directory
    // that was authorized, even if its pathname is concurrently replaced.
    // linkat has no replace flag: an existing destination fails with EEXIST.
    let linked = unsafe {
        libc::linkat(
            parent.as_raw_fd(),
            temporary_name.as_ptr(),
            parent.as_raw_fd(),
            target_name.as_ptr(),
            0,
        )
    };
    if linked == -1 {
        return Err(format!(
            "Could not install secure target file without replacement: {}",
            io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn install_new_no_replace(
    temporary_path: &Path,
    target: &Path,
    _parent: &File,
) -> Result<(), String> {
    fs::hard_link(temporary_path, target)
        .map_err(|error| format!("Could not install secure target file without replacement: {error}"))
}

#[cfg(unix)]
fn remove_temporary_at(parent: &File, temporary_path: &Path) {
    use std::os::unix::io::AsRawFd;

    let Ok(temporary_name) = unix_file_name(temporary_path, "secure temporary filename") else {
        return;
    };
    // SAFETY: the name is a NUL-terminated single component and the live
    // parent descriptor keeps cleanup bound to the authorized directory.
    let _ = unsafe { libc::unlinkat(parent.as_raw_fd(), temporary_name.as_ptr(), 0) };
}

#[cfg(not(unix))]
fn remove_temporary_at(_parent: &File, temporary_path: &Path) {
    let _ = fs::remove_file(temporary_path);
}

#[cfg(windows)]
fn atomic_replace_existing(
    temporary: &File,
    _temporary_path: &Path,
    target: &Path,
    _parent: &File,
) -> Result<(), String> {
    use std::{
        mem::{offset_of, size_of, MaybeUninit},
        os::windows::{ffi::OsStrExt, io::AsRawHandle},
        ptr,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FileRenameInfo, SetFileInformationByHandle, FILE_RENAME_INFO,
    };

    // SetFileInformationByHandle rejects a Win32 directory handle in
    // FILE_RENAME_INFO.RootDirectory. The absolute destination remains safe:
    // every ancestor is already held without FILE_SHARE_DELETE on Windows, and
    // the source is the exact verified temporary handle rather than a path.
    let target_name = target.as_os_str().encode_wide().collect::<Vec<_>>();
    if target_name.is_empty() || target_name.contains(&0) {
        return Err("secure target filename is not a valid non-empty UTF-16 path".into());
    }
    let name_bytes = target_name
        .len()
        .checked_mul(size_of::<u16>())
        .ok_or_else(|| "secure target filename is too long".to_owned())?;
    let name_bytes_u32 = u32::try_from(name_bytes).map_err(|_| "secure target filename exceeds system limits")?;
    let buffer_size = offset_of!(FILE_RENAME_INFO, FileName)
        .checked_add(name_bytes)
        .ok_or_else(|| "Secure target rename buffer is too large".to_owned())?;
    let buffer_size_u32 =
        u32::try_from(buffer_size).map_err(|_| "Secure target rename buffer exceeds system limits")?;
    let slot_size = size_of::<FILE_RENAME_INFO>();
    let slot_count = buffer_size
        .checked_add(slot_size - 1)
        .ok_or_else(|| "Secure target rename buffer is too large".to_owned())?
        / slot_size;
    // A byte vector only guarantees byte alignment and cannot safely be cast
    // to FILE_RENAME_INFO. Using that structure as the allocation element
    // provides its exact required alignment while still allowing the trailing
    // flexible-array bytes to span multiple zeroed slots.
    let mut buffer = std::iter::repeat_with(MaybeUninit::<FILE_RENAME_INFO>::zeroed)
        .take(slot_count)
        .collect::<Vec<_>>();

    // SAFETY: `buffer` has FILE_RENAME_INFO alignment and at least
    // `buffer_size` initialized bytes. The flexible-array field receives the
    // exact checked UTF-16 filename bytes. The allocation stays alive and
    // uniquely borrowed for the complete synchronous Win32 call. `temporary`
    // was opened with DELETE access and remains the verified staging handle.
    let replaced = unsafe {
        let information = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
        (*information).Anonymous.ReplaceIfExists = true;
        (*information).RootDirectory = ptr::null_mut();
        (*information).FileNameLength = name_bytes_u32;
        ptr::copy_nonoverlapping(
            target_name.as_ptr(),
            ptr::addr_of_mut!((*information).FileName).cast::<u16>(),
            target_name.len(),
        );
        SetFileInformationByHandle(
            temporary.as_raw_handle().cast(),
            FileRenameInfo,
            information.cast(),
            buffer_size_u32,
        )
    };
    if replaced == 0 {
        return Err(format!(
            "Could not atomically replace secure target file {}: {}",
            target.display(),
            io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn atomic_replace_existing(
    _temporary: &File,
    temporary_path: &Path,
    target: &Path,
    parent: &File,
) -> Result<(), String> {
    use std::os::unix::io::AsRawFd;

    let temporary_name = unix_file_name(temporary_path, "secure temporary filename")?;
    let target_name = unix_file_name(target, "secure target filename")?;
    // SAFETY: both names are NUL-terminated single path components, and the
    // directory descriptor remains owned by the live authority for this call.
    let renamed = unsafe {
        libc::renameat(
            parent.as_raw_fd(),
            temporary_name.as_ptr(),
            parent.as_raw_fd(),
            target_name.as_ptr(),
        )
    };
    if renamed == -1 {
        return Err(format!(
            "Could not atomically replace secure target file {}: {}",
            target.display(),
            io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(not(any(windows, unix)))]
fn atomic_replace_existing(
    _temporary: &File,
    temporary_path: &Path,
    target: &Path,
    _parent: &File,
) -> Result<(), String> {
    fs::rename(temporary_path, target)
        .map_err(|error| format!("Could not atomically replace secure target file {}: {error}", target.display()))
}

fn verify_open_handle(
    file: &File,
    expected: &Path,
    scope: &ExecutionScope,
    label: &str,
) -> Result<(), String> {
    let actual = opened_handle_path(file, expected)
        .map_err(|error| format!("Could not verify {label} {}: {error}", expected.display()))?;
    ensure_allowed(scope, &actual)?;
    if !same_path_identity_portable(&actual, expected) {
        return Err(format!(
            "{label} passed through a directory replacement or reparse point while authority was held: {}",
            expected.display()
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn opened_handle_path(file: &File, _expected: &Path) -> io::Result<PathBuf> {
    use std::os::windows::{ffi::OsStringExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{GetFinalPathNameByHandleW, VOLUME_NAME_DOS};

    let mut buffer = vec![0_u16; 512];
    loop {
        // SAFETY: `file` owns the live handle and `buffer` is writable for the
        // exact length supplied to Win32.
        let length = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle().cast(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                VOLUME_NAME_DOS,
            )
        };
        if length == 0 {
            return Err(io::Error::last_os_error());
        }
        if (length as usize) < buffer.len() {
            return Ok(PathBuf::from(std::ffi::OsString::from_wide(
                &buffer[..length as usize],
            )));
        }
        buffer.resize(length as usize + 1, 0);
    }
}

#[cfg(target_os = "linux")]
fn opened_handle_path(file: &File, _expected: &Path) -> io::Result<PathBuf> {
    use std::os::unix::io::AsRawFd;
    fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

#[cfg(target_os = "macos")]
fn opened_handle_path(file: &File, _expected: &Path) -> io::Result<PathBuf> {
    use std::{
        ffi::CStr,
        os::unix::{ffi::OsStrExt, io::AsRawFd},
    };
    let mut buffer = vec![0_i8; libc::PATH_MAX as usize];
    // SAFETY: F_GETPATH writes a NUL-terminated path to this PATH_MAX buffer
    // while `file` keeps the descriptor live.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, buffer.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    let bytes = unsafe { CStr::from_ptr(buffer.as_ptr()) }.to_bytes();
    Ok(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn opened_handle_path(_file: &File, expected: &Path) -> io::Result<PathBuf> {
    fs::canonicalize(expected)
}

#[cfg(windows)]
fn same_path_identity_portable(left: &Path, right: &Path) -> bool {
    same_path_identity(left, right)
}

#[cfg(not(windows))]
fn same_path_identity_portable(left: &Path, right: &Path) -> bool {
    left == right
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
    normalize_absolute(&joined).map_err(|error| format!("Invalid path {}: {error}", joined.display()))
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
                    format!("Could not verify restricted execution root {}: {error}", root.display())
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
            return fs::canonicalize(path)
                .map_err(|error| format!("Could not verify protected path {}: {error}", path.display()))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("Could not inspect protected path {}: {error}", path.display())),
    }

    let mut ancestor = path.parent();
    let existing_ancestor = loop {
        let Some(candidate) = ancestor else {
            return Err(format!("Protected path has no verifiable parent directory: {}", path.display()));
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
    let suffix = path
        .strip_prefix(existing_ancestor)
        .map_err(|_| format!("Could not resolve the relationship between protected path and parent directory: {}", path.display()))?;
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
        assert!(resolve_for_write(&workspace, "../new-outside.txt").is_err());
    }

    #[test]
    fn accepts_existing_and_new_paths_inside_workspace() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        fs::write(workspace.join("readme.md"), "hello").unwrap();

        assert!(resolve_existing(&workspace, "./readme.md").is_ok());
        let new_path = resolve_for_write(&workspace, "src/new.rs").unwrap();
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
    fn secure_write_installs_a_new_file_under_a_held_directory_authority() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let scope = ExecutionScope::workspace_only(&workspace);

        let authority =
            prepare_secure_write_with_scope(&workspace, "shots/page.png", &scope).unwrap();
        let target = authority.target().to_path_buf();
        assert!(target.parent().unwrap().is_dir());
        authority.install(b"captured-png").unwrap();

        assert_eq!(fs::read(target).unwrap(), b"captured-png");
    }

    #[cfg(unix)]
    #[test]
    fn unix_temporary_creation_and_failed_verification_use_the_held_parent() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let parent = workspace.join("shots");
        let moved_parent = workspace.join("shots-authorized");
        fs::create_dir_all(&parent).unwrap();
        let scope = ExecutionScope::workspace_only(&workspace);
        let parent_handle = open_verified_directory(&parent, &scope).unwrap();

        fs::rename(&parent, &moved_parent).unwrap();
        fs::create_dir(&parent).unwrap();

        let error = create_secure_temporary_file(&parent, &parent_handle, &scope)
            .expect_err("the pathname replacement must fail verification");

        assert!(
            error.contains("replacement") || error.contains("while authority was held"),
            "{error}"
        );
        assert_eq!(fs::read_dir(&parent).unwrap().count(), 0);
        assert_eq!(fs::read_dir(&moved_parent).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn unix_new_target_install_and_cleanup_stay_bound_to_the_held_parent() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let parent = workspace.join("shots");
        let moved_parent = workspace.join("shots-authorized");
        let temporary = parent.join(".mework-secure-write-test.tmp");
        let target = parent.join("page.png");
        fs::create_dir_all(&parent).unwrap();
        fs::write(&temporary, b"authorized").unwrap();
        let scope = ExecutionScope::workspace_only(&workspace);
        let parent_handle = open_verified_directory(&parent, &scope).unwrap();

        fs::rename(&parent, &moved_parent).unwrap();
        fs::create_dir(&parent).unwrap();
        fs::write(&temporary, b"attacker").unwrap();

        install_new_no_replace(&temporary, &target, &parent_handle).unwrap();
        assert_eq!(
            fs::read(moved_parent.join("page.png")).unwrap(),
            b"authorized"
        );
        assert!(!target.exists());

        remove_temporary_at(&parent_handle, &temporary);
        assert!(!moved_parent.join(".mework-secure-write-test.tmp").exists());
        assert_eq!(fs::read(&temporary).unwrap(), b"attacker");
    }

    #[test]
    fn secure_write_rejects_a_target_created_after_authority_preparation() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let scope = ExecutionScope::workspace_only(&workspace);
        let authority =
            prepare_secure_write_with_scope(&workspace, "shots/page.png", &scope).unwrap();
        let target = authority.target().to_path_buf();
        fs::write(&target, b"attacker").unwrap();

        let error = authority
            .install(b"captured-png")
            .expect_err("a late target must fail closed");

        assert!(
            error.contains("was created by another process") || error.contains("without replacement"),
            "{error}"
        );
        assert_eq!(fs::read(target).unwrap(), b"attacker");
    }

    #[test]
    fn secure_write_rejects_a_late_symlink_target_without_touching_its_destination() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let outside = root.path().join("outside.png");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(&outside, b"outside").unwrap();
        let scope = ExecutionScope::workspace_only(&workspace);
        let authority =
            prepare_secure_write_with_scope(&workspace, "shots/page.png", &scope).unwrap();
        let target = authority.target().to_path_buf();
        if link_file(&outside, &target).is_err() {
            return;
        }

        let error = authority
            .install(b"captured-png")
            .expect_err("a late symlink target must fail closed");

        assert!(
            error.contains("symlink") || error.contains("reparse point"),
            "{error}"
        );
        assert_eq!(fs::read(outside).unwrap(), b"outside");
    }

    #[test]
    fn secure_write_rejects_existing_reparse_components_even_when_scope_is_unrestricted() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let outside = root.path().join("outside");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let alias = workspace.join("shots");
        if link_directory(&outside, &alias).is_err() {
            return;
        }

        let error = match prepare_secure_write_with_scope(
            &workspace,
            "shots/page.png",
            &ExecutionScope::Unrestricted,
        ) {
            Err(error) => error,
            Ok(_) => panic!("an ancestor reparse point must be rejected"),
        };

        assert!(
            error.contains("reparse point") || error.contains("symlink"),
            "{error}"
        );
        assert!(!outside.join("page.png").exists());
    }

    #[test]
    fn secure_write_locks_or_detects_a_parent_replacement_before_install() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let outside = root.path().join("outside");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let scope = ExecutionScope::workspace_only(&workspace);
        let authority =
            prepare_secure_write_with_scope(&workspace, "shots/page.png", &scope).unwrap();
        let parent = authority.target().parent().unwrap().to_path_buf();
        let moved = workspace.join("shots-original");

        match fs::rename(&parent, &moved) {
            Err(_) => {
                #[cfg(windows)]
                {
                    // Windows directory handles intentionally omit
                    // FILE_SHARE_DELETE, so replacement must be rejected for
                    // the entire sidecar/import interval.
                    authority.install(b"captured-png").unwrap();
                    assert_eq!(fs::read(parent.join("page.png")).unwrap(), b"captured-png");
                }
                #[cfg(not(windows))]
                {
                    authority.install(b"captured-png").unwrap();
                }
            }
            Ok(()) => {
                if link_directory(&outside, &parent).is_err() {
                    fs::create_dir(&parent).unwrap();
                }
                let error = authority
                    .install(b"captured-png")
                    .expect_err("a replaced parent must fail closed");
                assert!(
                    error.contains("replacement")
                        || error.contains("reparse point")
                        || error.contains("outside trusted roots"),
                    "{error}"
                );
                assert!(!outside.join("page.png").exists());
            }
        }
    }

    #[test]
    fn secure_write_keeps_an_existing_target_handle_bound_during_install() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let outside = root.path().join("outside");
        fs::create_dir_all(workspace.join("shots")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let target = workspace.join("shots/page.png");
        fs::write(&target, b"old").unwrap();
        let scope = ExecutionScope::workspace_only(&workspace);
        let authority =
            prepare_secure_write_with_scope(&workspace, "shots/page.png", &scope).unwrap();
        let moved = outside.join("moved.png");

        match fs::rename(&target, &moved) {
            Err(_) => {
                authority.install(b"new").unwrap();
                assert_eq!(fs::read(target).unwrap(), b"new");
            }
            Ok(()) => {
                let error = authority
                    .install(b"new")
                    .expect_err("a moved existing target must fail closed");
                assert!(
                    error.contains("outside trusted roots")
                        || error.contains("replacement")
                        || error.contains("while authority was held"),
                    "{error}"
                );
                assert_eq!(fs::read(moved).unwrap(), b"old");
            }
        }
    }

    #[test]
    fn secure_write_staging_failure_preserves_the_complete_existing_target() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let target = workspace.join("shots/page.png");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        let old = b"complete-old-screenshot-bytes";
        fs::write(&target, old).unwrap();
        let scope = ExecutionScope::workspace_only(&workspace);
        let mut authority =
            prepare_secure_write_with_scope(&workspace, "shots/page.png", &scope).unwrap();
        authority.inject_temporary_write_failure_after(5);

        let error = authority
            .install(b"replacement-screenshot-bytes")
            .expect_err("injected staging failure must abort before the atomic commit");

        assert!(error.contains("Test injection"), "{error}");
        assert_eq!(fs::read(&target).unwrap(), old);
        let remaining = fs::read_dir(target.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(remaining, vec![target.file_name().unwrap()]);
    }

    #[test]
    fn secure_write_atomically_replaces_an_existing_target() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let target = workspace.join("shots/page.png");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, b"old-screenshot").unwrap();
        let scope = ExecutionScope::workspace_only(&workspace);
        let authority =
            prepare_secure_write_with_scope(&workspace, "shots/page.png", &scope).unwrap();

        authority.install(b"complete-new-screenshot").unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"complete-new-screenshot");
        let remaining = fs::read_dir(target.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(remaining, vec![target.file_name().unwrap()]);
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
