//! LLM provider adapters.
//!
//! This module contains concrete implementations of the `Llm` trait
//! for various providers (OpenAI, Anthropic, Ollama, local models, etc.).

#[cfg(all(feature = "android-litert", target_os = "android"))]
pub mod litert;
pub mod openai;

#[cfg(all(feature = "android-litert", target_os = "android"))]
pub use litert::LiteRtAdapter;
pub use openai::OpenAIAdapter;
