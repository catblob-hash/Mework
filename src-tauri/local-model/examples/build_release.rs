//! Builds the files the app downloads, one build per inference backend, from
//! the official Qwen3.5-0.8B release, laid out as they are published:
//!
//! ```text
//! <out>/ane/<GRAPH_VERSION>/     config.json, tokenizer.json, embedding.json, model.mlpackage/…
//! <out>/vision/<VISION_VERSION>/ vision.safetensors (the vision tower, for both Apple builds)
//! <out>/mlx/<FORMAT>/            config.json, tokenizer.json, weights.json, weights.bin
//! <out>/mlx/runtime/<MLX>/       mlx.metallib (the kernels of the MLX the app links)
//! <out>/llama/<GGUF_VERSION>/    config.json, tokenizer.json, qwen3.5-0.8b-f16.gguf
//! <out>/llama/<MMPROJ_VERSION>/  mmproj-qwen3.5-0.8b-f16.gguf (the vision projector)
//! ```
//!
//! and the catalog the app pins them with (paths, sizes, SHA-256), which goes
//! to `src-tauri/src/helper_model/catalog.json`. The output is deterministic,
//! so anyone can rebuild and compare before uploading.
//!
//! `cargo run --release -p mework-local-model --features mlx --example build_release -- <official dir> <out dir> [catalog.json]`

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use serde::Serialize;
use sha2::{Digest, Sha256};

use local_model::coreml::graph::Shapes;
use local_model::coreml::package::{write_package, PackagePlan, EMBEDDING_FILE, GRAPH_VERSION};
use local_model::gguf::{convert_qwen35_mmproj_to_gguf, convert_qwen35_to_gguf, GGUF_FILE, GGUF_VERSION, MMPROJ_FILE, MMPROJ_VERSION};
use local_model::mlx::weights::{convert, FORMAT, INDEX_FILE, WEIGHTS_FILE};
use local_model::mlx::{shim_path, METALLIB_FILE, MLX_VERSION};
use local_model::qwen35::Config;
use local_model::safetensors::SafeTensors;
use local_model::service::CONTEXT;
use local_model::vision::{write_weights, VISION_FILE, VISION_VERSION};

/// The shapes the app runs the Neural Engine build with.
const SHAPES: Shapes = Shapes { slots: 4, chunk: 16, context: CONTEXT };
const PACKAGE: &str = "model.mlpackage";

#[derive(Clone, Serialize)]
struct File {
    /// Path in the published repository.
    remote: String,
    /// Path inside the installed model's directory.
    local: String,
    size: u64,
    sha256: String,
}

#[derive(Serialize)]
struct Variant {
    /// Changes whenever any file does; the installed copy records it.
    version: String,
    files: Vec<File>,
}

#[derive(Serialize)]
struct Catalog {
    source: String,
    variants: BTreeMap<String, Variant>,
}

fn sha256(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 8 << 20];
    loop {
        let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Every file under `root/prefix`, as catalog entries relative to it.
fn entries(root: &Path, prefix: &str, local_root: &Path) -> Result<Vec<File>, String> {
    let mut out = Vec::new();
    let mut stack = vec![root.join(prefix)];
    while let Some(dir) = stack.pop() {
        let mut children: Vec<PathBuf> =
            fs::read_dir(&dir).map_err(|e| e.to_string())?.map(|entry| entry.unwrap().path()).collect();
        children.sort();
        for path in children {
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let remote = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            let local = path.strip_prefix(local_root).unwrap().to_string_lossy().replace('\\', "/");
            let size = fs::metadata(&path).map_err(|e| e.to_string())?.len();
            eprintln!("  {remote} ({size} bytes)");
            out.push(File { remote, local, size, sha256: sha256(&path)? });
        }
    }
    out.sort_by(|a, b| a.remote.cmp(&b.remote));
    Ok(out)
}

fn copy(from: &Path, to: &Path) -> Result<(), String> {
    fs::copy(from, to).map(|_| ()).map_err(|e| format!("{} -> {}: {e}", from.display(), to.display()))
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        return Err("usage: build_release <official dir> <out dir> [catalog.json]".into());
    }
    let official = Path::new(&args[1]);
    let out = Path::new(&args[2]);
    let config = Config::load(&official.join("config.json"))?;
    let checkpoint = SafeTensors::open(&official.join("model.safetensors"))?;
    let cancel = AtomicBool::new(false);
    let mut variants = BTreeMap::new();

    // The vision tower, as the checkpoint has it, for both Apple builds.
    let vision = format!("vision/{VISION_VERSION}");
    let vision_dir = out.join(&vision);
    let _ = fs::remove_dir_all(&vision_dir);
    fs::create_dir_all(&vision_dir).map_err(|e| e.to_string())?;
    eprintln!("writing {vision}");
    write_weights(&checkpoint, &vision_dir.join(VISION_FILE))?;
    let vision_files = entries(out, &vision, &vision_dir)?;

    // Neural Engine: the Core ML package, compiled on the device after download.
    let ane = format!("ane/{GRAPH_VERSION}");
    let dir = out.join(&ane);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    eprintln!("writing {ane}");
    let plan = PackagePlan::standard(&config, SHAPES);
    let layout = write_package(&config, &checkpoint, &plan, &dir.join(PACKAGE), &cancel, &mut |_| {})?;
    layout.save(&dir.join(EMBEDDING_FILE))?;
    for name in ["config.json", "tokenizer.json"] {
        copy(&official.join(name), &dir.join(name))?;
    }
    let mut files = entries(out, &ane, &dir)?;
    files.extend(vision_files.iter().cloned());
    variants.insert("ane".to_string(), Variant { version: format!("{GRAPH_VERSION}+{VISION_VERSION}"), files });

    // MLX: page-aligned float16 weights, plus the kernels of the MLX the app links.
    let mlx = format!("mlx/{FORMAT}");
    let dir = out.join(&mlx);
    let _ = fs::remove_dir_all(&dir);
    eprintln!("writing {mlx}");
    convert(&config, &checkpoint, &dir, &cancel, &mut |_, _| {})?;
    for name in ["config.json", "tokenizer.json"] {
        copy(&official.join(name), &dir.join(name))?;
    }
    let runtime = format!("mlx/runtime/{MLX_VERSION}");
    let runtime_dir = out.join(&runtime);
    fs::create_dir_all(&runtime_dir).map_err(|e| e.to_string())?;
    let metallib = shim_path().ok_or("找不到 MLX 运行库")?.with_file_name(METALLIB_FILE);
    copy(&metallib, &runtime_dir.join(METALLIB_FILE))?;
    let mut files = entries(out, &mlx, &dir)?;
    files.extend(entries(out, &runtime, &runtime_dir)?);
    files.extend(vision_files.iter().cloned());
    for required in [INDEX_FILE, WEIGHTS_FILE, METALLIB_FILE, VISION_FILE] {
        assert!(files.iter().any(|file| file.local == required), "{required} missing");
    }
    variants.insert(
        "mlx".to_string(),
        Variant { version: format!("{FORMAT}+{VISION_VERSION}+mlx-{MLX_VERSION}"), files },
    );

    // llama.cpp (Windows and Linux): the GGUF llama.cpp's own converter writes.
    let llama = format!("llama/{GGUF_VERSION}");
    let dir = out.join(&llama);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    eprintln!("writing {llama}");
    convert_qwen35_to_gguf(official, &dir.join(GGUF_FILE), &cancel, &mut |_| {})?;
    for name in ["config.json", "tokenizer.json"] {
        copy(&official.join(name), &dir.join(name))?;
    }
    let mut files = entries(out, &llama, &dir)?;
    let mmproj = format!("llama/{MMPROJ_VERSION}");
    let mmproj_dir = out.join(&mmproj);
    let _ = fs::remove_dir_all(&mmproj_dir);
    fs::create_dir_all(&mmproj_dir).map_err(|e| e.to_string())?;
    eprintln!("writing {mmproj}");
    convert_qwen35_mmproj_to_gguf(official, &mmproj_dir.join(MMPROJ_FILE), &cancel, &mut |_| {})?;
    files.extend(entries(out, &mmproj, &mmproj_dir)?);
    variants.insert("llama".to_string(), Variant { version: format!("{GGUF_VERSION}+{MMPROJ_VERSION}"), files });

    let catalog = Catalog {
        source: "Qwen/Qwen3.5-0.8B@2fc06364715b967f1860aea9cf38778875588b17".to_string(),
        variants,
    };
    let text = serde_json::to_string_pretty(&catalog).expect("catalog serializes") + "\n";
    let target = args.get(3).map(PathBuf::from).unwrap_or_else(|| out.join("catalog.json"));
    fs::write(&target, text).map_err(|e| e.to_string())?;
    eprintln!("catalog: {}", target.display());
    Ok(())
}
