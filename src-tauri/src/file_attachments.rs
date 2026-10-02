//! Content-addressed non-image attachments on user messages.
//!
//! Conversation JSON persists only [`FileAttachment`] metadata. The original
//! bytes live under `app_data/file-attachments/`, named by their SHA-256; a PDF
//! also owns `<id>.txt`, the text layer the renderer extracted with pdf.js at
//! upload, because the host has no PDF parser of its own. The model never sees
//! the bytes: [`FileAttachmentStore::model_text`] reads the text back, checked,
//! when `aisdk::step` hydrates a request, and [`render_for_model`] wraps it.
//!
//! Lifecycle (quarantine, restore, expiry, the orphan sweep) is the shared
//! [`ContentDirectory`] with the image store's semantics, under its own lock
//! and root; the companion text moves with its PDF.

use std::{collections::HashSet, path::Path, sync::Mutex};

use base64::Engine as _;

pub use crate::content_store::ReconcileReport;
use crate::content_store::{
    format_limit, hex_digest, is_sha256_hex, read_regular_file, ContentDirectory, Label,
};
use crate::model::{AppDocument, ContextItem, FileAttachment, FileAttachmentFormat};

/// Largest stored text file (documents from before [`MAX_TEXT_FILE_UPLOAD_BYTES`]
/// may hold one this large), and largest text a PDF may carry to the model.
pub const MAX_FILE_ATTACHMENT_TEXT_BYTES: usize = 512 * 1024;
/// Largest text file a message takes now: Claude Code attaches none over
/// 256 KB (`tengu_attachment_file_too_large`).
pub const MAX_TEXT_FILE_UPLOAD_BYTES: usize = 262_144;
/// Claude Code's Read reads a whole PDF up to 20 MB.
pub const MAX_FILE_ATTACHMENT_PDF_BYTES: usize = 20 * 1024 * 1024;
/// Text over this many tokens is cut to its first [`TRUNCATED_TEXT_FILE_LINES`]
/// lines, as Claude Code's Read cuts a file (its default output cap).
pub const MAX_TEXT_FILE_TOKENS: u64 = 25_000;
pub const TRUNCATED_TEXT_FILE_LINES: usize = 2_000;
/// A PDF of at most this many pages goes to a model that reads PDFs as the
/// document itself (Claude Code's whole-file threshold); a longer one, or one
/// for a model that does not, goes as its extracted text.
pub const MAX_NATIVE_PDF_PAGES: u32 = 10;
pub const MAX_FILE_ATTACHMENT_NAME_BYTES: usize = 256;
/// Largest file `dropped_file_read` hands to the renderer: the larger of what a
/// picture and a PDF may be.
pub const MAX_DROPPED_FILE_BYTES: usize =
    if crate::image_attachments::MAX_IMAGE_UPLOAD_BYTES > MAX_FILE_ATTACHMENT_PDF_BYTES {
        crate::image_attachments::MAX_IMAGE_UPLOAD_BYTES
    } else {
        MAX_FILE_ATTACHMENT_PDF_BYTES
    };
const MAX_PDF_PAGES: u32 = 100_000;
const MAX_FILE_ATTACHMENT_TOKENS: u64 = 10_000_000;
/// `%PDF-` may follow a little junk; readers accept it within the first KiB.
const PDF_MAGIC_WINDOW: usize = 1024;
const PDF_TEXT_SUFFIX: &str = ".txt";
const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";
const LABEL: Label = Label {
    noun: "file attachment",
    title: "File attachment",
};
static FILE_ATTACHMENT_FS_LOCK: Mutex<()> = Mutex::new(());

/// The AI SDK part `aisdk::project` emits for an attachment before hydration.
/// It is not an AI SDK type: it must be replaced before the step is sent, and
/// the history records it as-is instead of the file's text.
pub(crate) const FILE_PART_TYPE: &str = "mework-file";

#[derive(Clone, Debug)]
pub struct FileAttachmentStore {
    directory: ContentDirectory,
}

impl FileAttachmentStore {
    pub fn new(app_data: &Path) -> Self {
        Self {
            directory: ContentDirectory::new(
                app_data.join("file-attachments"),
                LABEL,
                &FILE_ATTACHMENT_FS_LOCK,
                &[PDF_TEXT_SUFFIX],
            ),
        }
    }

    /// Stores `bytes` and returns the reference a message carries.
    ///
    /// A PDF arrives with the text the renderer extracted (`text`) and its page
    /// count; a text file arrives with neither, since its bytes are its text.
    pub fn import(
        &self,
        name: &str,
        bytes: &[u8],
        format: FileAttachmentFormat,
        text: Option<&str>,
        pages: Option<u32>,
    ) -> Result<FileAttachment, String> {
        let name = validate_name(name)?;
        if bytes.is_empty() {
            return Err("File content is empty".into());
        }
        let extracted = match format {
            FileAttachmentFormat::Text => {
                if text.is_some() || pages.is_some() {
                    return Err(
                        "A text file attachment takes no extracted text or page count".into(),
                    );
                }
                if bytes.len() > MAX_TEXT_FILE_UPLOAD_BYTES {
                    return Err(format!(
                        "Text file exceeds the {} limit ({} bytes)",
                        format_limit(MAX_TEXT_FILE_UPLOAD_BYTES),
                        bytes.len()
                    ));
                }
                if let ModelText::TooLong = ModelText::of(decode_text_file(bytes)?) {
                    return Err(format!(
                        "Text file is too long: even its first {TRUNCATED_TEXT_FILE_LINES} lines exceed {MAX_TEXT_FILE_TOKENS} tokens"
                    ));
                }
                None
            }
            FileAttachmentFormat::Pdf => {
                if bytes.len() > MAX_FILE_ATTACHMENT_PDF_BYTES {
                    return Err(format!(
                        "PDF exceeds the {} limit ({} bytes)",
                        format_limit(MAX_FILE_ATTACHMENT_PDF_BYTES),
                        bytes.len()
                    ));
                }
                if !looks_like_pdf(bytes) {
                    return Err("File is not a PDF".into());
                }
                validate_pages(pages)?;
                let text =
                    text.ok_or_else(|| "A PDF attachment needs its extracted text".to_owned())?;
                validate_pdf_text(text)?;
                Some(text)
            }
        };

        let id = hex_digest(bytes);
        let _guard = self.directory.lock();
        self.directory.ensure_root(true)?;
        self.directory.restore_quarantined_locked(&id)?;
        // The companion is written before the original, so an original on disk
        // always has its text; a companion a crash left behind on its own is an
        // orphan the next sweep collects.
        let tokens = match extracted {
            None => estimate_text_tokens(bytes)?,
            Some(text) => crate::api::estimate_tokens(&self.store_pdf_text_locked(&id, text)?),
        };
        let target = self.directory.path_for_id(&id)?;
        match read_original(&target) {
            Ok(existing) => {
                if existing != bytes {
                    return Err(format!(
                        "File attachment {id} has a content-addressed file collision"
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                crate::storage::atomic_write(&target, bytes)
                    .map_err(|error| format!("Could not write file attachment: {error}"))?;
            }
            Err(error) => return Err(format!("Could not read file attachment: {error}")),
        }

        Ok(FileAttachment {
            id,
            name,
            format,
            bytes: bytes.len() as u64,
            tokens,
            pages: match format {
                FileAttachmentFormat::Text => None,
                FileAttachmentFormat::Pdf => pages,
            },
        })
    }

    /// Keeps the first valid text stored for a PDF. The same bytes extract to
    /// the same text, and the stored copy is what earlier messages already
    /// estimated and sent, so a later upload must not change it under them. A
    /// companion that no longer reads as valid text is replaced rather than
    /// left to fail every request that names the PDF.
    fn store_pdf_text_locked(&self, id: &str, text: &str) -> Result<String, String> {
        let path = self.directory.companion_path(id, PDF_TEXT_SUFFIX)?;
        match read_companion(&path) {
            Ok(existing) => {
                if let Ok(stored) = decode_pdf_text(existing) {
                    return Ok(stored);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "Could not read the extracted text of file attachment {id}: {error}"
                ))
            }
        }
        crate::storage::atomic_write(&path, text.as_bytes()).map_err(|error| {
            format!("Could not write the extracted text of file attachment: {error}")
        })?;
        Ok(text.to_owned())
    }

    /// A PDF's original bytes as a `data:` URL, for a model that reads the
    /// document itself. Checked against its metadata and digest like an image
    /// sidecar, since these are the bytes that are sent.
    pub fn pdf_data_url(&self, file: &FileAttachment) -> Result<String, String> {
        validate_file_metadata(file)?;
        if file.format != FileAttachmentFormat::Pdf {
            return Err(format!("File attachment {} is not a PDF", file.id));
        }
        let _guard = self.directory.lock();
        self.directory.restore_quarantined_locked(&file.id)?;
        let bytes = read_original(&self.directory.path_for_id(&file.id)?)
            .map_err(|error| format!("Could not read file attachment {}: {error}", file.id))?;
        if bytes.len() as u64 != file.bytes
            || hex_digest(&bytes) != file.id
            || !looks_like_pdf(&bytes)
        {
            return Err(format!(
                "File attachment {} failed its content integrity check",
                file.id
            ));
        }
        Ok(format!(
            "data:application/pdf;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ))
    }

    /// The original bytes as a `data:` URL, for the renderer's preview.
    pub fn data_url_by_id(&self, id: &str) -> Result<String, String> {
        validate_id(id)?;
        let _guard = self.directory.lock();
        self.directory.restore_quarantined_locked(id)?;
        let bytes = read_original(&self.directory.path_for_id(id)?)
            .map_err(|error| format!("Could not read file attachment {id}: {error}"))?;
        if bytes.is_empty()
            || bytes.len() > MAX_FILE_ATTACHMENT_PDF_BYTES
            || hex_digest(&bytes) != id
        {
            return Err(format!(
                "File attachment {id} failed its content integrity check"
            ));
        }
        let mime = if looks_like_pdf(&bytes) {
            "application/pdf"
        } else {
            "text/plain;charset=utf-8"
        };
        Ok(format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ))
    }

    /// The text the model reads for `file`, checked against its metadata.
    ///
    /// Restores from quarantine first: a request may be running from a
    /// snapshot taken before an edit dropped the reference.
    pub fn model_text(&self, file: &FileAttachment) -> Result<String, String> {
        validate_file_metadata(file)?;
        let _guard = self.directory.lock();
        self.directory.restore_quarantined_locked(&file.id)?;
        let original = self.directory.path_for_id(&file.id)?;
        match file.format {
            FileAttachmentFormat::Text => {
                let bytes =
                    read_regular_file(&original, MAX_FILE_ATTACHMENT_TEXT_BYTES, LABEL.title)
                        .map_err(|error| {
                            format!("Could not read file attachment {}: {error}", file.id)
                        })?;
                if bytes.len() as u64 != file.bytes {
                    return Err(format!(
                        "File attachment {} byte count does not match metadata",
                        file.id
                    ));
                }
                if hex_digest(&bytes) != file.id {
                    return Err(format!(
                        "File attachment {} failed its content integrity check",
                        file.id
                    ));
                }
                decode_text_file(&bytes).map(str::to_owned)
            }
            FileAttachmentFormat::Pdf => {
                // The text is what is sent, so it is what is read. The original
                // is only checked to exist at the recorded size: hashing up to
                // 10 MiB on every step of every turn would buy nothing the
                // companion's own checks do not.
                let metadata = std::fs::symlink_metadata(&original).map_err(|error| {
                    format!("Could not read file attachment {}: {error}", file.id)
                })?;
                if metadata.file_type().is_symlink()
                    || !metadata.is_file()
                    || metadata.len() != file.bytes
                {
                    return Err(format!(
                        "File attachment {} does not match its metadata",
                        file.id
                    ));
                }
                let companion = self.directory.companion_path(&file.id, PDF_TEXT_SUFFIX)?;
                let bytes = read_companion(&companion).map_err(|error| {
                    format!(
                        "Could not read the extracted text of file attachment {}: {error}",
                        file.id
                    )
                })?;
                decode_pdf_text(bytes).map_err(|error| {
                    format!(
                        "The extracted text of file attachment {} is invalid: {error}",
                        file.id
                    )
                })
            }
        }
    }

    /// See [`crate::image_attachments::ImageAttachmentStore::reconcile_transition`];
    /// the semantics are the same, per entry rather than per file.
    /// [`Self::reconcile_transition`] for callers that hold the referenced ids
    /// rather than two documents carrying every body: the document snapshot
    /// keeps no bodies, and `attachment_refs` answers for them. `next` must
    /// already include every pinned id.
    pub fn reconcile_referenced(
        &self,
        previous: &HashSet<String>,
        next: &HashSet<String>,
    ) -> Result<ReconcileReport, String> {
        self.directory.reconcile_transition(previous, next)
    }

    #[cfg(test)]
    pub fn reconcile_transition(
        &self,
        previous: &AppDocument,
        next: &AppDocument,
        pinned: &HashSet<String>,
    ) -> Result<ReconcileReport, String> {
        let previous_ids = referenced_file_ids(previous);
        let mut next_ids = referenced_file_ids(next);
        next_ids.extend(pinned.iter().cloned());
        self.directory
            .reconcile_transition(&previous_ids, &next_ids)
    }

    /// See [`crate::image_attachments::ImageAttachmentStore::reconcile_startup`].
    /// A caller that cannot read the pinned template ids must skip this rather
    /// than pass none: this is the pass that deletes.
    /// [`Self::reconcile_startup`] for a caller that holds the referenced ids:
    /// the startup load drops each body once it has noted what it references
    /// (`attachment_refs`). `referenced` must already include every pinned id.
    pub fn reconcile_startup_referenced(
        &self,
        referenced: &HashSet<String>,
    ) -> Result<ReconcileReport, String> {
        self.directory.reconcile_startup(referenced)
    }

    #[cfg(test)]
    pub fn reconcile_startup(
        &self,
        document: &AppDocument,
        pinned: &HashSet<String>,
    ) -> Result<ReconcileReport, String> {
        let mut referenced = referenced_file_ids(document);
        referenced.extend(pinned.iter().cloned());
        self.directory.reconcile_startup(&referenced)
    }

    /// Used only by the explicit full-document reset after its durability
    /// barrier and dependency fence have succeeded.
    pub fn purge_all(&self) -> Result<(), String> {
        let scope = self.pool_scope();
        crate::memory_pool::MemoryPool::global().retain(|key| {
            key.kind != crate::memory_pool::PoolKind::FileData || !key.id.starts_with(&scope)
        });
        self.directory.purge_all()
    }

    /// Keys of this directory's entries in the shared memory pool. An id names
    /// the same bytes forever, so a pooled copy never goes stale.
    fn pool_scope(&self) -> String {
        format!("{}\u{0}", self.directory.root().display())
    }

    /// [`Self::data_url_by_id`] through the shared memory pool, as low-priority
    /// data: only the preview reads it.
    pub fn pooled_data_url_by_id(&self, id: &str) -> Result<std::sync::Arc<String>, String> {
        let pool = crate::memory_pool::MemoryPool::global();
        let key = crate::memory_pool::PoolKey::new(
            crate::memory_pool::PoolKind::FileData,
            format!("{}{id}", self.pool_scope()),
        );
        if let Some(cached) = pool.get::<String>(&key) {
            return Ok(cached);
        }
        let value = std::sync::Arc::new(self.data_url_by_id(id)?);
        pool.insert(key, std::sync::Arc::clone(&value), value.len() as u64);
        Ok(value)
    }

    #[cfg(test)]
    fn path_for_id(&self, id: &str) -> Result<std::path::PathBuf, String> {
        self.directory.path_for_id(id)
    }
}

fn read_original(path: &Path) -> std::io::Result<Vec<u8>> {
    // Either format may own an id, so the larger bound applies.
    read_regular_file(path, MAX_FILE_ATTACHMENT_PDF_BYTES, LABEL.title)
}

fn read_companion(path: &Path) -> std::io::Result<Vec<u8>> {
    read_regular_file(path, MAX_FILE_ATTACHMENT_TEXT_BYTES, LABEL.title)
}

pub fn referenced_file_ids(document: &AppDocument) -> HashSet<String> {
    let mut ids = HashSet::new();
    for workspace in &document.workspaces {
        for conversation in &workspace.conversations {
            for message in &conversation.queued_messages {
                ids.extend(message.files.iter().map(|file| file.id.clone()));
            }
            collect_context_file_ids(&conversation.contexts, &mut ids);
            for branch in &conversation.branches {
                collect_context_file_ids(&branch.contexts, &mut ids);
            }
        }
    }
    ids
}

pub(crate) fn collect_context_file_ids(contexts: &[ContextItem], ids: &mut HashSet<String>) {
    for context in contexts {
        match context {
            ContextItem::User { files, .. } => {
                ids.extend(files.iter().map(|file| file.id.clone()));
            }
            ContextItem::Tool { subagent, .. } => {
                if let Some(subagent) = subagent {
                    collect_context_file_ids(&subagent.contexts, ids);
                }
            }
            ContextItem::System { .. }
            | ContextItem::Assistant { .. }
            | ContextItem::Reasoning { .. } => {}
        }
    }
}

/// The model-visible form of one attachment: its text inside an
/// `<attached_file>` element whose attributes say what it is.
///
/// The element is a convention, not markup anyone parses, so the only escaping
/// that matters is the kind that would let a file speak outside its element: a
/// name cannot leave its attribute, and the body cannot close the element
/// early.
pub(crate) fn render_for_model(file: &FileAttachment, text: &str) -> String {
    let name = escape_attribute(&file.name);
    let (open, text) = match file.format {
        FileAttachmentFormat::Text => match truncated_for_model(file, text) {
            // Claude Code's note, less its pointer to Read: the model has no
            // other copy of the file to read the rest from.
            Some(head) => (
                format!(
                    "<attached_file name=\"{name}\" note=\"The file was too large and has been truncated to the first {TRUNCATED_TEXT_FILE_LINES} lines. No need to mention the truncation.\">"
                ),
                head,
            ),
            None => (format!("<attached_file name=\"{name}\">"), text),
        },
        FileAttachmentFormat::Pdf => (
            format!(
                "<attached_file name=\"{name}\" type=\"pdf\" pages=\"{}\" content=\"text extracted from the PDF\">",
                file.pages.unwrap_or_default()
            ),
            text,
        ),
    };
    let body = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .unwrap_or(text)
        .replace("</attached_file", "<\\/attached_file");
    format!("{open}\n{body}\n</attached_file>")
}

/// The element that stands for a PDF sent as the document itself, which
/// follows it as a file part of its own.
pub(crate) fn render_native_pdf_marker(file: &FileAttachment) -> String {
    format!(
        "<attached_file name=\"{}\" type=\"pdf\" pages=\"{}\" content=\"the PDF document, attached after this element\"></attached_file>",
        escape_attribute(&file.name),
        file.pages.unwrap_or_default()
    )
}

/// What of a text file the model reads, by Claude Code's Read rule: the whole
/// file within [`MAX_TEXT_FILE_TOKENS`], else its first
/// [`TRUNCATED_TEXT_FILE_LINES`] lines if those are within it, else nothing.
enum ModelText<'a> {
    Whole,
    Truncated(&'a str),
    TooLong,
}

impl<'a> ModelText<'a> {
    fn of(text: &'a str) -> Self {
        if crate::api::estimate_tokens(text) <= MAX_TEXT_FILE_TOKENS {
            return Self::Whole;
        }
        let head = match text.match_indices('\n').nth(TRUNCATED_TEXT_FILE_LINES - 1) {
            Some((end, _)) => &text[..end],
            None => text,
        };
        if head.len() == text.len() || crate::api::estimate_tokens(head) > MAX_TEXT_FILE_TOKENS {
            return Self::TooLong;
        }
        Self::Truncated(head)
    }
}

/// The first lines of `text` when the file was attached under the truncation
/// rule. A file attached before it was stored whole, with the whole text's
/// token estimate, and keeps being sent whole, so a conversation's history
/// never changes under it; one attached since carries the estimate of the
/// lines it keeps, which is always less than the whole text's.
fn truncated_for_model<'a>(file: &FileAttachment, text: &'a str) -> Option<&'a str> {
    match ModelText::of(text) {
        ModelText::Truncated(head) if file.tokens < crate::api::estimate_tokens(text) => Some(head),
        _ => None,
    }
}

fn escape_attribute(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '"' => escaped.push_str("&quot;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            other => escaped.push(other),
        }
    }
    escaped
}

pub fn validate_file_metadata(file: &FileAttachment) -> Result<(), String> {
    validate_id(&file.id)?;
    validate_name(&file.name)?;
    match file.format {
        FileAttachmentFormat::Text => {
            if file.bytes == 0 || file.bytes > MAX_FILE_ATTACHMENT_TEXT_BYTES as u64 {
                return Err(format!(
                    "File attachment {} has an invalid byte count",
                    file.id
                ));
            }
            if file.pages.is_some() {
                return Err(format!(
                    "Text file attachment {} must not carry a page count",
                    file.id
                ));
            }
        }
        FileAttachmentFormat::Pdf => {
            if file.bytes == 0 || file.bytes > MAX_FILE_ATTACHMENT_PDF_BYTES as u64 {
                return Err(format!(
                    "File attachment {} has an invalid byte count",
                    file.id
                ));
            }
            validate_pages(file.pages)
                .map_err(|error| format!("File attachment {}: {error}", file.id))?;
        }
    }
    if file.tokens > MAX_FILE_ATTACHMENT_TOKENS {
        return Err(format!(
            "File attachment {} has an invalid token estimate",
            file.id
        ));
    }
    Ok(())
}

/// Validates the attachments of one message before it is persisted or admitted
/// to a live steer. How many a message carries is not budgeted.
pub fn validate_file_list(files: &[FileAttachment], owner: &str) -> Result<(), String> {
    let mut seen = HashSet::new();
    for file in files {
        validate_file_metadata(file)
            .map_err(|error| format!("{owner} has an invalid file attachment: {error}"))?;
        if !seen.insert(file.id.as_str()) {
            return Err(format!(
                "{owner} attaches file attachment {} more than once",
                file.id
            ));
        }
    }
    Ok(())
}

/// Whether `bytes` carries the PDF header where readers look for it.
pub(crate) fn looks_like_pdf(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(PDF_MAGIC_WINDOW)]
        .windows(5)
        .any(|window| window == b"%PDF-")
}

fn validate_id(id: &str) -> Result<(), String> {
    if !is_sha256_hex(id) {
        return Err("File attachment ID must be a full lowercase SHA-256 digest".into());
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty()
        || name.len() > MAX_FILE_ATTACHMENT_NAME_BYTES
        || name.chars().any(char::is_control)
    {
        return Err(format!(
            "File name must not be empty, contain control characters, or exceed {MAX_FILE_ATTACHMENT_NAME_BYTES} bytes"
        ));
    }
    Ok(name.to_owned())
}

fn validate_pages(pages: Option<u32>) -> Result<(), String> {
    match pages {
        Some(pages) if (1..=MAX_PDF_PAGES).contains(&pages) => Ok(()),
        _ => Err(format!(
            "A PDF attachment needs a page count within 1–{MAX_PDF_PAGES}"
        )),
    }
}

fn validate_pdf_text(text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("The PDF has no extractable text".into());
    }
    if text.len() > MAX_FILE_ATTACHMENT_TEXT_BYTES {
        return Err(format!(
            "The text extracted from the PDF exceeds the {} limit",
            format_limit(MAX_FILE_ATTACHMENT_TEXT_BYTES)
        ));
    }
    if text.contains('\0') {
        return Err("The text extracted from the PDF contains NUL characters".into());
    }
    Ok(())
}

/// A text file's model-visible text: UTF-8 without its BOM, and nothing that
/// makes it binary in disguise.
fn decode_text_file(bytes: &[u8]) -> Result<&str, String> {
    let body = bytes.strip_prefix(UTF8_BOM).unwrap_or(bytes);
    if body.contains(&0) {
        return Err("The file contains NUL bytes and is not a text file".into());
    }
    std::str::from_utf8(body).map_err(|_| "The file is not valid UTF-8 text".to_owned())
}

fn decode_pdf_text(bytes: Vec<u8>) -> Result<String, String> {
    let text = String::from_utf8(bytes).map_err(|_| "not valid UTF-8".to_owned())?;
    validate_pdf_text(&text)?;
    Ok(text)
}

/// The estimate of what the model reads of a text file: its first lines when
/// it is cut (see [`truncated_for_model`], which reads the estimate back).
fn estimate_text_tokens(bytes: &[u8]) -> Result<u64, String> {
    let text = decode_text_file(bytes)?;
    Ok(match ModelText::of(text) {
        ModelText::Truncated(head) => crate::api::estimate_tokens(head),
        ModelText::Whole | ModelText::TooLong => crate::api::estimate_tokens(text),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content_store::{ORPHAN_GRACE_PERIOD, QUARANTINE_RETENTION};
    use serde_json::json;
    use std::{
        fs,
        time::{Duration, SystemTime},
    };

    const PDF: &[u8] = b"%PDF-1.7\n1 0 obj << >> endobj\ntrailer << >>\n%%EOF\n";

    fn store() -> (tempfile::TempDir, FileAttachmentStore) {
        let temp = tempfile::tempdir().unwrap();
        let store = FileAttachmentStore::new(temp.path());
        (temp, store)
    }

    fn import_text(store: &FileAttachmentStore, name: &str, text: &str) -> FileAttachment {
        store
            .import(
                name,
                text.as_bytes(),
                FileAttachmentFormat::Text,
                None,
                None,
            )
            .unwrap()
    }

    fn import_pdf(store: &FileAttachmentStore, text: &str) -> FileAttachment {
        store
            .import(
                "report.pdf",
                PDF,
                FileAttachmentFormat::Pdf,
                Some(text),
                Some(3),
            )
            .unwrap()
    }

    fn set_age(path: &Path, age: Duration) {
        fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(SystemTime::now() - age)
            .unwrap();
    }

    #[test]
    fn a_text_file_is_content_addressed_and_read_back_without_its_bom() {
        let (_temp, store) = store();
        let first = store
            .import(
                " notes.md ",
                b"\xEF\xBB\xBF# Title\nbody\n",
                FileAttachmentFormat::Text,
                None,
                None,
            )
            .unwrap();
        assert_eq!(first.id, hex_digest(b"\xEF\xBB\xBF# Title\nbody\n"));
        assert_eq!(first.name, "notes.md");
        assert_eq!(first.format, FileAttachmentFormat::Text);
        assert_eq!(first.bytes, 16);
        assert_eq!(first.pages, None);
        assert_eq!(first.tokens, crate::api::estimate_tokens("# Title\nbody\n"));
        assert_eq!(store.model_text(&first).unwrap(), "# Title\nbody\n");

        let second = store
            .import(
                "renamed.md",
                b"\xEF\xBB\xBF# Title\nbody\n",
                FileAttachmentFormat::Text,
                None,
                None,
            )
            .unwrap();
        assert_eq!(second.id, first.id);
        assert_eq!(second.name, "renamed.md");
    }

    #[test]
    fn text_imports_reject_binary_invalid_or_oversized_content() {
        let (_temp, store) = store();
        let import = |bytes: &[u8]| {
            store
                .import("a.txt", bytes, FileAttachmentFormat::Text, None, None)
                .unwrap_err()
        };
        assert!(import(b"").contains("empty"));
        assert!(import(b"ab\0cd").contains("NUL"));
        assert!(import(b"\xff\xfe\x00a").contains("NUL"));
        assert!(import(b"caf\xe9").contains("UTF-8"));
        let oversized = "a\n".repeat(MAX_TEXT_FILE_UPLOAD_BYTES / 2 + 1);
        assert!(import(oversized.as_bytes()).contains("256 KiB"));
        let exact = "a\n".repeat(MAX_TEXT_FILE_UPLOAD_BYTES / 2);
        assert!(store
            .import(
                "a.txt",
                exact.as_bytes(),
                FileAttachmentFormat::Text,
                None,
                None
            )
            .is_ok());
        assert!(store
            .import(
                "a.txt",
                b"text",
                FileAttachmentFormat::Text,
                Some("extra"),
                None
            )
            .is_err());
        assert!(store
            .import("a.txt", b"text", FileAttachmentFormat::Text, None, Some(1))
            .is_err());
        for name in [
            "",
            "   ",
            "a\nb",
            &"n".repeat(MAX_FILE_ATTACHMENT_NAME_BYTES + 1),
        ] {
            assert!(store
                .import(name, b"text", FileAttachmentFormat::Text, None, None)
                .unwrap_err()
                .contains("File name"));
        }
    }

    #[test]
    fn a_pdf_needs_its_header_text_and_page_count() {
        let (_temp, store) = store();
        let pdf = |bytes: &[u8], text: Option<&str>, pages: Option<u32>| {
            store.import("a.pdf", bytes, FileAttachmentFormat::Pdf, text, pages)
        };
        assert!(pdf(b"not a pdf", Some("text"), Some(1))
            .unwrap_err()
            .contains("not a PDF"));
        let mut late_header = vec![b' '; PDF_MAGIC_WINDOW];
        late_header.extend_from_slice(b"%PDF-1.4");
        assert!(pdf(&late_header, Some("text"), Some(1)).is_err());
        let mut junk_first = b"junk\n".to_vec();
        junk_first.extend_from_slice(PDF);
        assert!(pdf(&junk_first, Some("text"), Some(1)).is_ok());

        assert!(pdf(PDF, None, Some(1))
            .unwrap_err()
            .contains("extracted text"));
        assert!(pdf(PDF, Some("  \n "), Some(1)).is_err());
        assert!(pdf(PDF, Some("a\0b"), Some(1)).unwrap_err().contains("NUL"));
        let long = "a".repeat(MAX_FILE_ATTACHMENT_TEXT_BYTES + 1);
        assert!(pdf(PDF, Some(&long), Some(1)).is_err());
        assert!(pdf(PDF, Some("text"), None).is_err());
        assert!(pdf(PDF, Some("text"), Some(0)).is_err());
        assert!(pdf(PDF, Some("text"), Some(MAX_PDF_PAGES + 1)).is_err());
        let oversized = {
            let mut bytes = PDF.to_vec();
            bytes.resize(MAX_FILE_ATTACHMENT_PDF_BYTES + 1, b' ');
            bytes
        };
        assert!(pdf(&oversized, Some("text"), Some(1))
            .unwrap_err()
            .contains("20 MiB"));

        let file = pdf(PDF, Some("第一页\n第二页"), Some(2)).unwrap();
        assert_eq!(file.format, FileAttachmentFormat::Pdf);
        assert_eq!(file.pages, Some(2));
        assert_eq!(file.bytes, PDF.len() as u64);
        assert_eq!(file.tokens, crate::api::estimate_tokens("第一页\n第二页"));
        assert_eq!(store.model_text(&file).unwrap(), "第一页\n第二页");
    }

    #[test]
    fn text_over_the_token_cap_is_cut_to_its_first_2000_lines_with_a_note() {
        let (_temp, store) = store();
        // 3000 lines of 40 characters: 30 750 tokens, 20 500 in the first 2000.
        let line = "x".repeat(40);
        let text = (1..=3_000)
            .map(|index| format!("{index:05}{}", &line[5..]))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(crate::api::estimate_tokens(&text) > MAX_TEXT_FILE_TOKENS);
        let file = import_text(&store, "big.log", &text);
        let head = text
            .lines()
            .take(TRUNCATED_TEXT_FILE_LINES)
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(file.tokens, crate::api::estimate_tokens(&head));
        let rendered = render_for_model(&file, &store.model_text(&file).unwrap());
        assert_eq!(
            rendered,
            format!(
                "<attached_file name=\"big.log\" note=\"The file was too large and has been truncated to the first 2000 lines. No need to mention the truncation.\">\n{head}\n</attached_file>"
            )
        );
        assert!(rendered.contains("\n02000xxx"));
        assert!(!rendered.contains("02001xxx"));

        // A file attached before the rule carries its whole text's estimate,
        // and is sent whole as it always was.
        let legacy = FileAttachment {
            tokens: crate::api::estimate_tokens(&text),
            ..file
        };
        assert!(render_for_model(&legacy, &text).contains("03000xxx"));
    }

    #[test]
    fn text_still_over_the_token_cap_in_its_first_2000_lines_is_refused() {
        let (_temp, store) = store();
        // 1500 lines of 80 characters: nothing to cut, and 30 375 tokens.
        let text = vec!["y".repeat(80); 1_500].join("\n");
        assert!(store
            .import(
                "wide.csv",
                text.as_bytes(),
                FileAttachmentFormat::Text,
                None,
                None
            )
            .unwrap_err()
            .contains("too long"));
        // Within the cap, a file of any line count is sent whole.
        let short = vec!["z"; 5_000].join("\n");
        let file = import_text(&store, "short.txt", &short);
        assert_eq!(file.tokens, crate::api::estimate_tokens(&short));
        assert!(!render_for_model(&file, &short).contains("note="));
    }

    #[test]
    fn a_pdf_reads_back_as_a_checked_data_url_for_native_delivery() {
        let (_temp, store) = store();
        let pdf = import_pdf(&store, "text");
        assert_eq!(
            store.pdf_data_url(&pdf).unwrap(),
            format!(
                "data:application/pdf;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(PDF)
            )
        );
        let text = import_text(&store, "a.txt", "hello");
        assert!(store.pdf_data_url(&text).unwrap_err().contains("not a PDF"));
        fs::write(store.path_for_id(&pdf.id).unwrap(), b"%PDF-1.7 tampered").unwrap();
        assert!(store.pdf_data_url(&pdf).unwrap_err().contains("integrity"));
        assert_eq!(
            render_native_pdf_marker(&FileAttachment {
                name: "a \"b\".pdf".into(),
                ..pdf
            }),
            "<attached_file name=\"a &quot;b&quot;.pdf\" type=\"pdf\" pages=\"3\" content=\"the PDF document, attached after this element\"></attached_file>"
        );
    }

    #[test]
    fn the_first_stored_pdf_text_wins() {
        let (_temp, store) = store();
        let first = import_pdf(&store, "original extraction");
        let second = import_pdf(&store, "a different, much longer extraction");
        assert_eq!(second.id, first.id);
        assert_eq!(second.tokens, first.tokens);
        assert_eq!(store.model_text(&second).unwrap(), "original extraction");

        // A companion that no longer reads as text is repaired, not kept.
        let companion = store
            .directory
            .companion_path(&first.id, PDF_TEXT_SUFFIX)
            .unwrap();
        fs::write(&companion, b"\xff\xfe").unwrap();
        assert!(store.model_text(&first).is_err());
        let repaired = import_pdf(&store, "fresh extraction");
        assert_eq!(store.model_text(&repaired).unwrap(), "fresh extraction");
    }

    #[test]
    fn data_urls_carry_the_sniffed_type_and_verify_the_digest() {
        let (_temp, store) = store();
        let text = import_text(&store, "a.txt", "hello");
        let pdf = import_pdf(&store, "pdf text");
        assert_eq!(
            store.data_url_by_id(&text.id).unwrap(),
            "data:text/plain;charset=utf-8;base64,aGVsbG8="
        );
        assert!(store
            .data_url_by_id(&pdf.id)
            .unwrap()
            .starts_with("data:application/pdf;base64,JVBERi"));
        assert!(store.data_url_by_id("not-an-id").is_err());
        assert!(store.data_url_by_id(&"0".repeat(64)).is_err());

        fs::write(store.path_for_id(&text.id).unwrap(), b"jello").unwrap();
        assert!(store
            .data_url_by_id(&text.id)
            .unwrap_err()
            .contains("integrity"));
    }

    #[test]
    fn model_text_refuses_tampered_or_mismatched_content() {
        let (_temp, store) = store();
        let file = import_text(&store, "a.txt", "hello");
        fs::write(store.path_for_id(&file.id).unwrap(), b"jello").unwrap();
        assert!(store.model_text(&file).unwrap_err().contains("integrity"));

        let file = import_text(&store, "b.txt", "world");
        let mut wrong_size = file.clone();
        wrong_size.bytes += 1;
        assert!(store.model_text(&wrong_size).is_err());

        // Metadata that claims a PDF for bytes that never had text stored.
        let mut forged = file.clone();
        forged.format = FileAttachmentFormat::Pdf;
        forged.pages = Some(1);
        assert!(store.model_text(&forged).is_err());

        let missing = FileAttachment {
            id: "a".repeat(64),
            ..file
        };
        assert!(store.model_text(&missing).is_err());
    }

    #[test]
    fn metadata_and_list_validation() {
        let valid = FileAttachment {
            id: "a".repeat(64),
            name: "a.txt".into(),
            format: FileAttachmentFormat::Text,
            bytes: 10,
            tokens: 3,
            pages: None,
        };
        assert!(validate_file_metadata(&valid).is_ok());
        let pdf = FileAttachment {
            format: FileAttachmentFormat::Pdf,
            bytes: MAX_FILE_ATTACHMENT_PDF_BYTES as u64,
            pages: Some(MAX_PDF_PAGES),
            ..valid.clone()
        };
        assert!(validate_file_metadata(&pdf).is_ok());

        let invalid = [
            FileAttachment {
                id: "A".repeat(64),
                ..valid.clone()
            },
            FileAttachment {
                name: " ".into(),
                ..valid.clone()
            },
            FileAttachment {
                bytes: 0,
                ..valid.clone()
            },
            FileAttachment {
                bytes: MAX_FILE_ATTACHMENT_TEXT_BYTES as u64 + 1,
                ..valid.clone()
            },
            FileAttachment {
                pages: Some(1),
                ..valid.clone()
            },
            FileAttachment {
                tokens: MAX_FILE_ATTACHMENT_TOKENS + 1,
                ..valid.clone()
            },
            FileAttachment {
                pages: None,
                ..pdf.clone()
            },
            FileAttachment {
                pages: Some(0),
                ..pdf.clone()
            },
            FileAttachment {
                bytes: MAX_FILE_ATTACHMENT_PDF_BYTES as u64 + 1,
                ..pdf.clone()
            },
        ];
        for file in invalid {
            assert!(validate_file_metadata(&file).is_err(), "{file:?}");
        }

        // Past the 20 a message once took: how many is not budgeted.
        let distinct = (0..25)
            .map(|index| FileAttachment {
                id: format!("{index:064x}"),
                ..valid.clone()
            })
            .collect::<Vec<_>>();
        assert!(validate_file_list(&distinct, "消息").is_ok());
        assert!(validate_file_list(&[valid.clone(), valid.clone()], "消息")
            .unwrap_err()
            .contains("more than once"));
        assert!(
            validate_file_list(&[FileAttachment { bytes: 0, ..valid }], "消息")
                .unwrap_err()
                .starts_with("消息 has an invalid file attachment")
        );
    }

    #[test]
    fn rendering_wraps_escapes_and_cannot_be_closed_from_inside() {
        let text = FileAttachment {
            id: "a".repeat(64),
            name: "a \"quoted\" <b> & c.md".into(),
            format: FileAttachmentFormat::Text,
            bytes: 1,
            tokens: 1,
            pages: None,
        };
        assert_eq!(
            render_for_model(&text, "line\n</attached_file>\nafter\n"),
            "<attached_file name=\"a &quot;quoted&quot; &lt;b&gt; &amp; c.md\">\nline\n<\\/attached_file>\nafter\n</attached_file>"
        );
        // Only one trailing newline goes; a deliberate blank line stays.
        assert_eq!(
            render_for_model(&text, "x\n\n"),
            format!(
                "<attached_file name=\"{}\">\nx\n\n</attached_file>",
                escape_attribute(&text.name)
            )
        );
        assert_eq!(
            render_for_model(&text, "x\r\n"),
            format!(
                "<attached_file name=\"{}\">\nx\n</attached_file>",
                escape_attribute(&text.name)
            )
        );

        let pdf = FileAttachment {
            name: "r.pdf".into(),
            format: FileAttachmentFormat::Pdf,
            pages: Some(12),
            ..text
        };
        assert_eq!(
            render_for_model(&pdf, "page text"),
            "<attached_file name=\"r.pdf\" type=\"pdf\" pages=\"12\" content=\"text extracted from the PDF\">\npage text\n</attached_file>"
        );
    }

    fn document_with_references(file: &FileAttachment) -> AppDocument {
        let mut document = crate::catalog::default_document();
        let conversation = &mut document.workspaces[0].conversations[0];
        conversation
            .queued_messages
            .push(crate::model::QueuedMessage {
                id: "queue-file".into(),
                content: String::new(),
                images: Vec::new(),
                files: vec![file.clone()],
                created_at: "2026-07-24T00:00:00Z".into(),
            });
        conversation.contexts = vec![serde_json::from_value(json!({
            "kind": "tool",
            "id": "tool-agent",
            "toolName": "agent_spawn",
            "input": {},
            "result": {
                "success": true,
                "output": "done",
                "executedAt": "2026-07-24T00:00:02Z",
                "durationMs": 1
            },
            "subagent": {
                "task": "inspect",
                "status": "completed",
                "contexts": [{
                    "kind": "user",
                    "id": "child-file",
                    "content": "",
                    "files": [file],
                    "createdAt": "2026-07-24T00:00:03Z"
                }],
                "updates": []
            },
            "createdAt": "2026-07-24T00:00:02Z"
        }))
        .unwrap()];
        conversation
            .branches
            .push(crate::model::ConversationBranch {
                id: "branch-file".into(),
                fork_context_id: "tool-agent".into(),
                active: false,
                contexts: vec![serde_json::from_value(json!({
                    "kind": "user",
                    "id": "branch-user-file",
                    "content": "",
                    "files": [file],
                    "createdAt": "2026-07-24T00:00:06Z"
                }))
                .unwrap()],
                created_at: "2026-07-24T00:00:06Z".into(),
                updated_at: "2026-07-24T00:00:06Z".into(),
            });
        document
    }

    #[test]
    fn reference_scan_covers_queue_branches_and_subagents() {
        let (_temp, store) = store();
        let file = import_text(&store, "a.txt", "hello");
        let document = document_with_references(&file);
        assert_eq!(
            referenced_file_ids(&document),
            HashSet::from([file.id.clone()])
        );

        // Each carrier on its own is enough.
        let mut queue_only = document.clone();
        queue_only.workspaces[0].conversations[0].contexts.clear();
        queue_only.workspaces[0].conversations[0].branches.clear();
        assert_eq!(referenced_file_ids(&queue_only).len(), 1);
        let mut branch_only = document.clone();
        branch_only.workspaces[0].conversations[0].contexts.clear();
        branch_only.workspaces[0].conversations[0]
            .queued_messages
            .clear();
        assert_eq!(referenced_file_ids(&branch_only).len(), 1);
        let mut subagent_only = document;
        subagent_only.workspaces[0].conversations[0]
            .branches
            .clear();
        subagent_only.workspaces[0].conversations[0]
            .queued_messages
            .clear();
        assert_eq!(referenced_file_ids(&subagent_only).len(), 1);
    }

    #[test]
    fn a_dropped_reference_quarantines_the_pdf_with_its_text_and_a_read_restores_both() {
        let (_temp, store) = store();
        let pdf = import_pdf(&store, "the text layer");
        let previous = document_with_references(&pdf);
        let next = crate::catalog::default_document();

        let report = store
            .reconcile_transition(&previous, &next, &HashSet::new())
            .unwrap();
        assert_eq!(report.quarantined, 1, "one entry, whatever its file count");
        let directory = &store.directory;
        assert!(!directory.path_for_id(&pdf.id).unwrap().exists());
        assert!(!directory
            .companion_path(&pdf.id, PDF_TEXT_SUFFIX)
            .unwrap()
            .exists());
        assert!(directory.quarantine_path_for_id(&pdf.id).unwrap().exists());
        assert!(directory
            .quarantine_companion_path(&pdf.id, PDF_TEXT_SUFFIX)
            .unwrap()
            .exists());

        // A request still holding the old snapshot reads it back whole.
        assert_eq!(store.model_text(&pdf).unwrap(), "the text layer");
        assert!(directory.path_for_id(&pdf.id).unwrap().exists());
        assert!(directory
            .companion_path(&pdf.id, PDF_TEXT_SUFFIX)
            .unwrap()
            .exists());

        // A reference coming back restores what a transition quarantined.
        store
            .reconcile_transition(&previous, &next, &HashSet::new())
            .unwrap();
        let report = store
            .reconcile_transition(&next, &previous, &HashSet::new())
            .unwrap();
        assert_eq!(report.restored, 1);
        assert!(directory
            .companion_path(&pdf.id, PDF_TEXT_SUFFIX)
            .unwrap()
            .exists());
    }

    #[test]
    fn a_half_quarantined_entry_is_restored_whole() {
        let (_temp, store) = store();
        let pdf = import_pdf(&store, "text");
        let directory = &store.directory;
        // As a crash between the two renames would leave it.
        let quarantine = directory.quarantine_path_for_id(&pdf.id).unwrap();
        fs::create_dir_all(quarantine.parent().unwrap()).unwrap();
        fs::rename(
            directory.companion_path(&pdf.id, PDF_TEXT_SUFFIX).unwrap(),
            directory
                .quarantine_companion_path(&pdf.id, PDF_TEXT_SUFFIX)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(store.model_text(&pdf).unwrap(), "text");
    }

    #[test]
    fn startup_deletes_expired_entries_whole_and_spares_pinned_ones() {
        let (_temp, store) = store();
        let kept = import_pdf(&store, "kept text");
        let dropped = import_text(&store, "gone.txt", "gone");
        let mut referenced = document_with_references(&kept);
        referenced.workspaces[0].conversations[0].queued_messages[0]
            .files
            .push(dropped.clone());
        let empty = crate::catalog::default_document();
        store
            .reconcile_transition(&referenced, &empty, &HashSet::new())
            .unwrap();
        let directory = &store.directory;
        for path in [
            directory.quarantine_path_for_id(&kept.id).unwrap(),
            directory
                .quarantine_companion_path(&kept.id, PDF_TEXT_SUFFIX)
                .unwrap(),
            directory.quarantine_path_for_id(&dropped.id).unwrap(),
        ] {
            set_age(&path, QUARANTINE_RETENTION + Duration::from_secs(1));
        }

        let pinned = HashSet::from([kept.id.clone()]);
        let report = store.reconcile_startup(&empty, &pinned).unwrap();
        assert_eq!(report.restored, 1);
        assert_eq!(report.deleted, 1);
        assert_eq!(store.model_text(&kept).unwrap(), "kept text");
        assert!(!directory
            .quarantine_path_for_id(&dropped.id)
            .unwrap()
            .exists());
        assert!(store.model_text(&dropped).is_err());
    }

    #[test]
    fn startup_quarantines_an_old_unreferenced_import_with_its_companion() {
        let (_temp, store) = store();
        let pdf = import_pdf(&store, "text");
        let directory = &store.directory;
        let original = directory.path_for_id(&pdf.id).unwrap();
        let companion = directory.companion_path(&pdf.id, PDF_TEXT_SUFFIX).unwrap();
        // One old file is not enough: every file of the entry has to be.
        set_age(&original, ORPHAN_GRACE_PERIOD + Duration::from_secs(1));
        let empty = crate::catalog::default_document();
        let report = store.reconcile_startup(&empty, &HashSet::new()).unwrap();
        assert_eq!(report.quarantined, 0);

        set_age(&companion, ORPHAN_GRACE_PERIOD + Duration::from_secs(1));
        let report = store.reconcile_startup(&empty, &HashSet::new()).unwrap();
        assert_eq!(report.quarantined, 1);
        assert_eq!(report.deleted, 0);
        assert!(!original.exists());
        assert!(!companion.exists());
    }

    #[test]
    fn purge_removes_only_the_file_attachment_root() {
        let (temp, store) = store();
        import_text(&store, "a.txt", "hello");
        let images = crate::image_attachments::ImageAttachmentStore::new(temp.path());
        let unrelated = temp.path().join("unrelated.txt");
        fs::write(&unrelated, b"keep").unwrap();
        store.purge_all().unwrap();
        assert!(!store.directory.root().exists());
        assert_eq!(fs::read(&unrelated).unwrap(), b"keep");
        assert!(images.purge_all().is_ok());
    }
}
