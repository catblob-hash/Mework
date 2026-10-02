//! `engine::Backend` on the Neural Engine.
//!
//! The package has, per layer range, a `prefill_N` function (one sequence,
//! a chunk of tokens) and a `decode_N` function (all slots, one token each),
//! plus `head`. Prefill runs on its own single-sequence states; when a
//! request's prompt is in, its state is copied into its slot of the decode
//! states, and from then on every step advances all live slots in one pass.

use std::ops::Range;
use std::path::Path;

use super::blob::read_record;
use super::graph::{Shapes, MASK_OFF};
use super::package::{EmbeddingLayout, PackagePlan, GRAPH_VERSION, HEAD};
use super::runtime::{Function, State, Tensor16};
use crate::engine::{rope_tables, Backend, Capacity, Input, Layout, Logits, PrefixState, Segment};
use crate::qwen35::{Config, LayerKind};
use crate::safetensors::{f16_to_f32, f32_to_f16};
use crate::vision::VisionTower;

struct StateSpec {
    name: String,
    /// Shape with a batch of one.
    shape: Vec<usize>,
    /// KV caches only need the filled positions (axis 2) copied.
    positional: bool,
}

pub struct AneBackend {
    config: Config,
    shapes: Shapes,
    prefill: Vec<Function>,
    decode: Vec<Function>,
    head: Function,
    specs: Vec<Vec<StateSpec>>,
    prefill_states: Option<Vec<State>>,
    decode_states: Option<Vec<State>>,
    lens: Vec<usize>,
    /// Per slot: rotary position minus sequence position (negative after an image).
    deltas: Vec<i64>,
    embedding: Embedding,
    vision: Option<VisionTower>,
    cos: Vec<u16>,
    sin: Vec<u16>,
    off: u16,
}

/// One position of a prefill: its input embedding and rotary position.
struct Row {
    embedding: Vec<u16>,
    position: [u32; 3],
}

/// The embedding table read straight from the compiled model's weight file
/// (fp16 rows, in blocks; see `EmbeddingLayout`). The file is mapped, so the
/// rows are clean file-backed pages the system can drop and read back.
struct Embedding {
    map: memmap2::Mmap,
    hidden: usize,
    /// `(first row, rows, data offset)` per block.
    blocks: Vec<(usize, usize, usize)>,
}

impl Embedding {
    fn open(weight_file: &Path, layout: &EmbeddingLayout) -> Result<Self, String> {
        let file = std::fs::File::open(weight_file).map_err(|error| format!("无法打开 {}: {error}", weight_file.display()))?;
        // SAFETY: the compiled model is ours and is not modified while loaded.
        let map = unsafe { memmap2::Mmap::map(&file) }.map_err(|error| format!("无法映射权重文件: {error}"))?;
        let mut blocks = Vec::new();
        let mut first = 0;
        for (rows, metadata) in &layout.blocks {
            let (data, size) = read_record(&map, *metadata)?;
            if size != (rows * layout.hidden * 2) as u64 {
                return Err("词嵌入块大小与模型描述不符".into());
            }
            blocks.push((first, *rows, data as usize));
            first += rows;
        }
        Ok(Self { map, hidden: layout.hidden, blocks })
    }

    fn row(&self, token: u32) -> Result<Vec<u16>, String> {
        let token = token as usize;
        let &(first, _, data) = self
            .blocks
            .iter()
            .find(|(first, rows, _)| (*first..first + rows).contains(&token))
            .ok_or_else(|| format!("词元 {token} 超出词表"))?;
        let start = data + (token - first) * self.hidden * 2;
        let bytes = &self.map[start..start + self.hidden * 2];
        Ok(bytes.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect())
    }
}

/// The weight file inside a compiled model.
pub fn compiled_weights(compiled: &Path) -> std::path::PathBuf {
    compiled.join("weights").join("weight.bin")
}

fn specs_for(config: &Config, shapes: Shapes, range: Range<usize>) -> Vec<StateSpec> {
    let mut out = Vec::new();
    for layer in range {
        match config.layers[layer] {
            LayerKind::Linear => {
                out.push(StateSpec {
                    name: format!("conv_state_{layer}"),
                    shape: vec![1, 1, config.linear_conv_kernel_dim, config.linear_conv_dim()],
                    positional: false,
                });
                out.push(StateSpec {
                    name: format!("ssm_state_{layer}"),
                    shape: vec![1, config.linear_num_value_heads, config.linear_key_head_dim, config.linear_value_head_dim],
                    positional: false,
                });
            }
            LayerKind::Full => {
                for kind in ["k", "v"] {
                    out.push(StateSpec {
                        name: format!("{kind}_cache_{layer}"),
                        shape: vec![1, config.num_key_value_heads, shapes.context, config.head_dim],
                        positional: true,
                    });
                }
            }
        }
    }
    out
}

impl AneBackend {
    /// Loads every function of the compiled package (`.mlmodelc`) for the
    /// Neural Engine. The first load of a new package makes Core ML compile it
    /// for the ANE, which takes a couple of minutes; later loads hit the
    /// system's cache (well under a second). That cache is kept per executable
    /// and the system reclaims it when the disk runs low, so a new build of
    /// the app, or a full disk, compiles again. Embedding lookups read
    /// `weight_file` at `layout`: the compiled model's own
    /// `weights/weight.bin` (see `compiled_weights`), or the package's copy
    /// of it.
    pub fn load(
        compiled: &Path,
        weight_file: &Path,
        layout: &EmbeddingLayout,
        config: Config,
        plan: &PackagePlan,
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<Self, String> {
        let total = plan.parts.len() * 2 + 1;
        let mut done = 0;
        let mut step = |done: &mut usize| {
            *done += 1;
            progress(*done, total);
        };
        let mut prefill = Vec::new();
        let mut decode = Vec::new();
        for i in 0..plan.parts.len() {
            decode.push(Function::load(compiled, &PackagePlan::decode_name(i))?);
            step(&mut done);
        }
        for i in 0..plan.parts.len() {
            prefill.push(Function::load(compiled, &PackagePlan::prefill_name(i))?);
            step(&mut done);
        }
        let head = Function::load(compiled, HEAD)?;
        step(&mut done);

        let embedding = Embedding::open(weight_file, layout)?;
        if embedding.hidden != config.hidden_size {
            return Err("词嵌入宽度与模型配置不符".into());
        }
        let (cos, sin) = rope_tables(&config, plan.shapes.context, f32_to_f16);
        let specs = plan.parts.iter().map(|range| specs_for(&config, plan.shapes, range.clone())).collect();
        let mut backend = Self {
            shapes: plan.shapes,
            prefill,
            decode,
            head,
            specs,
            prefill_states: None,
            decode_states: None,
            lens: vec![0; plan.shapes.slots],
            deltas: vec![0; plan.shapes.slots],
            embedding,
            vision: None,
            cos,
            sin,
            off: f32_to_f16(MASK_OFF),
            config,
        };
        backend.warm_up()?;
        Ok(backend)
    }

    /// Lets requests carry images, encoded by `tower`.
    pub fn with_vision(mut self, tower: VisionTower) -> Self {
        self.vision = Some(tower);
        self
    }

    /// The first prediction of each function pays for loading its program
    /// onto the ANE (about half a second); pay it now, not in a request.
    fn warm_up(&mut self) -> Result<(), String> {
        self.ensure_states();
        let result = self
            .token_rows(&[0], 0)
            .and_then(|rows| self.prefill_chunk(&rows, 0))
            .map(|_| ())
            .and_then(|_| self.decode_step(&[(0, 0)]).map(|_| ()));
        self.trim();
        result
    }

    /// Rows of plain `tokens` from sequence position `start`.
    fn token_rows(&self, tokens: &[u32], start: usize) -> Result<Vec<Row>, String> {
        tokens
            .iter()
            .enumerate()
            .map(|(i, token)| Ok(Row { embedding: self.embedding(*token)?, position: [(start + i) as u32; 3] }))
            .collect()
    }

    fn rotary(&self, position: [u32; 3], cos: &mut [u16], sin: &mut [u16]) {
        self.config.rotary_row(&self.cos, position, cos);
        self.config.rotary_row(&self.sin, position, sin);
    }

    fn ensure_states(&mut self) {
        if self.prefill_states.is_none() {
            self.prefill_states = Some(self.prefill.iter().map(Function::new_state).collect());
        }
        if self.decode_states.is_none() {
            self.decode_states = Some(self.decode.iter().map(Function::new_state).collect());
        }
    }

    fn embedding(&self, token: u32) -> Result<Vec<u16>, String> {
        self.embedding.row(token)
    }

    /// Runs `rows` (at most one chunk) at sequence positions `start..`
    /// through the prefill functions; returns the final hidden states
    /// `[1, H, 1, T]`.
    fn prefill_chunk(&mut self, rows: &[Row], start: usize) -> Result<Tensor16, String> {
        let (t, ctx, h, rot) = (self.shapes.chunk, self.shapes.context, self.config.hidden_size, self.config.rotary_dim);
        let kc = self.config.linear_conv_kernel_dim;
        let n = rows.len();
        assert!(n >= 1 && n <= t && start + n <= ctx);
        let one = f32_to_f16(1.0);
        let mut x = Tensor16::zeros(&[1, h, 1, t]);
        let mut cos = Tensor16::zeros(&[1, 1, t, rot]);
        let mut sin = Tensor16::zeros(&[1, 1, t, rot]);
        let mut scatter = Tensor16::zeros(&[1, 1, ctx, t]);
        let mut keep = Tensor16::filled(&[1, 1, ctx, 1], one);
        let mut mask = Tensor16::filled(&[1, 1, t, ctx], self.off);
        let mut valid = Tensor16::zeros(&[1, 1, 1, t]);
        let mut csel = Tensor16::zeros(&[1, 1, kc, t + kc - 1]);
        for (j, row) in rows.iter().enumerate() {
            for (c, value) in row.embedding.iter().enumerate() {
                x.data[c * t + j] = *value;
            }
            let pos = start + j;
            self.rotary(row.position, &mut cos.data[j * rot..(j + 1) * rot], &mut sin.data[j * rot..(j + 1) * rot]);
            scatter.data[pos * t + j] = one;
            keep.data[pos] = 0;
            valid.data[j] = one;
        }
        for j in 0..t {
            let last = start + j.min(n - 1);
            mask.data[j * ctx..j * ctx + last + 1].fill(0);
        }
        for r in 0..kc {
            csel.data[r * (t + kc - 1) + n - 1 + r] = one;
        }
        let states = self.prefill_states.as_ref().expect("states");
        let mut hidden = x;
        for (part, function) in self.prefill.iter().enumerate() {
            hidden = function.predict(
                &[
                    ("x", &hidden),
                    ("cos", &cos),
                    ("sin", &sin),
                    ("scatter", &scatter),
                    ("keep", &keep),
                    ("mask", &mask),
                    ("valid", &valid),
                    ("csel", &csel),
                ],
                Some(&states[part]),
                "hidden",
            )?;
        }
        Ok(hidden)
    }

    /// Runs prefill over `rows` from sequence position `start`; returns the
    /// last one's final hidden state.
    fn prefill_all(&mut self, rows: &[Row], start: usize) -> Result<Vec<u16>, String> {
        let t = self.shapes.chunk;
        let h = self.config.hidden_size;
        let mut last = vec![0u16; h];
        for (i, chunk) in rows.chunks(t).enumerate() {
            let out = self.prefill_chunk(chunk, start + i * t)?;
            let j = chunk.len() - 1;
            for c in 0..h {
                last[c] = out.data[c * t + j];
            }
        }
        Ok(last)
    }

    fn clear_prefill(&self) {
        let states = self.prefill_states.as_ref().expect("states");
        for (part, specs) in self.specs.iter().enumerate() {
            for spec in specs.iter().filter(|s| !s.positional) {
                states[part].clear(&spec.name, 0);
            }
        }
    }

    fn head_logits(&self, rows: &[(usize, Vec<u16>)]) -> Result<Vec<Logits>, String> {
        let (b, h) = (self.shapes.slots, self.config.hidden_size);
        let mut x = Tensor16::zeros(&[b, h, 1, 1]);
        for (row, hidden) in rows {
            x.data[row * h..(row + 1) * h].copy_from_slice(hidden);
        }
        let logits = self.head.predict(&[("x", &x)], None, "logits")?;
        let vocab = logits.shape[3];
        Ok(rows
            .iter()
            .map(|(row, _)| logits.data[row * vocab..(row + 1) * vocab].iter().map(|v| f16_to_f32(*v)).collect())
            .collect())
    }

    fn decode_step(&mut self, batch: &[(usize, u32)]) -> Result<Vec<Logits>, String> {
        let (b, h, ctx, rot) = (self.shapes.slots, self.config.hidden_size, self.shapes.context, self.config.rotary_dim);
        let one = f32_to_f16(1.0);
        let mut x = Tensor16::zeros(&[b, h, 1, 1]);
        let mut cos = Tensor16::zeros(&[b, 1, 1, rot]);
        let mut sin = Tensor16::zeros(&[b, 1, 1, rot]);
        let mut onehot = Tensor16::zeros(&[b, 1, ctx, 1]);
        let mut keep = Tensor16::filled(&[b, 1, ctx, 1], one);
        let mut mask = Tensor16::filled(&[b, 1, 1, ctx], self.off);
        for row in 0..b {
            mask.data[row * ctx] = 0; // idle rows attend somewhere, never to nothing
        }
        for (slot, token) in batch {
            let pos = self.lens[*slot];
            if pos >= ctx {
                return Err("本地模型上下文已满".into());
            }
            let e = self.embedding(*token)?;
            x.data[slot * h..(slot + 1) * h].copy_from_slice(&e);
            let rotary = (pos as i64 + self.deltas[*slot]) as u32;
            let (c, s) = (&mut cos.data[slot * rot..(slot + 1) * rot], &mut sin.data[slot * rot..(slot + 1) * rot]);
            self.config.rotary_row(&self.cos, [rotary; 3], c);
            self.config.rotary_row(&self.sin, [rotary; 3], s);
            onehot.data[slot * ctx + pos] = one;
            keep.data[slot * ctx + pos] = 0;
            mask.data[slot * ctx..slot * ctx + pos + 1].fill(0);
        }
        let states = self.decode_states.as_ref().expect("states");
        let mut hidden = x;
        for (part, function) in self.decode.iter().enumerate() {
            hidden = function.predict(
                &[("x", &hidden), ("cos", &cos), ("sin", &sin), ("onehot", &onehot), ("keep", &keep), ("mask", &mask)],
                Some(&states[part]),
                "hidden",
            )?;
        }
        let rows: Vec<(usize, Vec<u16>)> =
            batch.iter().map(|(slot, _)| (*slot, hidden.data[slot * h..(slot + 1) * h].to_vec())).collect();
        for (slot, _) in batch {
            self.lens[*slot] += 1;
        }
        self.head_logits(&rows)
    }
}

fn push_u16s(out: &mut Vec<u8>, values: &[u16]) {
    out.reserve(values.len() * 2);
    for v in values {
        out.extend_from_slice(&v.to_le_bytes());
    }
}

impl Backend for AneBackend {
    fn device(&self) -> String {
        "Apple Neural Engine".into()
    }

    fn capacity(&self) -> Capacity {
        Capacity { slots: self.shapes.slots, context: self.shapes.context }
    }

    fn state_format(&self) -> String {
        format!("coreml/{GRAPH_VERSION}/{}x{}x{}", self.shapes.slots, self.shapes.chunk, self.shapes.context)
    }

    fn prefix_state(&mut self, tokens: &[u32]) -> Result<PrefixState, String> {
        if tokens.is_empty() || tokens.len() >= self.shapes.context {
            return Err("前置提示词长度超出本地模型上下文".into());
        }
        self.ensure_states();
        self.clear_prefill();
        let rows = self.token_rows(tokens, 0)?;
        self.prefill_all(&rows, 0)?;
        let states = self.prefill_states.as_ref().expect("states");
        let mut bytes = Vec::new();
        for (part, specs) in self.specs.iter().enumerate() {
            for spec in specs {
                let rows = if spec.positional { tokens.len() } else { spec.shape[2] };
                let block = states[part].read_block(&spec.name, 0, rows);
                push_u16s(&mut bytes, &block.data);
            }
        }
        Ok(PrefixState { tokens: tokens.len(), format: self.state_format(), bytes: bytes.into() })
    }

    fn admit(&mut self, slot: usize, prefix: &PrefixState, input: &[Segment]) -> Result<Logits, String> {
        if prefix.format != self.state_format() {
            return Err("前置提示词缓存与模型不匹配".into());
        }
        let layout = Layout::new(input, prefix.tokens, self.vision.as_ref())?;
        if layout.inputs.is_empty() || prefix.tokens + layout.inputs.len() >= self.shapes.context {
            return Err("请求长度超出本地模型上下文".into());
        }
        let rows = layout
            .inputs
            .iter()
            .zip(&layout.positions)
            .map(|(input, position)| {
                let embedding = match input {
                    Input::Token(token) => self.embedding(*token)?,
                    Input::Feature(row) => layout.feature(*row).iter().map(|v| f32_to_f16(*v)).collect(),
                };
                if embedding.len() != self.config.hidden_size {
                    return Err("图片特征宽度与模型不符".to_string());
                }
                Ok(Row { embedding, position: *position })
            })
            .collect::<Result<Vec<_>, String>>()?;
        self.ensure_states();
        // Restore the prefix into the prefill states.
        {
            let states = self.prefill_states.as_ref().expect("states");
            let mut cursor = 0usize;
            for (part, specs) in self.specs.iter().enumerate() {
                for spec in specs {
                    let rows = if spec.positional { prefix.tokens } else { spec.shape[2] };
                    let count = spec.shape[1] * rows * spec.shape[3];
                    let end = cursor + count * 2;
                    let raw = prefix.bytes.get(cursor..end).ok_or("前置提示词缓存已损坏")?;
                    let data = raw.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
                    states[part].write_block(&spec.name, 0, &Tensor16 { shape: vec![1, spec.shape[1], rows, spec.shape[3]], data });
                    cursor = end;
                }
            }
            if cursor != prefix.bytes.len() {
                return Err("前置提示词缓存已损坏".into());
            }
        }
        let last = self.prefill_all(&rows, prefix.tokens)?;
        let len = prefix.tokens + rows.len();
        // Move the sequence into its decode slot.
        {
            let prefill = self.prefill_states.as_ref().expect("states");
            let decode = self.decode_states.as_ref().expect("states");
            for (part, specs) in self.specs.iter().enumerate() {
                for spec in specs {
                    let rows = if spec.positional { len } else { spec.shape[2] };
                    let block = prefill[part].read_block(&spec.name, 0, rows);
                    decode[part].write_block(&spec.name, slot, &block);
                }
            }
        }
        self.lens[slot] = len;
        self.deltas[slot] = layout.delta(prefix.tokens);
        Ok(self.head_logits(&[(slot, last)])?.remove(0))
    }

    fn step(&mut self, batch: &[(usize, u32)]) -> Result<Vec<Logits>, String> {
        self.ensure_states();
        self.decode_step(batch)
    }

    fn release(&mut self, slot: usize) {
        self.lens[slot] = 0;
        self.deltas[slot] = 0;
    }

    fn trim(&mut self) {
        self.prefill_states = None;
        self.decode_states = None;
        self.lens.iter_mut().for_each(|len| *len = 0);
        self.deltas.iter_mut().for_each(|delta| *delta = 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coreml::package::write_package;
    use crate::engine::{greedy, prefix_tokens, suffix_tokens, Specials};
    use crate::qwen35::model_tensor;
    use crate::safetensors::SafeTensors;
    use crate::tokenizer::Tokenizer;
    use crate::vision::VisionTower;
    use std::sync::atomic::AtomicBool;

    /// Builds, compiles and runs the real model on the Neural Engine. Needs
    /// `MEWORK_LOCAL_MODEL_DIR` (the official release) and a few minutes for
    /// the first ANE compile.
    #[test]
    fn generates_on_the_neural_engine() {
        let Ok(dir) = std::env::var("MEWORK_LOCAL_MODEL_DIR") else {
            eprintln!("MEWORK_LOCAL_MODEL_DIR not set; skipping");
            return;
        };
        let dir = Path::new(&dir);
        let config = Config::load(&dir.join("config.json")).unwrap();
        let weights = SafeTensors::open(&dir.join("model.safetensors")).unwrap();
        let shapes = Shapes { slots: 4, chunk: 16, context: 1024 };
        let plan = PackagePlan::standard(&config, shapes);
        let work = std::env::var("MEWORK_LOCAL_MODEL_WORK").map(std::path::PathBuf::from).unwrap_or_else(|_| std::env::temp_dir());
        let package = work.join("test-qwen35.mlpackage");
        let compiled = work.join("test-qwen35.mlmodelc");
        let layout_path = work.join("test-qwen35.embedding.json");
        if !compiled.exists() || !layout_path.exists() {
            let layout = write_package(&config, &weights, &plan, &package, &AtomicBool::new(false), &mut |_| {}).unwrap();
            layout.save(&layout_path).unwrap();
            let _ = std::fs::remove_dir_all(&compiled);
            crate::coreml::runtime::compile(&package, &compiled).unwrap();
        }
        let layout = EmbeddingLayout::load(&layout_path).unwrap();
        let start = std::time::Instant::now();
        let mut backend =
            AneBackend::load(&compiled, &compiled_weights(&compiled), &layout, config, &plan, &mut |_, _| {}).unwrap();
        eprintln!("loaded in {:?}", start.elapsed());
        // Lookups come from the output projection's blocks; they are the table.
        let table = weights.tensor(&model_tensor("embed_tokens.weight")).unwrap();
        let hidden = backend.config.hidden_size;
        for token in [0u32, 15519, 15520, 151643, 248319] {
            let row: Vec<u16> =
                (0..hidden).map(|i| f32_to_f16(table.get_f32(token as usize * hidden + i))).collect();
            assert_eq!(backend.embedding(token).unwrap(), row, "embedding row {token}");
        }
        assert!(backend.embedding(248320).is_err());
        let tokenizer = Tokenizer::from_file(&dir.join("tokenizer.json")).unwrap();
        let specials = Specials::from_tokenizer(&tokenizer).unwrap();
        let prefix = prefix_tokens(&tokenizer, &specials, "You are a helpful assistant. Reply briefly.");
        let state = backend.prefix_state(&prefix).unwrap();
        let prompts = ["What is the capital of France?", "用一句话介绍你自己"];
        let mut alone = Vec::new();
        for prompt in prompts {
            let suffix = suffix_tokens(&tokenizer, &specials, prompt, 256);
            let mut token = greedy(&backend.admit_tokens(0, &state, &suffix).unwrap(), &specials);
            let mut out = vec![token];
            for _ in 0..14 {
                token = greedy(&backend.step(&[(0, token)]).unwrap()[0], &specials);
                out.push(token);
            }
            backend.release(0);
            eprintln!("{prompt:?} -> {:?}", tokenizer.decode(&out));
            alone.push(out);
        }
        // Both at once, in slots 1 and 3.
        let s1 = suffix_tokens(&tokenizer, &specials, prompts[0], 256);
        let s3 = suffix_tokens(&tokenizer, &specials, prompts[1], 256);
        let mut t1 = greedy(&backend.admit_tokens(1, &state, &s1).unwrap(), &specials);
        let mut t3 = greedy(&backend.admit_tokens(3, &state, &s3).unwrap(), &specials);
        let (mut o1, mut o3) = (vec![t1], vec![t3]);
        let started = std::time::Instant::now();
        for _ in 0..14 {
            let logits = backend.step(&[(1, t1), (3, t3)]).unwrap();
            t1 = greedy(&logits[0], &specials);
            t3 = greedy(&logits[1], &specials);
            o1.push(t1);
            o3.push(t3);
        }
        eprintln!("14 batched steps in {:?}", started.elapsed());
        assert_eq!(o1, alone[0]);
        assert_eq!(o3, alone[1]);
        backend.trim();

        // An image (`testdata/vision-golden.json`) in slot 2, beside a text
        // request in slot 0.
        let golden = crate::vision::tests::golden_request(&tokenizer);
        let margins: Vec<f64> =
            serde_json::from_value(crate::vision::tests::golden_json()["margins"].clone()).unwrap();
        let vision = crate::vision::VisionConfig::load(&dir.join("config.json")).unwrap();
        let tower = VisionTower::open(vision, &dir.join("model.safetensors")).unwrap();
        let mut backend = backend.with_vision(tower);
        let state = backend.prefix_state(&golden.prefix).unwrap();
        let started = std::time::Instant::now();
        let mut image = vec![greedy(&backend.admit(2, &state, &golden.input).unwrap(), &specials)];
        eprintln!("image admitted in {:?}", started.elapsed());
        let text = suffix_tokens(&tokenizer, &specials, prompts[0], 256);
        let mut other = greedy(&backend.admit_tokens(0, &state, &text).unwrap(), &specials);
        while image.len() < golden.expected.len() {
            let logits = backend.step(&[(0, other), (2, *image.last().unwrap())]).unwrap();
            other = greedy(&logits[0], &specials);
            image.push(greedy(&logits[1], &specials));
        }
        eprintln!("image -> {:?}", tokenizer.decode(&image));
        if let Some(k) = (0..image.len()).find(|k| image[*k] != golden.expected[*k]) {
            eprintln!("image: matches transformers up to token {k} (its margin {:.3})", margins[k]);
            assert!(margins[k] < 0.5, "image: differs from transformers at token {k} (margin {})", margins[k]);
        } else {
            eprintln!("image: matches transformers ({} tokens)", image.len());
        }
        backend.trim();
    }
}
