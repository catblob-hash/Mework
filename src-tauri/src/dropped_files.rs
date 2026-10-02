//! Native drag-and-drop onto the main window: what the renderer may learn
//! about, and read from, the paths being dragged.
//!
//! The renderer hears the dragged paths from Tauri's `onDragDropEvent`, but a
//! command that inspected or read whatever path the renderer named would be a
//! general "read any local file" capability. So the host records the paths of
//! the current native drag itself — `lib.rs` feeds the main window's
//! `DragDrop` events into [`DragDropSession`] — and the two commands answer
//! only for those: a hovered or dropped path may be probed, and only a dropped
//! one may be read.

use std::{
    collections::HashSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Mutex,
};

use base64::Engine as _;
use serde::Serialize;

use crate::file_attachments::{looks_like_pdf, MAX_DROPPED_FILE_BYTES};

/// How much of a file the probe reads to classify it.
const SNIFF_BYTES: usize = 8 * 1024;

/// The paths of the current native drag, as the window reported them.
#[derive(Debug, Default)]
pub struct DragDropSession {
    hovered: HashSet<PathBuf>,
    dropped: HashSet<PathBuf>,
}

impl DragDropSession {
    /// A new drag entered the window: it replaces whatever an earlier drag
    /// left, and nothing of it has been dropped yet.
    pub fn enter(&mut self, paths: &[PathBuf]) {
        self.hovered = paths.iter().cloned().collect();
        self.dropped.clear();
    }

    /// The drag was released over the window. Leave and Over change nothing:
    /// the renderer's probe for a hover may arrive after the pointer has left,
    /// and the dropped set must outlive the drop until the renderer has read it.
    pub fn drop_paths(&mut self, paths: &[PathBuf]) {
        self.hovered = paths.iter().cloned().collect();
        self.dropped = self.hovered.clone();
    }

    fn may_probe(&self, path: &Path) -> bool {
        self.hovered.contains(path) || self.dropped.contains(path)
    }

    fn may_read(&self, path: &Path) -> bool {
        self.dropped.contains(path)
    }
}

/// What the renderer needs to decide how to treat a dragged path before it is
/// dropped: whether it is a folder, and for a file, what kind of file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DroppedPathProbe {
    pub path: String,
    pub name: String,
    /// `file`, `directory`, `other`, or `missing`. Symlinks are followed.
    pub kind: &'static str,
    /// Byte size of a file; 0 for everything else.
    pub size: u64,
    /// For a file: `empty`, `image`, `pdf`, `text`, `binary`, or `unreadable`.
    /// For anything else: `none` (`unreadable` when even its metadata could
    /// not be read).
    pub sniff: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DroppedFile {
    pub name: String,
    /// The file's bytes, base64.
    pub data: String,
}

pub fn probe_paths(
    session: &Mutex<DragDropSession>,
    paths: &[String],
) -> Result<Vec<DroppedPathProbe>, String> {
    {
        let session = session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(stranger) = paths
            .iter()
            .find(|path| !session.may_probe(Path::new(path.as_str())))
        {
            return Err(format!("{stranger} 不在当前的拖放操作里，拒绝检查"));
        }
    }
    Ok(paths.iter().map(|path| probe_path(path)).collect())
}

pub fn read_dropped(session: &Mutex<DragDropSession>, path: &str) -> Result<DroppedFile, String> {
    let path_buf = PathBuf::from(path);
    if !session
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .may_read(&path_buf)
    {
        return Err(format!("{path} 不是本次拖放进来的文件，拒绝读取"));
    }
    let name = display_name(&path_buf, path);
    let too_large = || {
        format!(
            "拖放的文件 {name} 超过 {} MiB 上限",
            MAX_DROPPED_FILE_BYTES / 1024 / 1024
        )
    };
    let metadata =
        fs::metadata(&path_buf).map_err(|error| format!("无法读取拖放的文件 {name}：{error}"))?;
    if !metadata.is_file() {
        return Err(format!("拖放的 {name} 不是文件"));
    }
    if metadata.len() > MAX_DROPPED_FILE_BYTES as u64 {
        return Err(too_large());
    }
    let file =
        fs::File::open(&path_buf).map_err(|error| format!("无法读取拖放的文件 {name}：{error}"))?;
    if !file
        .metadata()
        .map_err(|error| format!("无法读取拖放的文件 {name}：{error}"))?
        .is_file()
    {
        return Err(format!("拖放的 {name} 不是文件"));
    }
    // The file may have grown since the size check; reading one byte past the
    // limit tells an exact-limit file from an oversized one.
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_DROPPED_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("无法读取拖放的文件 {name}：{error}"))?;
    if bytes.len() > MAX_DROPPED_FILE_BYTES {
        return Err(too_large());
    }
    if bytes.is_empty() {
        return Err(format!("拖放的文件 {name} 是空的"));
    }
    Ok(DroppedFile {
        name,
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
    })
}

fn display_name(path: &Path, fallback: &str) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| fallback.to_owned())
}

fn probe_path(path: &str) -> DroppedPathProbe {
    let path_buf = Path::new(path);
    let probe = |kind, size, sniff| DroppedPathProbe {
        path: path.to_owned(),
        name: display_name(path_buf, path),
        kind,
        size,
        sniff,
    };
    match fs::metadata(path_buf) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => probe("missing", 0, "none"),
        Err(_) => probe("other", 0, "unreadable"),
        Ok(metadata) if metadata.is_dir() => probe("directory", 0, "none"),
        Ok(metadata) if metadata.is_file() => {
            probe("file", metadata.len(), sniff_file(path_buf, metadata.len()))
        }
        Ok(_) => probe("other", 0, "none"),
    }
}

fn sniff_file(path: &Path, size: u64) -> &'static str {
    let mut head = Vec::with_capacity(SNIFF_BYTES);
    let read =
        fs::File::open(path).and_then(|file| file.take(SNIFF_BYTES as u64).read_to_end(&mut head));
    if read.is_err() {
        return "unreadable";
    }
    // `head.len() < size` means the window cut the file short.
    sniff_head(&head, (head.len() as u64) < size)
}

fn sniff_head(head: &[u8], truncated: bool) -> &'static str {
    if head.is_empty() {
        return "empty";
    }
    if crate::image_attachments::is_supported_image(head) {
        return "image";
    }
    if looks_like_pdf(head) {
        return "pdf";
    }
    if head.starts_with(b"\xEF\xBB\xBF")
        || head.starts_with(b"\xFF\xFE")
        || head.starts_with(b"\xFE\xFF")
    {
        return "text";
    }
    if head.contains(&0) {
        return "binary";
    }
    match std::str::from_utf8(head) {
        Ok(_) => "text",
        // A multi-byte character the window cut in half is still text; only a
        // file that really ends mid-character is not.
        Err(error) if error.error_len().is_none() && truncated => "text",
        Err(_) => "binary",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

    fn session_with(hovered: &[&Path], dropped: bool) -> Mutex<DragDropSession> {
        let paths = hovered
            .iter()
            .map(|path| path.to_path_buf())
            .collect::<Vec<_>>();
        let mut session = DragDropSession::default();
        session.enter(&paths);
        if dropped {
            session.drop_paths(&paths);
        }
        Mutex::new(session)
    }

    fn text(path: &Path) -> String {
        path.to_str().unwrap().to_owned()
    }

    #[test]
    fn sniffing_classifies_the_first_window() {
        assert_eq!(sniff_head(b"", false), "empty");
        assert_eq!(sniff_head(PNG_SIGNATURE, false), "image");
        assert_eq!(sniff_head(b"GIF89a....", false), "image");
        assert_eq!(sniff_head(b"%PDF-1.7\n\xff\x00binary", false), "pdf");
        let mut late_pdf = b"junk ".to_vec();
        late_pdf.extend_from_slice(b"%PDF-1.4");
        assert_eq!(sniff_head(&late_pdf, false), "pdf");
        assert_eq!(sniff_head("# 标题\nbody".as_bytes(), false), "text");
        assert_eq!(sniff_head(b"\xEF\xBB\xBFbom", false), "text");
        assert_eq!(sniff_head(b"\xFF\xFEh\0i\0", false), "text");
        assert_eq!(sniff_head(b"\xFE\xFF\0h\0i", false), "text");
        assert_eq!(sniff_head(b"ELF\0\x01\x02", false), "binary");
        assert_eq!(sniff_head(b"caf\xe9 au lait", false), "binary");

        // A character cut by the window is tolerated, a file ending in half of
        // one is not.
        let mut cut = "a".repeat(SNIFF_BYTES - 1).into_bytes();
        cut.push("中".as_bytes()[0]);
        assert_eq!(sniff_head(&cut, true), "text");
        assert_eq!(sniff_head(&cut, false), "binary");
    }

    #[test]
    fn probing_reports_kind_size_and_sniff_for_hovered_paths() {
        let temp = tempfile::tempdir().unwrap();
        let notes = temp.path().join("notes.md");
        fs::write(&notes, "# notes\n").unwrap();
        let empty = temp.path().join("empty.txt");
        fs::write(&empty, b"").unwrap();
        let blob = temp.path().join("blob.bin");
        fs::write(&blob, b"\0\x01\x02").unwrap();
        let folder = temp.path().join("folder");
        fs::create_dir(&folder).unwrap();
        let missing = temp.path().join("missing.txt");
        // A window-long file whose last character the window cuts.
        let long = temp.path().join("long.txt");
        let mut body = "a".repeat(SNIFF_BYTES - 1);
        body.push('中');
        fs::write(&long, &body).unwrap();

        let paths = [&notes, &empty, &blob, &folder, &missing, &long];
        let session = session_with(
            &paths.iter().map(|path| path.as_path()).collect::<Vec<_>>(),
            false,
        );
        let probes = probe_paths(
            &session,
            &paths.iter().map(|path| text(path)).collect::<Vec<_>>(),
        )
        .unwrap();
        let summary = probes
            .iter()
            .map(|probe| (probe.name.as_str(), probe.kind, probe.size, probe.sniff))
            .collect::<Vec<_>>();
        assert_eq!(
            summary,
            [
                ("notes.md", "file", 8, "text"),
                ("empty.txt", "file", 0, "empty"),
                ("blob.bin", "file", 3, "binary"),
                ("folder", "directory", 0, "none"),
                ("missing.txt", "missing", 0, "none"),
                ("long.txt", "file", body.len() as u64, "text"),
            ]
        );
        assert_eq!(probes[0].path, text(&notes));

        let serialized = serde_json::to_value(&probes[0]).unwrap();
        assert_eq!(
            serialized,
            serde_json::json!({
                "path": text(&notes),
                "name": "notes.md",
                "kind": "file",
                "size": 8,
                "sniff": "text",
            })
        );
    }

    #[cfg(unix)]
    #[test]
    fn probing_follows_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target.pdf");
        fs::write(&target, b"%PDF-1.7\n").unwrap();
        let link = temp.path().join("link.pdf");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let session = session_with(&[link.as_path()], true);
        let probe = probe_paths(&session, &[text(&link)]).unwrap().remove(0);
        assert_eq!((probe.kind, probe.sniff), ("file", "pdf"));
        assert_eq!(probe.name, "link.pdf");
        assert!(read_dropped(&session, &text(&link)).is_ok());
    }

    #[test]
    fn only_the_current_drag_may_be_probed_and_only_its_drop_read() {
        let temp = tempfile::tempdir().unwrap();
        let dragged = temp.path().join("dragged.txt");
        fs::write(&dragged, b"hello").unwrap();
        let other = temp.path().join("other.txt");
        fs::write(&other, b"secret").unwrap();

        let session = Mutex::new(DragDropSession::default());
        assert!(probe_paths(&session, &[text(&dragged)]).is_err());
        assert!(read_dropped(&session, &text(&dragged)).is_err());

        session.lock().unwrap().enter(&[dragged.clone()]);
        assert!(probe_paths(&session, &[text(&dragged)]).is_ok());
        assert!(probe_paths(&session, &[text(&dragged), text(&other)]).is_err());
        assert!(
            read_dropped(&session, &text(&dragged)).is_err(),
            "a hover is not a drop"
        );

        session.lock().unwrap().drop_paths(&[dragged.clone()]);
        let file = read_dropped(&session, &text(&dragged)).unwrap();
        assert_eq!(file.name, "dragged.txt");
        assert_eq!(file.data, "aGVsbG8=");
        assert!(read_dropped(&session, &text(&other)).is_err());

        // The next drag starts over: the previous drop is no longer readable.
        session.lock().unwrap().enter(&[other.clone()]);
        assert!(read_dropped(&session, &text(&dragged)).is_err());
        assert!(probe_paths(&session, &[text(&dragged)]).is_err());
    }

    #[test]
    fn reading_refuses_folders_empty_files_and_oversized_files() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("folder");
        fs::create_dir(&folder).unwrap();
        let empty = temp.path().join("empty.txt");
        fs::write(&empty, b"").unwrap();
        let oversized = temp.path().join("big.bin");
        fs::File::create(&oversized)
            .unwrap()
            .set_len(MAX_DROPPED_FILE_BYTES as u64 + 1)
            .unwrap();
        let exact = temp.path().join("exact.bin");
        fs::File::create(&exact)
            .unwrap()
            .set_len(MAX_DROPPED_FILE_BYTES as u64)
            .unwrap();
        let session = session_with(
            &[
                folder.as_path(),
                empty.as_path(),
                oversized.as_path(),
                exact.as_path(),
            ],
            true,
        );
        assert!(read_dropped(&session, &text(&folder))
            .unwrap_err()
            .contains("不是文件"));
        assert!(read_dropped(&session, &text(&empty))
            .unwrap_err()
            .contains("空的"));
        assert!(read_dropped(&session, &text(&oversized))
            .unwrap_err()
            .contains(&format!("{} MiB", MAX_DROPPED_FILE_BYTES / 1024 / 1024)));
        assert!(read_dropped(&session, &text(&exact)).is_ok());
    }
}
