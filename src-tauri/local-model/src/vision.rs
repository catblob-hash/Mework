//! The Qwen3.5 vision tower, run on the CPU: an image becomes rows of
//! features the language model reads in place of the embeddings of its
//! `<|image_pad|>` tokens. The Apple builds (Neural Engine and MLX) both use
//! this; the llama.cpp build runs llama.cpp's own (`mtmd`).
//!
//! It follows transformers' `Qwen3_5VisionModel` step for step, in float32:
//! the image is cut into 16×16 patches in 2×2 merge-block order (the order
//! `Qwen2VLImageProcessor` emits them), each patch is projected (a Conv3d
//! over two identical frames) and given a learned position embedding
//! bilinearly resampled from a 48×48 table; twelve pre-norm ViT blocks with
//! 2-D rotary attention follow, and the merger turns each 2×2 block into one
//! row of the language model's width.
//!
//! The weights stay as the checkpoint has them (bfloat16, mapped) and are
//! widened one matrix at a time, so encoding holds a layer's worth of float32
//! weights, not the tower's. Matrix products go to Accelerate on macOS (its
//! `cblas_sgemm` uses the AMX units); elsewhere a plain loop serves the tests.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::Deserialize;

use crate::safetensors::SafeTensors;

/// Prefix of every vision tensor in the checkpoint (and in the vision file
/// the Apple builds ship, which keeps the checkpoint's names).
pub const TENSOR_PREFIX: &str = "model.visual.";
/// The vision tower's weights in the Apple builds.
pub const VISION_FILE: &str = "vision.safetensors";
/// Names `write_weights`' output in the published builds; a change to the
/// output needs a new one.
pub const VISION_VERSION: &str = "qwen35-vision-1";
const LAYER_NORM_EPS: f32 = 1e-6;
/// Query rows per attention task: bounds the score matrix a task holds.
const QUERY_BLOCK: usize = 256;

#[derive(Clone, Debug, PartialEq)]
pub struct VisionConfig {
    pub depth: usize,
    pub hidden_size: usize,
    pub intermediate_size: usize,
    pub num_heads: usize,
    pub in_channels: usize,
    pub patch_size: usize,
    pub temporal_patch_size: usize,
    pub spatial_merge_size: usize,
    pub out_hidden_size: usize,
    pub num_position_embeddings: usize,
    pub rope_theta: f64,
    pub image_token_id: u32,
    pub vision_start_token_id: u32,
    pub vision_end_token_id: u32,
}

#[derive(Deserialize)]
struct RawRoot {
    vision_config: Option<RawVision>,
    image_token_id: Option<u32>,
    vision_start_token_id: Option<u32>,
    vision_end_token_id: Option<u32>,
}

#[derive(Deserialize)]
struct RawVision {
    depth: usize,
    hidden_size: usize,
    intermediate_size: usize,
    num_heads: usize,
    #[serde(default = "three")]
    in_channels: usize,
    patch_size: usize,
    temporal_patch_size: usize,
    spatial_merge_size: usize,
    out_hidden_size: usize,
    num_position_embeddings: usize,
    #[serde(default)]
    hidden_act: Option<String>,
    #[serde(default)]
    deepstack_visual_indexes: Vec<usize>,
    #[serde(default)]
    rope_parameters: Option<RawRope>,
}

#[derive(Deserialize)]
struct RawRope {
    #[serde(default)]
    rope_theta: Option<f64>,
}

fn three() -> usize {
    3
}

impl VisionConfig {
    pub fn load(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|error| format!("无法读取模型配置 {}: {error}", path.display()))?;
        Self::parse(&bytes)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let root: RawRoot = serde_json::from_slice(bytes).map_err(|error| format!("模型配置无法解析: {error}"))?;
        let raw = root.vision_config.ok_or("模型配置缺少 vision_config")?;
        if raw.hidden_act.as_deref().is_some_and(|act| act != "gelu_pytorch_tanh") {
            return Err("不支持的视觉激活函数".into());
        }
        if !raw.deepstack_visual_indexes.is_empty() {
            return Err("不支持 deepstack 视觉层".into());
        }
        let side = (raw.num_position_embeddings as f64).sqrt() as usize;
        if raw.num_heads == 0
            || raw.hidden_size % raw.num_heads != 0
            || (raw.hidden_size / raw.num_heads) % 4 != 0
            || side * side != raw.num_position_embeddings
            || raw.spatial_merge_size == 0
        {
            return Err("视觉配置无效".into());
        }
        Ok(Self {
            depth: raw.depth,
            hidden_size: raw.hidden_size,
            intermediate_size: raw.intermediate_size,
            num_heads: raw.num_heads,
            in_channels: raw.in_channels,
            patch_size: raw.patch_size,
            temporal_patch_size: raw.temporal_patch_size,
            spatial_merge_size: raw.spatial_merge_size,
            out_hidden_size: raw.out_hidden_size,
            num_position_embeddings: raw.num_position_embeddings,
            rope_theta: raw.rope_parameters.and_then(|rope| rope.rope_theta).unwrap_or(10000.0),
            image_token_id: root.image_token_id.ok_or("模型配置缺少 image_token_id")?,
            vision_start_token_id: root.vision_start_token_id.ok_or("模型配置缺少 vision_start_token_id")?,
            vision_end_token_id: root.vision_end_token_id.ok_or("模型配置缺少 vision_end_token_id")?,
        })
    }

    /// Image sides are multiples of this: a merge block of patches.
    pub fn factor(&self) -> usize {
        self.patch_size * self.spatial_merge_size
    }

    fn head_dim(&self) -> usize {
        self.hidden_size / self.num_heads
    }

    fn patch_dim(&self) -> usize {
        self.in_channels * self.temporal_patch_size * self.patch_size * self.patch_size
    }
}

/// The size an image of `width`×`height` is resized to so it becomes between
/// `min_tokens` and `max_tokens` image tokens: transformers' `smart_resize`
/// (sides multiples of `factor`, aspect ratio kept as closely as possible),
/// with the pixel bounds given in tokens. `None` for an empty image or one
/// more than 200 times as long as it is wide.
pub fn fit(width: usize, height: usize, factor: usize, min_tokens: usize, max_tokens: usize) -> Option<(usize, usize)> {
    if width == 0 || height == 0 || factor == 0 {
        return None;
    }
    if width.max(height) as f64 / width.min(height) as f64 > 200.0 {
        return None;
    }
    let (min_pixels, max_pixels) = ((min_tokens * factor * factor) as f64, (max_tokens * factor * factor) as f64);
    let (h, w) = (height as f64, width as f64);
    let f = factor as f64;
    // Python's round() rounds halves to even.
    let round = |v: f64| {
        let r = v.round();
        if (v - v.trunc()).abs() == 0.5 && r % 2.0 != 0.0 {
            r - v.signum()
        } else {
            r
        }
    };
    let mut h_bar = round(h / f) * f;
    let mut w_bar = round(w / f) * f;
    if h_bar * w_bar > max_pixels {
        let beta = (h * w / max_pixels).sqrt();
        h_bar = f.max((h / beta / f).floor() * f);
        w_bar = f.max((w / beta / f).floor() * f);
    } else if h_bar * w_bar < min_pixels {
        let beta = (min_pixels / (h * w)).sqrt();
        h_bar = (h * beta / f).ceil() * f;
        w_bar = (w * beta / f).ceil() * f;
    }
    Some((w_bar as usize, h_bar as usize))
}

/// An image at the size the model sees it: RGB, row-major, sides multiples
/// of `VisionConfig::factor`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Picture {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<u8>,
}

impl Picture {
    pub fn new(width: usize, height: usize, rgb: Vec<u8>) -> Result<Self, String> {
        if width == 0 || height == 0 || rgb.len() != width * height * 3 {
            return Err("图片尺寸与像素数据不符".into());
        }
        Ok(Self { width, height, rgb })
    }

    /// Image tokens `(rows, columns)`: one per merge block.
    pub fn grid(&self, config: &VisionConfig) -> (usize, usize) {
        (self.height / config.factor(), self.width / config.factor())
    }

    pub fn tokens(&self, config: &VisionConfig) -> usize {
        let (rows, columns) = self.grid(config);
        rows * columns
    }

    fn check(&self, config: &VisionConfig) -> Result<(), String> {
        let factor = config.factor();
        if self.width % factor != 0 || self.height % factor != 0 || self.rgb.len() != self.width * self.height * 3 {
            return Err(format!("图片尺寸必须是 {factor} 的倍数"));
        }
        Ok(())
    }
}

/// An encoded image: `rows` image tokens of `width` features, row-major in
/// the order of the merge blocks (left to right, top to bottom).
#[derive(Clone, Debug)]
pub struct Features {
    pub grid: (usize, usize),
    pub width: usize,
    pub rows: Vec<f32>,
}

/// The patches of `picture` as the processor flattens them, `[patches,
/// channels · frames · 16 · 16]`, in merge-block order and normalized to
/// `[-1, 1]` (mean and std 0.5).
pub fn patches(picture: &Picture, config: &VisionConfig) -> Result<Vec<f32>, String> {
    picture.check(config)?;
    let (p, m, t, c) = (config.patch_size, config.spatial_merge_size, config.temporal_patch_size, config.in_channels);
    let (gh, gw) = (picture.height / p, picture.width / p);
    let dim = config.patch_dim();
    let mut out = vec![0f32; gh * gw * dim];
    let mut index = 0;
    for block_row in 0..gh / m {
        for block_col in 0..gw / m {
            for in_row in 0..m {
                for in_col in 0..m {
                    let (row, col) = (block_row * m + in_row, block_col * m + in_col);
                    let patch = &mut out[index * dim..(index + 1) * dim];
                    for channel in 0..c {
                        for frame in 0..t {
                            for y in 0..p {
                                for x in 0..p {
                                    let pixel = (row * p + y) * picture.width + col * p + x;
                                    let value = picture.rgb[pixel * 3 + channel] as f32 / 255.0;
                                    patch[((channel * t + frame) * p + y) * p + x] = (value - 0.5) / 0.5;
                                }
                            }
                        }
                    }
                    index += 1;
                }
            }
        }
    }
    Ok(out)
}

/// `(row, column)` of each patch, in the order `patches` emits them.
fn patch_positions(gh: usize, gw: usize, m: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::with_capacity(gh * gw);
    for block_row in 0..gh / m {
        for block_col in 0..gw / m {
            for in_row in 0..m {
                for in_col in 0..m {
                    out.push((block_row * m + in_row, block_col * m + in_col));
                }
            }
        }
    }
    out
}

/// Bilinear (align_corners) taps into a `side`-long axis for target index
/// `index` of `size`, as transformers computes them in float32.
fn taps(index: usize, size: usize, side: usize) -> [(usize, f32); 2] {
    let src = index as f32 * (side - 1) as f32 / (size.max(2) - 1) as f32;
    let floor = src.floor();
    let mut out = [(0, 0.0); 2];
    for (offset, slot) in out.iter_mut().enumerate() {
        let raw = floor as i64 + offset as i64;
        let tap = raw.clamp(0, side as i64 - 1) as usize;
        let distance = (src - floor - offset as f32).abs();
        *slot = (tap, (1.0 - distance).max(0.0));
    }
    out
}

pub struct VisionTower {
    config: VisionConfig,
    weights: SafeTensors,
}

impl VisionTower {
    /// The tower from `weights` (the checkpoint, or a file with its
    /// `model.visual.*` tensors).
    pub fn open(config: VisionConfig, weights: &Path) -> Result<Self, String> {
        let weights = SafeTensors::open(weights)?;
        let tower = Self { config, weights };
        tower.check()?;
        Ok(tower)
    }

    pub fn config(&self) -> &VisionConfig {
        &self.config
    }

    fn check(&self) -> Result<(), String> {
        let c = &self.config;
        let (h, merged) = (c.hidden_size, c.hidden_size * c.spatial_merge_size * c.spatial_merge_size);
        let mut expected = vec![
            ("patch_embed.proj.weight".to_string(), vec![h, c.in_channels, c.temporal_patch_size, c.patch_size, c.patch_size]),
            ("patch_embed.proj.bias".to_string(), vec![h]),
            ("pos_embed.weight".to_string(), vec![c.num_position_embeddings, h]),
            ("merger.norm.weight".to_string(), vec![h]),
            ("merger.linear_fc1.weight".to_string(), vec![merged, merged]),
            ("merger.linear_fc2.weight".to_string(), vec![c.out_hidden_size, merged]),
        ];
        for block in 0..c.depth {
            let b = |s: &str| format!("blocks.{block}.{s}");
            expected.push((b("attn.qkv.weight"), vec![3 * h, h]));
            expected.push((b("attn.proj.weight"), vec![h, h]));
            expected.push((b("mlp.linear_fc1.weight"), vec![c.intermediate_size, h]));
            expected.push((b("mlp.linear_fc2.weight"), vec![h, c.intermediate_size]));
        }
        for (name, shape) in expected {
            let full = format!("{TENSOR_PREFIX}{name}");
            let info = self.weights.info(&full).ok_or_else(|| format!("视觉权重里缺少张量 {full}"))?;
            if info.shape != shape {
                return Err(format!("视觉张量 {full} 的形状不符"));
            }
        }
        Ok(())
    }

    fn tensor(&self, name: &str) -> Result<Vec<f32>, String> {
        Ok(self.weights.tensor(&format!("{TENSOR_PREFIX}{name}"))?.to_f32())
    }

    /// `x · Wᵀ + b` for the checkpoint's `[out, in]` linear `name`.
    fn linear(&self, x: &[f32], rows: usize, name: &str) -> Result<Vec<f32>, String> {
        let w = self.tensor(&format!("{name}.weight"))?;
        let b = self.tensor(&format!("{name}.bias"))?;
        let (out, inp) = (b.len(), w.len() / b.len());
        let mut y = vec![0f32; rows * out];
        gemm(true, rows, out, inp, x, &w, &mut y);
        parallel_rows(&mut y, out, |_, row| row.iter_mut().zip(&b).for_each(|(v, b)| *v += b));
        Ok(y)
    }

    fn layer_norm(&self, x: &[f32], width: usize, name: &str) -> Result<Vec<f32>, String> {
        let w = self.tensor(&format!("{name}.weight"))?;
        let b = self.tensor(&format!("{name}.bias"))?;
        let mut y = x.to_vec();
        parallel_rows(&mut y, width, |_, row| layer_norm(row, &w, &b));
        Ok(y)
    }

    /// Features of `picture`, whose sides must be multiples of
    /// `VisionConfig::factor` (see `fit`).
    pub fn encode(&self, picture: &Picture) -> Result<Features, String> {
        let c = &self.config;
        let (p, m, h) = (c.patch_size, c.spatial_merge_size, c.hidden_size);
        let input = patches(picture, c)?;
        let (gh, gw) = (picture.height / p, picture.width / p);
        let n = gh * gw;

        // Patch embedding: the Conv3d is a linear map of the flattened patch.
        let mut x = self.linear(&input, n, "patch_embed.proj")?;
        drop(input);

        // Learned positions, resampled to this grid.
        let table = self.tensor("pos_embed.weight")?;
        let side = (c.num_position_embeddings as f64).sqrt() as usize;
        let positions = patch_positions(gh, gw, m);
        parallel_rows(&mut x, h, |i, row| {
            let (r, col) = positions[i];
            let mut pos = vec![0f32; h];
            for (tr, wr) in taps(r, gh, side) {
                for (tc, wc) in taps(col, gw, side) {
                    let weight = wr * wc;
                    let entry = &table[(tr * side + tc) * h..(tr * side + tc + 1) * h];
                    pos.iter_mut().zip(entry).for_each(|(acc, e)| *acc += e * weight);
                }
            }
            row.iter_mut().zip(&pos).for_each(|(v, p)| *v += p);
        });
        drop(table);

        // Rotary angles: the first quarter of each head's features turns with
        // the patch row, the second with its column; both halves repeat them.
        let hd = c.head_dim();
        let quarter = hd / 4;
        let inv: Vec<f32> =
            (0..quarter).map(|i| (1.0 / c.rope_theta.powf((2 * i) as f64 / (hd / 2) as f64)) as f32).collect();
        let mut cos = vec![0f32; n * hd];
        let mut sin = vec![0f32; n * hd];
        for (i, (r, col)) in positions.iter().enumerate() {
            for (j, f) in inv.iter().enumerate() {
                for (slot, coordinate) in [(j, *r), (quarter + j, *col)] {
                    let angle = coordinate as f32 * f;
                    let (s, co) = angle.sin_cos();
                    for k in [slot, slot + hd / 2] {
                        cos[i * hd + k] = co;
                        sin[i * hd + k] = s;
                    }
                }
            }
        }

        for block in 0..c.depth {
            let b = |s: &str| format!("blocks.{block}.{s}");
            let normed = self.layer_norm(&x, h, &b("norm1"))?;
            let qkv = self.linear(&normed, n, &b("attn.qkv"))?;
            drop(normed);
            let attended = self.attention(&qkv, n, &cos, &sin);
            drop(qkv);
            let projected = self.linear(&attended, n, &b("attn.proj"))?;
            x.iter_mut().zip(&projected).for_each(|(v, a)| *v += a);
            let normed = self.layer_norm(&x, h, &b("norm2"))?;
            let mut hidden = self.linear(&normed, n, &b("mlp.linear_fc1"))?;
            parallel_rows(&mut hidden, c.intermediate_size, |_, row| row.iter_mut().for_each(|v| *v = gelu_tanh(*v)));
            let out = self.linear(&hidden, n, &b("mlp.linear_fc2"))?;
            x.iter_mut().zip(&out).for_each(|(v, o)| *v += o);
        }

        // Merger: norm per patch, then each merge block's patches side by side.
        let normed = self.layer_norm(&x, h, "merger.norm")?;
        let rows = n / (m * m);
        let mut hidden = self.linear(&normed, rows, "merger.linear_fc1")?;
        let merged = h * m * m;
        parallel_rows(&mut hidden, merged, |_, row| row.iter_mut().for_each(|v| *v = gelu_erf(*v)));
        let out = self.linear(&hidden, rows, "merger.linear_fc2")?;
        Ok(Features { grid: (gh / m, gw / m), width: c.out_hidden_size, rows: out })
    }

    /// Bidirectional multi-head attention over all `n` patches of `qkv`
    /// (`[n, 3, heads, head_dim]`), with rotary `cos`/`sin` `[n, head_dim]`.
    /// Returns `[n, heads · head_dim]`.
    fn attention(&self, qkv: &[f32], n: usize, cos: &[f32], sin: &[f32]) -> Vec<f32> {
        let (heads, hd, h) = (self.config.num_heads, self.config.head_dim(), self.config.hidden_size);
        let half = hd / 2;
        // Per head, contiguous [n, hd]: rotated q and k, and v.
        let gather = |which: usize, head: usize, rotate: bool| {
            let mut out = vec![0f32; n * hd];
            for i in 0..n {
                let src = &qkv[i * 3 * h + which * h + head * hd..][..hd];
                let dst = &mut out[i * hd..(i + 1) * hd];
                if rotate {
                    let (c, s) = (&cos[i * hd..(i + 1) * hd], &sin[i * hd..(i + 1) * hd]);
                    for k in 0..hd {
                        let rotated = if k < half { -src[k + half] } else { src[k - half] };
                        dst[k] = src[k] * c[k] + rotated * s[k];
                    }
                } else {
                    dst.copy_from_slice(src);
                }
            }
            out
        };
        let per_head: Vec<(Vec<f32>, Vec<f32>, Vec<f32>)> =
            (0..heads).map(|head| (gather(0, head, true), gather(1, head, true), gather(2, head, false))).collect();
        let scale = (hd as f32).powf(-0.5);
        let blocks = n.div_ceil(QUERY_BLOCK);
        let results: Vec<std::sync::Mutex<Vec<f32>>> = (0..heads * blocks).map(|_| Default::default()).collect();
        parallel_tasks(heads * blocks, |task| {
            let (head, block) = (task / blocks, task % blocks);
            let (q, k, v) = &per_head[head];
            let start = block * QUERY_BLOCK;
            let rows = QUERY_BLOCK.min(n - start);
            let mut scores = vec![0f32; rows * n];
            gemm(true, rows, n, hd, &q[start * hd..(start + rows) * hd], k, &mut scores);
            for row in scores.chunks_mut(n) {
                softmax(row, scale);
            }
            let mut out = vec![0f32; rows * hd];
            gemm(false, rows, hd, n, &scores, v, &mut out);
            *results[task].lock().expect("attention block") = out;
        });
        let mut merged = vec![0f32; n * h];
        for (task, result) in results.into_iter().enumerate() {
            let (head, block) = (task / blocks, task % blocks);
            let out = result.into_inner().expect("attention block");
            for (r, row) in out.chunks(hd).enumerate() {
                let i = block * QUERY_BLOCK + r;
                merged[i * h + head * hd..i * h + (head + 1) * hd].copy_from_slice(row);
            }
        }
        merged
    }
}

fn layer_norm(row: &mut [f32], weight: &[f32], bias: &[f32]) {
    let n = row.len() as f64;
    let mean = row.iter().map(|v| *v as f64).sum::<f64>() / n;
    let var = row.iter().map(|v| (*v as f64 - mean).powi(2)).sum::<f64>() / n;
    let inv = 1.0 / (var + LAYER_NORM_EPS as f64).sqrt();
    for ((v, w), b) in row.iter_mut().zip(weight).zip(bias) {
        *v = ((*v as f64 - mean) * inv) as f32 * w + b;
    }
}

fn gelu_tanh(x: f32) -> f32 {
    const SQRT_2_OVER_PI: f32 = 0.797_884_6;
    0.5 * x * (1.0 + (SQRT_2_OVER_PI * (x + 0.044715 * x * x * x)).tanh())
}

extern "C" {
    // The C library's error function; every target's libm has it.
    fn erff(x: f32) -> f32;
}

fn gelu_erf(x: f32) -> f32 {
    // SAFETY: a pure libm function.
    0.5 * x * (1.0 + unsafe { erff(x * std::f32::consts::FRAC_1_SQRT_2) })
}

/// In place: softmax of `row · scale`.
fn softmax(row: &mut [f32], scale: f32) {
    let max = row.iter().fold(f32::NEG_INFINITY, |a, b| a.max(*b)) * scale;
    for v in row.iter_mut() {
        *v = *v * scale - max;
    }
    exp_in_place(row);
    let sum: f32 = row.iter().sum();
    let inv = 1.0 / sum;
    row.iter_mut().for_each(|v| *v *= inv);
}

#[cfg(target_os = "macos")]
mod accelerate {
    #[link(name = "Accelerate", kind = "framework")]
    extern "C" {
        pub fn cblas_sgemm(
            order: i32,
            trans_a: i32,
            trans_b: i32,
            m: i32,
            n: i32,
            k: i32,
            alpha: f32,
            a: *const f32,
            lda: i32,
            b: *const f32,
            ldb: i32,
            beta: f32,
            c: *mut f32,
            ldc: i32,
        );
        pub fn vvexpf(y: *mut f32, x: *const f32, n: *const i32);
    }
    pub const ROW_MAJOR: i32 = 101;
    pub const NO_TRANS: i32 = 111;
    pub const TRANS: i32 = 112;
}

/// `c = a · op(b)`: `a` is `[m, k]`; `op(b)` is `[k, n]`, stored as `[n, k]`
/// when `trans_b` (a linear layer's weight) and as `[k, n]` otherwise.
#[cfg(target_os = "macos")]
fn gemm(trans_b: bool, m: usize, n: usize, k: usize, a: &[f32], b: &[f32], c: &mut [f32]) {
    use accelerate::*;
    assert!(a.len() >= m * k && b.len() >= n * k && c.len() >= m * n);
    let ldb = if trans_b { k } else { n };
    // SAFETY: the slices hold the matrices the dimensions describe.
    unsafe {
        cblas_sgemm(
            ROW_MAJOR,
            NO_TRANS,
            if trans_b { TRANS } else { NO_TRANS },
            m as i32,
            n as i32,
            k as i32,
            1.0,
            a.as_ptr(),
            k as i32,
            b.as_ptr(),
            ldb as i32,
            0.0,
            c.as_mut_ptr(),
            n as i32,
        )
    };
}

#[cfg(not(target_os = "macos"))]
fn gemm(trans_b: bool, m: usize, n: usize, k: usize, a: &[f32], b: &[f32], c: &mut [f32]) {
    assert!(a.len() >= m * k && b.len() >= n * k && c.len() >= m * n);
    parallel_rows(&mut c[..m * n], n, |i, row| {
        let a_row = &a[i * k..(i + 1) * k];
        if trans_b {
            for (j, out) in row.iter_mut().enumerate() {
                *out = a_row.iter().zip(&b[j * k..(j + 1) * k]).map(|(x, y)| x * y).sum();
            }
        } else {
            row.fill(0.0);
            for (p, x) in a_row.iter().enumerate() {
                row.iter_mut().zip(&b[p * n..(p + 1) * n]).for_each(|(out, y)| *out += x * y);
            }
        }
    });
}

#[cfg(target_os = "macos")]
fn exp_in_place(values: &mut [f32]) {
    let n = values.len() as i32;
    // SAFETY: vForce reads and writes `n` floats; in place is allowed.
    unsafe { accelerate::vvexpf(values.as_mut_ptr(), values.as_ptr(), &n) };
}

#[cfg(not(target_os = "macos"))]
fn exp_in_place(values: &mut [f32]) {
    values.iter_mut().for_each(|v| *v = v.exp());
}

fn threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(16)
}

/// Runs `task(i)` for every `i < count` on a few threads.
fn parallel_tasks(count: usize, task: impl Fn(usize) + Sync) {
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..threads().min(count) {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= count {
                    break;
                }
                task(i);
            });
        }
    });
}

/// Runs `f(row index, row)` over the `width`-long rows of `data`, in parallel.
fn parallel_rows(data: &mut [f32], width: usize, f: impl Fn(usize, &mut [f32]) + Sync) {
    let rows = data.len() / width;
    let per = rows.div_ceil(threads()).max(1);
    std::thread::scope(|scope| {
        for (chunk, part) in data.chunks_mut(per * width).enumerate() {
            let f = &f;
            scope.spawn(move || {
                for (r, row) in part.chunks_mut(width).enumerate() {
                    f(chunk * per + r, row);
                }
            });
        }
    });
}

/// Writes the checkpoint's vision tensors, as they are (names, types,
/// bytes), to the safetensors file `out`: what the Apple builds ship. The
/// output is deterministic.
pub fn write_weights(checkpoint: &SafeTensors, out: &Path) -> Result<(), String> {
    let names: Vec<&str> = checkpoint.names().filter(|name| name.starts_with(TENSOR_PREFIX)).collect();
    if names.is_empty() {
        return Err("权重里没有视觉张量".into());
    }
    let mut header = serde_json::Map::new();
    header.insert("__metadata__".into(), serde_json::json!({ "format": "pt" }));
    let mut offset = 0usize;
    for name in &names {
        let tensor = checkpoint.tensor(name)?;
        let dtype = match tensor.dtype {
            crate::safetensors::Dtype::F32 => "F32",
            crate::safetensors::Dtype::F16 => "F16",
            crate::safetensors::Dtype::BF16 => "BF16",
        };
        let end = offset + tensor.bytes.len();
        header.insert(
            name.to_string(),
            serde_json::json!({ "dtype": dtype, "shape": tensor.shape, "data_offsets": [offset, end] }),
        );
        offset = end;
    }
    // serde_json's map keeps keys sorted; pad with spaces to 8 bytes, as safetensors does.
    let mut text = serde_json::to_string(&header).map_err(|error| error.to_string())?;
    while text.len() % 8 != 0 {
        text.push(' ');
    }
    let temp = out.with_extension("writing");
    let result = (|| {
        use std::io::Write;
        let mut file = std::io::BufWriter::new(std::fs::File::create(&temp)?);
        file.write_all(&(text.len() as u64).to_le_bytes())?;
        file.write_all(text.as_bytes())?;
        for name in &names {
            file.write_all(checkpoint.tensor(name).map_err(std::io::Error::other)?.bytes)?;
        }
        file.into_inner().map_err(|error| error.into_error())?.sync_all()?;
        std::fs::rename(&temp, out)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result.map_err(|error| format!("无法写入视觉权重 {}: {error}", out.display()))
}

/// Rotary positions `[t, h, w]` of each token of an image laid out as
/// `grid` (rows, columns) whose first token goes at text position `start`,
/// and the position the text after it continues from.
pub fn image_positions(start: u32, grid: (usize, usize)) -> (Vec<[u32; 3]>, u32) {
    let (rows, columns) = grid;
    let mut out = Vec::with_capacity(rows * columns);
    for r in 0..rows {
        for c in 0..columns {
            out.push([start, start + r as u32, start + c as u32]);
        }
    }
    (out, start + rows.max(columns) as u32)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const RELEASE_CONFIG: &str = include_str!("../testdata/qwen3.5-0.8b-config.json");

    /// The synthetic image of `testdata/vision-golden.json`: a white ground, a
    /// red rectangle and a blue disc.
    pub(crate) fn golden_picture() -> Picture {
        let (width, height) = (320, 224);
        let mut rgb = vec![255u8; width * height * 3];
        for y in 0..height {
            for x in 0..width {
                let pixel = &mut rgb[(y * width + x) * 3..][..3];
                if (40..140).contains(&x) && (40..180).contains(&y) {
                    pixel.copy_from_slice(&[220, 30, 30]);
                }
                let (dx, dy) = (x as i64 - 230, y as i64 - 112);
                if dx * dx + dy * dy <= 60 * 60 {
                    pixel.copy_from_slice(&[30, 60, 220]);
                }
            }
        }
        Picture::new(width, height, rgb).unwrap()
    }

    #[test]
    fn parses_the_release_config() {
        let config = VisionConfig::parse(RELEASE_CONFIG.as_bytes()).unwrap();
        assert_eq!(config.depth, 12);
        assert_eq!(config.factor(), 32);
        assert_eq!(config.out_hidden_size, 1024);
        assert_eq!(config.image_token_id, 248056);
        assert_eq!(config.vision_start_token_id, 248053);
        assert_eq!(config.vision_end_token_id, 248054);
        assert_eq!(config.rope_theta, 10000.0);
    }

    #[test]
    fn fits_like_smart_resize() {
        // Already a multiple of 32 and within bounds: unchanged.
        assert_eq!(fit(320, 224, 32, 64, 1024), Some((320, 224)));
        // Upscaled to the minimum (65,536 pixels).
        assert_eq!(fit(100, 50, 32, 64, 1024), Some((384, 192)));
        // A 1920×1080 screenshot, capped at 1024 tokens (values from transformers).
        let (w, h) = fit(1920, 1080, 32, 64, 1024).unwrap();
        assert_eq!((w, h), (1344, 768));
        assert!(w / 32 * h / 32 <= 1024);
        // Round half to even, as Python does: 48/32 = 1.5 rounds to 2, 80/32 = 2.5 to 2.
        assert_eq!(fit(80, 48, 32, 1, 1024), Some((64, 64)));
        assert_eq!(fit(1, 300, 32, 64, 1024), None);
        assert_eq!(fit(0, 10, 32, 64, 1024), None);
    }

    #[test]
    fn patches_follow_merge_block_order() {
        let config = VisionConfig::parse(RELEASE_CONFIG.as_bytes()).unwrap();
        // 64×32: patches (0,0) (0,1) (1,0) (1,1) form the first block.
        let (width, height) = (64, 32);
        let mut rgb = vec![0u8; width * height * 3];
        for y in 0..height {
            for x in 0..width {
                rgb[(y * width + x) * 3] = ((y / 16) * 4 + x / 16) as u8 * 10;
            }
        }
        let picture = Picture::new(width, height, rgb).unwrap();
        let out = patches(&picture, &config).unwrap();
        let dim = config.patch_dim();
        let red: Vec<u8> = (0..8).map(|i| ((out[i * dim] * 0.5 + 0.5) * 255.0).round() as u8).collect();
        assert_eq!(red, [0, 10, 40, 50, 20, 30, 60, 70]);
        // Both frames carry the same pixels.
        assert_eq!(out[0], out[256]);
        assert_eq!(patch_positions(2, 4, 2), [(0, 0), (0, 1), (1, 0), (1, 1), (0, 2), (0, 3), (1, 2), (1, 3)]);
    }

    #[test]
    fn image_positions_match_get_rope_index() {
        let (positions, next) = image_positions(15, (7, 10));
        assert_eq!(positions[0], [15, 15, 15]);
        assert_eq!(positions[9], [15, 15, 24]);
        assert_eq!(positions[10], [15, 16, 15]);
        assert_eq!(next, 25);
    }

    #[test]
    fn small_math_matches_torch() {
        assert!((gelu_tanh(1.0) - 0.841_192).abs() < 1e-6);
        assert!((gelu_erf(1.0) - 0.841_344_7).abs() < 1e-6);
        let mut row = vec![1.0, 2.0, 3.0];
        softmax(&mut row, 1.0);
        assert!((row[2] - 0.665_240_9).abs() < 1e-6);
        let (a, b) = (vec![1.0, 2.0, 3.0, 4.0], vec![5.0, 6.0, 7.0, 8.0]);
        let mut c = vec![0.0; 4];
        gemm(true, 2, 2, 2, &a, &b, &mut c);
        assert_eq!(c, [17.0, 23.0, 39.0, 53.0]);
        gemm(false, 2, 2, 2, &a, &b, &mut c);
        assert_eq!(c, [19.0, 22.0, 43.0, 50.0]);
    }

    #[derive(serde::Deserialize)]
    struct Golden {
        grid_thw: Vec<usize>,
        pixel_values_head: Vec<f32>,
        features_shape: Vec<usize>,
        features_rows: std::collections::BTreeMap<String, Vec<f32>>,
        features_mean: f64,
        features_abs_mean: f64,
    }

    pub(crate) fn golden_json() -> serde_json::Value {
        serde_json::from_str(include_str!("../testdata/vision-golden.json")).unwrap()
    }

    /// The golden request split for a backend: the system prompt's prefix
    /// tokens, the input after it (text, the picture, text), and
    /// transformers' greedy reply.
    pub(crate) struct GoldenRequest {
        pub prefix: Vec<u32>,
        pub input: Vec<crate::engine::Segment>,
        pub expected: Vec<u32>,
    }

    pub(crate) fn golden_request(tokenizer: &crate::tokenizer::Tokenizer) -> GoldenRequest {
        use crate::engine::{prefix_tokens, reply_start, Segment, Specials};
        let golden = golden_json();
        let ids: Vec<u32> = serde_json::from_value(golden["input_ids"].clone()).unwrap();
        let config = VisionConfig::parse(RELEASE_CONFIG.as_bytes()).unwrap();
        let specials = Specials::from_tokenizer(tokenizer).unwrap();
        let prefix = prefix_tokens(tokenizer, &specials, "You are a helpful assistant.");
        assert_eq!(ids[..prefix.len()], prefix[..], "the system prompt tokenizes as transformers does");
        let first_pad = ids.iter().position(|t| *t == config.image_token_id).unwrap();
        let after = ids.iter().rposition(|t| *t == config.image_token_id).unwrap() + 1;
        let mut tail = vec![config.vision_end_token_id];
        tail.extend(tokenizer.encode_ordinary("What shapes and colors are in this image?"));
        tail.extend(reply_start(tokenizer, &specials));
        assert_eq!(ids[after..], tail[..], "the rest of the turn tokenizes as transformers does");
        let input = vec![
            Segment::Tokens(ids[prefix.len()..first_pad].to_vec()),
            Segment::Picture(std::sync::Arc::new(golden_picture())),
            Segment::Tokens(ids[after..].to_vec()),
        ];
        GoldenRequest {
            prefix,
            input,
            expected: serde_json::from_value(golden["greedy"].clone()).unwrap(),
        }
    }

    /// Against transformers' fp32 output for the same image. Needs
    /// `MEWORK_LOCAL_MODEL_DIR` (the official release).
    #[test]
    fn encodes_like_transformers() {
        let Ok(dir) = std::env::var("MEWORK_LOCAL_MODEL_DIR") else {
            eprintln!("MEWORK_LOCAL_MODEL_DIR not set; skipping");
            return;
        };
        let dir = Path::new(&dir);
        let golden: Golden = serde_json::from_value(golden_json()).unwrap();
        let config = VisionConfig::load(&dir.join("config.json")).unwrap();
        let tower = VisionTower::open(config.clone(), &dir.join("model.safetensors")).unwrap();
        let picture = golden_picture();
        assert_eq!(patches(&picture, &config).unwrap()[..8], golden.pixel_values_head[..]);
        let started = std::time::Instant::now();
        let features = tower.encode(&picture).unwrap();
        eprintln!("encoded {:?} in {:?}", features.grid, started.elapsed());
        assert_eq!([1, features.grid.0 * 2, features.grid.1 * 2], golden.grid_thw[..]);
        assert_eq!([features.rows.len() / features.width, features.width], golden.features_shape[..]);
        let mut worst = 0f32;
        for (row, expected) in &golden.features_rows {
            let row: usize = row.parse().unwrap();
            for (i, e) in expected.iter().enumerate() {
                worst = worst.max((features.rows[row * features.width + i] - e).abs());
            }
        }
        let n = features.rows.len() as f64;
        let mean = features.rows.iter().map(|v| *v as f64).sum::<f64>() / n;
        let abs_mean = features.rows.iter().map(|v| v.abs() as f64).sum::<f64>() / n;
        eprintln!("worst {worst:e}; mean {mean} vs {}; |mean| {abs_mean} vs {}", golden.features_mean, golden.features_abs_mean);
        assert!(worst < 1e-3, "features differ by {worst}");
        // A full-size screenshot's worth: 1,008 tokens.
        let big = Picture::new(1344, 768, (0..1344 * 768 * 3).map(|i| (i * 7 % 251) as u8).collect()).unwrap();
        let started = std::time::Instant::now();
        let features = tower.encode(&big).unwrap();
        eprintln!("encoded {:?} in {:?}", features.grid, started.elapsed());
        assert!((mean - golden.features_mean).abs() < 1e-5);
        assert!((abs_mean - golden.features_abs_mean).abs() < 1e-5);
    }
}
