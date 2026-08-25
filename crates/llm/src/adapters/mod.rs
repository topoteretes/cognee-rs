//! LLM provider adapters.
//!
//! This module contains concrete implementations of the `Llm` trait
//! for various providers (OpenAI, Anthropic, Ollama, local models, etc.).

pub mod anthropic;
// AWS Bedrock provider: the Converse adapter plus the shared AWS plumbing it
// is built on. Feature-gated: the AWS credential/signing stack is not free
// (plan §2.3). (Module docs live in `bedrock/mod.rs`.)
#[cfg(feature = "bedrock")]
pub mod bedrock;
pub mod openai;

pub use anthropic::AnthropicAdapter;
#[cfg(feature = "bedrock")]
pub use bedrock::BedrockAdapter;
pub use openai::OpenAIAdapter;
