//! Prefix states (a system prompt's KV cache and recurrent state), kept on
//! disk and mapped from there.
//!
//! A state is only valid for the backend build that wrote it, so the key is
//! the backend's `state_format` plus the prompt's tokens, and only the
//! scheduler thread, which owns the backend, asks for one. Serving states from
//! the mapped file means their pages are clean and file-backed: under memory
//! pressure the system drops them and reads them back when a request needs
//! them, instead of the app holding them resident.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use memmap2::Mmap;
use sha2::{Digest, Sha256};

use crate::engine::{Backend, PrefixBytes, PrefixState};

const MAGIC: &[u8; 8] = b"MWPFX01\n";
const EXTENSION: &str = "prefix";

pub struct PrefixCache {
    dir: PathBuf,
    memory: HashMap<String, Arc<PrefixState>>,
}

pub fn key(format: &str, tokens: &[u32]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format.as_bytes());
    hasher.update([0]);
    for token in tokens {
        hasher.update(token.to_le_bytes());
    }
    hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn open(path: &Path, format: &str) -> Option<PrefixState> {
    let file = File::open(path).ok()?;
    // SAFETY: cache files are written to a temporary name and renamed into
    // place; nothing writes them afterwards.
    let map = unsafe { Mmap::map(&file) }.ok()?;
    if map.len() < 16 || &map[..8] != MAGIC {
        return None;
    }
    let header_len = u32::from_le_bytes(map[8..12].try_into().ok()?) as usize;
    let tokens = u32::from_le_bytes(map[12..16].try_into().ok()?) as usize;
    if map.get(16..16 + header_len)? != format.as_bytes() {
        return None;
    }
    Some(PrefixState { tokens, format: format.to_string(), bytes: PrefixBytes::Mapped { map, offset: 16 + header_len } })
}

fn store(path: &Path, state: &PrefixState) -> std::io::Result<()> {
    let temp = path.with_extension("writing");
    let mut file = File::create(&temp)?;
    file.write_all(MAGIC)?;
    file.write_all(&(state.format.len() as u32).to_le_bytes())?;
    file.write_all(&(state.tokens as u32).to_le_bytes())?;
    file.write_all(state.format.as_bytes())?;
    file.write_all(&state.bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temp, path)
}

impl PrefixCache {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir, memory: HashMap::new() }
    }

    fn path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.{EXTENSION}"))
    }

    /// The state after `tokens` for `backend`: mapped from its file, or
    /// computed by the backend and written first.
    pub fn get(&mut self, backend: &mut dyn Backend, tokens: &[u32]) -> Result<Arc<PrefixState>, String> {
        let format = backend.state_format();
        let key = key(&format, tokens);
        if let Some(state) = self.memory.get(&key) {
            return Ok(state.clone());
        }
        let path = self.path(&key);
        let state = match open(&path, &format) {
            Some(state) => state,
            None => {
                let computed = backend.prefix_state(tokens)?;
                let _ = fs::create_dir_all(&self.dir);
                match store(&path, &computed).ok().and_then(|_| open(&path, &format)) {
                    Some(mapped) => mapped,
                    // Unwritable cache directory: keep the computed copy in memory.
                    None => computed,
                }
            }
        };
        let state = Arc::new(state);
        self.memory.insert(key, state.clone());
        Ok(state)
    }

    /// Drops the mappings (the files stay), e.g. when the model unloads.
    pub fn forget(&mut self) {
        self.memory.clear();
    }

    /// Deletes cached states of prompts not in `keep`, for `format`, and
    /// every state written by another backend build.
    pub fn prune(&mut self, format: &str, keep: &[Vec<u32>]) {
        let keys: Vec<String> = keep.iter().map(|tokens| key(format, tokens)).collect();
        self.memory.retain(|key, _| keys.contains(key));
        let Ok(entries) = fs::read_dir(&self.dir) else { return };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let stem = name.split('.').next().unwrap_or_default();
            if !keys.iter().any(|key| key == stem) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Capacity, Logits};

    struct Counting {
        computed: usize,
        format: String,
    }

    impl Backend for Counting {
        fn device(&self) -> String {
            "test".into()
        }
        fn capacity(&self) -> Capacity {
            Capacity { slots: 1, context: 64 }
        }
        fn state_format(&self) -> String {
            self.format.clone()
        }
        fn prefix_state(&mut self, tokens: &[u32]) -> Result<PrefixState, String> {
            self.computed += 1;
            let bytes: Vec<u8> = tokens.iter().flat_map(|t| t.to_le_bytes()).collect();
            Ok(PrefixState { tokens: tokens.len(), format: self.format.clone(), bytes: bytes.into() })
        }
        fn admit(&mut self, _: usize, _: &PrefixState, _: &[u32]) -> Result<Logits, String> {
            unreachable!()
        }
        fn step(&mut self, _: &[(usize, u32)]) -> Result<Vec<Logits>, String> {
            unreachable!()
        }
        fn release(&mut self, _: usize) {}
        fn trim(&mut self) {}
    }

    #[test]
    fn computes_once_then_maps_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut backend = Counting { computed: 0, format: "a".into() };
        let mut cache = PrefixCache::new(dir.path().to_path_buf());
        let first = cache.get(&mut backend, &[1, 2, 3]).unwrap();
        assert!(matches!(first.bytes, PrefixBytes::Mapped { .. }));
        assert_eq!(&first.bytes[..4], &1u32.to_le_bytes());
        cache.forget();
        let again = cache.get(&mut backend, &[1, 2, 3]).unwrap();
        assert_eq!(backend.computed, 1, "second read comes from disk");
        assert_eq!(again.tokens, 3);

        // Another backend build never reads it.
        let mut other = Counting { computed: 0, format: "b".into() };
        cache.get(&mut other, &[1, 2, 3]).unwrap();
        assert_eq!(other.computed, 1);

        cache.prune("b", &[vec![1, 2, 3]]);
        let files = fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(files, 1, "only b's state is left");
    }
}
