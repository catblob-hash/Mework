//! The MLX build of the model (Metal GPU, Apple silicon).
//!
//! `weights` writes and reads its weight files and builds anywhere; the
//! backend itself exists with the `mlx` feature on Apple silicon. Its model
//! code is C++ against Apple's prebuilt MLX (`mlx/mework_mlx.cpp`, built by
//! `build.rs` into `libmework_mlx.dylib`), which this module `dlopen`s on first
//! use: MLX needs macOS 14 and the app starts on macOS 13.

pub mod weights;

#[cfg(all(feature = "mlx", target_os = "macos", target_arch = "aarch64"))]
mod backend;
#[cfg(all(feature = "mlx", target_os = "macos", target_arch = "aarch64"))]
pub use backend::{shim_path, MlxBackend, MLX_VERSION};

/// The MLX kernels (`mlx.metallib`) the model download carries; it has to
/// match the MLX the app was built with.
pub const METALLIB_FILE: &str = "mlx.metallib";
