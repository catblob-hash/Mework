//! The Neural Engine backend: the model is written as a Core ML ML Program
//! (the only public way onto the ANE), compiled by Core ML on the device, and
//! run with Core ML states for the KV cache and DeltaNet states.
//!
//! Writing the program is plain Rust and builds everywhere (its tests run on
//! every platform); running it needs macOS 15.

pub mod blob;
pub mod graph;
pub mod mil;
pub mod package;
pub mod proto;
#[cfg(target_os = "macos")]
pub mod runtime;
#[cfg(target_os = "macos")]
pub mod backend;
