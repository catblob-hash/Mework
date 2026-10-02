//! Conversation titles, shell explanations and error explanations on top of
//! the scheduler.
//!
//! Each task has a system prompt whose prefix state the scheduler caches
//! (`prefix_cache`); a request then only runs its own input and at most
//! `MAX_NEW_TOKENS` generated tokens.
//!
//! A title reads the whole user message the way the main model gets it: the
//! attached files as `<attached_file>` elements, then the text, then the
//! images. The message is cut to `MAX_INPUT_TOKENS` by priority rather than
//! from the end, since the ask is what names a conversation: the text keeps
//! its beginning, the images share what it leaves (each resized to at most
//! `MAX_IMAGE_TOKENS`), and the files get the rest, in order, each keeping
//! its beginning.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};

use crate::engine::{prefix_tokens, reply_start, suffix_tokens, Segment, Specials};
use crate::prompts::{clean_reply, error_request, shell_request, Task};
use crate::scheduler::{Generate, Limits, Loader, Observer, Reply, Scheduler, Status};
pub use crate::scheduler::Lane;
use crate::tokenizer::Tokenizer;
use crate::vision::{fit, Picture, VisionConfig};

/// Generated tokens per reply, titles and explanations alike.
pub const MAX_NEW_TOKENS: usize = 15;
/// A request's own input beyond this many tokens is cut (see the module
/// doc). Qwen publishes no length past which the model degrades; 4,096 keeps
/// a full screenshot plus the message and files around it.
pub const MAX_INPUT_TOKENS: usize = 4096;
/// The most tokens one image becomes (a million pixels: a screenshot's text
/// stays legible).
pub const MAX_IMAGE_TOKENS: usize = 1024;
/// The fewest: the official image processor's minimum, 256×256 pixels.
pub const MIN_IMAGE_TOKENS: usize = 64;
/// Positions per sequence the backends are built or configured with: a
/// prompt of up to ~990 tokens, a full input, and the reply.
pub const CONTEXT: usize = 5120;
/// `<|vision_start|>` and `<|vision_end|>` around each image.
const IMAGE_MARKERS: usize = 2;
/// Template tokens after the input (see `engine::reply_start`).
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

/// What is known about a prompt without the model: its tokens, and its
/// cached state's size if one is on disk.
#[derive(Clone, Debug)]
pub struct CachedPromptInfo {
    pub tokens: usize,
    pub cache_bytes: Option<u64>,
}

pub struct ServiceConfig {
    pub model_dir: PathBuf,
    pub cache_dir: PathBuf,
    /// Positions per sequence of the backend the loader makes.
    pub context: usize,
    pub limits: Limits,
    /// The build can read images (it has the vision tower's files).
    pub vision: bool,
}

/// An image attached to a message, decoded only at the size it is read at.
pub trait PictureSource: Send {
    /// The original's width and height in pixels.
    fn size(&self) -> (usize, usize);
    /// Its pixels, RGB and row-major, resized to `width`×`height`.
    fn render(&self, width: usize, height: usize) -> Result<Vec<u8>, String>;
}

/// A user message as the main model reads it.
#[derive(Default)]
pub struct Message {
    pub text: String,
    /// Each attached file as its `<attached_file>` element, in order.
    pub files: Vec<String>,
    pub images: Vec<Box<dyn PictureSource>>,
}

struct Inner {
    tokenizer: Tokenizer,
    specials: Specials,
    vision: Option<VisionConfig>,
    scheduler: Scheduler,
    context: usize,
    cache_dir: PathBuf,
    results: Mutex<(HashMap<String, String>, VecDeque<String>)>,
}

#[derive(Clone)]
pub struct Service(Arc<Inner>);

/// What of a message fits, by `plan`.
#[derive(Debug, PartialEq, Eq)]
struct Plan {
    text: usize,
    /// Per image taken (in order): its size and tokens.
    images: Vec<(usize, (usize, usize), usize)>,
    /// Tokens kept of each file, in order (zero: left out).
    files: Vec<usize>,
}

/// Splits `MAX_INPUT_TOKENS` between a message's text (`text` tokens), its
/// images (`images`, original sizes) and its files (`files` tokens each),
/// with `separator` tokens between the text and each file.
fn plan(text: usize, images: &[(usize, usize)], files: &[usize], separator: usize, factor: usize) -> Plan {
    let mut budget = MAX_INPUT_TOKENS;
    let text = text.min(budget);
    budget -= text;
    let mut taken = Vec::new();
    let count = images.len().min(budget / (MIN_IMAGE_TOKENS + IMAGE_MARKERS));
    let mut left = count;
    for (index, (width, height)) in images.iter().enumerate().take(count) {
        let share = (budget / left).saturating_sub(IMAGE_MARKERS).min(MAX_IMAGE_TOKENS);
        left -= 1;
        let mut size = fit(*width, *height, factor, MIN_IMAGE_TOKENS, share);
        let tokens = |size: (usize, usize)| (size.0 / factor) * (size.1 / factor);
        // Rounding up to the minimum can overshoot a share; then fit it under.
        if size.is_some_and(|size| tokens(size) > share) {
            size = fit(*width, *height, factor, 1, share);
        }
        let Some(size) = size.filter(|size| tokens(*size) > 0 && tokens(*size) <= share) else { continue };
        budget -= tokens(size) + IMAGE_MARKERS;
        taken.push((index, size, tokens(size)));
    }
    let mut kept = Vec::new();
    for file in files {
        let room = budget.saturating_sub(separator);
        let keep = (*file).min(room);
        if keep > 0 {
            budget -= keep + separator;
        }
        kept.push(keep);
    }
    Plan { text, images: taken, files: kept }
}

impl Service {
    pub fn start(config: ServiceConfig, loader: Loader) -> Result<Self, String> {
        let tokenizer = Tokenizer::from_file(&config.model_dir.join("tokenizer.json"))?;
        let specials = Specials::from_tokenizer(&tokenizer)?;
        let vision =
            if config.vision { Some(VisionConfig::load(&config.model_dir.join("config.json"))?) } else { None };
        let scheduler = Scheduler::start(loader, specials.clone(), config.limits, config.cache_dir.clone());
        Ok(Self(Arc::new(Inner {
            tokenizer,
            specials,
            vision,
            scheduler,
            context: config.context,
            cache_dir: config.cache_dir,
            results: Mutex::new((HashMap::new(), VecDeque::new())),
        })))
    }

    fn prefix_tokens(&self, prompt: &str) -> Vec<u32> {
        prefix_tokens(&self.0.tokenizer, &self.0.specials, prompt)
    }

    /// Longest prefix that still leaves room for a full request.
    pub fn max_prefix_tokens(&self) -> usize {
        self.0.context.saturating_sub(MAX_INPUT_TOKENS + SUFFIX_OVERHEAD + MAX_NEW_TOKENS)
    }

    fn checked_prefix(&self, prompt: &str) -> Result<Vec<u32>, String> {
        let tokens = self.prefix_tokens(prompt);
        if tokens.len() > self.max_prefix_tokens() {
            return Err(format!("提示词过长：{} 个 token，最多 {} 个", tokens.len(), self.max_prefix_tokens()));
        }
        Ok(tokens)
    }

    /// `prompt`'s tokens and, if its prefix state is already on disk, that
    /// state's size; never loads the model.
    pub fn cached_prompt_info(&self, prompt: &str) -> Result<CachedPromptInfo, String> {
        let tokens = self.checked_prefix(prompt)?;
        let cache_bytes = crate::prefix_cache::peek(&self.0.cache_dir, &tokens);
        Ok(CachedPromptInfo { tokens: tokens.len(), cache_bytes })
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

    fn result_key(task: Task, prompt: &str, input: &[Segment]) -> String {
        let mut hasher = Sha256::new();
        for part in [task.id(), prompt] {
            hasher.update(part.as_bytes());
            hasher.update([0]);
        }
        for segment in input {
            match segment {
                Segment::Tokens(tokens) => {
                    hasher.update([1]);
                    hasher.update((tokens.len() as u64).to_le_bytes());
                    tokens.iter().for_each(|token| hasher.update(token.to_le_bytes()));
                }
                Segment::Picture(picture) => {
                    hasher.update([2]);
                    hasher.update((picture.width as u64).to_le_bytes());
                    hasher.update((picture.height as u64).to_le_bytes());
                    hasher.update(&picture.rgb);
                }
            }
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

    fn factor(&self) -> usize {
        self.0.vision.as_ref().map(VisionConfig::factor).unwrap_or(1)
    }

    fn run(&self, task: Task, prompt: &str, input: Vec<Segment>, lane: Lane, done: Reply<Option<String>>) {
        let key = Self::result_key(task, prompt, &input);
        if let Some(hit) = self.0.results.lock().expect("results").0.get(&key) {
            return done(Ok(Some(hit.clone())));
        }
        let prefix = match self.checked_prefix(prompt) {
            Ok(tokens) => Arc::new(tokens),
            Err(error) => return done(Err(error)),
        };
        let factor = self.factor();
        let positions = input.iter().map(|segment| segment.len(factor)).sum();
        let for_stop = self.clone();
        let stop_when = Arc::new(move |output: &[u32]| {
            output.last().and_then(|t| for_stop.0.tokenizer.token_bytes(*t)).is_some_and(|bytes| bytes.contains(&b'\n'))
        });
        let owner = self.clone();
        self.0.scheduler.generate(Generate {
            prefix,
            input: Arc::new(input),
            positions,
            key: key.clone(),
            max_new: MAX_NEW_TOKENS,
            stop_when,
            lane,
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

    /// The model's input for `message` (see the module doc); `None` when
    /// nothing of it is left. Decodes and resizes the images, so it can take
    /// a moment.
    fn message_input(&self, message: &Message) -> Option<Vec<Segment>> {
        let tokenizer = &self.0.tokenizer;
        let separator = tokenizer.encode_ordinary("\n\n");
        let mut text = tokenizer.encode_ordinary(message.text.trim());
        let files: Vec<Vec<u32>> = message.files.iter().map(|file| tokenizer.encode_ordinary(file.trim())).collect();
        let sizes: Vec<(usize, usize)> = match &self.0.vision {
            Some(_) => message.images.iter().map(|image| image.size()).collect(),
            None => Vec::new(),
        };
        let file_lens: Vec<usize> = files.iter().map(Vec::len).collect();
        let plan = plan(text.len(), &sizes, &file_lens, separator.len(), self.factor());
        text.truncate(plan.text);

        let mut parts: Vec<Vec<u32>> =
            files.into_iter().zip(&plan.files).filter(|(_, keep)| **keep > 0).map(|(mut file, keep)| {
                file.truncate(*keep);
                file
            }).collect();
        if !text.is_empty() {
            parts.push(text);
        }
        let mut current = parts.join(&separator[..]);
        let mut input = Vec::new();
        let vision = self.0.vision.as_ref();
        for (index, (width, height), _) in &plan.images {
            let Some(vision) = vision else { break };
            let picture = match message.images[*index].render(*width, *height).and_then(|rgb| Picture::new(*width, *height, rgb)) {
                Ok(picture) => picture,
                Err(error) => {
                    eprintln!("本地模型无法读取图片：{error}");
                    continue;
                }
            };
            current.push(vision.vision_start_token_id);
            input.push(Segment::Tokens(std::mem::take(&mut current)));
            input.push(Segment::Picture(Arc::new(picture)));
            current.push(vision.vision_end_token_id);
        }
        if input.is_empty() && current.is_empty() {
            return None;
        }
        current.extend(reply_start(tokenizer, &self.0.specials));
        input.push(Segment::Tokens(current));
        Some(input)
    }

    /// A title for a conversation whose chosen message is `message` (or for
    /// a task given to a subagent). Reads the message's images first (see
    /// `message_input`), so call it off any thread that must stay responsive.
    pub fn title(&self, prompt: &str, message: &Message, lane: Lane, done: Reply<Option<String>>) {
        match self.message_input(message) {
            Some(input) => self.run(Task::Title, prompt, input, lane, done),
            None => done(Ok(None)),
        }
    }

    /// A one-line description of `command` run by `shell` (e.g. "bash").
    pub fn explain(&self, prompt: &str, shell: &str, command: &str, lane: Lane, done: Reply<Option<String>>) {
        let tokens =
            suffix_tokens(&self.0.tokenizer, &self.0.specials, &shell_request(shell, command), MAX_INPUT_TOKENS);
        self.run(Task::Shell, prompt, vec![Segment::Tokens(tokens)], lane, done);
    }

    /// Why a call of `tool` (a tool's name, or a shell's for a command)
    /// failed, from the `error` it returned. Keeps the error's beginning:
    /// a failed command's output leads with its exit code and stderr.
    pub fn explain_error(&self, prompt: &str, tool: &str, error: &str, lane: Lane, done: Reply<Option<String>>) {
        let tokens =
            suffix_tokens(&self.0.tokenizer, &self.0.specials, &error_request(tool, error), MAX_INPUT_TOKENS);
        self.run(Task::Error, prompt, vec![Segment::Tokens(tokens)], lane, done);
    }

    /// The scheduler's status as last published; never waits for a load.
    pub fn status(&self) -> Status {
        self.0.scheduler.status()
    }

    /// Calls `observer` (on the model's thread) when the model starts or
    /// stops loading, loads, unloads or fails.
    pub fn observe(&self, observer: Observer) {
        self.0.scheduler.observe(observer);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_comes_first_then_images_then_files() {
        // Short text, one screenshot, one small file: all of it.
        let p = plan(20, &[(1920, 1080)], &[100], 1, 32);
        assert_eq!(p.text, 20);
        assert_eq!(p.images, vec![(0, (1344, 768), 1008)]);
        assert_eq!(p.files, vec![100]);

        // A long file gets what the text and the image leave.
        let p = plan(20, &[(1920, 1080)], &[10_000, 50], 1, 32);
        assert_eq!(p.files, vec![MAX_INPUT_TOKENS - 20 - 1010 - 1, 0]);

        // Text over the cap: cut, and nothing else fits.
        let p = plan(9000, &[(640, 480)], &[10], 1, 32);
        assert_eq!((p.text, p.images.len(), p.files.clone()), (MAX_INPUT_TOKENS, 0, vec![0]));
    }

    #[test]
    fn many_images_share_the_budget() {
        // Five screenshots cannot all have 1024 tokens; they share the budget,
        // the last taking what rounding left.
        let p = plan(10, &[(1920, 1080); 5], &[], 1, 32);
        assert_eq!(p.images.len(), 5);
        let used: usize = p.images.iter().map(|(_, _, tokens)| tokens + IMAGE_MARKERS).sum();
        assert!(used <= MAX_INPUT_TOKENS - 10 && used > MAX_INPUT_TOKENS - 200, "{used}");
        assert!(p.images[..4].iter().all(|(_, _, tokens)| *tokens == 798), "{:?}", p.images);
        assert!(p.images[4].2 <= MAX_IMAGE_TOKENS);

        // Seventy images: as many as fit at the minimum, in order.
        let p = plan(0, &[(256, 256); 70], &[], 1, 32);
        assert_eq!(p.images.len(), MAX_INPUT_TOKENS / (MIN_IMAGE_TOKENS + IMAGE_MARKERS));
        assert_eq!(p.images[0], (0, (256, 256), 64));

        // A tiny, wide image rounds up to the minimum without overshooting a share.
        let p = plan(MAX_INPUT_TOKENS - 66, &[(100, 50)], &[], 1, 32);
        assert!(p.images.iter().all(|(_, _, tokens)| *tokens <= 64), "{:?}", p.images);

        // Absurd aspect ratios are skipped.
        assert!(plan(0, &[(1, 500)], &[], 1, 32).images.is_empty());
    }
}

/// Titles from the published builds, end to end: the service's budget, the
/// picture through the vision tower, the backend at `CONTEXT`.
/// `MEWORK_LOCAL_MODEL_RELEASE` names `examples/build_release`'s output; the
/// Neural Engine build is compiled into `MEWORK_LOCAL_MODEL_WORK` (minutes,
/// the first time).
#[cfg(all(test, target_os = "macos"))]
mod release {
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use super::*;
    use crate::coreml::backend::{compiled_weights, AneBackend};
    use crate::coreml::graph::Shapes;
    use crate::coreml::package::{EmbeddingLayout, PackagePlan, EMBEDDING_FILE, GRAPH_VERSION};
    use crate::engine::Backend;
    use crate::prompts::default_prompt;
    use crate::qwen35::Config;
    use crate::vision::{VisionConfig, VisionTower, VISION_FILE, VISION_VERSION};

    struct Golden;

    impl PictureSource for Golden {
        fn size(&self) -> (usize, usize) {
            (320, 224)
        }

        fn render(&self, width: usize, height: usize) -> Result<Vec<u8>, String> {
            assert_eq!((width, height), (320, 224), "already the model's size");
            Ok(crate::vision::tests::golden_picture().rgb)
        }
    }

    fn release() -> Option<PathBuf> {
        let dir = std::env::var_os("MEWORK_LOCAL_MODEL_RELEASE").map(PathBuf::from);
        if dir.is_none() {
            eprintln!("MEWORK_LOCAL_MODEL_RELEASE not set; skipping");
        }
        dir
    }

    fn title(service: &Service, message: &Message) -> Result<Option<String>, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        let prompt = default_prompt(Task::Title, "zh-CN");
        let started = Instant::now();
        service.title(prompt, message, Lane::Foreground, Box::new(move |result| tx.send(result).unwrap()));
        let result = rx.recv_timeout(Duration::from_secs(900)).unwrap();
        eprintln!("{:?} in {:?}", result, started.elapsed());
        result
    }

    /// Messages that exercise the budget: an image with a question, an image
    /// alone, a file, and text far over the input limit.
    fn exercise(service: &Service) {
        let image = || Box::new(Golden) as Box<dyn PictureSource>;
        let asked = Message { text: "这张图里是什么？".into(), files: vec![], images: vec![image()] };
        assert!(title(service, &asked).unwrap().is_some());
        let alone = Message { text: String::new(), files: vec![], images: vec![image()] };
        assert!(title(service, &alone).unwrap().is_some());
        let file = "<attached_file name=\"build.log\">\nerror[E0425]: cannot find value `config` in this scope\n</attached_file>";
        let with_file = Message { text: "帮我看看".into(), files: vec![file.into()], images: vec![] };
        assert!(title(service, &with_file).unwrap().is_some());
        let long = Message { text: "把这段日志整理成表格。".repeat(2000), files: vec![file.into()], images: vec![image()] };
        assert!(title(service, &long).unwrap().is_some());
        // A full screenshot's worth: 1,000 image tokens.
        let screen = Message { text: String::new(), files: vec![], images: vec![Box::new(Screen)] };
        assert!(title(service, &screen).unwrap().is_some());
    }

    struct Screen;

    impl PictureSource for Screen {
        fn size(&self) -> (usize, usize) {
            (1280, 800)
        }

        fn render(&self, width: usize, height: usize) -> Result<Vec<u8>, String> {
            Ok((0..width * height * 3).map(|i| if (i / 3 / width) % 40 < 20 { 240 } else { (i % 251) as u8 }).collect())
        }
    }

    fn model_dir(release: &Path, variant: &str, work: &Path) -> PathBuf {
        let dir = work.join(format!("release-{}", variant.replace('/', "-")));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["config.json", "tokenizer.json"] {
            std::fs::copy(release.join(variant).join(name), dir.join(name)).unwrap();
        }
        dir
    }

    #[test]
    fn titles_on_the_neural_engine() {
        let Some(release) = release() else { return };
        let work = std::env::var_os("MEWORK_LOCAL_MODEL_WORK").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let variant = format!("ane/{GRAPH_VERSION}");
        let dir = model_dir(&release, &variant, &work);
        std::fs::copy(release.join(&variant).join(EMBEDDING_FILE), dir.join(EMBEDDING_FILE)).unwrap();
        let compiled = dir.join("model.mlmodelc");
        if !compiled.exists() {
            let started = Instant::now();
            crate::coreml::runtime::compile(&release.join(&variant).join("model.mlpackage"), &compiled).unwrap();
            eprintln!("compiled in {:?}", started.elapsed());
        }
        let vision = release.join(format!("vision/{VISION_VERSION}")).join(VISION_FILE);
        let config_path = dir.join("config.json");
        let loader_dir = dir.clone();
        let loader: Loader = Box::new(move || {
            let config = Config::load(&config_path)?;
            let plan = PackagePlan::standard(&config, Shapes { slots: 4, chunk: 16, context: CONTEXT });
            let layout = EmbeddingLayout::load(&loader_dir.join(EMBEDDING_FILE))?;
            let compiled = loader_dir.join("model.mlmodelc");
            let started = Instant::now();
            let backend = AneBackend::load(&compiled, &compiled_weights(&compiled), &layout, config, &plan, &mut |_, _| {})?;
            eprintln!("loaded in {:?}", started.elapsed());
            let tower = VisionTower::open(VisionConfig::load(&config_path)?, &vision)?;
            Ok(Box::new(backend.with_vision(tower)) as Box<dyn Backend>)
        });
        let config = ServiceConfig {
            model_dir: dir.clone(),
            cache_dir: dir.join("prompt-cache"),
            context: CONTEXT,
            limits: Limits::default(),
            vision: true,
        };
        exercise(&Service::start(config, loader).unwrap());
    }

    #[cfg(all(feature = "mlx", target_arch = "aarch64"))]
    #[test]
    fn titles_on_mlx() {
        let Some(release) = release() else { return };
        let work = std::env::var_os("MEWORK_LOCAL_MODEL_WORK").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let variant = format!("mlx/{}", crate::mlx::weights::FORMAT);
        let dir = model_dir(&release, &variant, &work);
        for name in [crate::mlx::weights::INDEX_FILE, crate::mlx::weights::WEIGHTS_FILE] {
            let _ = std::fs::remove_file(dir.join(name));
            std::os::unix::fs::symlink(release.join(&variant).join(name), dir.join(name)).unwrap();
        }
        let metallib = release.join(format!("mlx/runtime/{}", crate::mlx::MLX_VERSION)).join(crate::mlx::METALLIB_FILE);
        let _ = std::fs::remove_file(dir.join(crate::mlx::METALLIB_FILE));
        std::os::unix::fs::symlink(metallib, dir.join(crate::mlx::METALLIB_FILE)).unwrap();
        let vision = release.join(format!("vision/{VISION_VERSION}")).join(VISION_FILE);
        let loader_dir = dir.clone();
        let loader: Loader = Box::new(move || {
            let tower = VisionTower::open(VisionConfig::load(&loader_dir.join("config.json"))?, &vision)?;
            let backend = crate::mlx::MlxBackend::load(&loader_dir, 4, CONTEXT)?;
            Ok(Box::new(backend.with_vision(tower)) as Box<dyn Backend>)
        });
        let config = ServiceConfig {
            model_dir: dir.clone(),
            cache_dir: dir.join("prompt-cache"),
            context: CONTEXT,
            limits: Limits::default(),
            vision: true,
        };
        exercise(&Service::start(config, loader).unwrap());
    }
}
