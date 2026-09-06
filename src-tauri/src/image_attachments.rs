//! Content-addressed image attachments shared by user input and tool results.
//!
//! Conversation JSON persists only [`ImageAttachment`] metadata. The bytes live
//! under `app_data/image-attachments/` and are verified again immediately before
//! a provider request is hydrated.

use std::{
    collections::HashSet,
    fs,
    io::{Cursor, Read, Write},
    num::NonZeroU64,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, SystemTime},
};

use base64::Engine as _;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::model::{AppDocument, ContextItem, ImageAttachment};

pub const MAX_IMAGE_ATTACHMENT_BYTES: usize = 5 * 1024 * 1024;
pub const MAX_IMAGE_ATTACHMENT_NAME_BYTES: usize = 256;
pub const MAX_IMAGE_ATTACHMENT_DIMENSION: u32 = 8_000;
pub const MAX_IMAGE_ATTACHMENT_PIXELS: u64 = 16 * 1024 * 1024;
pub const MAX_REQUEST_IMAGES: usize = 20;
pub const MAX_REQUEST_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
pub const MAX_REQUEST_IMAGE_PIXELS: u64 = 64 * 1024 * 1024;
const MAX_DECODED_IMAGE_BYTES: usize = (MAX_IMAGE_ATTACHMENT_PIXELS as usize) * 4;
const MAX_DECODER_WORKING_BYTES: usize = MAX_DECODED_IMAGE_BYTES + 32 * 1024 * 1024;
const PLACEHOLDER_KEY: &str = "$meworkImageAttachment";
const QUARANTINE_DIRECTORY: &str = ".orphaned";
const ORPHAN_GRACE_PERIOD: Duration = Duration::from_secs(24 * 60 * 60);
const QUARANTINE_RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);
static IMAGE_ATTACHMENT_FS_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub restored: usize,
    pub quarantined: usize,
    pub deleted: usize,
}

/// Encoding stored in an image placeholder.
///
/// `DataUrl` is the only `ImagePart` representation that carries MIME data. The
/// enum is persisted in placeholder payloads and checked during hydration, so it
/// proves that the host created the slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireEncoding {
    DataUrl,
}

impl WireEncoding {
    fn as_str(self) -> &'static str {
        match self {
            Self::DataUrl => "data_url",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ImageAttachmentStore {
    root: PathBuf,
}

impl ImageAttachmentStore {
    pub fn new(app_data: &Path) -> Self {
        Self {
            root: app_data.join("image-attachments"),
        }
    }

    pub fn import(&self, name: &str, bytes: &[u8]) -> Result<ImageAttachment, String> {
        let name = validate_name(name)?;
        if bytes.is_empty() {
            return Err("Image content is empty".into());
        }
        if bytes.len() > MAX_IMAGE_ATTACHMENT_BYTES {
            return Err(format!(
                "Image exceeds the {} MiB limit ({} bytes)",
                MAX_IMAGE_ATTACHMENT_BYTES / 1024 / 1024,
                bytes.len()
            ));
        }
        let canonical = canonicalize_image(bytes)?;
        let id = hex_digest(&canonical.bytes);
        let _guard = image_attachment_fs_lock();
        ensure_directory_within(
            &self.root,
            self.root
                .parent()
                .ok_or_else(|| "Image attachment root directory has no parent directory".to_owned())?,
            true,
            "image attachment directory",
        )?;
        self.restore_quarantined_locked(&id)?;
        let target = self.path_for_id(&id)?;
        match read_regular_file(&target) {
            Ok(existing) => {
                if existing != canonical.bytes {
                    return Err(format!("Image attachment {id} has a content-addressed file collision"));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                crate::storage::atomic_write(&target, &canonical.bytes)
                    .map_err(|error| format!("Could not write image attachment: {error}"))?;
            }
            Err(error) => return Err(format!("Could not read image attachment: {error}")),
        }

        Ok(ImageAttachment {
            id,
            name,
            mime: canonical.mime.into(),
            width: canonical.width,
            height: canonical.height,
            bytes: canonical.bytes.len() as u64,
            // Conversation numbering is the run loop's job; a fresh import has
            // no number until the transcript assigns one.
            short_id: None,
        })
    }

    pub fn read_bytes(&self, image: &ImageAttachment) -> Result<Vec<u8>, String> {
        validate_metadata(image)?;
        let _guard = image_attachment_fs_lock();
        self.restore_quarantined_locked(&image.id)?;
        let bytes = read_regular_file(&self.path_for_id(&image.id)?)
            .map_err(|error| format!("Could not read image attachment {}: {error}", image.id))?;
        if bytes.len() as u64 != image.bytes {
            return Err(format!("Image attachment {} byte count does not match metadata", image.id));
        }
        if hex_digest(&bytes) != image.id {
            return Err(format!("Image attachment {} failed its content integrity check", image.id));
        }
        let decoded = validate_canonical_sidecar(&bytes)
            .map_err(|error| format!("Image attachment {} failed its canonical-content integrity check: {error}", image.id))?;
        if decoded.mime != image.mime {
            return Err(format!("Image attachment {} MIME type does not match metadata", image.id));
        }
        if decoded.width != image.width || decoded.height != image.height {
            return Err(format!("Image attachment {} dimensions do not match metadata", image.id));
        }
        Ok(bytes)
    }

    pub fn data_url(&self, image: &ImageAttachment) -> Result<String, String> {
        let bytes = self.read_bytes(image)?;
        Ok(format!(
            "data:{};base64,{}",
            image.mime,
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ))
    }

    pub fn data_url_by_id(&self, id: &str) -> Result<String, String> {
        validate_id(id)?;
        let _guard = image_attachment_fs_lock();
        self.restore_quarantined_locked(id)?;
        let bytes = read_regular_file(&self.path_for_id(id)?)
            .map_err(|error| format!("Could not read image attachment {id}: {error}"))?;
        if bytes.is_empty() || bytes.len() > MAX_IMAGE_ATTACHMENT_BYTES || hex_digest(&bytes) != id
        {
            return Err(format!("Image attachment {id} failed its content integrity check"));
        }
        let decoded = validate_canonical_sidecar(&bytes)
            .map_err(|error| format!("Image attachment {id} failed its canonical-content integrity check: {error}"))?;
        Ok(format!(
            "data:{};base64,{}",
            decoded.mime,
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ))
    }

    /// Reconciles a committed document transition without physically deleting
    /// attachment bytes. References removed by an edit, branch deletion or
    /// conversation deletion are moved into a private quarantine immediately;
    /// old never-persisted imports are quarantined after a grace period.
    ///
    /// Reads and same-content imports transparently restore quarantined bytes.
    /// This is intentionally safe while a model/tool request is still using an
    /// in-memory snapshot, and it also preserves crash recovery while the
    /// document writer is still flushing the new snapshot.
    pub fn reconcile_transition(
        &self,
        previous: &AppDocument,
        next: &AppDocument,
    ) -> Result<ReconcileReport, String> {
        let previous_ids = referenced_image_ids(previous);
        let next_ids = referenced_image_ids(next);
        let quarantine_ids = previous_ids
            .difference(&next_ids)
            .cloned()
            .collect::<HashSet<_>>();
        let now = SystemTime::now();
        let _guard = image_attachment_fs_lock();
        let mut report = ReconcileReport::default();

        for id in &next_ids {
            report.restored += usize::from(self.restore_quarantined_locked(id)?);
        }
        // A transition observes only two committed snapshots. A hash dropped
        // here may simultaneously belong to a composer draft or request
        // snapshot outside both documents, so every transition candidate
        // remains recoverable. Startup reconciliation owns physical deletion
        // after its complete persisted-document scan.
        for id in &quarantine_ids {
            report.quarantined += usize::from(self.quarantine_locked(id, now)?);
        }
        report.quarantined += self.quarantine_old_unreferenced_locked(&next_ids, now)?;
        Ok(report)
    }

    /// Startup-only reconciliation. Call this before model/tool operations can
    /// begin: referenced crash-recovery files are restored first, expired
    /// quarantine entries are then physically removed, and old unreferenced
    /// active files are quarantined last so they cannot be deleted in the same
    /// pass.
    pub fn reconcile_startup(&self, document: &AppDocument) -> Result<ReconcileReport, String> {
        let referenced = referenced_image_ids(document);
        let now = SystemTime::now();
        let _guard = image_attachment_fs_lock();
        let mut report = ReconcileReport::default();

        for id in &referenced {
            report.restored += usize::from(self.restore_quarantined_locked(id)?);
        }
        report.deleted += self.delete_expired_quarantine_locked(&referenced, now)?;
        report.quarantined += self.quarantine_old_unreferenced_locked(&referenced, now)?;
        Ok(report)
    }

    /// Used only by the explicit full-document reset after its durability
    /// barrier and dependency fence have succeeded.
    pub fn purge_all(&self) -> Result<(), String> {
        let _guard = image_attachment_fs_lock();
        let metadata = match fs::symlink_metadata(&self.root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(format!("Could not read image attachment directory: {error}")),
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err("Image attachment root path is not a regular directory; recursive cleanup is denied".into());
        }
        ensure_directory_within(
            &self.root,
            self.root
                .parent()
                .ok_or_else(|| "Image attachment root directory has no parent directory".to_owned())?,
            false,
            "image attachment directory",
        )?;
        fs::remove_dir_all(&self.root).map_err(|error| format!("Could not remove image attachment directory: {error}"))
    }

    fn path_for_id(&self, id: &str) -> Result<PathBuf, String> {
        validate_id(id)?;
        Ok(self.root.join(id))
    }

    fn quarantine_path_for_id(&self, id: &str) -> Result<PathBuf, String> {
        validate_id(id)?;
        Ok(self.root.join(QUARANTINE_DIRECTORY).join(id))
    }

    fn restore_quarantined_locked(&self, id: &str) -> Result<bool, String> {
        let Some(()) = ensure_directory_within(
            &self.root,
            self.root
                .parent()
                .ok_or_else(|| "Image attachment root directory has no parent directory".to_owned())?,
            false,
            "image attachment directory",
        )?
        else {
            return Ok(false);
        };
        let active = self.path_for_id(id)?;
        match fs::symlink_metadata(&active) {
            Ok(metadata) => {
                ensure_regular_metadata(&metadata, &active)?;
                return Ok(false);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "Could not inspect image attachment path {}: {error}",
                    active.display()
                ))
            }
        }

        let quarantine = self.root.join(QUARANTINE_DIRECTORY);
        let Some(()) = ensure_directory_within(&quarantine, &self.root, false, "image attachment quarantine directory")?
        else {
            return Ok(false);
        };
        let quarantined = self.quarantine_path_for_id(id)?;
        let metadata = match fs::symlink_metadata(&quarantined) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(format!(
                    "Could not read quarantined image attachment {}: {error}",
                    quarantined.display()
                ))
            }
        };
        ensure_regular_metadata(&metadata, &quarantined)?;
        fs::rename(&quarantined, &active)
            .map_err(|error| format!("Could not restore image attachment {id}: {error}"))?;
        Ok(true)
    }

    fn quarantine_locked(&self, id: &str, now: SystemTime) -> Result<bool, String> {
        let Some(()) = ensure_directory_within(
            &self.root,
            self.root
                .parent()
                .ok_or_else(|| "Image attachment root directory has no parent directory".to_owned())?,
            false,
            "image attachment directory",
        )?
        else {
            return Ok(false);
        };
        let active = self.path_for_id(id)?;
        let metadata = match fs::symlink_metadata(&active) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(format!(
                    "Could not inspect image attachment path {}: {error}",
                    active.display()
                ))
            }
        };
        ensure_regular_metadata(&metadata, &active)?;

        let quarantine = self.root.join(QUARANTINE_DIRECTORY);
        ensure_directory_within(&quarantine, &self.root, true, "image attachment quarantine directory")?;
        let target = self.quarantine_path_for_id(id)?;
        match fs::symlink_metadata(&target) {
            Ok(existing) => {
                ensure_regular_metadata(&existing, &target)?;
                fs::remove_file(&target)
                    .map_err(|error| format!("Could not replace quarantined image attachment {id}: {error}"))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("Could not read quarantined image attachment {id}: {error}")),
        }
        fs::rename(&active, &target).map_err(|error| format!("Could not quarantine image attachment {id}: {error}"))?;
        let touch_result = fs::OpenOptions::new()
            .write(true)
            .open(&target)
            .and_then(|file| file.set_modified(now));
        if let Err(error) = touch_result {
            let _ = fs::rename(&target, &active);
            return Err(format!("Could not record quarantine time for image attachment {id}: {error}"));
        }
        Ok(true)
    }

    fn quarantine_old_unreferenced_locked(
        &self,
        referenced: &HashSet<String>,
        now: SystemTime,
    ) -> Result<usize, String> {
        let Some(()) = ensure_directory_within(
            &self.root,
            self.root
                .parent()
                .ok_or_else(|| "Image attachment root directory has no parent directory".to_owned())?,
            false,
            "image attachment directory",
        )?
        else {
            return Ok(0);
        };
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(format!("Could not read image attachment directory: {error}")),
        };
        let mut candidates = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| format!("Could not read image attachment directory entry: {error}"))?;
            let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if validate_id(&id).is_err() || referenced.contains(&id) {
                continue;
            }
            let metadata = entry
                .metadata()
                .map_err(|error| format!("Could not read metadata for image attachment {id}: {error}"))?;
            if !entry
                .file_type()
                .map_err(|error| format!("Could not read file type for image attachment {id}: {error}"))?
                .is_file()
                || !is_old_enough(&metadata, now, ORPHAN_GRACE_PERIOD)
            {
                continue;
            }
            candidates.push(id);
        }
        let mut quarantined = 0;
        for id in candidates {
            quarantined += usize::from(self.quarantine_locked(&id, now)?);
        }
        Ok(quarantined)
    }

    fn delete_expired_quarantine_locked(
        &self,
        referenced: &HashSet<String>,
        now: SystemTime,
    ) -> Result<usize, String> {
        let quarantine = self.root.join(QUARANTINE_DIRECTORY);
        let Some(()) = ensure_directory_within(&quarantine, &self.root, false, "image attachment quarantine directory")?
        else {
            return Ok(0);
        };
        let entries = match fs::read_dir(&quarantine) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(format!("Could not read image attachment quarantine directory: {error}")),
        };
        let mut deleted = 0;
        for entry in entries {
            let entry = entry.map_err(|error| format!("Could not read quarantined image attachment directory entry: {error}"))?;
            let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if validate_id(&id).is_err() || referenced.contains(&id) {
                continue;
            }
            let file_type = entry
                .file_type()
                .map_err(|error| format!("Could not read file type for quarantined image attachment {id}: {error}"))?;
            if !file_type.is_file() {
                continue;
            }
            let metadata = entry
                .metadata()
                .map_err(|error| format!("Could not read metadata for quarantined image attachment {id}: {error}"))?;
            if !is_old_enough(&metadata, now, QUARANTINE_RETENTION) {
                continue;
            }
            fs::remove_file(entry.path())
                .map_err(|error| format!("Could not delete expired quarantined image attachment {id}: {error}"))?;
            deleted += 1;
        }
        Ok(deleted)
    }
}

fn image_attachment_fs_lock() -> std::sync::MutexGuard<'static, ()> {
    IMAGE_ATTACHMENT_FS_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn ensure_directory_within(
    path: &Path,
    parent: &Path,
    create: bool,
    label: &str,
) -> Result<Option<()>, String> {
    if create {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(format!("{label} is not a regular directory; access is denied"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir_all(path).map_err(|error| format!("Could not create {label}: {error}"))?;
            }
            Err(error) => return Err(format!("Could not inspect {label}: {error}")),
        }
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !create => return Ok(None),
        Err(error) => return Err(format!("Could not inspect {label}: {error}")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!("{label} is not a regular directory; access is denied"));
    }
    let canonical_parent =
        fs::canonicalize(parent).map_err(|error| format!("Could not canonicalize parent directory of {label}: {error}"))?;
    let canonical_path =
        fs::canonicalize(path).map_err(|error| format!("Could not canonicalize {label}: {error}"))?;
    if canonical_path.parent() != Some(canonical_parent.as_path()) {
        return Err(format!("{label} escapes the fixed application-data path; access is denied"));
    }
    Ok(Some(()))
}

fn ensure_regular_metadata(metadata: &fs::Metadata, path: &Path) -> Result<(), String> {
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "Image attachment path {} is not a regular file; access is denied",
            path.display()
        ));
    }
    Ok(())
}

fn read_regular_file(path: &Path) -> std::io::Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(std::io::Error::other("Image attachment path is not a regular file"));
    }
    if metadata.len() > MAX_IMAGE_ATTACHMENT_BYTES as u64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Image attachment exceeds the {} MiB read limit",
                MAX_IMAGE_ATTACHMENT_BYTES / 1024 / 1024
            ),
        ));
    }

    let file = fs::File::open(path)?;
    let opened_metadata = file.metadata()?;
    if !opened_metadata.is_file() {
        return Err(std::io::Error::other("Image attachment path is not a regular file"));
    }
    if opened_metadata.len() > MAX_IMAGE_ATTACHMENT_BYTES as u64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Image attachment exceeds the {} MiB read limit",
                MAX_IMAGE_ATTACHMENT_BYTES / 1024 / 1024
            ),
        ));
    }

    // The file may grow or be replaced after either metadata check. Reading at
    // most limit + 1 keeps that race bounded; the extra byte distinguishes an
    // exact-limit image from an oversized one without allocating the whole file.
    let mut bytes = Vec::with_capacity(opened_metadata.len() as usize);
    file.take((MAX_IMAGE_ATTACHMENT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_IMAGE_ATTACHMENT_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Image attachment exceeds the {} MiB read limit",
                MAX_IMAGE_ATTACHMENT_BYTES / 1024 / 1024
            ),
        ));
    }
    Ok(bytes)
}

fn is_old_enough(metadata: &fs::Metadata, now: SystemTime, minimum: Duration) -> bool {
    metadata
        .modified()
        .ok()
        .and_then(|modified| now.duration_since(modified).ok())
        .is_some_and(|age| age >= minimum)
}

pub fn referenced_image_ids(document: &AppDocument) -> HashSet<String> {
    let mut ids = HashSet::new();
    for workspace in &document.workspaces {
        for conversation in &workspace.conversations {
            for message in &conversation.queued_messages {
                ids.extend(message.images.iter().map(|image| image.id.clone()));
            }
            collect_context_image_ids(&conversation.contexts, &mut ids);
            for branch in &conversation.branches {
                collect_context_image_ids(&branch.contexts, &mut ids);
            }
        }
    }
    ids
}

fn collect_context_image_ids(contexts: &[ContextItem], ids: &mut HashSet<String>) {
    for context in contexts {
        match context {
            ContextItem::User { images, .. } => {
                ids.extend(images.iter().map(|image| image.id.clone()));
            }
            ContextItem::Tool {
                result, subagent, ..
            } => {
                ids.extend(result.images.iter().map(|image| image.id.clone()));
                if let Some(subagent) = subagent {
                    collect_context_image_ids(&subagent.contexts, ids);
                }
            }
            ContextItem::System { .. }
            | ContextItem::Assistant { .. }
            | ContextItem::Reasoning { .. } => {}
        }
    }
}

pub fn placeholder(image: &ImageAttachment, encoding: WireEncoding) -> Value {
    json!({
        PLACEHOLDER_KEY: {
            "image": image,
            "encoding": encoding.as_str(),
        }
    })
}

pub(crate) fn is_placeholder(value: &Value) -> bool {
    value
        .as_object()
        .is_some_and(|object| object.len() == 1 && object.contains_key(PLACEHOLDER_KEY))
}

fn parse_placeholder(
    value: &Value,
    expected_encoding: WireEncoding,
) -> Result<Option<ImageAttachment>, String> {
    if !is_placeholder(value) {
        return Ok(None);
    }
    let payload = value
        .get(PLACEHOLDER_KEY)
        .and_then(Value::as_object)
        .ok_or_else(|| "Image attachment placeholder payload is invalid".to_owned())?;
    if payload.len() != 2 || !payload.contains_key("image") || !payload.contains_key("encoding") {
        return Err("Image attachment placeholder payload field is invalid".into());
    }
    if payload.get("encoding").and_then(Value::as_str) != Some(expected_encoding.as_str()) {
        return Err("Image attachment placeholder encoding does not match the provider image slot".into());
    }
    let image: ImageAttachment = serde_json::from_value(
        payload
            .get("image")
            .cloned()
            .ok_or_else(|| "Image attachment placeholder is missing image".to_owned())?,
    )
    .map_err(|error| format!("Image attachment placeholder is invalid: {error}"))?;
    validate_metadata(&image)?;
    Ok(Some(image))
}




/// Request-wide image placeholder totals.
///
/// Calculate all three values from one slot-table traversal so they describe the
/// same request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImagePlaceholderStats {
    pub count: usize,
    pub bytes: u64,
    pub pixels: u64,
}

/// Host-owned image slots in AI SDK `ModelMessage[]`, indexed by
/// `(message index, part index)`.
///
/// Do not recursively search JSON. Tool arguments and continuation blocks are
/// model-controlled and may contain matching fields. Only image parts in user
/// messages are host-owned; AI SDK response messages use assistant or tool roles.
fn ai_sdk_image_slots(messages: &[Value]) -> Vec<(usize, usize)> {
    let mut slots = Vec::new();
    for (message_index, message) in messages.iter().enumerate() {
        if message.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let Some(parts) = message.get("content").and_then(Value::as_array) else {
            continue;
        };
        for (part_index, part) in parts.iter().enumerate() {
            if part.get("type").and_then(Value::as_str) == Some("image")
                && part.get("image").is_some()
            {
                slots.push((message_index, part_index));
            }
        }
    }
    slots
}

fn ai_sdk_slot_value<'a>(messages: &'a [Value], slot: (usize, usize)) -> Option<&'a Value> {
    messages
        .get(slot.0)?
        .get("content")?
        .as_array()?
        .get(slot.1)?
        .get("image")
}

/// Returns the three values required by the request guards. `DataUrl` is the
/// only MIME-carrying `DataContent` representation accepted by `ImagePart`.
pub fn ai_sdk_placeholder_stats(messages: &[Value]) -> Result<ImagePlaceholderStats, String> {
    let mut stats = ImagePlaceholderStats::default();
    for slot in ai_sdk_image_slots(messages) {
        let Some(value) = ai_sdk_slot_value(messages, slot) else {
            continue;
        };
        let Some(image) = parse_placeholder(value, WireEncoding::DataUrl)? else {
            continue;
        };
        stats.count = stats.count.saturating_add(1);
        stats.bytes = stats.bytes.saturating_add(image.bytes);
        stats.pixels = stats
            .pixels
            .saturating_add(u64::from(image.width) * u64::from(image.height));
    }
    Ok(stats)
}

/// Replaces placeholders with data URLs.
///
/// Only recognized slots are changed. The cached projection must retain neither
/// base64 nor data URLs because it is reused incrementally in later rounds.
pub fn hydrate_ai_sdk_images(
    messages: &[Value],
    store: &ImageAttachmentStore,
) -> Result<Vec<Value>, String> {
    let slots = ai_sdk_image_slots(messages);
    if slots.is_empty() {
        return Ok(messages.to_vec());
    }
    let mut hydrated = messages.to_vec();
    for slot in slots {
        let Some(image) = ai_sdk_slot_value(&hydrated, slot)
            .map(|value| parse_placeholder(value, WireEncoding::DataUrl))
            .transpose()?
            .flatten()
        else {
            continue;
        };
        let data_url = store.data_url(&image)?;
        let target = hydrated
            .get_mut(slot.0)
            .and_then(|message| message.get_mut("content"))
            .and_then(Value::as_array_mut)
            .and_then(|parts| parts.get_mut(slot.1))
            .and_then(|part| part.get_mut("image"))
            .ok_or_else(|| "Image attachment slot changed during hydration".to_owned())?;
        *target = Value::String(data_url);
    }
    Ok(hydrated)
}

pub fn validate_metadata(image: &ImageAttachment) -> Result<(), String> {
    validate_id(&image.id)?;
    validate_name(&image.name)?;
    if image.short_id == Some(0) {
        return Err(format!("Image attachment {} number must start at 1", image.id));
    }
    if !matches!(
        image.mime.as_str(),
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    ) {
        return Err(format!("Image attachment {} has an unsupported MIME type", image.id));
    }
    if image.bytes == 0 || image.bytes > MAX_IMAGE_ATTACHMENT_BYTES as u64 {
        return Err(format!("Image attachment {} has an invalid byte count", image.id));
    }
    validate_dimensions(image.width, image.height)
}

/// Validates one canonical image-bearing message/result before it is persisted
/// or admitted to a live steer. Request-wide history budgets remain a separate
/// provider-hydration concern.
pub fn validate_image_list(images: &[ImageAttachment], owner: &str) -> Result<(), String> {
    if images.len() > MAX_REQUEST_IMAGES {
        return Err(format!(
            "{owner} has more than {MAX_REQUEST_IMAGES} image attachments"
        ));
    }

    let mut total_bytes = 0_u64;
    let mut total_pixels = 0_u64;
    for image in images {
        validate_metadata(image).map_err(|error| format!("{owner} has an invalid image attachment: {error}"))?;
        total_bytes = total_bytes
            .checked_add(image.bytes)
            .ok_or_else(|| format!("{owner} image attachment total byte count overflowed"))?;
        let pixels = u64::from(image.width)
            .checked_mul(u64::from(image.height))
            .ok_or_else(|| format!("{owner} image attachment total pixel count overflowed"))?;
        total_pixels = total_pixels
            .checked_add(pixels)
            .ok_or_else(|| format!("{owner} image attachment total pixel count overflowed"))?;
    }

    if total_bytes > MAX_REQUEST_IMAGE_BYTES {
        return Err(format!(
            "{owner} image attachment total size exceeds the {} MiB limit",
            MAX_REQUEST_IMAGE_BYTES / 1024 / 1024
        ));
    }
    if total_pixels > MAX_REQUEST_IMAGE_PIXELS {
        return Err(format!(
            "{owner} image attachment total pixels exceed the {} MP limit",
            MAX_REQUEST_IMAGE_PIXELS / 1024 / 1024
        ));
    }
    Ok(())
}

fn validate_id(id: &str) -> Result<(), String> {
    if id.len() != 64
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("Image attachment ID must be a full lowercase SHA-256 digest".into());
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty()
        || name.len() > MAX_IMAGE_ATTACHMENT_NAME_BYTES
        || name.chars().any(char::is_control)
    {
        return Err(format!(
            "Image name must not be empty, contain control characters, or exceed {MAX_IMAGE_ATTACHMENT_NAME_BYTES} bytes"
        ));
    }
    Ok(name.to_owned())
}

fn validate_dimensions(width: u32, height: u32) -> Result<(), String> {
    if width == 0
        || height == 0
        || width > MAX_IMAGE_ATTACHMENT_DIMENSION
        || height > MAX_IMAGE_ATTACHMENT_DIMENSION
    {
        return Err(format!(
            "Image dimensions must be within 1–{MAX_IMAGE_ATTACHMENT_DIMENSION} pixels"
        ));
    }
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_IMAGE_ATTACHMENT_PIXELS {
        return Err(format!(
            "Total image pixels exceed the {} MP limit",
            MAX_IMAGE_ATTACHMENT_PIXELS / 1024 / 1024
        ));
    }
    Ok(())
}

fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn sniff_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

pub fn is_supported_image(bytes: &[u8]) -> bool {
    sniff_mime(bytes).is_some()
}

#[derive(Debug)]
struct DecodedImage {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

#[derive(Debug)]
struct CanonicalImage {
    bytes: Vec<u8>,
    mime: &'static str,
    width: u32,
    height: u32,
}

#[derive(Debug)]
struct ValidatedCanonicalImage {
    mime: &'static str,
    width: u32,
    height: u32,
}

#[derive(Debug)]
enum CanonicalPixels {
    L8(Vec<u8>),
    La8(Vec<u8>),
    Rgb8(Vec<u8>),
    Rgba8(Vec<u8>),
}

impl CanonicalPixels {
    fn from_rgba(mut rgba: Vec<u8>) -> Self {
        for pixel in rgba.chunks_exact_mut(4) {
            if pixel[3] == 0 {
                pixel[..3].fill(0);
            }
        }
        let grayscale = rgba
            .chunks_exact(4)
            .all(|pixel| pixel[0] == pixel[1] && pixel[1] == pixel[2]);
        let opaque = rgba.chunks_exact(4).all(|pixel| pixel[3] == u8::MAX);
        match (grayscale, opaque) {
            (true, true) => Self::L8(
                rgba.chunks_exact(4)
                    .map(|pixel| pixel[0])
                    .collect::<Vec<_>>(),
            ),
            (true, false) => Self::La8(
                rgba.chunks_exact(4)
                    .flat_map(|pixel| [pixel[0], pixel[3]])
                    .collect::<Vec<_>>(),
            ),
            (false, true) => Self::Rgb8(
                rgba.chunks_exact(4)
                    .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
                    .collect::<Vec<_>>(),
            ),
            (false, false) => Self::Rgba8(rgba),
        }
    }

    fn bytes(&self) -> &[u8] {
        match self {
            Self::L8(bytes) | Self::La8(bytes) | Self::Rgb8(bytes) | Self::Rgba8(bytes) => bytes,
        }
    }

    fn png_color_type(&self) -> png::ColorType {
        match self {
            Self::L8(_) => png::ColorType::Grayscale,
            Self::La8(_) => png::ColorType::GrayscaleAlpha,
            Self::Rgb8(_) => png::ColorType::Rgb,
            Self::Rgba8(_) => png::ColorType::Rgba,
        }
    }

    fn webp_color_type(&self) -> image_webp::ColorType {
        match self {
            Self::L8(_) => image_webp::ColorType::L8,
            Self::La8(_) => image_webp::ColorType::La8,
            Self::Rgb8(_) => image_webp::ColorType::Rgb8,
            Self::Rgba8(_) => image_webp::ColorType::Rgba8,
        }
    }
}

#[derive(Debug)]
struct LimitedImageWriter {
    bytes: Vec<u8>,
    exceeded: bool,
    limit: usize,
}

impl LimitedImageWriter {
    fn new() -> Self {
        Self::with_limit(MAX_IMAGE_ATTACHMENT_BYTES)
    }

    fn with_limit(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            exceeded: false,
            limit,
        }
    }
}

impl Write for LimitedImageWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let exceeds_limit = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .map_or(true, |length| length > self.limit);
        if exceeds_limit {
            self.exceeded = true;
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "canonical image exceeds attachment limit",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn canonicalize_image(bytes: &[u8]) -> Result<CanonicalImage, String> {
    let decoded = decode_supported_image(bytes)?;
    let width = decoded.width;
    let height = decoded.height;
    let pixels = CanonicalPixels::from_rgba(decoded.rgba);

    if let Some(bytes) = encode_canonical_png(width, height, &pixels)? {
        return Ok(CanonicalImage {
            bytes,
            mime: "image/png",
            width,
            height,
        });
    }
    if let Some(bytes) = encode_canonical_webp(width, height, &pixels)? {
        return Ok(CanonicalImage {
            bytes,
            mime: "image/webp",
            width,
            height,
        });
    }

    Err(format!(
        "Image decoded successfully, but its metadata-free lossless canonical form exceeds {} MiB; reduce the image dimensions or complexity",
        MAX_IMAGE_ATTACHMENT_BYTES / 1024 / 1024
    ))
}

// Re-decode every sidecar at the provider boundary. Structural checks enforce
// the metadata-free containers emitted above without re-encoding persisted
// pixels, so an encoder upgrade cannot silently invalidate existing hashes.
fn validate_canonical_sidecar(bytes: &[u8]) -> Result<ValidatedCanonicalImage, String> {
    let mime = sniff_mime(bytes).ok_or_else(|| "Canonical image format is unsupported".to_owned())?;
    match mime {
        "image/png" => validate_canonical_png_container(bytes)?,
        "image/webp" => validate_canonical_webp_container(bytes)?,
        _ => return Err("Canonical image must be metadata-free PNG or lossless WebP".into()),
    }
    let decoded = decode_supported_image(bytes)?;
    Ok(ValidatedCanonicalImage {
        mime,
        width: decoded.width,
        height: decoded.height,
    })
}

fn decode_supported_image(bytes: &[u8]) -> Result<DecodedImage, String> {
    match sniff_mime(bytes) {
        Some("image/png") => decode_png(bytes),
        Some("image/jpeg") => {
            let orientation = jpeg_exif_orientation(bytes)?;
            let mut decoded = decode_jpeg(bytes)?;
            apply_exif_orientation(&mut decoded, orientation)?;
            Ok(decoded)
        }
        Some("image/gif") => decode_gif(bytes),
        Some("image/webp") => decode_webp(bytes),
        _ => Err("Unsupported image format; only PNG, JPEG, WebP, and static GIF are supported".into()),
    }
}

fn decoded_rgba_len(width: u32, height: u32) -> Result<usize, String> {
    validate_dimensions(width, height)?;
    u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .and_then(|bytes| usize::try_from(bytes).ok())
        .filter(|bytes| *bytes <= MAX_DECODED_IMAGE_BYTES)
        .ok_or_else(|| "Decoded image pixel buffer exceeds the limit".to_owned())
}

fn decode_png(bytes: &[u8]) -> Result<DecodedImage, String> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    let mut limits = png::Limits::default();
    limits.bytes = MAX_DECODER_WORKING_BYTES;
    decoder.set_limits(limits);
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .map_err(|error| format!("Invalid PNG header: {error}"))?;
    if reader.info().animation_control.is_some() {
        return Err("APNG animation is not supported; provide a static PNG".into());
    }
    let (width, height) = reader.info().size();
    let expected_rgba = decoded_rgba_len(width, height)?;
    let output_size = reader.output_buffer_size();
    if output_size > expected_rgba {
        return Err("PNG decode buffer exceeds the pixel limit".into());
    }
    let mut output = vec![0; output_size];
    let info = reader
        .next_frame(&mut output)
        .map_err(|error| format!("Corrupt PNG pixel data: {error}"))?;
    if info.width != width || info.height != height || info.bit_depth != png::BitDepth::Eight {
        return Err("Decoded PNG dimensions or bit depth mismatch".into());
    }
    let pixels = &output[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Grayscale => pixels
            .iter()
            .flat_map(|value| [*value, *value, *value, u8::MAX])
            .collect(),
        png::ColorType::GrayscaleAlpha => pixels
            .chunks_exact(2)
            .flat_map(|pixel| [pixel[0], pixel[0], pixel[0], pixel[1]])
            .collect(),
        png::ColorType::Rgb => pixels
            .chunks_exact(3)
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], u8::MAX])
            .collect(),
        png::ColorType::Rgba => pixels.to_vec(),
        png::ColorType::Indexed => {
            return Err("PNG palette was not fully expanded".into());
        }
    };
    if rgba.len() != expected_rgba {
        return Err("Decoded PNG pixel length mismatch".into());
    }
    reader
        .finish()
        .map_err(|error| format!("Corrupt PNG trailer or checksum data: {error}"))?;
    Ok(DecodedImage {
        width,
        height,
        rgba,
    })
}

fn decode_jpeg(bytes: &[u8]) -> Result<DecodedImage, String> {
    use zune_jpeg::zune_core::{colorspace::ColorSpace, options::DecoderOptions};

    let options = DecoderOptions::default()
        .set_max_width(MAX_IMAGE_ATTACHMENT_DIMENSION as usize)
        .set_max_height(MAX_IMAGE_ATTACHMENT_DIMENSION as usize)
        .set_strict_mode(true)
        .jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(bytes, options);
    decoder
        .decode_headers()
        .map_err(|error| format!("Invalid JPEG header: {error}"))?;
    let (width, height) = decoder
        .dimensions()
        .ok_or_else(|| "JPEG is missing valid dimensions".to_owned())?;
    let width = u32::try_from(width).map_err(|_| "JPEG width exceeds the supported range".to_owned())?;
    let height = u32::try_from(height).map_err(|_| "JPEG height exceeds the supported range".to_owned())?;
    let expected_rgba = decoded_rgba_len(width, height)?;
    // A single-component frame makes the decoder override the requested RGBA output
    // back to Luma, so the native buffer is one byte per pixel and is expanded below.
    let colorspace = decoder
        .get_output_colorspace()
        .ok_or_else(|| "JPEG is missing an output color space".to_owned())?;
    let expected_native = expected_rgba / 4 * colorspace.num_components();
    if decoder.output_buffer_size() != Some(expected_native) {
        return Err("JPEG decode buffer exceeds the pixel limit".into());
    }
    let pixels = decoder
        .decode()
        .map_err(|error| format!("Corrupt JPEG pixel data: {error}"))?;
    if pixels.len() != expected_native {
        return Err("Decoded JPEG pixel length mismatch".into());
    }
    let rgba = match colorspace {
        ColorSpace::RGBA => pixels,
        ColorSpace::Luma => pixels
            .iter()
            .flat_map(|value| [*value, *value, *value, u8::MAX])
            .collect(),
        _ => return Err("JPEG output color space is unsupported".into()),
    };
    if rgba.len() != expected_rgba {
        return Err("Decoded JPEG pixel length mismatch".into());
    }
    Ok(DecodedImage {
        width,
        height,
        rgba,
    })
}

fn jpeg_exif_orientation(bytes: &[u8]) -> Result<u8, String> {
    if !bytes.starts_with(b"\xff\xd8") {
        return Err("Invalid JPEG SOI marker".into());
    }
    let mut cursor = 2_usize;
    let mut orientation = None;
    while cursor < bytes.len() {
        if bytes[cursor] != 0xff {
            return Err("Invalid JPEG marker boundary".into());
        }
        while bytes.get(cursor) == Some(&0xff) {
            cursor += 1;
        }
        let marker = *bytes
            .get(cursor)
            .ok_or_else(|| "Truncated JPEG marker".to_owned())?;
        cursor += 1;
        match marker {
            0xd9 | 0xda => break,
            0x01 | 0xd0..=0xd7 => continue,
            0x00 => return Err("JPEG header contains an invalid stuffed marker".into()),
            _ => {}
        }
        let length_end = cursor
            .checked_add(2)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| "Truncated JPEG segment length".to_owned())?;
        let segment_length = usize::from(u16::from_be_bytes(
            bytes[cursor..length_end]
                .try_into()
                .map_err(|_| "Invalid JPEG segment length".to_owned())?,
        ));
        if segment_length < 2 {
            return Err("JPEG segment length is less than 2".into());
        }
        let segment_end = cursor
            .checked_add(segment_length)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| "Truncated JPEG segment data".to_owned())?;
        if marker == 0xe1 {
            if let Some(found) = parse_exif_orientation(&bytes[length_end..segment_end])? {
                if orientation
                    .replace(found)
                    .is_some_and(|current| current != found)
                {
                    return Err("JPEG contains conflicting EXIF orientation".into());
                }
            }
        }
        cursor = segment_end;
    }
    Ok(orientation.unwrap_or(1))
}

fn parse_exif_orientation(payload: &[u8]) -> Result<Option<u8>, String> {
    let Some(tiff) = payload.strip_prefix(b"Exif\0\0") else {
        return Ok(None);
    };
    if tiff.len() < 8 {
        return Err("Truncated JPEG EXIF TIFF header".into());
    }
    let little_endian = match &tiff[..2] {
        b"II" => true,
        b"MM" => false,
        _ => return Err("Invalid JPEG EXIF byte order".into()),
    };
    if read_tiff_u16(tiff, 2, little_endian)? != 42 {
        return Err("Invalid JPEG EXIF TIFF magic".into());
    }
    let ifd_offset = usize::try_from(read_tiff_u32(tiff, 4, little_endian)?)
        .map_err(|_| "JPEG EXIF IFD0 offset is out of range".to_owned())?;
    let entry_count = usize::from(read_tiff_u16(tiff, ifd_offset, little_endian)?);
    if entry_count > 512 {
        return Err("JPEG EXIF IFD0 entry count exceeds the limit".into());
    }
    let entries_start = ifd_offset
        .checked_add(2)
        .ok_or_else(|| "JPEG EXIF IFD0 offset overflow".to_owned())?;
    let entries_end = entries_start
        .checked_add(
            entry_count
                .checked_mul(12)
                .ok_or_else(|| "JPEG EXIF IFD0 entry length overflow".to_owned())?,
        )
        .filter(|end| *end <= tiff.len())
        .ok_or_else(|| "Truncated JPEG EXIF IFD0 entry".to_owned())?;
    let mut orientation = None;
    for entry in tiff[entries_start..entries_end].chunks_exact(12) {
        if read_tiff_u16(entry, 0, little_endian)? != 0x0112 {
            continue;
        }
        if read_tiff_u16(entry, 2, little_endian)? != 3
            || read_tiff_u32(entry, 4, little_endian)? != 1
        {
            return Err("Invalid JPEG EXIF orientation type or count".into());
        }
        let value = read_tiff_u16(entry, 8, little_endian)?;
        let value = u8::try_from(value).map_err(|_| "JPEG EXIF orientation is out of range".to_owned())?;
        if !(1..=8).contains(&value) {
            return Err("JPEG EXIF orientation must be within 1–8".into());
        }
        if orientation.replace(value).is_some() {
            return Err("JPEG EXIF IFD0 contains duplicate orientation".into());
        }
    }
    Ok(orientation)
}

fn read_tiff_u16(bytes: &[u8], offset: usize, little_endian: bool) -> Result<u16, String> {
    let end = offset
        .checked_add(2)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| "Truncated JPEG EXIF u16".to_owned())?;
    let raw: [u8; 2] = bytes[offset..end]
        .try_into()
        .map_err(|_| "Invalid JPEG EXIF u16 length".to_owned())?;
    Ok(if little_endian {
        u16::from_le_bytes(raw)
    } else {
        u16::from_be_bytes(raw)
    })
}

fn read_tiff_u32(bytes: &[u8], offset: usize, little_endian: bool) -> Result<u32, String> {
    let end = offset
        .checked_add(4)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| "Truncated JPEG EXIF u32".to_owned())?;
    let raw: [u8; 4] = bytes[offset..end]
        .try_into()
        .map_err(|_| "Invalid JPEG EXIF u32 length".to_owned())?;
    Ok(if little_endian {
        u32::from_le_bytes(raw)
    } else {
        u32::from_be_bytes(raw)
    })
}

fn apply_exif_orientation(image: &mut DecodedImage, orientation: u8) -> Result<(), String> {
    if orientation == 1 {
        return Ok(());
    }
    if !(2..=8).contains(&orientation) {
        return Err("JPEG EXIF orientation exceeds the supported range".into());
    }
    let source_width = image.width as usize;
    let source_height = image.height as usize;
    let (output_width, output_height) = if orientation >= 5 {
        (source_height, source_width)
    } else {
        (source_width, source_height)
    };
    let mut output = vec![0; image.rgba.len()];
    for y in 0..output_height {
        for x in 0..output_width {
            let (source_x, source_y) = match orientation {
                2 => (source_width - 1 - x, y),
                3 => (source_width - 1 - x, source_height - 1 - y),
                4 => (x, source_height - 1 - y),
                5 => (y, x),
                6 => (y, source_height - 1 - x),
                7 => (source_width - 1 - y, source_height - 1 - x),
                8 => (source_width - 1 - y, x),
                _ => unreachable!("orientation 1 was handled before allocation"),
            };
            let source = (source_y * source_width + source_x) * 4;
            let destination = (y * output_width + x) * 4;
            output[destination..destination + 4].copy_from_slice(&image.rgba[source..source + 4]);
        }
    }
    image.width =
        u32::try_from(output_width).map_err(|_| "JPEG orientation-adjusted width overflow".to_owned())?;
    image.height =
        u32::try_from(output_height).map_err(|_| "JPEG orientation-adjusted height overflow".to_owned())?;
    image.rgba = output;
    Ok(())
}

fn decode_gif(bytes: &[u8]) -> Result<DecodedImage, String> {
    let mut options = gif::DecodeOptions::new();
    options.set_color_output(gif::ColorOutput::RGBA);
    options.set_memory_limit(gif::MemoryLimit::Bytes(
        NonZeroU64::new(MAX_DECODED_IMAGE_BYTES as u64)
            .ok_or_else(|| "Invalid GIF decode memory limit".to_owned())?,
    ));
    options.check_frame_consistency(true);
    options.check_lzw_end_code(true);
    let mut decoder = options
        .read_info(Cursor::new(bytes))
        .map_err(|error| format!("Invalid GIF header: {error}"))?;
    let width = u32::from(decoder.width());
    let height = u32::from(decoder.height());
    let expected_rgba = decoded_rgba_len(width, height)?;
    let mut rgba = vec![0; expected_rgba];
    let frame = decoder
        .read_next_frame()
        .map_err(|error| format!("Corrupt GIF pixel data: {error}"))?
        .ok_or_else(|| "GIF contains no image frame".to_owned())?;
    let frame_width = usize::from(frame.width);
    let frame_height = usize::from(frame.height);
    let frame_left = usize::from(frame.left);
    let frame_top = usize::from(frame.top);
    let canvas_width = width as usize;
    let canvas_height = height as usize;
    frame_left
        .checked_add(frame_width)
        .filter(|right| *right <= canvas_width)
        .ok_or_else(|| "GIF frame horizontal extent exceeds canvas".to_owned())?;
    frame_top
        .checked_add(frame_height)
        .filter(|bottom| *bottom <= canvas_height)
        .ok_or_else(|| "GIF frame vertical extent exceeds canvas".to_owned())?;
    if frame.buffer.len() != frame_width.saturating_mul(frame_height).saturating_mul(4) {
        return Err("GIF frame pixel length mismatch".into());
    }
    for row in 0..frame_height {
        let source_start = row * frame_width * 4;
        let destination_start = ((frame_top + row) * canvas_width + frame_left) * 4;
        rgba[destination_start..destination_start + frame_width * 4]
            .copy_from_slice(&frame.buffer[source_start..source_start + frame_width * 4]);
    }
    if decoder
        .read_next_frame()
        .map_err(|error| format!("Corrupt subsequent GIF frame: {error}"))?
        .is_some()
    {
        return Err("Only single-frame GIF is supported; animated GIF cannot be used as an image input across APIs".into());
    }
    Ok(DecodedImage {
        width,
        height,
        rgba,
    })
}

fn decode_webp(bytes: &[u8]) -> Result<DecodedImage, String> {
    if bytes.get(12..16) == Some(b"VP8X") && bytes.get(20).map_or(false, |flags| *flags & 2 != 0) {
        return Err("Animated WebP is not supported; provide a static WebP".into());
    }
    let mut decoder = image_webp::WebPDecoder::new(Cursor::new(bytes))
        .map_err(|error| format!("Invalid WebP header: {error}"))?;
    decoder.set_memory_limit(MAX_DECODER_WORKING_BYTES);
    if decoder.is_animated() {
        return Err("Animated WebP is not supported; provide a static WebP".into());
    }
    let (width, height) = decoder.dimensions();
    let expected_rgba = decoded_rgba_len(width, height)?;
    let output_size = decoder
        .output_buffer_size()
        .filter(|size| *size <= expected_rgba)
        .ok_or_else(|| "WebP decode buffer exceeds the pixel limit".to_owned())?;
    let mut output = vec![0; output_size];
    decoder
        .read_image(&mut output)
        .map_err(|error| format!("Corrupt WebP pixel data: {error}"))?;
    let rgba = if decoder.has_alpha() {
        output
    } else {
        output
            .chunks_exact(3)
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], u8::MAX])
            .collect()
    };
    if rgba.len() != expected_rgba {
        return Err("Decoded WebP pixel length mismatch".into());
    }
    Ok(DecodedImage {
        width,
        height,
        rgba,
    })
}

fn encode_canonical_png(
    width: u32,
    height: u32,
    pixels: &CanonicalPixels,
) -> Result<Option<Vec<u8>>, String> {
    encode_canonical_png_with_limit(width, height, pixels, MAX_IMAGE_ATTACHMENT_BYTES)
}

fn encode_canonical_png_with_limit(
    width: u32,
    height: u32,
    pixels: &CanonicalPixels,
    max_output_bytes: usize,
) -> Result<Option<Vec<u8>>, String> {
    let mut output = LimitedImageWriter::with_limit(max_output_bytes);
    let result = (|| -> Result<(), png::EncodingError> {
        let mut encoder = png::Encoder::new(&mut output, width, height);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_color(pixels.png_color_type());
        encoder.set_compression(png::Compression::Best);
        encoder.set_filter(png::FilterType::Paeth);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(pixels.bytes())?;
        writer.finish()
    })();
    match result {
        Ok(()) => Ok(Some(output.bytes)),
        Err(_) if output.exceeded => Ok(None),
        Err(error) => Err(format!("Could not generate canonical PNG: {error}")),
    }
}

fn encode_canonical_webp(
    width: u32,
    height: u32,
    pixels: &CanonicalPixels,
) -> Result<Option<Vec<u8>>, String> {
    let mut output = LimitedImageWriter::new();
    let result = image_webp::WebPEncoder::new(&mut output).encode(
        pixels.bytes(),
        width,
        height,
        pixels.webp_color_type(),
    );
    match result {
        Ok(()) => Ok(Some(output.bytes)),
        Err(_) if output.exceeded => Ok(None),
        Err(error) => Err(format!("Could not generate canonical WebP: {error}")),
    }
}

fn validate_canonical_png_container(bytes: &[u8]) -> Result<(), String> {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("Canonical PNG signature is invalid".into());
    }
    let mut cursor = 8_usize;
    let mut saw_header = false;
    let mut saw_data = false;
    loop {
        let header_end = cursor
            .checked_add(8)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| "Canonical PNG chunk header is truncated".to_owned())?;
        let length = u32::from_be_bytes(
            bytes[cursor..cursor + 4]
                .try_into()
                .map_err(|_| "Canonical PNG chunk length is invalid".to_owned())?,
        ) as usize;
        let chunk_type = &bytes[cursor + 4..header_end];
        let chunk_end = header_end
            .checked_add(length)
            .and_then(|end| end.checked_add(4))
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| "Canonical PNG chunk data is truncated".to_owned())?;
        match chunk_type {
            b"IHDR" if !saw_header && !saw_data && length == 13 => saw_header = true,
            b"IDAT" if saw_header => saw_data = true,
            b"IEND" if saw_header && saw_data && length == 0 => {
                if chunk_end != bytes.len() {
                    return Err("Canonical PNG has trailing data after IEND".into());
                }
                return Ok(());
            }
            b"IHDR" | b"IDAT" | b"IEND" => return Err("Canonical PNG chunk order is invalid".into()),
            _ => return Err("Canonical PNG must not contain metadata or private chunks".into()),
        }
        cursor = chunk_end;
    }
}

fn validate_canonical_webp_container(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() < 20 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return Err("Canonical WebP signature is invalid".into());
    }
    let riff_size = u32::from_le_bytes(
        bytes[4..8]
            .try_into()
            .map_err(|_| "Canonical WebP RIFF length is invalid".to_owned())?,
    ) as usize;
    if riff_size.checked_add(8) != Some(bytes.len()) {
        return Err("Canonical WebP RIFF length or trailing data is invalid".into());
    }
    if &bytes[12..16] != b"VP8L" {
        return Err("Canonical WebP must contain only a lossless VP8L image".into());
    }
    let chunk_size = u32::from_le_bytes(
        bytes[16..20]
            .try_into()
            .map_err(|_| "Canonical WebP VP8L length is invalid".to_owned())?,
    ) as usize;
    let padded_size = chunk_size
        .checked_add(chunk_size & 1)
        .ok_or_else(|| "Canonical WebP VP8L length overflowed".to_owned())?;
    if 20_usize.checked_add(padded_size) != Some(bytes.len()) {
        return Err("Canonical WebP contains extra chunks, metadata, or trailing data".into());
    }
    if chunk_size & 1 == 1 && bytes.last() != Some(&0) {
        return Err("Canonical WebP padding is invalid".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Small valid 1x1 RGBA PNG.
    const PNG: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 8, 215, 99, 248, 207, 192, 240, 31,
        0, 5, 0, 1, 255, 114, 156, 82, 103, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];

    fn test_png_rgba(pixel: [u8; 4], text: Option<&str>) -> Vec<u8> {
        let mut output = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut output, 1, 1);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_color(png::ColorType::Rgba);
            let mut writer = encoder.write_header().unwrap();
            if let Some(text) = text {
                writer
                    .write_text_chunk(&png::text_metadata::TEXtChunk::new("Comment", text))
                    .unwrap();
            }
            writer.write_image_data(&pixel).unwrap();
            writer.finish().unwrap();
        }
        output
    }

    fn test_solid_png(width: u32, height: u32, pixel: [u8; 4]) -> Vec<u8> {
        let pixels = pixel
            .into_iter()
            .cycle()
            .take((u64::from(width) * u64::from(height) * 4) as usize)
            .collect::<Vec<_>>();
        let mut output = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut output, width, height);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_color(png::ColorType::Rgba);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&pixels).unwrap();
            writer.finish().unwrap();
        }
        output
    }

    fn test_jpeg_2x1() -> Vec<u8> {
        let mut jpeg = base64::engine::general_purpose::STANDARD
            .decode("/9j/4AAQSkZJRgABAQEAYABgAAD/2wBDAAMCAgMCAgMDAwMEAwMEBQgFBQQEBQoHBwYIDAoMDAsKCwsNDhIQDQ4RDgsLEBYQERMUFRUVDA8XGBYUGBIUFRT/2wBDAQMEBAUEBQkFBQkUDQsNFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBT/wAARCAABAAIDASIAAhEBAxEB/8QAHwAAAQUBAQEBAQEAAAAAAAAAAAECAwQFBgcICQoL/8QAtRAAAgEDAwIEAwUFBAQAAAF9AQIDAAQRBRIhMUEGE1FhByJxFDKBkaEII0KxwRVS0fAkM2JyggkKFhcYGRolJicoKSo0NTY3ODk6Q0RFRkdISUpTVFVWV1hZWmNkZWZnaGlqc3R1dnd4eXqDhIWGh4iJipKTlJWWl5iZmqKjpKWmp6ipqrKztLW2t7i5usLDxMXGx8jJytLT1NXW19jZ2uHi4+Tl5ufo6erx8vP09fb3+Pn6/8QAHwEAAwEBAQEBAQEBAQAAAAAAAAECAwQFBgcICQoL/8QAtREAAgECBAQDBAcFBAQAAQJ3AAECAxEEBSExBhJBUQdhcRMiMoEIFEKRobHBCSMzUvAVYnLRChYkNOEl8RcYGRomJygpKjU2Nzg5OkNERUZHSElKU1RVVldYWVpjZGVmZ2hpanN0dXZ3eHl6goOEhYaHiImKkpOUlZaXmJmaoqOkpaanqKmqsrO0tba3uLm6wsPExcbHyMnK0tPU1dbX2Nna4uPk5ebn6Onq8vP09fb3+Pn6/9oADAMBAAIRAxEAPwD4H8Q/8h/Uv+vmX/0M0UUV/ptkP/Ipwn/XuH/pKPAzr/kZ4r/r5P8A9KZ//9k=")
            .unwrap();
        // Complete the fixture's second 8-bit quantization table.
        jpeg.splice(155..155, [0x14, 0x14, 0x14]);
        jpeg
    }

    fn test_static_webp(pixel: [u8; 4]) -> Vec<u8> {
        let mut output = Vec::new();
        image_webp::WebPEncoder::new(&mut output)
            .encode(&pixel, 1, 1, image_webp::ColorType::Rgba8)
            .unwrap();
        output
    }

    fn test_solid_webp(width: u32, height: u32, pixel: [u8; 4]) -> Vec<u8> {
        let pixels = pixel
            .into_iter()
            .cycle()
            .take((u64::from(width) * u64::from(height) * 4) as usize)
            .collect::<Vec<_>>();
        let mut output = Vec::new();
        image_webp::WebPEncoder::new(&mut output)
            .encode(&pixels, width, height, image_webp::ColorType::Rgba8)
            .unwrap();
        output
    }

    fn append_riff_chunk(output: &mut Vec<u8>, chunk_type: &[u8; 4], data: &[u8]) {
        output.extend_from_slice(chunk_type);
        output.extend_from_slice(&(data.len() as u32).to_le_bytes());
        output.extend_from_slice(data);
        if data.len() & 1 == 1 {
            output.push(0);
        }
    }

    fn test_animated_webp() -> Vec<u8> {
        let static_webp = test_static_webp([255, 0, 0, 255]);
        let mut payload = b"WEBP".to_vec();
        append_riff_chunk(
            &mut payload,
            b"VP8X",
            &[0b0000_0010, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        );
        append_riff_chunk(&mut payload, b"ANIM", &[0, 0, 0, 0, 0, 0]);
        let mut frame = vec![0; 16];
        frame.extend_from_slice(&static_webp[12..]);
        append_riff_chunk(&mut payload, b"ANMF", &frame);

        let mut output = b"RIFF".to_vec();
        output.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        output.extend_from_slice(&payload);
        output
    }

    fn test_apng() -> Vec<u8> {
        let mut output = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut output, 1, 1);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_animated(2, 0).unwrap();
            encoder.validate_sequence(true);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[255, 0, 0, 255]).unwrap();
            writer.write_image_data(&[0, 0, 255, 255]).unwrap();
            writer.finish().unwrap();
        }
        output
    }

    fn jpeg_with_orientation(jpeg: &[u8], orientation: u8) -> Vec<u8> {
        assert!(jpeg.starts_with(b"\xff\xd8"));
        let mut exif = b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0".to_vec();
        exif.extend_from_slice(&[orientation, 0, 0, 0, 0, 0, 0, 0]);
        let mut output = jpeg[..2].to_vec();
        output.extend_from_slice(b"\xff\xe1");
        output.extend_from_slice(&u16::try_from(exif.len() + 2).unwrap().to_be_bytes());
        output.extend_from_slice(&exif);
        output.extend_from_slice(&jpeg[2..]);
        output
    }

        #[test]
    fn import_is_content_addressed_and_hydration_keeps_base64_external() {
        let temp = tempfile::tempdir().unwrap();
        let store = ImageAttachmentStore::new(temp.path());
        let first = store.import("pixel.png", PNG).unwrap();
        let second = store.import("renamed.png", PNG).unwrap();
        assert_eq!(first.id, second.id);
        assert_eq!(first.id.len(), 64);

        let projected = vec![json!({
            "role": "user",
            "content": [{
                "type": "image",
                "image": placeholder(&first, WireEncoding::DataUrl),
                "mediaType": "image/png",
            }],
        })];
        // The cached projection must not contain bytes because it is reused
        // incrementally in later rounds.
        let serialized = serde_json::to_string(&projected).unwrap();
        assert!(!serialized.contains("iVBOR"));

        let hydrated = hydrate_ai_sdk_images(&projected, &store).unwrap();
        assert!(hydrated[0]["content"][0]["image"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,"));
        assert_eq!(serde_json::to_string(&projected).unwrap(), serialized);
    }


    /// Only host-owned image slots are counted and hydrated.
    ///
    /// Tool-result images are projected into the following user message.
    #[test]
    fn hydrates_only_the_hosts_own_image_slots() {
        let temp = tempfile::tempdir().unwrap();
        let store = ImageAttachmentStore::new(temp.path());
        let image = store.import("pixel.png", PNG).unwrap();

        let projected = vec![
            json!({
                "role": "user",
                "content": [
                    { "type": "text", "text": "看这张" },
                    { "type": "image", "image": placeholder(&image, WireEncoding::DataUrl), "mediaType": "image/png" },
                ],
            }),
            json!({
                "role": "assistant",
                "content": [{
                    "type": "tool-call",
                    "toolCallId": "call_read",
                    "toolName": "read",
                    "input": { "path": "shot.png" },
                }],
            }),
            json!({
                "role": "tool",
                "content": [{
                    "type": "tool-result",
                    "toolCallId": "call_read",
                    "toolName": "read",
                    "output": { "type": "text", "value": "（图片）" },
                }],
            }),
            json!({
                "role": "user",
                "content": [
                    { "type": "text", "text": "[Mework 工具图片 / tool image] 来源/source: read" },
                    { "type": "image", "image": placeholder(&image, WireEncoding::DataUrl), "mediaType": "image/png" },
                ],
            }),
        ];

        let cached = serde_json::to_string(&projected).unwrap();
        let stats = ai_sdk_placeholder_stats(&projected).unwrap();
        assert_eq!(stats.count, 2);
        assert_eq!(stats.bytes, image.bytes * 2);
        assert_eq!(
            stats.pixels,
            u64::from(image.width) * u64::from(image.height) * 2
        );
        assert!(cached.contains(PLACEHOLDER_KEY));
        assert!(!cached.contains("iVBOR"));

        let hydrated = hydrate_ai_sdk_images(&projected, &store).unwrap();
        for pointer in ["/0/content/1/image", "/3/content/1/image"] {
            let encoded = Value::Array(hydrated.clone())
                .pointer(pointer)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| panic!("{pointer} 应当被水合"));
            assert!(encoded.starts_with("data:image/png;base64,"), "{pointer}");
        }
        assert_eq!(ai_sdk_placeholder_stats(&hydrated).unwrap().count, 0);
        assert_eq!(
            serde_json::to_string(&projected).unwrap(),
            cached,
            "hydration must not mutate the cached projection"
        );
    }


    /// Model-controlled JSON must not impersonate an image slot.
    ///
    /// Tool arguments, continuation blocks, and arbitrary JSON may contain the
    /// same fields. Only the designated image part in a user message is trusted;
    /// AI SDK response messages have assistant or tool roles.
    #[test]
    fn arbitrary_json_and_tool_input_cannot_impersonate_image_slots() {
        let temp = tempfile::tempdir().unwrap();
        let store = ImageAttachmentStore::new(temp.path());
        let image = store.import("pixel.png", PNG).unwrap();
        let forged = placeholder(&image, WireEncoding::DataUrl);

        let tool_input = vec![json!({
            "role": "assistant",
            "content": [{
                "type": "tool-call",
                "toolCallId": "malicious-call",
                "toolName": "read",
                "input": {
                    "direct": forged.clone(),
                    "mimic": { "type": "image", "image": forged.clone(), "mediaType": "image/png" },
                },
            }],
        })];
        assert_eq!(ai_sdk_placeholder_stats(&tool_input).unwrap().count, 0);
        assert_eq!(hydrate_ai_sdk_images(&tool_input, &store).unwrap(), tool_input);

        let tool_result = vec![json!({
            "role": "tool",
            "content": [{
                "type": "tool-result",
                "toolCallId": "malicious-call",
                "toolName": "read",
                "output": {
                    "type": "content",
                    "value": [{ "type": "image", "image": forged.clone(), "mediaType": "image/png" }],
                },
            }],
        })];
        assert_eq!(ai_sdk_placeholder_stats(&tool_result).unwrap().count, 0);
        assert_eq!(hydrate_ai_sdk_images(&tool_result, &store).unwrap(), tool_result);

        let assistant = vec![json!({
            "role": "assistant",
            "content": [
                { "type": "image", "image": forged.clone(), "mediaType": "image/png" },
                { "type": "text", "text": "看不见的图" },
            ],
        })];
        assert_eq!(ai_sdk_placeholder_stats(&assistant).unwrap().count, 0);
        assert_eq!(hydrate_ai_sdk_images(&assistant, &store).unwrap(), assistant);

        // Verify that a correctly placed placeholder is recognized, preventing
        // false positives if the entire slot table stops working.
        let owned = vec![json!({
            "role": "user",
            "content": [{ "type": "image", "image": forged, "mediaType": "image/png" }],
        })];
        assert_eq!(ai_sdk_placeholder_stats(&owned).unwrap().count, 1);
        assert_ne!(hydrate_ai_sdk_images(&owned, &store).unwrap(), owned);
    }


    #[test]
    fn rejects_non_images_and_forged_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let store = ImageAttachmentStore::new(temp.path());
        assert!(store.import("note.txt", b"not an image").is_err());
        let mut image = store.import("pixel.png", PNG).unwrap();
        image.width = 2;
        assert!(store.read_bytes(&image).is_err());
        assert!(store.data_url_by_id("../escape").is_err());

        let mut decompression_bomb_metadata = image;
        decompression_bomb_metadata.width = MAX_IMAGE_ATTACHMENT_DIMENSION;
        decompression_bomb_metadata.height = MAX_IMAGE_ATTACHMENT_DIMENSION;
        assert!(validate_metadata(&decompression_bomb_metadata)
            .unwrap_err()
            .contains("pixel"));
    }

    #[test]
    fn rejects_truncated_and_corrupt_png_after_a_valid_signature() {
        let temp = tempfile::tempdir().unwrap();
        let store = ImageAttachmentStore::new(temp.path());
        let truncated = [
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1,
        ];
        let truncated_error = store.import("truncated.png", &truncated).unwrap_err();
        assert!(
            truncated_error.contains("PNG"),
            "unexpected error: {truncated_error}"
        );

        let mut corrupt = PNG.to_vec();
        corrupt[48] ^= 0x80;
        let corrupt_error = store.import("corrupt.png", &corrupt).unwrap_err();
        assert!(
            corrupt_error.contains("PNG"),
            "unexpected error: {corrupt_error}"
        );
    }

    #[test]
    fn canonicalization_strips_private_data_and_deduplicates_equal_pixels() {
        let temp = tempfile::tempdir().unwrap();
        let store = ImageAttachmentStore::new(temp.path());
        let pixel = [19, 83, 211, 255];
        let plain = test_png_rgba(pixel, None);
        let mut private = test_png_rgba(pixel, Some("PRIVATE_METADATA"));
        private.extend_from_slice(b"PRIVATE_TRAILER");
        let webp = test_static_webp(pixel);

        let plain_image = store.import("plain.png", &plain).unwrap();
        let private_image = store.import("private.png", &private).unwrap();
        let webp_image = store.import("same-pixels.webp", &webp).unwrap();
        assert_eq!(plain_image.id, private_image.id);
        assert_eq!(plain_image.id, webp_image.id);
        assert_eq!(plain_image.mime, "image/png");

        let canonical = store.read_bytes(&plain_image).unwrap();
        assert!(!canonical.windows(7).any(|window| window == b"PRIVATE"));
        validate_canonical_png_container(&canonical).unwrap();
        assert_eq!(hex_digest(&canonical), plain_image.id);
    }

    #[test]
    fn canonical_webp_roundtrips_and_rejects_extra_chunks_or_trailing_data() {
        let pixels = CanonicalPixels::Rgb8(vec![255, 0, 0, 0, 0, 255]);
        let webp = encode_canonical_webp(2, 1, &pixels)
            .unwrap()
            .expect("two pixels must fit the attachment limit");
        validate_canonical_webp_container(&webp).unwrap();
        let decoded = decode_webp(&webp).unwrap();
        assert_eq!((decoded.width, decoded.height), (2, 1));
        assert_eq!(decoded.rgba, vec![255, 0, 0, 255, 0, 0, 255, 255]);

        let mut trailer = webp.clone();
        trailer.extend_from_slice(b"PRIVATE_TRAILER");
        assert!(validate_canonical_webp_container(&trailer).is_err());

        let mut extra_chunk = webp;
        append_riff_chunk(&mut extra_chunk, b"XMP ", b"PRIVATE");
        let riff_size = u32::try_from(extra_chunk.len() - 8).unwrap();
        extra_chunk[4..8].copy_from_slice(&riff_size.to_le_bytes());
        assert!(validate_canonical_webp_container(&extra_chunk).is_err());
    }

    #[test]
    fn jpeg_is_fully_decoded_and_reencoded_without_trailing_data() {
        let jpeg = test_jpeg_2x1();
        let mut with_trailer = jpeg.clone();
        with_trailer.extend_from_slice(b"PRIVATE_JPEG_TRAILER");
        let temp = tempfile::tempdir().unwrap();
        let store = ImageAttachmentStore::new(temp.path());
        let clean = store.import("photo.jpg", &jpeg).unwrap();
        let tailed = store.import("tailed.jpg", &with_trailer).unwrap();
        assert_eq!(clean.id, tailed.id);
        assert_eq!(clean.mime, "image/png");
        let canonical = store.read_bytes(&clean).unwrap();
        assert!(!canonical.windows(7).any(|window| window == b"PRIVATE"));
        assert!(!canonical.starts_with(b"\xff\xd8\xff"));
    }

    #[test]
    fn jpeg_exif_orientation_six_rotates_pixels_before_metadata_is_removed() {
        let jpeg = base64::engine::general_purpose::STANDARD
            .decode("/9j/4AAQSkZJRgABAQEAYABgAAD/2wBDAAMCAgMCAgMDAwMEAwMEBQgFBQQEBQoHBwYIDAoMDAsKCwsNDhIQDQ4RDgsLEBYQERMUFRUVDA8XGBYUGBIUFRT/2wBDAQMEBAUEBQkFBQkUDQsNFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBT/wAARCAABAAIDASIAAhEBAxEB/8QAHwAAAQUBAQEBAQEAAAAAAAAAAAECAwQFBgcICQoL/8QAtRAAAgEDAwIEAwUFBAQAAAF9AQIDAAQRBRIhMUEGE1FhByJxFDKBkaEII0KxwRVS0fAkM2JyggkKFhcYGRolJicoKSo0NTY3ODk6Q0RFRkdISUpTVFVWV1hZWmNkZWZnaGlqc3R1dnd4eXqDhIWGh4iJipKTlJWWl5iZmqKjpKWmp6ipqrKztLW2t7i5usLDxMXGx8jJytLT1NXW19jZ2uHi4+Tl5ufo6erx8vP09fb3+Pn6/8QAHwEAAwEBAQEBAQEBAQAAAAAAAAECAwQFBgcICQoL/8QAtREAAgECBAQDBAcFBAQAAQJ3AAECAxEEBSExBhJBUQdhcRMiMoEIFEKRobHBCSMzUvAVYnLRChYkNOEl8RcYGRomJygpKjU2Nzg5OkNERUZHSElKU1RVVldYWVpjZGVmZ2hpanN0dXZ3eHl6goOEhYaHiImKkpOUlZaXmJmaoqOkpaanqKmqsrO0tba3uLm6wsPExcbHyMnK0tPU1dbX2Nna4uPk5ebn6Onq8vP09fb3+Pn6/9oADAMBAAIRAxEAPwD4H8Q/8h/Uv+vmX/0M0UUV/ptkP/Ipwn/XuH/pKPAzr/kZ4r/r5P8A9KZ//9k=")
            .unwrap();
        let raw = decode_jpeg(&jpeg).unwrap();
        assert_eq!((raw.width, raw.height), (2, 1));
        let oriented = jpeg_with_orientation(&jpeg, 6);
        assert_eq!(jpeg_exif_orientation(&oriented).unwrap(), 6);

        let temp = tempfile::tempdir().unwrap();
        let store = ImageAttachmentStore::new(temp.path());
        let image = store.import("oriented.jpg", &oriented).unwrap();
        assert_eq!((image.width, image.height), (1, 2));
        let canonical = store.read_bytes(&image).unwrap();
        let decoded = decode_supported_image(&canonical).unwrap();
        assert_eq!(decoded.rgba[..4], raw.rgba[..4]);
        assert_eq!(decoded.rgba[4..8], raw.rgba[4..8]);
        assert!(!canonical.windows(6).any(|window| window == b"Exif\0\0"));
    }

    #[test]
    fn rejects_gif_apng_and_webp_animation() {
        let static_gif = vec![
            71, 73, 70, 56, 57, 97, 1, 0, 1, 0, 128, 0, 0, 0, 0, 0, 255, 255, 255, 44, 0, 0, 0, 0,
            1, 0, 1, 0, 0, 2, 2, 68, 1, 0, 59,
        ];
        let mut animated_gif = static_gif[..static_gif.len() - 1].to_vec();
        animated_gif.extend_from_slice(&static_gif[19..static_gif.len() - 1]);
        animated_gif.push(59);
        let mut outside_canvas_gif = static_gif.clone();
        outside_canvas_gif[20] = 1;

        let temp = tempfile::tempdir().unwrap();
        let store = ImageAttachmentStore::new(temp.path());
        assert!(store
            .import("animated.gif", &animated_gif)
            .unwrap_err()
            .contains("animated GIF"));
        assert!(store
            .import("animated.png", &test_apng())
            .unwrap_err()
            .contains("APNG"));
        assert!(store
            .import("animated.webp", &test_animated_webp())
            .unwrap_err()
            .contains("Animated WebP"));
        let outside_canvas =
            std::panic::catch_unwind(|| store.import("outside-canvas.gif", &outside_canvas_gif))
                .expect("out-of-canvas GIF must return an error instead of panicking");
        assert!(outside_canvas.is_err());
    }

    #[test]
    fn hydration_rejects_hash_valid_but_malformed_sidecar() {
        let temp = tempfile::tempdir().unwrap();
        let store = ImageAttachmentStore::new(temp.path());
        store.import("seed.png", PNG).unwrap();
        let malformed = vec![
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1,
        ];
        let forged = ImageAttachment {
            id: hex_digest(&malformed),
            name: "forged.png".into(),
            mime: "image/png".into(),
            width: 1,
            height: 1,
            bytes: malformed.len() as u64,
            short_id: None,
        };
        crate::storage::atomic_write(&store.path_for_id(&forged.id).unwrap(), &malformed).unwrap();

        let read_error = store.read_bytes(&forged).unwrap_err();
        assert!(read_error.contains("canonical-content integrity check"), "{read_error}");
        let projected = vec![json!({
            "role": "user",
            "content": [{
                "type": "image",
                "image": placeholder(&forged, WireEncoding::DataUrl),
                "mediaType": "image/png",
            }],
        })];
        let hydration_error = hydrate_ai_sdk_images(&projected, &store).unwrap_err();
        assert!(
            hydration_error.contains("canonical-content integrity check"),
            "{hydration_error}"
        );

        let mut noncanonical = test_png_rgba([7, 11, 13, 255], Some("PRIVATE_SIDECAR"));
        noncanonical.extend_from_slice(b"PRIVATE_TRAILER");
        let noncanonical_image = ImageAttachment {
            id: hex_digest(&noncanonical),
            name: "noncanonical.png".into(),
            mime: "image/png".into(),
            width: 1,
            height: 1,
            bytes: noncanonical.len() as u64,
            short_id: None,
        };
        crate::storage::atomic_write(
            &store.path_for_id(&noncanonical_image.id).unwrap(),
            &noncanonical,
        )
        .unwrap();
        let noncanonical_error = store.read_bytes(&noncanonical_image).unwrap_err();
        assert!(
            noncanonical_error.contains("metadata") || noncanonical_error.contains("trailing data"),
            "{noncanonical_error}"
        );
    }

    #[test]
    fn oversized_tampered_sidecars_are_rejected_before_every_read_entrypoint() {
        let temp = tempfile::tempdir().unwrap();
        let store = ImageAttachmentStore::new(temp.path());
        let image = store.import("pixel.png", PNG).unwrap();
        let path = store.path_for_id(&image.id).unwrap();
        fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&path)
            .unwrap()
            .set_len(MAX_IMAGE_ATTACHMENT_BYTES as u64 + 1)
            .unwrap();

        let read_error = store.read_bytes(&image).unwrap_err();
        assert!(read_error.contains("read limit"), "{read_error}");

        let data_url_error = store.data_url_by_id(&image.id).unwrap_err();
        assert!(data_url_error.contains("read limit"), "{data_url_error}");

        let import_error = store.import("same-content.png", PNG).unwrap_err();
        assert!(import_error.contains("read limit"), "{import_error}");
        assert_eq!(
            fs::metadata(path).unwrap().len(),
            MAX_IMAGE_ATTACHMENT_BYTES as u64 + 1
        );
    }

    #[test]
    fn accepts_static_gif_and_rejects_animated_gif() {
        let static_gif = vec![
            71, 73, 70, 56, 57, 97, 1, 0, 1, 0, 128, 0, 0, 0, 0, 0, 255, 255, 255, 44, 0, 0, 0, 0,
            1, 0, 1, 0, 0, 2, 2, 68, 1, 0, 59,
        ];
        let temp = tempfile::tempdir().unwrap();
        let store = ImageAttachmentStore::new(temp.path());
        assert_eq!(
            store.import("static.gif", &static_gif).unwrap().mime,
            "image/png"
        );

        let mut animated = static_gif[..static_gif.len() - 1].to_vec();
        animated.extend_from_slice(&static_gif[19..static_gif.len() - 1]);
        animated.push(59);
        assert!(store
            .import("animated.gif", &animated)
            .unwrap_err()
            .contains("animated GIF"));
    }

    fn document_with_references(root: &Path, image: &ImageAttachment) -> AppDocument {
        let mut document = crate::catalog::default_document(root);
        let conversation = &mut document.workspaces[0].conversations[0];
        conversation
            .queued_messages
            .push(crate::model::QueuedMessage {
                id: "queue-image".into(),
                content: String::new(),
                images: vec![image.clone()],
                created_at: "2026-07-24T00:00:00Z".into(),
            });
        conversation.contexts = vec![
            serde_json::from_value(json!({
                "kind": "user",
                "id": "user-image",
                "content": "",
                "images": [image],
                "createdAt": "2026-07-24T00:00:01Z"
            }))
            .unwrap(),
            serde_json::from_value(json!({
                "kind": "tool",
                "id": "tool-image",
                "toolName": "read",
                "input": {},
                "result": {
                    "success": true,
                    "output": "image",
                    "images": [image],
                    "executedAt": "2026-07-24T00:00:02Z",
                    "durationMs": 1
                },
                "subagent": {
                    "task": "inspect",
                    "status": "completed",
                    "contexts": [{
                        "kind": "user",
                        "id": "child-image",
                        "content": "",
                        "images": [image],
                        "createdAt": "2026-07-24T00:00:03Z"
                    }],
                    "updates": []
                },
                "createdAt": "2026-07-24T00:00:02Z"
            }))
            .unwrap(),
        ];
        conversation
            .branches
            .push(crate::model::ConversationBranch {
                id: "branch-image".into(),
                fork_context_id: "user-image".into(),
                active: false,
                contexts: vec![serde_json::from_value(json!({
                    "kind": "user",
                    "id": "branch-user-image",
                    "content": "",
                    "images": [image],
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
        let temp = tempfile::tempdir().unwrap();
        let store = ImageAttachmentStore::new(temp.path());
        let image = store.import("pixel.png", PNG).unwrap();
        let document = document_with_references(temp.path(), &image);
        assert_eq!(referenced_image_ids(&document), HashSet::from([image.id]));
    }

    #[test]
    fn removed_references_are_quarantined_but_active_snapshots_can_restore_them() {
        let temp = tempfile::tempdir().unwrap();
        let store = ImageAttachmentStore::new(temp.path());
        let image = store.import("pixel.png", PNG).unwrap();
        let canonical = store.read_bytes(&image).unwrap();
        let previous = document_with_references(temp.path(), &image);
        let next = crate::catalog::default_document(temp.path());

        let report = store.reconcile_transition(&previous, &next).unwrap();
        assert_eq!(report.quarantined, 1);
        assert!(!store.path_for_id(&image.id).unwrap().exists());
        assert!(store.quarantine_path_for_id(&image.id).unwrap().exists());

        // A model/tool request that retained the previous in-memory document
        // remains valid even after an intermediate save removed the reference.
        assert_eq!(store.read_bytes(&image).unwrap(), canonical);
        assert!(store.path_for_id(&image.id).unwrap().exists());
        assert!(!store.quarantine_path_for_id(&image.id).unwrap().exists());
    }

    #[test]
    fn removing_one_branch_does_not_quarantine_content_still_referenced_elsewhere() {
        let temp = tempfile::tempdir().unwrap();
        let store = ImageAttachmentStore::new(temp.path());
        let image = store.import("pixel.png", PNG).unwrap();
        let previous = document_with_references(temp.path(), &image);
        let mut next = crate::catalog::default_document(temp.path());
        next.workspaces[0].conversations[0].contexts = vec![serde_json::from_value(json!({
            "kind": "user",
            "id": "remaining-user-image",
            "content": "",
            "images": [{
                "id": image.id.clone(),
                "name": "renamed-after-edit.png",
                "mime": image.mime.clone(),
                "width": image.width,
                "height": image.height,
                "bytes": image.bytes
            }],
            "createdAt": "2026-07-24T00:00:07Z"
        }))
        .unwrap()];

        let report = store.reconcile_transition(&previous, &next).unwrap();
        assert_eq!(report.quarantined, 0);
        assert!(store.path_for_id(&image.id).unwrap().exists());
    }

    #[test]
    fn startup_restores_crash_references_before_deleting_expired_quarantine() {
        let temp = tempfile::tempdir().unwrap();
        let store = ImageAttachmentStore::new(temp.path());
        let image = store.import("pixel.png", PNG).unwrap();
        let canonical = store.read_bytes(&image).unwrap();
        let referenced = document_with_references(temp.path(), &image);
        let empty = crate::catalog::default_document(temp.path());

        store.reconcile_transition(&referenced, &empty).unwrap();
        let quarantined = store.quarantine_path_for_id(&image.id).unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&quarantined)
            .unwrap()
            .set_modified(SystemTime::now() - QUARANTINE_RETENTION - Duration::from_secs(1))
            .unwrap();

        let report = store.reconcile_startup(&referenced).unwrap();
        assert_eq!(report.restored, 1);
        assert_eq!(report.deleted, 0);
        assert_eq!(store.read_bytes(&image).unwrap(), canonical);
    }

    #[test]
    fn startup_deletes_only_expired_unreferenced_quarantine_entries() {
        let temp = tempfile::tempdir().unwrap();
        let store = ImageAttachmentStore::new(temp.path());
        let image = store.import("pixel.png", PNG).unwrap();
        let referenced = document_with_references(temp.path(), &image);
        let empty = crate::catalog::default_document(temp.path());

        store.reconcile_transition(&referenced, &empty).unwrap();
        let quarantined = store.quarantine_path_for_id(&image.id).unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&quarantined)
            .unwrap()
            .set_modified(SystemTime::now() - QUARANTINE_RETENTION - Duration::from_secs(1))
            .unwrap();

        let report = store.reconcile_startup(&empty).unwrap();
        assert_eq!(report.deleted, 1);
        assert!(!quarantined.exists());
    }

    #[test]
    fn startup_quarantines_old_never_persisted_import_without_same_pass_deletion() {
        let temp = tempfile::tempdir().unwrap();
        let store = ImageAttachmentStore::new(temp.path());
        let image = store.import("pixel.png", PNG).unwrap();
        let active = store.path_for_id(&image.id).unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&active)
            .unwrap()
            .set_modified(SystemTime::now() - ORPHAN_GRACE_PERIOD - Duration::from_secs(1))
            .unwrap();

        let empty = crate::catalog::default_document(temp.path());
        let report = store.reconcile_startup(&empty).unwrap();
        assert_eq!(report.quarantined, 1);
        assert_eq!(report.deleted, 0);
        assert!(store.quarantine_path_for_id(&image.id).unwrap().exists());
    }

    #[test]
    fn explicit_purge_removes_only_the_fixed_attachment_root() {
        let temp = tempfile::tempdir().unwrap();
        let store = ImageAttachmentStore::new(temp.path());
        store.import("pixel.png", PNG).unwrap();
        let unrelated = temp.path().join("unrelated.txt");
        fs::write(&unrelated, b"keep").unwrap();

        store.purge_all().unwrap();
        assert!(!store.root.exists());
        assert_eq!(fs::read(&unrelated).unwrap(), b"keep");
    }

    #[test]
    fn refuses_non_directory_attachment_roots_without_touching_them() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("image-attachments");
        fs::write(&root, b"not a directory").unwrap();
        let store = ImageAttachmentStore::new(temp.path());

        assert!(store
            .import("pixel.png", PNG)
            .unwrap_err()
            .contains("not a regular directory"));
        assert!(store.purge_all().unwrap_err().contains("not a regular directory"));
        assert_eq!(fs::read(root).unwrap(), b"not a directory");
    }
}
