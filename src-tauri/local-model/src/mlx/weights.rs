//! The MLX build's weight files: `weights.bin`, every tensor at a 16 KiB
//! (Apple silicon page) boundary, and `weights.json` naming each one's dtype,
//! shape and offset.
//!
//! Page alignment lets MLX use the mapped file as GPU buffers without a copy,
//! so the weights stay clean, file-backed pages the system can drop under
//! memory pressure and read back from disk. The conversion from the official
//! checkpoint does once what the model would otherwise redo on every load:
//! text weights only, float16 matrices, `(1 + w)` norms folded, `-exp(A_log)`
//! precomputed, conv taps transposed to `[kernel, channels]`.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::qwen35::{layer_tensor, model_tensor, Config, LayerKind};
use crate::safetensors::{f32_to_f16, SafeTensors};

/// Bumped whenever a tensor's name, layout or transform changes.
pub const FORMAT: &str = "qwen35-mlx-1";
pub const WEIGHTS_FILE: &str = "weights.bin";
pub const INDEX_FILE: &str = "weights.json";
const ALIGN: u64 = 16384;
pub const CANCELLED: &str = "转换已取消";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dtype {
    F16,
    F32,
}

impl Dtype {
    pub fn size(self) -> usize {
        match self {
            Self::F16 => 2,
            Self::F32 => 4,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub dtype: Dtype,
    pub shape: Vec<usize>,
    pub offset: u64,
}

impl Entry {
    pub fn bytes(&self) -> usize {
        self.shape.iter().product::<usize>() * self.dtype.size()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Index {
    pub format: String,
    pub tensors: BTreeMap<String, Entry>,
}

impl Index {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = fs::read_to_string(path).map_err(|error| format!("无法读取 {}: {error}", path.display()))?;
        let index: Self = serde_json::from_str(&text).map_err(|error| format!("{} 格式不对: {error}", path.display()))?;
        if index.format != FORMAT {
            return Err(format!("权重格式 {} 与程序需要的 {FORMAT} 不符", index.format));
        }
        Ok(index)
    }

    /// Checks every tensor lies inside a weight file of `len` bytes.
    pub fn check(&self, len: u64) -> Result<(), String> {
        for (name, entry) in &self.tensors {
            if entry.offset % ALIGN != 0 || entry.offset + entry.bytes() as u64 > len {
                return Err(format!("权重 {name} 超出文件或未对齐"));
            }
        }
        Ok(())
    }
}

enum Transform {
    /// float16 as is (matrices).
    Half,
    /// float32 as is (small vectors).
    Single,
    /// float32 `1 + w` (Qwen3.5's RMSNorm weights).
    PlusOne,
    /// float32 `-exp(w)` (the DeltaNet decay rate from `A_log`).
    NegExp,
    /// float32 `[kernel, channels]` from the `[channels, 1, kernel]` conv weight.
    Taps,
}

struct Planned {
    name: String,
    source: String,
    transform: Transform,
}

fn plan(config: &Config) -> Vec<Planned> {
    let mut out = Vec::new();
    let mut add = |name: String, source: String, transform: Transform| out.push(Planned { name, source, transform });
    add("embed".into(), model_tensor("embed_tokens.weight"), Transform::Half);
    add("norm".into(), model_tensor("norm.weight"), Transform::PlusOne);
    for (layer, kind) in config.layers.iter().enumerate() {
        let src = |suffix: &str| layer_tensor(layer, suffix);
        let name = |short: &str| format!("{layer}.{short}");
        add(name("input_norm"), src("input_layernorm.weight"), Transform::PlusOne);
        add(name("post_norm"), src("post_attention_layernorm.weight"), Transform::PlusOne);
        add(name("gate"), src("mlp.gate_proj.weight"), Transform::Half);
        add(name("up"), src("mlp.up_proj.weight"), Transform::Half);
        add(name("down"), src("mlp.down_proj.weight"), Transform::Half);
        match kind {
            LayerKind::Linear => {
                add(name("qkv"), src("linear_attn.in_proj_qkv.weight"), Transform::Half);
                add(name("z"), src("linear_attn.in_proj_z.weight"), Transform::Half);
                add(name("b"), src("linear_attn.in_proj_b.weight"), Transform::Half);
                add(name("a"), src("linear_attn.in_proj_a.weight"), Transform::Half);
                add(name("conv"), src("linear_attn.conv1d.weight"), Transform::Taps);
                add(name("A"), src("linear_attn.A_log"), Transform::NegExp);
                add(name("dt_bias"), src("linear_attn.dt_bias"), Transform::Single);
                add(name("linear_norm"), src("linear_attn.norm.weight"), Transform::Single);
                add(name("out"), src("linear_attn.out_proj.weight"), Transform::Half);
            }
            LayerKind::Full => {
                add(name("q"), src("self_attn.q_proj.weight"), Transform::Half);
                add(name("k"), src("self_attn.k_proj.weight"), Transform::Half);
                add(name("v"), src("self_attn.v_proj.weight"), Transform::Half);
                add(name("q_norm"), src("self_attn.q_norm.weight"), Transform::PlusOne);
                add(name("k_norm"), src("self_attn.k_norm.weight"), Transform::PlusOne);
                add(name("o"), src("self_attn.o_proj.weight"), Transform::Half);
            }
        }
    }
    out
}

fn align(offset: u64) -> u64 {
    offset.div_ceil(ALIGN) * ALIGN
}

/// Writes `weights.bin` and `weights.json` into `dir` from the official
/// checkpoint; reports `(written, total)` bytes.
pub fn convert(
    config: &Config,
    checkpoint: &SafeTensors,
    dir: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Index, String> {
    let planned = plan(config);
    let mut tensors = BTreeMap::new();
    let mut end = 0u64;
    for item in &planned {
        let info = checkpoint.info(&item.source).ok_or_else(|| format!("权重里缺少张量 {}", item.source))?;
        let (dtype, shape) = match item.transform {
            Transform::Half => (Dtype::F16, info.shape.clone()),
            Transform::Taps => {
                let [channels, 1, kernel] = info.shape[..] else {
                    return Err(format!("{} 的形状不对", item.source));
                };
                (Dtype::F32, vec![kernel, channels])
            }
            _ => (Dtype::F32, info.shape.clone()),
        };
        let entry = Entry { dtype, shape, offset: align(end) };
        end = entry.offset + entry.bytes() as u64;
        tensors.insert(item.name.clone(), entry);
    }
    let total = align(end);
    let index = Index { format: FORMAT.to_string(), tensors };

    fs::create_dir_all(dir).map_err(|error| format!("无法创建 {}: {error}", dir.display()))?;
    let staging = dir.join(format!("{WEIGHTS_FILE}.writing"));
    let io = |error: std::io::Error| format!("写入权重失败: {error}");
    let result = (|| {
        let mut out = BufWriter::with_capacity(1 << 20, File::create(&staging).map_err(io)?);
        let mut position = 0u64;
        let mut buffer: Vec<u8> = Vec::with_capacity(1 << 20);
        for item in &planned {
            if cancel.load(Ordering::Acquire) {
                return Err(CANCELLED.to_string());
            }
            let entry = &index.tensors[&item.name];
            pad(&mut out, &mut position, entry.offset).map_err(io)?;
            let view = checkpoint.tensor(&item.source)?;
            buffer.clear();
            match item.transform {
                Transform::Half => {
                    const CHUNK: usize = 1 << 19;
                    let mut start = 0;
                    while start < view.len() {
                        let stop = (start + CHUNK).min(view.len());
                        buffer.clear();
                        for i in start..stop {
                            buffer.extend_from_slice(&f32_to_f16(view.get_f32(i)).to_le_bytes());
                        }
                        out.write_all(&buffer).map_err(io)?;
                        start = stop;
                    }
                    buffer.clear();
                }
                Transform::Single => (0..view.len()).for_each(|i| buffer.extend_from_slice(&view.get_f32(i).to_le_bytes())),
                Transform::PlusOne => {
                    (0..view.len()).for_each(|i| buffer.extend_from_slice(&(1.0 + view.get_f32(i)).to_le_bytes()))
                }
                Transform::NegExp => {
                    (0..view.len()).for_each(|i| buffer.extend_from_slice(&(-view.get_f32(i).exp()).to_le_bytes()))
                }
                Transform::Taps => {
                    let (kernel, channels) = (entry.shape[0], entry.shape[1]);
                    for t in 0..kernel {
                        for ch in 0..channels {
                            buffer.extend_from_slice(&view.get_f32(ch * kernel + t).to_le_bytes());
                        }
                    }
                }
            }
            out.write_all(&buffer).map_err(io)?;
            position += entry.bytes() as u64;
            progress(position, total);
        }
        pad(&mut out, &mut position, total).map_err(io)?;
        out.flush().map_err(io)?;
        out.into_inner().map_err(|error| io(error.into_error()))?.sync_all().map_err(io)?;
        let text = serde_json::to_string_pretty(&index).expect("index serializes");
        fs::write(dir.join(INDEX_FILE), text).map_err(io)?;
        fs::rename(&staging, dir.join(WEIGHTS_FILE)).map_err(io)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&staging);
    }
    result.map(|()| index)
}

fn pad(out: &mut impl Write, position: &mut u64, target: u64) -> std::io::Result<()> {
    let zeros = [0u8; 4096];
    while *position < target {
        let n = ((target - *position) as usize).min(zeros.len());
        out.write_all(&zeros[..n])?;
        *position += n as u64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_every_text_weight_of_the_release() {
        let config = Config::parse(include_bytes!("../../testdata/qwen3.5-0.8b-config.json")).unwrap();
        let planned = plan(&config);
        // embed + norm, 5 per layer, 9 per DeltaNet layer, 6 per attention layer.
        assert_eq!(planned.len(), 2 + 24 * 5 + 18 * 9 + 6 * 6);
        assert!(planned.iter().any(|p| p.name == "3.q_norm"));
        assert!(planned.iter().any(|p| p.name == "0.A"));
    }

    #[test]
    fn rejects_misaligned_tensors() {
        let mut tensors = BTreeMap::new();
        tensors.insert("a".to_string(), Entry { dtype: Dtype::F16, shape: vec![4], offset: 100 });
        let index = Index { format: FORMAT.into(), tensors };
        assert!(index.check(1 << 20).is_err());
        let ok = Index {
            format: FORMAT.into(),
            tensors: [("a".to_string(), Entry { dtype: Dtype::F32, shape: vec![4], offset: ALIGN })].into(),
        };
        assert!(ok.check(ALIGN + 16).is_ok());
        assert!(ok.check(ALIGN + 15).is_err());
    }
}
