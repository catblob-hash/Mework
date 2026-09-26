//! What the scheduler needs from an inference backend, and the pieces every
//! backend shares (sampling, stop tokens, prompt layout).
//!
//! A backend owns a fixed number of sequence slots. The scheduler admits a
//! request into a free slot (restoring a cached prefix state and running the
//! request's own tokens), then advances all live slots one token per step, so
//! concurrent requests share every pass over the weights.

use crate::tokenizer::Tokenizer;

/// Logits for one slot's next token.
pub type Logits = Vec<f32>;

/// The saved state after running a prefix (the fixed system prompt), in the
/// backend's own format. Restoring it costs a copy instead of a prefill.
pub struct PrefixState {
    /// Tokens the prefix covers; the request's first token goes at this position.
    pub tokens: usize,
    /// Which backend and model build wrote it; a mismatch means recompute.
    pub format: String,
    pub bytes: PrefixBytes,
}

/// A prefix state's bytes: freshly computed, or mapped from its cache file.
/// Mapped pages are clean and file-backed, so under memory pressure the
/// system drops them and reads them back from disk when a request needs them.
pub enum PrefixBytes {
    Owned(Vec<u8>),
    Mapped { map: memmap2::Mmap, offset: usize },
}

impl std::ops::Deref for PrefixBytes {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            Self::Owned(bytes) => bytes,
            Self::Mapped { map, offset } => &map[*offset..],
        }
    }
}

impl From<Vec<u8>> for PrefixBytes {
    fn from(bytes: Vec<u8>) -> Self {
        Self::Owned(bytes)
    }
}

/// Limits a backend was built or configured with.
#[derive(Clone, Copy, Debug)]
pub struct Capacity {
    /// Sequences that can run at once.
    pub slots: usize,
    /// Positions (prefix + request + generated) per sequence.
    pub context: usize,
}

pub trait Backend: Send {
    /// A short human-readable description of where the model runs, e.g.
    /// "Apple Neural Engine" or "Vulkan · NVIDIA GeForce RTX 4070".
    fn device(&self) -> String;

    fn capacity(&self) -> Capacity;

    /// Identifies the state format; `PrefixState::format` must equal it.
    fn state_format(&self) -> String;

    /// Runs `tokens` from an empty sequence and returns the resulting state.
    /// Uses no slot and leaves live slots untouched.
    fn prefix_state(&mut self, tokens: &[u32]) -> Result<PrefixState, String>;

    /// Starts `slot` from `prefix` followed by `tokens` (at least one) and
    /// returns the logits after the last of them.
    fn admit(&mut self, slot: usize, prefix: &PrefixState, tokens: &[u32]) -> Result<Logits, String>;

    /// Appends one token to each listed slot and returns their next logits,
    /// in the same order.
    fn step(&mut self, batch: &[(usize, u32)]) -> Result<Vec<Logits>, String>;

    /// Forgets `slot`'s sequence. Its memory is reused by the next admit.
    fn release(&mut self, slot: usize);

    /// No request is live: drop per-sequence buffers (KV caches, recurrent
    /// states, activation buffers). The weights stay loaded.
    fn trim(&mut self);
}

/// Token ids the prompt layout and the sampler need.
#[derive(Clone, Debug)]
pub struct Specials {
    pub im_start: u32,
    pub im_end: u32,
    pub end_of_text: u32,
    pub think_open: u32,
    pub think_close: u32,
    /// Every added token: never sampled except the stop tokens.
    pub banned: Vec<u32>,
    pub newline: Vec<u32>,
}

impl Specials {
    pub fn from_tokenizer(tokenizer: &Tokenizer) -> Result<Self, String> {
        let id = |name: &str| tokenizer.token_id(name).ok_or_else(|| format!("分词器缺少 {name}"));
        let im_end = id("<|im_end|>")?;
        let end_of_text = id("<|endoftext|>")?;
        let banned = tokenizer.added_token_ids().into_iter().filter(|t| *t != im_end && *t != end_of_text).collect();
        Ok(Self {
            im_start: id("<|im_start|>")?,
            im_end,
            end_of_text,
            think_open: id("<think>")?,
            think_close: id("</think>")?,
            banned,
            newline: tokenizer.encode_ordinary("\n"),
        })
    }

    pub fn is_stop(&self, token: u32) -> bool {
        token == self.im_end || token == self.end_of_text
    }
}

/// The chat-template layout (thinking disabled) split at the request text:
/// the prefix is cached once per system prompt, the suffix is per request.
pub fn prefix_tokens(tokenizer: &Tokenizer, specials: &Specials, system_prompt: &str) -> Vec<u32> {
    let mut tokens = vec![specials.im_start];
    tokens.extend(tokenizer.encode_ordinary("system\n"));
    tokens.extend(tokenizer.encode_ordinary(system_prompt.trim()));
    tokens.push(specials.im_end);
    tokens.extend(tokenizer.encode_ordinary("\n"));
    tokens.push(specials.im_start);
    tokens.extend(tokenizer.encode_ordinary("user\n"));
    tokens
}

/// The request's own tokens after the cached prefix, ending where the
/// assistant's reply begins. `text` is capped at `max_text_tokens`, keeping
/// its beginning.
pub fn suffix_tokens(tokenizer: &Tokenizer, specials: &Specials, text: &str, max_text_tokens: usize) -> Vec<u32> {
    let mut body = tokenizer.encode_ordinary(text.trim());
    body.truncate(max_text_tokens);
    let mut tokens = body;
    tokens.push(specials.im_end);
    tokens.extend(tokenizer.encode_ordinary("\n"));
    tokens.push(specials.im_start);
    tokens.extend(tokenizer.encode_ordinary("assistant\n"));
    tokens.push(specials.think_open);
    tokens.extend(tokenizer.encode_ordinary("\n\n"));
    tokens.push(specials.think_close);
    tokens.extend(tokenizer.encode_ordinary("\n\n"));
    tokens
}

/// Greedy choice over `logits`, never picking a banned token.
pub fn greedy(logits: &[f32], specials: &Specials) -> u32 {
    let mut best = 0usize;
    let mut best_value = f32::NEG_INFINITY;
    for (i, value) in logits.iter().enumerate() {
        if *value > best_value && !value.is_nan() {
            best = i;
            best_value = *value;
        }
    }
    if specials.banned.binary_search(&(best as u32)).is_err() {
        return best as u32;
    }
    // Rare: the top token is an added token. Rescan without them.
    let mut best = specials.end_of_text as usize;
    let mut best_value = f32::NEG_INFINITY;
    for (i, value) in logits.iter().enumerate() {
        if *value > best_value && specials.banned.binary_search(&(i as u32)).is_err() {
            best = i;
            best_value = *value;
        }
    }
    best as u32
}
