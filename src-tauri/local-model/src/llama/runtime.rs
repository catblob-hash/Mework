//! llama.cpp's official release build, which the backend runs on instead of a
//! llama.cpp compiled into the app: the archives this platform downloads, pinned
//! by SHA-256, and their unpacking. The Vulkan build carries every CPU variant
//! (SSE4.2 to AVX-512) and a Vulkan backend for any GPU vendor, each a ggml
//! module picked at run time. On Windows x64 a second archive brings Khronos'
//! Vulkan loader (`vulkan-1.dll`) for systems whose GPU driver installed none;
//! `ffi` prefers the system's. Libraries are unpacked once per release and never
//! overwritten: they may be loaded in this process, and Windows refuses to
//! replace a loaded file.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// The llama.cpp release whose C API `ffi` declares, and whose build the backend loads.
pub const RELEASE: &str = "b11074";

/// What's inside a pinned archive, and how `install` filters and places its entries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Contents {
    /// llama.cpp's release build: its libraries and ggml backend modules.
    Llama,
    /// Khronos' Vulkan loader for Windows x64 (LunarG's runtime components), for systems whose GPU driver installed none.
    VulkanLoader,
}

/// A pinned release archive.
#[derive(Clone, Copy, Debug)]
pub struct Archive {
    /// File name in the release, e.g. "llama-b11074-bin-win-vulkan-x64.zip".
    pub name: &'static str,
    /// Where it's published.
    pub url: &'static str,
    /// Size in bytes, as published.
    pub size: u64,
    /// SHA-256 of the archive, as published.
    pub sha256: &'static str,
    /// What's inside.
    pub contents: Contents,
}

/// Windows x86_64: Vulkan build (covers every GPU vendor) plus every CPU variant.
#[allow(dead_code)]
const WINDOWS_X64: Archive = Archive {
    name: "llama-b11074-bin-win-vulkan-x64.zip",
    url: "https://github.com/ggml-org/llama.cpp/releases/download/b11074/llama-b11074-bin-win-vulkan-x64.zip",
    size: 31934470,
    sha256: "a275b2b12491e895161e95a2f834ae8916a7f3792425cd97bccaef185be400d8",
    contents: Contents::Llama,
};

/// Windows aarch64: no Vulkan build published upstream, CPU only.
#[allow(dead_code)]
const WINDOWS_ARM64: Archive = Archive {
    name: "llama-b11074-bin-win-cpu-arm64.zip",
    url: "https://github.com/ggml-org/llama.cpp/releases/download/b11074/llama-b11074-bin-win-cpu-arm64.zip",
    size: 12026880,
    sha256: "539bd3e0226bf071af41c0ae09222c7ec92bb450b89ad7361f1693bf3a92929f",
    contents: Contents::Llama,
};

/// Linux x86_64: Vulkan build plus every CPU variant.
#[allow(dead_code)]
const LINUX_X64: Archive = Archive {
    name: "llama-b11074-bin-ubuntu-vulkan-x64.tar.gz",
    url: "https://github.com/ggml-org/llama.cpp/releases/download/b11074/llama-b11074-bin-ubuntu-vulkan-x64.tar.gz",
    size: 30489036,
    sha256: "7ee221e810515fd1cfc0dbd528a3f9e61a9ca6bf32053834c16b06b8d4358398",
    contents: Contents::Llama,
};

/// Linux aarch64: Vulkan build plus every CPU variant.
#[allow(dead_code)]
const LINUX_ARM64: Archive = Archive {
    name: "llama-b11074-bin-ubuntu-vulkan-arm64.tar.gz",
    url: "https://github.com/ggml-org/llama.cpp/releases/download/b11074/llama-b11074-bin-ubuntu-vulkan-arm64.tar.gz",
    size: 24420121,
    sha256: "c025900065cd74719114555ec494d30f3a34990748f911da90d5b45d98e0f605",
    contents: Contents::Llama,
};

/// Windows x64: Khronos' Vulkan loader (LunarG's runtime components), for GPUs whose
/// driver didn't already install one. Not published by llama.cpp itself.
#[allow(dead_code)]
const WINDOWS_VULKAN_LOADER: Archive = Archive {
    name: "VulkanRT-X64-1.4.357.0-Components.zip",
    url: "https://sdk.lunarg.com/sdk/download/1.4.357.0/windows/vulkan-runtime-components.zip",
    size: 18134567,
    sha256: "a14672efed15aafc7f5a16572d35cd3a3416eadf670aeee3cdf50ee32d5fbf83",
    contents: Contents::VulkanLoader,
};

/// What this platform needs, llama.cpp's archive first; empty where llama.cpp
/// publishes no build we can use.
pub fn archives() -> &'static [Archive] {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        const ARCHIVES: [Archive; 2] = [WINDOWS_X64, WINDOWS_VULKAN_LOADER];
        return &ARCHIVES;
    }
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    {
        const ARCHIVES: [Archive; 1] = [WINDOWS_ARM64];
        return &ARCHIVES;
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        const ARCHIVES: [Archive; 1] = [LINUX_X64];
        return &ARCHIVES;
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        const ARCHIVES: [Archive; 1] = [LINUX_ARM64];
        return &ARCHIVES;
    }
    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
    )))]
    return &[];
}

/// The name the host should give the unpacked directory: this platform's first
/// archive's name, minus its `.zip`/`.tar.gz` suffix. `None` where `archives` is empty.
pub fn dir_name() -> Option<String> {
    let name = archives().first()?.name;
    name.strip_suffix(".zip").or_else(|| name.strip_suffix(".tar.gz")).map(str::to_string)
}

/// The libraries the loader opens, each after the ones it depends on: ggml,
/// llama, and mtmd for images.
#[cfg(target_os = "windows")]
pub const LIBRARIES: [&str; 4] = ["ggml-base.dll", "ggml.dll", "llama.dll", "mtmd.dll"];
/// The libraries the loader opens, each after the ones it depends on.
#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
pub const LIBRARIES: [&str; 4] = ["libggml-base.so", "libggml.so", "libllama.so", "libmtmd.so"];
/// The macOS release build, which only development tests load (the app runs
/// Core ML and MLX there).
#[cfg(target_os = "macos")]
pub const LIBRARIES: [&str; 4] = ["libggml-base.dylib", "libggml.dylib", "libllama.dylib", "libmtmd.dylib"];

/// File name the Vulkan loader is unpacked as; the loader module falls back to this
/// only when the system didn't already register its own `vulkan-1.dll`.
pub const VULKAN_LOADER: &str = "vulkan-1.dll";

/// Name of the marker file that records which archives `dir` was unpacked from.
const MARKER_FILE: &str = "runtime.json";

/// One archive's identity as recorded in the marker file.
#[derive(Serialize, Deserialize)]
struct MarkerArchive {
    name: String,
    sha256: String,
}

/// One unpacked file as recorded in the marker file.
#[derive(Serialize, Deserialize)]
struct MarkerFile {
    name: String,
    size: u64,
}

/// Contents of `runtime.json`: which archives `dir` was unpacked from, in install order,
/// and every file unpacked from them, so that one gone missing (quarantined, say) makes
/// the runtime count as not installed rather than fail to load on every start.
#[derive(Serialize, Deserialize)]
struct Marker {
    archives: Vec<MarkerArchive>,
    files: Vec<MarkerFile>,
}

/// Whether `dir` holds a complete unpacking of exactly `archives`: the marker names the
/// same archives, in the same order, and every file they're pinned to provide is there.
pub fn is_installed(dir: &Path, archives: &[Archive]) -> bool {
    is_installed_libraries(dir, archives, &LIBRARIES)
}

/// `is_installed`, parameterized over the required library list (tests exercise this
/// directly with `lib*.so` names so the tar.gz path is covered on every host OS).
fn is_installed_libraries(dir: &Path, archives: &[Archive], required: &[&str]) -> bool {
    let marker = match read_marker(dir) {
        Some(marker) => marker,
        None => return false,
    };
    let same_archives = marker.archives.len() == archives.len()
        && marker.archives.iter().zip(archives).all(|(m, a)| m.name == a.name && m.sha256 == a.sha256);
    if !same_archives {
        return false;
    }
    let intact = |file: &MarkerFile| fs::metadata(dir.join(&file.name)).is_ok_and(|meta| meta.is_file() && meta.len() == file.size);
    if marker.files.is_empty() || !marker.files.iter().all(intact) {
        return false;
    }
    if !required.iter().all(|name| dir.join(name).is_file()) {
        return false;
    }
    if archives.iter().any(|a| a.contents == Contents::VulkanLoader) && !dir.join(VULKAN_LOADER).is_file() {
        return false;
    }
    true
}

/// Reads and parses `dir`'s marker file, if present and well-formed.
fn read_marker(dir: &Path) -> Option<Marker> {
    let text = fs::read_to_string(dir.join(MARKER_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Writes `staging`'s marker file recording `archives`, in the given order, and every
/// file unpacked into `staging`.
fn write_marker(staging: &Path, archives: &[(&Path, &Archive)]) -> Result<(), String> {
    let mut files = Vec::new();
    for entry in fs::read_dir(staging).map_err(stage_err)? {
        let entry = entry.map_err(stage_err)?;
        let meta = entry.metadata().map_err(stage_err)?;
        if meta.is_file() {
            files.push(MarkerFile { name: entry.file_name().to_string_lossy().into_owned(), size: meta.len() });
        }
    }
    files.sort_by(|a, b| a.name.cmp(&b.name));
    let marker = Marker {
        archives: archives
            .iter()
            .map(|&(_, a)| MarkerArchive { name: a.name.to_string(), sha256: a.sha256.to_string() })
            .collect(),
        files,
    };
    let text = serde_json::to_string(&marker).map_err(|e| corrupt(format!("无法写入安装清单：{e}")))?;
    fs::write(staging.join(MARKER_FILE), text).map_err(|e| corrupt(format!("无法写入安装清单：{e}")))
}

/// Unpacks every given archive (already downloaded and verified, each paired with the
/// path it was saved to) into one staging directory under `dir`, then swaps it in.
/// A no-op when `dir` already holds exactly this set of archives, fully unpacked.
pub fn install(archives: &[(&Path, &Archive)], dir: &Path) -> Result<(), String> {
    install_libraries(archives, dir, &LIBRARIES)
}

/// `install`, parameterized over the required library list (see `is_installed_libraries`).
fn install_libraries(archives: &[(&Path, &Archive)], dir: &Path, required: &[&str]) -> Result<(), String> {
    let installed: Vec<Archive> = archives.iter().map(|&(_, a)| *a).collect();
    if is_installed_libraries(dir, &installed, required) {
        return Ok(());
    }

    let staging = staging_dir(dir)?;
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(stage_err)?;
    }
    fs::create_dir_all(&staging).map_err(stage_err)?;

    let result = unpack_into(archives, &staging, required).and_then(|()| finish_install(&staging, dir).map_err(stage_err));
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

/// The sibling staging directory `install` unpacks into before the atomic rename.
fn staging_dir(dir: &Path) -> Result<PathBuf, String> {
    let file_name = dir.file_name().ok_or_else(|| "无效的安装目录".to_string())?;
    let mut staging_name = file_name.to_os_string();
    staging_name.push(".unpacking");
    Ok(dir.with_file_name(staging_name))
}

/// Unpacks every archive into `staging`, checks every `required` library (and, when a
/// `VulkanLoader` archive is among them, the Vulkan loader) landed, and writes the marker.
fn unpack_into(archives: &[(&Path, &Archive)], staging: &Path, required: &[&str]) -> Result<(), String> {
    unpack_all(archives, staging)?;

    for name in required {
        if !staging.join(name).is_file() {
            return Err(format!("llama.cpp 运行库缺少 {name}"));
        }
    }
    let needs_vulkan_loader = archives.iter().any(|&(_, a)| a.contents == Contents::VulkanLoader);
    if needs_vulkan_loader && !staging.join(VULKAN_LOADER).is_file() {
        return Err(format!("llama.cpp 运行库缺少 {VULKAN_LOADER}"));
    }

    write_marker(staging, archives)
}

/// Unpacks every archive's kept entries into `staging`, flattened to their own file
/// name. The same flattened name landing from two different archives is an error —
/// nothing we ship should collide; a collision means our allow-list, or llama.cpp's
/// release layout, changed under us.
fn unpack_all(archives: &[(&Path, &Archive)], staging: &Path) -> Result<(), String> {
    let mut claimed: HashSet<String> = HashSet::new();
    for &(path, archive) in archives {
        let written = match kind_of(archive.name)? {
            Kind::Zip => unpack_zip(path, archive.contents, staging)?,
            Kind::TarGz => unpack_tar_gz(path, staging)?,
        };
        for name in written {
            if !claimed.insert(name.clone()) {
                return Err(corrupt(format!("压缩包中有重复的文件名：{name}")));
            }
        }
    }
    Ok(())
}

/// Replaces `dir` with the fully-unpacked `staging`, atomically on the same volume.
/// The rename is retried for a moment: right after the libraries are written, a virus
/// scanner may still hold one open, and Windows then refuses to move the directory.
fn finish_install(staging: &Path, dir: &Path) -> io::Result<()> {
    if dir.exists() {
        fs::remove_dir_all(dir)?;
    }
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut delay = std::time::Duration::from_millis(100);
    for _ in 0..5 {
        match fs::rename(staging, dir) {
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => std::thread::sleep(delay),
            result => return result,
        }
        delay *= 2;
    }
    fs::rename(staging, dir)
}

/// Archive container format, chosen by the archive's file name suffix.
enum Kind {
    Zip,
    TarGz,
}

/// Determines the container format from the archive's published file name.
fn kind_of(name: &str) -> Result<Kind, String> {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".zip") {
        Ok(Kind::Zip)
    } else if lower.ends_with(".tar.gz") {
        Ok(Kind::TarGz)
    } else {
        Err(format!("不支持的压缩包格式：{name}"))
    }
}

/// Wraps a low-level error preparing or finishing the staging directory (not tied to
/// any one archive, since `install` may be unpacking several into it).
fn stage_err(error: impl std::fmt::Display) -> String {
    format!("无法解压 llama.cpp 运行库：{error}")
}

/// Wraps a low-level error as "this archive's contents are corrupt".
fn corrupt(what: impl std::fmt::Display) -> String {
    format!("llama.cpp 运行库压缩包损坏：{what}")
}

/// Reduces an archive entry's path to its last component, rejecting names that would
/// escape the staging directory. Handles both `/` and `\` separators since the zip
/// and tar.gz releases use different conventions.
fn flatten_name(raw: &str) -> Option<String> {
    let normalized = raw.replace('\\', "/");
    let name = normalized.rsplit('/').next().unwrap_or("");
    if name.is_empty() || name == "." || name == ".." {
        None
    } else {
        Some(name.to_string())
    }
}

/// Whether a llama.cpp release file (its flattened, ASCII-lowercased name) is one of
/// the shared libraries `install` ships, rather than one of the release's CLI tools or
/// their private `-impl` DLLs. Strips a leading `lib`, then cuts at the first `.dll` or
/// `.so`; the remaining stem must be a known library, or start with `ggml-cpu` (one
/// entry per CPU feature level, e.g. `ggml-cpu-zen4`).
fn keeps(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let stripped = lower.strip_prefix("lib").unwrap_or(&lower);
    let cut = [".dll", ".so"].iter().filter_map(|ext| stripped.find(ext)).min();
    let Some(at) = cut else { return false };
    let stem = &stripped[..at];
    matches!(stem, "ggml-base" | "ggml" | "llama" | "mtmd" | "ggml-vulkan" | "omp" | "gomp") || stem.starts_with("ggml-cpu")
}

// ---- zip ----

/// A little-endian u16 at `at`; the caller has checked the bounds.
fn le_u16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

/// A little-endian u32 at `at`; the caller has checked the bounds.
fn le_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// One parsed central-directory record: enough to locate and validate the entry's data.
struct ZipEntry {
    name: String,
    method: u16,
    flags: u16,
    crc32: u32,
    compressed_size: u64,
    uncompressed_size: u64,
    local_header_offset: u64,
}

/// Whether a zip entry should be unpacked, and under what flattened file name, given
/// what the archive's `contents` is.
fn zip_entry_target(raw_name: &str, contents: Contents) -> Option<String> {
    match contents {
        // The release zip also carries CLI tool executables and their private "-impl"
        // DLLs; `keeps` is the allow-list that screens those out.
        Contents::Llama => {
            let flat = flatten_name(raw_name)?;
            (flat.to_ascii_lowercase().ends_with(".dll") && keeps(&flat)).then_some(flat)
        }
        // The archive carries both an x86 and an x64 copy of the loader (plus license,
        // pdb and vulkaninfo files); we only ever load the x64 one, under a fixed name.
        Contents::VulkanLoader => is_x64_vulkan_loader(raw_name).then(|| VULKAN_LOADER.to_string()),
    }
}

/// Whether `raw` (a zip entry's path, `/`- or `\`-separated) is the x64 Vulkan loader
/// DLL: file name `vulkan-1.dll` directly under an `x64` directory, both compared
/// ASCII case-insensitively.
fn is_x64_vulkan_loader(raw: &str) -> bool {
    let normalized = raw.replace('\\', "/");
    let mut parts = normalized.rsplit('/');
    let file = parts.next().unwrap_or("");
    let parent = parts.next().unwrap_or("");
    file.eq_ignore_ascii_case("vulkan-1.dll") && parent.eq_ignore_ascii_case("x64")
}

/// Unpacks the entries of the zip at `path` into `staging`, flattened to their own
/// file name and filtered by `contents` (see `zip_entry_target`). Returns the flattened
/// names written, for `unpack_all`'s cross-archive duplicate check.
fn unpack_zip(path: &Path, contents: Contents, staging: &Path) -> Result<HashSet<String>, String> {
    let file = File::open(path).map_err(|e| corrupt(format!("无法打开压缩包：{e}")))?;
    let mut reader = BufReader::new(file);
    let entries = read_central_directory(&mut reader)?;

    let mut planned: HashMap<String, &ZipEntry> = HashMap::new();
    for entry in &entries {
        let flat = match zip_entry_target(&entry.name, contents) {
            Some(flat) => flat,
            None => continue,
        };
        if planned.insert(flat.clone(), entry).is_some() {
            return Err(corrupt(format!("压缩包中有重复的文件名：{flat}")));
        }
    }

    for (flat, entry) in &planned {
        extract_zip_entry(&mut reader, entry, &staging.join(flat))?;
    }
    Ok(planned.into_keys().collect())
}

/// Finds and parses the End Of Central Directory record, then reads every central
/// directory entry it points to. Rejects ZIP64 archives outright (we never need one:
/// llama.cpp's release archives, and the Vulkan loader's, are a few tens of megabytes).
fn read_central_directory(reader: &mut BufReader<File>) -> Result<Vec<ZipEntry>, String> {
    let file_len = reader.get_ref().metadata().map_err(|e| corrupt(format!("无法读取压缩包信息：{e}")))?.len();
    let eocd = find_eocd(reader, file_len)?;

    let total_entries = le_u16(&eocd, 10);
    let cd_size = le_u32(&eocd, 12);
    let cd_offset = le_u32(&eocd, 16);
    if total_entries == 0xFFFF || cd_size == 0xFFFFFFFF || cd_offset == 0xFFFFFFFF {
        return Err(corrupt("不支持 ZIP64 格式"));
    }

    reader.seek(SeekFrom::Start(cd_offset as u64)).map_err(|e| corrupt(format!("无法读取目录：{e}")))?;
    let mut cd_buf = vec![0u8; cd_size as usize];
    reader.read_exact(&mut cd_buf).map_err(|e| corrupt(format!("无法读取目录：{e}")))?;

    let mut entries = Vec::with_capacity(total_entries as usize);
    let mut pos = 0usize;
    for _ in 0..total_entries {
        if pos + 46 > cd_buf.len() {
            return Err(corrupt("目录记录被截断"));
        }
        let record = &cd_buf[pos..];
        let signature = le_u32(record, 0);
        if signature != 0x02014b50 {
            return Err(corrupt("目录签名无效"));
        }
        let flags = le_u16(record, 8);
        let method = le_u16(record, 10);
        let crc32 = le_u32(record, 16);
        let compressed_size = le_u32(record, 20);
        let uncompressed_size = le_u32(record, 24);
        let name_len = le_u16(record, 28) as usize;
        let extra_len = le_u16(record, 30) as usize;
        let comment_len = le_u16(record, 32) as usize;
        let local_header_offset = le_u32(record, 42);
        if compressed_size == 0xFFFFFFFF || uncompressed_size == 0xFFFFFFFF || local_header_offset == 0xFFFFFFFF {
            return Err(corrupt("不支持 ZIP64 格式"));
        }

        let name_start = pos + 46;
        let name_end = name_start + name_len;
        let record_end = name_end + extra_len + comment_len;
        if record_end > cd_buf.len() {
            return Err(corrupt("目录记录被截断"));
        }
        let name = String::from_utf8_lossy(&cd_buf[name_start..name_end]).into_owned();

        entries.push(ZipEntry {
            name,
            method,
            flags,
            crc32,
            compressed_size: compressed_size as u64,
            uncompressed_size: uncompressed_size as u64,
            local_header_offset: local_header_offset as u64,
        });
        pos = record_end;
    }
    Ok(entries)
}

/// Scans backward from the end of the file for the EOCD signature, within the range
/// it can possibly appear (fixed 22-byte record plus up to a 64 KiB comment).
fn find_eocd(reader: &mut BufReader<File>, file_len: u64) -> Result<Vec<u8>, String> {
    const MIN_LEN: u64 = 22;
    const MAX_COMMENT: u64 = 65535;
    let search_len = (MIN_LEN + MAX_COMMENT).min(file_len);
    let start = file_len - search_len;
    reader.seek(SeekFrom::Start(start)).map_err(|e| corrupt(format!("无法读取压缩包：{e}")))?;
    let mut buf = vec![0u8; search_len as usize];
    reader.read_exact(&mut buf).map_err(|e| corrupt(format!("无法读取压缩包：{e}")))?;

    if buf.len() >= 4 {
        for i in (0..=buf.len() - 4).rev() {
            if buf[i..i + 4] == [0x50, 0x4b, 0x05, 0x06] && buf.len() - i >= 22 {
                return Ok(buf[i..].to_vec());
            }
        }
    }
    Err(corrupt("找不到 ZIP 目录结束记录"))
}

/// Locates one entry's data via its local file header and streams it to `out_path`,
/// verifying size and CRC-32 as it goes.
fn extract_zip_entry(reader: &mut BufReader<File>, entry: &ZipEntry, out_path: &Path) -> Result<(), String> {
    if entry.flags & 0x1 != 0 {
        return Err(corrupt("不支持加密的压缩包条目"));
    }

    reader.seek(SeekFrom::Start(entry.local_header_offset)).map_err(|e| corrupt(format!("无法读取本地文件头：{e}")))?;
    let mut header = [0u8; 30];
    reader.read_exact(&mut header).map_err(|e| corrupt(format!("无法读取本地文件头：{e}")))?;
    if le_u32(&header, 0) != 0x04034b50 {
        return Err(corrupt("本地文件头签名无效"));
    }
    let name_len = le_u16(&header, 26) as i64;
    let extra_len = le_u16(&header, 28) as i64;
    reader.seek(SeekFrom::Current(name_len + extra_len)).map_err(|e| corrupt(format!("无法读取本地文件头：{e}")))?;

    let mut out = File::create(out_path).map_err(unpack_write_err)?;
    let mut crc = flate2::Crc::new();
    let mut written = 0u64;
    let limited = reader.take(entry.compressed_size);

    match entry.method {
        0 => stream_out(limited, &mut out, &mut crc, &mut written)?,
        8 => stream_out(flate2::read::DeflateDecoder::new(limited), &mut out, &mut crc, &mut written)?,
        other => return Err(corrupt(format!("不支持的压缩方式：{other}"))),
    }

    if written != entry.uncompressed_size {
        return Err(corrupt("条目大小与目录记录不符"));
    }
    if crc.sum() != entry.crc32 {
        return Err(corrupt("条目校验和不匹配"));
    }
    Ok(())
}

/// Copies `src` to `out` in fixed-size chunks, updating a running CRC-32 and byte count.
fn stream_out(mut src: impl Read, out: &mut File, crc: &mut flate2::Crc, written: &mut u64) -> Result<(), String> {
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = src.read(&mut buf).map_err(|e| corrupt(format!("解压失败：{e}")))?;
        if n == 0 {
            break;
        }
        crc.update(&buf[..n]);
        out.write_all(&buf[..n]).map_err(unpack_write_err)?;
        *written += n as u64;
    }
    Ok(())
}

/// A failure to write an unpacked file: the disk's fault, not the archive's.
fn unpack_write_err(error: io::Error) -> String {
    stage_err(format!("无法写入文件：{error}"))
}

// ---- tar.gz ----

/// Unpacks the `lib*.so*` entries of the tar.gz at `path` into `staging` that also pass
/// `keeps`, flattened to their own file name, resolving symlinks to copies of the file
/// they point at. Returns the flattened names written, for `unpack_all`'s cross-archive
/// duplicate check.
fn unpack_tar_gz(path: &Path, staging: &Path) -> Result<HashSet<String>, String> {
    let file = File::open(path).map_err(|e| corrupt(format!("无法打开压缩包：{e}")))?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);

    let mut written: HashMap<String, PathBuf> = HashMap::new();
    let mut symlink_targets: HashMap<String, String> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();

    let entries = archive.entries().map_err(|e| corrupt(format!("无法读取压缩包：{e}")))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| corrupt(format!("无法读取压缩包：{e}")))?;
        let entry_type = entry.header().entry_type();
        let raw_name = entry.path().map_err(|e| corrupt(format!("无法读取压缩包：{e}")))?.to_string_lossy().into_owned();
        let flat = match flatten_name(&raw_name) {
            Some(flat) => flat,
            None => continue,
        };
        let lower = flat.to_ascii_lowercase();
        if !(lower.starts_with("lib") && lower.contains(".so")) {
            continue;
        }
        if !keeps(&flat) {
            continue;
        }
        if !seen.insert(flat.clone()) {
            return Err(corrupt(format!("压缩包中有重复的文件名：{flat}")));
        }

        match entry_type {
            tar::EntryType::Regular => {
                let out_path = staging.join(&flat);
                let mut out = File::create(&out_path).map_err(unpack_write_err)?;
                io::copy(&mut entry, &mut out).map_err(unpack_write_err)?;
                written.insert(flat, out_path);
            }
            // A hard link is stored like a symlink: a name and the entry it shares data with.
            tar::EntryType::Symlink | tar::EntryType::Link => {
                let link_name = entry
                    .link_name()
                    .map_err(|e| corrupt(format!("无法读取符号链接：{e}")))?
                    .ok_or_else(|| corrupt("符号链接缺少目标"))?
                    .to_string_lossy()
                    .into_owned();
                let target = flatten_name(&link_name).ok_or_else(|| corrupt(format!("符号链接目标无效：{flat}")))?;
                symlink_targets.insert(flat, target);
            }
            _ => {}
        }
    }

    const MAX_HOPS: usize = 8;
    for (link_name, target) in &symlink_targets {
        let resolved = resolve_symlink(target, &written, &symlink_targets, MAX_HOPS)
            .ok_or_else(|| corrupt(format!("符号链接未解析：{link_name}")))?;
        fs::copy(&resolved, staging.join(link_name)).map_err(unpack_write_err)?;
    }

    let mut names: HashSet<String> = written.into_keys().collect();
    names.extend(symlink_targets.into_keys());
    Ok(names)
}

/// Follows a chain of symlink targets (by flattened name) to the regular file they
/// ultimately resolve to, bounded to `max_hops` to reject cycles.
fn resolve_symlink(
    start: &str,
    written: &HashMap<String, PathBuf>,
    symlink_targets: &HashMap<String, String>,
    max_hops: usize,
) -> Option<PathBuf> {
    let mut current = start.to_string();
    for _ in 0..max_hops {
        if let Some(path) = written.get(&current) {
            return Some(path.clone());
        }
        current = symlink_targets.get(&current)?.clone();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- shared fixture helpers ----

    struct ZipEntrySpec {
        name: &'static str,
        data: Vec<u8>,
        method: u16,
    }

    fn zip_stored(name: &'static str, data: &[u8]) -> ZipEntrySpec {
        ZipEntrySpec { name, data: data.to_vec(), method: 0 }
    }

    fn zip_deflated(name: &'static str, data: &[u8]) -> ZipEntrySpec {
        ZipEntrySpec { name, data: data.to_vec(), method: 8 }
    }

    fn zip_dir(name: &'static str) -> ZipEntrySpec {
        ZipEntrySpec { name, data: Vec::new(), method: 0 }
    }

    /// Builds a minimal but spec-correct zip file: local headers, central directory, EOCD.
    fn build_zip(entries: &[ZipEntrySpec]) -> Vec<u8> {
        struct Prepared {
            name: &'static str,
            compressed: Vec<u8>,
            crc: u32,
            orig_len: u32,
            method: u16,
        }

        let prepared: Vec<Prepared> = entries
            .iter()
            .map(|e| {
                let mut crc = flate2::Crc::new();
                crc.update(&e.data);
                let compressed = match e.method {
                    0 => e.data.clone(),
                    8 => {
                        let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
                        enc.write_all(&e.data).unwrap();
                        enc.finish().unwrap()
                    }
                    other => panic!("unsupported test method {other}"),
                };
                Prepared { name: e.name, compressed, crc: crc.sum(), orig_len: e.data.len() as u32, method: e.method }
            })
            .collect();

        let mut out = Vec::new();
        let mut offsets = Vec::new();
        for p in &prepared {
            offsets.push(out.len() as u32);
            out.extend_from_slice(&0x04034b50u32.to_le_bytes());
            out.extend_from_slice(&20u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&p.method.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&p.crc.to_le_bytes());
            out.extend_from_slice(&(p.compressed.len() as u32).to_le_bytes());
            out.extend_from_slice(&p.orig_len.to_le_bytes());
            out.extend_from_slice(&(p.name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(p.name.as_bytes());
            out.extend_from_slice(&p.compressed);
        }

        let mut central = Vec::new();
        for (i, p) in prepared.iter().enumerate() {
            central.extend_from_slice(&0x02014b50u32.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&p.method.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&p.crc.to_le_bytes());
            central.extend_from_slice(&(p.compressed.len() as u32).to_le_bytes());
            central.extend_from_slice(&p.orig_len.to_le_bytes());
            central.extend_from_slice(&(p.name.len() as u16).to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u32.to_le_bytes());
            central.extend_from_slice(&offsets[i].to_le_bytes());
            central.extend_from_slice(p.name.as_bytes());
        }

        let cd_offset = out.len() as u32;
        let cd_size = central.len() as u32;
        out.extend_from_slice(&central);

        out.extend_from_slice(&0x06054b50u32.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(prepared.len() as u16).to_le_bytes());
        out.extend_from_slice(&(prepared.len() as u16).to_le_bytes());
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    fn write_temp(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    /// The libraries the zip fixtures carry (Windows names, on every host).
    const ZIP_LIBS: [&str; 3] = ["ggml-base.dll", "ggml.dll", "llama.dll"];

    fn install_zip(archives: &[(&Path, &Archive)], dir: &Path) -> Result<(), String> {
        install_libraries(archives, dir, &ZIP_LIBS)
    }

    fn zip_installed(dir: &Path, archives: &[Archive]) -> bool {
        is_installed_libraries(dir, archives, &ZIP_LIBS)
    }

    fn test_archive(name: &'static str) -> Archive {
        test_archive_with(name, Contents::Llama)
    }

    fn test_archive_with(name: &'static str, contents: Contents) -> Archive {
        Archive { name, url: "https://example.test/archive", size: 0, sha256: "test-sha", contents }
    }

    // ---- zip tests ----

    #[test]
    fn zip_install_unpacks_only_dlls_and_flattens_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let data_a = b"stored dll bytes".to_vec();
        let data_b = b"deflated dll bytes, deflated dll bytes, deflated dll bytes".to_vec();
        let zip_bytes = build_zip(&[
            zip_stored("llama-b11074/ggml-base.dll", &data_a),
            zip_deflated("llama-b11074/nested/dir/ggml.dll", &data_b),
            zip_deflated("llama-b11074/llama.dll", b"third dll"),
            zip_stored("llama-b11074/llama-cli.exe", b"not a dll"),
            zip_dir("llama-b11074/"),
        ]);
        let archive_path = write_temp(tmp.path(), "archive.zip", &zip_bytes);
        let archive = test_archive("archive.zip");
        let install_dir = tmp.path().join("install");

        install_zip(&[(archive_path.as_path(), &archive)], &install_dir).unwrap();
        assert!(zip_installed(&install_dir, &[archive]));
        assert_eq!(fs::read(install_dir.join("ggml-base.dll")).unwrap(), data_a);
        assert_eq!(fs::read(install_dir.join("ggml.dll")).unwrap(), data_b);
        assert!(!install_dir.join("llama-cli.exe").exists());

        // Second install is a no-op: prove it by changing a file (same size) and re-running.
        let same_size = vec![b'x'; data_a.len()];
        fs::write(install_dir.join("ggml-base.dll"), &same_size).unwrap();
        install_zip(&[(archive_path.as_path(), &archive)], &install_dir).unwrap();
        assert_eq!(fs::read(install_dir.join("ggml-base.dll")).unwrap(), same_size);
    }

    #[test]
    fn a_missing_or_cut_file_is_unpacked_again() {
        let tmp = tempfile::tempdir().unwrap();
        let zip_bytes = build_zip(&[
            zip_stored("ggml-base.dll", b"base"),
            zip_stored("ggml.dll", b"ggml"),
            zip_stored("llama.dll", b"llama"),
            zip_deflated("ggml-cpu-haswell.dll", b"a cpu variant, not a required library"),
        ]);
        let archive_path = write_temp(tmp.path(), "archive.zip", &zip_bytes);
        let archive = test_archive("archive.zip");
        let install_dir = tmp.path().join("install");
        install_zip(&[(archive_path.as_path(), &archive)], &install_dir).unwrap();

        // A backend module the loader does not open itself still counts.
        fs::remove_file(install_dir.join("ggml-cpu-haswell.dll")).unwrap();
        assert!(!zip_installed(&install_dir, &[archive]));
        install_zip(&[(archive_path.as_path(), &archive)], &install_dir).unwrap();
        assert!(zip_installed(&install_dir, &[archive]));

        fs::write(install_dir.join("llama.dll"), b"ll").unwrap();
        assert!(!zip_installed(&install_dir, &[archive]));
        install_zip(&[(archive_path.as_path(), &archive)], &install_dir).unwrap();
        assert_eq!(fs::read(install_dir.join("llama.dll")).unwrap(), b"llama");
    }

    #[test]
    fn zip_corrupted_crc_fails_cleanly() {
        let tmp = tempfile::tempdir().unwrap();
        let mut zip_bytes = build_zip(&[zip_stored("ggml-base.dll", b"hello world")]);
        // Flip a byte inside the stored entry's data (right after the 30-byte local header + name).
        let flip_at = 30 + "ggml-base.dll".len() + 2;
        zip_bytes[flip_at] ^= 0xFF;
        let archive_path = write_temp(tmp.path(), "archive.zip", &zip_bytes);
        let archive = test_archive("archive.zip");
        let install_dir = tmp.path().join("install");

        let result = install_zip(&[(archive_path.as_path(), &archive)], &install_dir);
        assert!(result.is_err());
        assert!(!install_dir.exists());
        assert!(!tmp.path().join("install.unpacking").exists());
    }

    #[test]
    fn zip_missing_required_library_fails_and_names_it() {
        let tmp = tempfile::tempdir().unwrap();
        let zip_bytes = build_zip(&[zip_stored("ggml-base.dll", b"only one dll")]);
        let archive_path = write_temp(tmp.path(), "archive.zip", &zip_bytes);
        let archive = test_archive("archive.zip");
        let install_dir = tmp.path().join("install");

        let err = install_zip(&[(archive_path.as_path(), &archive)], &install_dir).unwrap_err();
        assert!(ZIP_LIBS.iter().skip(1).any(|missing| err.contains(missing)), "error was: {err}");
    }

    #[test]
    fn zip_duplicate_flattened_names_fail() {
        let tmp = tempfile::tempdir().unwrap();
        let zip_bytes = build_zip(&[
            zip_stored("a/ggml.dll", b"one"),
            zip_stored("b/ggml.dll", b"two"),
        ]);
        let archive_path = write_temp(tmp.path(), "archive.zip", &zip_bytes);
        let archive = test_archive("archive.zip");
        let install_dir = tmp.path().join("install");

        assert!(install_zip(&[(archive_path.as_path(), &archive)], &install_dir).is_err());
        assert!(!install_dir.exists());
    }

    #[test]
    fn existing_incomplete_install_dir_is_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let zip_bytes = build_zip(&[zip_stored("ggml-base.dll", b"a"), zip_stored("ggml.dll", b"b"), zip_stored("llama.dll", b"c")]);
        let archive_path = write_temp(tmp.path(), "archive.zip", &zip_bytes);
        let archive = test_archive("archive.zip");
        let install_dir = tmp.path().join("install");

        fs::create_dir_all(&install_dir).unwrap();
        fs::write(install_dir.join("stale.txt"), b"leftover").unwrap();

        install_zip(&[(archive_path.as_path(), &archive)], &install_dir).unwrap();
        assert!(zip_installed(&install_dir, &[archive]));
        assert!(!install_dir.join("stale.txt").exists());
    }

    #[test]
    fn keeps_filters_tool_dlls_and_keeps_shared_libraries() {
        for name in [
            "ggml-base.dll",
            "ggml.dll",
            "llama.dll",
            "ggml-vulkan.dll",
            "libomp.dll",
            "ggml-cpu-zen4.dll",
            "libllama.so.0.0.11074",
            "libggml-cpu-haswell.so",
            "mtmd.dll",
            "libmtmd.so.0.0.11074",
        ] {
            assert!(keeps(name), "expected to keep {name}");
        }
        for name in ["llama-common.dll", "ggml-rpc.dll", "llama-cli-impl.dll", "mtmd-cli-impl.dll"] {
            assert!(!keeps(name), "expected to drop {name}");
        }
    }

    #[test]
    fn vulkan_loader_zip_unpacks_only_the_x64_dll() {
        let tmp = tempfile::tempdir().unwrap();
        let x64_bytes = b"the x64 loader".to_vec();
        let vulkan_zip = build_zip(&[
            zip_dir("VulkanRT-X64-1.4.357.0-Components/"),
            zip_stored("VulkanRT-X64-1.4.357.0-Components/VulkanRT-License.txt", b"license"),
            zip_stored("VulkanRT-X64-1.4.357.0-Components/x86/vulkan-1.dll", b"the x86 loader"),
            zip_stored("VulkanRT-X64-1.4.357.0-Components/x86/vulkan-1.pdb", b"pdb"),
            zip_stored("VulkanRT-X64-1.4.357.0-Components/x86/vulkaninfo.exe", b"exe"),
            zip_stored("VulkanRT-X64-1.4.357.0-Components/x86/vulkaninfo.pdb", b"pdb"),
            zip_stored("VulkanRT-X64-1.4.357.0-Components/x64/vulkan-1.dll", &x64_bytes),
            zip_stored("VulkanRT-X64-1.4.357.0-Components/x64/vulkan-1.pdb", b"pdb"),
            zip_stored("VulkanRT-X64-1.4.357.0-Components/x64/vulkaninfo.exe", b"exe"),
            zip_stored("VulkanRT-X64-1.4.357.0-Components/x64/vulkaninfo.pdb", b"pdb"),
        ]);
        let vulkan_path = write_temp(tmp.path(), "vkrt.zip", &vulkan_zip);
        let vulkan_archive = test_archive_with("vkrt.zip", Contents::VulkanLoader);

        let llama_zip = build_zip(&[zip_stored("ggml-base.dll", b"a"), zip_stored("ggml.dll", b"b"), zip_stored("llama.dll", b"c")]);
        let llama_path = write_temp(tmp.path(), "llama.zip", &llama_zip);
        let llama_archive = test_archive("llama.zip");

        let install_dir = tmp.path().join("install");
        let pair = [(llama_path.as_path(), &llama_archive), (vulkan_path.as_path(), &vulkan_archive)];
        install_zip(&pair, &install_dir).unwrap();

        assert_eq!(fs::read(install_dir.join(VULKAN_LOADER)).unwrap(), x64_bytes);
        assert!(zip_installed(&install_dir, &[llama_archive, vulkan_archive]));

        // is_installed requires the Vulkan loader file too: prove it by deleting it.
        fs::remove_file(install_dir.join(VULKAN_LOADER)).unwrap();
        assert!(!zip_installed(&install_dir, &[llama_archive, vulkan_archive]));
    }

    #[test]
    fn archives_matches_this_platform() {
        #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
        {
            let list = archives();
            assert_eq!(list.len(), 2);
            assert_eq!(list[0].name, WINDOWS_X64.name);
            assert_eq!(list[0].contents, Contents::Llama);
            assert!(list[0].url.contains("github.com/ggml-org/llama.cpp/releases/download/b11074/"));
            assert!(list[0].url.ends_with(list[0].name));
            assert_eq!(list[1].name, WINDOWS_VULKAN_LOADER.name);
            assert_eq!(list[1].contents, Contents::VulkanLoader);
            assert_eq!(dir_name().unwrap(), "llama-b11074-bin-win-vulkan-x64");
        }
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            let list = archives();
            assert_eq!(list.len(), 1);
            assert_eq!(list[0].name, LINUX_X64.name);
            assert!(list[0].name.ends_with(".tar.gz"));
            assert_eq!(dir_name().unwrap(), "llama-b11074-bin-ubuntu-vulkan-x64");
        }
        // Every constant is reachable from tests regardless of host platform.
        let _ = (&WINDOWS_X64, &WINDOWS_ARM64, &LINUX_X64, &LINUX_ARM64, &WINDOWS_VULKAN_LOADER);
    }

    // ---- tar.gz tests ----

    const TAR_LIBS: [&str; 3] = ["libggml-base.so", "libggml.so", "libllama.so"];

    fn build_tar_gz(entries: Vec<(&str, TarEntryKind)>) -> Vec<u8> {
        let enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut builder = tar::Builder::new(enc);
        for (name, kind) in entries {
            match kind {
                TarEntryKind::Regular(data) => {
                    let mut header = tar::Header::new_gnu();
                    header.set_size(data.len() as u64);
                    header.set_mode(0o644);
                    header.set_cksum();
                    builder.append_data(&mut header, name, data.as_slice()).unwrap();
                }
                TarEntryKind::Symlink(target) => {
                    let mut header = tar::Header::new_gnu();
                    header.set_entry_type(tar::EntryType::Symlink);
                    header.set_size(0);
                    header.set_mode(0o777);
                    header.set_cksum();
                    builder.append_link(&mut header, name, target).unwrap();
                }
            }
        }
        let enc = builder.into_inner().unwrap();
        enc.finish().unwrap()
    }

    enum TarEntryKind {
        Regular(Vec<u8>),
        Symlink(&'static str),
    }

    #[test]
    fn tar_gz_unpacks_libs_and_resolves_symlink_chain() {
        let tmp = tempfile::tempdir().unwrap();
        let content = b"the real library bytes".to_vec();
        let bytes = build_tar_gz(vec![
            ("llama-b11074/libggml-base.so", TarEntryKind::Regular(b"base".to_vec())),
            ("llama-b11074/libggml.so", TarEntryKind::Regular(b"ggml".to_vec())),
            ("llama-b11074/libllama.so.0.0.1", TarEntryKind::Regular(content.clone())),
            ("llama-b11074/libllama.so.0", TarEntryKind::Symlink("libllama.so.0.0.1")),
            ("llama-b11074/libllama.so", TarEntryKind::Symlink("libllama.so.0")),
            ("llama-b11074/bin/llama-cli", TarEntryKind::Regular(b"not a lib".to_vec())),
            ("llama-b11074/libllama-common.so", TarEntryKind::Regular(b"not kept".to_vec())),
        ]);
        let archive_path = write_temp(tmp.path(), "archive.tar.gz", &bytes);
        let archive = test_archive("archive.tar.gz");
        let install_dir = tmp.path().join("install");

        install_libraries(&[(archive_path.as_path(), &archive)], &install_dir, &TAR_LIBS).unwrap();
        assert!(is_installed_libraries(&install_dir, &[archive], &TAR_LIBS));
        assert_eq!(fs::read(install_dir.join("libllama.so.0.0.1")).unwrap(), content);
        assert_eq!(fs::read(install_dir.join("libllama.so.0")).unwrap(), content);
        assert_eq!(fs::read(install_dir.join("libllama.so")).unwrap(), content);
        assert!(!install_dir.join("llama-cli").exists());
        assert!(!install_dir.join("libllama-common.so").exists());
    }

    #[test]
    fn tar_gz_dangling_symlink_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let bytes = build_tar_gz(vec![
            ("libggml-base.so", TarEntryKind::Regular(b"base".to_vec())),
            ("libggml.so", TarEntryKind::Regular(b"ggml".to_vec())),
            ("libllama.so", TarEntryKind::Symlink("libllama.so.0")),
        ]);
        let archive_path = write_temp(tmp.path(), "archive.tar.gz", &bytes);
        let archive = test_archive("archive.tar.gz");
        let install_dir = tmp.path().join("install");

        let result = install_libraries(&[(archive_path.as_path(), &archive)], &install_dir, &TAR_LIBS);
        assert!(result.is_err());
        assert!(!install_dir.exists());
    }

    #[test]
    fn tar_gz_duplicate_flattened_names_fail() {
        let tmp = tempfile::tempdir().unwrap();
        let bytes = build_tar_gz(vec![
            ("a/libggml.so", TarEntryKind::Regular(b"one".to_vec())),
            ("b/libggml.so", TarEntryKind::Regular(b"two".to_vec())),
        ]);
        let archive_path = write_temp(tmp.path(), "archive.tar.gz", &bytes);
        let archive = test_archive("archive.tar.gz");
        let install_dir = tmp.path().join("install");

        assert!(install_libraries(&[(archive_path.as_path(), &archive)], &install_dir, &TAR_LIBS).is_err());
        assert!(!install_dir.exists());
    }

    // ---- optional real-archives test ----

    #[test]
    fn real_archives_if_present() {
        let Ok(dir_str) = std::env::var("MEWORK_LLAMA_ARCHIVES_DIR") else {
            eprintln!("MEWORK_LLAMA_ARCHIVES_DIR not set, skipping real-archive test");
            return;
        };
        let dir = PathBuf::from(dir_str);
        let list = archives();
        if list.is_empty() {
            eprintln!("no pinned archives for this platform, skipping real-archive test");
            return;
        }

        let mut paths = Vec::new();
        for archive in list {
            let path = dir.join(archive.name);
            let len = match fs::metadata(&path) {
                Ok(meta) => meta.len(),
                Err(_) => {
                    eprintln!("{} not found in {}, skipping real-archive test", archive.name, dir.display());
                    return;
                }
            };
            if len != archive.size {
                eprintln!("{} is {len} bytes, expected {} (still downloading?), skipping", archive.name, archive.size);
                return;
            }
            paths.push(path);
        }

        let pairs: Vec<(&Path, &Archive)> = paths.iter().map(|p| p.as_path()).zip(list.iter()).collect();
        let tmp = tempfile::tempdir().unwrap();
        let install_dir = tmp.path().join("install");
        install(&pairs, &install_dir).unwrap();
        assert!(is_installed(&install_dir, list));

        let mut names: Vec<String> =
            fs::read_dir(&install_dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        eprintln!("unpacked files: {names:?}");

        #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
        {
            assert!(names.iter().any(|n| n == "ggml-vulkan.dll"));
            assert!(names.iter().any(|n| n.starts_with("ggml-cpu") && n.ends_with(".dll")));
            assert!(names.iter().any(|n| n == VULKAN_LOADER));
        }
    }
}
