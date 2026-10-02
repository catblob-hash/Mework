//! On-disk artifacts for workflow runs.
//!
//! Each run is stored in `<app_data_dir>/workflows/<conversationId>/<runId>/`:
//!
//! - `script.js`: the approved script body of the latest attempt. A scriptless resume checks its
//!   SHA-256 against the timeline; a resume with an edited, re-approved script replaces it.
//! - `args.json`: the `args` of the latest attempt, so a resume may omit them.
//! - `journal.jsonl`: append-only `started`, `result`, and diagnostic-only `settled` records.
//! - `manifest.json`: run summary. A `running` status after restart identifies an interrupted run.
//! - `steps/<index>.json`: complete records for individual steps; `steps/<index>.key` names the
//!   step that wrote each one.
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
/// The `args` the latest attempt ran with. Absent when that run was given none.
const ARGS_FILE: &str = "args.json";
/// Which `workflow` call supplied the current `script.js` and `args.json`, by provider call id.
/// See [`RunStore::record_provenance`].
const PROVENANCE_FILE: &str = "provenance.json";
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
pub(crate) fn validate_path_component(kind: &str, value: &str) -> Result<(), String> {
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
    /// Keys whose latest record is `started`: the attempt that dispatched them ended before they
    /// settled, so the script never received their outcome.
    unsettled: std::collections::HashSet<String>,
    /// Number of malformed records skipped while loading.
    skipped_lines: usize,
}

impl Journal {
    /// Returns whether this key has a reusable result.
    pub fn result(&self, key: &str) -> Option<&Value> {
        self.results.get(key)
    }

    /// Classifies a key for the cache chain.
    ///
    /// A result is reusable whenever it was written. Without one, a key whose latest record is
    /// `started` is unsettled and reruns alone; anything else is a miss that latches the chain.
    pub fn lookup(&self, key: &str) -> workflow_core::chain::JournalLookup {
        use workflow_core::chain::JournalLookup;
        if self.results.contains_key(key) {
            JournalLookup::Hit
        } else if self.unsettled.contains(key) {
            JournalLookup::Unsettled
        } else {
            JournalLookup::Miss
        }
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
            .filter(|(key, _)| !self.results.contains_key(*key) && !self.settled.contains(*key))
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

    /// Opens a run that already exists, for a resume.
    ///
    /// Unlike [`Self::open`], this neither creates anything nor compares a submitted script: a
    /// resume may carry an edited script that was approved again, and [`Self::replace_source`]
    /// adopts it once the caller holds the driver lock. `Ok(None)` means there is no run to resume
    /// — no directory, or no pinned script — and the caller must say so rather than start over.
    pub fn open_existing(
        app_data_path: &Path,
        conversation_id: &str,
        run_id: &str,
    ) -> Result<Option<Self>, String> {
        validate_path_component("会话 id", conversation_id)?;
        validate_path_component("运行 id", run_id)?;
        let directory = app_data_path
            .join(RUNS_DIRECTORY)
            .join(conversation_id)
            .join(run_id);
        let source = match fs::read(directory.join(SOURCE_FILE)) {
            Ok(source) => source,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("无法读取工作流正文：{error}")),
        };
        fs::create_dir_all(directory.join(STEPS_DIRECTORY))
            .map_err(|error| format!("无法创建工作流运行目录：{error}"))?;
        Ok(Some(Self {
            directory,
            source_digest: hex_digest(&source),
            fresh: false,
            journal_degraded: std::sync::atomic::AtomicBool::new(false),
        }))
    }

    /// Pins the script this attempt runs, replacing the one an earlier attempt ran.
    ///
    /// Only a resume calls this, after the user approved the submitted script and while the caller
    /// holds the driver lock, so no other driver can be reading or replacing it. The body is staged
    /// and renamed into place: a crash leaves one whole script or the other, and a scriptless resume
    /// then checks whichever it finds against the call [`Self::record_provenance`] names.
    pub fn replace_source(&mut self, source: &[u8]) -> Result<(), String> {
        let digest = hex_digest(source);
        if digest == self.source_digest {
            return Ok(());
        }
        write_private_atomic(&self.directory.join(SOURCE_FILE), source)
            .map_err(|error| format!("无法写入工作流正文：{error}"))?;
        self.source_digest = digest;
        Ok(())
    }

    /// Saves the `args` this attempt runs with, so a resume that omits them runs with the same
    /// values. The bytes are `serde_json::to_vec` of the value, as the resume check re-derives them
    /// from the call that supplied it.
    ///
    /// A failed write only warns: the run itself does not depend on it, and a later resume that
    /// finds a stale or missing file against that call refuses to guess.
    pub fn write_args(&self, args: &Value) {
        let Ok(body) = serde_json::to_vec(args) else {
            eprintln!("工作流 args 无法序列化");
            return;
        };
        if let Err(error) = write_private_atomic(&self.directory.join(ARGS_FILE), &body) {
            eprintln!("工作流 args 写入失败（恢复时须重新提供 args）：{error}");
        }
    }

    /// Records which approved `workflow` call supplied the files this attempt runs with: `script`
    /// when it pinned `script.js`, `args` when it saved `args.json`. A value left `None` keeps the
    /// call an earlier attempt recorded, which is what a resume that reused the file means.
    ///
    /// A resume checks each file against the input that call ran with, as the history recorded it
    /// once every hook had had its say. The disk cannot vouch for its own contents, the timeline
    /// is the user's to edit, and the call as it ran is the one record of the bytes the user was
    /// asked to approve that neither of them wrote. This file only says which call to read; the bytes come from
    /// the ledger. Called under the driver lock, after the files it describes are written. A
    /// failed write only warns: the run does not depend on it, and a resume that cannot tell
    /// which call supplied a file refuses to reuse it.
    pub fn record_provenance(&self, script: Option<&str>, args: Option<&str>) {
        let path = self.directory.join(PROVENANCE_FILE);
        let mut provenance = read_provenance(&path).unwrap_or_default();
        if let Some(call) = script {
            provenance.script = Some(call.to_owned());
        }
        if let Some(call) = args {
            provenance.args = Some(call.to_owned());
        }
        let body = serde_json::json!({
            "script": provenance.script,
            "args": provenance.args,
        });
        if let Err(error) = write_private_atomic(&path, body.to_string().as_bytes()) {
            eprintln!("工作流来源记录写入失败（恢复时须重新提供脚本与 args）：{error}");
        }
    }

    /// Whether this `open` created a new run.
    #[cfg(test)]
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

    /// Writes a complete step record, and beside it the cache key of the step that produced it.
    /// The result signals whether the record reached disk so callers retain inline timeline
    /// content when externalization fails.
    ///
    /// Records are addressed by index, and a later attempt that dispatches a different step at the
    /// same index overwrites the file. The key file lets a replay tell its own record from one a
    /// later attempt left there.
    pub fn write_step(&self, index: usize, cache_key: &str, record: &Value) -> bool {
        let steps = self.directory.join(STEPS_DIRECTORY);
        let Ok(body) = serde_json::to_vec(record) else {
            eprintln!("工作流步骤记录无法序列化：步骤 {index}");
            return false;
        };
        // Drop the previous owner's key before touching the record, and write this step's key only
        // once the record is whole. A crash in between leaves no key, which reads like a record
        // from before key files — right for the step that just wrote it, the one a resume replays.
        let key_path = steps.join(format!("{index}.key"));
        match fs::remove_file(&key_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                eprintln!("工作流步骤键无法清除：{error}");
                return false;
            }
        }
        if let Err(error) = write_private(&steps.join(format!("{index}.json")), &body) {
            eprintln!("工作流步骤记录写入失败：{error}");
            return false;
        }
        if let Err(error) = write_private(&key_path, cache_key.as_bytes()) {
            eprintln!("工作流步骤键写入失败：{error}");
            return false;
        }
        true
    }

    /// Whether the record on disk at `index` is the one the step with `cache_key` wrote. Replay
    /// must confirm its own record survives before externalizing timeline content.
    ///
    /// A record from before key files existed has none and is accepted as it always was.
    pub fn step_matches(&self, index: usize, cache_key: &str) -> bool {
        let steps = self.directory.join(STEPS_DIRECTORY);
        if !steps.join(format!("{index}.json")).is_file() {
            return false;
        }
        match fs::read(steps.join(format!("{index}.key"))) {
            Ok(stored) => stored == cache_key.as_bytes(),
            Err(error) => error.kind() == std::io::ErrorKind::NotFound,
        }
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

/// Whether a run id is already taken in this conversation.
///
/// The model names its own runs and the name becomes the run id, so a name that was used before
/// must not silently reopen — and must not be rejected either, because the caller resolves a free
/// one by suffixing. A directory with no source body still counts as taken: it is either a run
/// whose creation was interrupted or one whose id is about to be claimed, and reusing it would let
/// two runs share a journal.
pub fn run_id_taken(app_data_path: &Path, conversation_id: &str, run_id: &str) -> bool {
    if validate_path_component("会话 id", conversation_id).is_err()
        || validate_path_component("运行 id", run_id).is_err()
        || is_reserved_device_name(run_id)
    {
        // An id the store cannot address is not available to claim.
        return true;
    }
    app_data_path
        .join(RUNS_DIRECTORY)
        .join(conversation_id)
        .join(run_id)
        .exists()
}

/// Whether Windows refuses this name as a directory. `con` and `aux` are legal agent names, so a
/// model can pick one; treating them as taken resolves the run to a numbered variant instead of
/// leaving it with an unwritable directory and no recoverability.
///
/// Only reached for values `validate_path_component` already accepted, so the charset is ASCII
/// alphanumerics, `_` and `-` — no extension to strip, and no trailing dot or space to reject.
fn is_reserved_device_name(value: &str) -> bool {
    if matches!(value, "con" | "prn" | "aux" | "nul") {
        return true;
    }
    let Some(port) = value
        .strip_prefix("com")
        .or_else(|| value.strip_prefix("lpt"))
    else {
        return false;
    };
    port.len() == 1 && port.as_bytes()[0].is_ascii_digit()
}

/// Driver liveness lock for a run: holding it means this process drives that run.
///
/// An exclusive OS file lock on `driver.lock` (`flock` on Unix, `LockFileEx` on Windows). The OS
/// releases it when the process dies, so a crash never leaves a run looking driven. Recovery must
/// acquire this lock before claiming a `status:"running"` run, otherwise a live second instance
/// could be misclassified. The lock belongs to the open file, so a second acquisition fails inside
/// the same process too — a driver that outlived its task's forced settlement still holds it.
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
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| format!("运行 {run_id} 的驱动器锁无法打开：{error}"))?;
    match file.try_lock() {
        Ok(()) => Ok(RunDriverLock { _file: file }),
        Err(std::fs::TryLockError::WouldBlock) => Err(format!(
            "运行 {run_id} 仍由另一个驱动器持有（它可能还在运行），不能同时驱动"
        )),
        Err(std::fs::TryLockError::Error(error)) => Err(format!(
            "运行 {run_id} 的驱动器锁不可用：{error}"
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
pub(crate) fn write_private(path: &Path, body: &[u8]) -> std::io::Result<()> {
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

/// Replaces a private file through a staged copy and a rename, so a reader never sees a partial
/// body: after a crash the file holds either the old content or the new.
fn write_private_atomic(path: &Path, body: &[u8]) -> std::io::Result<()> {
    let mut staged = path.as_os_str().to_owned();
    staged.push(".staged");
    let staged = PathBuf::from(staged);
    write_private(&staged, body)?;
    fs::rename(&staged, path)
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
                *journal.started.entry(key.clone()).or_insert(0) += 1;
                journal.unsettled.insert(key);
            }
            Ok(JournalLine::Result { key, result, .. }) => {
                journal.unsettled.remove(&key);
                journal.results.insert(key, result);
            }
            Ok(JournalLine::Settled { key, .. }) => {
                journal.unsettled.remove(&key);
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
fn read_bounded_line(reader: &mut BufReader<File>, buffer: &mut Vec<u8>) -> std::io::Result<usize> {
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

/// Reads the `args` bytes the run's latest attempt ran with, for a resume that omits them.
///
/// `Ok(None)` means that attempt had no `args`. The caller checks the bytes against the call that
/// supplied them before trusting them, exactly as it does for `script.js`.
pub fn read_run_args(
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
        .join(ARGS_FILE);
    match fs::read(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("无法读取运行 {run_id} 保存的 args：{error}")),
        Ok(bytes) => Ok(Some(bytes)),
    }
}

/// Which provider calls supplied a run's current `script.js` and `args.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunProvenance {
    pub script: Option<String>,
    pub args: Option<String>,
}

fn read_provenance(path: &Path) -> Option<RunProvenance> {
    let body = serde_json::from_slice::<Value>(&fs::read(path).ok()?).ok()?;
    let field = |key: &str| {
        body.get(key)
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
    };
    Some(RunProvenance {
        script: field("script"),
        args: field("args"),
    })
}

/// Reads which calls supplied a run's files, for a resume that reuses them. `None` means no
/// attempt recorded any — a run from before provenance was kept, or one whose write failed.
pub fn read_run_provenance(
    app_data_path: &Path,
    conversation_id: &str,
    run_id: &str,
) -> Result<Option<RunProvenance>, String> {
    validate_path_component("会话 id", conversation_id)?;
    validate_path_component("运行 id", run_id)?;
    Ok(read_provenance(
        &app_data_path
            .join(RUNS_DIRECTORY)
            .join(conversation_id)
            .join(run_id)
            .join(PROVENANCE_FILE),
    ))
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
    for component in [
        None,
        Some(conversation_id),
        Some(run_id),
        Some(STEPS_DIRECTORY),
    ] {
        if let Some(component) = component {
            checked = checked.join(component);
        }
        if fs::symlink_metadata(&checked).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err("工作流历史路径不能是符号链接".into());
        }
    }
    if fs::symlink_metadata(&path).is_ok_and(|metadata| {
        metadata.file_type().is_symlink() || metadata.len() > 16 * 1024 * 1024
    }) {
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
        Ok(metadata) if !metadata.is_dir() => Err("工作流运行路径不是目录，拒绝删除".into()),
        Ok(_) => fs::remove_dir_all(&directory)
            .map(|()| true)
            .map_err(|error| format!("无法删除工作流运行目录：{error}")),
    }
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

pub(crate) fn document_conversation_ids(
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
                manifest["interruptedAt"] = Value::String(chrono::Utc::now().to_rfc3339());
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
        })
        .unwrap();
    }

    #[test]
    fn journal_append_requires_sync_after_complete_write() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("journal");
        let line = JournalLine::Started {
            key: "k".into(),
            agent_id: "a".into(),
        };
        let mut expected = serde_json::to_vec(&line).unwrap();
        expected.push(b'\n');
        let called = std::cell::Cell::new(false);
        let result = append_line_with_sync(&path, &line, |_| {
            assert_eq!(fs::read(&path).unwrap(), expected);
            called.set(true);
            Err(std::io::Error::other("injected append sync failure"))
        });
        assert!(called.get(), "append must reach the persistence barrier");
        assert_eq!(
            result.unwrap_err().to_string(),
            "injected append sync failure"
        );
        append_line_with_sync(&path, &line, |file| {
            assert_eq!(fs::read(&path).unwrap(), expected.repeat(2));
            file.sync_all()
        })
        .unwrap();
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

    /// The run id is a model-chosen name now, so "is this id available" has to answer for the two
    /// ways a name can be unusable: another run already owns it, or the filesystem will not.
    #[test]
    fn a_taken_or_unwritable_run_id_is_reported_as_unavailable() {
        let directory = tempfile::tempdir().unwrap();
        assert!(!run_id_taken(directory.path(), "conv1", "sweep"));
        RunStore::open(directory.path(), "conv1", "sweep", b"{}").unwrap();
        assert!(run_id_taken(directory.path(), "conv1", "sweep"));
        // Another conversation's runs are its own.
        assert!(!run_id_taken(directory.path(), "conv2", "sweep"));
        // Legal agent names that Windows refuses as directories. Reporting them as taken sends the
        // caller to a numbered variant instead of leaving the run without a directory.
        for reserved in ["con", "prn", "aux", "nul", "com1", "lpt9"] {
            assert!(
                run_id_taken(directory.path(), "conv1", reserved),
                "{reserved} 不能当运行目录名"
            );
        }
        for usable in ["com", "com10", "console", "auxiliary", "nulls"] {
            assert!(
                !run_id_taken(directory.path(), "conv1", usable),
                "{usable} 是可用的普通名字"
            );
        }
        // An id the store cannot address at all is never available to claim.
        assert!(run_id_taken(directory.path(), "conv1", "../escape"));
    }

    /// Three dispatch paths discard a store after a failure that leaves nothing running under its
    /// id. The id is the name the model chose, so a directory that outlived its failed dispatch
    /// would push the next attempt at that name onto a numbered variant. A resume must survive the
    /// same call, because its directory belongs to the run being resumed.
    #[test]
    fn discarding_removes_a_directory_this_open_created_and_keeps_one_it_found() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .join(RUNS_DIRECTORY)
            .join("conv1")
            .join("sweep");

        let created = RunStore::open(directory.path(), "conv1", "sweep", b"{}").unwrap();
        assert!(created.is_fresh());
        assert!(path.is_dir());
        created.discard_if_fresh();
        assert!(!path.exists(), "失败的首次派发不得留下运行目录");

        let created = RunStore::open(directory.path(), "conv1", "sweep", b"{}").unwrap();
        created.discard_if_fresh();
        let resumed = RunStore::open(directory.path(), "conv1", "sweep", b"{}").unwrap();
        assert!(resumed.is_fresh(), "目录已被清理，这仍然是一次新建");
        drop(resumed);
        let resumed = RunStore::open(directory.path(), "conv1", "sweep", b"{}").unwrap();
        assert!(!resumed.is_fresh());
        resumed.discard_if_fresh();
        assert!(path.is_dir(), "恢复用的运行目录必须留下");
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

    /// The chain's three answers come from the latest record per key: a result is reusable
    /// whenever it was written, a key last seen `started` is unsettled, and a key whose null the
    /// plan consumed is a miss — until a later attempt starts it again and never settles it.
    #[test]
    fn a_key_reads_as_unsettled_while_its_latest_record_is_a_start() {
        use workflow_core::chain::JournalLookup;
        let directory = tempfile::tempdir().unwrap();
        let store = store(directory.path());
        let started = |key: &str| {
            store.append(&JournalLine::Started {
                key: key.into(),
                agent_id: "ws1".into(),
            })
        };
        let settled = |key: &str| {
            store.append(&JournalLine::Settled {
                key: key.into(),
                agent_id: "ws1".into(),
                error: None,
            })
        };
        started("mw1:done");
        store.append(&JournalLine::Result {
            key: "mw1:done".into(),
            agent_id: "ws1".into(),
            result: json!("value"),
        });
        started("mw1:crashed");
        started("mw1:failed");
        settled("mw1:failed");
        started("mw1:failed-then-crashed");
        settled("mw1:failed-then-crashed");
        started("mw1:failed-then-crashed");
        started("mw1:failed-then-done");
        settled("mw1:failed-then-done");
        started("mw1:failed-then-done");
        store.append(&JournalLine::Result {
            key: "mw1:failed-then-done".into(),
            agent_id: "ws2".into(),
            result: json!(1),
        });

        let journal = store.load_journal();
        assert_eq!(journal.lookup("mw1:done"), JournalLookup::Hit);
        assert_eq!(journal.lookup("mw1:crashed"), JournalLookup::Unsettled);
        assert_eq!(journal.lookup("mw1:failed"), JournalLookup::Miss);
        assert_eq!(
            journal.lookup("mw1:failed-then-crashed"),
            JournalLookup::Unsettled,
            "最近一次尝试没结算，计划没收到它的结果"
        );
        assert_eq!(journal.lookup("mw1:failed-then-done"), JournalLookup::Hit);
        assert_eq!(journal.lookup("mw1:never"), JournalLookup::Miss);
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
            file.write_all(b"{\"type\":\"started\",\"key\":\"mw1:pad")
                .unwrap();
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

    /// `open` pins a fresh run's script; an edited script only enters through a resume's
    /// `replace_source`, after that script was approved on its own.
    #[test]
    fn reopening_with_an_edited_body_refuses_to_resume_under_the_old_approval() {
        let directory = tempfile::tempdir().unwrap();
        let first = RunStore::open(directory.path(), "conv1", "run1", b"{\"a\":1}").unwrap();
        let again = RunStore::open(directory.path(), "conv1", "run1", b"{\"a\":1}").unwrap();
        assert_eq!(first.source_digest(), again.source_digest());

        let error = RunStore::open(directory.path(), "conv1", "run1", b"{\"a\":2}").unwrap_err();
        assert!(error.contains("脚本内容在批准后发生变化"), "{error}");
    }

    /// A resume opens the run it names without creating one, and adopts a re-approved script by
    /// replacing the pinned one whole.
    #[test]
    fn a_resume_opens_only_an_existing_run_and_adopts_its_reapproved_script() {
        let directory = tempfile::tempdir().unwrap();
        let run_path = directory
            .path()
            .join(RUNS_DIRECTORY)
            .join("conv1")
            .join("run1");
        assert!(RunStore::open_existing(directory.path(), "conv1", "run1")
            .unwrap()
            .is_none());
        assert!(!run_path.exists(), "找不到的恢复不得建出运行目录");

        let original = RunStore::open(directory.path(), "conv1", "run1", b"old").unwrap();
        let original_digest = original.source_digest().to_owned();
        drop(original);
        let mut resumed = RunStore::open_existing(directory.path(), "conv1", "run1")
            .unwrap()
            .expect("既有运行可以恢复");
        assert_eq!(resumed.source_digest(), original_digest);

        resumed.replace_source(b"old").unwrap();
        assert_eq!(resumed.source_digest(), original_digest, "同一脚本不重写");
        resumed.replace_source(b"edited").unwrap();
        assert_eq!(resumed.source_digest(), hex_digest(b"edited"));
        assert_eq!(
            read_run_script(directory.path(), "conv1", "run1").unwrap(),
            Some(b"edited".to_vec())
        );
        assert!(
            !run_path.join("script.js.staged").exists(),
            "暂存文件改名后不得留下"
        );
        assert_eq!(
            RunStore::open_existing(directory.path(), "conv1", "run1")
                .unwrap()
                .unwrap()
                .source_digest(),
            hex_digest(b"edited")
        );
    }

    /// The args a run ran with are saved as the exact bytes the timeline fingerprints.
    #[test]
    fn saved_args_are_the_bytes_the_timeline_fingerprints() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(directory.path());
        assert_eq!(read_run_args(directory.path(), "conv1", "run1").unwrap(), None);

        let args = json!({"files": ["a.rs", "b.rs"], "depth": 2});
        store.write_args(&args);
        assert_eq!(
            read_run_args(directory.path(), "conv1", "run1").unwrap(),
            Some(serde_json::to_vec(&args).unwrap())
        );
        let replaced = json!(["c.rs"]);
        store.write_args(&replaced);
        assert_eq!(
            read_run_args(directory.path(), "conv1", "run1").unwrap(),
            Some(serde_json::to_vec(&replaced).unwrap())
        );
    }

    /// A later attempt that runs a different step at an index overwrites that index's record; a
    /// replay of the earlier step must not claim it. Records from before key files stay claimable.
    #[test]
    fn a_step_record_overwritten_by_another_step_is_not_claimed_by_the_replay() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(directory.path());
        assert!(store.write_step(0, "mw1:first", &json!({"task": "first"})));
        assert!(store.step_matches(0, "mw1:first"));
        assert!(store.write_step(0, "mw1:second", &json!({"task": "second"})));
        assert!(!store.step_matches(0, "mw1:first"));
        assert!(store.step_matches(0, "mw1:second"));

        let steps = store.directory().join(STEPS_DIRECTORY);
        fs::write(steps.join("1.json"), b"{}").unwrap();
        assert!(store.step_matches(1, "mw1:anything"), "旧记录没有键文件，照旧认领");
    }

    /// Run records follow conversation deletion and cannot remove another conversation's records.
    #[test]
    fn a_conversation_deletion_removes_its_runs_and_only_its_runs() {
        let directory = tempfile::tempdir().unwrap();
        let kept = RunStore::open(directory.path(), "convkeep", "run1", b"{}").unwrap();
        let removed_store = RunStore::open(directory.path(), "convgone", "run1", b"{}").unwrap();

        let mut previous = crate::catalog::default_document();
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
        let mut document = crate::catalog::default_document();
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
        let mut document = crate::catalog::default_document();
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
    #[test]
    fn a_live_driver_lock_shields_a_running_manifest_from_the_sweep() {
        let directory = tempfile::tempdir().unwrap();
        let mut document = crate::catalog::default_document();
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
        assert!(!store.step_matches(0, "mw1:a"));
        let record = json!({"task": "检查调度器", "status": "completed"});
        assert!(store.write_step(0, "mw1:a", &record));
        assert!(store.step_matches(0, "mw1:a"));

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
}
