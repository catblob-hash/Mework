//! Staging for the built-in browser's "open file" entry.
//!
//! The page WebView admits only `http`/`https`, so a picked file is copied alone into a throwaway
//! directory that WebView2 maps onto a virtual host name. Mapping a copy rather than the file's own
//! directory is what keeps a previewed HTML document from reading — and exfiltrating — its siblings.

use std::{
    fs,
    path::{Path, PathBuf},
};

use url::Url;

/// RFC 6761 reserves `.invalid`, so this name can never belong to a real site. The dotted form also
/// avoids the single-label host names Chromium resolves as search terms instead of as a mapping.
pub(crate) const PREVIEW_VIRTUAL_HOST: &str = "mework-file-preview.invalid";

pub(crate) const PREVIEWABLE_EXTENSIONS: [&str; 10] = [
    "html", "htm", "svg", "png", "jpg", "jpeg", "gif", "webp", "avif", "pdf",
];

const STAGING_ROOT: &str = "file-previews";
const MAX_PREVIEW_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// `SetVirtualHostNameToFolderMapping` documents MAX_PATH as the limit for the mapped folder.
const MAX_MAPPED_FOLDER_CHARS: usize = 260;
const MAX_PREVIEW_STEM_CHARS: usize = 64;
const SWEEP_MAX_DIRECTORIES: usize = 64;

pub(crate) struct StagedFilePreview {
    pub(crate) directory: PathBuf,
    pub(crate) url: Url,
}

/// Validates a user-picked path and copies it alone into a fresh staging directory.
pub(crate) fn stage(app_data: &Path, picked: &Path) -> Result<StagedFilePreview, String> {
    let extension = validate_picked_file(picked)?;
    let name = sanitized_preview_name(picked, &extension);
    let url = preview_url(&name)?;
    let directory = create_staging_directory(app_data)?;
    if let Err(error) = fs::copy(picked, directory.join(&name)) {
        discard(&directory);
        return Err(format!("无法准备本地文件预览副本: {error}"));
    }
    Ok(StagedFilePreview { directory, url })
}

/// Deletes one staging directory. Idempotent.
///
/// A refusal is swallowed because WebView2 can still hold the outgoing document's copy open; the
/// startup sweep collects whatever is left behind.
pub(crate) fn discard(directory: &Path) {
    let _ = fs::remove_dir_all(directory);
}

/// Removes staging directories a crash left behind.
///
/// Bounded the way the browser-profile startup sweep is: a pathological directory count must not
/// stall startup, and the remainder is collected by the next launch.
pub(crate) fn sweep_orphans(app_data: &Path) {
    let root = app_data.join(STAGING_ROOT);
    let Ok(metadata) = fs::symlink_metadata(&root) else {
        return;
    };
    if is_link_like(&metadata) || !metadata.is_dir() {
        return;
    }
    let Ok(entries) = fs::read_dir(&root) else {
        return;
    };
    for entry in entries.take(SWEEP_MAX_DIRECTORIES) {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if is_link_like(&metadata) {
            let _ = remove_link_entry(&path, &metadata);
        } else if metadata.is_dir() {
            let _ = fs::remove_dir_all(&path);
        } else {
            let _ = fs::remove_file(&path);
        }
    }
}

fn validate_picked_file(picked: &Path) -> Result<String, String> {
    let metadata =
        fs::symlink_metadata(picked).map_err(|error| format!("无法读取所选文件: {error}"))?;
    if is_link_like(&metadata) {
        return Err("拒绝预览链接形式的文件".to_owned());
    }
    let canonical =
        fs::canonicalize(picked).map_err(|error| format!("无法解析所选文件路径: {error}"))?;
    let canonical = local_volume_path(&canonical)?;
    let metadata =
        fs::metadata(&canonical).map_err(|error| format!("无法读取所选文件: {error}"))?;
    if !metadata.is_file() {
        return Err("只能预览普通文件".to_owned());
    }
    if metadata.len() > MAX_PREVIEW_FILE_BYTES {
        return Err(format!(
            "所选文件超过 {} MiB 的本地预览上限",
            MAX_PREVIEW_FILE_BYTES / (1024 * 1024)
        ));
    }
    let extension = canonical
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    // The dialog filter is advisory — the Windows name box accepts a typed `*.*` — so this is the
    // gate that actually decides what the built-in browser will display.
    if !PREVIEWABLE_EXTENSIONS.contains(&extension.as_str()) {
        return Err(format!(
            "内置浏览器只能预览这些类型的文件: {}",
            PREVIEWABLE_EXTENSIONS.join("、")
        ));
    }
    Ok(extension)
}

fn sanitized_preview_name(picked: &Path, extension: &str) -> String {
    let stem = picked
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();
    let mut sanitized = String::new();
    for character in stem.chars() {
        if sanitized.len() >= MAX_PREVIEW_STEM_CHARS {
            break;
        }
        if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
            sanitized.push(character);
        } else if !sanitized.ends_with('_') {
            sanitized.push('_');
        }
    }
    let stem = sanitized.trim_matches('_');
    if stem.is_empty() {
        format!("preview.{extension}")
    } else {
        format!("{stem}.{extension}")
    }
}

fn preview_url(name: &str) -> Result<Url, String> {
    let mut url = Url::parse(&format!("https://{PREVIEW_VIRTUAL_HOST}/"))
        .map_err(|error| format!("无法构造本地文件预览地址: {error}"))?;
    url.set_path(name);
    // Nothing in this module may widen what the page WebView admits; the minted address has to
    // clear the same gate the address bar does.
    if !crate::browser::is_navigation_allowed(&url) {
        return Err("本地文件预览地址未通过内置浏览器的导航许可".to_owned());
    }
    Ok(url)
}

fn create_staging_directory(app_data: &Path) -> Result<PathBuf, String> {
    let root = app_data.join(STAGING_ROOT);
    fs::create_dir_all(&root).map_err(|error| format!("无法创建本地文件预览根目录: {error}"))?;
    let directory = root.join(uuid::Uuid::new_v4().simple().to_string());
    fs::create_dir(&directory).map_err(|error| format!("无法创建本地文件预览目录: {error}"))?;
    let canonical = match fs::canonicalize(&directory) {
        Ok(canonical) => canonical,
        Err(error) => {
            discard(&directory);
            return Err(format!("无法验证本地文件预览目录: {error}"));
        }
    };
    let canonical = match local_volume_path(&canonical) {
        Ok(canonical) => canonical,
        Err(error) => {
            discard(&directory);
            return Err(error);
        }
    };
    if canonical.as_os_str().len() > MAX_MAPPED_FOLDER_CHARS {
        discard(&canonical);
        return Err("本地文件预览目录路径超出 WebView2 的 MAX_PATH 上限".to_owned());
    }
    Ok(canonical)
}

/// Drops Windows' `\\?\` verbatim prefix and refuses network locations.
///
/// WebView2 rejects a UNC folder mapping outright, and canonicalization is what turns an ordinary
/// drive path into the verbatim form that the mapping API does not understand either.
#[cfg(windows)]
fn local_volume_path(path: &Path) -> Result<PathBuf, String> {
    let text = path
        .to_str()
        .ok_or_else(|| "本地文件预览路径不是有效的 Unicode".to_owned())?;
    if text.starts_with(r"\\?\UNC\") {
        return Err("拒绝预览网络位置上的文件".to_owned());
    }
    let plain = text.strip_prefix(r"\\?\").unwrap_or(text);
    if plain.starts_with(r"\\") {
        return Err("拒绝预览网络位置上的文件".to_owned());
    }
    Ok(PathBuf::from(plain))
}

#[cfg(not(windows))]
fn local_volume_path(path: &Path) -> Result<PathBuf, String> {
    Ok(path.to_path_buf())
}

#[cfg(not(windows))]
fn is_link_like(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
fn is_link_like(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn remove_link_entry(path: &Path, _metadata: &fs::Metadata) -> std::io::Result<()> {
    fs::remove_file(path)
}

#[cfg(windows)]
fn remove_link_entry(path: &Path, metadata: &fs::Metadata) -> std::io::Result<()> {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0010;
    if metadata.file_attributes() & FILE_ATTRIBUTE_DIRECTORY != 0 {
        fs::remove_dir(path)
    } else {
        fs::remove_file(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_picked(directory: &Path, name: &str) -> PathBuf {
        let path = directory.join(name);
        fs::write(&path, b"preview fixture").unwrap();
        path
    }

    #[test]
    fn every_filtered_extension_is_staged_as_a_lone_copy() {
        let app_data = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();

        for extension in PREVIEWABLE_EXTENSIONS {
            let picked = write_picked(source.path(), &format!("report.{extension}"));
            let staged = stage(app_data.path(), &picked).unwrap();
            assert_eq!(
                staged.url.as_str(),
                format!("https://{PREVIEW_VIRTUAL_HOST}/report.{extension}")
            );
            let entries = fs::read_dir(&staged.directory)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            assert_eq!(entries, vec![format!("report.{extension}")]);
        }
    }

    #[test]
    fn extensions_outside_the_filter_are_refused() {
        let app_data = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();

        for name in ["payload.exe", "notes.txt", "readme.md", "noextension"] {
            let picked = write_picked(source.path(), name);
            assert!(
                stage(app_data.path(), &picked).is_err(),
                "unexpectedly staged {name}"
            );
        }
    }

    #[test]
    fn an_oversize_file_is_refused_before_anything_is_copied() {
        let app_data = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let picked = source.path().join("huge.png");
        let file = fs::File::create(&picked).unwrap();
        file.set_len(MAX_PREVIEW_FILE_BYTES + 1).unwrap();
        drop(file);

        assert!(stage(app_data.path(), &picked).is_err());
        assert!(!app_data.path().join(STAGING_ROOT).exists());
    }

    #[test]
    fn a_directory_is_not_a_previewable_file() {
        let app_data = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let picked = source.path().join("bundle.html");
        fs::create_dir(&picked).unwrap();

        assert_eq!(
            stage(app_data.path(), &picked).err(),
            Some("只能预览普通文件".to_owned())
        );
    }

    #[test]
    fn the_minted_url_clears_the_page_navigation_gate() {
        let url = preview_url("report.pdf").unwrap();
        assert!(crate::browser::is_navigation_allowed(&url));
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some(PREVIEW_VIRTUAL_HOST));
    }

    #[test]
    fn awkward_file_names_become_an_ascii_safe_single_segment() {
        assert_eq!(
            sanitized_preview_name(Path::new("C:/tmp/我的 报告 v2.html"), "html"),
            "v2.html"
        );
        assert_eq!(
            sanitized_preview_name(Path::new("C:/tmp/..html"), "html"),
            "preview.html"
        );
        let long = "a".repeat(MAX_PREVIEW_STEM_CHARS + 20);
        assert_eq!(
            sanitized_preview_name(Path::new(&format!("C:/tmp/{long}.png")), "png"),
            format!("{}.png", "a".repeat(MAX_PREVIEW_STEM_CHARS))
        );
    }

    #[test]
    fn discarding_a_staging_directory_is_idempotent() {
        let app_data = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let picked = write_picked(source.path(), "page.html");
        let staged = stage(app_data.path(), &picked).unwrap();

        discard(&staged.directory);
        assert!(!staged.directory.exists());
        discard(&staged.directory);
        assert!(!staged.directory.exists());
        assert!(picked.is_file());
    }

    #[test]
    fn the_orphan_sweep_is_bounded_and_leaves_the_remainder_for_the_next_launch() {
        let app_data = tempfile::tempdir().unwrap();
        let root = app_data.path().join(STAGING_ROOT);
        let remainder = 3;
        fs::create_dir_all(&root).unwrap();
        for index in 0..SWEEP_MAX_DIRECTORIES + remainder {
            let directory = root.join(format!("{index:032x}"));
            fs::create_dir(&directory).unwrap();
            fs::write(directory.join("staged.png"), b"leftover").unwrap();
        }

        sweep_orphans(app_data.path());
        assert_eq!(fs::read_dir(&root).unwrap().count(), remainder);

        sweep_orphans(app_data.path());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    }

    #[test]
    fn sweeping_a_missing_root_does_nothing() {
        let app_data = tempfile::tempdir().unwrap();
        sweep_orphans(app_data.path());
        assert!(!app_data.path().join(STAGING_ROOT).exists());
    }
}
