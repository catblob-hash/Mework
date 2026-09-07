//! On-disk artifacts for workflow runs.
//!
//! Each run is stored in `<app_data_dir>/workflows/<conversationId>/<runId>/`:
//!
//! - `script.js`: the approved script body. Resume verifies its SHA-256 against the submitted body.
//! - `journal.jsonl`: append-only `started`, `result`, and diagnostic-only `settled` records.
//! - `manifest.json`: run summary. A `running` status after restart identifies an interrupted run.
//! - `steps/<index>.json`: complete records for individual steps.
//!
//! # Why artifacts stay on disk
//!
//! Run artifacts can exhaust the 16 MiB document limit and do not need document synchronization or
//! versioning. Timeline records retain only compact fingerprints and summaries.
//!
//! # Why journal failures are non-fatal
//!
//! The journal enables recovery, not execution. Failed writes make a run non-resumable but must not
//! fail working steps. Writes only warn; malformed read records are skipped so a partial JSON line
//! cannot invalidate earlier results.
//!
//! # Unconditional journaling
//!
//! Every run writes a journal. Recovery is a property of runs, not an optional setting.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

const RUNS_DIRECTORY: &str = "workflows";
const JOURNAL_FILE: &str = "journal.jsonl";
const SOURCE_FILE: &str = "script.js";
const MANIFEST_FILE: &str = "manifest.json";
const STEPS_DIRECTORY: &str = "steps";
/// Driver liveness lock file. See [`acquire_driver_lock`].
const DRIVER_LOCK_FILE: &str = "driver.lock";

/// Maximum byte length of one journal record.
///
/// Results may be large, but a corrupt journal can turn the entire file into one line. Bounded
/// reads prevent unbounded allocation; oversized records are treated as malformed.
const MAX_JOURNAL_LINE_BYTES: usize = 4 * 1024 * 1024;

/// Validates characters allowed in run-directory names.
///
/// `conversation_id` and `run_id` become path components, so they must be opaque flat identifiers,
/// not merely identifier-like values. Reject path traversal, separators, and platform-reserved forms.
fn validate_path_component(kind: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{kind}不能为空"));
    }
    if value.len() > 128 {
        return Err(format!("{kind}超过 128 字节"));
    }
    if value == "." || value == ".." {
        return Err(format!("{kind}不能是 . 或 .."));
    }
    if value.ends_with('.') || value.ends_with(' ') {
        return Err(format!("{kind}不能以点或空格结尾"));
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!("{kind}只能包含 ASCII 字母、数字、_ 和 -"));
    }
    Ok(())
}

/// One journal record. Only these three variants are accepted.
///
/// `deny_unknown_fields` makes older versions skip newer records instead of treating a truncated
/// record as a valid cache hit. Unknown variants are likewise skipped; only diagnostics degrade.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum JournalLine {
    /// A key's step was dispatched.
    #[serde(rename_all = "camelCase")]
    Started { key: String, agent_id: String },
    /// A key's step produced a result.
    #[serde(rename_all = "camelCase")]
    Result {
        key: String,
        agent_id: String,
        result: Value,
    },
    /// A key's step settled without a value (failed, skipped, or interrupted).
    ///
    /// This diagnostic record never caches a result, so the step reruns on recovery. It
    /// distinguishes a real failed step from a host crash after `started`.
    #[serde(rename_all = "camelCase")]
    Settled {
        key: String,
        agent_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

/// An in-memory journal.
#[derive(Clone, Debug, Default)]
pub struct Journal {
    /// Reusable results. Only `result` records enter this map.
    results: HashMap<String, Value>,
    /// Number of `started` records for each key.
    started: HashMap<String, usize>,
    /// Keys with a no-value terminal record, indicating step failure rather than a host crash.
    settled: std::collections::HashSet<String>,
    /// Number of malformed records skipped while loading.
    skipped_lines: usize,
}

impl Journal {
    /// Returns whether this key has a reusable result.
    pub fn result(&self, key: &str) -> Option<&Value> {
        self.results.get(key)
    }

    /// Number of reusable results, used by recovery notifications.
    pub fn result_count(&self) -> usize {
        self.results.len()
    }

    #[cfg(test)]
    pub fn skipped_lines(&self) -> usize {
        self.skipped_lines
    }

    /// Returns keys that started but have neither a result nor a terminal record, with start counts.
    ///
    /// This is the only signal that distinguishes repeated host crashes from slow or failed steps.
    /// Output is sorted by descending count, then ascending key.
    pub fn respawn_diagnostics(&self) -> Vec<(String, usize)> {
        let mut rows = self
            .started
            .iter()
            .filter(|(key, _)| {
                !self.results.contains_key(*key) && !self.settled.contains(*key)
            })
            .map(|(key, count)| (key.clone(), *count))
            .collect::<Vec<_>>();
        rows.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        rows
    }
}

/// Handle for a run's on-disk storage.
///
/// Construction creates the directory, so a returned handle guarantees the directory and source
/// body exist.
#[derive(Debug)]
pub struct RunStore {
    directory: PathBuf,
    /// SHA-256 of the source body in lowercase hexadecimal, used to validate a resume approval.
    source_digest: String,
    /// Whether this `open` created the run because no source existed on disk.
    fresh: bool,
    /// A failed journal append makes the recovery promise unreliable without failing execution.
    journal_degraded: std::sync::atomic::AtomicBool,
}

impl RunStore {
    /// Opens or creates a run directory and fixes its source body.
    ///
    /// A resume never overwrites stored source: its digest must match the submitted source, or the
    /// approval no longer applies.
    pub fn open(
        app_data_path: &Path,
        conversation_id: &str,
        run_id: &str,
        source: &[u8],
    ) -> Result<Self, String> {
        validate_path_component("会话 id", conversation_id)?;
        validate_path_component("运行 id", run_id)?;
        let directory = app_data_path
            .join(RUNS_DIRECTORY)
            .join(conversation_id)
            .join(run_id);
        fs::create_dir_all(directory.join(STEPS_DIRECTORY))
            .map_err(|error| format!("无法创建工作流运行目录：{error}"))?;
        restrict_directory(&directory);
        let source_digest = hex_digest(source);
        let source_path = directory.join(SOURCE_FILE);
        let mut fresh = false;
        match fs::read(&source_path) {
            Ok(existing) => {
                let existing_digest = hex_digest(&existing);
                if existing_digest != source_digest {
                    return Err("脚本内容在批准后发生变化；请重新发起这次工作流".into());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                write_private(&source_path, source)
                    .map_err(|error| format!("无法写入工作流正文：{error}"))?;
                fresh = true;
            }
            Err(error) => return Err(format!("无法读取工作流正文：{error}")),
        }
        Ok(Self {
            directory,
            source_digest,
            fresh,
            journal_degraded: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Whether this `open` created a new run.
    ///
    /// Recovery must reject a fresh directory rather than silently performing a full rerun.
    pub fn is_fresh(&self) -> bool {
        self.fresh
    }

    /// Discards a directory this `open` just created; does nothing for an existing run.
    ///
    /// Recovery must remove a fresh directory so a later retry still detects the missing run.
    pub fn discard_if_fresh(self) {
        if !self.fresh {
            return;
        }
        if let Err(error) = fs::remove_dir_all(&self.directory) {
            eprintln!("无法清理失败恢复留下的空运行目录：{error}");
        }
    }

    pub fn source_digest(&self) -> &str {
        &self.source_digest
    }

    #[cfg(test)]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Loads the journal, returning an empty journal when it does not exist.
    pub fn load_journal(&self) -> Journal {
        load_journal(&self.directory.join(JOURNAL_FILE))
    }

    /// Appends one record. Failures only mark recovery as degraded and do not fail execution.
    pub fn append(&self, line: &JournalLine) {
        let path = self.directory.join(JOURNAL_FILE);
        if let Err(error) = append_line(&path, line) {
            self.journal_degraded
                .store(true, std::sync::atomic::Ordering::Relaxed);
            eprintln!("工作流日志追加失败（本次运行将不可恢复）：{error}");
        }
    }

    /// Whether a journal append has failed during this run.
    pub fn journal_degraded(&self) -> bool {
        self.journal_degraded
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Writes a complete step record. The result signals whether it reached disk so callers retain
    /// inline timeline content when externalization fails.
    pub fn write_step(&self, index: usize, record: &Value) -> bool {
        let path = self
            .directory
            .join(STEPS_DIRECTORY)
            .join(format!("{index}.json"));
        let Ok(body) = serde_json::to_vec(record) else {
            eprintln!("工作流步骤记录无法序列化：步骤 {index}");
            return false;
        };
        if let Err(error) = write_private(&path, &body) {
            eprintln!("工作流步骤记录写入失败：{error}");
            return false;
        }
        true
    }

    /// Whether a step record exists on disk. Replay must confirm an earlier record survives before
    /// externalizing timeline content.
    pub fn step_exists(&self, index: usize) -> bool {
        self.directory
            .join(STEPS_DIRECTORY)
            .join(format!("{index}.json"))
            .is_file()
    }

    /// Writes the run manifest. Failures are non-fatal.
    pub fn write_manifest(&self, manifest: &Value) {
        let Ok(body) = serde_json::to_vec(manifest) else {
            eprintln!("工作流运行摘要无法序列化");
            return;
        };
        if let Err(error) = write_private(&self.directory.join(MANIFEST_FILE), &body) {
            eprintln!("工作流运行摘要写入失败：{error}");
        }
    }
}

/// Driver liveness lock for a run: holding it means this process drives that run.
///
/// On Windows, `share_mode(0)` opens `driver.lock` exclusively. A live handle prevents another
/// process from opening it; the OS releases it after a crash. Recovery must acquire this lock before
/// claiming a `status:"running"` run, otherwise a live second instance could be misclassified.
///
/// Other platforms use a normal open because no equivalent zero-dependency crash-safe primitive is
/// available; the application is currently released only for Windows.
#[derive(Debug)]
pub struct RunDriverLock {
    _file: File,
}

/// Attempts to acquire a run's driver lock. `Err` means another live process holds it.
pub fn acquire_driver_lock(
    app_data_path: &Path,
    conversation_id: &str,
    run_id: &str,
) -> Result<RunDriverLock, String> {
    validate_path_component("会话 id", conversation_id)?;
    validate_path_component("运行 id", run_id)?;
    let path = app_data_path
        .join(RUNS_DIRECTORY)
        .join(conversation_id)
        .join(run_id)
        .join(DRIVER_LOCK_FILE);
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }
    match options.open(&path) {
        Ok(file) => Ok(RunDriverLock { _file: file }),
        Err(error) => Err(format!(
            "运行 {run_id} 的驱动器锁不可用（另一个实例可能正在驱动它）：{error}"
        )),
    }
}

/// Computes a lowercase hexadecimal SHA-256 digest. Source-approval validation and timeline
/// externalization use the same implementation.
pub(crate) fn hex_digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(unix)]
fn restrict_directory(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(error) = fs::set_permissions(path, fs::Permissions::from_mode(0o700)) {
        eprintln!("无法收紧工作流运行目录权限：{error}");
    }
}

#[cfg(not(unix))]
fn restrict_directory(_path: &Path) {
    // Windows ACL inheritance provides the per-user boundary for Mework application-data directories.
}

/// Overwrites a file readable only by the current user.
fn write_private(path: &Path, body: &[u8]) -> std::io::Result<()> {
    write_private_with_sync(path, body, File::sync_all)
}

fn write_private_with_sync(
    path: &Path,
    body: &[u8],
    sync: impl FnOnce(&File) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(body)?;
    file.flush()?;
    sync(&file)
}

fn append_line(path: &Path, line: &JournalLine) -> std::io::Result<()> {
    append_line_with_sync(path, line, File::sync_all)
}

fn append_line_with_sync(
    path: &Path,
    line: &JournalLine,
    sync: impl FnOnce(&File) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let mut body = serde_json::to_vec(line)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    body.push(b'\n');
    let mut options = OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // Write the entire record, including its newline, atomically. Separate writes could interleave
    // concurrent appends into a partial record indistinguishable from crash truncation.
    let mut file = options.open(path)?;
    file.write_all(&body)?;
    file.flush()?;
    sync(&file)
}

/// Loads a journal from a path, skipping malformed records and returning an empty journal on read failure.
fn load_journal(path: &Path) -> Journal {
    let mut journal = Journal::default();
    let Ok(file) = File::open(path) else {
        return journal;
    };
    let mut reader = BufReader::new(file);
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        // Read bytes rather than using `lines()`: non-UTF-8 corruption would otherwise stop the
        // entire iterator and invalidate every following record.
        let read = match read_bounded_line(&mut reader, &mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) => {
                eprintln!("工作流日志读取中断，其余行按未记录处理：{error}");
                break;
            }
        };
        if read > MAX_JOURNAL_LINE_BYTES {
            journal.skipped_lines += 1;
            continue;
        }
        let trimmed = trim_line(&buffer);
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_slice::<JournalLine>(trimmed) {
            Ok(JournalLine::Started { key, .. }) => {
                *journal.started.entry(key).or_insert(0) += 1;
            }
            Ok(JournalLine::Result { key, result, .. }) => {
                journal.results.insert(key, result);
            }
            Ok(JournalLine::Settled { key, .. }) => {
                journal.settled.insert(key);
            }
            Err(_) => journal.skipped_lines += 1,
        }
    }
    journal
}

/// Reads one line without accepting an unbounded allocation.
///
/// Returns the raw byte count, which may exceed the buffer length for discarded oversized lines;
/// zero indicates end of file.
fn read_bounded_line(
    reader: &mut BufReader<File>,
    buffer: &mut Vec<u8>,
) -> std::io::Result<usize> {
    let mut total = 0usize;
    loop {
        let available = match reader.fill_buf() {
            Ok(available) => available,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if available.is_empty() {
            return Ok(total);
        }
        let (chunk, done) = match available.iter().position(|byte| *byte == b'\n') {
            Some(position) => (&available[..=position], true),
            None => (available, false),
        };
        let consumed = chunk.len();
        total += consumed;
        if total <= MAX_JOURNAL_LINE_BYTES {
            buffer.extend_from_slice(chunk);
        } else {
            // Consume through the line end without accumulating an oversized corrupted record.
            buffer.clear();
        }
        reader.consume(consumed);
        if done {
            return Ok(total);
        }
    }
}

fn trim_line(buffer: &[u8]) -> &[u8] {
    let mut end = buffer.len();
    while end > 0 && (buffer[end - 1] == b'\n' || buffer[end - 1] == b'\r') {
        end -= 1;
    }
    &buffer[..end]
}

/// Reads the script body fixed on disk for script-free recovery.
///
/// `Ok(None)` means the run or script is missing. Callers must report this explicit failure rather
/// than silently performing a new execution.
pub fn read_run_script(
    app_data_path: &Path,
    conversation_id: &str,
    run_id: &str,
) -> Result<Option<Vec<u8>>, String> {
    validate_path_component("会话 id", conversation_id)?;
    validate_path_component("运行 id", run_id)?;
    let path = app_data_path
        .join(RUNS_DIRECTORY)
        .join(conversation_id)
        .join(run_id)
        .join(SOURCE_FILE);
    match fs::read(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("无法读取运行 {run_id} 保存的脚本：{error}")),
        Ok(bytes) => Ok(Some(bytes)),
    }
}

/// Reads a complete step record on demand from `steps/<index>.json`.
///
/// Timeline contexts retain only a fingerprint and summary; the renderer obtains the full record
/// through IPC when its drawer opens. A missing file is a valid dangling record and returns `None`.
pub fn read_step_record(
    app_data_path: &Path,
    conversation_id: &str,
    run_id: &str,
    step_index: u32,
) -> Result<Option<Value>, String> {
    validate_path_component("会话 id", conversation_id)?;
    validate_path_component("运行 id", run_id)?;
    let path = app_data_path
        .join(RUNS_DIRECTORY)
        .join(conversation_id)
        .join(run_id)
        .join(STEPS_DIRECTORY)
        .join(format!("{step_index}.json"));
    let mut checked = app_data_path.join(RUNS_DIRECTORY);
    for component in [None, Some(conversation_id), Some(run_id), Some(STEPS_DIRECTORY)] {
        if let Some(component) = component { checked = checked.join(component); }
        if fs::symlink_metadata(&checked).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err("工作流历史路径不能是符号链接".into());
        }
    }
    if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink() || metadata.len() > 16 * 1024 * 1024) {
        return Err("工作流步骤记录不是可读取的常规文件".into());
    }
    match fs::read(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("无法读取工作流步骤记录：{error}")),
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
            .map(Some)
            .map_err(|error| format!("工作流步骤记录已损坏：{error}")),
    }
}

/// Run records are conversation child data and share the conversation lifecycle.
///
/// Deletes only the complete `workflows/<conversationId>` directory. Never remove individual files:
/// a running workflow's source body is written once and must remain available throughout the run.
/// Returns whether an existing directory was removed.
pub fn remove_conversation_runs(
    app_data_path: &Path,
    conversation_id: &str,
) -> Result<bool, String> {
    let directory = app_data_path.join(RUNS_DIRECTORY).join(conversation_id);
    match fs::symlink_metadata(&directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("无法检查工作流运行目录：{error}")),
        Ok(metadata) if !metadata.is_dir() => {
            Err("工作流运行路径不是目录，拒绝删除".into())
        }
        Ok(_) => fs::remove_dir_all(&directory)
            .map(|()| true)
            .map_err(|error| format!("无法删除工作流运行目录：{error}")),
    }
}

/// Duplicates one conversation's run directories under another conversation.
///
/// A forked conversation inherits copies of the parent's timeline cards, and a workflow card reads
/// its step bodies from `workflows/<conversationId>/<runId>/steps/`. Without this copy the child's
/// cards would open onto nothing, because run artifacts are addressed by the conversation that owns
/// them, not by the card. A source with no runs is a no-op, and nothing is ever removed: the source
/// conversation keeps driving its own runs from the same files.
pub fn copy_conversation_runs(
    app_data_path: &Path,
    source_conversation_id: &str,
    target_conversation_id: &str,
) -> Result<(), String> {
    validate_path_component("会话 id", source_conversation_id)?;
    validate_path_component("会话 id", target_conversation_id)?;
    let source = app_data_path
        .join(RUNS_DIRECTORY)
        .join(source_conversation_id);
    if !source.is_dir() {
        return Ok(());
    }
    let target = app_data_path
        .join(RUNS_DIRECTORY)
        .join(target_conversation_id);
    copy_directory(&source, &target)
}

/// Recursively copies run content, creating the destination as it goes.
///
/// Only regular files and directories are copied. A symbolic link is not run content, and following
/// one would write outside the destination tree; the driver lock is process liveness rather than
/// content, and on Windows it may be held exclusively by a live driver, which would fail the whole
/// copy for a file the copy does not want.
fn copy_directory(source: &Path, target: &Path) -> Result<(), String> {
    fs::create_dir_all(target).map_err(|error| format!("无法创建工作流运行目录：{error}"))?;
    restrict_directory(target);
    let entries =
        fs::read_dir(source).map_err(|error| format!("无法读取工作流运行目录：{error}"))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("无法读取工作流运行目录：{error}"))?;
        let name = entry.file_name();
        if name == DRIVER_LOCK_FILE {
            continue;
        }
        let from = entry.path();
        let to = target.join(&name);
        let metadata = fs::symlink_metadata(&from)
            .map_err(|error| format!("无法检查工作流运行文件：{error}"))?;
        if metadata.is_dir() {
            copy_directory(&from, &to)?;
        } else if metadata.is_file() {
            fs::copy(&from, &to)
                .map(|_| ())
                .map_err(|error| format!("无法复制工作流运行文件：{error}"))?;
        }
    }
    Ok(())
}

/// Finishes a save transaction by deleting runs for conversations removed by that transaction.
///
/// Failures are logged and deferred to [`reap_conversation_orphans`] so a save does not fail solely
/// because cleanup failed. Returns the number of directories actually removed.
pub fn remove_removed_conversation_runs(
    app_data_path: &Path,
    previous: &crate::model::AppDocument,
    next: &crate::model::AppDocument,
) -> usize {
    let live = document_conversation_ids(next);
    let mut removed = 0usize;
    for workspace in &previous.workspaces {
        for conversation in &workspace.conversations {
            if live.contains(conversation.id.as_str()) {
                continue;
            }
            match remove_conversation_runs(app_data_path, &conversation.id) {
                Ok(true) => removed += 1,
                Ok(false) => {}
                Err(error) => eprintln!(
                    "会话已删除，但清理其工作流运行目录失败（{}）：{error}",
                    conversation.id
                ),
            }
        }
    }
    removed
}

/// Startup fallback that removes orphaned `workflows/` directories for non-live conversations.
///
/// The normal save path removes them synchronously. This only handles crash leftovers and never
/// considers directory age: an old run remains valid while its conversation is live.
pub fn reap_conversation_orphans(
    app_data_path: &Path,
    document: &crate::model::AppDocument,
) -> usize {
    let live = document_conversation_ids(document);
    let root = app_data_path.join(RUNS_DIRECTORY);
    let Ok(entries) = fs::read_dir(&root) else {
        return 0;
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if live.contains(name) {
            continue;
        }
        match fs::remove_dir_all(&path) {
            Ok(()) => removed += 1,
            Err(error) => eprintln!("无法清理孤儿工作流运行目录：{error}"),
        }
    }
    removed
}

fn document_conversation_ids(
    document: &crate::model::AppDocument,
) -> std::collections::HashSet<&str> {
    document
        .workspaces
        .iter()
        .flat_map(|workspace| workspace.conversations.iter())
        .map(|conversation| conversation.id.as_str())
        .collect()
}

/// An interrupted run claimed during startup recovery. Its driver disappeared with the prior
/// process, while its script, journal, and step records remain available for `resume_run_id`.
#[derive(Clone, Debug, PartialEq)]
pub struct InterruptedRun {
    pub conversation_id: String,
    pub run_id: String,
    pub script_name: String,
    /// Number of reusable step results in the journal.
    pub reusable_steps: usize,
    /// Whether this scan first classified the run as interrupted. `false` is a redelivery after a
    /// crash before notification delivery.
    pub freshly_interrupted: bool,
}

/// Manifest keys for startup recovery. The manifest is internal JSON read and written only here,
/// so these centralized string literals define its complete schema.
const MANIFEST_STATUS_KEY: &str = "status";
const MANIFEST_INTERRUPTED_BY_KEY: &str = "interruptedBy";
const MANIFEST_NOTICE_DELIVERED_KEY: &str = "crashNoticeDelivered";
const MANIFEST_STATUS_RUNNING: &str = "running";
const MANIFEST_STATUS_INTERRUPTED: &str = "interrupted";
const MANIFEST_INTERRUPTED_BY_RESTART: &str = "app_restart";

/// Claims workflow runs left unfinished when the prior process died.
///
/// A `running` manifest is interrupted because the driver writes it on startup and a terminal
/// status only on completion. Claiming writes `interrupted` and an undelivered notice marker;
/// delivery is acknowledged by [`mark_crash_notice_delivered`].
///
/// Scan only live conversations. Orphan cleanup belongs to [`reap_conversation_orphans`]; missing
/// or corrupt manifests cannot prove a run started and are skipped.
pub fn sweep_interrupted_runs(
    app_data_path: &Path,
    document: &crate::model::AppDocument,
) -> Vec<InterruptedRun> {
    let live = document_conversation_ids(document);
    let root = app_data_path.join(RUNS_DIRECTORY);
    let Ok(conversations) = fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut claimed = Vec::new();
    for conversation in conversations.flatten() {
        let conversation_path = conversation.path();
        if !conversation_path.is_dir() {
            continue;
        }
        let Some(conversation_id) = conversation.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !live.contains(conversation_id.as_str()) {
            continue;
        }
        let Ok(runs) = fs::read_dir(&conversation_path) else {
            continue;
        };
        for run in runs.flatten() {
            let run_path = run.path();
            if !run_path.is_dir() {
                continue;
            }
            let Some(run_id) = run.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let manifest_path = run_path.join(MANIFEST_FILE);
            let Ok(bytes) = fs::read(&manifest_path) else {
                continue;
            };
            let Ok(mut manifest) = serde_json::from_slice::<Value>(&bytes) else {
                continue;
            };
            let status = manifest
                .get(MANIFEST_STATUS_KEY)
                .and_then(Value::as_str)
                .unwrap_or_default();
            let undelivered_redo = status == MANIFEST_STATUS_INTERRUPTED
                && manifest
                    .get(MANIFEST_INTERRUPTED_BY_KEY)
                    .and_then(Value::as_str)
                    == Some(MANIFEST_INTERRUPTED_BY_RESTART)
                && manifest
                    .get(MANIFEST_NOTICE_DELIVERED_KEY)
                    .and_then(Value::as_bool)
                    == Some(false);
            let freshly_interrupted = status == MANIFEST_STATUS_RUNNING;
            if !freshly_interrupted && !undelivered_redo {
                continue;
            }
            // A driver lock held by another live instance means this is not a crash remnant.
            // Probe without retaining: startup scanning is single-threaded and a successful probe
            // establishes that no live driver exists.
            if acquire_driver_lock(app_data_path, &conversation_id, &run_id).is_err() {
                continue;
            }
            if freshly_interrupted {
                manifest[MANIFEST_STATUS_KEY] = Value::String(MANIFEST_STATUS_INTERRUPTED.into());
                manifest[MANIFEST_INTERRUPTED_BY_KEY] =
                    Value::String(MANIFEST_INTERRUPTED_BY_RESTART.into());
                manifest[MANIFEST_NOTICE_DELIVERED_KEY] = Value::Bool(false);
                manifest["interruptedAt"] =
                    Value::String(chrono::Utc::now().to_rfc3339());
                let Ok(body) = serde_json::to_vec(&manifest) else {
                    continue;
                };
                if let Err(error) = write_private(&manifest_path, &body) {
                    // Do not synthesize a notification unless the claim persists. A later startup
                    // can retry a failed write, whereas dropping the claim loses recovery.
                    eprintln!("无法认领中断的工作流运行 {run_id}：{error}");
                    continue;
                }
            }
            let script_name = manifest
                .get("scriptName")
                .and_then(Value::as_str)
                .unwrap_or("workflow")
                .to_owned();
            let reusable_steps = load_journal(&run_path.join(JOURNAL_FILE)).result_count();
            claimed.push(InterruptedRun {
                conversation_id: conversation_id.clone(),
                run_id,
                script_name,
                reusable_steps,
                freshly_interrupted,
            });
        }
    }
    claimed
}

/// Acknowledges delivery of an interruption notification. A failed acknowledgement may duplicate a
/// notification after restart, which is preferable to losing it.
pub fn mark_crash_notice_delivered(app_data_path: &Path, conversation_id: &str, run_id: &str) {
    if validate_path_component("会话 id", conversation_id).is_err()
        || validate_path_component("运行 id", run_id).is_err()
    {
        return;
    }
    let manifest_path = app_data_path
        .join(RUNS_DIRECTORY)
        .join(conversation_id)
        .join(run_id)
        .join(MANIFEST_FILE);
    let Ok(bytes) = fs::read(&manifest_path) else {
        return;
    };
    let Ok(mut manifest) = serde_json::from_slice::<Value>(&bytes) else {
        return;
    };
    manifest[MANIFEST_NOTICE_DELIVERED_KEY] = Value::Bool(true);
    let Ok(body) = serde_json::to_vec(&manifest) else {
        return;
    };
    if let Err(error) = write_private(&manifest_path, &body) {
        eprintln!("无法销账工作流中断通知（{run_id}）：{error}");
    }
}

/// Assembles manifest step entries in stable index order.
///
/// Steps complete in pipeline order, which differs from dispatch order. A `BTreeMap` preserves the
/// required dispatch ordering.
pub fn manifest_steps(entries: impl IntoIterator<Item = (usize, Value)>) -> Value {
    let ordered = entries.into_iter().collect::<BTreeMap<_, _>>();
    Value::Array(ordered.into_values().collect())
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use serde_json::json;

    use super::*;

    fn store(directory: &Path) -> RunStore {
        RunStore::open(directory, "conv1", "run1", b"{\"name\":\"p\"}").unwrap()
    }

    #[test]
    fn private_write_requires_sync_after_complete_write() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("private");
        let called = std::cell::Cell::new(false);
        let result = write_private_with_sync(&path, b"complete body", |_| {
            assert_eq!(fs::read(&path).unwrap(), b"complete body");
            called.set(true);
            Err(std::io::Error::other("injected sync failure"))
        });
        assert!(called.get(), "write must reach the persistence barrier");
        assert_eq!(result.unwrap_err().to_string(), "injected sync failure");
        write_private_with_sync(&path, b"replacement", |file| {
            assert_eq!(fs::read(&path).unwrap(), b"replacement");
            file.sync_all()
        }).unwrap();
    }

    #[test]
    fn journal_append_requires_sync_after_complete_write() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("journal");
        let line = JournalLine::Started { key: "k".into(), agent_id: "a".into() };
        let mut expected = serde_json::to_vec(&line).unwrap();
        expected.push(b'\n');
        let called = std::cell::Cell::new(false);
        let result = append_line_with_sync(&path, &line, |_| {
            assert_eq!(fs::read(&path).unwrap(), expected);
            called.set(true);
            Err(std::io::Error::other("injected append sync failure"))
        });
        assert!(called.get(), "append must reach the persistence barrier");
        assert_eq!(result.unwrap_err().to_string(), "injected append sync failure");
        append_line_with_sync(&path, &line, |file| {
            assert_eq!(fs::read(&path).unwrap(), expected.repeat(2));
            file.sync_all()
        }).unwrap();
    }

    #[test]
    fn a_run_directory_refuses_a_path_traversing_identifier() {
        let directory = tempfile::tempdir().unwrap();
        for bad in ["..", ".", "a/b", "a\\b", "c:", "a.", "a ", ""] {
            let error = RunStore::open(directory.path(), bad, "run1", b"{}").unwrap_err();
            assert!(!error.is_empty(), "{bad} must be refused");
        }
        // Validate run IDs as strictly as conversation IDs because recovery receives a model-supplied
        // run ID.
        assert!(RunStore::open(directory.path(), "conv1", "../escape", b"{}").is_err());
        // Validate before creating directories. Do not assert that `workflows/..` is absent: lexical
        // normalization resolves that path to `tempdir`, making the assertion vacuous.
        assert!(!directory.path().join(RUNS_DIRECTORY).exists());
        assert!(!directory.path().join("escape").exists());
    }

    #[test]
    fn a_journal_round_trips_results_and_counts_starts_without_them() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(directory.path());
        store.append(&JournalLine::Started {
            key: "mw1:a".into(),
            agent_id: "ws1".into(),
        });
        store.append(&JournalLine::Result {
            key: "mw1:a".into(),
            agent_id: "ws1".into(),
            result: json!({"ok": true}),
        });
        for _ in 0..3 {
            store.append(&JournalLine::Started {
                key: "mw1:b".into(),
                agent_id: "ws2".into(),
            });
        }

        let journal = store.load_journal();
        assert_eq!(journal.result("mw1:a"), Some(&json!({"ok": true})));
        assert_eq!(journal.result("mw1:b"), None);
        assert_eq!(journal.respawn_diagnostics(), vec![("mw1:b".to_owned(), 3)]);
        assert_eq!(journal.skipped_lines(), 0);
    }

    /// A settled record distinguishes a failed step from a host crash and never creates a cache hit.
    #[test]
    fn a_settled_step_is_not_misdiagnosed_as_a_host_crash_and_never_caches() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(directory.path());
        store.append(&JournalLine::Started {
            key: "mw1:failed".into(),
            agent_id: "ws1".into(),
        });
        store.append(&JournalLine::Settled {
            key: "mw1:failed".into(),
            agent_id: "ws1".into(),
            error: Some("步骤以 failed 结束".into()),
        });
        store.append(&JournalLine::Started {
            key: "mw1:crashed".into(),
            agent_id: "ws2".into(),
        });

        let journal = store.load_journal();
        assert_eq!(journal.result("mw1:failed"), None);
        assert_eq!(
            journal.respawn_diagnostics(),
            vec![("mw1:crashed".to_owned(), 1)]
        );
        assert_eq!(journal.skipped_lines(), 0);
    }

    /// A failed append latches the degraded flag. A directory at `journal.jsonl` reliably exercises
    /// the append failure path without depending on disk exhaustion or antivirus locking.
    #[test]
    fn a_failed_append_latches_the_degraded_flag() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(directory.path());
        assert!(!store.journal_degraded());
        fs::create_dir(store.directory().join(JOURNAL_FILE)).unwrap();
        store.append(&JournalLine::Started {
            key: "mw1:a".into(),
            agent_id: "ws1".into(),
        });
        assert!(store.journal_degraded());
    }

    #[test]
    fn a_truncated_line_is_skipped_rather_than_failing_the_whole_journal() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(directory.path());
        store.append(&JournalLine::Result {
            key: "mw1:a".into(),
            agent_id: "ws1".into(),
            result: json!(1),
        });
        {
            let mut file = OpenOptions::new()
                .append(true)
                .open(store.directory().join(JOURNAL_FILE))
                .unwrap();
            // A crash-truncated JSON record, non-UTF-8 bytes, and a record with an unknown field.
            file.write_all(b"{\"type\":\"result\",\"key\":\"mw1:b\",\"ag\n")
                .unwrap();
            file.write_all(&[0xff, 0xfe, b'\n']).unwrap();
            file.write_all(
                b"{\"type\":\"started\",\"key\":\"mw1:c\",\"agentId\":\"ws3\",\"extra\":1}\n",
            )
            .unwrap();
        }
        store.append(&JournalLine::Result {
            key: "mw1:d".into(),
            agent_id: "ws4".into(),
            result: json!(2),
        });

        let journal = store.load_journal();
        assert_eq!(journal.result("mw1:a"), Some(&json!(1)));
        assert_eq!(journal.result("mw1:d"), Some(&json!(2)));
        assert_eq!(journal.skipped_lines(), 3);
        assert!(journal.respawn_diagnostics().is_empty());
    }

    #[test]
    fn an_oversized_line_is_dropped_without_reading_it_into_memory() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(directory.path());
        {
            let mut file = OpenOptions::new()
                .append(true)
                .create(true)
                .open(store.directory().join(JOURNAL_FILE))
                .unwrap();
            file.write_all(b"{\"type\":\"started\",\"key\":\"mw1:pad").unwrap();
            let chunk = vec![b'x'; 1024 * 1024];
            for _ in 0..5 {
                file.write_all(&chunk).unwrap();
            }
            file.write_all(b"\"}\n").unwrap();
        }
        store.append(&JournalLine::Result {
            key: "mw1:after".into(),
            agent_id: "ws1".into(),
            result: json!("kept"),
        });

        let journal = store.load_journal();
        assert_eq!(journal.skipped_lines(), 1);
        assert_eq!(journal.result("mw1:after"), Some(&json!("kept")));
    }

    #[test]
    fn a_missing_journal_reads_as_empty_rather_than_erroring() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(directory.path());
        let journal = store.load_journal();
        assert!(journal.result("mw1:a").is_none());
        assert_eq!(journal.skipped_lines(), 0);
        assert!(journal.respawn_diagnostics().is_empty());
    }

    #[test]
    fn reopening_with_an_edited_body_refuses_to_resume_under_the_old_approval() {
        let directory = tempfile::tempdir().unwrap();
        let first = RunStore::open(directory.path(), "conv1", "run1", b"{\"a\":1}").unwrap();
        let again = RunStore::open(directory.path(), "conv1", "run1", b"{\"a\":1}").unwrap();
        assert_eq!(first.source_digest(), again.source_digest());

        let error = RunStore::open(directory.path(), "conv1", "run1", b"{\"a\":2}").unwrap_err();
        assert!(error.contains("脚本内容在批准后发生变化"), "{error}");
    }

    /// Run records follow conversation deletion and cannot remove another conversation's records.
    #[test]
    fn a_conversation_deletion_removes_its_runs_and_only_its_runs() {
        let directory = tempfile::tempdir().unwrap();
        let kept = RunStore::open(directory.path(), "convkeep", "run1", b"{}").unwrap();
        let removed_store = RunStore::open(directory.path(), "convgone", "run1", b"{}").unwrap();

        let mut previous = crate::catalog::default_document(directory.path());
        previous.workspaces[0].conversations[0].id = "convkeep".into();
        let mut gone = previous.workspaces[0].conversations[0].clone();
        gone.id = "convgone".into();
        previous.workspaces[0].conversations.push(gone);
        let mut next = previous.clone();
        next.workspaces[0]
            .conversations
            .retain(|conversation| conversation.id != "convgone");

        let removed = remove_removed_conversation_runs(directory.path(), &previous, &next);
        assert_eq!(removed, 1);
        assert!(!removed_store.directory().exists());
        assert!(kept.directory().exists());
        assert!(kept.directory().join(SOURCE_FILE).exists());
        assert!(kept.directory().join(STEPS_DIRECTORY).is_dir());

        assert_eq!(
            remove_removed_conversation_runs(directory.path(), &previous, &next),
            0
        );
    }

    /// The orphan sweep uses only live-conversation membership, never directory age.
    #[test]
    fn the_orphan_sweep_removes_only_directories_without_a_live_conversation() {
        let directory = tempfile::tempdir().unwrap();
        let live = RunStore::open(directory.path(), "convlive", "run1", b"{}").unwrap();
        let dead = RunStore::open(directory.path(), "convdead", "run1", b"{}").unwrap();
        let mut document = crate::catalog::default_document(directory.path());
        document.workspaces[0].conversations[0].id = "convlive".into();

        let removed = reap_conversation_orphans(directory.path(), &document);
        assert_eq!(removed, 1);
        assert!(!dead.directory().exists());
        assert!(live.directory().exists());
        assert!(live.directory().join(SOURCE_FILE).exists());
    }

    #[test]
    fn manifest_steps_order_by_index_rather_than_completion() {
        let manifest = manifest_steps([(2, json!("c")), (0, json!("a")), (1, json!("b"))]);
        assert_eq!(manifest, json!(["a", "b", "c"]));
    }

    /// A startup scan claims a running manifest, redelivers an unacknowledged notice, and stops
    /// claiming it after acknowledgement. Terminal and orphaned runs remain untouched.
    #[test]
    fn the_interrupted_sweep_claims_redelivers_and_settles() {
        let directory = tempfile::tempdir().unwrap();
        let mut document = crate::catalog::default_document(directory.path());
        document.workspaces[0].conversations[0].id = "convlive".into();

        let crashed = RunStore::open(directory.path(), "convlive", "run1", b"{}").unwrap();
        crashed.write_manifest(&json!({
            "runId": "run1",
            "scriptName": "audit",
            "status": "running",
        }));
        crashed.append(&JournalLine::Result {
            key: "k1".into(),
            agent_id: "ws1".into(),
            result: json!(1),
        });
        crashed.append(&JournalLine::Result {
            key: "k2".into(),
            agent_id: "ws2".into(),
            result: json!(2),
        });
        let finished = RunStore::open(directory.path(), "convlive", "run2", b"{}").unwrap();
        finished.write_manifest(&json!({
            "runId": "run2",
            "scriptName": "done",
            "status": "completed",
        }));
        let dead = RunStore::open(directory.path(), "convdead", "run3", b"{}").unwrap();
        dead.write_manifest(&json!({
            "runId": "run3",
            "scriptName": "orphan",
            "status": "running",
        }));

        let claimed = sweep_interrupted_runs(directory.path(), &document);
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].conversation_id, "convlive");
        assert_eq!(claimed[0].run_id, "run1");
        assert_eq!(claimed[0].script_name, "audit");
        assert_eq!(claimed[0].reusable_steps, 2);
        assert!(claimed[0].freshly_interrupted);

        let redelivered = sweep_interrupted_runs(directory.path(), &document);
        assert_eq!(redelivered.len(), 1);
        assert_eq!(redelivered[0].run_id, "run1");
        assert!(!redelivered[0].freshly_interrupted);

        mark_crash_notice_delivered(directory.path(), "convlive", "run1");
        assert!(sweep_interrupted_runs(directory.path(), &document).is_empty());
    }

    /// A live driver lock prevents the sweep from claiming a running manifest. It may be claimed
    /// only after the lock releases. This test is Windows-only because `share_mode(0)` supplies the
    /// required zero-dependency liveness guarantee there.
    #[cfg(windows)]
    #[test]
    fn a_live_driver_lock_shields_a_running_manifest_from_the_sweep() {
        let directory = tempfile::tempdir().unwrap();
        let mut document = crate::catalog::default_document(directory.path());
        document.workspaces[0].conversations[0].id = "convlive".into();
        let store = RunStore::open(directory.path(), "convlive", "run1", b"{}").unwrap();
        store.write_manifest(&json!({
            "runId": "run1",
            "scriptName": "live",
            "status": "running",
        }));

        let lock = acquire_driver_lock(directory.path(), "convlive", "run1").unwrap();
        assert!(sweep_interrupted_runs(directory.path(), &document).is_empty());
        assert!(acquire_driver_lock(directory.path(), "convlive", "run1").is_err());

        drop(lock);
        let claimed = sweep_interrupted_runs(directory.path(), &document);
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].run_id, "run1");
    }

    /// Step records round-trip by index; missing records are valid `None` values.
    #[test]
    fn a_step_record_round_trips_and_a_missing_one_reads_as_none() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(directory.path());
        assert!(!store.step_exists(0));
        let record = json!({"task": "检查调度器", "status": "completed"});
        assert!(store.write_step(0, &record));
        assert!(store.step_exists(0));

        assert_eq!(
            read_step_record(directory.path(), "conv1", "run1", 0).unwrap(),
            Some(record)
        );
        assert_eq!(
            read_step_record(directory.path(), "conv1", "run1", 1).unwrap(),
            None
        );
        assert_eq!(
            read_step_record(directory.path(), "conv1", "runmissing", 0).unwrap(),
            None
        );
        assert!(read_step_record(directory.path(), "../escape", "run1", 0).is_err());
        assert!(read_step_record(directory.path(), "conv1", "..", 0).is_err());
    }

    /// A forked conversation must be able to open the step bodies its copied cards reference, so the
    /// whole run tree — nested directories included — arrives byte for byte, and the source keeps
    /// driving its own runs from files the copy never touched.
    #[test]
    fn copying_runs_duplicates_the_whole_tree_and_leaves_the_source_intact() {
        let directory = tempfile::tempdir().unwrap();
        let source = RunStore::open(directory.path(), "convsrc", "run1", b"{\"a\":1}").unwrap();
        assert!(source.write_step(0, &json!({"status": "completed"})));
        source.append(&JournalLine::Result {
            key: "k1".into(),
            agent_id: "ws1".into(),
            result: json!(1),
        });

        copy_conversation_runs(directory.path(), "convsrc", "convchild").unwrap();

        let source_run = directory.path().join(RUNS_DIRECTORY).join("convsrc").join("run1");
        let target_run = directory
            .path()
            .join(RUNS_DIRECTORY)
            .join("convchild")
            .join("run1");
        for relative in [
            PathBuf::from(SOURCE_FILE),
            PathBuf::from(JOURNAL_FILE),
            Path::new(STEPS_DIRECTORY).join("0.json"),
        ] {
            let copied = target_run.join(&relative);
            assert!(copied.is_file(), "{} 必须被复制", relative.display());
            assert_eq!(
                fs::read(&copied).unwrap(),
                fs::read(source_run.join(&relative)).unwrap(),
                "{} 的内容必须逐字节相同",
                relative.display()
            );
        }
        // The child reads its inherited step through the normal address, not a special case.
        assert_eq!(
            read_step_record(directory.path(), "convchild", "run1", 0).unwrap(),
            Some(json!({"status": "completed"}))
        );
        assert!(source_run.join(SOURCE_FILE).is_file());
        assert_eq!(source.load_journal().result_count(), 1);
    }

    /// Forking a conversation that never ran a workflow copies nothing rather than failing, and an
    /// identifier is validated before any path is touched.
    #[test]
    fn copying_runs_from_a_conversation_without_any_is_a_no_op() {
        let directory = tempfile::tempdir().unwrap();
        copy_conversation_runs(directory.path(), "convempty", "convchild").unwrap();
        assert!(!directory
            .path()
            .join(RUNS_DIRECTORY)
            .join("convchild")
            .exists());

        assert!(copy_conversation_runs(directory.path(), "../escape", "convchild").is_err());
        assert!(copy_conversation_runs(directory.path(), "convempty", "..").is_err());
    }
}
