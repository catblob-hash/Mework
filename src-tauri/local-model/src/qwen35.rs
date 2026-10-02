//! The Qwen3.5 text model's hyperparameters, read from the release's
//! `config.json`, and the names its tensors carry in the checkpoint.
//!
//! The release is a vision-language model; only the language model is used.
//! Its layers alternate Gated DeltaNet ("linear attention", a recurrent state
//! of fixed size per sequence) with gated full attention (a KV cache that grows
//! with the sequence), three to one.

use std::path::Path;

use serde::Deserialize;

/// Prefix of every language-model tensor in the checkpoint. The vision tower
/// (`model.visual.*`) and the multi-token-prediction head (`mtp.*`) are unused.
pub const TENSOR_PREFIX: &str = "model.language_model.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerKind {
    /// Gated DeltaNet.
    Linear,
    /// Gated softmax attention with a KV cache.
    Full,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub hidden_size: usize,
    pub intermediate_size: usize,
    pub layers: Vec<LayerKind>,
    pub vocab_size: usize,
    pub rms_norm_eps: f32,
    pub tie_word_embeddings: bool,
    pub max_position_embeddings: usize,
    // Full attention.
    pub num_attention_heads: usize,
    pub num_key_value_heads: usize,
    pub head_dim: usize,
    pub rope_theta: f64,
    /// Rotary dimensions per head (a prefix of `head_dim`).
    pub rotary_dim: usize,
    /// Frequencies per position axis (temporal, height, width) for image
    /// tokens; text tokens have the same position on all three.
    pub mrope_section: Vec<usize>,
    /// The axes take turns frequency by frequency instead of in runs.
    pub mrope_interleaved: bool,
    // Gated DeltaNet.
    pub linear_num_key_heads: usize,
    pub linear_num_value_heads: usize,
    pub linear_key_head_dim: usize,
    pub linear_value_head_dim: usize,
    pub linear_conv_kernel_dim: usize,
    pub eos_token_id: u32,
}

#[derive(Deserialize)]
struct RawRoot {
    #[serde(default)]
    model_type: String,
    text_config: Option<RawText>,
}

#[derive(Deserialize)]
struct RawText {
    #[serde(default)]
    model_type: String,
    hidden_size: usize,
    intermediate_size: usize,
    num_hidden_layers: usize,
    layer_types: Vec<String>,
    vocab_size: usize,
    rms_norm_eps: f32,
    #[serde(default)]
    tie_word_embeddings: bool,
    max_position_embeddings: usize,
    num_attention_heads: usize,
    num_key_value_heads: usize,
    head_dim: usize,
    #[serde(default)]
    attn_output_gate: Option<bool>,
    rope_parameters: RawRope,
    linear_num_key_heads: usize,
    linear_num_value_heads: usize,
    linear_key_head_dim: usize,
    linear_value_head_dim: usize,
    linear_conv_kernel_dim: usize,
    eos_token_id: u32,
}

#[derive(Deserialize)]
struct RawRope {
    rope_theta: f64,
    #[serde(default = "default_partial_rotary")]
    partial_rotary_factor: f64,
    #[serde(default)]
    mrope_section: Vec<usize>,
    #[serde(default)]
    mrope_interleaved: bool,
    #[serde(default)]
    rope_type: Option<String>,
}

fn default_partial_rotary() -> f64 {
    1.0
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|error| format!("无法读取模型配置 {}: {error}", path.display()))?;
        Self::parse(&bytes)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let root: RawRoot = serde_json::from_slice(bytes).map_err(|error| format!("模型配置无法解析: {error}"))?;
        if root.model_type != "qwen3_5" {
            return Err(format!("不是 Qwen3.5 模型（model_type = {:?}）", root.model_type));
        }
        let text = root.text_config.ok_or("模型配置缺少 text_config")?;
        if text.model_type != "qwen3_5_text" {
            return Err(format!("不是 Qwen3.5 语言模型（text_config.model_type = {:?}）", text.model_type));
        }
        if text.attn_output_gate == Some(false) {
            return Err("不支持没有输出门的注意力".into());
        }
        if text.rope_parameters.rope_type.as_deref().is_some_and(|kind| kind != "default") {
            return Err("不支持的 RoPE 类型".into());
        }
        if text.layer_types.len() != text.num_hidden_layers {
            return Err("layer_types 与层数不符".into());
        }
        let layers = text
            .layer_types
            .iter()
            .map(|kind| match kind.as_str() {
                "linear_attention" => Ok(LayerKind::Linear),
                "full_attention" => Ok(LayerKind::Full),
                other => Err(format!("未知的层类型 {other}")),
            })
            .collect::<Result<Vec<_>, _>>()?;
        if text.num_attention_heads % text.num_key_value_heads != 0
            || text.linear_num_value_heads % text.linear_num_key_heads != 0
        {
            return Err("注意力头数不能整除".into());
        }
        let rotary_dim = (text.head_dim as f64 * text.rope_parameters.partial_rotary_factor) as usize;
        if rotary_dim == 0 || rotary_dim % 2 != 0 || rotary_dim > text.head_dim {
            return Err("RoPE 维度无效".into());
        }
        let sections = &text.rope_parameters.mrope_section;
        if !sections.is_empty() && (sections.len() != 3 || sections.iter().sum::<usize>() != rotary_dim / 2) {
            return Err("mrope_section 与 RoPE 维度不符".into());
        }
        Ok(Self {
            hidden_size: text.hidden_size,
            intermediate_size: text.intermediate_size,
            layers,
            vocab_size: text.vocab_size,
            rms_norm_eps: text.rms_norm_eps,
            tie_word_embeddings: text.tie_word_embeddings,
            max_position_embeddings: text.max_position_embeddings,
            num_attention_heads: text.num_attention_heads,
            num_key_value_heads: text.num_key_value_heads,
            head_dim: text.head_dim,
            rope_theta: text.rope_parameters.rope_theta,
            rotary_dim,
            mrope_section: text.rope_parameters.mrope_section,
            mrope_interleaved: text.rope_parameters.mrope_interleaved,
            linear_num_key_heads: text.linear_num_key_heads,
            linear_num_value_heads: text.linear_num_value_heads,
            linear_key_head_dim: text.linear_key_head_dim,
            linear_value_head_dim: text.linear_value_head_dim,
            linear_conv_kernel_dim: text.linear_conv_kernel_dim,
            eos_token_id: text.eos_token_id,
        })
    }

    pub fn linear_key_dim(&self) -> usize {
        self.linear_num_key_heads * self.linear_key_head_dim
    }

    pub fn linear_value_dim(&self) -> usize {
        self.linear_num_value_heads * self.linear_value_head_dim
    }

    /// Channels of the DeltaNet short convolution: q, k and v side by side.
    pub fn linear_conv_dim(&self) -> usize {
        2 * self.linear_key_dim() + self.linear_value_dim()
    }

    /// Which position axis (0 temporal, 1 height, 2 width) turns rotary
    /// frequency `i` (of `rotary_dim / 2`), as transformers'
    /// `recomposition_frequencies` assigns them.
    pub fn rotary_axis(&self, i: usize) -> usize {
        let s = &self.mrope_section;
        if s.len() != 3 {
            return 0;
        }
        if self.mrope_interleaved {
            match i % 3 {
                1 if i < 3 * s[1] => 1,
                2 if i < 3 * s[2] => 2,
                _ => 0,
            }
        } else if i < s[0] {
            0
        } else if i < s[0] + s[1] {
            1
        } else {
            2
        }
    }

    /// RoPE `cos` and `sin` (`rotary_dim` each, both halves filled) at
    /// position `[t, h, w]`, from per-position tables of `rotary_dim` values
    /// (`rope_tables`).
    pub fn rotary_row<T: Copy>(&self, table: &[T], position: [u32; 3], out: &mut [T]) {
        let rot = self.rotary_dim;
        let half = rot / 2;
        for i in 0..half {
            let p = position[self.rotary_axis(i)] as usize;
            out[i] = table[p * rot + i];
            out[i + half] = table[p * rot + i + half];
        }
    }

    pub fn full_attention_layers(&self) -> usize {
        self.layers.iter().filter(|kind| **kind == LayerKind::Full).count()
    }

    pub fn linear_attention_layers(&self) -> usize {
        self.layers.len() - self.full_attention_layers()
    }
}

/// Checkpoint name of a language-model tensor, e.g. `layer(3, "self_attn.q_proj.weight")`.
pub fn layer_tensor(layer: usize, suffix: &str) -> String {
    format!("{TENSOR_PREFIX}layers.{layer}.{suffix}")
}

pub fn model_tensor(suffix: &str) -> String {
    format!("{TENSOR_PREFIX}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) const RELEASE_CONFIG: &str = include_str!("../testdata/qwen3.5-0.8b-config.json");

    #[test]
    fn parses_the_release_config() {
        let config = Config::parse(RELEASE_CONFIG.as_bytes()).unwrap();
        assert_eq!(config.layers.len(), 24);
        assert_eq!(config.full_attention_layers(), 6);
        assert_eq!(config.layers[3], LayerKind::Full);
        assert_eq!(config.rotary_dim, 64);
        assert_eq!(config.linear_conv_dim(), 6144);
        assert_eq!(config.vocab_size, 248320);
        assert!(config.tie_word_embeddings);
        assert!(config.mrope_interleaved);
        // transformers: h takes 1, 4, …, 31; w takes 2, 5, …, 29; t the rest.
        let axes: Vec<usize> = (0..32).map(|i| config.rotary_axis(i)).collect();
        assert_eq!(axes.iter().filter(|a| **a == 1).count(), 11);
        assert_eq!(axes.iter().filter(|a| **a == 2).count(), 10);
        assert_eq!(&axes[..6], &[0, 1, 2, 0, 1, 2]);
        assert_eq!(&axes[29..], &[2, 0, 1]);
        let table: Vec<u32> = (0..3 * 64).map(|i| i as u32).collect();
        let mut row = vec![0u32; 64];
        config.rotary_row(&table, [0, 1, 2], &mut row);
        assert_eq!(&row[..3], &[0, 64 + 1, 128 + 2]);
        assert_eq!(&row[32..35], &[32, 64 + 33, 128 + 34]);
    }

    #[test]
    fn rejects_other_models() {
        assert!(Config::parse(br#"{"model_type":"qwen3"}"#).is_err());
    }
}
