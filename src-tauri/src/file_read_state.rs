//! What a conversation has read, as the file tools remember it.
//!
//! The host half of Claude Code's `readFileState`: one record per canonical
//! path the model has read or written, holding the file's modification time at
//! that moment and its normalized text. `edit` and `write` consult it before
//! touching a file, the round loop scans it for changes made behind the model's
//! back, and the hook and shell legs refresh or cite it.
//!
//! Process-local by design, like the shell sessions: a restart forgets it, and
//! the first edit after that is answered with "read it again" — exactly what a
//! resumed Claude Code session does.

use std::{
    collections::{HashMap, HashSet},
    fs::Metadata,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

/// Claude Code's `XT`: entries a scope keeps before the least recently touched
/// go.
pub const MAX_ENTRIES_PER_SCOPE: usize = 5_000;
/// Claude Code's `T`: bytes of remembered text per scope. Past it the least
/// recently touched entries keep their hash and time but drop their text, so
/// staleness is still decided and only the diff snippet is lost.
pub const MAX_CONTENT_BYTES_PER_SCOPE: usize = 25 * 1024 * 1024;
/// Child scopes one conversation keeps before the oldest is dropped. A child
/// scope is a subagent's clone of its parent's record; nothing tears it down
/// when the agent ends, so the cap is what bounds a long conversation.
const MAX_CHILD_SCOPES_PER_CONVERSATION: usize = 64;

/// Separates a conversation id from the child suffix in a child scope id.
const CHILD_SCOPE_SEPARATOR: &str = "#child:";

/// One remembered file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileReadRecord {
    /// Whole milliseconds of the file's modification time when the text was
    /// taken. Compared with a fresh stat by `>`; equal means unchanged.
    pub modified_ms: i64,
    /// BOM-stripped, LF-normalized text. `None` for a partial read, or once
    /// the scope's text budget evicted it.
    pub content: Option<String>,
    /// Hash of the full normalized text, kept even after the text is dropped.
    /// `None` for a partial read: a slice cannot vouch for the whole file.
    pub content_hash: Option<u64>,
    /// Whether the record covers the whole file: no line range, not cut by
    /// the line cap. Only a full record can rescue a stale check by content,
    /// and only a full record is scanned for external changes.
    pub full: bool,
    /// Whether the model has seen `content`. False after a hook re-sync or a
    /// stale-recovered edit: the file on disk is known, but the model's copy
    /// of it is not the current one.
    pub in_model_context: bool,
}

impl FileReadRecord {
    /// A whole-file text read.
    pub fn full_read(modified_ms: i64, normalized: String) -> Self {
        Self {
            modified_ms,
            content_hash: Some(content_hash(&normalized)),
            content: Some(normalized),
            full: true,
            in_model_context: true,
        }
    }

    /// A ranged read. It satisfies "was this file read" but vouches for
    /// nothing about the whole file.
    pub fn partial_read(modified_ms: i64) -> Self {
        Self {
            modified_ms,
            content: None,
            content_hash: None,
            full: false,
            in_model_context: true,
        }
    }

    /// The record after the model's own write: the text it just sent, at the
    /// modification time the write left behind.
    pub fn written(modified_ms: i64, normalized: String, in_model_context: bool) -> Self {
        Self {
            in_model_context,
            ..Self::full_read(modified_ms, normalized)
        }
    }

    /// Whether `normalized` is the text this record remembers.
    pub fn matches(&self, normalized: &str) -> bool {
        self.full && self.content_hash == Some(content_hash(normalized))
    }
}

/// The text the record keeps: no byte-order mark, LF line endings. Both sides
/// of every comparison go through this, so a formatter that only touched line
/// endings still reads as a change (the hash differs) while the model's own
/// LF-spelled edit of a CRLF file does not.
pub fn normalize_text(raw: &str) -> String {
    let stripped = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    if stripped.contains('\r') {
        stripped.replace("\r\n", "\n")
    } else {
        stripped.to_owned()
    }
}

pub fn content_hash(text: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// Whole milliseconds of a file's modification time, as Claude Code floors
/// `mtimeMs`. A clock before the epoch reads as `i64::MIN`, which no stat can
/// later beat, so such a file is never reported stale.
pub fn modified_ms_of(metadata: &Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(system_time_ms)
        .unwrap_or(i64::MIN)
}

fn system_time_ms(time: SystemTime) -> Option<i64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|since| i64::try_from(since.as_millis()).ok())
}

/// Stats `path` for its modification time. `None` when the file is gone or
/// unreadable.
pub fn modified_ms(path: &Path) -> Option<i64> {
    std::fs::metadata(path).ok().map(|metadata| modified_ms_of(&metadata))
}

/// Whole milliseconds now, on the clock the records are compared against.
pub fn now_ms() -> i64 {
    system_time_ms(SystemTime::now()).unwrap_or(0)
}

/// Leading segment of the key a file on another machine is remembered under.
///
/// A NUL can appear in no real path on any platform, so a key starting with one
/// can never collide with a host file, and a stat of it fails at once rather
/// than reaching the network the way a UNC-shaped spelling would.
const REMOTE_KEY_PREFIX: &str = "\u{0}mework-remote\u{0}";

/// The registry key for `path` on the machine addressed by `machine_key`
/// (`run_environment::env_key`), which is the machine's identity everywhere
/// else in the host: one machine's `/srv/app` is not another's.
///
/// `path` is the path the remote shell resolved, not the one the model typed,
/// so `~/app/x` and `/home/dev/app/x` share one record.
pub fn remote_key(machine_key: &str, path: &str) -> PathBuf {
    PathBuf::from(format!("{REMOTE_KEY_PREFIX}{machine_key}\u{0}{path}"))
}

/// Whether a registry key names a file on another machine. Everything that
/// stats its keys on this host — the external-change scan, the shell hint —
/// has to skip these: a stat here says nothing about the file there.
pub fn is_remote_key(path: &Path) -> bool {
    path.to_str()
        .is_some_and(|text| text.starts_with(REMOTE_KEY_PREFIX))
}

/// The machine key and remote path a remote registry key was minted from.
pub fn remote_key_parts(path: &Path) -> Option<(&str, &str)> {
    path.to_str()?
        .strip_prefix(REMOTE_KEY_PREFIX)?
        .split_once('\u{0}')
}

/// Which scope a call consults, and where to seed it from on first use.
#[derive(Clone, Copy, Debug)]
pub struct ScopeRef<'a> {
    pub id: &'a str,
    pub parent: Option<&'a str>,
}

/// Mints a child scope id under `conversation_id`. The conversation prefix is
/// what lets deletion of the conversation drop every child scope with it.
pub fn child_scope_id(conversation_id: &str) -> String {
    format!(
        "{conversation_id}{CHILD_SCOPE_SEPARATOR}{}",
        uuid::Uuid::new_v4().simple()
    )
}

fn conversation_of_scope(scope: &str) -> &str {
    scope
        .split_once(CHILD_SCOPE_SEPARATOR)
        .map(|(conversation, _)| conversation)
        .unwrap_or(scope)
}

struct Entry {
    record: FileReadRecord,
    touched: u64,
}

#[derive(Default)]
struct Scope {
    entries: HashMap<PathBuf, Entry>,
    content_bytes: usize,
    clock: u64,
    /// When this scope was created, for evicting the oldest child scopes.
    born: u64,
}

impl Scope {
    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    fn cloned_from(parent: &Scope, born: u64) -> Self {
        Self {
            entries: parent
                .entries
                .iter()
                .map(|(path, entry)| {
                    (
                        path.clone(),
                        Entry {
                            record: entry.record.clone(),
                            touched: entry.touched,
                        },
                    )
                })
                .collect(),
            content_bytes: parent.content_bytes,
            clock: parent.clock,
            born,
        }
    }

    fn insert(&mut self, path: PathBuf, record: FileReadRecord) {
        if let Some(previous) = self.entries.remove(&path) {
            self.content_bytes = self
                .content_bytes
                .saturating_sub(text_bytes(&previous.record));
        }
        self.content_bytes += text_bytes(&record);
        let touched = self.tick();
        self.entries.insert(path, Entry { record, touched });
        self.enforce_budgets();
    }

    fn enforce_budgets(&mut self) {
        while self.entries.len() > MAX_ENTRIES_PER_SCOPE {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.touched)
                .map(|(path, _)| path.clone())
            else {
                break;
            };
            if let Some(entry) = self.entries.remove(&oldest) {
                self.content_bytes = self.content_bytes.saturating_sub(text_bytes(&entry.record));
            }
        }
        while self.content_bytes > MAX_CONTENT_BYTES_PER_SCOPE {
            let Some(oldest) = self
                .entries
                .iter()
                .filter(|(_, entry)| entry.record.content.is_some())
                .min_by_key(|(_, entry)| entry.touched)
                .map(|(path, _)| path.clone())
            else {
                break;
            };
            if let Some(entry) = self.entries.get_mut(&oldest) {
                self.content_bytes = self.content_bytes.saturating_sub(text_bytes(&entry.record));
                entry.record.content = None;
            }
        }
    }
}

fn text_bytes(record: &FileReadRecord) -> usize {
    record.content.as_ref().map_or(0, String::len)
}

/// Every scope's records, keyed by scope id.
#[derive(Default)]
pub struct FileReadRegistry {
    scopes: Mutex<HashMap<String, Scope>>,
    /// Creation order for scopes, so the oldest child scope is the one to go.
    births: Mutex<u64>,
}

impl FileReadRegistry {
    fn with_scope<T>(
        &self,
        scope: ScopeRef<'_>,
        create: bool,
        body: impl FnOnce(&mut Scope) -> T,
    ) -> Option<T> {
        let mut scopes = self
            .scopes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !scopes.contains_key(scope.id) {
            if !create {
                return None;
            }
            let born = {
                let mut births = self
                    .births
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                *births += 1;
                *births
            };
            // A child scope starts as its parent's record, the way Claude Code
            // clones `readFileState` for a subagent: what the parent read, the
            // child may edit. From here on the two diverge.
            let seeded = scope
                .parent
                .and_then(|parent| scopes.get(parent))
                .map(|parent| Scope::cloned_from(parent, born))
                .unwrap_or_else(|| Scope {
                    born,
                    ..Scope::default()
                });
            scopes.insert(scope.id.to_owned(), seeded);
            evict_excess_child_scopes(&mut scopes, conversation_of_scope(scope.id));
        }
        scopes.get_mut(scope.id).map(body)
    }

    /// The record for `path`, touching it as recently used.
    pub fn get(&self, scope: ScopeRef<'_>, path: &Path) -> Option<FileReadRecord> {
        self.with_scope(scope, true, |state| {
            let tick = state.tick();
            state.entries.get_mut(path).map(|entry| {
                entry.touched = tick;
                entry.record.clone()
            })
        })
        .flatten()
    }

    pub fn record(&self, scope: ScopeRef<'_>, path: PathBuf, record: FileReadRecord) {
        self.with_scope(scope, true, |state| state.insert(path, record));
    }

    pub fn forget(&self, scope: ScopeRef<'_>, path: &Path) {
        self.with_scope(scope, false, |state| {
            if let Some(entry) = state.entries.remove(path) {
                state.content_bytes = state.content_bytes.saturating_sub(text_bytes(&entry.record));
            }
        });
    }

    /// Every full record in the scope, for the external-change scan. A
    /// snapshot: the scan stats and reads outside the lock.
    pub fn full_records(&self, scope: ScopeRef<'_>) -> Vec<(PathBuf, FileReadRecord)> {
        self.with_scope(scope, false, |state| {
            let mut records = state
                .entries
                .iter()
                .filter(|(_, entry)| entry.record.full)
                .map(|(path, entry)| (path.clone(), entry.record.clone()))
                .collect::<Vec<_>>();
            records.sort_by(|left, right| left.0.cmp(&right.0));
            records
        })
        .unwrap_or_default()
    }

    /// Every recorded path with its modification time, full or not, for the
    /// shell hint.
    pub fn recorded_times(&self, scope: ScopeRef<'_>) -> Vec<(PathBuf, i64)> {
        self.with_scope(scope, false, |state| {
            let mut times = state
                .entries
                .iter()
                .map(|(path, entry)| (path.clone(), entry.record.modified_ms))
                .collect::<Vec<_>>();
            times.sort_by(|left, right| left.0.cmp(&right.0));
            times
        })
        .unwrap_or_default()
    }

    /// Drops a conversation's scope and every child scope minted under it.
    pub fn retire_conversation(&self, conversation_id: &str) {
        let mut scopes = self
            .scopes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        scopes.retain(|scope, _| conversation_of_scope(scope) != conversation_id);
    }

    /// Drops every scope whose conversation is not in `retained`. Called after
    /// a document save, the one event that can make a conversation go away.
    pub fn retain_conversations(&self, retained: &HashSet<String>) {
        let mut scopes = self
            .scopes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        scopes.retain(|scope, _| retained.contains(conversation_of_scope(scope)));
    }

    #[cfg(test)]
    pub(crate) fn scope_count(&self) -> usize {
        self.scopes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }
}

fn evict_excess_child_scopes(scopes: &mut HashMap<String, Scope>, conversation_id: &str) {
    loop {
        let mut children = scopes
            .iter()
            .filter(|(scope, _)| {
                scope.contains(CHILD_SCOPE_SEPARATOR) && conversation_of_scope(scope) == conversation_id
            })
            .map(|(scope, state)| (state.born, scope.clone()))
            .collect::<Vec<_>>();
        if children.len() <= MAX_CHILD_SCOPES_PER_CONVERSATION {
            break;
        }
        children.sort();
        scopes.remove(&children[0].1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(id: &str) -> ScopeRef<'_> {
        ScopeRef { id, parent: None }
    }

    #[test]
    fn normalization_strips_the_bom_and_carriage_returns() {
        assert_eq!(normalize_text("\u{feff}a\r\nb\r\n"), "a\nb\n");
        assert_eq!(normalize_text("plain\n"), "plain\n");
        assert_eq!(normalize_text("mixed\r\nlf\nend"), "mixed\nlf\nend");
    }

    #[test]
    fn a_full_record_matches_its_own_text_and_a_partial_one_vouches_for_nothing() {
        let full = FileReadRecord::full_read(10, "alpha\n".into());
        assert!(full.matches("alpha\n"));
        assert!(!full.matches("alpha\nbeta\n"));
        let partial = FileReadRecord::partial_read(10);
        assert!(!partial.matches(""));
        assert!(partial.content_hash.is_none());
    }

    #[test]
    fn a_child_scope_starts_as_a_copy_of_its_parent_and_then_diverges() {
        let registry = FileReadRegistry::default();
        let path = PathBuf::from("/w/a.txt");
        registry.record(scope("conv"), path.clone(), FileReadRecord::full_read(1, "a".into()));
        let child_id = child_scope_id("conv");
        let child = ScopeRef {
            id: &child_id,
            parent: Some("conv"),
        };
        assert_eq!(
            registry.get(child, &path).map(|record| record.modified_ms),
            Some(1)
        );
        registry.record(child, path.clone(), FileReadRecord::full_read(2, "b".into()));
        assert_eq!(
            registry.get(scope("conv"), &path).map(|record| record.modified_ms),
            Some(1),
            "the parent must not see the child's write"
        );
        assert_eq!(
            registry.get(child, &path).map(|record| record.modified_ms),
            Some(2)
        );
        registry.retire_conversation("conv");
        assert_eq!(registry.scope_count(), 0, "retiring drops the children too");
    }

    #[test]
    fn retaining_conversations_forgets_the_rest() {
        let registry = FileReadRegistry::default();
        registry.record(
            scope("keep"),
            PathBuf::from("/k"),
            FileReadRecord::partial_read(1),
        );
        registry.record(
            scope("drop"),
            PathBuf::from("/d"),
            FileReadRecord::partial_read(1),
        );
        registry.retain_conversations(&HashSet::from(["keep".to_owned()]));
        assert!(registry.get(scope("keep"), Path::new("/k")).is_some());
        assert!(registry
            .with_scope(scope("drop"), false, |_| ())
            .is_none());
    }

    #[test]
    fn the_text_budget_drops_text_but_keeps_the_hash_and_time() {
        let registry = FileReadRegistry::default();
        let big = "x".repeat(MAX_CONTENT_BYTES_PER_SCOPE / 2 + 1);
        registry.record(
            scope("c"),
            PathBuf::from("/one"),
            FileReadRecord::full_read(1, big.clone()),
        );
        registry.record(
            scope("c"),
            PathBuf::from("/two"),
            FileReadRecord::full_read(2, big.clone()),
        );
        let one = registry.get(scope("c"), Path::new("/one")).unwrap();
        assert!(one.content.is_none(), "the older text is evicted");
        assert!(one.matches(&big), "but it still recognises its own text");
        assert_eq!(one.modified_ms, 1);
        let two = registry.get(scope("c"), Path::new("/two")).unwrap();
        assert!(two.content.is_some());
    }

    #[test]
    fn the_entry_cap_evicts_the_least_recently_touched() {
        let registry = FileReadRegistry::default();
        for index in 0..=MAX_ENTRIES_PER_SCOPE {
            registry.record(
                scope("c"),
                PathBuf::from(format!("/f{index}")),
                FileReadRecord::partial_read(index as i64),
            );
        }
        assert!(registry.get(scope("c"), Path::new("/f0")).is_none());
        assert!(registry
            .get(scope("c"), Path::new(&format!("/f{MAX_ENTRIES_PER_SCOPE}")))
            .is_some());
    }

    #[test]
    fn child_scopes_are_capped_per_conversation() {
        let registry = FileReadRegistry::default();
        let mut ids = Vec::new();
        for _ in 0..=MAX_CHILD_SCOPES_PER_CONVERSATION {
            let id = child_scope_id("conv");
            registry.record(
                ScopeRef {
                    id: &id,
                    parent: Some("conv"),
                },
                PathBuf::from("/p"),
                FileReadRecord::partial_read(1),
            );
            ids.push(id);
        }
        assert_eq!(registry.scope_count(), MAX_CHILD_SCOPES_PER_CONVERSATION);
        assert!(registry
            .with_scope(
                ScopeRef {
                    id: &ids[0],
                    parent: None
                },
                false,
                |_| ()
            )
            .is_none());
    }

    #[test]
    fn remote_keys_collide_with_no_host_path_and_are_told_apart_by_machine() {
        let key = remote_key("ssh:m1", "/srv/app/main.rs");
        assert!(is_remote_key(&key));
        assert_eq!(remote_key_parts(&key), Some(("ssh:m1", "/srv/app/main.rs")));
        assert_ne!(key, remote_key("wsl:Ubuntu", "/srv/app/main.rs"));
        assert!(!is_remote_key(Path::new("/srv/app/main.rs")));
        assert!(!is_remote_key(Path::new("C:/srv/app/main.rs")));
        // A stat of the key fails at once: nothing on this host can be named by it.
        assert!(modified_ms(&key).is_none());
    }
}
