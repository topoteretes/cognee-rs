//! LLM provider adapters.
//!
//! This module contains concrete implementations of the `Llm` trait
//! for various providers (OpenAI, Anthropic, Ollama, local models, etc.).

pub mod anthropic;
pub mod openai;

pub use anthropic::AnthropicAdapter;
pub use openai::OpenAIAdapter;
