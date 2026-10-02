//! The part of llama.cpp's C API the backend uses, behind owned handles.
//!
//! llama.cpp is not linked in: `init` opens the libraries of its release build
//! (`runtime::RELEASE`, unpacked by `runtime`) and looks up each function
//! once. The declarations here follow that release's `llama.h`, `ggml.h`,
//! `ggml-backend.h`, `gguf.h`, and `mtmd.h`/`mtmd-helper.h` for images;
//! `init` refuses a build of any other commit, whose structs may be laid out
//! differently.
//!
//! Every pointer from llama.cpp lives in one of the types here and is freed by
//! its `Drop`; nothing else in the backend calls into llama.cpp. None of these
//! types is `Sync`: llama.cpp allows a model or context on any thread, but not
//! on two at once.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::Path;
use std::ptr::NonNull;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use super::runtime;

/// `ggml_commit()` of the release `runtime` pins (b11074).
const COMMIT: &str = "26394b4";

type DevT = *mut c_void;
type RegT = *mut c_void;
type LogCallback = unsafe extern "C" fn(level: c_int, text: *const c_char, user_data: *mut c_void);

// enum ggml_log_level
const GGML_LOG_LEVEL_WARN: c_int = 3;
const GGML_LOG_LEVEL_ERROR: c_int = 4;
const GGML_LOG_LEVEL_CONT: c_int = 5;
// enum ggml_backend_dev_type
const GGML_BACKEND_DEVICE_TYPE_CPU: c_int = 0;
const GGML_BACKEND_DEVICE_TYPE_GPU: c_int = 1;
const GGML_BACKEND_DEVICE_TYPE_IGPU: c_int = 2;
const GGML_BACKEND_DEVICE_TYPE_ACCEL: c_int = 3;
// enum gguf_type
const GGUF_TYPE_UINT32: c_int = 4;
const GGUF_TYPE_INT32: c_int = 5;
const GGUF_TYPE_STRING: c_int = 8;
const GGUF_TYPE_ARRAY: c_int = 9;

/// `enum llama_load_mode`: memory-map the model file.
pub const LOAD_MODE_MMAP: c_int = 1;
/// `enum llama_split_mode`: one device holds the whole model.
pub const SPLIT_MODE_NONE: c_int = 0;
/// `enum llama_flash_attn_type`: where the device supports it.
pub const FLASH_ATTN_AUTO: c_int = -1;

/// `struct llama_model_params`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ModelParams {
    pub devices: *mut DevT,
    tensor_buft_overrides: *const c_void,
    pub n_gpu_layers: i32,
    pub split_mode: c_int,
    pub load_mode: c_int,
    lazy_mode: c_int,
    pub main_gpu: i32,
    tensor_split: *const f32,
    progress_callback: *const c_void,
    progress_callback_user_data: *mut c_void,
    kv_overrides: *const c_void,
    vocab_only: bool,
    check_tensors: bool,
    pub use_extra_bufts: bool,
    no_host: bool,
    no_alloc: bool,
    pub load_mtp: bool,
}

/// `struct llama_context_params`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ContextParams {
    pub n_ctx: u32,
    pub n_batch: u32,
    pub n_ubatch: u32,
    pub n_seq_max: u32,
    n_rs_seq: u32,
    pub n_outputs_max: u32,
    n_outputs_max_per_seq: u32,
    pub n_threads: i32,
    pub n_threads_batch: i32,
    ctx_type: c_int,
    rope_scaling_type: c_int,
    pooling_type: c_int,
    attention_type: c_int,
    pub flash_attn_type: c_int,
    rope_freq_base: f32,
    rope_freq_scale: f32,
    yarn_ext_factor: f32,
    yarn_attn_factor: f32,
    yarn_beta_fast: f32,
    yarn_beta_slow: f32,
    yarn_orig_ctx: u32,
    defrag_thold: f32,
    cb_eval: *const c_void,
    cb_eval_user_data: *mut c_void,
    type_k: c_int,
    type_v: c_int,
    abort_callback: *const c_void,
    abort_callback_data: *mut c_void,
    embeddings: bool,
    offload_kqv: bool,
    pub no_perf: bool,
    op_offload: bool,
    swa_full: bool,
    pub kv_unified: bool,
    samplers: *mut c_void,
    n_samplers: usize,
    ctx_other: *mut c_void,
}

/// `struct mtmd_context_params`.
#[repr(C)]
#[derive(Clone, Copy)]
struct MtmdContextParams {
    use_gpu: bool,
    device: DevT,
    print_timings: bool,
    n_threads: c_int,
    image_marker: *const c_char,
    media_marker: *const c_char,
    flash_attn_type: c_int,
    warmup: bool,
    image_min_tokens: c_int,
    image_max_tokens: c_int,
    cb_eval: *const c_void,
    cb_eval_user_data: *mut c_void,
    batch_max_tokens: i32,
    progress_callback: *const c_void,
    progress_callback_user_data: *mut c_void,
}

/// `struct mtmd_input_text`.
#[repr(C)]
struct MtmdInputText {
    text: *const c_char,
    text_len: usize,
    add_special: bool,
    parse_special: bool,
}

/// `enum mtmd_input_chunk_type`: an image's tokens.
const MTMD_INPUT_CHUNK_TYPE_IMAGE: c_int = 1;

/// `struct llama_batch`.
#[repr(C)]
struct RawBatch {
    n_tokens: i32,
    token: *mut i32,
    embd: *mut f32,
    pos: *mut i32,
    n_seq_id: *mut i32,
    seq_id: *mut *mut i32,
    logits: *mut i8,
}

/// `struct ggml_backend_dev_caps`.
#[repr(C)]
struct DevCaps {
    async_: bool,
    host_buffer: bool,
    buffer_from_host_ptr: bool,
    events: bool,
    mmap_support: bool,
}

/// `struct ggml_backend_dev_props`.
#[repr(C)]
struct DevProps {
    name: *const c_char,
    description: *const c_char,
    memory_free: usize,
    memory_total: usize,
    type_: c_int,
    device_id: *const c_char,
    caps: DevCaps,
}

/// `struct ggml_init_params`.
#[repr(C)]
struct GgmlInitParams {
    mem_size: usize,
    mem_buffer: *mut c_void,
    no_alloc: bool,
}

/// `struct gguf_init_params`.
#[repr(C)]
struct GgufInitParams {
    no_alloc: bool,
    ctx: *mut *mut c_void,
}

/// Declares `Api`, one function pointer per entry, and `Api::load`, which
/// finds each in the first library that exports it.
macro_rules! api {
    ($($name:ident: fn($($arg:ty),*) $(-> $ret:ty)?;)*) => {
        #[allow(non_snake_case)]
        struct Api {
            $($name: unsafe extern "C" fn($($arg),*) $(-> $ret)?,)*
            /// Kept open for the life of the process: the pointers above point into them.
            _libraries: Vec<libloading::Library>,
        }

        impl Api {
            fn load(libraries: Vec<libloading::Library>) -> Result<Self, String> {
                Ok(Self {
                    $($name: {
                        let name = concat!(stringify!($name), "\0").as_bytes();
                        // SAFETY: the declared type is the function's signature in the pinned release.
                        libraries
                            .iter()
                            .find_map(|library| unsafe { library.get::<unsafe extern "C" fn($($arg),*) $(-> $ret)?>(name) }.ok().map(|symbol| *symbol))
                            .ok_or_else(|| format!("llama.cpp 运行库缺少函数 {}", stringify!($name)))?
                    },)*
                    _libraries: libraries,
                })
            }
        }
    };
}

api! {
    llama_log_set: fn(Option<LogCallback>, *mut c_void);
    llama_model_default_params: fn() -> ModelParams;
    llama_context_default_params: fn() -> ContextParams;
    llama_model_load_from_file: fn(*const c_char, ModelParams) -> *mut c_void;
    llama_model_get_vocab: fn(*const c_void) -> *const c_void;
    llama_vocab_n_tokens: fn(*const c_void) -> i32;
    llama_model_size: fn(*const c_void) -> u64;
    llama_model_free: fn(*mut c_void);
    llama_init_from_model: fn(*mut c_void, ContextParams) -> *mut c_void;
    llama_get_memory: fn(*const c_void) -> *mut c_void;
    llama_n_batch: fn(*const c_void) -> u32;
    llama_n_ctx_seq: fn(*const c_void) -> u32;
    llama_decode: fn(*mut c_void, RawBatch) -> i32;
    llama_get_logits_ith: fn(*mut c_void, i32) -> *mut f32;
    llama_memory_seq_rm: fn(*mut c_void, i32, i32, i32) -> bool;
    llama_memory_seq_pos_max: fn(*mut c_void, i32) -> i32;
    llama_state_seq_get_size: fn(*mut c_void, i32) -> usize;
    llama_state_seq_get_data: fn(*mut c_void, *mut u8, usize, i32) -> usize;
    llama_state_seq_set_data: fn(*mut c_void, *const u8, usize, i32) -> usize;
    llama_free: fn(*mut c_void);
    llama_version: fn() -> *const c_char;
    ggml_version: fn() -> *const c_char;
    ggml_commit: fn() -> *const c_char;
    ggml_time_init: fn();
    ggml_init: fn(GgmlInitParams) -> *mut c_void;
    ggml_free: fn(*mut c_void);
    ggml_backend_load_all_from_path: fn(*const c_char);
    ggml_backend_dev_count: fn() -> usize;
    ggml_backend_dev_get: fn(usize) -> DevT;
    ggml_backend_dev_get_props: fn(DevT, *mut DevProps);
    ggml_backend_dev_backend_reg: fn(DevT) -> RegT;
    ggml_backend_reg_name: fn(RegT) -> *const c_char;
    gguf_init_from_file: fn(*const c_char, GgufInitParams) -> *mut c_void;
    gguf_find_key: fn(*const c_void, *const c_char) -> i64;
    gguf_get_kv_type: fn(*const c_void, i64) -> c_int;
    gguf_get_val_u32: fn(*const c_void, i64) -> u32;
    gguf_get_val_i32: fn(*const c_void, i64) -> i32;
    gguf_get_val_str: fn(*const c_void, i64) -> *const c_char;
    gguf_get_arr_n: fn(*const c_void, i64) -> usize;
    gguf_free: fn(*mut c_void);
    mtmd_log_set: fn(Option<LogCallback>, *mut c_void);
    mtmd_helper_log_set: fn(Option<LogCallback>, *mut c_void);
    mtmd_default_marker: fn() -> *const c_char;
    mtmd_context_params_default: fn() -> MtmdContextParams;
    mtmd_init_from_file: fn(*const c_char, *const c_void, MtmdContextParams) -> *mut c_void;
    mtmd_free: fn(*mut c_void);
    mtmd_support_vision: fn(*const c_void) -> bool;
    mtmd_bitmap_init: fn(u32, u32, *const u8) -> *mut c_void;
    mtmd_bitmap_free: fn(*mut c_void);
    mtmd_input_chunks_init: fn() -> *mut c_void;
    mtmd_input_chunks_free: fn(*mut c_void);
    mtmd_input_chunks_size: fn(*const c_void) -> usize;
    mtmd_input_chunks_get: fn(*const c_void, usize) -> *const c_void;
    mtmd_input_chunk_get_type: fn(*const c_void) -> c_int;
    mtmd_input_chunk_get_n_tokens: fn(*const c_void) -> usize;
    mtmd_tokenize: fn(*const c_void, *mut c_void, *const MtmdInputText, *const *const c_void, usize) -> i32;
    mtmd_helper_eval_chunk_single: fn(*mut c_void, *mut c_void, *const c_void, i32, i32, i32, bool, *mut i32) -> i32;
}

/// The loaded runtime; set by the first `init` that succeeds.
static API: OnceLock<Api> = OnceLock::new();

fn api() -> &'static Api {
    API.get().expect("llama.cpp called before ffi::init succeeded")
}

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

/// Opens the runtime in `dir` (`None`: the executable's directory), registers
/// the log callback and loads the ggml backends, once per process: after a
/// success, later calls return at once whatever `dir` they name. A failure
/// closes what it opened, so a later call tries again (once the runtime is
/// reinstalled, say).
pub fn init(dir: Option<&Path>) -> Result<(), String> {
    static OPENING: Mutex<()> = Mutex::new(());
    let _opening = lock(&OPENING);
    if API.get().is_some() {
        return Ok(());
    }
    let dir = dir
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)))
        .unwrap_or_default();
    let _ = API.set(open(&dir)?);
    Ok(())
}

fn open(dir: &Path) -> Result<Api, String> {
    check_cpp_runtime()?;
    let mut libraries = runtime::LIBRARIES
        .iter()
        .map(|name| open_library(&dir.join(name)))
        .collect::<Result<Vec<_>, _>>()?;
    libraries.extend(vulkan_loader(dir));
    let api = Api::load(libraries)?;
    // SAFETY: static strings.
    let commit = unsafe { string((api.ggml_commit)()) };
    if !commit.starts_with(COMMIT) {
        return Err(format!("llama.cpp 运行库不是 {}（提交 {commit}），与本程序不兼容", runtime::RELEASE));
    }
    // SAFETY: `log` is a valid callback for the life of the process.
    unsafe {
        (api.llama_log_set)(Some(log), std::ptr::null_mut());
        (api.mtmd_log_set)(Some(log), std::ptr::null_mut());
        (api.mtmd_helper_log_set)(Some(log), std::ptr::null_mut());
    }
    // What llama_backend_init does, except that with no backend registered it
    // would search the build machine's backend directory, the executable's
    // directory and the working directory; the working directory is no place
    // to load code from. The context only fills ggml's f16 tables.
    // SAFETY: plain initialization calls.
    unsafe {
        (api.ggml_time_init)();
        let context = (api.ggml_init)(GgmlInitParams { mem_size: 0, mem_buffer: std::ptr::null_mut(), no_alloc: false });
        if !context.is_null() {
            (api.ggml_free)(context);
        }
    }
    // Every backend, the CPU ones included, is a module of the release build.
    // ggml loads each one it finds in `dir`, picks the CPU variant that suits
    // this processor, and skips a GPU module whose runtime (vulkan-1) is missing.
    let c_dir = CString::new(path_text(dir)?).map_err(|_| format!("路径含有空字符：{}", dir.display()))?;
    // SAFETY: a NUL-terminated UTF-8 path, read during the call.
    unsafe { (api.ggml_backend_load_all_from_path)(c_dir.as_ptr()) };
    Ok(api)
}

/// Opens one library of the runtime; the ones it depends on are found beside it.
#[cfg(windows)]
fn open_library(path: &Path) -> Result<libloading::Library, String> {
    use libloading::os::windows::{Library, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR};
    // The library's own directory and the system's, never the working directory or PATH.
    // SAFETY: the release's libraries run no code on load beyond their C++ static initializers.
    unsafe { Library::load_with_flags(path, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS) }
        .map(Into::into)
        .map_err(|error| format!("无法加载 llama.cpp 运行库 {}: {error}", path.display()))
}

/// Vulkan's loader, which ggml's Vulkan module imports, opened first so that
/// the import resolves to it: the system's, which GPU drivers normally
/// install, unless it predates Vulkan 1.1 (the module's floor), or else
/// Khronos' loader unpacked with the runtime. With neither, ggml's own
/// `LoadLibraryW` of the module looks for one the classic way and, finding
/// none, skips the module: the model runs on the CPU.
#[cfg(windows)]
fn vulkan_loader(dir: &Path) -> Option<libloading::Library> {
    use libloading::os::windows::{Library, LOAD_LIBRARY_SEARCH_SYSTEM32};
    // SAFETY: looks up an address without calling it.
    let usable =
        |library: &libloading::Library| unsafe { library.get::<unsafe extern "C" fn()>(b"vkGetPhysicalDeviceFeatures2\0") }.is_ok();
    // SAFETY: the Vulkan loader runs no code on load beyond its static initializers.
    let system = unsafe { Library::load_with_flags(runtime::VULKAN_LOADER, LOAD_LIBRARY_SEARCH_SYSTEM32) };
    let bundled = dir.join(runtime::VULKAN_LOADER);
    system
        .ok()
        .map(libloading::Library::from)
        .filter(usable)
        .or_else(|| bundled.exists().then(|| open_library(&bundled).ok()).flatten())
}

/// The oldest Microsoft C++ runtime (`msvcp140.dll`) the release build runs
/// on: it is built with Visual Studio 2022 17.10 or later, whose `std::mutex`
/// an older runtime's `_Mtx_lock` crashes on, taking the app down with it, and
/// it brings no runtime of its own.
#[cfg(windows)]
const CPP_RUNTIME: (u32, u32) = (14, 40);

/// Refuses to load the release build on a C++ runtime it would crash on.
#[cfg(windows)]
fn check_cpp_runtime() -> Result<(), String> {
    let redist = if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" };
    let redist = format!("https://aka.ms/vs/17/release/vc_redist.{redist}.exe");
    match cpp_runtime_path().as_deref().and_then(file_version) {
        Some(version) if version >= CPP_RUNTIME => Ok(()),
        Some((major, minor)) => Err(format!(
            "Microsoft Visual C++ 运行库版本过旧（{major}.{minor}），本地模型需要 {}.{} 或更高，请安装最新的 Visual C++ 可再发行程序包：{redist}",
            CPP_RUNTIME.0, CPP_RUNTIME.1
        )),
        None => Err(format!("找不到 Microsoft Visual C++ 运行库（msvcp140.dll），请安装 Visual C++ 可再发行程序包：{redist}")),
    }
}

#[cfg(not(windows))]
fn check_cpp_runtime() -> Result<(), String> {
    Ok(())
}

/// The `msvcp140.dll` the runtime's libraries bind to: the copy already loaded
/// in this process if any, else the system's.
#[cfg(windows)]
fn cpp_runtime_path() -> Option<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW};
    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
    let name: Vec<u16> = "msvcp140.dll".encode_utf16().chain(Some(0)).collect();
    let mut buffer = vec![0u16; 32_768];
    let capacity = buffer.len() as u32;
    // SAFETY: a NUL-terminated name, and a buffer of the capacity given.
    let (len, loaded) = unsafe {
        let module = GetModuleHandleW(name.as_ptr());
        if module.is_null() {
            (GetSystemDirectoryW(buffer.as_mut_ptr(), capacity), false)
        } else {
            (GetModuleFileNameW(module, buffer.as_mut_ptr(), capacity), true)
        }
    };
    if len == 0 || len >= capacity {
        return None;
    }
    let path = std::path::PathBuf::from(std::ffi::OsString::from_wide(&buffer[..len as usize]));
    Some(if loaded { path } else { path.join("msvcp140.dll") })
}

/// (major, minor) of a file's version resource.
#[cfg(windows)]
fn file_version(path: &Path) -> Option<(u32, u32)> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW, VS_FIXEDFILEINFO,
    };
    let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let root: Vec<u16> = "\\".encode_utf16().chain(Some(0)).collect();
    // SAFETY: NUL-terminated strings; the version block is as large as asked
    // for, and VerQueryValueW points into it for at least `len` bytes.
    unsafe {
        let mut handle = 0;
        let size = GetFileVersionInfoSizeW(name.as_ptr(), &mut handle);
        if size == 0 {
            return None;
        }
        let mut block = vec![0u8; size as usize];
        if GetFileVersionInfoW(name.as_ptr(), 0, size, block.as_mut_ptr().cast()) == 0 {
            return None;
        }
        let (mut info, mut len) = (std::ptr::null_mut::<c_void>(), 0u32);
        if VerQueryValueW(block.as_ptr().cast(), root.as_ptr(), &mut info, &mut len) == 0
            || info.is_null()
            || (len as usize) < std::mem::size_of::<VS_FIXEDFILEINFO>()
        {
            return None;
        }
        let fixed = std::ptr::read_unaligned(info as *const VS_FIXEDFILEINFO);
        Some((fixed.dwFileVersionMS >> 16, fixed.dwFileVersionMS & 0xffff))
    }
}

/// Linux finds `libvulkan.so.1` in the system's library path, where the
/// distribution's Vulkan package puts it.
#[cfg(not(windows))]
fn vulkan_loader(_dir: &Path) -> Option<libloading::Library> {
    None
}

#[cfg(not(windows))]
fn open_library(path: &Path) -> Result<libloading::Library, String> {
    // SAFETY: as above.
    unsafe { libloading::Library::new(path) }.map_err(|error| format!("无法加载 llama.cpp 运行库 {}: {error}", path.display()))
}

unsafe extern "C" fn log(level: c_int, text: *const c_char, _user_data: *mut c_void) {
    if text.is_null() {
        return;
    }
    // SAFETY: llama.cpp passes a NUL-terminated message.
    let text = unsafe { CStr::from_ptr(text) }.to_string_lossy();
    let is_error = match level {
        GGML_LOG_LEVEL_ERROR => true,
        GGML_LOG_LEVEL_CONT => *lock(&LAST_WAS_ERROR),
        _ => false,
    };
    if level != GGML_LOG_LEVEL_CONT {
        *lock(&LAST_WAS_ERROR) = is_error;
    }
    if is_error {
        let mut last = lock(&LAST_ERROR);
        if level != GGML_LOG_LEVEL_CONT {
            last.clear();
        }
        last.push_str(&text);
    }
    // Nothing reaches stdout. Debug builds show warnings and errors on stderr;
    // MEWORK_LLAMA_LOG=1 shows everything llama.cpp says (buffer sizes, devices).
    static VERBOSE: OnceLock<bool> = OnceLock::new();
    let verbose = *VERBOSE.get_or_init(|| std::env::var_os("MEWORK_LLAMA_LOG").is_some_and(|value| value != "0"));
    if verbose || (cfg!(debug_assertions) && (is_error || level == GGML_LOG_LEVEL_WARN)) {
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

/// # Safety
/// `ptr` is null or a NUL-terminated string that outlives the call.
unsafe fn string(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    // SAFETY: per the contract.
    unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned()
}

/// "llama <version> · ggml <version> <commit>" of the loaded runtime.
pub fn library_identity() -> String {
    let api = api();
    // SAFETY: static strings.
    unsafe {
        format!(
            "llama {} · ggml {} {}",
            string((api.llama_version)()),
            string((api.ggml_version)()),
            string((api.ggml_commit)())
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
    raw: DevT,
    /// Registry name: "CPU", "Vulkan", …
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
    let api = api();
    // SAFETY: registry enumeration after `init`; every handle returned is valid.
    unsafe {
        (0..(api.ggml_backend_dev_count)())
            .filter_map(|index| {
                let raw = (api.ggml_backend_dev_get)(index);
                if raw.is_null() {
                    return None;
                }
                let mut props: DevProps = std::mem::zeroed();
                (api.ggml_backend_dev_get_props)(raw, &mut props);
                let reg = (api.ggml_backend_dev_backend_reg)(raw);
                let kind = match props.type_ {
                    GGML_BACKEND_DEVICE_TYPE_CPU => DeviceKind::Cpu,
                    GGML_BACKEND_DEVICE_TYPE_GPU => DeviceKind::Gpu,
                    GGML_BACKEND_DEVICE_TYPE_IGPU => DeviceKind::IntegratedGpu,
                    GGML_BACKEND_DEVICE_TYPE_ACCEL => DeviceKind::Accelerator,
                    _ => DeviceKind::Other,
                };
                Some(Device {
                    raw,
                    backend: if reg.is_null() { String::new() } else { string((api.ggml_backend_reg_name)(reg)) },
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
pub struct GgufMetadata(NonNull<c_void>);

impl GgufMetadata {
    pub fn read(path: &Path) -> Result<Self, String> {
        let c_path = c_path(path)?;
        let params = GgufInitParams { no_alloc: true, ctx: std::ptr::null_mut() };
        clear_last_error();
        // SAFETY: a NUL-terminated path; with `no_alloc` only metadata is read.
        let raw = unsafe { (api().gguf_init_from_file)(c_path.as_ptr(), params) };
        NonNull::new(raw).map(Self).ok_or_else(|| with_last_error(format!("无法读取 GGUF 文件 {}", path.display())))
    }

    fn key(&self, key: &str) -> Option<i64> {
        let key = CString::new(key).ok()?;
        // SAFETY: valid context and NUL-terminated key.
        let id = unsafe { (api().gguf_find_key)(self.0.as_ptr(), key.as_ptr()) };
        (id >= 0).then_some(id)
    }

    fn kv_type(&self, id: i64) -> c_int {
        // SAFETY: `id` was found in this context.
        unsafe { (api().gguf_get_kv_type)(self.0.as_ptr(), id) }
    }

    pub fn string(&self, key: &str) -> Option<String> {
        let id = self.key(key)?;
        // SAFETY: the type is checked before reading.
        (self.kv_type(id) == GGUF_TYPE_STRING).then(|| unsafe { string((api().gguf_get_val_str)(self.0.as_ptr(), id)) })
    }

    /// An unsigned or signed 32-bit value.
    pub fn u32(&self, key: &str) -> Option<u32> {
        let id = self.key(key)?;
        // SAFETY: as above.
        unsafe {
            match self.kv_type(id) {
                GGUF_TYPE_UINT32 => Some((api().gguf_get_val_u32)(self.0.as_ptr(), id)),
                GGUF_TYPE_INT32 => u32::try_from((api().gguf_get_val_i32)(self.0.as_ptr(), id)).ok(),
                _ => None,
            }
        }
    }

    /// Length of an array value.
    pub fn array_len(&self, key: &str) -> Option<usize> {
        let id = self.key(key)?;
        // SAFETY: as above.
        (self.kv_type(id) == GGUF_TYPE_ARRAY).then(|| unsafe { (api().gguf_get_arr_n)(self.0.as_ptr(), id) })
    }
}

impl Drop for GgufMetadata {
    fn drop(&mut self) {
        // SAFETY: owned context, freed once.
        unsafe { (api().gguf_free)(self.0.as_ptr()) }
    }
}

fn path_text(path: &Path) -> Result<&str, String> {
    path.to_str().ok_or_else(|| format!("路径不是有效的 UTF-8：{}", path.display()))
}

fn c_path(path: &Path) -> Result<CString, String> {
    // llama.cpp opens paths with fopen, which takes UTF-8 on every platform it
    // supports (it converts to UTF-16 on Windows).
    CString::new(path_text(path)?).map_err(|_| format!("模型路径含有空字符：{}", path.display()))
}

pub fn default_model_params() -> ModelParams {
    // SAFETY: returns a value.
    unsafe { (api().llama_model_default_params)() }
}

pub fn default_context_params() -> ContextParams {
    // SAFETY: returns a value.
    unsafe { (api().llama_context_default_params)() }
}

pub struct Model {
    raw: NonNull<c_void>,
    n_vocab: usize,
}

// SAFETY: a model is immutable after loading and used from one thread at a time.
unsafe impl Send for Model {}

impl Model {
    /// Loads `path` on `devices` (none: CPU only). `params.devices` is set here.
    pub fn load(path: &Path, devices: &[Device], mut params: ModelParams) -> Result<Self, String> {
        let api = api();
        let c_path = c_path(path)?;
        // NULL-terminated; an empty list keeps every layer on the CPU.
        let mut list: Vec<DevT> = devices.iter().map(|device| device.raw).collect();
        list.push(std::ptr::null_mut());
        params.devices = list.as_mut_ptr();
        clear_last_error();
        // SAFETY: the path and device list outlive the call, which copies what it keeps.
        let raw = unsafe { (api.llama_model_load_from_file)(c_path.as_ptr(), params) };
        let raw =
            NonNull::new(raw).ok_or_else(|| with_last_error(format!("llama.cpp 无法加载模型 {}", path.display())))?;
        // SAFETY: a loaded model always has a vocabulary.
        let n_vocab = unsafe { (api.llama_vocab_n_tokens)((api.llama_model_get_vocab)(raw.as_ptr())) };
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
        unsafe { (api().llama_model_size)(self.raw.as_ptr()) }
    }
}

impl Drop for Model {
    fn drop(&mut self) {
        // SAFETY: owned model; every context on it was freed first (see `LlamaBackend`).
        unsafe { (api().llama_model_free)(self.raw.as_ptr()) }
    }
}

/// A context on a model: the KV caches and recurrent states of its sequences
/// and the compute buffers. It must be dropped before its model.
pub struct Context {
    raw: NonNull<c_void>,
    memory: *mut c_void,
    n_vocab: usize,
}

// SAFETY: used from one thread at a time, like the model.
unsafe impl Send for Context {}

impl Context {
    pub fn new(model: &Model, params: ContextParams) -> Result<Self, String> {
        let api = api();
        clear_last_error();
        // SAFETY: valid model; params is a plain value.
        let raw = unsafe { (api.llama_init_from_model)(model.raw.as_ptr(), params) };
        let raw = NonNull::new(raw).ok_or_else(|| with_last_error("llama.cpp 无法创建推理上下文".into()))?;
        // SAFETY: valid context; a decoder context always has memory.
        let memory = unsafe { (api.llama_get_memory)(raw.as_ptr()) };
        let context = Self { raw, memory, n_vocab: model.n_vocab };
        if memory.is_null() {
            return Err("llama.cpp 上下文没有 KV 缓存".into());
        }
        Ok(context)
    }

    pub fn n_batch(&self) -> usize {
        // SAFETY: valid context.
        unsafe { (api().llama_n_batch)(self.raw.as_ptr()) as usize }
    }

    pub fn n_ctx_seq(&self) -> usize {
        // SAFETY: valid context.
        unsafe { (api().llama_n_ctx_seq)(self.raw.as_ptr()) as usize }
    }

    /// Runs `batch`. Its tokens must fit `n_batch`.
    pub fn decode(&mut self, batch: &mut Batch) -> Result<(), String> {
        if batch.len() == 0 {
            return Ok(());
        }
        clear_last_error();
        // SAFETY: the batch's arrays are alive and all `len()` long.
        let status = unsafe { (api().llama_decode)(self.raw.as_ptr(), batch.raw()) };
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
            let ptr = (api().llama_get_logits_ith)(self.raw.as_ptr(), index);
            if ptr.is_null() {
                return Err(with_last_error("llama.cpp 没有返回 logits".into()));
            }
            Ok(std::slice::from_raw_parts(ptr, self.n_vocab).to_vec())
        }
    }

    /// Drops everything stored for `seq`.
    pub fn seq_remove(&mut self, seq: i32) {
        // SAFETY: valid memory; removing a whole sequence never fails.
        unsafe { (api().llama_memory_seq_rm)(self.memory, seq, -1, -1) };
    }

    /// Largest position stored for `seq`, or -1 when it is empty.
    pub fn seq_pos_max(&self, seq: i32) -> i32 {
        // SAFETY: valid memory.
        unsafe { (api().llama_memory_seq_pos_max)(self.memory, seq) }
    }

    /// `seq`'s KV cache and recurrent state, serialized.
    pub fn seq_state(&mut self, seq: i32) -> Result<Vec<u8>, String> {
        let api = api();
        // SAFETY: valid context; the buffer is as large as llama.cpp asked for.
        unsafe {
            let size = (api.llama_state_seq_get_size)(self.raw.as_ptr(), seq);
            let mut bytes = vec![0u8; size];
            let written = (api.llama_state_seq_get_data)(self.raw.as_ptr(), bytes.as_mut_ptr(), bytes.len(), seq);
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
        let read = unsafe { (api().llama_state_seq_set_data)(self.raw.as_ptr(), bytes.as_ptr(), bytes.len(), seq) };
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
        unsafe { (api().llama_free)(self.raw.as_ptr()) }
    }
}

/// The vision projector (`mtmd`): encodes an image and decodes its rows into
/// a sequence of a context on the same model. It must be dropped before its
/// model.
pub struct Projector(NonNull<c_void>);

// SAFETY: used from one thread at a time, like the model.
unsafe impl Send for Projector {}

impl Projector {
    /// Loads the projector file `path` for `model`, on `device` (none: the
    /// CPU). Images are taken at the size given, between `min_tokens` and
    /// `max_tokens` tokens; llama.cpp would resize any other.
    pub fn load(
        path: &Path,
        model: &Model,
        device: Option<&Device>,
        threads: usize,
        min_tokens: usize,
        max_tokens: usize,
    ) -> Result<Self, String> {
        let api = api();
        let c_path = c_path(path)?;
        // SAFETY: returns a value.
        let mut params = unsafe { (api.mtmd_context_params_default)() };
        params.use_gpu = device.is_some();
        params.device = device.map_or(std::ptr::null_mut(), |device| device.raw);
        params.print_timings = false;
        params.n_threads = threads.max(1) as c_int;
        params.flash_attn_type = FLASH_ATTN_AUTO;
        // The warm-up encodes a 2,116-token image to size the buffers: seconds
        // on a CPU, for images this app never sends. They grow on first use.
        params.warmup = false;
        params.image_min_tokens = min_tokens as c_int;
        params.image_max_tokens = max_tokens as c_int;
        clear_last_error();
        // SAFETY: the path outlives the call; the model outlives the projector.
        let raw = unsafe { (api.mtmd_init_from_file)(c_path.as_ptr(), model.raw.as_ptr(), params) };
        let projector = Self(
            NonNull::new(raw).ok_or_else(|| with_last_error(format!("llama.cpp 无法加载视觉投影 {}", path.display())))?,
        );
        // SAFETY: valid context.
        if !unsafe { (api.mtmd_support_vision)(projector.0.as_ptr()) } {
            return Err("视觉投影文件不支持图片".into());
        }
        Ok(projector)
    }

    /// Encodes `rgb` (`width`×`height`, sides multiples of the merge size) and
    /// decodes its `tokens` rows into `seq` of `context` from rotary position
    /// `position`, `n_batch` at a time. Returns the position after the image.
    #[allow(clippy::too_many_arguments)]
    pub fn eval(
        &mut self,
        context: &mut Context,
        width: usize,
        height: usize,
        rgb: &[u8],
        tokens: usize,
        position: usize,
        seq: i32,
        n_batch: usize,
    ) -> Result<usize, String> {
        let api = api();
        if rgb.len() != width * height * 3 {
            return Err("图片尺寸与像素数据不符".into());
        }
        clear_last_error();
        // SAFETY: llama.cpp copies the pixels; both handles are freed below on every path.
        unsafe {
            let bitmap = (api.mtmd_bitmap_init)(width as u32, height as u32, rgb.as_ptr());
            if bitmap.is_null() {
                return Err("llama.cpp 无法读取图片".into());
            }
            let chunks = (api.mtmd_input_chunks_init)();
            let result = (|| {
                // The marker alone: the chunks are `<|vision_start|>`, the image and
                // `<|vision_end|>`; the request carries those two tokens itself.
                let marker = (api.mtmd_default_marker)();
                let text = MtmdInputText {
                    text: marker,
                    text_len: CStr::from_ptr(marker).to_bytes().len(),
                    add_special: false,
                    parse_special: true,
                };
                let bitmaps = [bitmap as *const c_void];
                let status = (api.mtmd_tokenize)(self.0.as_ptr(), chunks, &text, bitmaps.as_ptr(), 1);
                if status != 0 {
                    return Err(with_last_error(format!("llama.cpp 无法处理图片（{status}）")));
                }
                let image = (0..(api.mtmd_input_chunks_size)(chunks))
                    .map(|i| (api.mtmd_input_chunks_get)(chunks, i))
                    .find(|chunk| (api.mtmd_input_chunk_get_type)(*chunk) == MTMD_INPUT_CHUNK_TYPE_IMAGE)
                    .ok_or("llama.cpp 没有得到图片的词元")?;
                let produced = (api.mtmd_input_chunk_get_n_tokens)(image);
                if produced != tokens {
                    return Err(format!("llama.cpp 把图片变成了 {produced} 个词元，应为 {tokens} 个"));
                }
                let mut next = 0i32;
                let status = (api.mtmd_helper_eval_chunk_single)(
                    self.0.as_ptr(),
                    context.raw.as_ptr(),
                    image,
                    position as i32,
                    seq,
                    n_batch as i32,
                    false,
                    &mut next,
                );
                if status != 0 {
                    return Err(with_last_error(format!("llama.cpp 读图失败（{status}）")));
                }
                Ok(next as usize)
            })();
            (api.mtmd_input_chunks_free)(chunks);
            (api.mtmd_bitmap_free)(bitmap);
            result
        }
    }
}

impl Drop for Projector {
    fn drop(&mut self) {
        // SAFETY: owned context, freed once, before its model.
        unsafe { (api().mtmd_free)(self.0.as_ptr()) }
    }
}

/// Tokens for one `llama_decode`, each in one sequence.
#[derive(Default)]
pub struct Batch {
    tokens: Vec<i32>,
    positions: Vec<i32>,
    n_seq_id: Vec<i32>,
    seq_ids: Vec<i32>,
    seq_id_ptrs: Vec<*mut i32>,
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
        self.tokens.push(token as i32);
        self.positions.push(position as i32);
        self.n_seq_id.push(1);
        self.seq_ids.push(seq);
        self.outputs.push(output as i8);
    }

    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    fn raw(&mut self) -> RawBatch {
        // Pointers into `seq_ids`, taken now that it no longer grows.
        self.seq_id_ptrs.clear();
        let base = self.seq_ids.as_mut_ptr();
        // SAFETY: every index is within `seq_ids`.
        self.seq_id_ptrs.extend((0..self.seq_ids.len()).map(|i| unsafe { base.add(i) }));
        RawBatch {
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

#[cfg(all(test, windows))]
mod cpp_runtime {
    use super::*;

    #[test]
    fn reads_a_file_version() {
        let system = cpp_runtime_path().and_then(|path| path.parent().map(Path::to_path_buf)).unwrap();
        // This test runs on the C runtime of the same redistributable. (Not
        // kernel32: Windows reports its own files as 6.2 to an unmanifested process.)
        assert_eq!(file_version(&system.join("vcruntime140.dll")).map(|version| version.0), Some(14));
        assert_eq!(file_version(&system.join("no-such-file.dll")), None);
        // Wherever the C++ runtime is installed, it is 14.x.
        if let Some(version) = cpp_runtime_path().as_deref().and_then(file_version) {
            assert_eq!(version.0, 14, "{version:?}");
        }
    }
}

#[cfg(test)]
mod layout {
    //! Sizes and offsets of the structs passed by value, as a C compiler lays
    //! them out from the pinned release's headers (x86_64 and aarch64 alike).

    use std::mem::{align_of, offset_of, size_of};

    use super::*;

    #[test]
    fn matches_the_release_headers() {
        assert_eq!((size_of::<ModelParams>(), align_of::<ModelParams>()), (80, 8));
        assert_eq!(offset_of!(ModelParams, n_gpu_layers), 16);
        assert_eq!(offset_of!(ModelParams, lazy_mode), 28);
        assert_eq!(offset_of!(ModelParams, main_gpu), 32);
        assert_eq!(offset_of!(ModelParams, kv_overrides), 64);
        assert_eq!(offset_of!(ModelParams, vocab_only), 72);
        assert_eq!(offset_of!(ModelParams, load_mtp), 77);

        assert_eq!((size_of::<ContextParams>(), align_of::<ContextParams>()), (160, 8));
        assert_eq!(offset_of!(ContextParams, n_seq_max), 12);
        assert_eq!(offset_of!(ContextParams, n_outputs_max), 20);
        assert_eq!(offset_of!(ContextParams, n_threads), 28);
        assert_eq!(offset_of!(ContextParams, ctx_type), 36);
        assert_eq!(offset_of!(ContextParams, flash_attn_type), 52);
        assert_eq!(offset_of!(ContextParams, rope_freq_base), 56);
        assert_eq!(offset_of!(ContextParams, yarn_orig_ctx), 80);
        assert_eq!(offset_of!(ContextParams, cb_eval), 88);
        assert_eq!(offset_of!(ContextParams, type_k), 104);
        assert_eq!(offset_of!(ContextParams, abort_callback), 112);
        assert_eq!(offset_of!(ContextParams, embeddings), 128);
        assert_eq!(offset_of!(ContextParams, kv_unified), 133);
        assert_eq!(offset_of!(ContextParams, samplers), 136);
        assert_eq!(offset_of!(ContextParams, ctx_other), 152);

        assert_eq!(size_of::<RawBatch>(), 56);
        assert_eq!(offset_of!(RawBatch, token), 8);
        assert_eq!(offset_of!(RawBatch, logits), 48);

        assert_eq!(size_of::<DevProps>(), 56);
        assert_eq!(offset_of!(DevProps, memory_free), 16);
        assert_eq!(offset_of!(DevProps, type_), 32);
        assert_eq!(offset_of!(DevProps, device_id), 40);
        assert_eq!(offset_of!(DevProps, caps), 48);
        assert_eq!(size_of::<DevCaps>(), 5);

        assert_eq!((size_of::<GgmlInitParams>(), offset_of!(GgmlInitParams, no_alloc)), (24, 16));

        assert_eq!((size_of::<MtmdContextParams>(), align_of::<MtmdContextParams>()), (96, 8));
        assert_eq!(offset_of!(MtmdContextParams, n_threads), 20);
        assert_eq!(offset_of!(MtmdContextParams, flash_attn_type), 40);
        assert_eq!(offset_of!(MtmdContextParams, warmup), 44);
        assert_eq!(offset_of!(MtmdContextParams, image_max_tokens), 52);
        assert_eq!(offset_of!(MtmdContextParams, batch_max_tokens), 72);
        assert_eq!(offset_of!(MtmdContextParams, progress_callback_user_data), 88);
        assert_eq!((size_of::<MtmdInputText>(), offset_of!(MtmdInputText, parse_special)), (24, 17));
        assert_eq!((size_of::<GgufInitParams>(), offset_of!(GgufInitParams, ctx)), (16, 8));
    }
}
