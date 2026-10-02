//! Writes the `.mlpackage` for the Neural Engine backend from the official
//! checkpoint: one multifunction ML Program whose functions share one weight
//! file. Core ML compiles it on the device (`MLModel.compileModel`).

use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use sha2::{Digest, Sha256};

use super::graph::{Builder, Shapes};
use super::mil::{model_spec, Function};
use crate::qwen35::Config;
use crate::safetensors::SafeTensors;

/// Bumped whenever the graph or its inputs change, so installed packages are
/// rebuilt instead of being fed inputs they were not built for. 2: 5,120
/// positions (was 1,024).
pub const GRAPH_VERSION: &str = "qwen35-ane-2";

#[derive(Clone, Debug)]
pub struct PackagePlan {
    pub shapes: Shapes,
    /// Layer ranges, one decode and one prefill function per range. The ANE
    /// compiles programs of about half a gigabyte of weights most reliably.
    pub parts: Vec<Range<usize>>,
    pub head_splits: usize,
}

impl PackagePlan {
    pub fn standard(config: &Config, shapes: Shapes) -> Self {
        let n = config.layers.len();
        Self { shapes, parts: vec![0..n / 2, n / 2..n], head_splits: 16 }
    }

    pub fn decode_name(part: usize) -> String {
        format!("decode_{part}")
    }

    pub fn prefill_name(part: usize) -> String {
        format!("prefill_{part}")
    }
}

pub const HEAD: &str = "head";

/// Saved beside the package: where the embedding table sits in its weight
/// file. The embeddings are tied, so the table is the output projection's
/// weights, and the host reads its lookups from there instead of keeping a
/// second copy.
pub const EMBEDDING_FILE: &str = "embedding.json";

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EmbeddingLayout {
    /// `GRAPH_VERSION` of the package this describes.
    pub graph: String,
    pub hidden: usize,
    /// `(rows, metadata offset)` per block of the table, in vocabulary order.
    pub blocks: Vec<(usize, u64)>,
}

impl EmbeddingLayout {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = fs::read_to_string(path).map_err(|error| format!("无法读取 {}: {error}", path.display()))?;
        let layout: Self = serde_json::from_str(&text).map_err(|error| format!("{} 格式不对: {error}", path.display()))?;
        if layout.graph != GRAPH_VERSION {
            return Err(format!("模型包版本 {} 与程序需要的 {GRAPH_VERSION} 不符", layout.graph));
        }
        Ok(layout)
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).expect("layout serializes");
        fs::write(path, text).map_err(|error| format!("无法写入 {}: {error}", path.display()))
    }
}

pub struct BuildProgress {
    pub written: u64,
    pub total: u64,
}

fn item_id(name: &str) -> String {
    let digest = Sha256::digest(format!("mework:{name}").as_bytes());
    let hex: String = digest.iter().take(16).map(|b| format!("{b:02X}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

fn manifest() -> String {
    let spec = item_id("model.mlmodel");
    let weights = item_id("weights");
    format!(
        r#"{{
    "fileFormatVersion": "1.0.0",
    "itemInfoEntries": {{
        "{weights}": {{
            "author": "com.apple.CoreML",
            "description": "CoreML Model Weights",
            "name": "weights",
            "path": "com.apple.CoreML/weights"
        }},
        "{spec}": {{
            "author": "com.apple.CoreML",
            "description": "CoreML Model Specification",
            "name": "model.mlmodel",
            "path": "com.apple.CoreML/model.mlmodel"
        }}
    }},
    "rootModelIdentifier": "{spec}"
}}
"#
    )
}

/// Builds the functions for `plan` (weights planned, not yet written).
pub fn build_functions(builder: &mut Builder, plan: &PackagePlan) -> Result<Vec<Function>, String> {
    let mut functions = Vec::new();
    for (i, range) in plan.parts.iter().enumerate() {
        functions.push(builder.decode(&PackagePlan::decode_name(i), range.clone())?);
    }
    for (i, range) in plan.parts.iter().enumerate() {
        functions.push(builder.prefill(&PackagePlan::prefill_name(i), range.clone())?);
    }
    functions.push(builder.head(HEAD, plan.head_splits)?);
    Ok(functions)
}

/// Writes `<out>` (an `.mlpackage` directory) atomically and returns where
/// its weight file keeps the embedding table.
pub fn write_package(
    config: &Config,
    weights: &SafeTensors,
    plan: &PackagePlan,
    out: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(BuildProgress),
) -> Result<EmbeddingLayout, String> {
    if !config.tie_word_embeddings {
        return Err("只支持词嵌入与输出层共享权重的模型".into());
    }
    let mut builder = Builder::new(config, weights, plan.shapes);
    let functions = build_functions(&mut builder, plan)?;
    let layout =
        EmbeddingLayout { graph: GRAPH_VERSION.to_string(), hidden: config.hidden_size, blocks: builder.head_blobs.clone() };
    let shapes = format!("{}x{}x{}", plan.shapes.slots, plan.shapes.chunk, plan.shapes.context);
    let spec = model_spec(&functions, &[("mework.graph", GRAPH_VERSION), ("mework.shapes", &shapes)]);

    let staging: PathBuf = out.with_extension("mlpackage.building");
    let _ = fs::remove_dir_all(&staging);
    let data = staging.join("Data").join("com.apple.CoreML");
    fs::create_dir_all(data.join("weights")).map_err(|error| format!("无法创建模型目录: {error}"))?;
    let result = (|| {
        fs::write(staging.join("Manifest.json"), manifest()).map_err(|error| format!("写入模型清单失败: {error}"))?;
        fs::write(data.join("model.mlmodel"), spec).map_err(|error| format!("写入模型描述失败: {error}"))?;
        builder.blobs.write(&data.join("weights").join("weight.bin"), weights, cancel, &mut |written, total| {
            progress(BuildProgress { written, total })
        })?;
        let _ = fs::remove_dir_all(out);
        fs::rename(&staging, out).map_err(|error| format!("无法放置模型包: {error}"))
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result.map(|()| layout)
}
