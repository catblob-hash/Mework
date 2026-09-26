//! Conversation titles and shell explanations on top of the scheduler.
//!
//! Each task has a system prompt whose prefix state the scheduler caches
//! (`prefix_cache`); a request then only runs its own text and at most
//! `MAX_NEW_TOKENS` generated tokens.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};

use crate::engine::{prefix_tokens, suffix_tokens, Specials};
use crate::prompts::{clean_reply, shell_request, Task};
use crate::scheduler::{Generate, Limits, Loader, Reply, Scheduler, Status};
use crate::tokenizer::Tokenizer;

/// Generated tokens per reply, titles and explanations alike.
pub const MAX_NEW_TOKENS: usize = 15;
/// Request text beyond this many tokens is cut (its beginning is kept).
pub const MAX_TEXT_TOKENS: usize = 256;
/// Template tokens around the request text (see `engine::suffix_tokens`).
const SUFFIX_OVERHEAD: usize = 16;
const RESULT_CACHE: usize = 256;

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptInfo {
    /// Tokens of the cached prefix (the prompt plus its chat-template framing).
    pub tokens: usize,
    /// Size of the cached state for this backend.
    pub cache_bytes: u64,
}

pub struct ServiceConfig {
    pub model_dir: PathBuf,
    pub cache_dir: PathBuf,
    /// Positions per sequence of the backend the loader makes.
    pub context: usize,
    pub limits: Limits,
}

struct Inner {
    tokenizer: Tokenizer,
    specials: Specials,
    scheduler: Scheduler,
    context: usize,
    results: Mutex<(HashMap<String, String>, VecDeque<String>)>,
}

#[derive(Clone)]
pub struct Service(Arc<Inner>);

impl Service {
    pub fn start(config: ServiceConfig, loader: Loader) -> Result<Self, String> {
        let tokenizer = Tokenizer::from_file(&config.model_dir.join("tokenizer.json"))?;
        let specials = Specials::from_tokenizer(&tokenizer)?;
        let scheduler = Scheduler::start(loader, specials.clone(), config.limits, config.cache_dir);
        Ok(Self(Arc::new(Inner {
            tokenizer,
            specials,
            scheduler,
            context: config.context,
            results: Mutex::new((HashMap::new(), VecDeque::new())),
        })))
    }

    fn prefix_tokens(&self, prompt: &str) -> Vec<u32> {
        prefix_tokens(&self.0.tokenizer, &self.0.specials, prompt)
    }

    /// Longest prefix that still leaves room for a full request.
    pub fn max_prefix_tokens(&self) -> usize {
        self.0.context.saturating_sub(MAX_TEXT_TOKENS + SUFFIX_OVERHEAD + MAX_NEW_TOKENS)
    }

    /// Tokens `prompt` occupies as a prefix; needs no backend.
    pub fn count_prefix_tokens(&self, prompt: &str) -> usize {
        self.prefix_tokens(prompt).len()
    }

    fn checked_prefix(&self, prompt: &str) -> Result<Vec<u32>, String> {
        let tokens = self.prefix_tokens(prompt);
        if tokens.len() > self.max_prefix_tokens() {
            return Err(format!("提示词过长：{} 个 token，最多 {} 个", tokens.len(), self.max_prefix_tokens()));
        }
        Ok(tokens)
    }

    /// Makes sure `prompt`'s prefix state is cached and reports its size.
    pub fn prompt_info(&self, prompt: &str, done: Reply<PromptInfo>) {
        let tokens = match self.checked_prefix(prompt) {
            Ok(tokens) => tokens,
            Err(error) => return done(Err(error)),
        };
        self.0.scheduler.prefix(
            tokens,
            Box::new(move |result| {
                done(result.map(|summary| PromptInfo { tokens: summary.tokens, cache_bytes: summary.bytes }))
            }),
        );
    }

    fn result_key(task: Task, prompt: &str, text: &str) -> String {
        let mut hasher = Sha256::new();
        for part in [task.id(), prompt, text] {
            hasher.update(part.as_bytes());
            hasher.update([0]);
        }
        hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }

    fn remember(&self, key: String, value: String) {
        let mut guard = self.0.results.lock().expect("results");
        let (map, order) = &mut *guard;
        if map.insert(key.clone(), value).is_none() {
            order.push_back(key);
            if order.len() > RESULT_CACHE {
                if let Some(old) = order.pop_front() {
                    map.remove(&old);
                }
            }
        }
    }

    fn run(&self, task: Task, prompt: &str, text: String, done: Reply<Option<String>>) {
        let key = Self::result_key(task, prompt, &text);
        if let Some(hit) = self.0.results.lock().expect("results").0.get(&key) {
            return done(Ok(Some(hit.clone())));
        }
        let prefix = match self.checked_prefix(prompt) {
            Ok(tokens) => Arc::new(tokens),
            Err(error) => return done(Err(error)),
        };
        let tokens = suffix_tokens(&self.0.tokenizer, &self.0.specials, &text, MAX_TEXT_TOKENS);
        let for_stop = self.clone();
        let stop_when = Arc::new(move |output: &[u32]| {
            output.last().and_then(|t| for_stop.0.tokenizer.token_bytes(*t)).is_some_and(|bytes| bytes.contains(&b'\n'))
        });
        let owner = self.clone();
        self.0.scheduler.generate(Generate {
            prefix,
            tokens,
            max_new: MAX_NEW_TOKENS,
            stop_when,
            reply: Box::new(move |result| {
                done(result.map(|output| {
                    let reply = clean_reply(task, &owner.0.tokenizer.decode(&output));
                    if let Some(reply) = &reply {
                        owner.remember(key, reply.clone());
                    }
                    reply
                }))
            }),
        });
    }

    /// A title for a conversation whose chosen message is `message`.
    pub fn title(&self, prompt: &str, message: &str, done: Reply<Option<String>>) {
        self.run(Task::Title, prompt, message.to_string(), done);
    }

    /// A one-line description of `command` run by `shell` (e.g. "bash").
    pub fn explain(&self, prompt: &str, shell: &str, command: &str, done: Reply<Option<String>>) {
        self.run(Task::Shell, prompt, shell_request(shell, command), done);
    }

    pub fn status(&self, done: Reply<Status>) {
        self.0.scheduler.status(done);
    }

    /// Drops the weights once nothing is running (e.g. on memory pressure).
    pub fn unload(&self) {
        self.0.scheduler.unload();
    }

    /// Deletes cached prefix states of prompts no longer in `keep`.
    pub fn prune_cache(&self, keep: &[&str]) {
        self.0.scheduler.prune(keep.iter().map(|prompt| self.prefix_tokens(prompt)).collect());
    }
}
