//! The part of llama.cpp's C API the backend uses, behind owned handles.
//!
//! Every pointer from llama.cpp lives in one of the types here and is freed by
//! its `Drop`; nothing else in the backend touches `llama_cpp_sys_2`. None of
//! these types is `Sync`: llama.cpp allows a model or context on any thread,
//! but not on two at once.

use std::ffi::{c_char, c_void, CStr, CString};
use std::path::Path;
use std::ptr::NonNull;
use std::sync::{Mutex, MutexGuard, Once, OnceLock, PoisonError};

use llama_cpp_sys_2 as sys;

pub use sys::{llama_context_params as ContextParams, llama_model_params as ModelParams};

/// Last error llama.cpp logged, appended to the message of a failed call:
/// the C API reports most failures only as a null pointer or a status code.
static LAST_ERROR: Mutex<String> = Mutex::new(String::new());
/// Whether the last message logged was an error, so its continuation lines
/// (`GGML_LOG_LEVEL_CONT`) join it.
static LAST_WAS_ERROR: Mutex<bool> = Mutex::new(false);

/// The logger must not panic across the C boundary, poisoned or not.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Registers the log callback and initializes the backends, once per process.
/// `backend_dir` is where `llama-dynamic` builds find the ggml backend modules.
pub fn init(backend_dir: Option<&Path>) {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        // SAFETY: `log` is a valid callback for the life of the process.
        unsafe { sys::llama_log_set(Some(log), std::ptr::null_mut()) };
        init_backends(backend_dir);
    });
}

#[cfg(not(feature = "llama-dynamic"))]
fn init_backends(_backend_dir: Option<&Path>) {
    // The backends are linked in and register themselves.
    // SAFETY: once per process, before any other llama.cpp call but the logger.
    unsafe { sys::llama_backend_init() };
}

#[cfg(feature = "llama-dynamic")]
fn init_backends(backend_dir: Option<&Path>) {
    // What llama_backend_init does, except that with no backend registered it
    // would search the build machine's backend directory, the executable's
    // directory and the working directory; the working directory is no place
    // to load code from. Load from one known directory instead.
    // SAFETY: plain initialization calls; the context only fills ggml's f16 tables.
    unsafe {
        sys::ggml_time_init();
        let params = sys::ggml_init_params { mem_size: 0, mem_buffer: std::ptr::null_mut(), no_alloc: false };
        let context = sys::ggml_init(params);
        if !context.is_null() {
            sys::ggml_free(context);
        }
    }
    // Every backend, the CPU ones included, is a module here. ggml loads each
    // module it finds, picks the CPU variant that suits this processor, and
    // skips a GPU module whose runtime (vulkan-1, cudart) is not installed.
    let dir = backend_dir
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)));
    if let Some(dir) = dir.and_then(|dir| CString::new(dir.to_string_lossy().into_owned()).ok()) {
        // SAFETY: a NUL-terminated UTF-8 path, read during the call.
        unsafe { sys::ggml_backend_load_all_from_path(dir.as_ptr()) };
    }
}

unsafe extern "C" fn log(level: sys::ggml_log_level, text: *const c_char, _user_data: *mut c_void) {
    if text.is_null() {
        return;
    }
    // SAFETY: llama.cpp passes a NUL-terminated message.
    let text = unsafe { CStr::from_ptr(text) }.to_string_lossy();
    let is_error = match level {
        sys::GGML_LOG_LEVEL_ERROR => true,
        sys::GGML_LOG_LEVEL_CONT => *lock(&LAST_WAS_ERROR),
        _ => false,
    };
    if level != sys::GGML_LOG_LEVEL_CONT {
        *lock(&LAST_WAS_ERROR) = is_error;
    }
    if is_error {
        let mut last = lock(&LAST_ERROR);
        if level != sys::GGML_LOG_LEVEL_CONT {
            last.clear();
        }
        last.push_str(&text);
    }
    // Nothing reaches stdout. Debug builds show warnings and errors on stderr;
    // MEWORK_LLAMA_LOG=1 shows everything llama.cpp says (buffer sizes, devices).
    static VERBOSE: OnceLock<bool> = OnceLock::new();
    let verbose = *VERBOSE.get_or_init(|| std::env::var_os("MEWORK_LLAMA_LOG").is_some_and(|value| value != "0"));
    if verbose || (cfg!(debug_assertions) && (is_error || level == sys::GGML_LOG_LEVEL_WARN)) {
        eprint!("{text}");
    }
}

/// Forgets the last logged error, before a call whose failure should report it.
fn clear_last_error() {
    lock(&LAST_ERROR).clear();
}

/// `message`, with what llama.cpp logged about the failure if anything.
pub fn with_last_error(message: String) -> String {
    let last = lock(&LAST_ERROR);
    let detail = last.trim();
    if detail.is_empty() {
        message
    } else {
        format!("{message}（{detail}）")
    }
}

fn string(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    // SAFETY: llama.cpp returns NUL-terminated strings that outlive the call.
    unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned()
}

/// x86 extensions the linked CPU backend was compiled for that this processor
/// lacks; running it anyway kills the process with an illegal instruction.
/// A static build compiled on an x86 machine uses AVX2, FMA and F16C (ggml's
/// defaults once `GGML_NATIVE` is off); `llama-dynamic` builds pick a CPU
/// variant at run time instead and need no check.
#[cfg(all(not(feature = "llama-dynamic"), any(target_arch = "x86", target_arch = "x86_64")))]
pub fn missing_cpu_features() -> Vec<&'static str> {
    use std::arch::is_x86_feature_detected as has;
    // SAFETY: each returns a compile-time constant and runs no vector code.
    let built = unsafe {
        [
            ("SSE3", sys::ggml_cpu_has_sse3() != 0, has!("sse3")),
            ("SSSE3", sys::ggml_cpu_has_ssse3() != 0, has!("ssse3")),
            ("AVX", sys::ggml_cpu_has_avx() != 0, has!("avx")),
            ("AVX2", sys::ggml_cpu_has_avx2() != 0, has!("avx2")),
            ("BMI2", sys::ggml_cpu_has_bmi2() != 0, has!("bmi2")),
            ("FMA", sys::ggml_cpu_has_fma() != 0, has!("fma")),
            ("F16C", sys::ggml_cpu_has_f16c() != 0, has!("f16c")),
            ("AVX512F", sys::ggml_cpu_has_avx512() != 0, has!("avx512f")),
        ]
    };
    built.iter().filter(|(_, used, present)| *used && !*present).map(|(name, _, _)| *name).collect()
}

#[cfg(not(all(not(feature = "llama-dynamic"), any(target_arch = "x86", target_arch = "x86_64"))))]
pub fn missing_cpu_features() -> Vec<&'static str> {
    Vec::new()
}

/// "llama <version> · ggml <version> <commit>" of the linked library.
pub fn library_identity() -> String {
    // SAFETY: static strings.
    unsafe {
        format!(
            "llama {} · ggml {} {}",
            string(sys::llama_version()),
            string(sys::ggml_version()),
            string(sys::ggml_commit())
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    Cpu,
    /// Dedicated memory.
    Gpu,
    /// Shares system memory.
    IntegratedGpu,
    /// Helps the CPU backend (BLAS, AMX); used automatically.
    Accelerator,
    Other,
}

/// One ggml backend device, as registered at `init`.
#[derive(Clone, Debug)]
pub struct Device {
    raw: sys::ggml_backend_dev_t,
    /// Registry name: "CPU", "Metal", "Vulkan", "CUDA", …
    pub backend: String,
    /// Device name within the backend, e.g. "Vulkan0".
    pub name: String,
    /// Human-readable, e.g. "NVIDIA GeForce RTX 4070".
    pub description: String,
    pub kind: DeviceKind,
    pub memory_free: u64,
    pub memory_total: u64,
    /// Can wrap host memory (the mapped model file) as a device buffer, so
    /// weights stay in the file's pages instead of being copied.
    pub maps_host_memory: bool,
}

// SAFETY: device handles are process-wide registry entries that live until exit.
unsafe impl Send for Device {}

#[cfg(test)]
impl Device {
    /// A device that is never handed to llama.cpp, for placement tests.
    pub fn fake(backend: &str, description: &str, kind: DeviceKind, memory_free: u64, maps_host_memory: bool) -> Self {
        Self {
            raw: std::ptr::null_mut(),
            backend: backend.into(),
            name: format!("{backend}0"),
            description: description.into(),
            kind,
            memory_free,
            memory_total: memory_free,
            maps_host_memory,
        }
    }
}

pub fn devices() -> Vec<Device> {
    // SAFETY: registry enumeration after `init`; every handle returned is valid.
    unsafe {
        (0..sys::ggml_backend_dev_count())
            .filter_map(|index| {
                let raw = sys::ggml_backend_dev_get(index);
                if raw.is_null() {
                    return None;
                }
                let mut props: sys::ggml_backend_dev_props = std::mem::zeroed();
                sys::ggml_backend_dev_get_props(raw, &mut props);
                let reg = sys::ggml_backend_dev_backend_reg(raw);
                let kind = match props.type_ {
                    sys::GGML_BACKEND_DEVICE_TYPE_CPU => DeviceKind::Cpu,
                    sys::GGML_BACKEND_DEVICE_TYPE_GPU => DeviceKind::Gpu,
                    sys::GGML_BACKEND_DEVICE_TYPE_IGPU => DeviceKind::IntegratedGpu,
                    sys::GGML_BACKEND_DEVICE_TYPE_ACCEL => DeviceKind::Accelerator,
                    _ => DeviceKind::Other,
                };
                Some(Device {
                    raw,
                    backend: if reg.is_null() { String::new() } else { string(sys::ggml_backend_reg_name(reg)) },
                    name: string(props.name),
                    description: string(props.description),
                    kind,
                    memory_free: props.memory_free as u64,
                    memory_total: props.memory_total as u64,
                    maps_host_memory: props.caps.buffer_from_host_ptr && props.caps.mmap_support,
                })
            })
            .collect()
    }
}

/// Metadata of a GGUF file, read without its tensor data.
pub struct GgufMetadata(NonNull<sys::gguf_context>);

impl GgufMetadata {
    pub fn read(path: &Path) -> Result<Self, String> {
        let c_path = c_path(path)?;
        let params = sys::gguf_init_params { no_alloc: true, ctx: std::ptr::null_mut() };
        clear_last_error();
        // SAFETY: a NUL-terminated path; with `no_alloc` only metadata is read.
        let raw = unsafe { sys::gguf_init_from_file(c_path.as_ptr(), params) };
        NonNull::new(raw).map(Self).ok_or_else(|| with_last_error(format!("无法读取 GGUF 文件 {}", path.display())))
    }

    fn key(&self, key: &str) -> Option<i64> {
        let key = CString::new(key).ok()?;
        // SAFETY: valid context and NUL-terminated key.
        let id = unsafe { sys::gguf_find_key(self.0.as_ptr(), key.as_ptr()) };
        (id >= 0).then_some(id)
    }

    pub fn string(&self, key: &str) -> Option<String> {
        let id = self.key(key)?;
        // SAFETY: `id` was found in this context; the type is checked before reading.
        unsafe {
            (sys::gguf_get_kv_type(self.0.as_ptr(), id) == sys::GGUF_TYPE_STRING)
                .then(|| string(sys::gguf_get_val_str(self.0.as_ptr(), id)))
        }
    }

    /// An unsigned or signed 32-bit value.
    pub fn u32(&self, key: &str) -> Option<u32> {
        let id = self.key(key)?;
        // SAFETY: as above.
        unsafe {
            match sys::gguf_get_kv_type(self.0.as_ptr(), id) {
                sys::GGUF_TYPE_UINT32 => Some(sys::gguf_get_val_u32(self.0.as_ptr(), id)),
                sys::GGUF_TYPE_INT32 => u32::try_from(sys::gguf_get_val_i32(self.0.as_ptr(), id)).ok(),
                _ => None,
            }
        }
    }

    /// Length of an array value.
    pub fn array_len(&self, key: &str) -> Option<usize> {
        let id = self.key(key)?;
        // SAFETY: as above.
        unsafe {
            (sys::gguf_get_kv_type(self.0.as_ptr(), id) == sys::GGUF_TYPE_ARRAY)
                .then(|| sys::gguf_get_arr_n(self.0.as_ptr(), id))
        }
    }
}

impl Drop for GgufMetadata {
    fn drop(&mut self) {
        // SAFETY: owned context, freed once.
        unsafe { sys::gguf_free(self.0.as_ptr()) }
    }
}

fn c_path(path: &Path) -> Result<CString, String> {
    // llama.cpp opens paths with fopen, which takes UTF-8 on every platform it
    // supports (it converts to UTF-16 on Windows).
    let text = path.to_str().ok_or_else(|| format!("模型路径不是有效的 UTF-8：{}", path.display()))?;
    CString::new(text).map_err(|_| format!("模型路径含有空字符：{}", path.display()))
}

pub fn default_model_params() -> ModelParams {
    // SAFETY: returns a value.
    unsafe { sys::llama_model_default_params() }
}

pub fn default_context_params() -> ContextParams {
    // SAFETY: returns a value.
    unsafe { sys::llama_context_default_params() }
}

pub const LOAD_MODE_MMAP: sys::llama_load_mode = sys::LLAMA_LOAD_MODE_MMAP;
pub const SPLIT_MODE_NONE: sys::llama_split_mode = sys::LLAMA_SPLIT_MODE_NONE;
pub const FLASH_ATTN_AUTO: sys::llama_flash_attn_type = sys::LLAMA_FLASH_ATTN_TYPE_AUTO;

pub struct Model {
    raw: NonNull<sys::llama_model>,
    n_vocab: usize,
}

// SAFETY: a model is immutable after loading and used from one thread at a time.
unsafe impl Send for Model {}

impl Model {
    /// Loads `path` on `devices` (none: CPU only). `params.devices` is set here.
    pub fn load(path: &Path, devices: &[Device], mut params: ModelParams) -> Result<Self, String> {
        let c_path = c_path(path)?;
        // NULL-terminated; an empty list keeps every layer on the CPU.
        let mut list: Vec<sys::ggml_backend_dev_t> = devices.iter().map(|device| device.raw).collect();
        list.push(std::ptr::null_mut());
        params.devices = list.as_mut_ptr();
        clear_last_error();
        // SAFETY: the path and device list outlive the call, which copies what it keeps.
        let raw = unsafe { sys::llama_model_load_from_file(c_path.as_ptr(), params) };
        let raw =
            NonNull::new(raw).ok_or_else(|| with_last_error(format!("llama.cpp 无法加载模型 {}", path.display())))?;
        // SAFETY: a loaded model always has a vocabulary.
        let n_vocab = unsafe { sys::llama_vocab_n_tokens(sys::llama_model_get_vocab(raw.as_ptr())) };
        let model = Self { raw, n_vocab: n_vocab.max(0) as usize };
        if model.n_vocab == 0 {
            return Err("模型词表为空".into());
        }
        Ok(model)
    }

    pub fn n_vocab(&self) -> usize {
        self.n_vocab
    }

    /// Bytes of weights, wherever they were placed.
    pub fn size(&self) -> u64 {
        // SAFETY: valid model.
        unsafe { sys::llama_model_size(self.raw.as_ptr()) }
    }
}

impl Drop for Model {
    fn drop(&mut self) {
        // SAFETY: owned model; every context on it was freed first (see `LlamaBackend`).
        unsafe { sys::llama_model_free(self.raw.as_ptr()) }
    }
}

/// A context on a model: the KV caches and recurrent states of its sequences
/// and the compute buffers. It must be dropped before its model.
pub struct Context {
    raw: NonNull<sys::llama_context>,
    memory: sys::llama_memory_t,
    n_vocab: usize,
}

// SAFETY: used from one thread at a time, like the model.
unsafe impl Send for Context {}

impl Context {
    pub fn new(model: &Model, params: ContextParams) -> Result<Self, String> {
        clear_last_error();
        // SAFETY: valid model; params is a plain value.
        let raw = unsafe { sys::llama_init_from_model(model.raw.as_ptr(), params) };
        let raw = NonNull::new(raw).ok_or_else(|| with_last_error("llama.cpp 无法创建推理上下文".into()))?;
        // SAFETY: valid context; a decoder context always has memory.
        let memory = unsafe { sys::llama_get_memory(raw.as_ptr()) };
        let context = Self { raw, memory, n_vocab: model.n_vocab };
        if memory.is_null() {
            return Err("llama.cpp 上下文没有 KV 缓存".into());
        }
        Ok(context)
    }

    pub fn n_batch(&self) -> usize {
        // SAFETY: valid context.
        unsafe { sys::llama_n_batch(self.raw.as_ptr()) as usize }
    }

    pub fn n_ctx_seq(&self) -> usize {
        // SAFETY: valid context.
        unsafe { sys::llama_n_ctx_seq(self.raw.as_ptr()) as usize }
    }

    /// Runs `batch`. Its tokens must fit `n_batch`.
    pub fn decode(&mut self, batch: &mut Batch) -> Result<(), String> {
        if batch.len() == 0 {
            return Ok(());
        }
        clear_last_error();
        // SAFETY: the batch's arrays are alive and all `len()` long.
        let status = unsafe { sys::llama_decode(self.raw.as_ptr(), batch.raw()) };
        match status {
            0 => Ok(()),
            1 => Err(with_last_error("llama.cpp 的 KV 缓存已满".into())),
            status => Err(with_last_error(format!("llama.cpp 推理失败（{status}）"))),
        }
    }

    /// Logits after token `index` of the last decoded batch, which must have
    /// asked for that token's output.
    pub fn logits(&mut self, index: usize) -> Result<Vec<f32>, String> {
        let index = i32::try_from(index).map_err(|_| "输出索引过大".to_owned())?;
        // SAFETY: valid context; a non-null result points at n_vocab floats that
        // stay valid until the next decode.
        unsafe {
            let ptr = sys::llama_get_logits_ith(self.raw.as_ptr(), index);
            if ptr.is_null() {
                return Err(with_last_error("llama.cpp 没有返回 logits".into()));
            }
            Ok(std::slice::from_raw_parts(ptr, self.n_vocab).to_vec())
        }
    }

    /// Drops everything stored for `seq`.
    pub fn seq_remove(&mut self, seq: i32) {
        // SAFETY: valid memory; removing a whole sequence never fails.
        unsafe { sys::llama_memory_seq_rm(self.memory, seq, -1, -1) };
    }

    /// Largest position stored for `seq`, or -1 when it is empty.
    pub fn seq_pos_max(&self, seq: i32) -> i32 {
        // SAFETY: valid memory.
        unsafe { sys::llama_memory_seq_pos_max(self.memory, seq) }
    }

    /// `seq`'s KV cache and recurrent state, serialized.
    pub fn seq_state(&mut self, seq: i32) -> Result<Vec<u8>, String> {
        // SAFETY: valid context; the buffer is as large as llama.cpp asked for.
        unsafe {
            let size = sys::llama_state_seq_get_size(self.raw.as_ptr(), seq);
            let mut bytes = vec![0u8; size];
            let written = sys::llama_state_seq_get_data(self.raw.as_ptr(), bytes.as_mut_ptr(), bytes.len(), seq);
            if written == 0 || written > size {
                return Err(with_last_error("无法保存 llama.cpp 序列状态".into()));
            }
            bytes.truncate(written);
            Ok(bytes)
        }
    }

    /// Replaces `seq` with a state from `seq_state`.
    pub fn set_seq_state(&mut self, seq: i32, bytes: &[u8]) -> Result<(), String> {
        clear_last_error();
        // SAFETY: valid context; llama.cpp reads at most `bytes.len()` bytes and
        // rejects (returns 0) data it cannot parse.
        let read = unsafe { sys::llama_state_seq_set_data(self.raw.as_ptr(), bytes.as_ptr(), bytes.len(), seq) };
        if read == 0 {
            self.seq_remove(seq);
            return Err(with_last_error("前缀状态与当前模型不符".into()));
        }
        Ok(())
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        // SAFETY: owned context, freed once, before its model.
        unsafe { sys::llama_free(self.raw.as_ptr()) }
    }
}

/// Tokens for one `llama_decode`, each in one sequence.
#[derive(Default)]
pub struct Batch {
    tokens: Vec<sys::llama_token>,
    positions: Vec<sys::llama_pos>,
    n_seq_id: Vec<i32>,
    seq_ids: Vec<sys::llama_seq_id>,
    seq_id_ptrs: Vec<*mut sys::llama_seq_id>,
    outputs: Vec<i8>,
}

// SAFETY: the raw pointers only ever point into this batch's own vectors.
unsafe impl Send for Batch {}

impl Batch {
    pub fn clear(&mut self) {
        self.tokens.clear();
        self.positions.clear();
        self.n_seq_id.clear();
        self.seq_ids.clear();
        self.seq_id_ptrs.clear();
        self.outputs.clear();
    }

    pub fn push(&mut self, token: u32, position: usize, seq: i32, output: bool) {
        self.tokens.push(token as sys::llama_token);
        self.positions.push(position as sys::llama_pos);
        self.n_seq_id.push(1);
        self.seq_ids.push(seq);
        self.outputs.push(output as i8);
    }

    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    fn raw(&mut self) -> sys::llama_batch {
        // Pointers into `seq_ids`, taken now that it no longer grows.
        self.seq_id_ptrs.clear();
        let base = self.seq_ids.as_mut_ptr();
        // SAFETY: every index is within `seq_ids`.
        self.seq_id_ptrs.extend((0..self.seq_ids.len()).map(|i| unsafe { base.add(i) }));
        sys::llama_batch {
            n_tokens: self.tokens.len() as i32,
            token: self.tokens.as_mut_ptr(),
            embd: std::ptr::null_mut(),
            pos: self.positions.as_mut_ptr(),
            n_seq_id: self.n_seq_id.as_mut_ptr(),
            seq_id: self.seq_id_ptrs.as_mut_ptr(),
            logits: self.outputs.as_mut_ptr(),
        }
    }
}
