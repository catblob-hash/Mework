//! Converts the official Qwen3.5 checkpoint into GGUF, the only format
//! llama.cpp loads, on the user's machine and without Python.
//!
//! The output is meant to be the file llama.cpp's own converter writes with
//! `convert_hf_to_gguf.py --outtype f16 --no-mtp`: the same metadata keys,
//! types, values and order, the same tensor names, types, shapes and
//! transforms, in the same order. Matching it keeps us on the path llama.cpp
//! tests; anything invented here would only ever be checked by us. That goes
//! down to the bits: the one value computed rather than converted,
//! `ssm_a = -exp(A_log)`, uses the exp torch uses, not the platform's.
//!
//! Weights stream from the mapped checkpoint to the output a few megabytes at
//! a time, so memory stays flat however large the model is, and the file only
//! appears under its final name once it is complete.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Deserialize;
use serde_json::Value as Json;

use crate::qwen35::{layer_tensor, model_tensor, Config, LayerKind, TENSOR_PREFIX};
use crate::safetensors::{bf16_to_f32, f16_to_f32, f32_to_f16, Dtype, SafeTensors, Tensor};

const MAGIC: &[u8; 4] = b"GGUF";
const VERSION: u32 = 3;
/// GGUF's default tensor alignment, so no `general.alignment` key is needed.
pub const ALIGNMENT: u64 = 32;
const ARCH: &str = "qwen35";
/// `LLAMA_FTYPE_MOSTLY_F16`: weight matrices in f16, everything else in f32.
const FILE_TYPE_MOSTLY_F16: u32 = 1;
/// `GGML_QNT_VERSION`; every converter writes it, whatever the type.
const QUANTIZATION_VERSION: u32 = 2;
/// Output bytes produced between writes, progress reports and cancel checks.
const CHUNK_BYTES: usize = 4 << 20;
pub const CANCELLED: &str = "转换已取消";
/// The converted file in the app's llama.cpp build.
pub const GGUF_FILE: &str = "qwen3.5-0.8b-f16.gguf";
/// Names this converter's output in the published build; a change to the
/// output needs a new one.
pub const GGUF_VERSION: &str = "qwen35-gguf-f16-1";
/// The vision projector llama.cpp's `mtmd` loads beside the model.
pub const MMPROJ_FILE: &str = "mmproj-qwen3.5-0.8b-f16.gguf";
/// Names `convert_qwen35_mmproj_to_gguf`'s output in the published build.
pub const MMPROJ_VERSION: &str = "qwen35-mmproj-f16-1";
const CLIP_PROJECTOR: &str = "qwen3vl_merger";

/// `tokenizer.ggml.pre` selects a pre-tokenizer regex built into llama.cpp.
/// The reference converter recognizes it by hashing a sample tokenization;
/// the regex is what that hash stands for, so compare it directly.
const QWEN35_PRE_TOKENIZER: &str = r"(?i:'s|'t|'re|'ve|'m|'ll|'d)|[^\r\n\p{L}\p{N}]?[\p{L}\p{M}]+|\p{N}| ?[^\s\p{L}\p{M}\p{N}]+[\r\n]*|\s*[\r\n]+|\s+(?!\S)|\s+";

// GGUF metadata value types.
const TYPE_U32: u32 = 4;
const TYPE_I32: u32 = 5;
const TYPE_F32: u32 = 6;
const TYPE_BOOL: u32 = 7;
const TYPE_STRING: u32 = 8;
const TYPE_ARRAY: u32 = 9;

// llama.cpp token types.
const TOKEN_NORMAL: i32 = 1;
const TOKEN_CONTROL: i32 = 3;
const TOKEN_USER_DEFINED: i32 = 4;
const TOKEN_UNUSED: i32 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConvertProgress {
    pub written_bytes: u64,
    pub total_bytes: u64,
}

/// Converts the release in `model_dir` (config, tokenizer files and the
/// safetensors weights) into the GGUF file `out`, replacing it atomically.
pub fn convert_qwen35_to_gguf(
    model_dir: &Path,
    out: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(ConvertProgress),
) -> Result<(), String> {
    let config_path = model_dir.join("config.json");
    let config_bytes =
        fs::read(&config_path).map_err(|error| format!("无法读取模型配置 {}: {error}", config_path.display()))?;
    let config = Config::parse(&config_bytes)?;
    let raw_config: Json =
        serde_json::from_slice(&config_bytes).map_err(|error| format!("模型配置无法解析: {error}"))?;

    let checkpoint = Checkpoint::open(model_dir)?;
    let plan = plan_tensors(&checkpoint, &config)?;
    let mut metadata = Metadata::default();
    model_metadata(&config, &raw_config, &mut metadata);
    vocab_metadata(model_dir, &raw_config, config.vocab_size, &mut metadata)?;
    write_gguf(out, metadata, &checkpoint, &plan, cancel, progress)
}

/// Converts the release's vision tower into the projector file llama.cpp's
/// `mtmd` loads beside the model (architecture `clip`): what llama.cpp's
/// converter writes with `--mmproj --outtype f16` (its
/// `Qwen3VLVisionModel`), down to the bytes, like `convert_qwen35_to_gguf`.
pub fn convert_qwen35_mmproj_to_gguf(
    model_dir: &Path,
    out: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(ConvertProgress),
) -> Result<(), String> {
    let config = Config::load(&model_dir.join("config.json"))?;
    let vision = crate::vision::VisionConfig::load(&model_dir.join("config.json"))?;
    let preprocessor: Json = read_json(&model_dir.join("preprocessor_config.json"))?;
    let floats = |key: &str| -> Result<Vec<f32>, String> {
        preprocessor
            .get(key)
            .and_then(Json::as_array)
            .and_then(|values| values.iter().map(|value| value.as_f64().map(|v| v as f32)).collect())
            .ok_or_else(|| format!("preprocessor_config.json 缺少 {key}"))
    };
    let (mean, std) = (floats("image_mean")?, floats("image_std")?);

    let checkpoint = Checkpoint::open(model_dir)?;
    let plan = plan_mmproj(&checkpoint, &vision)?;
    let parameters: u64 = plan.iter().map(|planned| planned.rule.shape.iter().product::<usize>() as u64).sum();

    let mut metadata = Metadata::default();
    let count = |value: usize| Value::U32(value as u32);
    metadata.add("general.architecture", Value::Str("clip".into()));
    metadata.add("general.type", Value::Str("mmproj".into()));
    // What the reference derives from the repository name `Qwen3.5-0.8B` once
    // the parameter count (the tower's) no longer matches the name's size.
    let label = size_label(&config);
    metadata.add("general.name", Value::Str(label.map_or_else(|| "Qwen3.5".into(), |label| format!("Qwen3.5 {label}"))));
    if let Some(label) = label {
        metadata.add("general.finetune", Value::Str(label.to_lowercase()));
    }
    metadata.add("general.basename", Value::Str("Qwen3.5".into()));
    metadata.add("general.size_label", Value::Str(rounded_count(parameters)));
    metadata.add("general.file_type", Value::U32(FILE_TYPE_MOSTLY_F16));
    metadata.add("clip.has_vision_encoder", Value::Bool(true));
    metadata.add("clip.vision.projection_dim", count(config.hidden_size));
    let side = (vision.num_position_embeddings as f64).sqrt() as usize;
    metadata.add("clip.vision.image_size", count(side * vision.patch_size));
    metadata.add("clip.vision.patch_size", count(vision.patch_size));
    metadata.add("clip.vision.embedding_length", count(vision.hidden_size));
    metadata.add("clip.vision.feed_forward_length", count(vision.intermediate_size));
    metadata.add("clip.vision.block_count", count(vision.depth));
    metadata.add("clip.vision.attention.head_count", count(vision.num_heads));
    metadata.add("clip.vision.image_mean", Value::F32s(mean));
    metadata.add("clip.vision.image_std", Value::F32s(std));
    metadata.add("clip.projector_type", Value::Str(CLIP_PROJECTOR.into()));
    metadata.add("clip.use_gelu", Value::Bool(true));
    metadata.add("clip.vision.spatial_merge_size", count(vision.spatial_merge_size));
    // The reference takes the text model's RMSNorm epsilon here.
    metadata.add("clip.vision.attention.layer_norm_epsilon", Value::F32(config.rms_norm_eps));
    metadata.add("clip.vision.is_deepstack_layers", Value::Bools(vec![false; vision.depth]));
    metadata.add("general.quantization_version", Value::U32(QUANTIZATION_VERSION));
    write_gguf(out, metadata, &checkpoint, &plan, cancel, progress)
}

/// gguf-py's `model_weight_count_rounded_notation` (two significant digits at least).
fn rounded_count(count: u64) -> String {
    let count = count as f64;
    let (scaled, suffix) = if count > 1e12 {
        (count * 1e-12, "T")
    } else if count > 1e9 {
        (count * 1e-9, "B")
    } else if count > 1e6 {
        (count * 1e-6, "M")
    } else {
        (count * 1e-3, "K")
    };
    let digits = format!("{}", scaled.round_ties_even() as u64).trim_start_matches('0').len();
    let fix = 2usize.saturating_sub(digits);
    format!("{scaled:.fix$}{suffix}")
}

/// The vision tower's tensors in checkpoint order, as the reference maps them.
fn plan_mmproj(checkpoint: &Checkpoint, vision: &crate::vision::VisionConfig) -> Result<Vec<Planned>, String> {
    let v = vision;
    let (h, merged) = (v.hidden_size, v.hidden_size * v.spatial_merge_size * v.spatial_merge_size);
    let patch = vec![h, v.in_channels, v.temporal_patch_size, v.patch_size, v.patch_size];
    let mut expected: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut expect = |name: String, shape: Vec<usize>| expected.insert(format!("{}{name}", crate::vision::TENSOR_PREFIX), shape);
    expect("patch_embed.proj.weight".into(), patch.clone());
    expect("patch_embed.proj.bias".into(), vec![h]);
    expect("pos_embed.weight".into(), vec![v.num_position_embeddings, h]);
    for (name, shape) in [
        ("merger.norm.weight", vec![h]),
        ("merger.norm.bias", vec![h]),
        ("merger.linear_fc1.weight", vec![merged, merged]),
        ("merger.linear_fc1.bias", vec![merged]),
        ("merger.linear_fc2.weight", vec![v.out_hidden_size, merged]),
        ("merger.linear_fc2.bias", vec![v.out_hidden_size]),
    ] {
        expect(name.into(), shape);
    }
    for block in 0..v.depth {
        for (name, shape) in [
            ("attn.qkv.weight", vec![3 * h, h]),
            ("attn.qkv.bias", vec![3 * h]),
            ("attn.proj.weight", vec![h, h]),
            ("attn.proj.bias", vec![h]),
            ("mlp.linear_fc1.weight", vec![v.intermediate_size, h]),
            ("mlp.linear_fc1.bias", vec![v.intermediate_size]),
            ("mlp.linear_fc2.weight", vec![h, v.intermediate_size]),
            ("mlp.linear_fc2.bias", vec![h]),
            ("norm1.weight", vec![h]),
            ("norm1.bias", vec![h]),
            ("norm2.weight", vec![h]),
            ("norm2.bias", vec![h]),
        ] {
            expect(format!("blocks.{block}.{name}"), shape);
        }
    }
    if v.temporal_patch_size != 2 {
        return Err("视觉投影只支持 temporal_patch_size = 2".into());
    }
    let mut plan = Vec::new();
    let mut seen = 0;
    for (index, file) in checkpoint.files.iter().enumerate() {
        for name in file.names() {
            let Some(local) = name.strip_prefix(crate::vision::TENSOR_PREFIX) else { continue };
            let shape = &file.info(name).expect("listed").shape;
            match expected.get(name) {
                Some(want) if want == shape => seen += 1,
                Some(want) => return Err(format!("张量 {name} 的形状 {shape:?} 与配置不符（应为 {want:?}）")),
                None => return Err(format!("权重里有无法识别的视觉张量 {name}")),
            }
            let mut push = |gguf: String, shape: Vec<usize>, ty: GgmlType, cols: Option<Vec<usize>>| {
                plan.push(Planned {
                    file: index,
                    source: name.to_owned(),
                    rule: Rule { name: gguf, shape, ty, op: Op::Copy, rows: None, cols },
                })
            };
            if local == "patch_embed.proj.weight" {
                // The Conv3d becomes one Conv2d per frame.
                let (c, p) = (v.in_channels, v.patch_size);
                for frame in 0..2 {
                    let cols = (0..c * p * p)
                        .map(|i| {
                            let (channel, pixel) = (i / (p * p), i % (p * p));
                            (channel * 2 + frame) * p * p + pixel
                        })
                        .collect();
                    let gguf = if frame == 0 { "v.patch_embd.weight".to_string() } else { "v.patch_embd.weight.1".into() };
                    push(gguf, vec![h, c, p, p], GgmlType::F16, Some(cols));
                }
                continue;
            }
            let gguf = mmproj_name(local).ok_or_else(|| format!("无法识别的视觉张量 {name}"))?;
            let ty = if gguf == "v.position_embd.weight" || shape.len() <= 1 { GgmlType::F32 } else { GgmlType::F16 };
            push(gguf, shape.clone(), ty, None);
        }
    }
    if seen != expected.len() {
        return Err("权重里缺少视觉张量".into());
    }
    Ok(plan)
}

/// gguf-py's tensor map for the `clip` architecture, as Qwen3-VL uses it.
fn mmproj_name(local: &str) -> Option<String> {
    let fixed = match local {
        "patch_embed.proj.bias" => Some("v.patch_embd.bias"),
        "pos_embed.weight" => Some("v.position_embd.weight"),
        "merger.norm.weight" => Some("v.post_ln.weight"),
        "merger.norm.bias" => Some("v.post_ln.bias"),
        "merger.linear_fc1.weight" => Some("mm.0.weight"),
        "merger.linear_fc1.bias" => Some("mm.0.bias"),
        "merger.linear_fc2.weight" => Some("mm.2.weight"),
        "merger.linear_fc2.bias" => Some("mm.2.bias"),
        _ => None,
    };
    if let Some(name) = fixed {
        return Some(name.into());
    }
    let (block, rest) = local.strip_prefix("blocks.")?.split_once('.')?;
    let block: usize = block.parse().ok()?;
    let (module, kind) = rest.rsplit_once('.')?;
    let short = match module {
        "attn.qkv" => "attn_qkv",
        "attn.proj" => "attn_out",
        "mlp.linear_fc1" => "ffn_up",
        "mlp.linear_fc2" => "ffn_down",
        "norm1" => "ln1",
        "norm2" => "ln2",
        _ => return None,
    };
    Some(format!("v.blk.{block}.{short}.{kind}"))
}

/// Writes `metadata` and the planned tensors to `out`, atomically.
fn write_gguf(
    out: &Path,
    metadata: Metadata,
    checkpoint: &Checkpoint,
    plan: &[Planned],
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(ConvertProgress),
) -> Result<(), String> {
    let entries: Vec<TensorEntry> = plan.iter().map(|planned| planned.rule.entry()).collect();
    let header = encode_header(&metadata, &entries);
    drop(metadata);
    let total = header.len() as u64 + entries.iter().map(|entry| padded(entry.nbytes())).sum::<u64>();

    // Declared before the writer so the file is closed before it is removed.
    let mut partial = PartialFile::new(out)?;
    let file =
        File::create(&partial.path).map_err(|error| format!("无法创建模型文件 {}: {error}", partial.path.display()))?;
    let mut sink = Sink { file: BufWriter::with_capacity(CHUNK_BYTES, file), written: 0, total, cancel, progress };
    sink.write(&header)?;
    for (planned, entry) in plan.iter().zip(&entries) {
        if cancel.load(Ordering::Relaxed) {
            return Err(CANCELLED.into());
        }
        let tensor = checkpoint.files[planned.file].tensor(&planned.source)?;
        convert_tensor(tensor, &planned.rule, &mut |bytes| sink.write(bytes))?;
        let padding = (padded(entry.nbytes()) - entry.nbytes()) as usize;
        if padding > 0 {
            sink.write(&[0; ALIGNMENT as usize][..padding])?;
        }
    }
    if sink.written != total {
        return Err("写入的字节数与预期不符".into());
    }
    let file = sink.file.into_inner().map_err(|error| format!("写入模型文件失败: {}", error.error()))?;
    file.sync_all().map_err(|error| format!("写入模型文件失败: {error}"))?;
    drop(file);
    fs::rename(&partial.path, out).map_err(|error| format!("无法保存模型文件 {}: {error}", out.display()))?;
    partial.keep = true;
    Ok(())
}

// ---------------------------------------------------------------------------
// GGUF encoding

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GgmlType {
    F32,
    F16,
}

impl GgmlType {
    pub fn id(self) -> u32 {
        match self {
            Self::F32 => 0,
            Self::F16 => 1,
        }
    }

    pub fn size(self) -> usize {
        match self {
            Self::F32 => 4,
            Self::F16 => 2,
        }
    }
}

/// The metadata value types this converter writes.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    U32(u32),
    F32(f32),
    Bool(bool),
    Str(String),
    F32s(Vec<f32>),
    I32s(Vec<i32>),
    Bools(Vec<bool>),
    Strs(Vec<String>),
}

impl Value {
    fn is_empty(&self) -> bool {
        match self {
            Self::Str(value) => value.is_empty(),
            Self::F32s(values) => values.is_empty(),
            Self::I32s(values) => values.is_empty(),
            Self::Bools(values) => values.is_empty(),
            Self::Strs(values) => values.is_empty(),
            Self::U32(_) | Self::F32(_) | Self::Bool(_) => false,
        }
    }

    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Self::U32(value) => {
                put_u32(out, TYPE_U32);
                put_u32(out, *value);
            }
            Self::F32(value) => {
                put_u32(out, TYPE_F32);
                out.extend_from_slice(&value.to_le_bytes());
            }
            Self::Bool(value) => {
                put_u32(out, TYPE_BOOL);
                out.push(*value as u8);
            }
            Self::Str(value) => {
                put_u32(out, TYPE_STRING);
                put_str(out, value);
            }
            Self::F32s(values) => {
                put_array_header(out, TYPE_F32, values.len());
                for value in values {
                    out.extend_from_slice(&value.to_le_bytes());
                }
            }
            Self::I32s(values) => {
                put_array_header(out, TYPE_I32, values.len());
                for value in values {
                    out.extend_from_slice(&value.to_le_bytes());
                }
            }
            Self::Bools(values) => {
                put_array_header(out, TYPE_BOOL, values.len());
                out.extend(values.iter().map(|&value| value as u8));
            }
            Self::Strs(values) => {
                put_array_header(out, TYPE_STRING, values.len());
                for value in values {
                    put_str(out, value);
                }
            }
        }
    }
}

/// Key-value metadata in write order.
#[derive(Default, Debug)]
pub struct Metadata(Vec<(String, Value)>);

impl Metadata {
    /// Like gguf-py, empty strings and arrays are not written at all.
    pub fn add(&mut self, key: impl Into<String>, value: Value) {
        if value.is_empty() {
            return;
        }
        let key = key.into();
        debug_assert!(self.get(&key).is_none(), "duplicate key {key}");
        self.0.push((key, value));
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.iter().find(|(name, _)| name == key).map(|(_, value)| value)
    }

    pub fn entries(&self) -> &[(String, Value)] {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TensorEntry {
    pub name: String,
    /// ggml order: `dims[0]` is the contiguous axis, the reverse of PyTorch's shape.
    pub dims: Vec<u64>,
    pub ty: GgmlType,
}

impl TensorEntry {
    pub fn nbytes(&self) -> u64 {
        self.dims.iter().product::<u64>() * self.ty.size() as u64
    }
}

pub fn padded(bytes: u64) -> u64 {
    bytes.div_ceil(ALIGNMENT) * ALIGNMENT
}

/// Everything before the tensor data, zero-padded so the data starts aligned.
/// Each tensor's offset is relative to the data start and padded in turn.
pub fn encode_header(metadata: &Metadata, tensors: &[TensorEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    put_u32(&mut out, VERSION);
    put_u64(&mut out, tensors.len() as u64);
    put_u64(&mut out, metadata.0.len() as u64);
    for (key, value) in &metadata.0 {
        put_str(&mut out, key);
        value.encode(&mut out);
    }
    let mut offset = 0;
    for tensor in tensors {
        put_str(&mut out, &tensor.name);
        put_u32(&mut out, tensor.dims.len() as u32);
        for &dim in &tensor.dims {
            put_u64(&mut out, dim);
        }
        put_u32(&mut out, tensor.ty.id());
        put_u64(&mut out, offset);
        offset += padded(tensor.nbytes());
    }
    out.resize(padded(out.len() as u64) as usize, 0);
    out
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_str(out: &mut Vec<u8>, value: &str) {
    put_u64(out, value.len() as u64);
    out.extend_from_slice(value.as_bytes());
}

fn put_array_header(out: &mut Vec<u8>, item_type: u32, len: usize) {
    put_u32(out, TYPE_ARRAY);
    put_u32(out, item_type);
    put_u64(out, len as u64);
}

// ---------------------------------------------------------------------------
// Model metadata

fn model_metadata(config: &Config, raw: &Json, metadata: &mut Metadata) {
    let arch = |key: &str| format!("{ARCH}.{key}");
    let count = |value: usize| Value::U32(value as u32);
    metadata.add("general.architecture", Value::Str(ARCH.into()));
    metadata.add("general.type", Value::Str("model".into()));
    // What the reference derives from the repository name `Qwen3.5-0.8B`.
    let label = size_label(config);
    let name = label.map_or_else(|| "Qwen3.5".to_owned(), |label| format!("Qwen3.5 {label}"));
    metadata.add("general.name", Value::Str(name));
    metadata.add("general.basename", Value::Str("Qwen3.5".into()));
    if let Some(label) = label {
        metadata.add("general.size_label", Value::Str(label.into()));
    }

    metadata.add(arch("block_count"), count(config.layers.len()));
    metadata.add(arch("context_length"), count(config.max_position_embeddings));
    metadata.add(arch("embedding_length"), count(config.hidden_size));
    metadata.add(arch("feed_forward_length"), count(config.intermediate_size));
    metadata.add(arch("attention.head_count"), count(config.num_attention_heads));
    metadata.add(arch("attention.head_count_kv"), count(config.num_key_value_heads));
    if !config.mrope_section.is_empty() {
        let mut sections: Vec<i32> = config.mrope_section.iter().map(|&section| section as i32).collect();
        sections.resize(4, 0);
        metadata.add(arch("rope.dimension_sections"), Value::I32s(sections));
    }
    metadata.add(arch("rope.freq_base"), Value::F32(config.rope_theta as f32));
    metadata.add(arch("attention.layer_norm_rms_epsilon"), Value::F32(config.rms_norm_eps));
    metadata.add(arch("attention.key_length"), count(config.head_dim));
    metadata.add(arch("attention.value_length"), count(config.head_dim));
    metadata.add("general.file_type", Value::U32(FILE_TYPE_MOSTLY_F16));

    metadata.add(arch("ssm.conv_kernel"), count(config.linear_conv_kernel_dim));
    metadata.add(arch("ssm.state_size"), count(config.linear_key_head_dim));
    metadata.add(arch("ssm.group_count"), count(config.linear_num_key_heads));
    metadata.add(arch("ssm.time_step_rank"), count(config.linear_num_value_heads));
    metadata.add(arch("ssm.inner_size"), count(config.linear_value_dim()));
    let recurrent = config.layers.iter().map(|kind| *kind == LayerKind::Linear).collect();
    metadata.add(arch("attention.recurrent_layers"), Value::Bools(recurrent));
    let interval = text_config_value(raw, "full_attention_interval").and_then(Json::as_u64).unwrap_or(4);
    metadata.add(arch("full_attention_interval"), Value::U32(interval as u32));
    metadata.add(arch("rope.dimension_count"), count(config.rotary_dim));
    if config.mrope_section.is_empty() {
        // llama.cpp requires the sections; this is Qwen3.5's default.
        metadata.add(arch("rope.dimension_sections"), Value::I32s(vec![11, 11, 10, 0]));
    }
    metadata.add("general.quantization_version", Value::U32(QUANTIZATION_VERSION));
}

/// llama.cpp's own names for the Qwen3.5 dense sizes (`llama_model_qwen35`).
fn size_label(config: &Config) -> Option<&'static str> {
    match (config.layers.len(), config.hidden_size) {
        (24, 1024) => Some("0.8B"),
        (24, _) => Some("2B"),
        (32, 2560) => Some("4B"),
        (32, _) => Some("9B"),
        (64, _) => Some("27B"),
        _ => None,
    }
}

/// A hyperparameter as the reference sees it: the root config merged with
/// `text_config`, the latter winning.
fn text_config_value<'a>(raw: &'a Json, key: &str) -> Option<&'a Json> {
    raw.get("text_config").and_then(|text| text.get(key)).or_else(|| raw.get(key))
}

// ---------------------------------------------------------------------------
// Tokenizer metadata

#[derive(Deserialize)]
struct TokenizerFile {
    #[serde(default)]
    added_tokens: Vec<AddedToken>,
    #[serde(default)]
    pre_tokenizer: Json,
    #[serde(default)]
    post_processor: Json,
    model: BpeModel,
}

#[derive(Deserialize)]
struct BpeModel {
    #[serde(rename = "type", default)]
    kind: String,
    vocab: HashMap<String, u64>,
    #[serde(default)]
    merges: Vec<Merge>,
}

/// `tokenizers` writes merges as `"a b"`, or since 0.20 as `["a", "b"]`.
#[derive(Deserialize)]
#[serde(untagged)]
enum Merge {
    Joined(String),
    Pair([String; 2]),
}

#[derive(Clone, Deserialize)]
struct AddedToken {
    id: u64,
    content: String,
    #[serde(default)]
    special: bool,
}

fn vocab_metadata(model_dir: &Path, config: &Json, vocab_size: usize, metadata: &mut Metadata) -> Result<(), String> {
    let tokenizer: TokenizerFile = read_json(&model_dir.join("tokenizer.json"))?;
    let tokenizer_config: Json = read_json(&model_dir.join("tokenizer_config.json"))?;
    check_tokenizer(&tokenizer)?;
    let (tokens, types) = build_vocab(&tokenizer, &tokenizer_config, vocab_size)?;
    metadata.add("tokenizer.ggml.model", Value::Str("gpt2".into()));
    metadata.add("tokenizer.ggml.pre", Value::Str("qwen35".into()));
    metadata.add("tokenizer.ggml.tokens", Value::Strs(tokens));
    metadata.add("tokenizer.ggml.token_type", Value::I32s(types));
    metadata.add("tokenizer.ggml.merges", Value::Strs(merges(&tokenizer)));
    special_tokens(&tokenizer, &tokenizer_config, config, metadata)?;
    let template = match tokenizer_config.get("chat_template") {
        Some(template) => template.as_str().map(str::to_owned),
        None => chat_template_file(model_dir)?,
    };
    if let Some(template) = template {
        metadata.add("tokenizer.chat_template", Value::Str(template));
    }
    Ok(())
}

fn check_tokenizer(tokenizer: &TokenizerFile) -> Result<(), String> {
    if tokenizer.model.kind != "BPE" {
        return Err(format!("分词器类型 {:?} 不受支持", tokenizer.model.kind));
    }
    let steps = |value: &Json, list: &str| -> Vec<Json> {
        match value.get(list).and_then(Json::as_array) {
            Some(items) => items.clone(),
            None if value.is_null() => Vec::new(),
            None => vec![value.clone()],
        }
    };
    let pre = steps(&tokenizer.pre_tokenizer, "pretokenizers");
    let splits_like_qwen35 = pre
        .iter()
        .any(|step| step["type"] == "Split" && step["pattern"]["Regex"].as_str() == Some(QWEN35_PRE_TOKENIZER));
    let byte_level = pre.iter().any(|step| step["type"] == "ByteLevel");
    if !splits_like_qwen35 || !byte_level {
        return Err("分词器的预分词规则与 Qwen3.5 不同，无法转换".into());
    }
    // These would make the reference set add_bos/add_eos differently.
    for step in steps(&tokenizer.post_processor, "processors") {
        if let Some(kind @ ("TemplateProcessing" | "RobertaProcessing")) = step["type"].as_str() {
            return Err(format!("不支持分词器的后处理 {kind}"));
        }
    }
    Ok(())
}

/// Every id below `vocab_size`: the BPE vocabulary, then the added tokens
/// from `tokenizer.json` and those only listed in `tokenizer_config.json`
/// (transformers registers both), and `[PADn]` placeholders for the rest.
fn build_vocab(
    tokenizer: &TokenizerFile,
    tokenizer_config: &Json,
    vocab_size: usize,
) -> Result<(Vec<String>, Vec<i32>), String> {
    let mut slots: Vec<Option<(String, i32)>> = vec![None; vocab_size];
    let slot = |id: u64| {
        usize::try_from(id)
            .ok()
            .filter(|&id| id < vocab_size)
            .ok_or_else(|| format!("分词器里的编号 {id} 超出词表大小 {vocab_size}"))
    };
    for (text, &id) in &tokenizer.model.vocab {
        slots[slot(id)?] = Some((text.clone(), TOKEN_NORMAL));
    }
    let from_config =
        tokenizer_config.get("added_tokens_decoder").and_then(Json::as_object).into_iter().flatten().filter_map(
            |(id, entry)| {
                Some(AddedToken {
                    id: id.parse().ok()?,
                    content: entry.get("content")?.as_str()?.to_owned(),
                    special: entry.get("special").and_then(Json::as_bool).unwrap_or(false),
                })
            },
        );
    let mut seen = HashSet::new();
    for token in tokenizer.added_tokens.iter().cloned().chain(from_config) {
        if !seen.insert(token.id) {
            continue;
        }
        let entry = if token.special || looks_special(&token.content) {
            (token.content, TOKEN_CONTROL)
        } else {
            // Pre-normalize SentencePiece spaces, as the reference does.
            (token.content.replace('\u{2581}', " "), TOKEN_USER_DEFINED)
        };
        slots[slot(token.id)?] = Some(entry);
    }
    Ok(slots
        .into_iter()
        .enumerate()
        .map(|(id, entry)| entry.unwrap_or_else(|| (format!("[PAD{id}]"), TOKEN_UNUSED)))
        .unzip())
}

/// Added tokens that ought to be control tokens even when not marked special.
fn looks_special(token: &str) -> bool {
    matches!(token, "<pad>" | "<mask>" | "<2mass>" | "[@BOS@]")
        || (token.starts_with("<|") && token.ends_with("|>"))
        || (token.starts_with("<｜") && token.ends_with("｜>"))
        || (token.starts_with("<unused") && token.ends_with('>'))
}

fn merges(tokenizer: &TokenizerFile) -> Vec<String> {
    // Spaces inside a pair would split the joined form; the reference shifts them to U+0120.
    let shift_spaces = |part: &str| part.replace(' ', "\u{0120}");
    tokenizer
        .model
        .merges
        .iter()
        .map(|merge| match merge {
            Merge::Joined(joined) => joined.clone(),
            Merge::Pair([left, right]) => format!("{} {}", shift_spaces(left), shift_spaces(right)),
        })
        .collect()
}

/// gguf-py's `SpecialVocab`: token ids named in `tokenizer_config.json`
/// (resolved through `tokenizer.json`'s added tokens), then ids from
/// `config.json`, first one wins; then the `add_*_token` flags.
fn special_tokens(
    tokenizer: &TokenizerFile,
    tokenizer_config: &Json,
    config: &Json,
    metadata: &mut Metadata,
) -> Result<(), String> {
    const TYPES: [&str; 7] = ["bos", "eos", "unk", "sep", "pad", "cls", "mask"];
    let token_content = |typ: &str| match tokenizer_config.get(format!("{typ}_token")) {
        Some(Json::String(content)) => Some(content.as_str()),
        Some(Json::Object(entry)) => entry.get("content").and_then(Json::as_str),
        _ => None,
    };
    let fallbacks = [("bos", "cls"), ("eos", "sep")];
    let mut ids: Vec<(&str, u64)> = Vec::new();
    let mut set = |typ: &'static str, id: Option<&Json>| -> Result<(), String> {
        let Some(id) = id.filter(|id| id.is_i64() || id.is_u64()) else {
            return Ok(());
        };
        let id = id.as_u64().ok_or_else(|| format!("特殊词元 {typ} 的编号无效"))?;
        if !ids.iter().any(|(name, _)| *name == typ) {
            ids.push((typ, id));
        }
        Ok(())
    };
    let mut adds = Vec::new();
    for typ in TYPES {
        if let Some(add) = tokenizer_config.get(format!("add_{typ}_token")).and_then(Json::as_bool) {
            adds.push((typ, add));
        }
        let fallback = fallbacks.iter().find(|(name, _)| *name == typ).map(|(_, other)| *other);
        let Some(content) = token_content(typ).or_else(|| fallback.and_then(token_content)) else {
            continue;
        };
        let id = tokenizer.added_tokens.iter().find(|token| token.content == content).map(|token| Json::from(token.id));
        set(typ, id.as_ref())?;
    }
    for typ in TYPES {
        let key = format!("{typ}_token_id");
        set(typ, config.get(&key).filter(|id| !id.is_null()).or_else(|| config.get("text_config")?.get(&key)))?;
    }
    for (typ, id) in ids {
        let key = match typ {
            "bos" => "tokenizer.ggml.bos_token_id",
            "eos" => "tokenizer.ggml.eos_token_id",
            "unk" => "tokenizer.ggml.unknown_token_id",
            "sep" => "tokenizer.ggml.seperator_token_id",
            "pad" => "tokenizer.ggml.padding_token_id",
            "mask" => "tokenizer.ggml.mask_token_id",
            _ => continue,
        };
        metadata.add(key, Value::U32(id as u32));
    }
    for (typ, add) in adds {
        if matches!(typ, "bos" | "eos" | "sep") {
            metadata.add(format!("tokenizer.ggml.add_{typ}_token"), Value::Bool(add));
        }
    }
    Ok(())
}

fn chat_template_file(model_dir: &Path) -> Result<Option<String>, String> {
    let jinja = model_dir.join("chat_template.jinja");
    if jinja.is_file() {
        return fs::read_to_string(&jinja)
            .map(Some)
            .map_err(|error| format!("无法读取对话模板 {}: {error}", jinja.display()));
    }
    let json = model_dir.join("chat_template.json");
    if json.is_file() {
        let value: Json = read_json(&json)?;
        return Ok(value.get("chat_template").and_then(Json::as_str).map(str::to_owned));
    }
    Ok(None)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let bytes = fs::read(path).map_err(|error| format!("无法读取 {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("{} 无法解析: {error}", path.display()))
}

// ---------------------------------------------------------------------------
// Tensors

struct Checkpoint {
    files: Vec<SafeTensors>,
}

impl Checkpoint {
    fn open(model_dir: &Path) -> Result<Self, String> {
        let files = weight_files(model_dir)?.iter().map(|path| SafeTensors::open(path)).collect::<Result<_, _>>()?;
        Ok(Self { files })
    }
}

/// The shards named by `model.safetensors.index.json` when they are all
/// there, otherwise every `model*.safetensors` in the directory, sorted —
/// the files the reference converter would read.
fn weight_files(model_dir: &Path) -> Result<Vec<PathBuf>, String> {
    #[derive(Deserialize)]
    struct Index {
        weight_map: BTreeMap<String, String>,
    }
    let index_path = model_dir.join("model.safetensors.index.json");
    if index_path.is_file() {
        let index: Index = read_json(&index_path)?;
        let names: BTreeSet<&String> = index.weight_map.values().collect();
        let paths: Vec<PathBuf> = names
            .iter()
            .filter(|name| Path::new(name.as_str()).file_name() == Some(std::ffi::OsStr::new(name.as_str())))
            .map(|name| model_dir.join(name))
            .collect();
        if !paths.is_empty() && paths.len() == names.len() && paths.iter().all(|path| path.is_file()) {
            return Ok(paths);
        }
    }
    let entries =
        fs::read_dir(model_dir).map_err(|error| format!("无法读取模型目录 {}: {error}", model_dir.display()))?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("无法读取模型目录: {error}"))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("model") && name.ends_with(".safetensors") && entry.path().is_file() {
            paths.push(entry.path());
        }
    }
    if paths.is_empty() {
        return Err(format!("模型目录 {} 里没有权重文件", model_dir.display()));
    }
    paths.sort();
    Ok(paths)
}

struct Planned {
    file: usize,
    source: String,
    rule: Rule,
}

/// The language-model tensors in the reference's order (files in order,
/// names sorted within each), after checking the checkpoint holds exactly
/// the tensors llama.cpp will ask for, with the shapes it expects.
fn plan_tensors(checkpoint: &Checkpoint, config: &Config) -> Result<Vec<Planned>, String> {
    let mut location = HashMap::new();
    for (index, file) in checkpoint.files.iter().enumerate() {
        for name in file.names() {
            if location.insert(name, index).is_some() {
                return Err(format!("张量 {name} 出现在多个权重文件里"));
            }
        }
    }
    let with_lm_head = !config.tie_word_embeddings || location.contains_key("lm_head.weight");
    let expected = expected_tensors(config, with_lm_head);
    for (name, shape) in &expected {
        let Some(&index) = location.get(name.as_str()) else {
            return Err(format!("权重里缺少张量 {name}"));
        };
        let actual = &checkpoint.files[index].info(name).expect("indexed above").shape;
        if actual != shape {
            return Err(format!("张量 {name} 的形状 {actual:?} 与配置不符（应为 {shape:?}）"));
        }
    }
    let expected: HashSet<&str> = expected.iter().map(|(name, _)| name.as_str()).collect();
    let mut plan = Vec::with_capacity(expected.len());
    for (index, file) in checkpoint.files.iter().enumerate() {
        for name in file.names() {
            let shape = &file.info(name).expect("listed").shape;
            let Some(rule) = rule_for(name, shape, config)? else {
                continue;
            };
            // llama.cpp refuses a file with tensors it did not ask for.
            if !expected.contains(name) {
                return Err(format!("权重里有多余的张量 {name}"));
            }
            plan.push(Planned { file: index, source: name.to_owned(), rule });
        }
    }
    Ok(plan)
}

/// Checkpoint names and shapes of the text model, as `llama_model_qwen35` loads it.
fn expected_tensors(config: &Config, with_lm_head: bool) -> Vec<(String, Vec<usize>)> {
    let c = config;
    let h = c.hidden_size;
    let mut out =
        vec![(model_tensor("embed_tokens.weight"), vec![c.vocab_size, h]), (model_tensor("norm.weight"), vec![h])];
    if with_lm_head {
        out.push(("lm_head.weight".into(), vec![c.vocab_size, h]));
    }
    for (layer, kind) in c.layers.iter().enumerate() {
        let mut add = |suffix: &str, shape: Vec<usize>| out.push((layer_tensor(layer, suffix), shape));
        add("input_layernorm.weight", vec![h]);
        add("post_attention_layernorm.weight", vec![h]);
        match kind {
            LayerKind::Full => {
                let q = c.num_attention_heads * c.head_dim;
                let kv = c.num_key_value_heads * c.head_dim;
                // The query projection also produces the output gate.
                add("self_attn.q_proj.weight", vec![2 * q, h]);
                add("self_attn.k_proj.weight", vec![kv, h]);
                add("self_attn.v_proj.weight", vec![kv, h]);
                add("self_attn.o_proj.weight", vec![h, q]);
                add("self_attn.q_norm.weight", vec![c.head_dim]);
                add("self_attn.k_norm.weight", vec![c.head_dim]);
            }
            LayerKind::Linear => {
                add("linear_attn.in_proj_qkv.weight", vec![c.linear_conv_dim(), h]);
                add("linear_attn.in_proj_z.weight", vec![c.linear_value_dim(), h]);
                add("linear_attn.in_proj_a.weight", vec![c.linear_num_value_heads, h]);
                add("linear_attn.in_proj_b.weight", vec![c.linear_num_value_heads, h]);
                add("linear_attn.conv1d.weight", vec![c.linear_conv_dim(), 1, c.linear_conv_kernel_dim]);
                add("linear_attn.A_log", vec![c.linear_num_value_heads]);
                add("linear_attn.dt_bias", vec![c.linear_num_value_heads]);
                add("linear_attn.norm.weight", vec![c.linear_value_head_dim]);
                add("linear_attn.out_proj.weight", vec![h, c.linear_value_dim()]);
            }
        }
        add("mlp.gate_proj.weight", vec![c.intermediate_size, h]);
        add("mlp.up_proj.weight", vec![c.intermediate_size, h]);
        add("mlp.down_proj.weight", vec![h, c.intermediate_size]);
    }
    out
}

/// Checkpoint suffix under `layers.N.` → GGUF name under `blk.N.` (gguf-py's
/// tensor map for `qwen35`).
const LAYER_TENSORS: &[(&str, &str)] = &[
    ("input_layernorm.weight", "attn_norm.weight"),
    ("post_attention_layernorm.weight", "post_attention_norm.weight"),
    ("self_attn.q_proj.weight", "attn_q.weight"),
    ("self_attn.k_proj.weight", "attn_k.weight"),
    ("self_attn.v_proj.weight", "attn_v.weight"),
    ("self_attn.o_proj.weight", "attn_output.weight"),
    ("self_attn.q_norm.weight", "attn_q_norm.weight"),
    ("self_attn.k_norm.weight", "attn_k_norm.weight"),
    ("linear_attn.in_proj_qkv.weight", "attn_qkv.weight"),
    ("linear_attn.in_proj_z.weight", "attn_gate.weight"),
    ("linear_attn.in_proj_a.weight", "ssm_alpha.weight"),
    ("linear_attn.in_proj_b.weight", "ssm_beta.weight"),
    ("linear_attn.conv1d.weight", "ssm_conv1d.weight"),
    ("linear_attn.A_log", "ssm_a"),
    ("linear_attn.dt_bias", "ssm_dt.bias"),
    ("linear_attn.norm.weight", "ssm_norm.weight"),
    ("linear_attn.out_proj.weight", "ssm_out.weight"),
    ("mlp.gate_proj.weight", "ffn_gate.weight"),
    ("mlp.up_proj.weight", "ffn_up.weight"),
    ("mlp.down_proj.weight", "ffn_down.weight"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Copy,
    /// Qwen3.5 stores RMSNorm weights as offsets from one.
    AddOne,
    /// `A_log` → the decay rate `-exp(A_log)` llama.cpp uses directly.
    NegExp,
}

/// How one checkpoint tensor becomes one GGUF tensor.
#[derive(Debug, PartialEq)]
struct Rule {
    name: String,
    /// PyTorch order, after any squeeze.
    shape: Vec<usize>,
    ty: GgmlType,
    op: Op,
    /// Output row → source row, where V heads are reordered.
    rows: Option<Vec<usize>>,
    /// Output column → source column, likewise.
    cols: Option<Vec<usize>>,
}

impl Rule {
    fn entry(&self) -> TensorEntry {
        TensorEntry {
            name: self.name.clone(),
            dims: self.shape.iter().rev().map(|&dim| dim as u64).collect(),
            ty: self.ty,
        }
    }
}

/// The reference's treatment of one checkpoint tensor: `None` for the vision
/// tower, the MTP head and anything else outside the language model.
fn rule_for(name: &str, shape: &[usize], config: &Config) -> Result<Option<Rule>, String> {
    let local = match name.strip_prefix(TENSOR_PREFIX) {
        Some(local) => local,
        None if name == "lm_head.weight" => name,
        None => return Ok(None),
    };
    let gguf_name = gguf_name(local, config.layers.len()).ok_or_else(|| format!("无法识别的张量 {name}"))?;

    // Qwen3NextModel.modify_tensors
    let mut shape = shape.to_vec();
    let op = if local.ends_with(".A_log") {
        Op::NegExp
    } else if local.ends_with(".dt_bias") {
        Op::Copy
    } else if local.contains("conv1d") {
        shape.retain(|&dim| dim != 1);
        Op::Copy
    } else if local.ends_with("norm.weight") && !local.ends_with("linear_attn.norm.weight") {
        Op::AddOne
    } else {
        Op::Copy
    };
    let (rows, cols) = v_head_reorder(local, &shape, config)?;
    let ty = output_type(&gguf_name, shape.len());
    Ok(Some(Rule { name: gguf_name, shape, ty, op, rows, cols }))
}

fn gguf_name(local: &str, layers: usize) -> Option<String> {
    match local {
        "embed_tokens.weight" => return Some("token_embd.weight".into()),
        "norm.weight" => return Some("output_norm.weight".into()),
        "lm_head.weight" => return Some("output.weight".into()),
        _ => {}
    }
    let (layer, suffix) = local.strip_prefix("layers.")?.split_once('.')?;
    let layer: usize = layer.parse().ok().filter(|&layer| layer < layers)?;
    let (_, short) = LAYER_TENSORS.iter().find(|(hf, _)| *hf == suffix)?;
    Some(format!("blk.{layer}.{short}"))
}

/// `--outtype f16`: f16 for weight matrices; f32 for 1-D tensors, norms, the
/// short convolution and anything that is not a `.weight`.
fn output_type(gguf_name: &str, dims: usize) -> GgmlType {
    if dims <= 1
        || gguf_name.ends_with("_norm.weight")
        || gguf_name.ends_with(".ssm_conv1d.weight")
        || !gguf_name.ends_with(".weight")
    {
        GgmlType::F32
    } else {
        GgmlType::F16
    }
}

/// With fewer K heads than V heads, the checkpoint groups V heads by K head
/// while ggml broadcasts K heads tiled across V heads; the reference
/// (`_LinearAttentionVReorderBase`) reorders every V-indexed axis to match.
/// Row and column orders, as in [`Rule`].
type Reorder = (Option<Vec<usize>>, Option<Vec<usize>>);

fn v_head_reorder(local: &str, shape: &[usize], config: &Config) -> Result<Reorder, String> {
    let (k_heads, v_heads) = (config.linear_num_key_heads, config.linear_num_value_heads);
    if k_heads == v_heads || !local.contains("linear_attn.") {
        return Ok((None, None));
    }
    let per_k = v_heads / k_heads;
    let v_dim = config.linear_value_head_dim;
    let order = |head_dim: usize| v_head_order(k_heads, per_k, head_dim);
    let after_qk = |head_dim: usize| {
        let qk = 2 * config.linear_key_dim();
        (0..qk).chain(order(head_dim).into_iter().map(|index| qk + index)).collect::<Vec<_>>()
    };
    let (rows, cols) = if local.contains(".in_proj_qkv.") || local.contains(".conv1d") {
        (Some(after_qk(v_dim)), None)
    } else if local.contains(".in_proj_z.") {
        (Some(order(v_dim)), None)
    } else if local.contains(".in_proj_b.") || local.contains(".in_proj_a.") {
        (Some(order(1)), None)
    } else if local.contains(".A_log") || local.contains(".dt_bias") || local.contains(".dt_proj") {
        if shape.len() == 1 {
            (Some(order(1)), None)
        } else {
            (None, Some(order(1)))
        }
    } else if local.contains(".out_proj.") {
        (None, Some(order(v_dim)))
    } else {
        (None, None)
    };
    let row_count = shape.first().copied().unwrap_or(1);
    let col_count = shape.iter().skip(1).product::<usize>();
    if rows.as_ref().is_some_and(|rows| rows.len() != row_count)
        || cols.as_ref().is_some_and(|cols| cols.len() != col_count)
    {
        return Err(format!("张量 {local} 的形状 {shape:?} 无法按 V 头重排"));
    }
    Ok((rows, cols))
}

/// Tiled position of every grouped V element: output head `v * k_heads + k`
/// comes from source head `k * per_k + v`.
fn v_head_order(k_heads: usize, per_k: usize, head_dim: usize) -> Vec<usize> {
    let mut order = Vec::with_capacity(k_heads * per_k * head_dim);
    for v in 0..per_k {
        for k in 0..k_heads {
            let source = (k * per_k + v) * head_dim;
            order.extend(source..source + head_dim);
        }
    }
    order
}

/// Streams one tensor's converted bytes to `emit`, a chunk at a time.
fn convert_tensor(
    source: Tensor<'_>,
    rule: &Rule,
    emit: &mut dyn FnMut(&[u8]) -> Result<(), String>,
) -> Result<(), String> {
    let rows = rule.shape.first().copied().unwrap_or(1);
    let cols = rule.shape.iter().skip(1).product::<usize>();
    // A column order may also pick a subset of the source's columns.
    let source_cols = if rule.cols.is_some() { source.len() / rows } else { cols };
    if rows * source_cols != source.len() || rule.cols.as_ref().is_some_and(|order| order.iter().any(|c| *c >= source_cols)) {
        return Err(format!("张量 {} 的元素数与形状不符", rule.name));
    }
    let same_type = matches!((source.dtype, rule.ty), (Dtype::F32, GgmlType::F32) | (Dtype::F16, GgmlType::F16));
    if same_type && rule.op == Op::Copy && rule.rows.is_none() && rule.cols.is_none() {
        for chunk in source.bytes.chunks(CHUNK_BYTES) {
            emit(chunk)?;
        }
        return Ok(());
    }

    let rows_per_chunk = (CHUNK_BYTES / (cols * rule.ty.size()).max(1)).max(1);
    let mut row = vec![0f32; source_cols];
    let mut permuted = vec![0f32; if rule.cols.is_some() { cols } else { 0 }];
    let mut buffer = Vec::with_capacity(rows_per_chunk.min(rows) * cols * rule.ty.size());
    let mut start = 0;
    while start < rows {
        let end = (start + rows_per_chunk).min(rows);
        buffer.clear();
        for out_row in start..end {
            let source_row = rule.rows.as_ref().map_or(out_row, |rows| rows[out_row]);
            decode(&source, source_row * source_cols, &mut row);
            let values = match &rule.cols {
                Some(order) => {
                    for (value, &col) in permuted.iter_mut().zip(order) {
                        *value = row[col];
                    }
                    &mut permuted
                }
                None => &mut row,
            };
            apply(rule.op, values);
            encode(rule.ty, values, &mut buffer);
        }
        emit(&buffer)?;
        start = end;
    }
    Ok(())
}

fn decode(source: &Tensor<'_>, start: usize, out: &mut [f32]) {
    let size = source.dtype.size();
    let bytes = &source.bytes[start * size..(start + out.len()) * size];
    match source.dtype {
        Dtype::BF16 => {
            for (value, raw) in out.iter_mut().zip(bytes.chunks_exact(2)) {
                *value = bf16_to_f32(u16::from_le_bytes([raw[0], raw[1]]));
            }
        }
        Dtype::F16 => {
            for (value, raw) in out.iter_mut().zip(bytes.chunks_exact(2)) {
                *value = f16_to_f32(u16::from_le_bytes([raw[0], raw[1]]));
            }
        }
        Dtype::F32 => {
            for (value, raw) in out.iter_mut().zip(bytes.chunks_exact(4)) {
                *value = f32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
            }
        }
    }
}

fn apply(op: Op, values: &mut [f32]) {
    match op {
        Op::Copy => {}
        Op::AddOne => values.iter_mut().for_each(|value| *value += 1.0),
        Op::NegExp => values.iter_mut().for_each(|value| *value = -sleef_expf(*value)),
    }
}

/// `expf` as torch computes it on CPU: SLEEF's `expf_u10`, an FMA-based
/// algorithm that yields the same bits on NEON, AVX2 and AVX-512. It is within
/// an ulp but not correctly rounded, so neither the platform's `expf` nor a
/// correctly rounded one reproduces the reference.
fn sleef_expf(d: f32) -> f32 {
    let pow2 = |q: i32| f32::from_bits(((q + 127) as u32) << 23);
    let q = (d * std::f32::consts::LOG2_E).round_ties_even();
    let s = q.mul_add(-0.69314575, d);
    let s = q.mul_add(-1.4286068e-6, s);
    let mut u = 0.00019852762f32;
    for c in [0.0013930436, 0.008333361, 0.041666485, 0.16666667, 0.5] {
        u = u.mul_add(s, c);
    }
    let u = 1.0 + (s * s).mul_add(u, s);
    if d < -104.0 {
        0.0
    } else if d > 100.0 {
        f32::INFINITY
    } else {
        // In two steps, as SLEEF does, so neither factor leaves the normal range.
        let q = q as i32;
        u * pow2(q >> 1) * pow2(q - (q >> 1))
    }
}

fn encode(ty: GgmlType, values: &[f32], out: &mut Vec<u8>) {
    match ty {
        GgmlType::F32 => values.iter().for_each(|value| out.extend_from_slice(&value.to_le_bytes())),
        GgmlType::F16 => values.iter().for_each(|&value| out.extend_from_slice(&f32_to_f16(value).to_le_bytes())),
    }
}

// ---------------------------------------------------------------------------
// Output

/// The output under a temporary name in the same directory, removed unless
/// the conversion finishes and renames it into place.
struct PartialFile {
    path: PathBuf,
    keep: bool,
}

impl PartialFile {
    fn new(out: &Path) -> Result<Self, String> {
        let name = out.file_name().ok_or_else(|| format!("输出路径无效: {}", out.display()))?;
        let mut partial = name.to_os_string();
        partial.push(".converting");
        Ok(Self { path: out.with_file_name(partial), keep: false })
    }
}

impl Drop for PartialFile {
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_file(&self.path);
        }
    }
}

struct Sink<'a> {
    file: BufWriter<File>,
    written: u64,
    total: u64,
    cancel: &'a AtomicBool,
    progress: &'a mut dyn FnMut(ConvertProgress),
}

impl Sink<'_> {
    fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.file.write_all(bytes).map_err(|error| format!("写入模型文件失败: {error}"))?;
        self.written += bytes.len() as u64;
        (self.progress)(ConvertProgress { written_bytes: self.written, total_bytes: self.total });
        if self.cancel.load(Ordering::Relaxed) {
            return Err(CANCELLED.into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELEASE_CONFIG: &str = include_str!("../testdata/qwen3.5-0.8b-config.json");

    /// A GGUF header read back: metadata, tensor infos and where data starts.
    struct Parsed {
        version: u32,
        metadata: Vec<(String, Value)>,
        tensors: Vec<(String, Vec<u64>, u32, u64)>,
        data_start: usize,
    }

    impl Parsed {
        fn get(&self, key: &str) -> Option<&Value> {
            self.metadata.iter().find(|(name, _)| name == key).map(|(_, value)| value)
        }
    }

    struct Cursor<'a> {
        bytes: &'a [u8],
        at: usize,
    }

    impl<'a> Cursor<'a> {
        fn take(&mut self, n: usize) -> &'a [u8] {
            let slice = &self.bytes[self.at..self.at + n];
            self.at += n;
            slice
        }
        fn u8(&mut self) -> u8 {
            self.take(1)[0]
        }
        fn u32(&mut self) -> u32 {
            u32::from_le_bytes(self.take(4).try_into().unwrap())
        }
        fn u64(&mut self) -> u64 {
            u64::from_le_bytes(self.take(8).try_into().unwrap())
        }
        fn str(&mut self) -> String {
            let len = self.u64() as usize;
            String::from_utf8(self.take(len).to_vec()).unwrap()
        }
        fn value(&mut self) -> Value {
            match self.u32() {
                TYPE_U32 => Value::U32(self.u32()),
                TYPE_F32 => Value::F32(f32::from_bits(self.u32())),
                TYPE_BOOL => Value::Bool(self.u8() != 0),
                TYPE_STRING => Value::Str(self.str()),
                TYPE_ARRAY => {
                    let item = self.u32();
                    let len = self.u64() as usize;
                    match item {
                        TYPE_I32 => Value::I32s((0..len).map(|_| self.u32() as i32).collect()),
                        TYPE_F32 => Value::F32s((0..len).map(|_| f32::from_bits(self.u32())).collect()),
                        TYPE_BOOL => Value::Bools((0..len).map(|_| self.u8() != 0).collect()),
                        TYPE_STRING => Value::Strs((0..len).map(|_| self.str()).collect()),
                        other => panic!("array of {other}"),
                    }
                }
                other => panic!("value type {other}"),
            }
        }
    }

    fn parse(bytes: &[u8]) -> Parsed {
        let mut cursor = Cursor { bytes, at: 0 };
        assert_eq!(cursor.take(4), MAGIC);
        let version = cursor.u32();
        let tensor_count = cursor.u64();
        let kv_count = cursor.u64();
        let metadata = (0..kv_count).map(|_| (cursor.str(), cursor.value())).collect();
        let tensors = (0..tensor_count)
            .map(|_| {
                let name = cursor.str();
                let dims = (0..cursor.u32()).map(|_| cursor.u64()).collect();
                (name, dims, cursor.u32(), cursor.u64())
            })
            .collect();
        let data_start = padded(cursor.at as u64) as usize;
        Parsed { version, metadata, tensors, data_start }
    }

    fn release_config() -> Config {
        Config::parse(RELEASE_CONFIG.as_bytes()).unwrap()
    }

    fn bf16_bytes(values: &[f32]) -> Vec<u8> {
        values.iter().flat_map(|value| ((value.to_bits() >> 16) as u16).to_le_bytes()).collect()
    }

    fn convert(source: Tensor<'_>, rule: &Rule) -> Vec<u8> {
        let mut out = Vec::new();
        convert_tensor(source, rule, &mut |bytes| {
            out.extend_from_slice(bytes);
            Ok(())
        })
        .unwrap();
        out
    }

    fn f32s(bytes: &[u8]) -> Vec<f32> {
        bytes.chunks_exact(4).map(|raw| f32::from_le_bytes(raw.try_into().unwrap())).collect()
    }

    fn f16s(bytes: &[u8]) -> Vec<u16> {
        bytes.chunks_exact(2).map(|raw| u16::from_le_bytes(raw.try_into().unwrap())).collect()
    }

    #[test]
    fn encodes_values_like_gguf_py() {
        let mut out = Vec::new();
        Value::U32(7).encode(&mut out);
        assert_eq!(out, [4, 0, 0, 0, 7, 0, 0, 0]);

        out.clear();
        Value::Str("ab".into()).encode(&mut out);
        assert_eq!(out, [8, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, b'a', b'b']);

        out.clear();
        Value::Bools(vec![true, false]).encode(&mut out);
        assert_eq!(out, [9, 0, 0, 0, 7, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 1, 0]);

        out.clear();
        Value::I32s(vec![-1]).encode(&mut out);
        assert_eq!(out, [9, 0, 0, 0, 5, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0xff]);

        out.clear();
        Value::Strs(vec!["x".into()]).encode(&mut out);
        assert_eq!(out, [9, 0, 0, 0, 8, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, b'x']);

        out.clear();
        Value::F32(1e-6).encode(&mut out);
        assert_eq!(out[..4], [6, 0, 0, 0]);
        assert_eq!(f32::from_le_bytes(out[4..].try_into().unwrap()), 1e-6);
    }

    #[test]
    fn header_lays_out_tensors_aligned() {
        let mut metadata = Metadata::default();
        metadata.add("general.architecture", Value::Str("qwen35".into()));
        metadata.add("skipped.string", Value::Str(String::new()));
        metadata.add("skipped.array", Value::Strs(Vec::new()));
        metadata.add("x.flag", Value::Bool(true));
        let tensors = vec![
            TensorEntry { name: "a".into(), dims: vec![3], ty: GgmlType::F32 },
            TensorEntry { name: "b".into(), dims: vec![5, 2], ty: GgmlType::F16 },
            TensorEntry { name: "c".into(), dims: vec![1], ty: GgmlType::F32 },
        ];
        let header = encode_header(&metadata, &tensors);
        assert_eq!(header.len() % ALIGNMENT as usize, 0);
        let parsed = parse(&header);
        assert_eq!(parsed.version, 3);
        assert_eq!(parsed.data_start, header.len());
        assert_eq!(
            parsed.metadata,
            vec![
                ("general.architecture".to_owned(), Value::Str("qwen35".into())),
                ("x.flag".to_owned(), Value::Bool(true)),
            ]
        );
        // 12 bytes pad to 32; 20 bytes pad to 32.
        assert_eq!(
            parsed.tensors,
            vec![
                ("a".to_owned(), vec![3], 0, 0),
                ("b".to_owned(), vec![5, 2], 1, 32),
                ("c".to_owned(), vec![1], 0, 64),
            ]
        );
    }

    #[test]
    fn rules_follow_the_reference_converter() {
        let config = release_config();
        let rule = |name: &str, shape: &[usize]| rule_for(name, shape, &config).unwrap().unwrap();

        let embd = rule("model.language_model.embed_tokens.weight", &[248320, 1024]);
        assert_eq!(
            embd.entry(),
            TensorEntry { name: "token_embd.weight".into(), dims: vec![1024, 248320], ty: GgmlType::F16 }
        );
        assert_eq!(embd.op, Op::Copy);

        let norm = rule("model.language_model.layers.3.input_layernorm.weight", &[1024]);
        assert_eq!((norm.name.as_str(), norm.ty, norm.op), ("blk.3.attn_norm.weight", GgmlType::F32, Op::AddOne));
        let post = rule("model.language_model.layers.0.post_attention_layernorm.weight", &[1024]);
        assert_eq!((post.name.as_str(), post.op), ("blk.0.post_attention_norm.weight", Op::AddOne));
        let q_norm = rule("model.language_model.layers.3.self_attn.q_norm.weight", &[256]);
        assert_eq!((q_norm.name.as_str(), q_norm.op), ("blk.3.attn_q_norm.weight", Op::AddOne));
        let ssm_norm = rule("model.language_model.layers.0.linear_attn.norm.weight", &[128]);
        assert_eq!(
            (ssm_norm.name.as_str(), ssm_norm.ty, ssm_norm.op),
            ("blk.0.ssm_norm.weight", GgmlType::F32, Op::Copy)
        );
        let output_norm = rule("model.language_model.norm.weight", &[1024]);
        assert_eq!((output_norm.name.as_str(), output_norm.op), ("output_norm.weight", Op::AddOne));

        let a = rule("model.language_model.layers.0.linear_attn.A_log", &[16]);
        assert_eq!((a.name.as_str(), a.ty, a.op), ("blk.0.ssm_a", GgmlType::F32, Op::NegExp));
        let dt = rule("model.language_model.layers.0.linear_attn.dt_bias", &[16]);
        assert_eq!((dt.name.as_str(), dt.ty, dt.op), ("blk.0.ssm_dt.bias", GgmlType::F32, Op::Copy));
        let conv = rule("model.language_model.layers.0.linear_attn.conv1d.weight", &[6144, 1, 4]);
        assert_eq!(
            conv.entry(),
            TensorEntry { name: "blk.0.ssm_conv1d.weight".into(), dims: vec![4, 6144], ty: GgmlType::F32 }
        );

        for (suffix, name, dims) in [
            ("linear_attn.in_proj_qkv.weight", "blk.0.attn_qkv.weight", vec![1024, 6144]),
            ("linear_attn.in_proj_z.weight", "blk.0.attn_gate.weight", vec![1024, 2048]),
            ("linear_attn.in_proj_a.weight", "blk.0.ssm_alpha.weight", vec![1024, 16]),
            ("linear_attn.in_proj_b.weight", "blk.0.ssm_beta.weight", vec![1024, 16]),
            ("linear_attn.out_proj.weight", "blk.0.ssm_out.weight", vec![2048, 1024]),
            ("mlp.down_proj.weight", "blk.0.ffn_down.weight", vec![3584, 1024]),
        ] {
            let shape: Vec<usize> = dims.iter().rev().map(|&dim| dim as usize).collect();
            let rule = rule(&format!("model.language_model.layers.0.{suffix}"), &shape);
            assert_eq!(rule.entry(), TensorEntry { name: name.into(), dims, ty: GgmlType::F16 });
            // 16 K heads and 16 V heads: nothing to reorder.
            assert!(rule.rows.is_none() && rule.cols.is_none());
        }

        assert!(rule_for("model.visual.blocks.0.attn.qkv.weight", &[2304, 768], &config).unwrap().is_none());
        assert!(rule_for("mtp.fc.weight", &[1024, 2048], &config).unwrap().is_none());
        assert!(rule_for("model.language_model.layers.0.mlp.extra.weight", &[1], &config).is_err());
        assert!(rule_for("model.language_model.layers.24.mlp.up_proj.weight", &[3584, 1024], &config).is_err());
        assert_eq!(rule("lm_head.weight", &[248320, 1024]).name, "output.weight");
    }

    /// Orders computed by the reference's `_reorder_v_heads` for 2 K heads,
    /// 4 V heads and a head size of 3.
    #[test]
    fn v_head_order_matches_the_reference() {
        assert_eq!(v_head_order(2, 2, 3), [0, 1, 2, 6, 7, 8, 3, 4, 5, 9, 10, 11]);
        assert_eq!(v_head_order(2, 2, 1), [0, 2, 1, 3]);
    }

    #[test]
    fn reorders_every_v_axis_when_k_heads_are_fewer() {
        let mut config = release_config();
        config.linear_num_key_heads = 2;
        config.linear_num_value_heads = 4;
        config.linear_key_head_dim = 1;
        config.linear_value_head_dim = 3;
        let rule = |suffix: &str, shape: &[usize]| {
            rule_for(&format!("model.language_model.layers.0.linear_attn.{suffix}"), shape, &config).unwrap().unwrap()
        };
        let v = v_head_order(2, 2, 3);
        let qkv = rule("in_proj_qkv.weight", &[16, 1024]);
        assert_eq!(qkv.rows.as_deref().unwrap()[..4], [0, 1, 2, 3]);
        assert_eq!(qkv.rows.as_deref().unwrap()[4..], v.iter().map(|i| i + 4).collect::<Vec<_>>()[..]);
        assert_eq!(rule("in_proj_z.weight", &[12, 1024]).rows.as_deref(), Some(&v[..]));
        assert_eq!(rule("in_proj_a.weight", &[4, 1024]).rows.as_deref(), Some(&[0, 2, 1, 3][..]));
        assert_eq!(rule("dt_bias", &[4]).rows.as_deref(), Some(&[0, 2, 1, 3][..]));
        assert_eq!(rule("A_log", &[4]).rows.as_deref(), Some(&[0, 2, 1, 3][..]));
        let conv = rule("conv1d.weight", &[16, 1, 4]);
        assert_eq!(conv.shape, [16, 4]);
        assert_eq!(conv.rows, qkv.rows);
        let out = rule("out_proj.weight", &[1024, 12]);
        assert_eq!((out.rows.as_deref(), out.cols.as_deref()), (None, Some(&v[..])));
        assert!(rule("norm.weight", &[3]).rows.is_none());
        assert!(rule_for("model.language_model.layers.0.linear_attn.in_proj_z.weight", &[10, 1024], &config).is_err());

        // Applied to data: rows come from the mapped source rows, columns likewise.
        let source: Vec<f32> = (0..24).map(|i| i as f32).collect();
        let bytes = bf16_bytes(&source);
        let tensor = Tensor { dtype: Dtype::BF16, shape: &[2, 12], bytes: &bytes };
        let out_rule = Rule {
            name: "blk.0.ssm_out.weight".into(),
            shape: vec![2, 12],
            ty: GgmlType::F16,
            op: Op::Copy,
            rows: None,
            cols: Some(v.clone()),
        };
        let expected: Vec<u16> =
            (0..2).flat_map(|row| v.iter().map(move |&col| f32_to_f16((row * 12 + col) as f32))).collect();
        assert_eq!(f16s(&convert(tensor, &out_rule)), expected);

        let tensor = Tensor { dtype: Dtype::BF16, shape: &[12, 2], bytes: &bytes };
        let z_rule = Rule {
            name: "blk.0.attn_gate.weight".into(),
            shape: vec![12, 2],
            ty: GgmlType::F16,
            op: Op::Copy,
            rows: Some(v.clone()),
            cols: None,
        };
        let expected: Vec<u16> =
            v.iter().flat_map(|&row| (0..2).map(move |col| f32_to_f16((row * 2 + col) as f32))).collect();
        assert_eq!(f16s(&convert(tensor, &z_rule)), expected);
    }

    #[test]
    fn converts_values_and_types() {
        // bf16 → f16 rounds to nearest even through f32.
        let values = [1.0f32, -2.5, 1.0 + 1.0 / 256.0, 3.0e-8, 70000.0];
        let bytes = bf16_bytes(&values);
        let tensor = Tensor { dtype: Dtype::BF16, shape: &[1, 5], bytes: &bytes };
        let rule = Rule {
            name: "w.weight".into(),
            shape: vec![1, 5],
            ty: GgmlType::F16,
            op: Op::Copy,
            rows: None,
            cols: None,
        };
        let expected: Vec<u16> =
            values.iter().map(|&value| f32_to_f16(bf16_to_f32((value.to_bits() >> 16) as u16))).collect();
        assert_eq!(f16s(&convert(tensor, &rule)), expected);
        assert_eq!(expected[4], 0x7c00, "out of f16 range saturates to infinity");

        let bytes = bf16_bytes(&[0.5, -1.0]);
        let tensor = Tensor { dtype: Dtype::BF16, shape: &[2], bytes: &bytes };
        let rule = Rule {
            name: "n_norm.weight".into(),
            shape: vec![2],
            ty: GgmlType::F32,
            op: Op::AddOne,
            rows: None,
            cols: None,
        };
        assert_eq!(f32s(&convert(tensor, &rule)), [1.5, 0.0]);

        let a_log = [0.0f32, 1.0, -3.25];
        let bytes: Vec<u8> = a_log.iter().flat_map(|value| value.to_le_bytes()).collect();
        let tensor = Tensor { dtype: Dtype::F32, shape: &[3], bytes: &bytes };
        let rule = Rule {
            name: "blk.0.ssm_a".into(),
            shape: vec![3],
            ty: GgmlType::F32,
            op: Op::NegExp,
            rows: None,
            cols: None,
        };
        assert_eq!(f32s(&convert(tensor, &rule)), [-1.0, -f32::from_bits(0x402df854), -f32::from_bits(0x3d1ed1b4)]);

        // f32 → f32 copies bytes untouched, NaN payloads included.
        let bytes: Vec<u8> = [f32::from_bits(0x7fa0_0001), 2.0].iter().flat_map(|value| value.to_le_bytes()).collect();
        let tensor = Tensor { dtype: Dtype::F32, shape: &[2], bytes: &bytes };
        let rule = Rule {
            name: "blk.0.ssm_norm.weight".into(),
            shape: vec![2],
            ty: GgmlType::F32,
            op: Op::Copy,
            rows: None,
            cols: None,
        };
        assert_eq!(convert(tensor, &rule), bytes);
    }

    /// `torch.exp` outputs; the first five are not the correctly rounded value.
    #[test]
    fn exp_matches_torch_bit_for_bit() {
        let cases = [
            (0xc073e508u32, 0x3cb54874u32),
            (0xc04d3750, 0x3d25e14e),
            (0xc11daf71, 0x385c0faa),
            (0x406c465f, 0x4220779a),
            (0xc00c48dd, 0x3de4c276),
            (0x00000000, 0x3f800000),
            (0x3f800000, 0x402df854),
            (0xc2d10000, 0x00000000),
            (0x42c90000, 0x7f800000),
        ];
        for (index, (input, output)) in cases.into_iter().enumerate() {
            let x = f32::from_bits(input);
            assert_eq!(sleef_expf(x).to_bits(), output, "exp({x})");
            if index < 5 {
                assert_ne!(((x as f64).exp() as f32).to_bits(), output);
            }
        }
        assert!(sleef_expf(f32::NAN).is_nan());
    }

    #[test]
    fn large_tensors_stream_in_chunks() {
        let cols = 1024;
        let rows = CHUNK_BYTES / (cols * 2) * 2 + 3;
        let bytes = vec![0x3f; rows * cols * 2];
        let tensor = Tensor { dtype: Dtype::BF16, shape: &[rows, cols], bytes: &bytes };
        let rule = Rule {
            name: "w.weight".into(),
            shape: vec![rows, cols],
            ty: GgmlType::F16,
            op: Op::Copy,
            rows: None,
            cols: None,
        };
        let mut chunks = Vec::new();
        convert_tensor(tensor, &rule, &mut |bytes| {
            chunks.push(bytes.len());
            Ok(())
        })
        .unwrap();
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|&len| len <= CHUNK_BYTES));
        assert_eq!(chunks.iter().sum::<usize>(), rows * cols * 2);
    }

    fn tokenizer_json(pre_regex: &str) -> String {
        serde_json::json!({
            "added_tokens": [
                {"id": 5, "content": "<|endoftext|>", "special": true, "normalized": false},
                {"id": 6, "content": "<|im_end|>", "special": true, "normalized": false},
                {"id": 7, "content": "<think>", "special": false, "normalized": false},
                {"id": 8, "content": "<|fim_pad|>", "special": false, "normalized": false},
            ],
            "normalizer": {"type": "NFC"},
            "pre_tokenizer": {"type": "Sequence", "pretokenizers": [
                {"type": "Split", "pattern": {"Regex": pre_regex}, "behavior": "Isolated", "invert": false},
                {"type": "ByteLevel", "add_prefix_space": false, "trim_offsets": false, "use_regex": false},
            ]},
            "post_processor": {"type": "ByteLevel", "add_prefix_space": false, "trim_offsets": false, "use_regex": false},
            "model": {
                "type": "BPE",
                "vocab": {"a": 0, "b": 1, "ab": 2, "Ġ": 3, "Ġa": 4},
                "merges": [["a", "b"], ["Ġ", "a"], ["x y", "z"]],
            },
        })
        .to_string()
    }

    fn tokenizer_config() -> Json {
        serde_json::json!({
            "add_bos_token": false,
            "added_tokens_decoder": {
                "5": {"content": "<|endoftext|>", "special": true},
                "9": {"content": "<tts_pad>", "special": true},
            },
            "bos_token": null,
            "eos_token": "<|im_end|>",
            "pad_token": "<|endoftext|>",
            "unk_token": null,
            "chat_template": "{{ messages }}",
        })
    }

    #[test]
    fn exports_the_vocabulary_like_the_reference() {
        let tokenizer: TokenizerFile = serde_json::from_str(&tokenizer_json(QWEN35_PRE_TOKENIZER)).unwrap();
        check_tokenizer(&tokenizer).unwrap();
        let (tokens, types) = build_vocab(&tokenizer, &tokenizer_config(), 12).unwrap();
        assert_eq!(
            tokens,
            [
                "a",
                "b",
                "ab",
                "Ġ",
                "Ġa",
                "<|endoftext|>",
                "<|im_end|>",
                "<think>",
                "<|fim_pad|>",
                "<tts_pad>",
                "[PAD10]",
                "[PAD11]"
            ]
        );
        assert_eq!(types, [1, 1, 1, 1, 1, 3, 3, 4, 3, 3, 5, 5]);
        assert_eq!(merges(&tokenizer), ["a b", "Ġ a", "x\u{120}y z"]);
        assert!(build_vocab(&tokenizer, &tokenizer_config(), 9).is_err());

        let mut metadata = Metadata::default();
        let config = serde_json::json!({"text_config": {"eos_token_id": 5, "bos_token_id": 1}});
        special_tokens(&tokenizer, &tokenizer_config(), &config, &mut metadata).unwrap();
        assert_eq!(
            metadata.entries(),
            [
                // bos only comes from config.json, after everything tokenizer_config.json names.
                ("tokenizer.ggml.eos_token_id".to_owned(), Value::U32(6)),
                ("tokenizer.ggml.padding_token_id".to_owned(), Value::U32(5)),
                ("tokenizer.ggml.bos_token_id".to_owned(), Value::U32(1)),
                ("tokenizer.ggml.add_bos_token".to_owned(), Value::Bool(false)),
            ]
        );

        let other: TokenizerFile = serde_json::from_str(&tokenizer_json(r"\s+")).unwrap();
        assert!(check_tokenizer(&other).is_err());
    }

    /// A miniature release: 3 DeltaNet layers and one attention layer, with
    /// fewer K heads than V heads so every transform runs, plus vision and
    /// MTP tensors that must be left out.
    enum Edit {
        None,
        Remove(&'static str),
        Add(&'static str, Vec<usize>),
        Reshape(&'static str, Vec<usize>),
    }

    fn write_tiny_model(dir: &Path) {
        write_tiny_model_with(dir, &Edit::None);
    }

    fn write_tiny_model_with(dir: &Path, edit: &Edit) {
        let config = serde_json::json!({
            "architectures": ["Qwen3_5ForConditionalGeneration"],
            "model_type": "qwen3_5",
            "tie_word_embeddings": true,
            "text_config": {
                "model_type": "qwen3_5_text",
                "attn_output_gate": true,
                "eos_token_id": 5,
                "full_attention_interval": 4,
                "head_dim": 8,
                "hidden_size": 8,
                "intermediate_size": 16,
                "layer_types": ["linear_attention", "linear_attention", "linear_attention", "full_attention"],
                "linear_conv_kernel_dim": 4,
                "linear_key_head_dim": 4,
                "linear_num_key_heads": 1,
                "linear_num_value_heads": 2,
                "linear_value_head_dim": 4,
                "max_position_embeddings": 4096,
                "num_attention_heads": 2,
                "num_hidden_layers": 4,
                "num_key_value_heads": 1,
                "rms_norm_eps": 1e-6,
                "tie_word_embeddings": true,
                "vocab_size": 12,
                "rope_parameters": {"rope_type": "default", "rope_theta": 10000000, "partial_rotary_factor": 0.25, "mrope_section": [1, 0, 0], "mrope_interleaved": true},
            },
        });
        fs::write(dir.join("config.json"), config.to_string()).unwrap();
        fs::write(dir.join("tokenizer.json"), tokenizer_json(QWEN35_PRE_TOKENIZER)).unwrap();
        fs::write(dir.join("tokenizer_config.json"), tokenizer_config().to_string()).unwrap();

        let config = Config::parse(&fs::read(dir.join("config.json")).unwrap()).unwrap();
        let mut tensors = expected_tensors(&config, false);
        tensors.push(("model.visual.patch_embed.proj.weight".into(), vec![4, 3]));
        tensors.push(("mtp.fc.weight".into(), vec![8, 16]));
        match edit {
            Edit::None => {}
            Edit::Remove(name) => tensors.retain(|(tensor, _)| tensor != name),
            Edit::Add(name, shape) => tensors.push((name.to_string(), shape.clone())),
            Edit::Reshape(name, shape) => {
                tensors.iter_mut().filter(|(tensor, _)| tensor == name).for_each(|entry| entry.1 = shape.clone())
            }
        }
        let mut header = serde_json::Map::new();
        let mut data = Vec::new();
        for (index, (name, shape)) in tensors.iter().enumerate() {
            let count: usize = shape.iter().product();
            let values: Vec<f32> = (0..count).map(|i| ((index * 31 + i) % 17) as f32 / 8.0 - 1.0).collect();
            let (dtype, bytes) = if name.ends_with("A_log") || name.ends_with("linear_attn.norm.weight") {
                ("F32", values.iter().flat_map(|value| value.to_le_bytes()).collect())
            } else {
                ("BF16", bf16_bytes(&values))
            };
            header.insert(
                name.clone(),
                serde_json::json!({"dtype": dtype, "shape": shape, "data_offsets": [data.len(), data.len() + bytes.len()]}),
            );
            data.extend_from_slice(&bytes);
        }
        let header = Json::Object(header).to_string();
        let mut file = File::create(dir.join("model.safetensors-00001-of-00001.safetensors")).unwrap();
        file.write_all(&(header.len() as u64).to_le_bytes()).unwrap();
        file.write_all(header.as_bytes()).unwrap();
        file.write_all(&data).unwrap();
    }

    #[test]
    fn converts_a_tiny_model_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        write_tiny_model(dir.path());
        let out = dir.path().join("model.gguf");
        let mut reports = Vec::new();
        convert_qwen35_to_gguf(dir.path(), &out, &AtomicBool::new(false), &mut |progress| reports.push(progress))
            .unwrap();

        let bytes = fs::read(&out).unwrap();
        let last = reports.last().unwrap();
        assert_eq!((last.written_bytes, last.total_bytes), (bytes.len() as u64, bytes.len() as u64));
        assert!(reports.windows(2).all(|pair| pair[0].written_bytes <= pair[1].written_bytes));
        assert!(!dir.path().join("model.gguf.converting").exists());

        let parsed = parse(&bytes);
        let keys: Vec<&str> = parsed.metadata.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "general.architecture",
                "general.type",
                "general.name",
                "general.basename",
                "qwen35.block_count",
                "qwen35.context_length",
                "qwen35.embedding_length",
                "qwen35.feed_forward_length",
                "qwen35.attention.head_count",
                "qwen35.attention.head_count_kv",
                "qwen35.rope.dimension_sections",
                "qwen35.rope.freq_base",
                "qwen35.attention.layer_norm_rms_epsilon",
                "qwen35.attention.key_length",
                "qwen35.attention.value_length",
                "general.file_type",
                "qwen35.ssm.conv_kernel",
                "qwen35.ssm.state_size",
                "qwen35.ssm.group_count",
                "qwen35.ssm.time_step_rank",
                "qwen35.ssm.inner_size",
                "qwen35.attention.recurrent_layers",
                "qwen35.full_attention_interval",
                "qwen35.rope.dimension_count",
                "general.quantization_version",
                "tokenizer.ggml.model",
                "tokenizer.ggml.pre",
                "tokenizer.ggml.tokens",
                "tokenizer.ggml.token_type",
                "tokenizer.ggml.merges",
                "tokenizer.ggml.eos_token_id",
                "tokenizer.ggml.padding_token_id",
                "tokenizer.ggml.add_bos_token",
                "tokenizer.chat_template",
            ]
        );
        assert_eq!(parsed.get("qwen35.rope.dimension_sections"), Some(&Value::I32s(vec![1, 0, 0, 0])));
        assert_eq!(parsed.get("qwen35.attention.recurrent_layers"), Some(&Value::Bools(vec![true, true, true, false])));
        assert_eq!(parsed.get("qwen35.rope.dimension_count"), Some(&Value::U32(2)));
        assert_eq!(parsed.get("qwen35.ssm.inner_size"), Some(&Value::U32(8)));
        assert_eq!(parsed.get("tokenizer.ggml.eos_token_id"), Some(&Value::U32(6)));

        // 3 × 14 DeltaNet tensors, 11 attention tensors, embeddings and final norm.
        assert_eq!(parsed.tensors.len(), 3 * 14 + 11 + 2);
        assert!(parsed.tensors.iter().all(|(name, ..)| !name.contains("visual") && !name.starts_with("mtp")));
        let mut next = 0;
        for (_, dims, ty, offset) in &parsed.tensors {
            assert_eq!(*offset, next);
            let size = if *ty == 0 { 4 } else { 2 };
            next += padded(dims.iter().product::<u64>() * size);
        }
        assert_eq!(parsed.data_start as u64 + next, bytes.len() as u64);

        let tensor = |name: &str| {
            let (_, dims, ty, offset) = parsed.tensors.iter().find(|(tensor, ..)| tensor == name).unwrap();
            let size = if *ty == 0 { 4 } else { 2 };
            let start = parsed.data_start + *offset as usize;
            (dims.clone(), *ty, &bytes[start..start + dims.iter().product::<u64>() as usize * size])
        };
        let (dims, ty, _) = tensor("blk.0.ssm_conv1d.weight");
        assert_eq!((dims, ty), (vec![4, 16], 0));
        let (dims, ty, data) = tensor("blk.1.ssm_a");
        assert_eq!((dims, ty), (vec![2], 0));
        assert!(f32s(data).iter().all(|&value| value < 0.0));
        let (dims, ty, _) = tensor("blk.3.attn_q.weight");
        assert_eq!((dims, ty), (vec![8, 32], 1));
        let (_, ty, data) = tensor("output_norm.weight");
        assert_eq!(ty, 0);
        let checkpoint = SafeTensors::open(&dir.path().join("model.safetensors-00001-of-00001.safetensors")).unwrap();
        let norm = checkpoint.tensor("model.language_model.norm.weight").unwrap().to_f32();
        assert_eq!(f32s(data), norm.iter().map(|value| value + 1.0).collect::<Vec<_>>());
    }

    #[test]
    fn cancelling_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        write_tiny_model(dir.path());
        let out = dir.path().join("model.gguf");
        let error = convert_qwen35_to_gguf(dir.path(), &out, &AtomicBool::new(true), &mut |_| {}).unwrap_err();
        assert_eq!(error, CANCELLED);
        assert!(!out.exists());
        assert!(!dir.path().join("model.gguf.converting").exists());
    }

    #[test]
    fn rejects_checkpoints_llama_cpp_would_refuse() {
        for (edit, expected) in [
            (Edit::Remove("model.language_model.layers.2.linear_attn.dt_bias"), "缺少"),
            (Edit::Add("model.language_model.layers.0.self_attn.q_proj.weight", vec![32, 8]), "多余"),
            (Edit::Reshape("model.language_model.layers.3.self_attn.k_norm.weight", vec![4]), "形状"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            write_tiny_model_with(dir.path(), &edit);
            let out = dir.path().join("model.gguf");
            let error = convert_qwen35_to_gguf(dir.path(), &out, &AtomicBool::new(false), &mut |_| {}).unwrap_err();
            assert!(error.contains(expected), "{error}");
            assert!(!out.exists());
        }
    }

    /// Converts the real release when `MEWORK_LOCAL_MODEL_DIR` points at it.
    #[test]
    fn converts_the_release() {
        let Some(dir) = std::env::var_os("MEWORK_LOCAL_MODEL_DIR") else {
            eprintln!("MEWORK_LOCAL_MODEL_DIR 未设置，跳过");
            return;
        };
        let dir = PathBuf::from(dir);
        let config = Config::load(&dir.join("config.json")).unwrap();
        let out_dir = tempfile::tempdir().unwrap();
        let out = out_dir.path().join("qwen3.5.gguf");
        let started = std::time::Instant::now();
        convert_qwen35_to_gguf(&dir, &out, &AtomicBool::new(false), &mut |_| {}).unwrap();
        eprintln!("转换用时 {:?}", started.elapsed());

        let size = fs::metadata(&out).unwrap().len();
        let mut head = vec![0; (64 << 20).min(size as usize)];
        std::io::Read::read_exact(&mut File::open(&out).unwrap(), &mut head).unwrap();
        let parsed = parse(&head);
        assert_eq!(parsed.get("general.architecture"), Some(&Value::Str("qwen35".into())));
        assert_eq!(parsed.get("qwen35.block_count"), Some(&Value::U32(config.layers.len() as u32)));
        let Some(Value::Strs(tokens)) = parsed.get("tokenizer.ggml.tokens") else { panic!("no tokens") };
        assert_eq!(tokens.len(), config.vocab_size);
        let expected = 2 + config.linear_attention_layers() * 14 + config.full_attention_layers() * 11;
        assert_eq!(parsed.tensors.len(), expected);
        let (_, dims, ty, offset) = parsed.tensors.last().unwrap();
        let size_of = if *ty == 0 { 4 } else { 2 };
        assert_eq!(parsed.data_start as u64 + offset + padded(dims.iter().product::<u64>() * size_of), size);
    }

    /// The projector llama.cpp b11074's own converter writes for the release
    /// (`convert_hf_to_gguf.py --mmproj --outtype f16`, Qwen/Qwen3.5-0.8B at
    /// 2fc06364): this converter must match it byte for byte.
    const REFERENCE_MMPROJ_SHA256: &str = "413334162c7cbacebe6bef772011cf916e9123a07a0805a78cdb097394aaa5be";

    #[test]
    fn converts_the_release_vision_tower_like_llama_cpp() {
        let Some(dir) = std::env::var_os("MEWORK_LOCAL_MODEL_DIR") else {
            eprintln!("MEWORK_LOCAL_MODEL_DIR 未设置，跳过");
            return;
        };
        let out_dir = tempfile::tempdir().unwrap();
        let out = out_dir.path().join(MMPROJ_FILE);
        convert_qwen35_mmproj_to_gguf(&PathBuf::from(dir), &out, &AtomicBool::new(false), &mut |_| {}).unwrap();
        let bytes = fs::read(&out).unwrap();
        let parsed = parse(&bytes);
        assert_eq!(parsed.get("clip.projector_type"), Some(&Value::Str(CLIP_PROJECTOR.into())));
        assert_eq!(parsed.get("general.size_label"), Some(&Value::Str("101M".into())));
        assert_eq!(parsed.tensors.len(), 154);
        use sha2::Digest;
        let digest: String = sha2::Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(digest, REFERENCE_MMPROJ_SHA256);
    }

    #[test]
    fn rounds_parameter_counts_like_gguf_py() {
        assert_eq!(rounded_count(100_592_896), "101M");
        assert_eq!(rounded_count(752_393_024), "752M");
        assert_eq!(rounded_count(1_500_000), "1.5M");
        assert_eq!(rounded_count(8_030_000_000), "8.0B");
    }
}
