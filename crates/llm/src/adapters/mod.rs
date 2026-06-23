//! LLM provider adapters.
//!
//! This module contains concrete implementations of the `Llm` trait
//! for various providers (OpenAI, Anthropic, Ollama, local models, etc.).

pub mod openai;

pub use openai::OpenAIAdapter;
