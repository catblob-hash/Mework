//! In-memory authoritative snapshot of the persisted document plus a
//! background writer.
//!
//! Every IPC command used to re-read and re-validate the whole document from
//! disk (O(document) JSON parse + shape walk under the global storage lock,
//! measured in seconds on real data). The store keeps one validated
//! `Arc<AppDocument>` in memory: reads are an `Arc` clone, commits swap the
//! snapshot synchronously and persist asynchronously with latest-wins
//! coalescing. The disk file remains the crash-recovery source of truth; the
//! writer fsyncs exactly like the old synchronous path did.
//!
//! Consistency rules:
//! - Commands keep serializing document read→validate→commit sections with
//!   `AppState::storage_lock` exactly as before; this module adds no new
//!   cross-command ordering assumptions.
//! - While no commit is pending, an external change to the file on disk
//!   (for example, a manual recovery edit) is detected via (mtime, len) and
//!   triggers a reload, matching the old always-read-from-disk behaviour.
//!   Current Mework builds deliberately hold one exclusive app-data lease,
//!   so two live processes cannot race document and attachment state.
//! - A failed background write is reported by the next `commit` (and retried
//!   with each later commit and on `flush`), so the renderer still surfaces
//!   "save failure" without blocking the interactive path. In addition, the
//!   optional write-failure observer (`set_write_failure_observer`) is
//!   invoked the moment the background writer fails — once per distinct
//!   failure, not per retry — and again with `None` when a later attempt
//!   succeeds, so the renderer can be told immediately over the push-event
//!   channel instead of waiting for its next save.

use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    thread,
    time::{Duration, SystemTime},
};

use crate::{model::AppDocument, storage, wire_history};

/// How long the writer waits before retrying a failed write when no newer
/// commit supersedes the failed one.
const WRITE_RETRY_DELAY: Duration = Duration::from_secs(1);

/// Callback for background write-state transitions: `Some(message)` on a new
/// failure, `None` when a write succeeds after a reported failure. Invoked on
/// the writer thread with no store lock held.
pub type WriteFailureObserver = Box<dyn Fn(Option<String>) + Send + Sync>;

#[derive(Clone, Default)]
pub struct DocumentStore {
    shared: Arc<Shared>,
}

#[derive(Default)]
struct Shared {
    state: Mutex<StoreState>,
    wake: Condvar,
    write_failure_observer: Mutex<Option<WriteFailureObserver>>,
}

#[derive(Default)]
struct StoreState {
    snapshot: Option<Snapshot>,
    pending: Option<PendingWrite>,
    write_in_flight: bool,
    writer_started: bool,
    last_write_error: Option<String>,
    /// The failure message most recently delivered to the write-failure
    /// observer, if any. Tracks observer reporting only — `commit`'s
    /// stale-error return path deliberately leaves it untouched, so a
    /// success after any reported failure produces exactly one recovery
    /// notification and retries of one unchanged failure produce none.
    reported_write_failure: Option<String>,
    /// Cooperative cross-process lifetime lease for this app-data document.
    ///
    /// The current Mework process holds an exclusive lock for its entire
    /// lifetime. Document writes, composer drafts, request snapshots, image
    /// reads/imports, transition quarantine, startup GC, and full reset all
    /// share one app-data authority. This intentionally makes the app-data
    /// directory single-instance: a short migration lock cannot protect an
    /// unpersisted attachment reference after an upload call returns.
    process_lease: Option<ProcessLease>,
    ipc_ready: bool,
}

struct ProcessLease {
    _file: File,
    document_path: PathBuf,
}

struct Snapshot {
    document: Arc<AppDocument>,
    path: PathBuf,
    /// Fingerprint of the anchor file the snapshot mirrors. Conversation data
    /// lives in the SQLite store, which the host owns exclusively — there is no
    /// second writer whose edits an anchor fingerprint could miss.
    disk_meta: LayoutDiskMeta,
}

struct PendingWrite {
    document: Arc<AppDocument>,
    path: PathBuf,
}

/// (mtime, len, creation) fingerprint of the disk file. Creation time is
/// meaningful on Windows (0 elsewhere): an external temp+rename replacement
/// necessarily creates a new file, so even a forged same-length/same-mtime
/// swap changes the creation stamp. Only an in-place overwrite with a
/// restored mtime and identical length remains invisible — that requires
/// deliberate tampering, and detecting it would cost a full content hash on
/// every read.
type DiskMeta = (SystemTime, u64, u64);
type LayoutDiskMeta = Option<DiskMeta>;

fn read_disk_meta(path: &Path) -> Option<DiskMeta> {
    let meta = std::fs::metadata(path).ok()?;
    #[cfg(windows)]
    let created = {
        use std::os::windows::fs::MetadataExt;
        meta.creation_time()
    };
    #[cfg(not(windows))]
    let created = 0u64;
    Some((meta.modified().ok()?, meta.len(), created))
}

fn read_layout_disk_meta(anchor: &Path) -> LayoutDiskMeta {
    read_disk_meta(anchor)
}

fn process_lease_path(path: &Path) -> Result<PathBuf, String> {
    let parent = path
        .parent()
        .ok_or_else(|| "数据文档没有父目录，无法建立跨进程租约".to_owned())?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "数据文档文件名无效，无法建立跨进程租约".to_owned())?;
    Ok(parent.join(format!(".{name}.instance.lock")))
}

fn open_process_lease(path: &Path) -> Result<File, String> {
    let lock_path = process_lease_path(path)?;
    let parent = lock_path
        .parent()
        .ok_or_else(|| "跨进程数据租约没有父目录".to_owned())?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("无法创建 Mework 应用数据目录: {error}"))?;
    match std::fs::symlink_metadata(&lock_path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(format!(
                "Mework 跨进程数据租约路径 {} 不是普通文件，拒绝启动",
                lock_path.display()
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "无法检查 Mework 跨进程数据租约 {}: {error}",
                lock_path.display()
            ));
        }
    }

    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        // The locked pathname must not be renamed/deleted and replaced with a
        // fresh unlocked file. OPEN_REPARSE_POINT plus the post-open metadata
        // check also rejects a symlink that wins the pre-open race.
        options
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(&lock_path).map_err(|error| {
        format!(
            "无法打开 Mework 跨进程数据租约 {}: {error}",
            lock_path.display()
        )
    })?;
    let metadata = file.metadata().map_err(|error| {
        format!(
            "无法验证 Mework 跨进程数据租约 {}: {error}",
            lock_path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "Mework 跨进程数据租约路径 {} 不是普通文件，拒绝启动",
            lock_path.display()
        ));
    }
    Ok(file)
}

/// Why the exclusive app-data lease could not be taken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessAuthorityError {
    /// A live Mework process already holds the lease on this app data.
    Contended,
    Failed(String),
}

impl std::fmt::Display for ProcessAuthorityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Contended => formatter.write_str(
                "另一个 Mework 实例正在使用同一应用数据；为保护对话、草稿和图片附件，当前实例已拒绝启动",
            ),
            Self::Failed(message) => formatter.write_str(message),
        }
    }
}

impl From<ProcessAuthorityError> for String {
    fn from(error: ProcessAuthorityError) -> Self {
        error.to_string()
    }
}

fn acquire_exclusive_process_lease(path: &Path) -> Result<ProcessLease, ProcessAuthorityError> {
    let file = open_process_lease(path).map_err(ProcessAuthorityError::Failed)?;
    fs2::FileExt::try_lock_exclusive(&file).map_err(|error| {
        let contended = fs2::lock_contended_error();
        let is_contended = match contended.raw_os_error() {
            Some(expected) => error.raw_os_error() == Some(expected),
            None => error.kind() == contended.kind(),
        };
        if is_contended {
            ProcessAuthorityError::Contended
        } else {
            ProcessAuthorityError::Failed(format!("无法取得 Mework 独占应用数据租约: {error}"))
        }
    })?;
    Ok(ProcessLease {
        _file: file,
        document_path: path.to_owned(),
    })
}

impl DocumentStore {
    pub fn is_ipc_ready(&self) -> bool {
        let state = self.lock();
        state.process_lease.is_some() && state.ipc_ready
    }

    pub fn mark_ipc_ready(&self) -> Result<(), String> {
        let mut state = self.lock();
        if state.process_lease.is_none() {
            return Err("独占应用数据租约尚未建立，拒绝开放 IPC".into());
        }
        state.ipc_ready = true;
        Ok(())
    }

    /// Acquires the single-instance app-data authority before document load,
    /// renderer startup, or any attachment command. The failure stays typed so
    /// the desktop launcher can tell a live sibling instance from a broken lease.
    pub fn acquire_process_authority(&self, path: &Path) -> Result<(), ProcessAuthorityError> {
        let mut state = self.lock();
        match state.process_lease.as_ref() {
            Some(lease) if lease.document_path == path => return Ok(()),
            Some(_) => {
                return Err(ProcessAuthorityError::Failed(
                    "当前独占应用数据租约属于另一应用数据路径".into(),
                ))
            }
            None => {}
        }
        state.process_lease = Some(acquire_exclusive_process_lease(path)?);
        Ok(())
    }

    /// Initializes the authoritative document under a cooperative
    /// cross-process authority and runs startup-only attachment reconciliation.
    ///
    /// The document is read only after the lifetime exclusive lease is held.
    /// A second current Mework process fails before it can read, write,
    /// migrate, import, hydrate, quarantine, or purge the shared app-data
    /// directory.
    pub fn initialize_with_startup_reconciliation<F>(
        &self,
        path: &Path,
        default_workspace: &Path,
        reconcile: F,
    ) -> Result<Arc<AppDocument>, String>
    where
        F: FnOnce(&AppDocument) -> Result<(), String>,
    {
        let mut state = self.lock();
        match state.process_lease.as_ref() {
            Some(lease) if lease.document_path == path => {}
            Some(_) => return Err("当前独占应用数据租约属于另一应用数据路径".into()),
            None => {
                state.process_lease = Some(acquire_exclusive_process_lease(path)?);
            }
        }

        // Keep the lifetime lease across the exact read -> ordinary startup GC
        // transaction and every later operation in this process.
        //
        // Startup is a recovery boundary: quarantine failed loads and rebuild from the seed.
        // Setup must not bring down the application; non-startup reads still use strict
        // `load_or_initialize`.
        let loaded = storage::load_or_recover(path, default_workspace);
        let reconciled = loaded
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|document| reconcile(document));
        let document = Arc::new(loaded?);
        state.snapshot = Some(Snapshot {
            document: document.clone(),
            path: path.to_owned(),
            disk_meta: read_layout_disk_meta(path),
        });
        reconciled?;
        Ok(document)
    }

    /// Returns the already-loaded authoritative in-memory snapshot for a trusted path.
    ///
    /// Long-running capability checks use this instead of re-reading the disk file: a validated
    /// settings save updates the snapshot before its asynchronous writer reaches disk. Missing or
    /// mismatched state fails closed rather than reconstructing policy with an arbitrary fallback
    /// workspace.
    pub fn current_snapshot(&self, path: &Path) -> Result<Arc<AppDocument>, String> {
        let state = self.lock();
        match state.snapshot.as_ref() {
            Some(snapshot) if snapshot.path == path => Ok(snapshot.document.clone()),
            Some(_) => Err("当前文档快照属于另一应用数据路径".into()),
            None => Err("当前文档快照尚未加载".into()),
        }
    }

    /// Returns the current document snapshot, loading (and migrating +
    /// validating) it from disk only on first use, after `invalidate`, or when
    /// the file changed under us while nothing of ours was being written.
    pub fn read(&self, path: &Path, default_workspace: &Path) -> Result<Arc<AppDocument>, String> {
        let mut state = self.lock();
        match state.process_lease.as_ref() {
            Some(lease) if lease.document_path == path => {}
            Some(_) => return Err("当前独占应用数据租约属于另一应用数据路径".into()),
            None => {
                state.process_lease = Some(acquire_exclusive_process_lease(path)?);
            }
        }
        let writer_idle = state.pending.is_none() && !state.write_in_flight;
        if let Some(snapshot) = state.snapshot.as_mut() {
            if snapshot.path == path {
                // While our own write is pending or in flight the anchor on disk
                // is legitimately behind/being replaced; skip external detection.
                let externally_modified =
                    writer_idle && snapshot.disk_meta != read_layout_disk_meta(path);
                if !externally_modified {
                    return Ok(snapshot.document.clone());
                }
            }
        }
        let document = Arc::new(storage::load_or_initialize(path, default_workspace)?);
        state.snapshot = Some(Snapshot {
            document: document.clone(),
            path: path.to_owned(),
            disk_meta: read_layout_disk_meta(path),
        });
        Ok(document)
    }

    /// Swaps the in-memory snapshot to `document` and queues the disk write.
    /// The caller must have validated the document already; this method never
    /// blocks on I/O. If a previous background write failed, that error is
    /// returned now (the new commit still supersedes it and will be retried).
    pub fn commit(&self, path: &Path, document: AppDocument) -> Result<(), String> {
        let document = Arc::new(document);
        let mut state = self.lock();
        match state.process_lease.as_ref() {
            Some(lease) if lease.document_path == path => {}
            Some(_) => return Err("当前独占应用数据租约属于另一应用数据路径".into()),
            None => return Err("独占应用数据租约尚未建立，拒绝保存".into()),
        }
        state.snapshot = Some(Snapshot {
            document: document.clone(),
            path: path.to_owned(),
            disk_meta: state
                .snapshot
                .take()
                .filter(|snapshot| snapshot.path == path)
                .map(|snapshot| snapshot.disk_meta)
                .unwrap_or_else(|| read_layout_disk_meta(path)),
        });
        state.pending = Some(PendingWrite {
            document: document.clone(),
            path: path.to_owned(),
        });
        let stale_error = state.last_write_error.take();
        let writer_available = self.ensure_writer(&mut state);
        // Fall back to a synchronous write if the background writer cannot start; otherwise commit could succeed without bytes reaching disk.
        let synchronous_job = if writer_available {
            None
        } else {
            state.pending.take()
        };
        self.shared.wake.notify_all();
        drop(state);
        if let Some(job) = synchronous_job {
            let result = storage::save_unchecked(&job.path, &job.document);
            let mut state = self.lock();
            match &result {
                Ok(()) => {
                    if state.pending.is_none() && !state.write_in_flight {
                        if let Some(snapshot) = &mut state.snapshot {
                            if snapshot.path == job.path {
                                snapshot.disk_meta = read_layout_disk_meta(&job.path);
                            }
                        }
                    }
                }
                Err(error) => state.last_write_error = Some(error.clone()),
            }
            drop(state);
            result?;
        }
        // Keep the wire-format projections in step with the committed document:
        // prune dead conversations, mirror manual context edits, and eagerly
        // re-project when the active provider/model switched wire formats.
        wire_history::on_document_committed(&document);
        match stale_error {
            Some(error) => Err(format!(
                "上一次后台保存失败：{error}；本次更改已重新排队写入"
            )),
            None => Ok(()),
        }
    }

    /// Blocks until every queued write has settled, returning the final error.
    /// A timeout never starts a competing write while the writer is in flight:
    /// two atomic replacements are individually safe but not ordered, so the
    /// older writer could finish last and restore stale data after reset.
    pub fn flush(&self, timeout: Duration) -> Result<(), String> {
        let deadline = std::time::Instant::now() + timeout;
        let mut state = self.lock();
        loop {
            if state.pending.is_none() && !state.write_in_flight {
                return match &state.last_write_error {
                    Some(error) => Err(error.clone()),
                    None => Ok(()),
                };
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                if state.write_in_flight {
                    return Err(
                        "等待文档落盘超时；仍有后台写入在途，未启动会与它乱序的同步写"
                            .into(),
                    );
                }
                let Some(job) = state.pending.take() else {
                    return Err("后台保存尚未完成".into());
                };
                drop(state);
                let result = storage::save_unchecked(&job.path, &job.document)
                    .map_err(|error| format!("后台保存超时后同步兜底写入也失败：{error}"));
                let mut state = self.lock();
                match &result {
                    Ok(()) => {
                        state.last_write_error = None;
                        if let Some(snapshot) = &mut state.snapshot {
                            if snapshot.path == job.path {
                                snapshot.disk_meta = read_layout_disk_meta(&job.path);
                            }
                        }
                    }
                    Err(error) => {
                        state.last_write_error = Some(error.clone());
                        state.pending = Some(job);
                    }
                }
                self.shared.wake.notify_all();
                return result;
            }
            let (next, _) = self
                .shared
                .wake
                .wait_timeout(state, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next;
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, StoreState> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Starts the background writer once. Returns whether a writer is
    /// available — `false` means the spawn failed and the caller must not
    /// assume queued bytes will ever be consumed.
    fn ensure_writer(&self, state: &mut StoreState) -> bool {
        if state.writer_started {
            return true;
        }
        state.writer_started = true;
        let shared = self.shared.clone();
        // The writer parks on the condvar between commits; it is deliberately
        // detached — `flush` provides the shutdown barrier.
        let spawned = thread::Builder::new()
            .name("mework-document-writer".into())
            .spawn(move || writer_loop(&shared));
        if spawned.is_err() {
            state.writer_started = false;
            return false;
        }
        true
    }

    /// Installs the process-wide background write-failure observer. The
    /// synchronous fallback paths (`commit` without a writer thread, `flush`
    /// timeout takeover) report their errors directly to their callers and
    /// never through this observer.
    pub fn set_write_failure_observer(&self, observer: WriteFailureObserver) {
        let mut slot = self
            .shared
            .write_failure_observer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *slot = Some(observer);
    }
}

fn notify_write_failure_observer(shared: &Shared, failure: Option<String>) {
    let observer = shared
        .write_failure_observer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(observer) = observer.as_ref() {
        observer(failure);
    }
}

fn writer_loop(shared: &Shared) {
    loop {
        let job = {
            let mut state = shared
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            loop {
                if let Some(job) = state.pending.take() {
                    state.write_in_flight = true;
                    break job;
                }
                state = shared
                    .wake
                    .wait(state)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
        };

        let result = storage::save_unchecked(&job.path, &job.document);

        let mut state = shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.write_in_flight = false;
        match result {
            Ok(()) => {
                state.last_write_error = None;
                let recovered = state.reported_write_failure.take().is_some();
                // Record the metadata of the bytes we just produced so external
                // change detection has a valid baseline once the queue drains.
                if state.pending.is_none() {
                    if let Some(snapshot) = &mut state.snapshot {
                        if snapshot.path == job.path {
                            snapshot.disk_meta = read_layout_disk_meta(&job.path);
                        }
                    }
                }
                shared.wake.notify_all();
                drop(state);
                if recovered {
                    notify_write_failure_observer(shared, None);
                }
            }
            Err(error) => {
                eprintln!("后台保存文档失败，将重试：{error}");
                state.last_write_error = Some(error.clone());
                let newly_reported =
                    state.reported_write_failure.as_deref() != Some(error.as_str());
                if newly_reported {
                    state.reported_write_failure = Some(error.clone());
                }
                let failed_document = Arc::as_ptr(&job.document);
                // Keep the failed document queued unless a newer commit already
                // superseded it, then back off so a persistently failing disk
                // does not spin.
                if state.pending.is_none() {
                    state.pending = Some(job);
                }
                shared.wake.notify_all();
                drop(state);
                if newly_reported {
                    notify_write_failure_observer(shared, Some(error));
                }
                let state = shared
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                // The observer ran outside the lock, so a commit may have
                // arrived in between; sleeping through its wakeup would delay
                // the superseding write by a full retry period.
                let still_the_failed_write = state
                    .pending
                    .as_ref()
                    .is_some_and(|pending| Arc::as_ptr(&pending.document) == failed_document);
                if still_the_failed_write {
                    let (next, _) = shared
                        .wake
                        .wait_timeout(state, WRITE_RETRY_DELAY)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    drop(next);
                } else {
                    drop(state);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::default_document;
    use crate::{
        image_attachments::ImageAttachmentStore,
        model::{ContextItem, ToolResult},
    };

    fn test_document() -> AppDocument {
        default_document(Path::new("C:/mework-store-test"))
    }

    const AUTHORITY_CHILD_PATH_ENV: &str = "MEWORK_TEST_AUTHORITY_CHILD_PATH";

    #[test]
    fn app_data_authority_child() {
        let Some(path) = std::env::var_os(AUTHORITY_CHILD_PATH_ENV).map(PathBuf::from) else {
            return;
        };
        let directory = path.parent().unwrap();
        let store = DocumentStore::default();
        store.acquire_process_authority(&path).unwrap();
        std::fs::write(directory.join("authority-child-ready"), b"ready").unwrap();
        let release = directory.join("authority-child-release");
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while !release.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "parent did not release authority child"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn read_loads_once_and_serves_snapshots() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.v1.json");
        let store = DocumentStore::default();
        let first = store.read(&path, directory.path()).unwrap();
        let second = store.read(&path, directory.path()).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert!(path.exists(), "first read initializes the file");
    }

    #[test]
    fn current_snapshot_never_loads_or_crosses_document_paths() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.v1.json");
        let other = directory.path().join("other.json");
        let store = DocumentStore::default();
        assert!(store.current_snapshot(&path).is_err());
        let loaded = store.read(&path, directory.path()).unwrap();
        assert!(Arc::ptr_eq(
            &loaded,
            &store.current_snapshot(&path).unwrap()
        ));
        assert!(store.current_snapshot(&other).is_err());
        assert!(!other.exists());
    }

    #[test]
    fn app_data_authority_is_exclusive_for_the_process_lifetime() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.v1.json");
        let first = DocumentStore::default();
        first.acquire_process_authority(&path).unwrap();

        let second = DocumentStore::default();
        let error = second.acquire_process_authority(&path).unwrap_err();
        assert_eq!(error, super::ProcessAuthorityError::Contended);
        assert!(error.to_string().contains("另一个 Mework 实例"), "{error}");

        drop(first);
        second.acquire_process_authority(&path).unwrap();
    }

    #[test]
    fn app_data_authority_rejects_a_real_second_process_and_recovers_after_exit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.v1.json");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("document_store::tests::app_data_authority_child")
            .arg("--nocapture")
            .env(AUTHORITY_CHILD_PATH_ENV, &path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let ready = directory.path().join("authority-child-ready");
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while !ready.exists() {
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let output = child.wait_with_output().unwrap();
                panic!(
                    "authority child did not become ready: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            assert!(
                child.try_wait().unwrap().is_none(),
                "authority child exited before publishing readiness"
            );
            std::thread::sleep(Duration::from_millis(20));
        }

        let store = DocumentStore::default();
        let error = store.acquire_process_authority(&path).unwrap_err();
        assert_eq!(error, super::ProcessAuthorityError::Contended);

        std::fs::write(directory.path().join("authority-child-release"), b"release").unwrap();
        let status = child.wait().unwrap();
        assert!(status.success(), "authority child failed: {status}");
        store.acquire_process_authority(&path).unwrap();
    }

    #[test]
    fn live_external_legacy_reload_is_rejected_without_deleting_transient_bytes() {
        const PNG: &[u8] = &[
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1,
            8, 6, 0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 8, 215, 99, 248, 207,
            192, 240, 31, 0, 5, 0, 1, 255, 114, 156, 82, 103, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66,
            96, 130,
        ];

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.json");
        let store = DocumentStore::default();
        let original = store.read(&path, directory.path()).unwrap();
        let attachment_store = ImageAttachmentStore::new(directory.path());
        let transient = attachment_store.import("transient.png", PNG).unwrap();

        let mut external = test_document();
        external.schema_version = 0;
        external.workspaces[0].conversations[0]
            .contexts
            .push(ContextItem::Tool {
                id: "legacy-external-screenshot".into(),
                tool_name: "playwright".into(),
                round: Some(1),
                model_turn_id: Some("legacy-external-turn".into()),
                requested_input: None,
                input: serde_json::json!({
                    "action": "screenshot",
                    "full_page": false
                })
                .as_object()
                .unwrap()
                .clone(),
                result: ToolResult {
                    success: true,
                    output: serde_json::json!({
                        "status": "completed",
                        "action": "screenshot"
                    })
                    .to_string(),
                    images: vec![transient.clone()],
                    diff: None,
                    executed_at: "2026-07-24T00:00:00Z".into(),
                    duration_ms: 1,
                },
                subagent: None,
                attestation: String::new(),
                created_at: "2026-07-24T00:00:00Z".into(),
            });
        storage::save_unchecked(&path, &external).unwrap();
        let bumped = std::time::SystemTime::now() + Duration::from_secs(2);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(bumped)
            .unwrap();

        let error = store.read(&path, directory.path()).unwrap_err();
        assert!(error.contains("旧版 schema 0"), "{error}");
        assert!(Arc::ptr_eq(
            &original,
            &store.current_snapshot(&path).unwrap()
        ));
        assert!(attachment_store.data_url_by_id(&transient.id).is_ok());
    }


    #[test]
    fn flush_timeout_never_starts_a_competing_write_while_writer_is_in_flight() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.v1.json");
        let store = DocumentStore::default();
        let document = Arc::new(test_document());
        {
            let mut state = store.lock();
            state.snapshot = Some(Snapshot {
                document: document.clone(),
                path: path.clone(),
                disk_meta: read_layout_disk_meta(&path),
            });
            state.pending = Some(PendingWrite {
                document,
                path: path.clone(),
            });
            state.write_in_flight = true;
        }

        let error = store.flush(Duration::ZERO).unwrap_err();

        assert!(error.contains("仍有后台写入在途"));
        let state = store.lock();
        assert!(state.write_in_flight);
        assert!(state.pending.is_some(), "latest write obligation must remain queued");
        assert!(!path.exists(), "flush must not start a competing fallback write");
    }

    fn set_readonly(path: &Path, readonly: bool) {
        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(readonly);
        std::fs::set_permissions(path, permissions).unwrap();
    }

    fn wait_until(reason: &str, mut condition: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !condition() {
            assert!(std::time::Instant::now() < deadline, "timed out: {reason}");
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// ReplaceFileW refuses a read-only destination, so flipping the read-only
    /// attribute injects and clears background write failure deterministically.
    /// Windows-only: elsewhere rename onto a read-only file succeeds.
    #[cfg(windows)]
    #[test]
    fn write_failure_observer_reports_each_transition_exactly_once() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.v1.json");
        let store = DocumentStore::default();
        store.read(&path, directory.path()).unwrap();

        let reports: Arc<Mutex<Vec<Option<String>>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = reports.clone();
        store.set_write_failure_observer(Box::new(move |failure| {
            sink.lock().unwrap().push(failure);
        }));

        // A healthy write reports nothing — no spurious recovery events.
        store.commit(&path, test_document()).unwrap();
        store.flush(Duration::from_secs(10)).unwrap();
        assert_eq!(reports.lock().unwrap().len(), 0);

        set_readonly(&path, true);
        store.commit(&path, test_document()).unwrap();
        wait_until("first write failure reaches the observer", || {
            reports.lock().unwrap().len() == 1
        });
        {
            let reports = reports.lock().unwrap();
            let failure = reports[0].as_deref().expect("failure carries a message");
            assert!(!failure.is_empty());
        }

        // The next commit surfaces the stale error to its caller, and the
        // writer's renewed attempts at the unchanged failure stay silent.
        let stale = store.commit(&path, test_document()).unwrap_err();
        assert!(stale.contains("上一次后台保存失败"), "{stale}");
        thread::sleep(Duration::from_millis(300));
        assert_eq!(reports.lock().unwrap().len(), 1);

        set_readonly(&path, false);
        store.commit(&path, test_document()).ok();
        wait_until("recovery reaches the observer", || {
            reports.lock().unwrap().len() == 2
        });
        assert_eq!(reports.lock().unwrap()[1], None);
        store.flush(Duration::from_secs(10)).unwrap();
        assert_eq!(reports.lock().unwrap().len(), 2);
    }
}
