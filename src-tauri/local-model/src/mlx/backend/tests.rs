//! The MLX backend against transformers (`testdata/llama-greedy.json`: greedy
//! continuations of the float32 official checkpoint, with the top-two logit
//! margin at every step). Needs `MEWORK_LOCAL_MODEL_DIR` (the official
//! release); the converted weights go to `MEWORK_LOCAL_MODEL_WORK` (or the
//! temp dir) and are reused.
//!
//! `cargo test --release --features mlx mlx:: -- --nocapture`

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use serde::Deserialize;

use super::*;
use crate::engine::{greedy, prefix_tokens, suffix_tokens, Specials};
use crate::mlx::weights::convert;
use crate::safetensors::SafeTensors;
use crate::tokenizer::Tokenizer;

/// Transformers' top-two margin below which float16 may pick the other token.
const NEAR_TIE: f64 = 0.25;
/// Our own margin below which batching may flip a token.
const BATCH_NOISE: f32 = 0.05;

#[derive(Deserialize)]
struct Reference {
    system: String,
    max_new: usize,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    request: String,
    expected: Vec<u32>,
    margins: Vec<f64>,
}

fn top_margin(logits: &[f32]) -> f32 {
    let (mut first, mut second) = (f32::NEG_INFINITY, f32::NEG_INFINITY);
    for value in logits {
        if *value > first {
            second = first;
            first = *value;
        } else if *value > second {
            second = *value;
        }
    }
    first - second
}

fn ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

/// The converted build, with the kernels from beside the built library.
fn build_dir(official: &std::path::Path) -> PathBuf {
    let work = std::env::var_os("MEWORK_LOCAL_MODEL_WORK").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = work.join("test-qwen35-mlx");
    if !dir.join(INDEX_FILE).exists() {
        let config = Config::load(&official.join("config.json")).unwrap();
        let checkpoint = SafeTensors::open(&official.join("model.safetensors")).unwrap();
        let start = Instant::now();
        convert(&config, &checkpoint, &dir, &AtomicBool::new(false), &mut |_, _| {}).unwrap();
        eprintln!("converted in {:.1} s", start.elapsed().as_secs_f64());
    }
    std::fs::copy(official.join("config.json"), dir.join("config.json")).unwrap();
    let metallib = shim_path().unwrap().with_file_name(METALLIB_FILE);
    let _ = std::fs::remove_file(dir.join(METALLIB_FILE));
    std::os::unix::fs::symlink(metallib, dir.join(METALLIB_FILE)).unwrap();
    dir
}

#[derive(Clone, Default)]
struct Generated {
    tokens: Vec<u32>,
    margins: Vec<f32>,
}

fn generate(
    backend: &mut MlxBackend,
    specials: &Specials,
    prefix: &PrefixState,
    requests: &[(usize, &[u32])],
    max_new: usize,
) -> Vec<Generated> {
    let mut out = vec![Generated::default(); requests.len()];
    let mut live = vec![false; requests.len()];
    let accept = |backend: &mut MlxBackend, out: &mut Generated, live: &mut bool, slot: usize, logits: &[f32]| {
        let token = greedy(logits, specials);
        out.tokens.push(token);
        out.margins.push(top_margin(logits));
        *live = !(specials.is_stop(token) || out.tokens.len() >= max_new);
        if !*live {
            backend.release(slot);
        }
    };
    for (i, (slot, tokens)) in requests.iter().enumerate() {
        let logits = backend.admit(*slot, prefix, tokens).unwrap();
        accept(backend, &mut out[i], &mut live[i], *slot, &logits);
    }
    loop {
        let index: Vec<usize> = (0..requests.len()).filter(|i| live[*i]).collect();
        if index.is_empty() {
            break;
        }
        let batch: Vec<(usize, u32)> = index.iter().map(|i| (requests[*i].0, *out[*i].tokens.last().unwrap())).collect();
        let all = backend.step(&batch).unwrap();
        for (i, logits) in index.iter().zip(all) {
            accept(backend, &mut out[*i], &mut live[*i], requests[*i].0, &logits);
        }
    }
    out
}

fn divergence(a: &[u32], b: &[u32]) -> Option<usize> {
    (0..a.len().max(b.len())).find(|i| a.get(*i) != b.get(*i))
}

#[test]
fn generates_like_transformers() {
    let Some(official) = std::env::var_os("MEWORK_LOCAL_MODEL_DIR").map(PathBuf::from) else {
        eprintln!("跳过：未设置 MEWORK_LOCAL_MODEL_DIR（官方模型目录）");
        return;
    };
    let reference: Reference = serde_json::from_str(include_str!("../../../testdata/llama-greedy.json")).unwrap();
    let tokenizer = Tokenizer::from_file(&official.join("tokenizer.json")).unwrap();
    let specials = Specials::from_tokenizer(&tokenizer).unwrap();
    let dir = build_dir(&official);

    let start = Instant::now();
    let mut backend = MlxBackend::load(&dir, 4, 1024).unwrap();
    eprintln!("[mlx] {} · load {:.0} ms", backend.device(), ms(start));

    let prefix_ids = prefix_tokens(&tokenizer, &specials, &reference.system);
    let start = Instant::now();
    let prefix = backend.prefix_state(&prefix_ids).unwrap();
    eprintln!(
        "[mlx] prefix_state {} tokens: {:.1} ms, {:.1} MiB",
        prefix.tokens,
        ms(start),
        prefix.bytes.len() as f64 / (1 << 20) as f64
    );
    let requests: Vec<Vec<u32>> =
        reference.cases.iter().map(|case| suffix_tokens(&tokenizer, &specials, &case.request, 512)).collect();

    let alone: Vec<Generated> = requests
        .iter()
        .map(|request| generate(&mut backend, &specials, &prefix, &[(0, request)], reference.max_new).remove(0))
        .collect();
    let together =
        generate(&mut backend, &specials, &prefix, &[(3, &requests[0]), (1, &requests[1])], reference.max_new);

    for (i, case) in reference.cases.iter().enumerate() {
        let (a, b) = (&alone[i], &together[i]);
        eprintln!("[mlx] case {i} {:?} -> {:?}", case.request, tokenizer.decode(&a.tokens));
        if let Some(k) = divergence(&a.tokens, &b.tokens) {
            let margin = a.margins.get(k).copied().unwrap_or(f32::INFINITY);
            assert!(margin < BATCH_NOISE, "case {i}: batching changed token {k} (margin {margin})");
        }
        match divergence(&a.tokens, &case.expected) {
            None => eprintln!("[mlx] case {i}: matches transformers ({} tokens)", case.expected.len()),
            Some(k) => {
                let margin = case.margins.get(k).copied().unwrap_or(f64::INFINITY);
                assert!(margin < NEAR_TIE, "case {i}: differs from transformers at token {k} (its margin {margin})");
                eprintln!("[mlx] case {i}: matches transformers up to token {k}, a near tie (margin {margin:.3})");
            }
        }
    }

    // Step cost with one live slot (three others admitted and waiting), then all four.
    let admit_all = |backend: &mut MlxBackend| -> Vec<u32> {
        (0..4).map(|slot| greedy(&backend.admit(slot, &prefix, &requests[slot % 2]).unwrap(), &specials)).collect()
    };
    let timed = |backend: &mut MlxBackend, first: &[u32], live: usize| {
        let mut tokens = first.to_vec();
        let mut total = 0.0;
        for step in 0..10 {
            let batch: Vec<(usize, u32)> = (0..live).map(|slot| (slot, tokens[slot])).collect();
            let start = Instant::now();
            let all = backend.step(&batch).unwrap();
            if step >= 2 {
                total += ms(start);
            }
            for (slot, logits) in all.into_iter().enumerate() {
                tokens[slot] = greedy(&logits, &specials);
            }
        }
        eprintln!("[mlx] step with {live} slot(s): {:.1} ms", total / 8.0);
    };
    let first = admit_all(&mut backend);
    timed(&mut backend, &first, 1);
    // Slot 1 sat those steps out and must be where it was.
    let resumed = backend.step(&[(1, first[1])]).unwrap();
    let fresh = generate(&mut backend, &specials, &prefix, &[(2, &requests[1])], 2).remove(0);
    assert_eq!(greedy(&resumed[0], &specials), fresh.tokens[1], "a waiting slot moved");
    for slot in 0..4 {
        backend.release(slot);
    }
    let first = admit_all(&mut backend);
    timed(&mut backend, &first, 4);
    for slot in 0..4 {
        backend.release(slot);
    }

    backend.trim();
    let logits = backend.admit(0, &prefix, &requests[0]).unwrap();
    assert_eq!(greedy(&logits, &specials), alone[0].tokens[0], "admit after trim");
    backend.release(0);
}
