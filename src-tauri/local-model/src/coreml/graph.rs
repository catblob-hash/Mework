//! The Qwen3.5 text model as ML Program functions laid out for the Apple
//! Neural Engine.
//!
//! Layout: activations are `[N, C, 1, T]` and every projection is a 1x1
//! `conv`; the ANE compiler rejects `linear` on 2-D tensors. Three functions:
//!
//! - `decode`:  B independent sequences, one token each (`x` is `[B, H, 1, 1]`);
//! - `prefill`: one sequence, T tokens at once, with the chunked gated delta
//!   rule for the DeltaNet layers (`x` is `[1, H, 1, T]`);
//! - `head`:    final hidden states of B rows to logits.
//!
//! The KV cache and the DeltaNet states are Core ML states. Everything the
//! host would otherwise index dynamically (cache write positions, masks,
//! RoPE angles, the conv-window selection) is passed in as dense inputs, so
//! no op needs a dynamic index. See `docs/why.md` for the Neural Engine
//! constraints each workaround below answers.

use std::collections::BTreeMap;

use super::blob::{BlobPlan, BlobSource};
use super::mil::{Function, Var};
use crate::qwen35::{layer_tensor, model_tensor, Config, LayerKind};
use crate::safetensors::{f32_to_f16, SafeTensors};

/// The DeltaNet state is stored times this, so `q·S` and `k·S` land in fp16's
/// normal range; the ANE flushes subnormal products to zero.
pub const STATE_SCALE: f32 = 64.0;
/// Additive attention mask value for excluded positions.
pub const MASK_OFF: f32 = -30000.0;

#[derive(Clone, Copy, Debug)]
pub struct Shapes {
    /// Sequences per decode call.
    pub slots: usize,
    /// Tokens per prefill call.
    pub chunk: usize,
    /// KV cache positions per sequence.
    pub context: usize,
}

pub struct Builder<'a> {
    pub config: &'a Config,
    pub shapes: Shapes,
    weights: &'a SafeTensors,
    pub blobs: BlobPlan,
    /// The output projection's row blocks as `(rows, metadata offset)`, in
    /// vocabulary order. With tied embeddings these are also the embedding
    /// table, which the host reads its lookups from.
    pub head_blobs: Vec<(usize, u64)>,
    /// Small derived constants (norm weights, gates) by key, computed once.
    small: BTreeMap<String, Vec<f32>>,
}

fn f16s(values: &[f32]) -> Vec<u16> {
    values.iter().map(|v| f32_to_f16(*v)).collect()
}

impl<'a> Builder<'a> {
    pub fn new(config: &'a Config, weights: &'a SafeTensors, shapes: Shapes) -> Self {
        Self { config, shapes, weights, blobs: BlobPlan::default(), head_blobs: Vec::new(), small: BTreeMap::new() }
    }

    fn vector(&mut self, name: &str) -> Result<Vec<f32>, String> {
        if let Some(values) = self.small.get(name) {
            return Ok(values.clone());
        }
        let values = self.weights.tensor(name)?.to_f32();
        self.small.insert(name.to_string(), values.clone());
        Ok(values)
    }

    /// A 1x1 conv weight `[out, in, 1, 1]` straight from a `[out, in]` matrix.
    fn conv_weight(&mut self, f: &mut Function, name: &str) -> Result<Var, String> {
        let info = self.weights.info(name).ok_or_else(|| format!("权重里缺少张量 {name}"))?;
        let (out, inp) = (info.shape[0], info.shape[1]);
        let offset = self.blobs.add(
            name,
            out * inp,
            BlobSource::Checkpoint { tensor: name.to_string(), rows: None, scale: 1.0 },
        );
        Ok(f.blob(&[out, inp, 1, 1], offset))
    }

    fn owned(&mut self, f: &mut Function, key: &str, shape: &[usize], values: &[f32]) -> Var {
        let n: usize = shape.iter().product();
        assert_eq!(n, values.len());
        if n <= 16 {
            return f.tensor_f16(shape, f16s(values));
        }
        let offset = self.blobs.add(key, n, BlobSource::Owned(f16s(values)));
        f.blob(shape, offset)
    }

    fn conv(&mut self, f: &mut Function, x: Var, name: &str) -> Result<Var, String> {
        let w = self.conv_weight(f, name)?;
        Ok(f.conv(x, w, None, 1))
    }

    /// RMSNorm over axis `axis` (1 or -1) as layer_norm of `[x, -x]`, whose
    /// mean is exactly zero; the ANE has no RMSNorm and computes layer_norm
    /// without overflowing on the squares.
    fn norm(
        &mut self,
        f: &mut Function,
        x: Var,
        axis: i64,
        weight: Option<(&str, bool)>,
        eps: f32,
    ) -> Result<Var, String> {
        let rank = f.shape(x).len();
        let a = if axis < 0 { rank - 1 } else { axis as usize };
        let n = f.shape(x)[a];
        let neg = f.mul_s(x, -1.0);
        let cat = f.concat(&[x, neg], axis);
        let ln = f.layer_norm(cat, axis, eps);
        let half = f.split(ln, &[n, n], axis)[0];
        let Some((name, plus_one)) = weight else { return Ok(half) };
        let mut w = self.vector(name)?;
        if plus_one {
            w.iter_mut().for_each(|v| *v += 1.0);
        }
        let shape: Vec<usize> = if a == rank - 1 { vec![n] } else { vec![1, n, 1, 1] };
        let key = format!("{name}#norm{}", if plus_one { "+1" } else { "" });
        let wv = self.owned(f, &key, &shape, &w);
        Ok(f.mul(half, wv))
    }

    fn l2(&mut self, f: &mut Function, x: Var) -> Result<Var, String> {
        let n = *f.shape(x).last().expect("rank") as f32;
        let unit = self.norm(f, x, -1, None, 1e-6 / n)?;
        Ok(f.mul_s(unit, 1.0 / n.sqrt()))
    }

    /// The ANE's sigmoid/silu are table approximations with large relative
    /// error near zero; 0.5 + 0.5·tanh(x/2) is not.
    fn sigmoid(f: &mut Function, x: Var) -> Var {
        let half = f.mul_s(x, 0.5);
        let t = f.tanh(half);
        let t = f.mul_s(t, 0.5);
        f.add_s(t, 0.5)
    }

    fn silu(f: &mut Function, x: Var) -> Var {
        let s = Self::sigmoid(f, x);
        f.mul(x, s)
    }

    /// softplus(min(x, 10)) + relu(x - 10): the ANE's softplus overflows fp16
    /// once exp(x) does.
    fn softplus(f: &mut Function, x: Var) -> Var {
        let ten = f.scalar(10.0);
        let low = f.minimum(x, ten);
        let low = f.softplus(low);
        let high = f.sub(x, ten);
        let high = f.relu(high);
        f.add(low, high)
    }

    fn rope(&mut self, f: &mut Function, x: Var, cos: Var, sin: Var) -> Var {
        let c = self.config;
        let rot = c.rotary_dim;
        let parts = f.split(x, &[rot, c.head_dim - rot], -1);
        let halves = f.split(parts[0], &[rot / 2, rot / 2], -1);
        let neg = f.mul_s(halves[1], -1.0);
        let rotated = f.concat(&[neg, halves[0]], -1);
        let a = f.mul(parts[0], cos);
        let b = f.mul(rotated, sin);
        let r = f.add(a, b);
        f.concat(&[r, parts[1]], -1)
    }

    fn mlp(&mut self, f: &mut Function, x: Var, layer: usize) -> Result<Var, String> {
        let h = self.norm(f, x, 1, Some((&layer_tensor(layer, "post_attention_layernorm.weight"), true)), self.eps())?;
        let gate = self.conv(f, h, &layer_tensor(layer, "mlp.gate_proj.weight"))?;
        let up = self.conv(f, h, &layer_tensor(layer, "mlp.up_proj.weight"))?;
        let act = Self::silu(f, gate);
        let act = f.mul(act, up);
        let down = self.conv(f, act, &layer_tensor(layer, "mlp.down_proj.weight"))?;
        Ok(f.add(x, down))
    }

    fn eps(&self) -> f32 {
        self.config.rms_norm_eps
    }

    /// `-exp(A_log)` per value head, `[1, heads, 1, 1]`.
    fn decay_rate(&mut self, f: &mut Function, layer: usize) -> Result<Var, String> {
        let a: Vec<f32> = self.vector(&layer_tensor(layer, "linear_attn.A_log"))?.iter().map(|v| -v.exp()).collect();
        let heads = a.len();
        Ok(self.owned(f, &format!("{layer}#A"), &[1, heads, 1, 1], &a))
    }

    fn dt_bias(&mut self, f: &mut Function, layer: usize) -> Result<Var, String> {
        let name = layer_tensor(layer, "linear_attn.dt_bias");
        let b = self.vector(&name)?;
        let heads = b.len();
        Ok(self.owned(f, &name, &[1, heads, 1, 1], &b))
    }

    /// The DeltaNet short-conv taps, `[1, 1, K, C]` (channels last).
    fn conv_taps(&mut self, f: &mut Function, layer: usize) -> Result<Var, String> {
        let c = self.config;
        let (channels, k) = (c.linear_conv_dim(), c.linear_conv_kernel_dim);
        let name = layer_tensor(layer, "linear_attn.conv1d.weight");
        let w = self.vector(&name)?; // [C, 1, K]
        let mut taps = vec![0f32; channels * k];
        for ch in 0..channels {
            for t in 0..k {
                taps[t * channels + ch] = w[ch * k + t];
            }
        }
        Ok(self.owned(f, &format!("{name}#taps"), &[1, 1, k, channels], &taps))
    }

    /// The same taps as a depthwise conv weight `[C, 1, 1, K]`.
    fn conv_depthwise(&mut self, f: &mut Function, layer: usize) -> Result<Var, String> {
        let c = self.config;
        let (channels, k) = (c.linear_conv_dim(), c.linear_conv_kernel_dim);
        let name = layer_tensor(layer, "linear_attn.conv1d.weight");
        let w = self.vector(&name)?;
        Ok(self.owned(f, &format!("{name}#dw"), &[channels, 1, 1, k], &w))
    }

    pub fn state_shapes(&self, slots: usize, layers: std::ops::Range<usize>) -> Vec<(String, Vec<usize>)> {
        let c = self.config;
        let ctx = self.shapes.context;
        let mut out = Vec::new();
        for layer in layers {
            match c.layers[layer] {
                LayerKind::Linear => {
                    out.push((format!("conv_state_{layer}"), vec![slots, 1, c.linear_conv_kernel_dim, c.linear_conv_dim()]));
                    out.push((
                        format!("ssm_state_{layer}"),
                        vec![slots, c.linear_num_value_heads, c.linear_key_head_dim, c.linear_value_head_dim],
                    ));
                }
                LayerKind::Full => {
                    out.push((format!("k_cache_{layer}"), vec![slots, c.num_key_value_heads, ctx, c.head_dim]));
                    out.push((format!("v_cache_{layer}"), vec![slots, c.num_key_value_heads, ctx, c.head_dim]));
                }
            }
        }
        out
    }

    // ------------------------------------------------------------ decode

    pub fn decode(&mut self, name: &str, layers: std::ops::Range<usize>) -> Result<Function, String> {
        let c = self.config.clone();
        let (b, ctx) = (self.shapes.slots, self.shapes.context);
        let mut f = Function::new(name);
        let mut x = f.input("x", &[b, c.hidden_size, 1, 1]);
        let cos = f.input("cos", &[b, 1, 1, c.rotary_dim]);
        let sin = f.input("sin", &[b, 1, 1, c.rotary_dim]);
        let onehot = f.input("onehot", &[b, 1, ctx, 1]);
        let keep = f.input("keep", &[b, 1, ctx, 1]);
        let mask = f.input("mask", &[b, 1, 1, ctx]);
        let states: Vec<Var> =
            self.state_shapes(b, layers.clone()).iter().map(|(name, shape)| f.state(name, shape)).collect();
        let (lk, lv, dk, dv) = (c.linear_num_key_heads, c.linear_num_value_heads, c.linear_key_head_dim, c.linear_value_head_dim);
        let (kd, vd, conv_dim, kc) = (c.linear_key_dim(), c.linear_value_dim(), c.linear_conv_dim(), c.linear_conv_kernel_dim);
        let mut si = 0;
        for layer in layers.clone() {
            let h = self.norm(&mut f, x, 1, Some((&layer_tensor(layer, "input_layernorm.weight"), true)), self.eps())?;
            let mixed = match c.layers[layer] {
                LayerKind::Linear => {
                    let (conv_st, ssm_st) = (states[si], states[si + 1]);
                    si += 2;
                    let p = |s: &str| layer_tensor(layer, &format!("linear_attn.{s}"));
                    let qkv = self.conv(&mut f, h, &p("in_proj_qkv.weight"))?;
                    let z = self.conv(&mut f, h, &p("in_proj_z.weight"))?;
                    let z = f.reshape(z, &[b, lv, 1, dv]);
                    let beta_logit = self.conv(&mut f, h, &p("in_proj_b.weight"))?;
                    let a = self.conv(&mut f, h, &p("in_proj_a.weight"))?;
                    // Conv window state [B,1,K,C]: channels last (the ANE pads the
                    // last axis to 64 bytes) and holding this token too, so the conv
                    // reads the written state back instead of the value it wrote.
                    let prev = f.read_state(conv_st);
                    let old = f.slice(prev, &[0, 0, 1, 0], &[b, 1, kc, conv_dim]);
                    let row = f.reshape(qkv, &[b, 1, 1, conv_dim]);
                    let window = f.concat(&[old, row], 2);
                    let window = f.update_state(conv_st, window);
                    let taps = self.conv_taps(&mut f, layer)?;
                    let conv = f.mul(window, taps);
                    let conv = f.reduce_sum(conv, 2);
                    let conv = f.reshape(conv, &[b, conv_dim, 1, 1]);
                    let conv = Self::silu(&mut f, conv);
                    let qkv = f.split(conv, &[kd, kd, vd], 1);
                    let q = f.reshape(qkv[0], &[b, lk, 1, dk]);
                    let q = self.l2(&mut f, q)?; // unit; 1/sqrt(dk) goes into the output norm's eps
                    let k = f.reshape(qkv[1], &[b, lk, 1, dk]);
                    let k = self.l2(&mut f, k)?;
                    let v = f.reshape(qkv[2], &[b, lv, 1, dv]);
                    let beta = Self::sigmoid(&mut f, beta_logit);
                    let bias = self.dt_bias(&mut f, layer)?;
                    let rate = self.decay_rate(&mut f, layer)?;
                    let g = f.add(a, bias);
                    let g = Self::softplus(&mut f, g);
                    let g = f.mul(g, rate);
                    let decay = f.exp(g);
                    let s = f.read_state(ssm_st);
                    let s = f.mul(s, decay);
                    let mem = f.matmul(k, s, false, false);
                    let mem = f.mul_s(mem, 1.0 / STATE_SCALE);
                    let delta = f.sub(v, mem);
                    let beta = f.mul_s(beta, STATE_SCALE);
                    let delta = f.mul(delta, beta);
                    let outer = f.matmul(k, delta, true, false);
                    let s = f.add(s, outer);
                    let s = f.update_state(ssm_st, s);
                    let o = f.matmul(q, s, false, false); // STATE_SCALE·sqrt(dk)·o
                    let scale = STATE_SCALE * (dk as f32).sqrt();
                    let o = self.norm(&mut f, o, -1, Some((&p("norm.weight"), false)), self.eps() * scale * scale)?;
                    let gate = Self::silu(&mut f, z);
                    let o = f.mul(o, gate);
                    let o = f.reshape(o, &[b, vd, 1, 1]);
                    self.conv(&mut f, o, &p("out_proj.weight"))?
                }
                LayerKind::Full => {
                    let (k_st, v_st) = (states[si], states[si + 1]);
                    si += 2;
                    let p = |s: &str| layer_tensor(layer, &format!("self_attn.{s}"));
                    let (nh, nkv, hd) = (c.num_attention_heads, c.num_key_value_heads, c.head_dim);
                    let qg = self.conv(&mut f, h, &p("q_proj.weight"))?;
                    let qg = f.reshape(qg, &[b, nh, 1, 2 * hd]);
                    let qg = f.split(qg, &[hd, hd], -1);
                    let k = self.conv(&mut f, h, &p("k_proj.weight"))?;
                    let k = f.reshape(k, &[b, nkv, 1, hd]);
                    let v = self.conv(&mut f, h, &p("v_proj.weight"))?;
                    let v = f.reshape(v, &[b, nkv, 1, hd]);
                    let q = self.norm(&mut f, qg[0], -1, Some((&p("q_norm.weight"), true)), self.eps())?;
                    let q = self.rope(&mut f, q, cos, sin);
                    let k = self.norm(&mut f, k, -1, Some((&p("k_norm.weight"), true)), self.eps())?;
                    let k = self.rope(&mut f, k, cos, sin);
                    let kc_ = f.read_state(k_st);
                    let kc_ = f.mul(kc_, keep);
                    let kn = f.mul(onehot, k);
                    let kc_ = f.add(kc_, kn);
                    let kcache = f.update_state(k_st, kc_);
                    let vc = f.read_state(v_st);
                    let vc = f.mul(vc, keep);
                    let vn = f.mul(onehot, v);
                    let vc = f.add(vc, vn);
                    let vcache = f.update_state(v_st, vc);
                    let qh = f.reshape(q, &[b, nkv, nh / nkv, hd]);
                    let scores = f.matmul(qh, kcache, false, true);
                    let scores = f.mul_s(scores, 1.0 / (hd as f32).sqrt());
                    let scores = f.add(scores, mask);
                    let probs = f.softmax(scores, -1);
                    let o = f.matmul(probs, vcache, false, false);
                    let o = f.reshape(o, &[b, nh * hd, 1, 1]);
                    let gate = f.reshape(qg[1], &[b, nh * hd, 1, 1]);
                    let gate = Self::sigmoid(&mut f, gate);
                    let o = f.mul(o, gate);
                    self.conv(&mut f, o, &p("o_proj.weight"))?
                }
            };
            x = f.add(x, mixed);
            x = self.mlp(&mut f, x, layer)?;
        }
        if layers.end == c.layers.len() {
            x = self.norm(&mut f, x, 1, Some((&model_tensor("norm.weight"), true)), self.eps())?;
        }
        f.output(x, "hidden");
        Ok(f)
    }

    // ------------------------------------------------------------ prefill

    pub fn prefill(&mut self, name: &str, layers: std::ops::Range<usize>) -> Result<Function, String> {
        let c = self.config.clone();
        let (t, ctx) = (self.shapes.chunk, self.shapes.context);
        let kc = c.linear_conv_kernel_dim;
        let mut f = Function::new(name);
        let mut x = f.input("x", &[1, c.hidden_size, 1, t]);
        let cos = f.input("cos", &[1, 1, t, c.rotary_dim]);
        let sin = f.input("sin", &[1, 1, t, c.rotary_dim]);
        let scatter = f.input("scatter", &[1, 1, ctx, t]);
        let keep = f.input("keep", &[1, 1, ctx, 1]);
        let mask = f.input("mask", &[1, 1, t, ctx]);
        let valid = f.input("valid", &[1, 1, 1, t]);
        let csel = f.input("csel", &[1, 1, kc, t + kc - 1]);
        let states: Vec<Var> =
            self.state_shapes(1, layers.clone()).iter().map(|(name, shape)| f.state(name, shape)).collect();
        let (lk, lv, dk, dv) = (c.linear_num_key_heads, c.linear_num_value_heads, c.linear_key_head_dim, c.linear_value_head_dim);
        let (kd, vd, conv_dim) = (c.linear_key_dim(), c.linear_value_dim(), c.linear_conv_dim());

        // Chunk constants: pairwise log-decay sums as one matmul (no cancellation
        // between large cumulative sums), inclusive prefix and exclusive suffix sums.
        let mut pair = vec![0f32; t * t * t];
        for i in 0..t {
            for j in 0..=i {
                for s in j + 1..=i {
                    pair[s * t * t + i * t + j] = 1.0;
                }
            }
        }
        let mut prefix = vec![0f32; t * t];
        let mut suffix = vec![0f32; t * t];
        let mut upper_off = vec![0f32; t * t];
        let mut strict_neg = vec![0f32; t * t];
        for i in 0..t {
            for j in 0..t {
                prefix[j * t + i] = if j <= i { 1.0 } else { 0.0 }; // [s, i]: s <= i
                suffix[j * t + i] = if j > i { 1.0 } else { 0.0 }; // [s, i]: s > i
                upper_off[i * t + j] = if j > i { MASK_OFF } else { 0.0 };
                strict_neg[i * t + j] = if j < i { -1.0 } else { 0.0 };
            }
        }
        let tag = format!("t{t}");

        let mut sinks = Vec::new();
        let mut si = 0;
        for layer in layers.clone() {
            let h = self.norm(&mut f, x, 1, Some((&layer_tensor(layer, "input_layernorm.weight"), true)), self.eps())?;
            let mixed = match c.layers[layer] {
                LayerKind::Linear => {
                    let (conv_st, ssm_st) = (states[si], states[si + 1]);
                    si += 2;
                    let p = |s: &str| layer_tensor(layer, &format!("linear_attn.{s}"));
                    let qkv = self.conv(&mut f, h, &p("in_proj_qkv.weight"))?; // [1,C,1,T]
                    let z = self.conv(&mut f, h, &p("in_proj_z.weight"))?;
                    let beta_logit = self.conv(&mut f, h, &p("in_proj_b.weight"))?;
                    let a = self.conv(&mut f, h, &p("in_proj_a.weight"))?;
                    let rows = f.transpose(qkv, &[0, 2, 3, 1]); // [1,1,T,C]
                    let prev = f.read_state(conv_st);
                    let old = f.slice(prev, &[0, 0, 1, 0], &[1, 1, kc, conv_dim]);
                    let window = f.concat(&[old, rows], 2); // [1,1,T+K-1,C]
                    let kept = f.matmul(csel, window, false, false); // the last K real inputs
                    sinks.push(f.update_state(conv_st, kept));
                    let wt = f.transpose(window, &[0, 3, 1, 2]); // [1,C,1,T+K-1]
                    let dw = self.conv_depthwise(&mut f, layer)?;
                    let conv = f.conv(wt, dw, None, conv_dim);
                    let conv = Self::silu(&mut f, conv); // [1,C,1,T]
                    let parts = f.split(conv, &[kd, kd, vd], 1);
                    let heads = |f: &mut Function, v: Var, n: usize, d: usize| {
                        let r = f.reshape(v, &[1, n, d, t]);
                        f.transpose(r, &[0, 1, 3, 2]) // [1,n,T,d]
                    };
                    let q = heads(&mut f, parts[0], lk, dk);
                    let q = self.l2(&mut f, q)?;
                    let k = heads(&mut f, parts[1], lk, dk);
                    let k = self.l2(&mut f, k)?;
                    let v = heads(&mut f, parts[2], lv, dv);
                    let beta = Self::sigmoid(&mut f, beta_logit);
                    let beta = f.mul(beta, valid);
                    let beta = f.reshape(beta, &[1, lv, t, 1]);
                    let bias = self.dt_bias(&mut f, layer)?;
                    let rate = self.decay_rate(&mut f, layer)?;
                    let g = f.add(a, bias);
                    let g = Self::softplus(&mut f, g);
                    let g = f.mul(g, rate);
                    let g = f.mul(g, valid); // [1,h,1,T]; padding neither decays nor writes
                    let pair_c = self.owned(&mut f, &format!("{tag}#pair"), &[t, t * t], &pair);
                    let log_d = f.matmul(g, pair_c, false, false);
                    let log_d = f.reshape(log_d, &[1, lv, t, t]);
                    let off = self.owned(&mut f, &format!("{tag}#upper"), &[t, t], &upper_off);
                    let log_d = f.add(log_d, off);
                    let d = f.exp(log_d); // decay from j to i, lower triangle incl. diagonal
                    let prefix_c = self.owned(&mut f, &format!("{tag}#prefix"), &[t, t], &prefix);
                    let cum = f.matmul(g, prefix_c, false, false); // [1,h,1,T]
                    let suffix_c = self.owned(&mut f, &format!("{tag}#suffix"), &[t, t], &suffix);
                    let suf = f.matmul(g, suffix_c, false, false);
                    let ecum = f.exp(cum);
                    let ecum = f.reshape(ecum, &[1, lv, t, 1]);
                    let esuf = f.exp(suf);
                    let esuf = f.reshape(esuf, &[1, lv, t, 1]);
                    let kb = f.mul(k, beta);
                    let vb = f.mul(v, beta);
                    let kk = f.matmul(kb, k, false, true);
                    let kk = f.mul(kk, d);
                    let strict = self.owned(&mut f, &format!("{tag}#strictneg"), &[t, t], &strict_neg);
                    let n = f.mul(kk, strict);
                    let inv = self.unit_lower_inverse(&mut f, n, lv, t);
                    let u = f.matmul(inv, vb, false, false);
                    let kbe = f.mul(kb, ecum);
                    let w = f.matmul(inv, kbe, false, false);
                    let s0 = f.read_state(ssm_st); // STATE_SCALE·S
                    let ws = f.matmul(w, s0, false, false);
                    let ws = f.mul_s(ws, 1.0 / STATE_SCALE);
                    let vnew = f.sub(u, ws);
                    let vnew = f.mul_s(vnew, STATE_SCALE);
                    let qk = f.matmul(q, k, false, true);
                    let attn = f.mul(qk, d);
                    let qe = f.mul(q, ecum);
                    let inter = f.matmul(qe, s0, false, false);
                    let intra = f.matmul(attn, vnew, false, false);
                    let o = f.add(inter, intra); // STATE_SCALE·sqrt(dk)·o
                    let last = f.slice(cum, &[0, 0, 0, t - 1], &[1, lv, 1, t]);
                    let chunk_decay = f.exp(last);
                    let s1 = f.mul(s0, chunk_decay);
                    let ke = f.mul(k, esuf);
                    let upd = f.matmul(ke, vnew, true, false);
                    let s1 = f.add(s1, upd);
                    sinks.push(f.update_state(ssm_st, s1));
                    let scale = STATE_SCALE * (dk as f32).sqrt();
                    let o = self.norm(&mut f, o, -1, Some((&p("norm.weight"), false)), self.eps() * scale * scale)?;
                    let zt = heads(&mut f, z, lv, dv);
                    let gate = Self::silu(&mut f, zt);
                    let o = f.mul(o, gate);
                    let o = f.transpose(o, &[0, 1, 3, 2]);
                    let o = f.reshape(o, &[1, vd, 1, t]);
                    self.conv(&mut f, o, &p("out_proj.weight"))?
                }
                LayerKind::Full => {
                    let (k_st, v_st) = (states[si], states[si + 1]);
                    si += 2;
                    let p = |s: &str| layer_tensor(layer, &format!("self_attn.{s}"));
                    let (nh, nkv, hd) = (c.num_attention_heads, c.num_key_value_heads, c.head_dim);
                    let rep = nh / nkv;
                    let heads = |f: &mut Function, v: Var, n: usize, d: usize| {
                        let r = f.reshape(v, &[1, n, d, t]);
                        f.transpose(r, &[0, 1, 3, 2])
                    };
                    let qg = self.conv(&mut f, h, &p("q_proj.weight"))?;
                    let qg = heads(&mut f, qg, nh, 2 * hd);
                    let qg = f.split(qg, &[hd, hd], -1);
                    let k = self.conv(&mut f, h, &p("k_proj.weight"))?;
                    let k = heads(&mut f, k, nkv, hd);
                    let v = self.conv(&mut f, h, &p("v_proj.weight"))?;
                    let v = heads(&mut f, v, nkv, hd);
                    let q = self.norm(&mut f, qg[0], -1, Some((&p("q_norm.weight"), true)), self.eps())?;
                    let q = self.rope(&mut f, q, cos, sin);
                    let k = self.norm(&mut f, k, -1, Some((&p("k_norm.weight"), true)), self.eps())?;
                    let k = self.rope(&mut f, k, cos, sin);
                    let kc_ = f.read_state(k_st);
                    let kc_ = f.mul(kc_, keep);
                    let kn = f.matmul(scatter, k, false, false);
                    let kc_ = f.add(kc_, kn);
                    let kcache = f.update_state(k_st, kc_);
                    let vc = f.read_state(v_st);
                    let vc = f.mul(vc, keep);
                    let vn = f.matmul(scatter, v, false, false);
                    let vc = f.add(vc, vn);
                    let vcache = f.update_state(v_st, vc);
                    let qh = f.reshape(q, &[1, nkv, rep * t, hd]);
                    let scores = f.matmul(qh, kcache, false, true);
                    let scores = f.reshape(scores, &[nkv, rep, t, ctx]);
                    let scores = f.mul_s(scores, 1.0 / (hd as f32).sqrt());
                    let scores = f.add(scores, mask);
                    let probs = f.softmax(scores, -1);
                    let probs = f.reshape(probs, &[1, nkv, rep * t, ctx]);
                    let o = f.matmul(probs, vcache, false, false);
                    let o = f.reshape(o, &[1, nh, t, hd]);
                    let gate = Self::sigmoid(&mut f, qg[1]);
                    let o = f.mul(o, gate);
                    let o = f.transpose(o, &[0, 1, 3, 2]);
                    let o = f.reshape(o, &[1, nh * hd, 1, t]);
                    self.conv(&mut f, o, &p("o_proj.weight"))?
                }
            };
            x = f.add(x, mixed);
            x = self.mlp(&mut f, x, layer)?;
        }
        // A state write compiles for the ANE only if its read-back is consumed:
        // fold one element of each into the output, times zero.
        for sink in sinks {
            let rank = f.shape(sink).len();
            let one = f.slice(sink, &vec![0; rank], &vec![1; rank]);
            let one = f.reshape(one, &[1, 1, 1, 1]);
            let zero = f.mul_s(one, 0.0);
            x = f.add(x, zero);
        }
        if layers.end == c.layers.len() {
            x = self.norm(&mut f, x, 1, Some((&model_tensor("norm.weight"), true)), self.eps())?;
        }
        f.output(x, "hidden");
        Ok(f)
    }

    /// `(I - N)^-1` for strictly lower-triangular `N` `[1, heads, t, t]`.
    ///
    /// The ANE rejects a matmul of a tensor with itself, so there is no
    /// repeated squaring; Horner (`I + N(I + N(...))`) works but its chain of
    /// dependent matmuls makes the ANE compiler take minutes past about eight
    /// steps. Blocks of at most 8 get Horner, batched across the two diagonal
    /// blocks; the off-diagonal block is `T22 · N21 · T11`.
    fn unit_lower_inverse(&mut self, f: &mut Function, n: Var, heads: usize, t: usize) -> Var {
        if t <= 8 {
            let eye: Vec<f32> = (0..t * t).map(|i| if i / t == i % t { 1.0 } else { 0.0 }).collect();
            let eye = self.owned(f, &format!("eye{t}"), &[t, t], &eye);
            let mut inv = f.add(n, eye);
            for _ in 0..t.saturating_sub(2) {
                let step = f.matmul(n, inv, false, false);
                inv = f.add(step, eye);
            }
            return inv;
        }
        assert!(t % 2 == 0, "chunk length must be even");
        let h = t / 2;
        let n11 = f.slice(n, &[0, 0, 0, 0], &[1, heads, h, h]);
        let n22 = f.slice(n, &[0, 0, h, h], &[1, heads, t, t]);
        let n21 = f.slice(n, &[0, 0, h, 0], &[1, heads, t, h]);
        let diagonal = f.concat(&[n11, n22], 1);
        let inv = self.unit_lower_inverse(f, diagonal, 2 * heads, h);
        let blocks = f.split(inv, &[heads, heads], 1);
        let lower = f.matmul(n21, blocks[0], false, false);
        let lower = f.matmul(blocks[1], lower, false, false);
        let zeros = self.owned(f, &format!("zeros{heads}x{h}"), &[1, heads, h, h], &vec![0.0; heads * h * h]);
        let top = f.concat(&[blocks[0], zeros], -1);
        let bottom = f.concat(&[lower, blocks[1]], -1);
        f.concat(&[top, bottom], -2)
    }

    // ------------------------------------------------------------ head

    /// Logits `[B, 1, 1, V]` from final hidden states `[B, H, 1, 1]`. The
    /// vocabulary is split in 16 convs (a single 248k-channel conv exceeds the
    /// ANE's limits) and transposed so the vocabulary is the dense last axis.
    pub fn head(&mut self, name: &str, splits: usize) -> Result<Function, String> {
        let c = self.config.clone();
        let b = self.shapes.slots;
        let table = if c.tie_word_embeddings { model_tensor("embed_tokens.weight") } else { "lm_head.weight".to_string() };
        let vocab = self.weights.info(&table).ok_or("权重里缺少词表")?.shape[0];
        let mut f = Function::new(name);
        let x = f.input("x", &[b, c.hidden_size, 1, 1]);
        let mut outs = Vec::new();
        let mut start = 0;
        for i in 0..splits {
            let n = vocab / splits + usize::from(i < vocab % splits);
            let offset = self.blobs.add(
                &format!("{table}#rows{start}"),
                n * c.hidden_size,
                BlobSource::Checkpoint { tensor: table.clone(), rows: Some((start, start + n)), scale: 1.0 },
            );
            let w = f.blob(&[n, c.hidden_size, 1, 1], offset);
            outs.push(f.conv(x, w, None, 1));
            self.head_blobs.push((n, offset));
            start += n;
        }
        let logits = f.concat(&outs, 1);
        let logits = f.reshape(logits, &[b, 1, vocab, 1]);
        let logits = f.transpose(logits, &[0, 1, 3, 2]);
        f.output(logits, "logits");
        Ok(f)
    }
}
