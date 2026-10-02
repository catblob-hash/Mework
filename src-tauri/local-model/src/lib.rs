//! The local helper model: a small instruct model that runs next to the app to
//! name conversations and explain shell commands. The weights are the official
//! Qwen3.5-0.8B release; everything here works from those files as published.

pub mod coreml;
pub mod engine;
pub mod gguf;
#[cfg(feature = "llama")]
pub mod llama;
pub mod mlx;
pub mod prefix_cache;
pub mod pressure;
pub mod prompts;
pub mod qwen35;
pub mod safetensors;
pub mod scheduler;
pub mod service;
pub mod tokenizer;
pub mod vision;
