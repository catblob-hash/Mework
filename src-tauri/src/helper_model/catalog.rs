//! The installable models. Each is one build of Qwen3.5-0.8B for one
//! inference backend:
//!
//! - `ane`: Core ML package for the Apple Neural Engine (Macs with one, macOS 15+);
//! - `mlx`: MLX weights and kernels for the GPU (Apple silicon, macOS 14+);
//! - `llama`: GGUF for llama.cpp (Windows and Linux).
//!
//! All are made ahead of time from the official release by
//! `local-model/examples/build_release.rs`, published to one Hugging Face
//! repository, and pinned here file by file (`catalog.json`, written by that
//! tool). The llama.cpp build also downloads the runtime it runs on from its
//! publishers' own releases (`llama_runtime_files`).

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use super::download::{RemoteFile, Repo};

/// Where the builds are published, at the commit whose files `catalog.json`
/// pins.
pub const PREBUILT_REPO: Repo =
    Repo { id: "catblob-hash/Mework-Qwen3.5-0.8B", revision: "9877818933675a4a28d356743420543f0f9de135" };

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VariantId {
    Ane,
    Mlx,
    Llama,
}

impl VariantId {
    /// Its directory under the local-model root.
    pub fn dir(self) -> &'static str {
        match self {
            Self::Ane => "ane",
            Self::Mlx => "mlx",
            Self::Llama => "llama",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "ane" => Some(Self::Ane),
            "mlx" => Some(Self::Mlx),
            "llama" => Some(Self::Llama),
            _ => None,
        }
    }
}

/// The variants this build of the app knows, best first.
pub fn platform_variants() -> &'static [VariantId] {
    if cfg!(target_os = "macos") {
        &[VariantId::Ane, VariantId::Mlx]
    } else {
        &[VariantId::Llama]
    }
}

#[derive(Deserialize)]
struct CatalogFile {
    variants: BTreeMap<String, VariantFiles>,
}

#[derive(Clone, Deserialize)]
pub struct VariantFiles {
    /// Recorded in the installed copy; a new version means a new download.
    pub version: String,
    pub files: Vec<RemoteFile>,
}

/// What `id` downloads: its files from `PREBUILT_REPO`, and for the llama.cpp
/// build what it runs on (`llama_runtime_files`).
pub fn files(id: VariantId) -> VariantFiles {
    static CATALOG: OnceLock<BTreeMap<String, VariantFiles>> = OnceLock::new();
    #[allow(unused_mut)]
    let mut files = CATALOG
        .get_or_init(|| {
            let file: CatalogFile = serde_json::from_str(include_str!("catalog.json")).expect("catalog.json");
            file.variants
        })[id.dir()]
    .clone();
    #[cfg(not(target_os = "macos"))]
    if id == VariantId::Llama {
        files.version = format!("{}+llama.cpp-{}", files.version, local_model::llama::runtime::RELEASE);
        // First: they come from other hosts than the model, so a host that
        // is unreachable fails the install before 1.5 GB of weights, not after.
        files.files.splice(0..0, llama_runtime_files());
    }
    files
}

/// The archives the llama.cpp backend's runtime is unpacked from, as their
/// publishers release them (llama.cpp's release build, and on Windows x64
/// Khronos' Vulkan loader from LunarG), each checked against its pinned
/// digest; a mirror serves them under `llama/runtime/` in the repository.
#[cfg(not(target_os = "macos"))]
pub fn llama_runtime_files() -> Vec<RemoteFile> {
    local_model::llama::runtime::archives()
        .iter()
        .map(|archive| RemoteFile {
            remote: format!("llama/runtime/{}", archive.name),
            local: archive.name.to_string(),
            size: archive.size,
            sha256: archive.sha256.to_string(),
            url: Some(archive.url.to_string()),
        })
        .collect()
}

pub fn download_bytes(id: VariantId) -> u64 {
    files(id).files.iter().map(|file| file.size).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_has_its_files() {
        for id in [VariantId::Ane, VariantId::Mlx, VariantId::Llama] {
            let variant = files(id);
            for file in &variant.files {
                assert_eq!(file.sha256.len(), 64, "{}", file.remote);
                assert!(!file.local.starts_with('/') && !file.local.contains(".."), "{}", file.local);
            }
            for common in ["config.json", "tokenizer.json"] {
                assert!(variant.files.iter().any(|file| file.local == common), "{id:?} {common}");
            }
            assert!(download_bytes(id) > 1_400_000_000, "{id:?}");
            assert_eq!(VariantId::parse(id.dir()), Some(id));
        }
        let has = |id, local: &str| files(id).files.iter().any(|file| file.local == local);
        assert!(has(VariantId::Ane, "model.mlpackage/Data/com.apple.CoreML/weights/weight.bin"));
        assert!(has(VariantId::Mlx, local_model::mlx::METALLIB_FILE));
        assert!(has(VariantId::Llama, local_model::gguf::GGUF_FILE));
    }

    #[cfg(all(not(target_os = "macos"), any(target_arch = "x86_64", target_arch = "aarch64")))]
    #[test]
    fn the_llama_build_brings_its_runtime() {
        let variant = files(VariantId::Llama);
        let runtime: Vec<_> = variant.files.iter().filter(|file| file.url.is_some()).collect();
        assert!(runtime[0].url.as_deref().unwrap().starts_with("https://github.com/ggml-org/llama.cpp/releases/download/b"));
        for file in &runtime {
            assert_eq!(file.remote, format!("llama/runtime/{}", file.local));
        }
        assert!(variant.version.ends_with(&format!("+llama.cpp-{}", local_model::llama::runtime::RELEASE)));
    }
}
