//! Content-addressed attachment directory shared by the image and file
//! attachment stores.
//!
//! An entry is named by the full lowercase SHA-256 of its primary bytes and
//! may own companion files named `<id><suffix>` — a PDF's extracted text is
//! one. Every lifecycle step (restore, quarantine, the orphan sweep, expiry)
//! moves or deletes an entry's files together, so a companion can neither
//! outlive the bytes it describes nor be separated from them by a crash that a
//! later pass cannot finish.
//!
//! Nothing here knows what the bytes are; each store validates its own
//! content, owns its own lock, and names itself in error messages.

use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
    time::{Duration, SystemTime},
};

use sha2::{Digest, Sha256};

const QUARANTINE_DIRECTORY: &str = ".orphaned";
/// How long an active entry nothing references is left alone. A fresh import
/// belongs to a composer draft until the message that carries it is saved.
pub(crate) const ORPHAN_GRACE_PERIOD: Duration = Duration::from_secs(24 * 60 * 60);
/// How long a quarantined entry stays restorable before startup deletes it.
pub(crate) const QUARANTINE_RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub restored: usize,
    pub quarantined: usize,
    pub deleted: usize,
}

/// How a store names itself in errors: `noun` mid-sentence ("image
/// attachment"), `title` at the start of one ("Image attachment").
#[derive(Clone, Copy, Debug)]
pub(crate) struct Label {
    pub noun: &'static str,
    pub title: &'static str,
}

#[derive(Clone, Debug)]
pub(crate) struct ContentDirectory {
    root: PathBuf,
    label: Label,
    lock: &'static Mutex<()>,
    /// Suffixes of the files an entry may own besides its primary bytes.
    companions: &'static [&'static str],
}

impl ContentDirectory {
    pub(crate) fn new(
        root: PathBuf,
        label: Label,
        lock: &'static Mutex<()>,
        companions: &'static [&'static str],
    ) -> Self {
        Self {
            root,
            label,
            lock,
            companions,
        }
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// Serializes every filesystem step of this store. Methods named
    /// `*_locked` expect the caller to hold it.
    pub(crate) fn lock(&self) -> MutexGuard<'static, ()> {
        self.lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Checks that the root is a real directory directly inside the app-data
    /// directory, creating it when asked. `None` means it does not exist yet.
    pub(crate) fn ensure_root(&self, create: bool) -> Result<Option<()>, String> {
        let parent = self.root.parent().ok_or_else(|| {
            format!(
                "{} root directory has no parent directory",
                self.label.title
            )
        })?;
        ensure_directory_within(
            &self.root,
            parent,
            create,
            &format!("{} directory", self.label.noun),
        )
    }

    pub(crate) fn path_for_id(&self, id: &str) -> Result<PathBuf, String> {
        self.validate_id(id)?;
        Ok(self.root.join(id))
    }

    pub(crate) fn companion_path(&self, id: &str, suffix: &str) -> Result<PathBuf, String> {
        self.validate_id(id)?;
        debug_assert!(self.companions.contains(&suffix));
        Ok(self.root.join(format!("{id}{suffix}")))
    }

    #[cfg(test)]
    pub(crate) fn quarantine_path_for_id(&self, id: &str) -> Result<PathBuf, String> {
        self.validate_id(id)?;
        Ok(self.root.join(QUARANTINE_DIRECTORY).join(id))
    }

    #[cfg(test)]
    pub(crate) fn quarantine_companion_path(
        &self,
        id: &str,
        suffix: &str,
    ) -> Result<PathBuf, String> {
        self.validate_id(id)?;
        Ok(self
            .root
            .join(QUARANTINE_DIRECTORY)
            .join(format!("{id}{suffix}")))
    }

    fn validate_id(&self, id: &str) -> Result<(), String> {
        if !is_sha256_hex(id) {
            return Err(format!(
                "{} ID must be a full lowercase SHA-256 digest",
                self.label.title
            ));
        }
        Ok(())
    }

    /// File names an entry may occupy, primary first.
    fn part_names(&self, id: &str) -> impl Iterator<Item = String> + '_ {
        let id = id.to_owned();
        std::iter::once(id.clone()).chain(
            self.companions
                .iter()
                .map(move |suffix| format!("{id}{suffix}")),
        )
    }

    /// The entry id a directory listing name belongs to, if it is one of ours.
    fn entry_id(&self, name: &str) -> Option<String> {
        if is_sha256_hex(name) {
            return Some(name.to_owned());
        }
        self.companions.iter().find_map(|suffix| {
            let id = name.strip_suffix(suffix)?;
            is_sha256_hex(id).then(|| id.to_owned())
        })
    }

    fn ensure_regular_metadata(&self, metadata: &fs::Metadata, path: &Path) -> Result<(), String> {
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(format!(
                "{} path {} is not a regular file; access is denied",
                self.label.title,
                path.display()
            ));
        }
        Ok(())
    }

    fn ensure_quarantine(&self, create: bool) -> Result<Option<()>, String> {
        ensure_directory_within(
            &self.root.join(QUARANTINE_DIRECTORY),
            &self.root,
            create,
            &format!("{} quarantine directory", self.label.noun),
        )
    }

    /// Moves every quarantined file of `id` whose active copy is missing back
    /// into place. Each file is restored on its own, so an entry a crash left
    /// half-quarantined comes back whole.
    pub(crate) fn restore_quarantined_locked(&self, id: &str) -> Result<bool, String> {
        let Some(()) = self.ensure_root(false)? else {
            return Ok(false);
        };
        let noun = self.label.noun;
        self.validate_id(id)?;
        let mut quarantine_present = None;
        let mut restored = false;
        for name in self.part_names(id) {
            let active = self.root.join(&name);
            match fs::symlink_metadata(&active) {
                Ok(metadata) => {
                    self.ensure_regular_metadata(&metadata, &active)?;
                    continue;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!(
                        "Could not inspect {noun} path {}: {error}",
                        active.display()
                    ))
                }
            }

            let present = match quarantine_present {
                Some(present) => present,
                None => *quarantine_present.insert(self.ensure_quarantine(false)?.is_some()),
            };
            if !present {
                return Ok(restored);
            }
            let quarantined = self.root.join(QUARANTINE_DIRECTORY).join(&name);
            let metadata = match fs::symlink_metadata(&quarantined) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(format!(
                        "Could not read quarantined {noun} {}: {error}",
                        quarantined.display()
                    ))
                }
            };
            self.ensure_regular_metadata(&metadata, &quarantined)?;
            fs::rename(&quarantined, &active)
                .map_err(|error| format!("Could not restore {noun} {name}: {error}"))?;
            restored = true;
        }
        Ok(restored)
    }

    /// Moves every active file of `id` into the quarantine and stamps it with
    /// `now`, which is what expiry is measured from.
    pub(crate) fn quarantine_locked(&self, id: &str, now: SystemTime) -> Result<bool, String> {
        let Some(()) = self.ensure_root(false)? else {
            return Ok(false);
        };
        let noun = self.label.noun;
        self.validate_id(id)?;
        let mut moved = false;
        for name in self.part_names(id) {
            let active = self.root.join(&name);
            let metadata = match fs::symlink_metadata(&active) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(format!(
                        "Could not inspect {noun} path {}: {error}",
                        active.display()
                    ))
                }
            };
            self.ensure_regular_metadata(&metadata, &active)?;

            self.ensure_quarantine(true)?;
            let target = self.root.join(QUARANTINE_DIRECTORY).join(&name);
            match fs::symlink_metadata(&target) {
                Ok(existing) => {
                    self.ensure_regular_metadata(&existing, &target)?;
                    fs::remove_file(&target).map_err(|error| {
                        format!("Could not replace quarantined {noun} {name}: {error}")
                    })?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!("Could not read quarantined {noun} {name}: {error}"))
                }
            }
            fs::rename(&active, &target)
                .map_err(|error| format!("Could not quarantine {noun} {name}: {error}"))?;
            let touch_result = fs::OpenOptions::new()
                .write(true)
                .open(&target)
                .and_then(|file| file.set_modified(now));
            if let Err(error) = touch_result {
                let _ = fs::rename(&target, &active);
                return Err(format!(
                    "Could not record quarantine time for {noun} {name}: {error}"
                ));
            }
            moved = true;
        }
        Ok(moved)
    }

    /// Quarantines active entries nothing references once every file they own
    /// is older than the grace period.
    fn quarantine_old_unreferenced_locked(
        &self,
        referenced: &HashSet<String>,
        now: SystemTime,
    ) -> Result<usize, String> {
        let Some(()) = self.ensure_root(false)? else {
            return Ok(0);
        };
        let noun = self.label.noun;
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(format!("Could not read {noun} directory: {error}")),
        };
        // id → whether every file seen so far qualifies.
        let mut candidates = HashMap::<String, bool>::new();
        for entry in entries {
            let entry =
                entry.map_err(|error| format!("Could not read {noun} directory entry: {error}"))?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(id) = self.entry_id(&name) else {
                continue;
            };
            if referenced.contains(&id) {
                continue;
            }
            let metadata = entry
                .metadata()
                .map_err(|error| format!("Could not read metadata for {noun} {name}: {error}"))?;
            let eligible = entry
                .file_type()
                .map_err(|error| format!("Could not read file type for {noun} {name}: {error}"))?
                .is_file()
                && is_old_enough(&metadata, now, ORPHAN_GRACE_PERIOD);
            *candidates.entry(id).or_insert(true) &= eligible;
        }
        let mut quarantined = 0;
        for (id, eligible) in candidates {
            if eligible {
                quarantined += usize::from(self.quarantine_locked(&id, now)?);
            }
        }
        Ok(quarantined)
    }

    /// Deletes quarantined entries nothing references once every file they own
    /// has been quarantined for longer than the retention period.
    fn delete_expired_quarantine_locked(
        &self,
        referenced: &HashSet<String>,
        now: SystemTime,
    ) -> Result<usize, String> {
        let Some(()) = self.ensure_quarantine(false)? else {
            return Ok(0);
        };
        let noun = self.label.noun;
        let quarantine = self.root.join(QUARANTINE_DIRECTORY);
        let entries = match fs::read_dir(&quarantine) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => {
                return Err(format!(
                    "Could not read {noun} quarantine directory: {error}"
                ))
            }
        };
        // id → (every file expired, the files to delete).
        let mut expired = HashMap::<String, (bool, Vec<(String, PathBuf)>)>::new();
        for entry in entries {
            let entry = entry.map_err(|error| {
                format!("Could not read quarantined {noun} directory entry: {error}")
            })?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(id) = self.entry_id(&name) else {
                continue;
            };
            if referenced.contains(&id) {
                continue;
            }
            let file_type = entry.file_type().map_err(|error| {
                format!("Could not read file type for quarantined {noun} {name}: {error}")
            })?;
            let group = expired.entry(id).or_insert_with(|| (true, Vec::new()));
            if !file_type.is_file() {
                group.0 = false;
                continue;
            }
            let metadata = entry.metadata().map_err(|error| {
                format!("Could not read metadata for quarantined {noun} {name}: {error}")
            })?;
            group.0 &= is_old_enough(&metadata, now, QUARANTINE_RETENTION);
            group.1.push((name, entry.path()));
        }
        let mut deleted = 0;
        for (all_expired, files) in expired.into_values() {
            if !all_expired || files.is_empty() {
                continue;
            }
            for (name, path) in files {
                fs::remove_file(path).map_err(|error| {
                    format!("Could not delete expired quarantined {noun} {name}: {error}")
                })?;
            }
            deleted += 1;
        }
        Ok(deleted)
    }

    /// Reconciles a committed document transition without physically deleting
    /// anything. `previous` and `next` are the ids each snapshot references;
    /// `next` must already include every pinned id.
    ///
    /// Reads and same-content imports transparently restore quarantined bytes,
    /// which keeps this safe while a model/tool request still holds an older
    /// in-memory snapshot, and keeps crash recovery possible while the document
    /// writer is still flushing the new one.
    pub(crate) fn reconcile_transition(
        &self,
        previous: &HashSet<String>,
        next: &HashSet<String>,
    ) -> Result<ReconcileReport, String> {
        let quarantine_ids = previous.difference(next).cloned().collect::<HashSet<_>>();
        let now = SystemTime::now();
        let _guard = self.lock();
        let mut report = ReconcileReport::default();

        for id in next {
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
        report.quarantined += self.quarantine_old_unreferenced_locked(next, now)?;
        Ok(report)
    }

    /// Startup-only reconciliation: referenced crash-recovery entries are
    /// restored first, expired quarantine entries are then physically removed,
    /// and old unreferenced active entries are quarantined last so they cannot
    /// be deleted in the same pass. `referenced` must include every pinned id.
    pub(crate) fn reconcile_startup(
        &self,
        referenced: &HashSet<String>,
    ) -> Result<ReconcileReport, String> {
        let now = SystemTime::now();
        let _guard = self.lock();
        let mut report = ReconcileReport::default();

        for id in referenced {
            report.restored += usize::from(self.restore_quarantined_locked(id)?);
        }
        report.deleted += self.delete_expired_quarantine_locked(referenced, now)?;
        report.quarantined += self.quarantine_old_unreferenced_locked(referenced, now)?;
        Ok(report)
    }

    /// Used only by the explicit full-document reset after its durability
    /// barrier and dependency fence have succeeded.
    pub(crate) fn purge_all(&self) -> Result<(), String> {
        let _guard = self.lock();
        let noun = self.label.noun;
        let metadata = match fs::symlink_metadata(&self.root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(format!("Could not read {noun} directory: {error}")),
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(format!(
                "{} root path is not a regular directory; recursive cleanup is denied",
                self.label.title
            ));
        }
        self.ensure_root(false)?;
        fs::remove_dir_all(&self.root)
            .map_err(|error| format!("Could not remove {noun} directory: {error}"))
    }
}

pub(crate) fn ensure_directory_within(
    path: &Path,
    parent: &Path,
    create: bool,
    label: &str,
) -> Result<Option<()>, String> {
    if create {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(format!(
                    "{label} is not a regular directory; access is denied"
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir_all(path)
                    .map_err(|error| format!("Could not create {label}: {error}"))?;
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
        return Err(format!(
            "{label} is not a regular directory; access is denied"
        ));
    }
    let canonical_parent = fs::canonicalize(parent)
        .map_err(|error| format!("Could not canonicalize parent directory of {label}: {error}"))?;
    let canonical_path = fs::canonicalize(path)
        .map_err(|error| format!("Could not canonicalize {label}: {error}"))?;
    if canonical_path.parent() != Some(canonical_parent.as_path()) {
        return Err(format!(
            "{label} escapes the fixed application-data path; access is denied"
        ));
    }
    Ok(Some(()))
}

/// Reads a regular, non-symlink file of at most `limit` bytes. `title` names
/// the store in the error ("Image attachment").
pub(crate) fn read_regular_file(
    path: &Path,
    limit: usize,
    title: &str,
) -> std::io::Result<Vec<u8>> {
    let too_large = || {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{title} exceeds the {} read limit", format_limit(limit)),
        )
    };
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(std::io::Error::other(format!(
            "{title} path is not a regular file"
        )));
    }
    if metadata.len() > limit as u64 {
        return Err(too_large());
    }

    let file = fs::File::open(path)?;
    let opened_metadata = file.metadata()?;
    if !opened_metadata.is_file() {
        return Err(std::io::Error::other(format!(
            "{title} path is not a regular file"
        )));
    }
    if opened_metadata.len() > limit as u64 {
        return Err(too_large());
    }

    // The file may grow or be replaced after either metadata check. Reading at
    // most limit + 1 keeps that race bounded; the extra byte distinguishes an
    // exact-limit file from an oversized one without allocating the whole file.
    let mut bytes = Vec::with_capacity(opened_metadata.len() as usize);
    file.take((limit + 1) as u64).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(too_large());
    }
    Ok(bytes)
}

/// `5 MiB`, `512 KiB`: whole mebibytes when the limit is one, else kibibytes.
pub(crate) fn format_limit(bytes: usize) -> String {
    const MIB: usize = 1024 * 1024;
    if bytes >= MIB && bytes % MIB == 0 {
        format!("{} MiB", bytes / MIB)
    } else {
        format!("{} KiB", bytes.div_ceil(1024))
    }
}

pub(crate) fn is_old_enough(metadata: &fs::Metadata, now: SystemTime, minimum: Duration) -> bool {
    metadata
        .modified()
        .ok()
        .and_then(|modified| now.duration_since(modified).ok())
        .is_some_and(|age| age >= minimum)
}

pub(crate) fn is_sha256_hex(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(crate) fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
