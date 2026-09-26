//! With the `mlx` feature on Apple silicon: fetches Apple's prebuilt MLX
//! (the `mlx-metal` wheel from PyPI, pinned by SHA-256) and builds
//! `mlx/mework_mlx.cpp` against it into `libmework_mlx.dylib`, beside
//! `libmlx.dylib`.
//!
//! Prebuilt, because building MLX from source needs the Metal shader compiler,
//! which only ships with the full Xcode. A dylib the app `dlopen`s, because
//! MLX needs macOS 14 and the app starts on macOS 13; nothing links against
//! it at build time.
//!
//! Everything lands in one directory, `<repository>/.mlx/<version>/mlx/lib`
//! (or `$MEWORK_MLX_DIR/mlx/lib`), which every build shares (like `.cef`) and
//! `scripts/stage-macos-mlx.mjs` copies into the bundle. The runtime kernels
//! (`mlx.metallib`, 136 MB) come with the MLX model download instead.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Also read by `scripts/stage-macos-mlx.mjs`.
const MLX_VERSION: &str = "0.32.2";
const WHEEL_URL: &str = "https://files.pythonhosted.org/packages/f7/ab/ba1952908c5d2a5070cf1cfbfea0161c4751ea62299e2776819810917483/mlx_metal-0.32.2-py3-none-macosx_14_0_arm64.whl";
const WHEEL_SHA256: &str = "3825fff379dbc107dd3413e564a06caeaa24819910ec49c0439e454c06a1b9b8";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=MEWORK_MLX_DIR");
    let wanted = env::var_os("CARGO_FEATURE_MLX").is_some()
        && env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos")
        && env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64");
    if !wanted {
        return;
    }
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let root = env::var_os("MEWORK_MLX_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("../../.mlx").join(MLX_VERSION));
    let lib = root.join("mlx").join("lib");
    if !lib.join("libmlx.dylib").exists() {
        fetch(&root);
    }
    let source = manifest.join("mlx").join("mework_mlx.cpp");
    let header = manifest.join("mlx").join("mework_mlx.h");
    println!("cargo:rerun-if-changed={}", source.display());
    println!("cargo:rerun-if-changed={}", header.display());
    let shim = lib.join("libmework_mlx.dylib");
    build_shim(&source, &root.join("mlx").join("include"), &lib, &shim);
    println!("cargo:rustc-env=MEWORK_MLX_SHIM={}", shim.display());
    println!("cargo:rustc-env=MEWORK_MLX_VERSION={MLX_VERSION}");
}

/// Downloads and verifies the wheel, then unpacks its `mlx/` tree into `root`.
fn fetch(root: &Path) {
    let parent = root.parent().expect("mlx dir has a parent");
    fs::create_dir_all(parent).expect("create .mlx");
    let wheel = parent.join(format!("mlx_metal-{MLX_VERSION}.whl.part"));
    let status = Command::new("curl")
        .args(["--fail", "--location", "--silent", "--show-error", "--retry", "3", "-o"])
        .arg(&wheel)
        .arg(WHEEL_URL)
        .status()
        .expect("run curl");
    assert!(status.success(), "下载 MLX（{WHEEL_URL}）失败");
    let digest = sha256_hex(&fs::read(&wheel).expect("read wheel"));
    assert_eq!(digest, WHEEL_SHA256, "MLX 安装包校验失败");
    let staging = parent.join(format!("{MLX_VERSION}.unpacking"));
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging).expect("create staging");
    let status = Command::new("/usr/bin/unzip")
        .args(["-q", "-o"])
        .arg(&wheel)
        .arg("mlx/*")
        .arg("-d")
        .arg(&staging)
        .status()
        .expect("run unzip");
    assert!(status.success(), "解压 MLX 失败");
    let _ = fs::remove_dir_all(root);
    fs::rename(&staging, root).expect("place mlx");
    let _ = fs::remove_file(&wheel);
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Always optimized, whatever the profile: debug and release builds share the
/// output, and the shim is the hot loop.
fn build_shim(source: &Path, include: &Path, lib: &Path, out: &Path) {
    let staging = out.with_extension(format!("dylib.{}", std::process::id()));
    let status = Command::new("/usr/bin/clang++")
        .args(["-std=c++20", "-O2", "-fPIC", "-dynamiclib", "-arch", "arm64", "-mmacosx-version-min=14.0"])
        .arg("-I")
        .arg(include)
        .arg(source)
        .arg("-L")
        .arg(lib)
        .args(["-lmlx", "-Wl,-rpath,@loader_path", "-install_name", "@rpath/libmework_mlx.dylib", "-o"])
        .arg(&staging)
        .status()
        .expect("run clang++");
    assert!(status.success(), "编译 mework_mlx.cpp 失败");
    fs::rename(&staging, out).expect("place libmework_mlx.dylib");
}
