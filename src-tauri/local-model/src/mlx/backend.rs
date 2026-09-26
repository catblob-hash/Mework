//! `engine::Backend` on MLX, through the C interface in `mlx/mework_mlx.h`.

use std::ffi::{c_char, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use memmap2::Mmap;

use super::weights::{Dtype, Index, INDEX_FILE, WEIGHTS_FILE};
use super::METALLIB_FILE;
use crate::engine::{Backend, Capacity, Logits, PrefixState};
use crate::qwen35::{Config, LayerKind};

/// The MLX release `libmework_mlx.dylib` was built against.
pub const MLX_VERSION: &str = env!("MEWORK_MLX_VERSION");
/// Freed GPU buffers MLX may keep for reuse; the rest go back to the system.
const CACHE_LIMIT: usize = 64 << 20;

#[repr(C)]
struct RawConfig {
    hidden: i32,
    intermediate: i32,
    vocab: i32,
    layers: i32,
    full: *const u8,
    heads: i32,
    kv_heads: i32,
    head_dim: i32,
    rotary_dim: i32,
    linear_key_heads: i32,
    linear_value_heads: i32,
    linear_key_dim: i32,
    linear_value_dim: i32,
    conv_kernel: i32,
    eps: f32,
    slots: i32,
    context: i32,
    cos: *const f32,
    sin: *const f32,
}

#[repr(C)]
struct RawTensor {
    name: *const c_char,
    data: *const c_void,
    dtype: i32,
    ndim: i32,
    shape: [i64; 4],
}

type Model = c_void;

struct Api {
    last_error: unsafe extern "C" fn() -> *const c_char,
    init: unsafe extern "C" fn(*const c_char, usize) -> i32,
    device_name: unsafe extern "C" fn() -> *const c_char,
    load: unsafe extern "C" fn(*const RawConfig, *const RawTensor, usize) -> *mut Model,
    free: unsafe extern "C" fn(*mut Model),
    prefix: unsafe extern "C" fn(*mut Model, *const u32, usize, *mut *mut u8, *mut usize) -> i32,
    free_bytes: unsafe extern "C" fn(*mut u8),
    admit: unsafe extern "C" fn(*mut Model, i32, *const u8, usize, i32, *const u32, usize, *mut f32) -> i32,
    step: unsafe extern "C" fn(*mut Model, *const i32, *const u32, usize, *mut f32) -> i32,
    release: unsafe extern "C" fn(*mut Model, i32),
    trim: unsafe extern "C" fn(*mut Model),
}

impl Api {
    fn error(&self) -> String {
        // SAFETY: returns a NUL-terminated thread-local string owned by the library.
        let text = unsafe { CStr::from_ptr((self.last_error)()) }.to_string_lossy().into_owned();
        if text.is_empty() {
            "MLX 出错".into()
        } else {
            format!("MLX：{text}")
        }
    }

    fn check(&self, status: i32) -> Result<(), String> {
        if status == 0 {
            Ok(())
        } else {
            Err(self.error())
        }
    }
}

/// Where `libmework_mlx.dylib` is: in the bundle's `Frameworks/mework-mlx`,
/// else where this build made it.
pub fn shim_path() -> Option<PathBuf> {
    let bundled = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("../Frameworks/mework-mlx/libmework_mlx.dylib")));
    bundled.filter(|path| path.exists()).or_else(|| {
        let built = PathBuf::from(env!("MEWORK_MLX_SHIM"));
        built.exists().then_some(built)
    })
}

fn open_api() -> Result<Api, String> {
    let path = shim_path().ok_or("找不到 MLX 运行库（libmework_mlx.dylib）")?;
    let c_path = CString::new(path.to_string_lossy().as_bytes()).map_err(|_| "路径含有空字符")?;
    // SAFETY: loading our own library; it has no initializers with side effects
    // beyond MLX's static setup.
    let handle = unsafe { libc::dlopen(c_path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if handle.is_null() {
        // SAFETY: dlerror returns a NUL-terminated string or null.
        let reason = unsafe { libc::dlerror() };
        let reason =
            if reason.is_null() { String::new() } else { unsafe { CStr::from_ptr(reason) }.to_string_lossy().into_owned() };
        return Err(format!("无法加载 MLX 运行库：{reason}"));
    }
    macro_rules! symbol {
        ($name:literal) => {{
            let name = CString::new($name).expect("symbol name");
            // SAFETY: `handle` is a live library handle.
            let pointer = unsafe { libc::dlsym(handle, name.as_ptr()) };
            if pointer.is_null() {
                return Err(format!("MLX 运行库缺少 {}", $name));
            }
            // SAFETY: the symbol has the signature declared in mework_mlx.h.
            unsafe { std::mem::transmute::<*mut c_void, _>(pointer) }
        }};
    }
    Ok(Api {
        last_error: symbol!("mwx_last_error"),
        init: symbol!("mwx_init"),
        device_name: symbol!("mwx_device_name"),
        load: symbol!("mwx_load"),
        free: symbol!("mwx_free"),
        prefix: symbol!("mwx_prefix"),
        free_bytes: symbol!("mwx_free_bytes"),
        admit: symbol!("mwx_admit"),
        step: symbol!("mwx_step"),
        release: symbol!("mwx_release"),
        trim: symbol!("mwx_trim"),
    })
}

/// The library, loaded and initialized once per process. MLX reads its
/// kernel library path once, before its first operation, so the first
/// `metallib` wins; later loads must name the same file.
fn api(metallib: &Path) -> Result<&'static Api, String> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    static METALLIB: Mutex<Option<PathBuf>> = Mutex::new(None);
    let api = API.get_or_init(open_api).as_ref().map_err(Clone::clone)?;
    let mut initialized = METALLIB.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    match initialized.as_deref() {
        Some(previous) if previous == metallib => {}
        Some(_) => return Err("MLX 内核库路径变了，请重启应用".into()),
        None => {
            let c_path = CString::new(metallib.to_string_lossy().as_bytes()).map_err(|_| "路径含有空字符")?;
            // SAFETY: plain C call with a valid string.
            api.check(unsafe { (api.init)(c_path.as_ptr(), CACHE_LIMIT) })?;
            *initialized = Some(metallib.to_path_buf());
        }
    }
    Ok(api)
}

pub struct MlxBackend {
    api: &'static Api,
    model: *mut Model,
    /// Borrowed by `model`'s weights; dropped after it (see `Drop`).
    _weights: Mmap,
    config: Config,
    slots: usize,
    context: usize,
    device: String,
}

// SAFETY: the model is only used through `&mut self`, from one thread at a time.
unsafe impl Send for MlxBackend {}

impl MlxBackend {
    /// Loads the MLX build in `dir` (weights, index, config, `mlx.metallib`).
    pub fn load(dir: &Path, slots: usize, context: usize) -> Result<Self, String> {
        let config = Config::load(&dir.join("config.json"))?;
        let index = Index::load(&dir.join(INDEX_FILE))?;
        let file = std::fs::File::open(dir.join(WEIGHTS_FILE)).map_err(|error| format!("无法打开权重: {error}"))?;
        // SAFETY: the model directory belongs to the app and is not modified while loaded.
        let weights = unsafe { Mmap::map(&file) }.map_err(|error| format!("无法映射权重: {error}"))?;
        index.check(weights.len() as u64)?;
        let api = api(&dir.join(METALLIB_FILE))?;

        let full: Vec<u8> = config.layers.iter().map(|kind| u8::from(*kind == LayerKind::Full)).collect();
        let (cos, sin) = rope_table(&config, context);
        let names: Vec<CString> =
            index.tensors.keys().map(|name| CString::new(name.as_str()).expect("tensor name")).collect();
        let tensors: Vec<RawTensor> = index
            .tensors
            .values()
            .zip(&names)
            .map(|(entry, name)| {
                let mut shape = [0i64; 4];
                for (slot, dim) in shape.iter_mut().zip(&entry.shape) {
                    *slot = *dim as i64;
                }
                RawTensor {
                    name: name.as_ptr(),
                    // SAFETY: `check` put the tensor inside the mapping.
                    data: unsafe { weights.as_ptr().add(entry.offset as usize) }.cast(),
                    dtype: match entry.dtype {
                        Dtype::F16 => 0,
                        Dtype::F32 => 1,
                    },
                    ndim: entry.shape.len() as i32,
                    shape,
                }
            })
            .collect();
        let raw = RawConfig {
            hidden: config.hidden_size as i32,
            intermediate: config.intermediate_size as i32,
            vocab: config.vocab_size as i32,
            layers: config.layers.len() as i32,
            full: full.as_ptr(),
            heads: config.num_attention_heads as i32,
            kv_heads: config.num_key_value_heads as i32,
            head_dim: config.head_dim as i32,
            rotary_dim: config.rotary_dim as i32,
            linear_key_heads: config.linear_num_key_heads as i32,
            linear_value_heads: config.linear_num_value_heads as i32,
            linear_key_dim: config.linear_key_head_dim as i32,
            linear_value_dim: config.linear_value_head_dim as i32,
            conv_kernel: config.linear_conv_kernel_dim as i32,
            eps: config.rms_norm_eps,
            slots: slots as i32,
            context: context as i32,
            cos: cos.as_ptr(),
            sin: sin.as_ptr(),
        };
        // SAFETY: every pointer is valid for the call; the tensor data stays
        // mapped for the model's life (`_weights`).
        let model = unsafe { (api.load)(&raw, tensors.as_ptr(), tensors.len()) };
        if model.is_null() {
            return Err(api.error());
        }
        // SAFETY: returns a NUL-terminated string owned by the library.
        let gpu = unsafe { CStr::from_ptr((api.device_name)()) }.to_string_lossy().into_owned();
        let mut backend =
            Self { api, model, _weights: weights, config, slots, context, device: format!("MLX · {gpu}") };
        backend.warm_up()?;
        Ok(backend)
    }

    /// The first call compiles the recurrence kernel and MLX's own pipelines;
    /// pay that now rather than in a request.
    fn warm_up(&mut self) -> Result<(), String> {
        let prefix = self.prefix_state(&[0, 1])?;
        let result = self.admit(0, &prefix, &[2]).and_then(|_| self.step(&[(0, 3)]).map(|_| ()));
        self.trim();
        result
    }

    fn vocab(&self) -> usize {
        self.config.vocab_size
    }
}

impl Drop for MlxBackend {
    fn drop(&mut self) {
        // SAFETY: frees the model before the mapping its weights borrow.
        unsafe { (self.api.free)(self.model) };
    }
}

/// NeoX RoPE tables `[context, rotary_dim]`, computed in double precision.
fn rope_table(config: &Config, context: usize) -> (Vec<f32>, Vec<f32>) {
    let rot = config.rotary_dim;
    let half = rot / 2;
    let mut cos = vec![0f32; context * rot];
    let mut sin = vec![0f32; context * rot];
    for pos in 0..context {
        for i in 0..half {
            let inv = config.rope_theta.powf(-(2.0 * i as f64) / rot as f64);
            let (s, c) = (pos as f64 * inv).sin_cos();
            for j in [i, i + half] {
                cos[pos * rot + j] = c as f32;
                sin[pos * rot + j] = s as f32;
            }
        }
    }
    (cos, sin)
}

impl Backend for MlxBackend {
    fn device(&self) -> String {
        self.device.clone()
    }

    fn capacity(&self) -> Capacity {
        Capacity { slots: self.slots, context: self.context }
    }

    fn state_format(&self) -> String {
        format!("mlx/{}", super::weights::FORMAT)
    }

    fn prefix_state(&mut self, tokens: &[u32]) -> Result<PrefixState, String> {
        if tokens.is_empty() || tokens.len() >= self.context {
            return Err("前置提示词长度无效".into());
        }
        let mut out = std::ptr::null_mut();
        let mut len = 0usize;
        // SAFETY: valid pointers; `out` is freed with mwx_free_bytes below.
        self.api.check(unsafe { (self.api.prefix)(self.model, tokens.as_ptr(), tokens.len(), &mut out, &mut len) })?;
        // SAFETY: the library wrote `len` bytes at `out`.
        let bytes = unsafe { std::slice::from_raw_parts(out, len) }.to_vec();
        unsafe { (self.api.free_bytes)(out) };
        Ok(PrefixState { tokens: tokens.len(), format: self.state_format(), bytes: bytes.into() })
    }

    fn admit(&mut self, slot: usize, prefix: &PrefixState, tokens: &[u32]) -> Result<Logits, String> {
        if prefix.format != self.state_format() {
            return Err("前置状态格式不符".into());
        }
        if prefix.tokens + tokens.len() >= self.context {
            return Err("请求超出上下文长度".into());
        }
        let mut logits = vec![0f32; self.vocab()];
        // SAFETY: valid pointers and lengths; `logits` holds `vocab` floats.
        self.api.check(unsafe {
            (self.api.admit)(
                self.model,
                slot as i32,
                prefix.bytes.as_ptr(),
                prefix.bytes.len(),
                prefix.tokens as i32,
                tokens.as_ptr(),
                tokens.len(),
                logits.as_mut_ptr(),
            )
        })?;
        Ok(logits)
    }

    fn step(&mut self, batch: &[(usize, u32)]) -> Result<Vec<Logits>, String> {
        let slots: Vec<i32> = batch.iter().map(|(slot, _)| *slot as i32).collect();
        let tokens: Vec<u32> = batch.iter().map(|(_, token)| *token).collect();
        let vocab = self.vocab();
        let mut logits = vec![0f32; vocab * batch.len()];
        // SAFETY: valid pointers; `logits` holds `n * vocab` floats.
        self.api.check(unsafe {
            (self.api.step)(self.model, slots.as_ptr(), tokens.as_ptr(), batch.len(), logits.as_mut_ptr())
        })?;
        Ok(logits.chunks_exact(vocab).map(<[f32]>::to_vec).collect())
    }

    fn release(&mut self, slot: usize) {
        // SAFETY: plain call on a live model.
        unsafe { (self.api.release)(self.model, slot as i32) };
    }

    fn trim(&mut self) {
        // SAFETY: plain call on a live model.
        unsafe { (self.api.trim)(self.model) };
    }
}

#[cfg(test)]
mod tests;
