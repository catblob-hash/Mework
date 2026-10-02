//! The process's one shared memory pool.
//!
//! Every conversation keeps an in-memory cache — its body, the wire projection
//! its next request reuses — and attachments keep their chips and raw bytes;
//! all of them draw on this one budget. Each entry has a priority, and once the
//! pool is over its cap it unloads the least recently used entry of the lowest
//! priority first:
//!
//! - [`Priority::High`]: what a conversation shows and sends — user messages,
//!   reasoning, assistant text, tool output, pasted text, the small chip an
//!   image shows on the timeline, and the wire projection. Opening a
//!   conversation or expanding a card must not wait on disk.
//! - [`Priority::Low`]: the raw bytes of images and uploaded files. Only the
//!   full-size viewer and a request's hydration read them, and both can afford
//!   a read from disk.
//!
//! Only data is managed here, never a process. What goes in is a copy of
//! something persisted — the database, the attachment directories — so that
//! unloading it costs a read and nothing else. The host itself, a run and its
//! agents, the AI SDK sidecar, remote agents, terminals and MCP sessions hold
//! their state outside the pool and are never unloaded; do not put anything
//! in the pool that cannot be read back from disk.
//!
//! The tiers share the one cap; only the order of unloading differs. Nothing
//! on the host needs pinning: a run works from its own request timeline and
//! wire session, never from a pooled entry, so unloading one mid-run costs the
//! next turn a read and nothing else. (The renderer's pool does pin — the
//! conversation on screen, one with a run in flight — see `lib/memoryPool.ts`.)
//!
//! Unloading only drops the pool's reference: whoever already holds the `Arc`
//! keeps a valid value, so an eviction never invalidates work in progress, and
//! a value too large for the pool on its own is still returned to the caller
//! that loaded it.

use std::{
    any::Any,
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard, OnceLock},
};

use serde::Serialize;

/// The pool's cap: 2 GiB across every tier.
pub const POOL_CAP_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Unloading order: every `Low` entry goes before any `High` one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    Low,
    High,
}

/// What an entry is. The kind fixes the priority, so no caller can file raw
/// image bytes as timeline content by mistake.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PoolKind {
    /// A conversation's contexts, branches and queue, as the store holds them.
    ConversationBody,
    /// A conversation's projected provider history (`wire_history`).
    WireProjection,
    /// The small image an image attachment shows on the timeline.
    ImageThumbnail,
    /// An image attachment's full bytes, as the data URL the viewer shows.
    ImageData,
    /// An uploaded file's bytes, as the data URL its preview reads.
    FileData,
}

impl PoolKind {
    pub const fn priority(self) -> Priority {
        match self {
            Self::ConversationBody | Self::WireProjection | Self::ImageThumbnail => Priority::High,
            Self::ImageData | Self::FileData => Priority::Low,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PoolKey {
    pub kind: PoolKind,
    pub id: String,
}

impl PoolKey {
    pub fn new(kind: PoolKind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
        }
    }
}

struct Entry {
    value: Arc<dyn Any + Send + Sync>,
    bytes: u64,
    last_used: u64,
}

#[derive(Default)]
struct Inner {
    entries: HashMap<PoolKey, Entry>,
    total: u64,
    tick: u64,
}

impl Inner {
    fn touch(&mut self) -> u64 {
        self.tick += 1;
        self.tick
    }

    fn remove(&mut self, key: &PoolKey) -> bool {
        match self.entries.remove(key) {
            Some(entry) => {
                self.total = self.total.saturating_sub(entry.bytes);
                true
            }
            None => false,
        }
    }
}

pub struct MemoryPool {
    cap: u64,
    inner: Mutex<Inner>,
}

impl MemoryPool {
    pub fn new(cap: u64) -> Self {
        Self {
            cap,
            inner: Mutex::new(Inner::default()),
        }
    }

    /// The pool every host cache shares.
    pub fn global() -> &'static MemoryPool {
        static POOL: OnceLock<MemoryPool> = OnceLock::new();
        POOL.get_or_init(|| MemoryPool::new(POOL_CAP_BYTES))
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The cached value, marked as just used. `None` when it was never loaded,
    /// was unloaded, or was stored as another type.
    pub fn get<T: Any + Send + Sync>(&self, key: &PoolKey) -> Option<Arc<T>> {
        let mut inner = self.lock();
        let tick = inner.touch();
        let entry = inner.entries.get_mut(key)?;
        entry.last_used = tick;
        entry.value.clone().downcast::<T>().ok()
    }

    /// The cached value without counting it as a use: for bookkeeping passes
    /// (pruning, re-mirroring) that must not reorder what gets unloaded.
    pub fn peek<T: Any + Send + Sync>(&self, key: &PoolKey) -> Option<Arc<T>> {
        let inner = self.lock();
        inner.entries.get(key)?.value.clone().downcast::<T>().ok()
    }

    pub fn contains(&self, key: &PoolKey) -> bool {
        self.lock().entries.contains_key(key)
    }

    /// Every loaded key of one kind, in no particular order.
    pub fn keys_of(&self, kind: PoolKind) -> Vec<PoolKey> {
        self.lock()
            .entries
            .keys()
            .filter(|key| key.kind == kind)
            .cloned()
            .collect()
    }

    /// Stores `value` as just used, replacing any earlier value under `key`,
    /// then unloads what the cap requires. Returns the keys it unloaded.
    pub fn insert<T: Any + Send + Sync>(&self, key: PoolKey, value: Arc<T>, bytes: u64) -> Vec<PoolKey> {
        let mut inner = self.lock();
        let tick = inner.touch();
        self.store(&mut inner, key, value, bytes, tick)
    }

    /// Like [`insert`](Self::insert), but keeps the replaced entry's place in
    /// the unloading order: a background refresh is not a use.
    pub fn replace<T: Any + Send + Sync>(&self, key: PoolKey, value: Arc<T>, bytes: u64) -> Vec<PoolKey> {
        let mut inner = self.lock();
        let tick = match inner.entries.get(&key) {
            Some(entry) => entry.last_used,
            None => inner.touch(),
        };
        self.store(&mut inner, key, value, bytes, tick)
    }

    fn store<T: Any + Send + Sync>(
        &self,
        inner: &mut Inner,
        key: PoolKey,
        value: Arc<T>,
        bytes: u64,
        last_used: u64,
    ) -> Vec<PoolKey> {
        inner.remove(&key);
        inner.total = inner.total.saturating_add(bytes);
        inner.entries.insert(
            key,
            Entry {
                value,
                bytes,
                last_used,
            },
        );
        self.enforce(inner)
    }

    pub fn remove(&self, key: &PoolKey) -> bool {
        self.lock().remove(key)
    }

    /// Drops every entry `keep` rejects.
    pub fn retain(&self, mut keep: impl FnMut(&PoolKey) -> bool) {
        let mut inner = self.lock();
        let doomed: Vec<PoolKey> = inner
            .entries
            .keys()
            .filter(|key| !keep(key))
            .cloned()
            .collect();
        for key in doomed {
            inner.remove(&key);
        }
    }

    /// Unloads entries, lowest priority and least recently used first, until
    /// the pool is back under its cap.
    fn enforce(&self, inner: &mut Inner) -> Vec<PoolKey> {
        if inner.total <= self.cap {
            return Vec::new();
        }
        let mut candidates: Vec<(Priority, u64, PoolKey)> = inner
            .entries
            .iter()
            .map(|(key, entry)| (key.kind.priority(), entry.last_used, key.clone()))
            .collect();
        candidates.sort_by(|left, right| (left.0, left.1).cmp(&(right.0, right.1)));
        let mut unloaded = Vec::new();
        for (_, _, key) in candidates {
            if inner.total <= self.cap {
                break;
            }
            inner.remove(&key);
            unloaded.push(key);
        }
        unloaded
    }

    #[cfg(test)]
    fn total_bytes(&self) -> u64 {
        self.lock().total
    }

    /// When `key` was last used, as the pool's own clock counts: what decides
    /// the unloading order within a tier.
    #[cfg(test)]
    pub fn last_used(&self, key: &PoolKey) -> Option<u64> {
        self.lock().entries.get(key).map(|entry| entry.last_used)
    }

    #[cfg(test)]
    pub fn clear(&self) {
        let mut inner = self.lock();
        inner.entries.clear();
        inner.total = 0;
    }
}

/// What a value costs the pool: the length of its JSON form. Text dominates
/// every cached value, and JSON holds text at about its in-memory size, so
/// this tracks residency closely without walking each type by hand. Counted
/// without building the string.
pub fn serialized_bytes<T: Serialize + ?Sized>(value: &T) -> u64 {
    struct Counter(u64);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len() as u64;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    let _ = serde_json::to_writer(&mut counter, value);
    counter.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(id: &str) -> PoolKey {
        PoolKey::new(PoolKind::ConversationBody, id)
    }

    fn image(id: &str) -> PoolKey {
        PoolKey::new(PoolKind::ImageData, id)
    }

    fn keys(pool: &MemoryPool) -> Vec<String> {
        let inner = pool.lock();
        let mut keys: Vec<String> = inner.entries.keys().map(|key| key.id.clone()).collect();
        keys.sort();
        keys
    }

    #[test]
    fn low_priority_goes_first_even_when_more_recent() {
        let pool = MemoryPool::new(100);
        pool.insert(body("old-body"), Arc::new(()), 40);
        pool.insert(image("new-image"), Arc::new(()), 40);
        let unloaded = pool.insert(body("new-body"), Arc::new(()), 40);
        assert_eq!(unloaded, vec![image("new-image")]);
        assert_eq!(keys(&pool), vec!["new-body", "old-body"]);
    }

    #[test]
    fn within_a_tier_the_least_recently_used_goes() {
        let pool = MemoryPool::new(100);
        pool.insert(body("a"), Arc::new(()), 40);
        pool.insert(body("b"), Arc::new(()), 40);
        assert!(pool.get::<()>(&body("a")).is_some());
        pool.insert(body("c"), Arc::new(()), 40);
        assert_eq!(keys(&pool), vec!["a", "c"]);
    }

    #[test]
    fn a_refresh_keeps_its_place_in_line() {
        let pool = MemoryPool::new(100);
        pool.insert(body("a"), Arc::new(()), 40);
        pool.insert(body("b"), Arc::new(()), 40);
        pool.replace(body("a"), Arc::new(()), 40);
        pool.insert(body("c"), Arc::new(()), 40);
        assert_eq!(keys(&pool), vec!["b", "c"], "replacing `a` was not a use");
    }

    #[test]
    fn a_peek_is_not_a_use() {
        let pool = MemoryPool::new(100);
        pool.insert(body("a"), Arc::new(()), 40);
        pool.insert(body("b"), Arc::new(()), 40);
        assert!(pool.peek::<()>(&body("a")).is_some());
        pool.insert(body("c"), Arc::new(()), 40);
        assert_eq!(keys(&pool), vec!["b", "c"]);
    }

    #[test]
    fn an_oversized_value_is_unloaded_but_its_caller_keeps_it() {
        let pool = MemoryPool::new(100);
        pool.insert(body("small"), Arc::new(()), 10);
        let value = Arc::new(String::from("huge"));
        let unloaded = pool.insert(body("huge"), value.clone(), 500);
        assert_eq!(unloaded, vec![body("small"), body("huge")]);
        assert_eq!(pool.total_bytes(), 0);
        assert_eq!(*value, "huge");
    }

    #[test]
    fn a_value_read_as_the_wrong_type_is_a_miss() {
        let pool = MemoryPool::new(100);
        pool.insert(body("a"), Arc::new(7_u32), 1);
        assert!(pool.get::<String>(&body("a")).is_none());
        assert_eq!(pool.get::<u32>(&body("a")).as_deref(), Some(&7));
    }

    #[test]
    fn replacing_an_entry_recounts_its_bytes() {
        let pool = MemoryPool::new(100);
        pool.insert(body("a"), Arc::new(()), 60);
        pool.insert(body("a"), Arc::new(()), 20);
        assert_eq!(pool.total_bytes(), 20);
        pool.retain(|key| key.id != "a");
        assert_eq!(pool.total_bytes(), 0);
    }
}
