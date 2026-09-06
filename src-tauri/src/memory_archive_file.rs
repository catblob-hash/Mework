use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static ARCHIVE_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Reads one user-selected archive through the same opened regular-file handle
/// that is later previewed and imported. The path is never returned to the
/// renderer and symbolic-link/reparse targets fail closed.
pub fn read_bounded_nofollow(path: &Path, maximum_bytes: usize) -> Result<Vec<u8>, String> {
    read_bounded_nofollow_labeled(path, maximum_bytes, "记忆归档")
}

/// Same guarantees as [`read_bounded_nofollow`], with the noun used in error
/// messages supplied by the caller. Other file kinds get to inherit the
/// no-follow/regular-file/size checks without reporting themselves as memory
/// archives to the user.
pub fn read_bounded_nofollow_labeled(
    path: &Path,
    maximum_bytes: usize,
    label: &str,
) -> Result<Vec<u8>, String> {
    let mut file = open_read_nofollow_labeled(path, label)?;
    let metadata = file
        .metadata()
        .map_err(|_| format!("无法检查已打开的{label}文件"))?;
    validate_regular_metadata(&metadata, label)?;
    if metadata.len() > maximum_bytes as u64 {
        return Err(format!("{label}超过 {maximum_bytes} 字节上限"));
    }

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut file)
        .take(maximum_bytes.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| format!("无法读取{label}文件"))?;
    if bytes.len() > maximum_bytes {
        return Err(format!("{label}超过 {maximum_bytes} 字节上限"));
    }
    Ok(bytes)
}

/// Writes a user-selected plaintext archive through a no-follow regular-file
/// handle. Errors deliberately omit the selected path and archive contents.
pub fn write_all_nofollow(path: &Path, bytes: &[u8], maximum_bytes: usize) -> Result<(), String> {
    write_all_nofollow_labeled(path, bytes, maximum_bytes, "记忆归档")
}

/// Same guarantees as [`write_all_nofollow`], with a caller-supplied noun for
/// error messages.
pub fn write_all_nofollow_labeled(
    path: &Path,
    bytes: &[u8],
    maximum_bytes: usize,
    label: &str,
) -> Result<(), String> {
    if bytes.len() > maximum_bytes {
        return Err(format!("{label}超过 {maximum_bytes} 字节上限"));
    }
    let parent = validate_parent_labeled(path, label)?;
    validate_existing_target_labeled(path, label)?;
    let (temporary, mut file) = create_temporary_nofollow_labeled(&parent, path, label)?;
    let result = (|| {
        file.write_all(bytes)
            .map_err(|_| format!("无法写入{label}临时文件"))?;
        file.flush()
            .map_err(|_| format!("无法刷新{label}临时文件"))?;
        file.sync_all()
            .map_err(|_| format!("无法同步{label}临时文件"))?;
        drop(file);

        // Recheck the destination at the publication boundary. The temporary
        // file lives in the same directory, so replacement is one atomic
        // namespace operation and a crash cannot expose a partial archive.
        validate_existing_target_labeled(path, label)?;
        atomic_replace(&temporary, path, &parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Removes a file through the same no-follow discipline as the writer: a
/// symlink or reparse point at the target is refused rather than followed to
/// whatever it points at.
#[allow(dead_code)] // No-follow deletion primitive; kept beside the writer it mirrors.
pub fn remove_file_nofollow(path: &Path, label: &str) -> Result<(), String> {
    validate_parent_labeled(path, label)?;
    let metadata = fs::symlink_metadata(path).map_err(|_| format!("无法检查{label}文件"))?;
    validate_regular_metadata(&metadata, label)?;
    fs::remove_file(path).map_err(|_| format!("无法删除{label}文件"))
}

fn validate_parent_labeled(path: &Path, label: &str) -> Result<PathBuf, String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| format!("{label}目标缺少父目录"))?;
    if path
        .file_name()
        .is_none_or(|name| name.is_empty() || name == "." || name == "..")
    {
        return Err(format!("{label}目标缺少有效文件名"));
    }
    let metadata =
        fs::symlink_metadata(parent).map_err(|_| format!("无法检查{label}目标目录"))?;
    if !metadata.is_dir() || metadata_is_link(&metadata) {
        return Err(format!("{label}目标目录必须是普通的非链接目录"));
    }
    Ok(parent.to_path_buf())
}

fn validate_existing_target_labeled(path: &Path, label: &str) -> Result<(), String> {
    let target = format!("{label}目标");
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(format!("无法检查{target}")),
        Ok(metadata) => {
            validate_regular_metadata(&metadata, &target)?;
            let file = open_read_nofollow_labeled(path, label)?;
            let opened = file
                .metadata()
                .map_err(|_| format!("无法检查已打开的{target}"))?;
            validate_regular_metadata(&opened, &target)
        }
    }
}

fn create_temporary_nofollow_labeled(
    parent: &Path,
    destination: &Path,
    label: &str,
) -> Result<(PathBuf, File), String> {
    let file_name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("memory.json");
    for _ in 0..32 {
        let sequence = ARCHIVE_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = parent.join(format!(
            ".{file_name}.mework-memory-export-{}-{sequence}.tmp",
            std::process::id()
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        apply_nofollow_write_options(&mut options);
        match options.open(&temporary) {
            Ok(file) => {
                let metadata = file
                    .metadata()
                    .map_err(|_| format!("无法检查{label}临时文件"))?;
                validate_regular_metadata(&metadata, &format!("{label}临时文件"))?;
                return Ok((temporary, file));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(format!("无法创建{label}临时文件")),
        }
    }
    Err(format!("无法分配{label}临时文件"))
}

fn open_read_nofollow_labeled(path: &Path, label: &str) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    apply_nofollow_read_options(&mut options);
    options
        .open(path)
        .map_err(|_| format!("无法以非链接方式打开{label}文件"))
}

#[cfg(windows)]
fn apply_nofollow_read_options(options: &mut OpenOptions) {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    options
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .share_mode(FILE_SHARE_READ);
}

#[cfg(not(windows))]
fn apply_nofollow_read_options(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
}

#[cfg(windows)]
fn apply_nofollow_write_options(options: &mut OpenOptions) {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    options
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .share_mode(FILE_SHARE_READ);
}

#[cfg(windows)]
fn atomic_replace(temporary: &Path, destination: &Path, _parent: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source = temporary
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let target = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: both UTF-16 buffers are NUL-terminated and live for the call.
    // The destination has just passed the no-follow regular-file check.
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err("无法原子发布记忆归档文件".into())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn atomic_replace(temporary: &Path, destination: &Path, parent: &Path) -> Result<(), String> {
    fs::rename(temporary, destination).map_err(|_| "无法原子发布记忆归档文件".to_owned())?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| "无法同步记忆归档目标目录".to_owned())
}

#[cfg(not(windows))]
fn apply_nofollow_write_options(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
}

fn validate_regular_metadata(metadata: &fs::Metadata, label: &str) -> Result<(), String> {
    if !metadata.is_file() || metadata_is_link(metadata) {
        return Err(format!("{label}必须是普通的非链接文件"));
    }
    Ok(())
}

#[cfg(windows)]
fn metadata_is_link(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn metadata_is_link(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_regular_archive_and_enforces_bound() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("memory.json");
        write_all_nofollow(&path, b"{\"safe\":true}", 1024).unwrap();
        assert_eq!(
            read_bounded_nofollow(&path, 1024).unwrap(),
            b"{\"safe\":true}"
        );
        assert!(read_bounded_nofollow(&path, 4)
            .unwrap_err()
            .contains("上限"));
        assert!(write_all_nofollow(&path, b"12345", 4)
            .unwrap_err()
            .contains("上限"));
        assert_eq!(
            read_bounded_nofollow(&path, 1024).unwrap(),
            b"{\"safe\":true}"
        );
    }

    #[test]
    fn directories_are_never_accepted_as_archive_files() {
        let directory = tempfile::tempdir().unwrap();
        assert!(read_bounded_nofollow(directory.path(), 1024).is_err());
        assert!(write_all_nofollow(directory.path(), b"{}", 1024).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_targets_fail_closed_without_changing_the_target() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target.json");
        let link = directory.path().join("link.json");
        fs::write(&target, b"outside").unwrap();
        symlink(&target, &link).unwrap();

        assert!(read_bounded_nofollow(&link, 1024).is_err());
        assert!(write_all_nofollow(&link, b"changed", 1024).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"outside");
    }

    #[cfg(windows)]
    #[test]
    fn file_reparse_targets_fail_closed_when_symlinks_are_available() {
        use std::os::windows::fs::symlink_file;

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target.json");
        let link = directory.path().join("link.json");
        fs::write(&target, b"outside").unwrap();
        if symlink_file(&target, &link).is_err() {
            return;
        }

        assert!(read_bounded_nofollow(&link, 1024).is_err());
        assert!(write_all_nofollow(&link, b"changed", 1024).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"outside");
    }
}
