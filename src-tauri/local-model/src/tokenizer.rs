//! Qwen3.5's tokenizer, rebuilt from the published `tokenizer.json`.
//!
//! Hugging Face's `tokenizers` would read the file directly, but it brings a C
//! regex library (Oniguruma) and a large dependency tree to run one fixed
//! configuration. This module implements exactly that configuration — NFC,
//! Qwen's split regex, byte-level BPE, the added tokens — and refuses files
//! that ask for anything else, so a different tokenizer fails at load instead
//! of encoding wrong. The ids match `tokenizers` case for case; the golden
//! file in `testdata` was produced by it.
//!
//! The split regex has one lookahead, `\s+(?!\S)`, which `regex` cannot
//! express. Without that branch the next one, `\s+`, takes the whole
//! whitespace run; the lookahead only differs when the run is longer than one
//! character and a non-space follows, and then it leaves the last whitespace
//! character to start the next piece (`"a   b"` splits as `"a"`, `"  "`,
//! `" b"`). `split` gives that character back by hand.
//!
//! `tokenizers` still normalizes with Unicode 9.0 tables, so marks added to
//! Unicode since then are left where they are; `nfc` does the same.

use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::hash::{BuildHasherDefault, Hasher};
use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};

use regex::Regex;
use serde::Deserialize;
use serde_json::Value;
use unicode_normalization::{is_nfc_quick, IsNormalized, UnicodeNormalization};

/// The pre-tokenizer regex as Qwen's `tokenizer.json` spells it.
const QWEN_SPLIT: &str = r"(?i:'s|'t|'re|'ve|'m|'ll|'d)|[^\r\n\p{L}\p{N}]?[\p{L}\p{M}]+|\p{N}| ?[^\s\p{L}\p{M}\p{N}]+[\r\n]*|\s*[\r\n]+|\s+(?!\S)|\s+";
/// The branch `split` emulates instead of handing it to `regex`.
const LOOKAHEAD_BRANCH: &str = r"|\s+(?!\S)";

/// Vocabularies with ids beyond this are not tokenizers this module can use.
const MAX_TOKEN_ID: u32 = 1 << 24;
/// Pieces longer than this skip the BPE cache; they rarely repeat.
const CACHE_MAX_PIECE: usize = 256;
/// The cache starts over once it holds this many pieces.
const CACHE_CAPACITY: usize = 16 * 1024;

pub struct Tokenizer {
    split: Regex,
    /// The id of each single byte, the starting symbols of BPE.
    byte_ids: [u32; 256],
    /// `(left << 32) | right` to `(rank, merged id)`.
    merges: HashMap<u64, (u32, u32), FxBuild>,
    /// Every token's decoded bytes, back to back; `spans[id]` locates one.
    bytes: Vec<u8>,
    spans: Vec<(u32, u32)>,
    /// Added tokens matched in the raw text, and those matched after NFC.
    raw_added: AddedMatcher,
    normalized_added: AddedMatcher,
    added_ids: HashMap<String, u32, FxBuild>,
    cache: Mutex<PieceCache>,
}

/// Encoded pieces, keyed by their normalized bytes.
type PieceCache = HashMap<Box<[u8]>, Box<[u32]>, FxBuild>;

#[derive(Deserialize)]
struct RawTokenizer {
    #[serde(default)]
    added_tokens: Vec<RawAddedToken>,
    normalizer: Option<Value>,
    pre_tokenizer: Option<Value>,
    decoder: Option<Value>,
    model: RawModel,
}

#[derive(Deserialize)]
struct RawAddedToken {
    id: u32,
    content: String,
    #[serde(default)]
    single_word: bool,
    #[serde(default)]
    lstrip: bool,
    #[serde(default)]
    rstrip: bool,
    normalized: Option<bool>,
    #[serde(default)]
    special: bool,
}

#[derive(Deserialize)]
struct RawModel {
    #[serde(rename = "type")]
    kind: Option<String>,
    vocab: HashMap<String, u32, FxBuild>,
    merges: Vec<RawMerge>,
    dropout: Option<f64>,
    unk_token: Option<String>,
    continuing_subword_prefix: Option<String>,
    end_of_word_suffix: Option<String>,
    #[serde(default)]
    byte_fallback: bool,
    #[serde(default)]
    ignore_merges: bool,
}

/// Merges are `"a b"` in older files and `["a", "b"]` in newer ones.
#[derive(Deserialize)]
#[serde(untagged)]
enum RawMerge {
    Joined(String),
    Pair(String, String),
}

impl Tokenizer {
    pub fn from_file(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|error| format!("无法读取分词器文件 {}: {error}", path.display()))?;
        Self::from_json(&bytes)
    }

    pub fn from_json(bytes: &[u8]) -> Result<Self, String> {
        let raw: RawTokenizer =
            serde_json::from_slice(bytes).map_err(|error| format!("分词器文件无法解析: {error}"))?;
        check_pipeline(&raw)?;
        let vocab = &raw.model.vocab;

        let mut byte_ids = [0u32; 256];
        for (byte, id) in byte_ids.iter_mut().enumerate() {
            let symbol = BYTE_CHARS[byte].to_string();
            *id = *vocab.get(&symbol).ok_or_else(|| format!("词表缺少字节 {byte:#04x} 对应的词元"))?;
        }

        let mut merges = HashMap::with_capacity_and_hasher(raw.model.merges.len(), FxBuild::default());
        let mut joined = String::new();
        for (rank, merge) in raw.model.merges.iter().enumerate() {
            let (left, right) = match merge {
                RawMerge::Joined(line) => {
                    let mut parts = line.split(' ');
                    match (parts.next(), parts.next(), parts.next()) {
                        (Some(left), Some(right), None) => (left, right),
                        _ => return Err(format!("第 {} 条合并规则格式错误", rank + 1)),
                    }
                }
                RawMerge::Pair(left, right) => (left.as_str(), right.as_str()),
            };
            joined.clear();
            joined.push_str(left);
            joined.push_str(right);
            let lookup =
                |token: &str| vocab.get(token).copied().ok_or_else(|| format!("合并规则里的词元 {token} 不在词表中"));
            let key = pair_key(lookup(left)?, lookup(right)?);
            merges.insert(key, (rank as u32, lookup(&joined)?));
        }

        let max_vocab_id = vocab.values().copied().max().unwrap_or(0);
        let max_added_id = raw.added_tokens.iter().map(|token| token.id).max().unwrap_or(0);
        let max_id = max_vocab_id.max(max_added_id);
        if max_id >= MAX_TOKEN_ID {
            return Err(format!("词元编号 {max_id} 过大"));
        }
        let mut table = TokenTable { bytes: Vec::new(), spans: vec![(u32::MAX, 0); max_id as usize + 1] };
        let mut decoded = Vec::new();
        for (token, &id) in vocab {
            // Like the ByteLevel decoder: a token spelled outside the byte
            // alphabet stands for its own UTF-8.
            decoded.clear();
            match token.chars().map(char_byte).collect::<Option<Vec<u8>>>() {
                Some(mapped) => decoded.extend_from_slice(&mapped),
                None => decoded.extend_from_slice(token.as_bytes()),
            }
            table.set(id, &decoded);
        }

        // `tokenizers` ignores the ids written next to added tokens and
        // numbers them itself: the vocab id when the content is a vocab token,
        // otherwise the next free id after the vocab. A file that disagrees
        // would encode differently there, so it is rejected.
        let vocab_len = vocab.len() as u32;
        let mut max_assigned: Option<u32> = None;
        let mut added_ids: HashMap<String, u32, FxBuild> = HashMap::default();
        let mut raw_added = Vec::new();
        let mut normalized_added = Vec::new();
        for token in &raw.added_tokens {
            if token.content.is_empty() {
                continue;
            }
            if added_ids.contains_key(&token.content) {
                return Err(format!("附加词元 {} 重复", token.content));
            }
            let expected = match vocab.get(&token.content) {
                Some(&id) => id,
                None => match max_assigned {
                    Some(max) if max >= vocab_len => max + 1,
                    _ => vocab_len,
                },
            };
            if token.id != expected {
                return Err(format!("附加词元 {} 的编号 {} 应为 {expected}", token.content, token.id));
            }
            max_assigned = Some(max_assigned.map_or(expected, |max| max.max(expected)));
            added_ids.insert(token.content.clone(), token.id);
            table.set(token.id, token.content.as_bytes());
            let normalized = token.normalized.unwrap_or(!token.special);
            let added = AddedToken {
                id: token.id,
                // Normalized tokens are looked for in normalized text, so their
                // content is normalized the same way.
                pattern: if normalized { nfc(&token.content).into_owned() } else { token.content.clone() },
                single_word: token.single_word,
                lstrip: token.lstrip,
                rstrip: token.rstrip,
            };
            if normalized {
                normalized_added.push(added);
            } else {
                raw_added.push(added);
            }
        }

        Ok(Self {
            split: split_regex(),
            byte_ids,
            merges,
            bytes: table.bytes,
            spans: table.spans,
            raw_added: AddedMatcher::new(raw_added),
            normalized_added: AddedMatcher::new(normalized_added),
            added_ids,
            cache: Mutex::new(HashMap::default()),
        })
    }

    /// Plain text: added and special tokens written in the text are encoded
    /// as the characters they are made of, so user text can never produce a
    /// control token such as `<|im_end|>`.
    pub fn encode_ordinary(&self, text: &str) -> Vec<u32> {
        let mut ids = Vec::with_capacity(text.len() / 3 + 1);
        self.encode_normalized(&nfc(text), &mut ids);
        ids
    }

    /// Like `tokenizers`' `encode(text, add_special_tokens=False)`: added
    /// tokens written in the text become their single ids, with each token's
    /// `lstrip`, `rstrip`, `single_word` and `normalized` flags applied.
    pub fn encode_with_added_tokens(&self, text: &str) -> Vec<u32> {
        let mut ids = Vec::with_capacity(text.len() / 3 + 1);
        for (start, end, added) in self.raw_added.split(text) {
            if let Some(id) = added {
                ids.push(id);
                continue;
            }
            let normalized = nfc(&text[start..end]);
            for (start, end, added) in self.normalized_added.split(&normalized) {
                match added {
                    Some(id) => ids.push(id),
                    None => self.encode_normalized(&normalized[start..end], &mut ids),
                }
            }
        }
        ids
    }

    /// The id of an added token such as `<|im_end|>`. Vocabulary tokens are
    /// not looked up by spelling; encode their text instead.
    pub fn token_id(&self, token: &str) -> Option<u32> {
        self.added_ids.get(token).copied()
    }

    /// Every added token's id, ascending.
    pub fn added_token_ids(&self) -> Vec<u32> {
        let mut ids: Vec<u32> = self.added_ids.values().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// Decodes like the ByteLevel decoder with `errors="replace"`: all bytes
    /// are joined first, so a character split across tokens survives, and
    /// what is still invalid UTF-8 becomes U+FFFD. Added tokens decode to
    /// their content; unknown ids are skipped.
    pub fn decode(&self, ids: &[u32]) -> String {
        let mut bytes = Vec::with_capacity(ids.len() * 4);
        for &id in ids {
            if let Some(token) = self.token_bytes(id) {
                bytes.extend_from_slice(token);
            }
        }
        match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(error) => String::from_utf8_lossy(error.as_bytes()).into_owned(),
        }
    }

    /// The raw bytes of one token, which may be part of a UTF-8 character;
    /// streaming callers buffer them until they form whole characters.
    pub fn token_bytes(&self, id: u32) -> Option<&[u8]> {
        let &(start, len) = self.spans.get(id as usize)?;
        if start == u32::MAX {
            return None;
        }
        Some(&self.bytes[start as usize..(start + len) as usize])
    }

    /// One more than the highest token id, added tokens included.
    pub fn vocab_size(&self) -> usize {
        self.spans.len()
    }

    fn encode_normalized(&self, text: &str, ids: &mut Vec<u32>) {
        split(&self.split, text, |piece| self.encode_piece(piece.as_bytes(), ids));
    }

    fn encode_piece(&self, piece: &[u8], ids: &mut Vec<u32>) {
        if let [byte] = piece {
            ids.push(self.byte_ids[*byte as usize]);
            return;
        }
        let cacheable = piece.len() <= CACHE_MAX_PIECE;
        if cacheable {
            if let Some(hit) = self.lock_cache().get(piece) {
                ids.extend_from_slice(hit);
                return;
            }
        }
        let start = ids.len();
        self.bpe(piece, ids);
        if cacheable {
            let mut cache = self.lock_cache();
            if cache.len() >= CACHE_CAPACITY {
                cache.clear();
            }
            cache.insert(piece.into(), ids[start..].into());
        }
    }

    fn lock_cache(&self) -> MutexGuard<'_, PieceCache> {
        // A panic elsewhere cannot leave a half-inserted entry behind.
        self.cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// `tokenizers`' `Word::merge_all` without dropout: repeatedly merge the
    /// lowest-ranked adjacent pair, leftmost first among equal ranks.
    fn bpe(&self, piece: &[u8], ids: &mut Vec<u32>) {
        struct Symbol {
            id: u32,
            prev: usize,
            next: usize,
            alive: bool,
        }
        let count = piece.len();
        let mut symbols: Vec<Symbol> = piece
            .iter()
            .enumerate()
            .map(|(index, &byte)| Symbol {
                id: self.byte_ids[byte as usize],
                prev: index.wrapping_sub(1),
                next: index + 1,
                alive: true,
            })
            .collect();
        let mut queue = BinaryHeap::with_capacity(count);
        for pos in 0..count.saturating_sub(1) {
            if let Some(&(rank, id)) = self.merges.get(&pair_key(symbols[pos].id, symbols[pos + 1].id)) {
                queue.push(Reverse((rank, pos, id)));
            }
        }
        while let Some(Reverse((_, pos, new_id))) = queue.pop() {
            let next = symbols[pos].next;
            if !symbols[pos].alive || next >= count {
                continue;
            }
            // Skip entries whose pair has changed since they were queued.
            match self.merges.get(&pair_key(symbols[pos].id, symbols[next].id)) {
                Some(&(_, id)) if id == new_id => {}
                _ => continue,
            }
            let after = symbols[next].next;
            symbols[next].alive = false;
            symbols[pos].id = new_id;
            symbols[pos].next = after;
            if after < count {
                symbols[after].prev = pos;
            }
            let prev = symbols[pos].prev;
            if prev < count {
                if let Some(&(rank, id)) = self.merges.get(&pair_key(symbols[prev].id, new_id)) {
                    queue.push(Reverse((rank, prev, id)));
                }
            }
            if after < count {
                if let Some(&(rank, id)) = self.merges.get(&pair_key(new_id, symbols[after].id)) {
                    queue.push(Reverse((rank, pos, id)));
                }
            }
        }
        ids.extend(symbols.iter().filter(|symbol| symbol.alive).map(|symbol| symbol.id));
    }
}

struct TokenTable {
    bytes: Vec<u8>,
    spans: Vec<(u32, u32)>,
}

impl TokenTable {
    fn set(&mut self, id: u32, bytes: &[u8]) {
        self.spans[id as usize] = (self.bytes.len() as u32, bytes.len() as u32);
        self.bytes.extend_from_slice(bytes);
    }
}

fn pair_key(left: u32, right: u32) -> u64 {
    (u64::from(left) << 32) | u64::from(right)
}

/// NFC as `tokenizers` computes it, with Unicode 9.0 tables. Characters
/// added since that NFC acts on are inert there, and nothing composes or
/// reorders across an inert starter, so the text is normalized in the stretches
/// between them. A text that is already NFC here is NFC there too.
fn nfc(text: &str) -> Cow<'_, str> {
    if is_nfc_quick(text.chars()) == IsNormalized::Yes {
        return Cow::Borrowed(text);
    }
    let mut normalized = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((at, c)) = rest.char_indices().find(|&(_, c)| unknown_to_unicode9_nfc(c)) {
        normalized.extend(rest[..at].nfc());
        normalized.push(c);
        rest = &rest[at + c.len_utf8()..];
    }
    normalized.extend(rest.nfc());
    Cow::Owned(normalized)
}

fn unknown_to_unicode9_nfc(c: char) -> bool {
    let code = c as u32;
    code >= UNICODE9_UNKNOWN[0].0
        && UNICODE9_UNKNOWN
            .binary_search_by(|&(first, last)| {
                if last < code {
                    std::cmp::Ordering::Less
                } else if first > code {
                    std::cmp::Ordering::Greater
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .is_ok()
}

/// Code points that `unicode-normalization` gives a combining class, a
/// decomposition or a part in one, but that were unassigned in Unicode 9.0,
/// the tables of the `unicode-normalization-alignments` crate `tokenizers`
/// normalizes with. Derived by diffing both crates over every code point.
#[rustfmt::skip]
const UNICODE9_UNKNOWN: &[(u32, u32)] = &[
    (0x07fd, 0x07fd), (0x0897, 0x089f), (0x08ca, 0x08d3), (0x09fe, 0x09fe), (0x0c3c, 0x0c3c), (0x0d3b, 0x0d3c),
    (0x0eba, 0x0eba), (0x1715, 0x1715), (0x1abf, 0x1add), (0x1ae0, 0x1aeb), (0x1df6, 0x1dfa), (0xa82c, 0xa82c),
    (0x105c9, 0x105c9), (0x105d2, 0x105d2), (0x105da, 0x105da), (0x105e4, 0x105e4), (0x10d24, 0x10d27),
    (0x10d69, 0x10d6d), (0x10eab, 0x10eac), (0x10efa, 0x10efb), (0x10efd, 0x10eff), (0x10f46, 0x10f50),
    (0x10f82, 0x10f85), (0x11070, 0x11070), (0x1133b, 0x1133b), (0x11382, 0x11385), (0x1138b, 0x1138b),
    (0x1138e, 0x1138e), (0x11390, 0x11391), (0x113b8, 0x113b8), (0x113bb, 0x113bb), (0x113c2, 0x113c2),
    (0x113c5, 0x113c5), (0x113c7, 0x113c9), (0x113ce, 0x113d0), (0x1145e, 0x1145e), (0x11839, 0x1183a),
    (0x11930, 0x11930), (0x11935, 0x11935), (0x11938, 0x11938), (0x1193d, 0x1193e), (0x11943, 0x11943),
    (0x119e0, 0x119e0), (0x11a34, 0x11a34), (0x11a47, 0x11a47), (0x11a99, 0x11a99), (0x11d42, 0x11d42),
    (0x11d44, 0x11d45), (0x11d97, 0x11d97), (0x11f41, 0x11f42), (0x1611e, 0x16129), (0x1612f, 0x1612f),
    (0x16d63, 0x16d63), (0x16d67, 0x16d6a), (0x16ff0, 0x16ff1), (0x1e08f, 0x1e08f), (0x1e130, 0x1e136),
    (0x1e2ae, 0x1e2ae), (0x1e2ec, 0x1e2ef), (0x1e4ec, 0x1e4ef), (0x1e5ee, 0x1e5ef), (0x1e6e3, 0x1e6e3),
    (0x1e6e6, 0x1e6e6), (0x1e6ee, 0x1e6ef), (0x1e6f5, 0x1e6f5),
];

/// Only the configuration Qwen3.5 ships is accepted; anything else would need
/// behaviour this module does not have.
fn check_pipeline(raw: &RawTokenizer) -> Result<(), String> {
    let kind = |value: &Value| value.get("type").and_then(Value::as_str).map(str::to_owned);
    let flag = |value: &Value, key: &str| value.get(key).and_then(Value::as_bool).unwrap_or(false);

    if raw.normalizer.as_ref().and_then(kind).as_deref() != Some("NFC") {
        return Err("分词器配置不受支持: 规范化方式应为 NFC".into());
    }

    let steps = raw
        .pre_tokenizer
        .as_ref()
        .filter(|value| kind(value).as_deref() == Some("Sequence"))
        .and_then(|value| value.get("pretokenizers"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let [split, byte_level] = steps else {
        return Err("分词器配置不受支持: 预切分应为 Split 加 ByteLevel".into());
    };
    let pattern = split.get("pattern").and_then(|pattern| pattern.get("Regex")).and_then(Value::as_str);
    if kind(split).as_deref() != Some("Split")
        || pattern != Some(QWEN_SPLIT)
        || split.get("behavior").and_then(Value::as_str) != Some("Isolated")
        || flag(split, "invert")
    {
        return Err("分词器配置不受支持: 切分正则与 Qwen3.5 不同".into());
    }
    if kind(byte_level).as_deref() != Some("ByteLevel")
        || flag(byte_level, "add_prefix_space")
        || flag(byte_level, "use_regex")
    {
        return Err("分词器配置不受支持: ByteLevel 预切分参数不同".into());
    }

    if raw.decoder.as_ref().and_then(kind).as_deref() != Some("ByteLevel") {
        return Err("分词器配置不受支持: 解码器应为 ByteLevel".into());
    }

    let model = &raw.model;
    let empty = |value: &Option<String>| value.as_deref().unwrap_or_default().is_empty();
    if model.kind.as_deref().is_some_and(|kind| kind != "BPE")
        || model.dropout.is_some()
        || model.unk_token.is_some()
        || !empty(&model.continuing_subword_prefix)
        || !empty(&model.end_of_word_suffix)
        || model.byte_fallback
        || model.ignore_merges
    {
        return Err("分词器配置不受支持: 模型应为不带 dropout、未知词元和字节回退的 BPE".into());
    }
    Ok(())
}

fn split_regex() -> Regex {
    // Anchored and run on the rest of the text: every character starts some
    // branch, so each match begins where the previous one ended, and no branch
    // looks behind its start.
    let pattern = format!("^(?:{})", QWEN_SPLIT.replacen(LOOKAHEAD_BRANCH, "", 1));
    Regex::new(&pattern).expect("the split pattern is a valid regex")
}

/// Splits normalized text into pieces the way the `Split` pre-tokenizer does
/// with the full pattern, lookahead included.
fn split<'t>(regex: &Regex, text: &'t str, mut piece: impl FnMut(&'t str)) {
    let mut start = 0;
    while start < text.len() {
        let rest = &text[start..];
        // Every character starts some branch and no branch matches empty, so
        // the fallback is unreachable; it only rules out looping forever.
        let Some(found) = regex.find(rest).filter(|found| !found.is_empty()) else {
            piece(rest);
            return;
        };
        let mut end = found.end();
        // Only the `\s+` branch ends in whitespace other than a line break;
        // `\s+(?!\S)` would have stopped one character short of a non-space.
        if end < rest.len() {
            if let Some(last) = found.as_str().chars().next_back() {
                if last.is_whitespace() && last != '\r' && last != '\n' && end > last.len_utf8() {
                    end -= last.len_utf8();
                }
            }
        }
        piece(&rest[..end]);
        start += end;
    }
}

struct AddedToken {
    id: u32,
    pattern: String,
    single_word: bool,
    lstrip: bool,
    rstrip: bool,
}

struct AddedMatcher {
    tokens: Vec<AddedToken>,
    first_bytes: [bool; 256],
}

impl AddedMatcher {
    fn new(tokens: Vec<AddedToken>) -> Self {
        let mut first_bytes = [false; 256];
        for token in &tokens {
            first_bytes[token.pattern.as_bytes()[0] as usize] = true;
        }
        Self { tokens, first_bytes }
    }

    /// `tokenizers`' `AddedVocabulary::find_matches`: leftmost-longest
    /// matches, then the flags widen or drop them. Returns byte ranges of
    /// `text`, each either plain text or one added token.
    fn split(&self, text: &str) -> Vec<(usize, usize, Option<u32>)> {
        let mut splits = Vec::new();
        let mut taken = 0;
        let mut from = 0;
        while let Some((raw_start, token)) = self.next_match(text, from) {
            let (mut start, mut stop) = (raw_start, raw_start + token.pattern.len());
            from = stop;
            if token.single_word {
                let word_before = text[..start].chars().next_back().is_some_and(is_word_char);
                let word_after = text[stop..].chars().next().is_some_and(is_word_char);
                if word_before || word_after {
                    continue;
                }
            }
            if token.lstrip {
                let spaces: usize =
                    text[..start].chars().rev().take_while(|c| c.is_whitespace()).map(char::len_utf8).sum();
                start = (start - spaces).max(taken);
            }
            if token.rstrip {
                stop += text[stop..].chars().take_while(|c| c.is_whitespace()).map(char::len_utf8).sum::<usize>();
            }
            if taken < start {
                splits.push((taken, start, None));
            }
            splits.push((start, stop, Some(token.id)));
            taken = stop;
        }
        if taken < text.len() {
            splits.push((taken, text.len(), None));
        }
        splits
    }

    fn next_match(&self, text: &str, from: usize) -> Option<(usize, &AddedToken)> {
        let bytes = text.as_bytes();
        // A pattern's first byte never continues a UTF-8 character, so every
        // hit is on a character boundary.
        (from..bytes.len()).filter(|&at| self.first_bytes[bytes[at] as usize]).find_map(|at| {
            self.tokens
                .iter()
                .filter(|token| bytes[at..].starts_with(token.pattern.as_bytes()))
                .max_by_key(|token| token.pattern.len())
                .map(|token| (at, token))
        })
    }
}

/// `\w` as the `regex` crate defines it, which is what `single_word` checks.
fn is_word_char(c: char) -> bool {
    static WORD: OnceLock<Regex> = OnceLock::new();
    WORD.get_or_init(|| Regex::new(r"^\w$").expect("valid regex")).is_match(c.encode_utf8(&mut [0; 4]))
}

/// GPT-2's byte alphabet: printable Latin-1 bytes stand for themselves and the
/// other 68 take U+0100 onward in byte order, so every token is printable.
const fn is_printable_byte(byte: u8) -> bool {
    matches!(byte, b'!'..=b'~' | 0xa1..=0xac | 0xae..=0xff)
}

const BYTE_CHARS: [char; 256] = byte_chars();
const NON_PRINTABLE_BYTES: [u8; 68] = non_printable_bytes();

const fn byte_chars() -> [char; 256] {
    let mut chars = ['\0'; 256];
    let mut next = 0x100;
    let mut byte = 0;
    while byte < 256 {
        let code = if is_printable_byte(byte as u8) {
            byte as u32
        } else {
            next += 1;
            next - 1
        };
        chars[byte] = match char::from_u32(code) {
            Some(c) => c,
            None => panic!("byte alphabet stays below U+0144"),
        };
        byte += 1;
    }
    chars
}

const fn non_printable_bytes() -> [u8; 68] {
    let mut bytes = [0; 68];
    let mut count = 0;
    let mut byte = 0;
    while byte < 256 {
        if !is_printable_byte(byte as u8) {
            bytes[count] = byte as u8;
            count += 1;
        }
        byte += 1;
    }
    bytes
}

fn char_byte(c: char) -> Option<u8> {
    match c as u32 {
        code @ 0..=0xff if is_printable_byte(code as u8) => Some(code as u8),
        code @ 0x100..=0x143 => Some(NON_PRINTABLE_BYTES[(code - 0x100) as usize]),
        _ => None,
    }
}

/// rustc's Fx hash with the rotating finish of rustc-hash 2, so the table
/// index sees every bit of a pair key rather than only the right id's low bits.
#[derive(Default, Clone, Copy)]
struct FxHasher {
    hash: u64,
}

type FxBuild = BuildHasherDefault<FxHasher>;

const FX_SEED: u64 = 0xf135_7aea_2e62_a9c5;

impl FxHasher {
    fn add(&mut self, word: u64) {
        self.hash = self.hash.wrapping_add(word).wrapping_mul(FX_SEED);
    }
}

impl Hasher for FxHasher {
    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for chunk in &mut chunks {
            self.add(u64::from_le_bytes(chunk.try_into().expect("8 bytes")));
        }
        let mut tail = [0; 8];
        let rest = chunks.remainder();
        tail[..rest.len()].copy_from_slice(rest);
        self.add(u64::from_le_bytes(tail) ^ ((rest.len() as u64) << 59));
    }

    fn write_u8(&mut self, value: u8) {
        self.add(u64::from(value));
    }

    fn write_u32(&mut self, value: u32) {
        self.add(u64::from(value));
    }

    fn write_u64(&mut self, value: u64) {
        self.add(value);
    }

    fn write_usize(&mut self, value: usize) {
        self.add(value as u64);
    }

    fn finish(&self) -> u64 {
        self.hash.rotate_left(26)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pieces(text: &str) -> Vec<&str> {
        let regex = split_regex();
        let mut out = Vec::new();
        split(&regex, text, |piece| out.push(piece));
        out
    }

    #[test]
    fn byte_alphabet_is_a_bijection() {
        let mut seen = std::collections::HashSet::new();
        for byte in 0..=255u8 {
            let c = BYTE_CHARS[byte as usize];
            assert!(seen.insert(c));
            assert_eq!(char_byte(c), Some(byte));
        }
        assert_eq!(BYTE_CHARS[b' ' as usize], 'Ġ');
        assert_eq!(BYTE_CHARS[b'\n' as usize], 'Ċ');
        assert_eq!(BYTE_CHARS[b'a' as usize], 'a');
        assert_eq!(BYTE_CHARS[0xad], '\u{143}');
        assert_eq!(char_byte('\u{144}'), None);
        assert_eq!(char_byte(' '), None);
    }

    #[test]
    fn splits_words_numbers_and_punctuation() {
        assert_eq!(pieces("Hello world"), ["Hello", " world"]);
        assert_eq!(pieces("I'M they'Re don't"), ["I", "'M", " they", "'Re", " don", "'t"]);
        assert_eq!(pieces("12345"), ["1", "2", "3", "4", "5"]);
        assert_eq!(pieces("$VAR && echo"), ["$VAR", " &&", " echo"]);
        assert_eq!(pieces("foo!!\n\nbar"), ["foo", "!!\n\n", "bar"]);
        assert_eq!(pieces("\u{3000}中文，好"), ["\u{3000}中文", "，好"]);
        assert_eq!(pieces("e\u{301}t\u{e9}"), ["e\u{301}t\u{e9}"]);
        assert_eq!(pieces("👍🏽 ok"), ["👍🏽", " ok"]);
        assert!(pieces("").is_empty());
    }

    #[test]
    fn whitespace_runs_follow_the_lookahead() {
        assert_eq!(pieces("a  b"), ["a", " ", " b"]);
        assert_eq!(pieces("a    b"), ["a", "   ", " b"]);
        assert_eq!(pieces("a   "), ["a", "   "]);
        assert_eq!(pieces("   "), ["   "]);
        assert_eq!(pieces("a \n b"), ["a", " \n", " b"]);
        assert_eq!(pieces("x\t\ty"), ["x", "\t", "\ty"]);
        assert_eq!(pieces("\r\n\r\n  x"), ["\r\n\r\n", " ", " x"]);
        assert_eq!(pieces("x \r y"), ["x", " \r", " y"]);
        assert_eq!(pieces("hello  \nworld"), ["hello", "  \n", "world"]);
        assert_eq!(pieces("a\u{a0}\u{a0}b"), ["a", "\u{a0}", "\u{a0}b"]);
        assert_eq!(pieces("a \u{3000}!"), ["a", " ", "\u{3000}", "!"]);
        assert_eq!(pieces("x  1"), ["x", " ", " ", "1"]);
        assert_eq!(pieces("  (x"), [" ", " (", "x"]);
    }

    /// A small tokenizer in the same format, with added tokens exercising
    /// every flag. Expected ids come from `tokenizers` on the same JSON.
    fn tiny_tokenizer() -> Tokenizer {
        let mut vocab = serde_json::Map::new();
        for byte in 0..=255u8 {
            vocab.insert(BYTE_CHARS[byte as usize].to_string(), byte.into());
        }
        let merges = ["h e", "l l", "he ll", "hell o", "Ġ w", "o r", "Ġw or", "l d", "Ġwor ld", "Ġ Ġ"];
        for (index, merge) in merges.iter().enumerate() {
            vocab.insert(merge.replace(' ', ""), (256 + index).into());
        }
        let added = |id: u32, content: &str, flags: &[&str], special: bool| {
            let flag = |name: &str| flags.contains(&name);
            serde_json::json!({
                "id": id, "content": content, "special": special,
                "single_word": flag("single_word"), "lstrip": flag("lstrip"),
                "rstrip": flag("rstrip"), "normalized": flag("normalized"),
            })
        };
        let json = serde_json::json!({
            "version": "1.0",
            "added_tokens": [
                added(266, "<s>", &[], true),
                added(267, "<l>", &["lstrip"], false),
                added(268, "<r>", &["rstrip"], false),
                added(269, "<w>", &["single_word"], false),
                added(270, "\u{1e31}", &["normalized"], false),
                added(271, "<ss>", &[], false),
            ],
            "normalizer": {"type": "NFC"},
            "pre_tokenizer": {"type": "Sequence", "pretokenizers": [
                {"type": "Split", "pattern": {"Regex": QWEN_SPLIT}, "behavior": "Isolated", "invert": false},
                {"type": "ByteLevel", "add_prefix_space": false, "trim_offsets": false, "use_regex": false},
            ]},
            "post_processor": null,
            "decoder": {"type": "ByteLevel", "add_prefix_space": false, "trim_offsets": false, "use_regex": false},
            "model": {
                "type": "BPE", "dropout": null, "unk_token": null, "continuing_subword_prefix": "",
                "end_of_word_suffix": "", "fuse_unk": false, "byte_fallback": false, "ignore_merges": false,
                "vocab": vocab, "merges": merges,
            },
        });
        Tokenizer::from_json(json.to_string().as_bytes()).unwrap()
    }

    #[test]
    fn tiny_tokenizer_encodes_like_tokenizers() {
        let tokenizer = tiny_tokenizer();
        let cases: &[(&str, &[u32], &[u32])] = &[
            ("hello world", &[259, 264], &[259, 264]),
            ("hello  world", &[259, 32, 264], &[259, 32, 264]),
            ("a<s>b", &[97, 60, 115, 62, 98], &[97, 266, 98]),
            ("a  <l>  b", &[97, 32, 32, 60, 108, 62, 32, 32, 98], &[97, 267, 32, 32, 98]),
            ("a  <r>  b", &[97, 32, 32, 60, 114, 62, 32, 32, 98], &[97, 265, 268, 98]),
            ("x  <l>", &[120, 32, 32, 60, 108, 62], &[120, 267]),
            ("<r>   <r>", &[60, 114, 62, 265, 32, 60, 114, 62], &[268, 268]),
            ("\u{3000}<l>", &[227, 128, 128, 60, 108, 62], &[267]),
            ("<l><l>", &[60, 108, 62, 60, 108, 62], &[267, 267]),
            (" <r> <l> ", &[32, 60, 114, 62, 32, 60, 108, 62, 32], &[32, 268, 267, 32]),
            ("a<w>b", &[97, 60, 119, 62, 98], &[97, 60, 119, 62, 98]),
            ("a <w> b", &[97, 32, 60, 119, 62, 32, 98], &[97, 32, 269, 32, 98]),
            ("<w> <w>", &[60, 119, 62, 32, 60, 119, 62], &[269, 32, 269]),
            ("中<w>文", &[228, 184, 173, 60, 119, 62, 230, 150, 135], &[228, 184, 173, 60, 119, 62, 230, 150, 135]),
            ("<w>é", &[60, 119, 62, 195, 169], &[60, 119, 62, 195, 169]),
            ("-<w>_", &[45, 60, 119, 62, 95], &[45, 60, 119, 62, 95]),
            ("k\u{301}x", &[225, 184, 177, 120], &[270, 120]),
            ("<ss> <s>", &[60, 115, 115, 62, 32, 60, 115, 62], &[271, 32, 266]),
            ("<s<s>>", &[60, 115, 60, 115, 62, 62], &[60, 115, 266, 62]),
            ("hello   world  ", &[259, 265, 264, 265], &[259, 265, 264, 265]),
        ];
        for &(text, ordinary, added) in cases {
            assert_eq!(tokenizer.encode_ordinary(text), ordinary, "{text:?}");
            assert_eq!(tokenizer.encode_with_added_tokens(text), added, "{text:?}");
        }
        assert_eq!(tokenizer.token_id("<w>"), Some(269));
        assert_eq!(tokenizer.token_id("hello"), None);
        assert_eq!(tokenizer.vocab_size(), 272);
    }

    #[test]
    fn tiny_tokenizer_decodes_bytes() {
        let tokenizer = tiny_tokenizer();
        assert_eq!(tokenizer.decode(&[259, 264, 266, 32]), "hello world<s> ");
        assert_eq!(tokenizer.decode(&[228, 189, 160]), "你");
        assert_eq!(tokenizer.decode(&[228, 189]), "\u{fffd}");
        assert_eq!(tokenizer.decode(&[228, 266, 189, 160]), "\u{fffd}<s>\u{fffd}\u{fffd}");
        assert_eq!(tokenizer.decode(&[270, 9999, 97]), "\u{1e31}a");
        assert_eq!(tokenizer.token_bytes(32), Some(&b" "[..]));
        assert_eq!(tokenizer.token_bytes(264), Some(&b" world"[..]));
        assert_eq!(tokenizer.token_bytes(272), None);
    }

    #[test]
    fn rejects_other_pipelines() {
        let base = serde_json::json!({
            "normalizer": {"type": "NFKC"},
            "model": {"type": "BPE", "vocab": {}, "merges": []},
        });
        let error = Tokenizer::from_json(base.to_string().as_bytes()).err().unwrap();
        assert!(error.contains("NFC"), "{error}");
        assert!(Tokenizer::from_json(b"{").is_err());
    }

    /// The real tokenizer against ids recorded from `tokenizers`. Needs the
    /// model directory, which is too large to keep in the repository.
    #[test]
    fn qwen35_matches_golden_ids() {
        let Some(dir) = std::env::var_os("MEWORK_LOCAL_MODEL_DIR") else {
            eprintln!("跳过: 未设置 MEWORK_LOCAL_MODEL_DIR");
            return;
        };
        let tokenizer = Tokenizer::from_file(&Path::new(&dir).join("tokenizer.json")).unwrap();
        let golden: Value = serde_json::from_str(include_str!("../testdata/tokenizer-golden.json")).unwrap();
        let ids = |value: &Value| -> Vec<u32> {
            value.as_array().unwrap().iter().map(|id| id.as_u64().unwrap() as u32).collect()
        };
        let cases = golden["cases"].as_array().unwrap();
        for case in cases {
            let text = case["text"].as_str().unwrap();
            let ordinary = ids(&case["ordinary"]);
            let added = case.get("added").map_or_else(|| ordinary.clone(), ids);
            assert_eq!(tokenizer.encode_ordinary(text), ordinary, "{text:?}");
            assert_eq!(tokenizer.encode_with_added_tokens(text), added, "{text:?}");
            let normalized = nfc(text).into_owned();
            assert_eq!(tokenizer.decode(&ordinary), normalized, "{text:?}");
            let decoded = case.get("added_decoded").and_then(Value::as_str).unwrap_or(&normalized);
            assert_eq!(tokenizer.decode(&added), decoded, "{text:?}");
        }
        for case in golden["decode"].as_array().unwrap() {
            assert_eq!(tokenizer.decode(&ids(&case["ids"])), case["text"].as_str().unwrap());
        }
        assert_eq!(tokenizer.token_id("<|im_end|>"), Some(248046));
        assert_eq!(tokenizer.token_id("<think>"), Some(248068));
        assert_eq!(tokenizer.vocab_size(), 248070);
        eprintln!("{} 个用例一致", cases.len());
    }
}
