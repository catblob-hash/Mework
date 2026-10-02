//! Runs the real model. Skipped unless `MEWORK_LOCAL_MODEL_GGUF` names the
//! converted GGUF; `MEWORK_LOCAL_MODEL_DIR` must then name the release
//! directory (for `tokenizer.json`), and `MEWORK_LLAMA_RUNTIME_DIR` the
//! unpacked llama.cpp release build.
//!
//! `testdata/llama-greedy.json` holds transformers' greedy continuations
//! (float32, the official checkpoint) of two prompts that share a ~290-token
//! system prompt, with the logit margin between its top two tokens at every
//! step. The GGUF has f16 weights and llama.cpp accumulates differently, so a
//! step where transformers' top two are nearly tied may go the other way; a
//! divergence is accepted only at such a step.
//!
//! `cargo test --features llama llama::tests -- --nocapture --test-threads=1`
//! prints timings; run `cpu` and `gpu` separately for their own peak RSS.

use std::path::PathBuf;
use std::time::Instant;

use serde::Deserialize;

use super::*;
use crate::engine::{greedy, prefix_tokens, suffix_tokens, Specials};
use crate::tokenizer::Tokenizer;

/// Transformers' top-two margin (logits) below which llama.cpp may pick the other token.
const NEAR_TIE: f64 = 0.25;
/// Our own top-two margin below which batching may flip a token (fp noise).
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
    prompt_ids: Vec<u32>,
    prefix_len: usize,
    expected: Vec<u32>,
    margins: Vec<f64>,
}

struct Fixture {
    gguf: PathBuf,
    specials: Specials,
    reference: Reference,
    prefix: Vec<u32>,
    requests: Vec<Vec<u32>>,
}

fn fixture() -> Option<Fixture> {
    let Some(gguf) = std::env::var_os("MEWORK_LOCAL_MODEL_GGUF") else {
        eprintln!("跳过：未设置 MEWORK_LOCAL_MODEL_GGUF（转换后的 GGUF）与 MEWORK_LOCAL_MODEL_DIR（官方模型目录）");
        return None;
    };
    let dir = PathBuf::from(std::env::var_os("MEWORK_LOCAL_MODEL_DIR").expect("MEWORK_LOCAL_MODEL_DIR 未设置"));
    let tokenizer = Tokenizer::from_file(&dir.join("tokenizer.json")).unwrap();
    let specials = Specials::from_tokenizer(&tokenizer).unwrap();
    let reference: Reference =
        serde_json::from_str(include_str!("../../testdata/llama-greedy.json")).expect("llama-greedy.json");
    let prefix = prefix_tokens(&tokenizer, &specials, &reference.system);
    let requests: Vec<Vec<u32>> =
        reference.cases.iter().map(|case| suffix_tokens(&tokenizer, &specials, &case.request, 512)).collect();
    for (case, request) in reference.cases.iter().zip(&requests) {
        // The reference ran on transformers' own tokenization of the whole prompt.
        assert_eq!(case.prefix_len, prefix.len(), "prefix tokens differ from the reference");
        let whole: Vec<u32> = prefix.iter().chain(request).copied().collect();
        assert_eq!(whole, case.prompt_ids, "prompt tokens differ from the reference");
    }
    Some(Fixture { gguf: PathBuf::from(gguf), specials, reference, prefix, requests })
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

#[derive(Clone, Debug, Default, PartialEq)]
struct Generated {
    tokens: Vec<u32>,
    /// Our top-two logit margin at each token.
    margins: Vec<f32>,
}

/// Admits each `(slot, tokens)` after `prefix`, then steps all of them
/// together, greedily, until each stops or has `max_new` tokens.
fn generate(
    backend: &mut LlamaBackend,
    specials: &Specials,
    prefix: &PrefixState,
    requests: &[(usize, &[u32])],
    max_new: usize,
) -> Vec<Generated> {
    let mut out = vec![Generated::default(); requests.len()];
    let mut live = vec![false; requests.len()];
    let accept = |backend: &mut LlamaBackend, out: &mut Generated, live: &mut bool, slot: usize, logits: &[f32]| {
        let token = greedy(logits, specials);
        out.tokens.push(token);
        out.margins.push(top_margin(logits));
        *live = !(specials.is_stop(token) || out.tokens.len() >= max_new);
        if !*live {
            backend.release(slot);
            assert_eq!(
                backend.context.as_ref().unwrap().seq_pos_max(slot as i32),
                -1,
                "release left slot {slot} in memory"
            );
        }
    };
    for (i, (slot, tokens)) in requests.iter().enumerate() {
        let logits = backend.admit_tokens(*slot, prefix, tokens).unwrap();
        assert!(backend.context.as_ref().unwrap().seq_pos_max(*slot as i32) >= 0);
        accept(backend, &mut out[i], &mut live[i], *slot, &logits);
    }
    loop {
        let index: Vec<usize> = (0..requests.len()).filter(|i| live[*i]).collect();
        if index.is_empty() {
            break;
        }
        let batch: Vec<(usize, u32)> =
            index.iter().map(|i| (requests[*i].0, *out[*i].tokens.last().unwrap())).collect();
        let all = backend.step(&batch).unwrap();
        for (i, logits) in index.iter().zip(all) {
            accept(backend, &mut out[*i], &mut live[*i], requests[*i].0, &logits);
        }
    }
    out
}

/// First index where `a` and `b` differ, if they do.
fn divergence(a: &[u32], b: &[u32]) -> Option<usize> {
    (0..a.len().max(b.len())).find(|i| a.get(*i) != b.get(*i))
}

fn peak_rss_mib() -> Option<f64> {
    #[cfg(unix)]
    {
        #[repr(C)]
        struct Rusage {
            utime: [i64; 2],
            stime: [i64; 2],
            maxrss: i64,
            rest: [i64; 13],
        }
        extern "C" {
            fn getrusage(who: i32, usage: *mut Rusage) -> i32;
        }
        let mut usage = Rusage { utime: [0; 2], stime: [0; 2], maxrss: 0, rest: [0; 13] };
        // SAFETY: RUSAGE_SELF into a struct laid out like `struct rusage` on 64-bit Unix.
        if unsafe { getrusage(0, &mut usage) } != 0 {
            return None;
        }
        // Bytes on macOS, KiB on Linux.
        let bytes = if cfg!(target_os = "macos") { usage.maxrss as f64 } else { usage.maxrss as f64 * 1024.0 };
        Some(bytes / MIB as f64)
    }
    #[cfg(not(unix))]
    None
}

fn rss_mib() -> Option<f64> {
    let output =
        std::process::Command::new("ps").args(["-o", "rss=", "-p", &std::process::id().to_string()]).output().ok()?;
    let kib: f64 = String::from_utf8_lossy(&output.stdout).trim().parse().ok()?;
    Some(kib / 1024.0)
}

fn ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

fn exercise(label: &str, options: LlamaOptions) {
    let Some(fixture) = fixture() else { return };
    let specials = &fixture.specials;
    let max_new = fixture.reference.max_new;

    let start = Instant::now();
    let mut backend = LlamaBackend::load(&fixture.gguf, options).unwrap();
    let load_ms = ms(start);
    let capacity = backend.capacity();
    eprintln!(
        "[{label}] device {} · {capacity:?} · weights {:.0} MiB · load {load_ms:.0} ms",
        backend.device(),
        backend.weights_bytes() as f64 / MIB as f64
    );
    let devices: Vec<String> =
        ffi::devices().iter().map(|device| format!("{} ({:?})", describe(device), device.kind)).collect();
    eprintln!("[{label}] ggml devices: {devices:?}");
    assert!(backend.context.is_none(), "load leaves no context behind");
    assert!(capacity.slots >= 2, "the test needs two slots");

    let start = Instant::now();
    let prefix = backend.prefix_state(&fixture.prefix).unwrap();
    eprintln!(
        "[{label}] prefix_state {} tokens: {:.1} ms, {:.1} MiB",
        prefix.tokens,
        ms(start),
        prefix.bytes.len() as f64 / MIB as f64
    );
    assert_eq!(prefix.format, backend.state_format());
    assert_eq!(
        backend.context.as_ref().unwrap().seq_pos_max(backend.scratch_seq()),
        -1,
        "scratch sequence left behind"
    );

    // Each request alone, in slot 0.
    let alone: Vec<Generated> = fixture
        .requests
        .iter()
        .map(|request| generate(&mut backend, specials, &prefix, &[(0, request)], max_new).remove(0))
        .collect();

    let admit_ms: Vec<f64> = fixture
        .requests
        .iter()
        .map(|request| {
            let start = Instant::now();
            backend.admit_tokens(0, &prefix, request).unwrap();
            backend.release(0);
            ms(start)
        })
        .collect();
    eprintln!(
        "[{label}] admit {:?} tokens: {:?} ms",
        fixture.requests.iter().map(Vec::len).collect::<Vec<_>>(),
        admit_ms.iter().map(|ms| (ms * 10.0).round() / 10.0).collect::<Vec<_>>()
    );
    // Both together, in slots 1 and 0 (swapped, so the slot does not matter).
    let together =
        generate(&mut backend, specials, &prefix, &[(1, &fixture.requests[0]), (0, &fixture.requests[1])], max_new);

    for (i, case) in fixture.reference.cases.iter().enumerate() {
        let (a, b) = (&alone[i], &together[i]);
        eprintln!("[{label}] case {i}: alone {:?}", a.tokens);
        eprintln!("[{label}] case {i}: batch {:?}", b.tokens);
        eprintln!("[{label}] case {i}: hf    {:?}", case.expected);
        eprintln!(
            "[{label}] case {i}: our margins {:?}",
            a.margins.iter().map(|m| (m * 100.0).round() / 100.0).collect::<Vec<_>>()
        );
        if let Some(k) = divergence(&a.tokens, &b.tokens) {
            let margin = a.margins.get(k).copied().unwrap_or(f32::INFINITY);
            assert!(margin < BATCH_NOISE, "[{label}] case {i}: batching changed token {k} (margin {margin})");
            eprintln!("[{label}] case {i}: batching flipped a near-tie at token {k} (margin {margin:.3})");
        }
        match divergence(&a.tokens, &case.expected) {
            None => eprintln!("[{label}] case {i}: matches transformers ({} tokens)", case.expected.len()),
            Some(k) => {
                let margin = case.margins.get(k).copied().unwrap_or(f64::INFINITY);
                assert!(
                    margin < NEAR_TIE,
                    "[{label}] case {i}: differs from transformers at token {k}, where its margin is {margin}"
                );
                eprintln!(
                    "[{label}] case {i}: matches transformers up to token {k}, a near tie there (margin {margin:.3})"
                );
            }
        }
    }

    // A prefix computed while a request runs leaves that request alone.
    let mut tokens = vec![greedy(&backend.admit_tokens(0, &prefix, &fixture.requests[0]).unwrap(), specials)];
    while tokens.len() < alone[0].tokens.len() {
        if tokens.len() == 3 {
            let again = backend.prefix_state(&fixture.prefix).unwrap();
            assert_eq!(again.tokens, prefix.tokens);
            assert_eq!(backend.context.as_ref().unwrap().seq_pos_max(backend.scratch_seq()), -1);
        }
        let logits = backend.step(&[(0, *tokens.last().unwrap())]).unwrap();
        tokens.push(greedy(&logits[0], specials));
    }
    backend.release(0);
    assert_eq!(tokens, alone[0].tokens, "prefix_state disturbed a live slot");

    // Step cost with one live slot and with all of them.
    if capacity.slots >= 4 {
        let requests: Vec<&[u32]> = (0..4).map(|i| fixture.requests[i % 2].as_slice()).collect();
        let first: Vec<u32> = requests
            .iter()
            .enumerate()
            .map(|(slot, request)| greedy(&backend.admit_tokens(slot, &prefix, request).unwrap(), specials))
            .collect();
        const STEPS: usize = 16;
        for live in [1usize, 2, 4] {
            let mut tokens = first.clone();
            let mut stepping = 0.0;
            // Two untimed steps first: GPU backends build kernels for a new batch shape.
            for step in 0..STEPS + 2 {
                if step == 2 {
                    stepping = 0.0;
                }
                let batch: Vec<(usize, u32)> = (0..live).map(|slot| (slot, tokens[slot])).collect();
                let start = Instant::now();
                let all = backend.step(&batch).unwrap();
                stepping += ms(start);
                for (slot, logits) in all.into_iter().enumerate() {
                    tokens[slot] = greedy(&logits, specials);
                }
            }
            eprintln!("[{label}] step with {live} slot(s): {:.1} ms", stepping / STEPS as f64);
        }
        for slot in 0..4 {
            backend.release(slot);
        }
    }

    // Trim frees the context; the next admit makes a new one and agrees.
    let before = rss_mib();
    backend.trim();
    assert!(backend.context.is_none(), "trim kept the context");
    let after = rss_mib();
    eprintln!("[{label}] RSS before/after trim: {before:?} / {after:?} MiB");
    let logits = backend.admit_tokens(0, &prefix, &fixture.requests[0]).unwrap();
    assert_eq!(greedy(&logits, specials), alone[0].tokens[0], "admit after trim");
    // A live slot keeps the context through trim.
    backend.trim();
    assert!(backend.context.is_some(), "trim dropped a live sequence");
    backend.release(0);
    backend.trim();
    assert!(backend.context.is_none());
    eprintln!("[{label}] peak RSS {:?} MiB", peak_rss_mib());
}

/// Options for the model tests: the release build unpacked in
/// `MEWORK_LLAMA_RUNTIME_DIR` (`runtime::install`).
fn options() -> LlamaOptions {
    LlamaOptions {
        runtime_dir: std::env::var_os("MEWORK_LLAMA_RUNTIME_DIR").map(PathBuf::from),
        ..LlamaOptions::default()
    }
}

#[test]
fn cpu() {
    exercise("cpu", LlamaOptions { cpu_only: true, ..options() });
}

#[test]
fn gpu() {
    exercise("gpu", options());
}

/// The golden image request (`testdata/vision-golden.json`) through `mtmd`,
/// beside a text request in another slot, against transformers' greedy
/// reply. Needs `MEWORK_LOCAL_MODEL_MMPROJ` (the projector,
/// `gguf::convert_qwen35_mmproj_to_gguf`) besides the variables above.
fn reads_images(label: &str, options: LlamaOptions) {
    let Some(fixture) = fixture() else { return };
    let Some(mmproj) = std::env::var_os("MEWORK_LOCAL_MODEL_MMPROJ").map(PathBuf::from) else {
        eprintln!("跳过：未设置 MEWORK_LOCAL_MODEL_MMPROJ（视觉投影）");
        return;
    };
    let dir = PathBuf::from(std::env::var_os("MEWORK_LOCAL_MODEL_DIR").unwrap());
    let tokenizer = Tokenizer::from_file(&dir.join("tokenizer.json")).unwrap();
    let golden = crate::vision::tests::golden_request(&tokenizer);
    let margins: Vec<f64> = serde_json::from_value(crate::vision::tests::golden_json()["margins"].clone()).unwrap();
    let specials = &fixture.specials;
    let options = LlamaOptions { projector: Some(mmproj), ..options };
    let mut backend = LlamaBackend::load(&fixture.gguf, options).unwrap();
    let prefix = backend.prefix_state(&golden.prefix).unwrap();
    let text = &fixture.requests[0];

    let start = Instant::now();
    let mut image = vec![greedy(&backend.admit(1, &prefix, &golden.input).unwrap(), specials)];
    eprintln!("[{label}] image admit (projector load included) {:.0} ms", ms(start));
    // The text after the image continued at the shifted rotary position
    // (transformers' `rope_deltas` is negative after an image).
    let delta = crate::vision::tests::golden_json()["rope_delta"].as_i64().unwrap();
    let sequence = golden.prefix.len() + golden.input.iter().map(|segment| segment.len(32)).sum::<usize>();
    assert_eq!(backend.next_position[1], Some((sequence as i64 + delta) as usize));
    let mut other = greedy(&backend.admit_tokens(0, &prefix, text).unwrap(), specials);
    while image.len() < golden.expected.len() {
        let all = backend.step(&[(0, other), (1, *image.last().unwrap())]).unwrap();
        other = greedy(&all[0], specials);
        image.push(greedy(&all[1], specials));
    }
    eprintln!("[{label}] image -> {:?}", tokenizer.decode(&image));
    match divergence(&image, &golden.expected) {
        None => eprintln!("[{label}] image: matches transformers ({} tokens)", image.len()),
        Some(k) => {
            assert!(margins[k] < 0.5, "image: differs from transformers at token {k} (its margin {})", margins[k]);
            eprintln!("[{label}] image: matches transformers up to token {k}, a near tie ({:.3})", margins[k]);
        }
    }
    // A second image reuses the loaded projector.
    backend.release(1);
    let start = Instant::now();
    let again = greedy(&backend.admit(1, &prefix, &golden.input).unwrap(), specials);
    eprintln!("[{label}] image admit again {:.0} ms", ms(start));
    assert_eq!(again, image[0]);
    backend.release(0);
    backend.release(1);
}

#[test]
fn cpu_images() {
    reads_images("cpu", LlamaOptions { cpu_only: true, ..options() });
}

#[test]
fn gpu_images() {
    reads_images("gpu", options());
}

/// Qwen3.5-0.8B's shape, as `Shape::read` finds it in the GGUF.
fn qwen35_08b() -> Shape {
    Shape {
        n_vocab: 248_320,
        n_head: 8,
        kv_bytes_per_position: 6 * 2 * (256 + 256) * 2,
        recurrent_bytes: 18 * (3 * 6144 + 128 * 2048) * 4,
    }
}

const GIB: u64 = 1 << 30;
const WEIGHTS: u64 = 1_506_000_000;

fn machine(devices: Vec<ffi::Device>, host_available: u64) -> Machine {
    let cpu = ffi::Device::fake("CPU", "Some CPU", ffi::DeviceKind::Cpu, 16 * GIB, true);
    Machine { devices: std::iter::once(cpu).chain(devices).collect(), host_available, threads: 8 }
}

#[test]
fn sequence_cost_matches_llama_cpp() {
    // llama.cpp reports 60 MiB of KV and 96.33 MiB of recurrent state for 5 × 1024.
    let shape = qwen35_08b();
    assert_eq!(5 * shape.kv_bytes_per_position * 1024, 60 * MIB);
    assert_eq!((5 * shape.recurrent_bytes) as f64 / MIB as f64, 96.328125);
}

#[test]
fn places_the_model() {
    let options = LlamaOptions::default();
    let shape = qwen35_08b();
    let place = |devices: Vec<ffi::Device>, host: u64, options: &LlamaOptions| {
        let plan = plan(options, &machine(devices, host), &shape, WEIGHTS, 1024);
        (plan.description, plan.slots, plan.devices.len(), plan.auto_load_mode)
    };
    let rtx = || ffi::Device::fake("Vulkan", "NVIDIA GeForce RTX 4070", ffi::DeviceKind::Gpu, 11 * GIB, false);
    let small = || ffi::Device::fake("Vulkan", "NVIDIA GeForce GT 710", ffi::DeviceKind::Gpu, 1536 * MIB, false);
    let iris =
        || ffi::Device::fake("Vulkan", "Intel(R) Iris(R) Xe Graphics", ffi::DeviceKind::IntegratedGpu, 8 * GIB, false);
    let metal = || ffi::Device::fake("MTL", "Apple M4", ffi::DeviceKind::Gpu, 12 * GIB, true);

    assert_eq!(place(vec![rtx()], 8 * GIB, &options), ("Vulkan · NVIDIA GeForce RTX 4070".into(), 4, 1, false));
    // The larger discrete GPU wins; one without room for the weights is skipped.
    assert_eq!(place(vec![small(), rtx()], 8 * GIB, &options).0, "Vulkan · NVIDIA GeForce RTX 4070");
    assert_eq!(place(vec![small()], 8 * GIB, &options), ("CPU · 8 threads".into(), 4, 0, false));
    // Discrete before integrated; unified memory the GPU maps is used in place.
    assert_eq!(place(vec![iris(), rtx()], 8 * GIB, &options).0, "Vulkan · NVIDIA GeForce RTX 4070");
    assert_eq!(place(vec![metal()], 8 * GIB, &options), ("Metal · Apple M4".into(), 4, 1, false));
    // An iGPU that would need its own copy of the weights only on request.
    assert_eq!(place(vec![iris()], 8 * GIB, &options).0, "CPU · 8 threads");
    let resident = LlamaOptions { igpu_resident_weights: true, ..LlamaOptions::default() };
    assert_eq!(place(vec![iris()], 8 * GIB, &resident), ("Vulkan · Intel(R) Iris(R) Xe Graphics".into(), 4, 1, true));
    assert_eq!(place(vec![iris()], 2 * GIB, &resident).0, "CPU · 8 threads");
    let cpu_only = LlamaOptions { cpu_only: true, ..LlamaOptions::default() };
    assert_eq!(place(vec![rtx(), metal()], 8 * GIB, &cpu_only).0, "CPU · 8 threads");
}

#[test]
fn slots_follow_memory() {
    let shape = qwen35_08b();
    let slots = |host: u64, max_slots: usize| {
        let options = LlamaOptions { max_slots, ..LlamaOptions::default() };
        plan(&options, &machine(Vec::new(), host), &shape, WEIGHTS, 1024).slots
    };
    assert_eq!(slots(16 * GIB, 4), 4);
    assert_eq!(slots(16 * GIB, 2), 2);
    assert_eq!(slots(16 * GIB, 64), 64);
    assert_eq!(slots(1 << 40, 1000), MAX_SLOTS);
    // Per slot: 31.3 MiB of state + 1 MiB of logits; 112 MiB of compute buffers.
    assert_eq!(slots(1000 * MIB, 4), 3);
    assert_eq!(slots(800 * MIB, 4), 1);
    // Never zero, never unbounded.
    assert_eq!(slots(64 * MIB, 4), 1);
    assert_eq!(slots(1 << 40, 0), 1);
    // A GPU with room for the weights and two sequences gets two.
    let tight =
        ffi::Device::fake("CUDA", "NVIDIA GeForce MX450", ffi::DeviceKind::Gpu, WEIGHTS + 256 * MIB + 215 * MIB, false);
    let plan = plan(&LlamaOptions::default(), &machine(vec![tight], 16 * GIB), &shape, WEIGHTS, 1024);
    assert_eq!((plan.description.as_str(), plan.slots), ("CUDA · NVIDIA GeForce MX450", 2));
}

#[test]
fn rejects_mismatched_prefix_and_bad_slots() {
    let Some(fixture) = fixture() else { return };
    let mut backend =
        LlamaBackend::load(&fixture.gguf, LlamaOptions { cpu_only: true, max_slots: 2, ..options() }).unwrap();
    let mut prefix = backend.prefix_state(&fixture.prefix[..8]).unwrap();
    assert!(backend.admit_tokens(2, &prefix, &fixture.requests[0]).is_err());
    assert!(backend.step(&[(0, 1)]).is_err(), "step on a slot that was never admitted");
    prefix.format.push('x');
    assert!(backend.admit_tokens(0, &prefix, &fixture.requests[0]).is_err());
    let too_long = vec![fixture.requests[0][0]; backend.capacity().context + 1];
    assert!(backend.prefix_state(&too_long).is_err());
}
