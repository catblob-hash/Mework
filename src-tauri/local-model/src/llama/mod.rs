//! The llama.cpp backend, for Windows and Linux (Apple platforms use Core ML).
//!
//! It runs the GGUF that `gguf::convert_qwen35_to_gguf` writes, on llama.cpp's
//! own release build (`runtime`), loaded at run time (`ffi`). Each slot is a
//! llama.cpp sequence (seq id = slot) in one context, plus one scratch
//! sequence that only `prefix_state` uses, so computing a prefix never
//! disturbs live requests. Every `step` advances all listed slots in a single
//! `llama_decode`.
//!
//! Placement, decided once at load:
//! - a discrete GPU whose free memory holds the weights and at least one
//!   sequence gets every layer (weights in VRAM);
//! - a GPU that can map host memory (Apple's unified memory under Metal) gets
//!   every layer too, with the weights left in the mapped file's pages;
//! - otherwise the CPU, on its performance cores, with the weights mapped from
//!   the file. An integrated GPU that cannot map the file (Vulkan and CUDA
//!   iGPUs today) would need its own resident copy of the weights, which the
//!   OS cannot evict under memory pressure; it is used only when
//!   `LlamaOptions::igpu_resident_weights` asks for that.
//!
//! Weights are never locked in memory. The slot count comes from a memory
//! budget (see `plan`), never more than `LlamaOptions::max_slots`.
//!
//! Images go through llama.cpp's `mtmd` with the projector file
//! (`LlamaOptions::projector`, loaded at the first image, on the same device
//! as the model): it encodes the picture and decodes its rows at 3-D rotary
//! positions, after which the text continues at the position it reports, not
//! at the count of positions taken.

mod ffi;
pub mod runtime;
mod system;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::engine::{Backend, Capacity, Logits, PrefixState, Segment};

/// Tokens per `llama_decode`, and per compute pass: bounds the activation
/// buffers. Longer inputs are run in chunks of this size.
const BATCH: usize = 256;
/// llama.cpp sizes each sequence's KV cache in multiples of this.
const CONTEXT_ALIGN: usize = 256;
/// Share of available memory the per-sequence buffers may take on the CPU or
/// with unified memory, where the weights are pages the OS can reclaim.
const HOST_BUDGET_SHARE: u64 = 4;
/// Most CPU threads; a 0.8B model's decode is memory-bound well before this.
const MAX_THREADS: usize = 8;
/// llama.cpp's `LLAMA_MAX_SEQ` (256), less the scratch sequence.
const MAX_SLOTS: usize = 255;
const MIB: u64 = 1 << 20;
/// The vision tower's compute buffers for the largest image the service
/// sends (1,024 tokens, 4,096 patches, flash attention).
const PROJECTOR_COMPUTE: u64 = 256 * MIB;

#[derive(Clone, Debug)]
pub struct LlamaOptions {
    /// Positions per sequence (prefix + request + reply), rounded up to a
    /// multiple of 256.
    pub context: usize,
    /// Most sequences to run at once; fewer when memory is short.
    pub max_slots: usize,
    /// Run on the CPU even when a usable GPU is present.
    pub cpu_only: bool,
    /// CPU threads; 0 chooses the performance cores (at most 8).
    pub threads: usize,
    /// The unpacked release build (`runtime::install`); `None` is the
    /// executable's directory. Only the first backend loaded in a process
    /// decides this.
    pub runtime_dir: Option<PathBuf>,
    /// Allow an integrated GPU that cannot map the model file to hold its own
    /// copy of the weights. That copy is driver memory the OS cannot page out.
    pub igpu_resident_weights: bool,
    /// The vision projector (`gguf::MMPROJ_FILE`); without it requests carry
    /// no images.
    pub projector: Option<PathBuf>,
    /// Image sides are multiples of this (`VisionConfig::factor`).
    pub image_factor: usize,
}

impl Default for LlamaOptions {
    fn default() -> Self {
        Self {
            context: 1024,
            max_slots: 4,
            cpu_only: false,
            threads: 0,
            runtime_dir: None,
            igpu_resident_weights: false,
            projector: None,
            image_factor: 32,
        }
    }
}

/// Per-sequence memory of a model, from its GGUF metadata.
#[derive(Clone, Copy, Debug)]
struct Shape {
    n_vocab: u64,
    n_head: u64,
    /// f16 K and V of every attention layer, per position.
    kv_bytes_per_position: u64,
    /// f32 conv and recurrent state of every recurrent layer (fixed size).
    recurrent_bytes: u64,
}

impl Shape {
    fn read(meta: &ffi::GgufMetadata) -> Result<Self, String> {
        let arch = meta.string("general.architecture").ok_or("GGUF 缺少 general.architecture")?;
        let key = |name: &str| meta.u32(&format!("{arch}.{name}")).map(u64::from);
        let n_layer = key("block_count").ok_or("GGUF 缺少层数")?;
        let n_embd = key("embedding_length").ok_or("GGUF 缺少隐藏层宽度")?;
        let n_head = key("attention.head_count").unwrap_or(1).max(1);
        let n_head_kv = key("attention.head_count_kv").unwrap_or(n_head);
        let key_length = key("attention.key_length").unwrap_or(n_embd / n_head);
        let value_length = key("attention.value_length").unwrap_or(key_length);
        let recurrent = (key("ssm.conv_kernel"), key("ssm.state_size"), key("ssm.group_count"), key("ssm.inner_size"));
        // Hybrid models (qwen35) put full attention on every `interval`-th layer.
        let attention_layers = match (key("full_attention_interval"), recurrent.0) {
            (Some(interval), _) if interval > 0 => n_layer / interval,
            (_, Some(_)) => 0,
            _ => n_layer,
        };
        let recurrent_bytes = match recurrent {
            (Some(conv_kernel), Some(state), Some(groups), Some(inner)) => {
                let conv = conv_kernel.saturating_sub(1) * (inner + 2 * groups * state);
                (n_layer - attention_layers) * (conv + state * inner) * 4
            }
            _ => 0,
        };
        let n_vocab = meta.array_len("tokenizer.ggml.tokens").ok_or("GGUF 缺少词表")? as u64;
        Ok(Self {
            n_vocab,
            n_head,
            kv_bytes_per_position: attention_layers * n_head_kv * (key_length + value_length) * 2,
            recurrent_bytes,
        })
    }

    fn sequence_bytes(&self, context: usize) -> u64 {
        self.recurrent_bytes + self.kv_bytes_per_position * context as u64
    }

    /// Compute buffers and the logits rows for `seqs` sequences of `context`
    /// positions, with batches of `BATCH` tokens. Qwen3.5-0.8B at 1024
    /// positions measures 30 MiB of compute buffers on the CPU and 66 MiB with
    /// Metal, plus 5 MiB of logits for five sequences; this budgets 96 MiB,
    /// attention scores for a backend without flash attention, and the rows.
    fn compute_bytes(&self, seqs: usize, context: usize) -> u64 {
        let scores = BATCH as u64 * context as u64 * self.n_head * 4 * 2;
        96 * MIB + scores + self.n_vocab * 4 * seqs as u64
    }
}

/// Where the model runs and how many sequences it gets.
struct Plan {
    /// Devices the weights go to; empty means the CPU.
    devices: Vec<ffi::Device>,
    description: String,
    slots: usize,
    threads: usize,
    /// llama.cpp decides whether to map the file (an iGPU holding its own copy).
    auto_load_mode: bool,
}

/// What the machine offers, for `plan`.
struct Machine {
    devices: Vec<ffi::Device>,
    /// System memory available now.
    host_available: u64,
    threads: usize,
}

impl Machine {
    fn detect(options: &LlamaOptions) -> Self {
        let devices = ffi::devices();
        let host_available = system::available_memory()
            .or_else(|| {
                // Windows: the CPU device reports GlobalMemoryStatusEx's available memory.
                devices.iter().find(|device| device.kind == ffi::DeviceKind::Cpu).map(|cpu| cpu.memory_free)
            })
            .unwrap_or(4 << 30);
        let threads = if options.threads > 0 {
            options.threads
        } else {
            system::performance_cores()
                .or_else(|| std::thread::available_parallelism().ok().map(|n| (n.get() / 2).max(1)))
                .unwrap_or(4)
                .clamp(1, MAX_THREADS)
        };
        Self { devices, host_available, threads }
    }
}

/// Chooses the device and the slot count. `weights` is the GGUF's size.
fn plan(options: &LlamaOptions, machine: &Machine, shape: &Shape, weights: u64, context: usize) -> Plan {
    let max_slots = options.max_slots.clamp(1, MAX_SLOTS);
    let (devices, host_available, threads) = (&machine.devices, machine.host_available, machine.threads);
    // Every slot plus the scratch sequence, and the compute buffers.
    let need =
        |slots: usize| (slots as u64 + 1) * shape.sequence_bytes(context) + shape.compute_bytes(slots + 1, context);
    // Most slots whose buffers fit `budget`, if any.
    let fit = |budget: u64| (1..=max_slots).rev().find(|slots| need(*slots) <= budget);
    // Never fewer than one: the weights are mapped, so a tight machine pages rather than fails.
    let host_slots = fit(host_available / HOST_BUDGET_SHARE).unwrap_or(1);
    let on_gpu = |device: &ffi::Device, slots: usize, auto_load_mode: bool| Plan {
        description: describe(device),
        devices: vec![device.clone()],
        slots,
        threads,
        auto_load_mode,
    };

    if !options.cpu_only {
        // Discrete GPUs, most free memory first: weights, sequences and a margin in VRAM.
        let mut discrete: Vec<&ffi::Device> =
            devices.iter().filter(|device| device.kind == ffi::DeviceKind::Gpu && !device.maps_host_memory).collect();
        discrete.sort_by_key(|device| std::cmp::Reverse(device.memory_free));
        for device in discrete {
            let margin = (device.memory_total / 20).max(256 * MIB);
            if let Some(slots) = fit(device.memory_free.saturating_sub(weights + margin)) {
                return on_gpu(device, slots, false);
            }
        }
        // Unified memory the GPU reads in place: weights stay in the file's pages.
        if let Some(device) = devices.iter().find(|device| {
            matches!(device.kind, ffi::DeviceKind::Gpu | ffi::DeviceKind::IntegratedGpu) && device.maps_host_memory
        }) {
            return on_gpu(device, host_slots, false);
        }
        // An iGPU holding its own copy: only where that copy leaves room.
        if options.igpu_resident_weights && host_available > 2 * weights {
            if let Some(device) = devices.iter().find(|device| device.kind == ffi::DeviceKind::IntegratedGpu) {
                let slots = fit((host_available - weights) / HOST_BUDGET_SHARE).unwrap_or(1);
                return on_gpu(device, slots, true);
            }
        }
    }
    Plan {
        devices: Vec::new(),
        description: format!("CPU · {threads} threads"),
        slots: host_slots,
        threads,
        auto_load_mode: false,
    }
}

/// "Vulkan · NVIDIA GeForce RTX 4070", "Metal · Apple M4".
fn describe(device: &ffi::Device) -> String {
    let backend = match device.backend.as_str() {
        "MTL" => "Metal",
        "" => "GPU",
        other => other,
    };
    let name = if device.description.is_empty() { &device.name } else { &device.description };
    format!("{backend} · {name}")
}

/// Context settings, kept to recreate the context after `trim`.
#[derive(Clone, Copy, Debug)]
struct ContextSettings {
    seqs: usize,
    context: usize,
    threads: usize,
}

impl ContextSettings {
    fn params(&self) -> ffi::ContextParams {
        let mut params = ffi::default_context_params();
        params.n_seq_max = self.seqs as u32;
        // One KV stream of `context` cells per sequence (kv_unified = false),
        // so a long request cannot take another's room.
        params.n_ctx = (self.context * self.seqs) as u32;
        params.kv_unified = false;
        params.n_batch = BATCH as u32;
        params.n_ubatch = BATCH as u32;
        // One logits row per sequence at most: `step` outputs one per slot,
        // `admit` one. The default (n_batch rows) is 250 MB of logits here.
        params.n_outputs_max = self.seqs as u32;
        params.n_threads = self.threads as i32;
        params.n_threads_batch = self.threads as i32;
        params.flash_attn_type = ffi::FLASH_ATTN_AUTO;
        params.no_perf = true;
        params
    }
}

pub struct LlamaBackend {
    /// Dropped before `model` (fields drop in order; `Drop` also makes it explicit).
    projector: Option<ffi::Projector>,
    context: Option<ffi::Context>,
    model: ffi::Model,
    settings: ContextSettings,
    capacity: Capacity,
    device: String,
    /// Where the weights went (none: the CPU); the projector goes there too.
    placement: Option<ffi::Device>,
    projector_path: Option<PathBuf>,
    image_factor: usize,
    format: String,
    /// For each live slot, the rotary position its next token goes at.
    next_position: Vec<Option<usize>>,
    /// For each live slot, the sequence positions (KV cells) it holds; behind
    /// `next_position` once an image has taken fewer rotary positions.
    cells: Vec<usize>,
    batch: ffi::Batch,
}

impl LlamaBackend {
    pub fn load(gguf: &Path, options: LlamaOptions) -> Result<Self, String> {
        let file = std::fs::metadata(gguf).map_err(|error| format!("无法读取模型文件 {}: {error}", gguf.display()))?;
        ffi::init(options.runtime_dir.as_deref())?;
        let shape = Shape::read(&ffi::GgufMetadata::read(gguf)?)?;
        let context = options.context.max(1).div_ceil(CONTEXT_ALIGN) * CONTEXT_ALIGN;
        let machine = Machine::detect(&options);
        if !machine.devices.iter().any(|device| device.kind == ffi::DeviceKind::Cpu) {
            // The release build's CPU modules were not found beside its libraries.
            return Err(format!(
                "找不到 llama.cpp 的 CPU 后端{}",
                options.runtime_dir.as_ref().map(|dir| format!("（{}）", dir.display())).unwrap_or_default()
            ));
        }
        // The projector's weights and buffers go to the same device.
        let projector_bytes = options
            .projector
            .as_ref()
            .and_then(|path| std::fs::metadata(path).ok())
            .map_or(0, |meta| meta.len() + PROJECTOR_COMPUTE);
        let plan = plan(&options, &machine, &shape, file.len() + projector_bytes, context);

        let mut params = ffi::default_model_params();
        params.n_gpu_layers = if plan.devices.is_empty() { 0 } else { -1 };
        params.split_mode = ffi::SPLIT_MODE_NONE;
        params.main_gpu = 0;
        // Mapped, never locked: pages the GPU or CPU reads in place stay clean
        // file pages the OS may reclaim; a discrete GPU copies from the map.
        if !plan.auto_load_mode {
            params.load_mode = ffi::LOAD_MODE_MMAP;
        }
        // No repacked CPU copies of the weights (anonymous memory); f16 has no
        // repacked form anyway.
        params.use_extra_bufts = false;
        params.load_mtp = false;
        let model = ffi::Model::load(gguf, &plan.devices, params)?;
        if model.n_vocab() as u64 != shape.n_vocab {
            return Err(format!("模型词表大小不一致（{} 与 {}）", model.n_vocab(), shape.n_vocab));
        }

        let settings = ContextSettings { seqs: plan.slots + 1, context, threads: plan.threads };
        let mtime = file.modified().ok().and_then(|time| time.duration_since(UNIX_EPOCH).ok()).unwrap_or_default();
        // What a saved sequence depends on: the library build, the exact file, the
        // number of KV streams (one per sequence) and the device (whether V is
        // stored transposed follows its flash-attention support).
        let format = format!(
            "llama.cpp {} ({}) · gguf {} bytes, mtime {}.{:09} · {} seqs × {} · {}",
            runtime::RELEASE,
            ffi::library_identity(),
            file.len(),
            mtime.as_secs(),
            mtime.subsec_nanos(),
            settings.seqs,
            context,
            plan.devices.first().map(describe).unwrap_or_else(|| "CPU".into()),
        );
        let mut backend = Self {
            projector: None,
            context: None,
            model,
            settings,
            capacity: Capacity { slots: plan.slots, context },
            device: plan.description,
            placement: plan.devices.first().cloned(),
            projector_path: options.projector.clone(),
            image_factor: options.image_factor.max(1),
            format,
            next_position: vec![None; plan.slots],
            cells: vec![0; plan.slots],
            batch: ffi::Batch::default(),
        };
        // Create the context once now so a device that cannot hold it fails
        // here, at load, rather than on the first request.
        backend.context()?;
        if let Some(context) = &backend.context {
            if context.n_ctx_seq() < backend.settings.context {
                return Err("llama.cpp 分配的上下文小于请求".into());
            }
        }
        backend.trim();
        Ok(backend)
    }

    /// Bytes of weights as loaded.
    pub fn weights_bytes(&self) -> u64 {
        self.model.size()
    }

    fn context(&mut self) -> Result<&mut ffi::Context, String> {
        if self.context.is_none() {
            self.context = Some(ffi::Context::new(&self.model, self.settings.params())?);
        }
        Ok(self.context.as_mut().expect("context"))
    }

    /// The sequence `prefix_state` works in.
    fn scratch_seq(&self) -> i32 {
        self.capacity.slots as i32
    }

    /// Runs `picture` in `seq` from rotary position `position`; returns the
    /// rotary position after it.
    fn run_picture(&mut self, seq: i32, position: usize, picture: &crate::vision::Picture) -> Result<usize, String> {
        if self.projector.is_none() {
            let path = self.projector_path.clone().ok_or("这个模型版本不能读图片")?;
            self.projector = Some(ffi::Projector::load(
                &path,
                &self.model,
                self.placement.as_ref(),
                self.settings.threads,
                crate::service::MIN_IMAGE_TOKENS,
                crate::service::MAX_IMAGE_TOKENS,
            )?);
        }
        let tokens = Segment::Picture(std::sync::Arc::new(picture.clone())).len(self.image_factor);
        let context = self.context.as_mut().ok_or("本地模型上下文已释放")?;
        let n_batch = context.n_batch().clamp(1, BATCH);
        let projector = self.projector.as_mut().expect("loaded above");
        projector.eval(context, picture.width, picture.height, &picture.rgb, tokens, position, seq, n_batch)
    }

    fn check_tokens(&self, tokens: &[u32]) -> Result<(), String> {
        let n_vocab = self.model.n_vocab();
        match tokens.iter().find(|token| **token as usize >= n_vocab) {
            Some(token) => Err(format!("词元 {token} 超出词表")),
            None => Ok(()),
        }
    }

    /// Runs `tokens` in `seq` from `start`, `BATCH` at a time; with `output`,
    /// returns the logits after the last one.
    fn run(&mut self, seq: i32, start: usize, tokens: &[u32], output: bool) -> Result<Option<Logits>, String> {
        if tokens.is_empty() {
            return Err("没有要计算的词元".into());
        }
        let mut batch = std::mem::take(&mut self.batch);
        let result = (|| {
            let context = self.context()?;
            let chunk_len = context.n_batch().clamp(1, BATCH);
            let chunks = tokens.chunks(chunk_len).count();
            for (index, chunk) in tokens.chunks(chunk_len).enumerate() {
                let last_chunk = index + 1 == chunks;
                batch.clear();
                let offset = start + index * chunk_len;
                for (i, token) in chunk.iter().enumerate() {
                    batch.push(*token, offset + i, seq, output && last_chunk && i + 1 == chunk.len());
                }
                context.decode(&mut batch)?;
            }
            if output {
                let last = tokens.len() - (chunks - 1) * chunk_len - 1;
                return context.logits(last).map(Some);
            }
            Ok(None)
        })();
        self.batch = batch;
        result
    }
}

impl Backend for LlamaBackend {
    fn device(&self) -> String {
        self.device.clone()
    }

    fn capacity(&self) -> Capacity {
        self.capacity
    }

    fn state_format(&self) -> String {
        self.format.clone()
    }

    fn prefix_state(&mut self, tokens: &[u32]) -> Result<PrefixState, String> {
        if tokens.len() > self.capacity.context {
            return Err("前缀超出本地模型上下文".into());
        }
        self.check_tokens(tokens)?;
        if tokens.is_empty() {
            return Ok(PrefixState { tokens: 0, format: self.format.clone(), bytes: Vec::new().into() });
        }
        let seq = self.scratch_seq();
        self.context()?.seq_remove(seq);
        let result = self.run(seq, 0, tokens, false).and_then(|_| self.context()?.seq_state(seq));
        if let Some(context) = self.context.as_mut() {
            context.seq_remove(seq);
        }
        Ok(PrefixState { tokens: tokens.len(), format: self.format.clone(), bytes: result?.into() })
    }

    fn admit(&mut self, slot: usize, prefix: &PrefixState, input: &[Segment]) -> Result<Logits, String> {
        if slot >= self.capacity.slots {
            return Err(format!("槽位 {slot} 不存在"));
        }
        // The logits come from the last text token.
        if !matches!(input.last(), Some(Segment::Tokens(tokens)) if !tokens.is_empty()) {
            return Err("请求必须以词元结尾".into());
        }
        if prefix.format != self.format {
            return Err("前缀状态来自另一个模型或配置，需要重新计算".into());
        }
        let cells: usize = input.iter().map(|segment| segment.len(self.image_factor)).sum();
        if prefix.tokens + cells > self.capacity.context {
            return Err("请求超出本地模型上下文".into());
        }
        for segment in input {
            if let Segment::Tokens(tokens) = segment {
                self.check_tokens(tokens)?;
            }
        }
        self.next_position[slot] = None;
        let seq = slot as i32;
        let context = self.context()?;
        context.seq_remove(seq);
        let result = (|| {
            if prefix.tokens > 0 {
                context.set_seq_state(seq, &prefix.bytes)?;
                if context.seq_pos_max(seq) != prefix.tokens as i32 - 1 {
                    return Err("前缀状态与记录的长度不符".to_owned());
                }
            }
            Ok(())
        })()
        .and_then(|()| {
            let mut position = prefix.tokens;
            let mut logits = None;
            for (index, segment) in input.iter().enumerate() {
                match segment {
                    Segment::Tokens(tokens) if tokens.is_empty() => {}
                    Segment::Tokens(tokens) => {
                        logits = self.run(seq, position, tokens, index + 1 == input.len())?;
                        position += tokens.len();
                    }
                    Segment::Picture(picture) => position = self.run_picture(seq, position, picture)?,
                }
            }
            Ok((logits, position))
        });
        match result {
            Ok((logits, position)) => {
                self.next_position[slot] = Some(position);
                self.cells[slot] = prefix.tokens + cells;
                Ok(logits.expect("logits requested"))
            }
            Err(error) => {
                if let Some(context) = self.context.as_mut() {
                    context.seq_remove(seq);
                }
                Err(error)
            }
        }
    }

    fn step(&mut self, batch: &[(usize, u32)]) -> Result<Vec<Logits>, String> {
        if batch.is_empty() {
            return Ok(Vec::new());
        }
        let mut seen = vec![false; self.capacity.slots];
        for (slot, token) in batch {
            if self.next_position.get(*slot).copied().flatten().is_none() {
                return Err(format!("槽位 {slot} 没有进行中的请求"));
            }
            if std::mem::replace(&mut seen[*slot], true) {
                return Err(format!("槽位 {slot} 在同一步中出现两次"));
            }
            if self.cells[*slot] >= self.capacity.context {
                return Err("请求超出本地模型上下文".into());
            }
            self.check_tokens(&[*token])?;
        }
        let mut raw = std::mem::take(&mut self.batch);
        raw.clear();
        for (slot, token) in batch {
            raw.push(*token, self.next_position[*slot].expect("checked"), *slot as i32, true);
        }
        let result = (|| {
            let context = self.context.as_mut().ok_or("本地模型上下文已释放")?;
            context.decode(&mut raw)?;
            (0..batch.len()).map(|index| context.logits(index)).collect::<Result<Vec<_>, _>>()
        })();
        self.batch = raw;
        let logits = result?;
        for (slot, _) in batch {
            if let Some(position) = self.next_position[*slot].as_mut() {
                *position += 1;
                self.cells[*slot] += 1;
            }
        }
        Ok(logits)
    }

    fn release(&mut self, slot: usize) {
        if slot >= self.capacity.slots {
            return;
        }
        self.next_position[slot] = None;
        self.cells[slot] = 0;
        if let Some(context) = self.context.as_mut() {
            context.seq_remove(slot as i32);
        }
    }

    fn trim(&mut self) {
        // Live sequences would be lost; the scheduler only trims when idle.
        if self.next_position.iter().any(Option::is_some) {
            return;
        }
        self.context = None;
    }
}

impl Drop for LlamaBackend {
    fn drop(&mut self) {
        // The projector and the context refer to the model; free them first.
        self.projector = None;
        self.context = None;
    }
}
