//! LLM provider adapters.
//!
//! This module contains concrete implementations of the `Llm` trait
//! for various providers (OpenAI, Anthropic, Ollama, local models, etc.).

pub mod anthropic;
// AWS Bedrock provider — shared AWS plumbing today, the adapter at R3.
// Feature-gated: the AWS credential/signing stack is not free (plan §2.3).
// (Module docs live in `bedrock/mod.rs`.)
#[cfg(feature = "bedrock")]
pub mod bedrock;
pub mod openai;

pub use anthropic::AnthropicAdapter;
pub use openai::OpenAIAdapter;
