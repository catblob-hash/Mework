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
//! tool).

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use super::download::{RemoteFile, Repo};

/// Where the builds are published, at the commit whose files `catalog.json`
/// pins.
pub const PREBUILT_REPO: Repo =
    Repo { id: "catblob-hash/Mework-Qwen3.5-0.8B", revision: "48c1c5551f115cef37d4a3129a5ac00a645034c7" };

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

/// What `id` downloads from `PREBUILT_REPO`.
pub fn files(id: VariantId) -> VariantFiles {
    static CATALOG: OnceLock<BTreeMap<String, VariantFiles>> = OnceLock::new();
    CATALOG
        .get_or_init(|| {
            let file: CatalogFile = serde_json::from_str(include_str!("catalog.json")).expect("catalog.json");
            file.variants
        })[id.dir()]
    .clone()
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
}
