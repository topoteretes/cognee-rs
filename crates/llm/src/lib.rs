//! LLM abstraction layer for Cognee.
//!
//! Provides async trait-based abstractions for Large Language Model interactions,
//! with support for structured output generation (inspired by Python's Instructor).
//!
//! # Features
//!
//! - **Async-first**: All operations are async, supporting both API calls and local inference
//! - **Structured outputs**: Generate type-safe structured data (e.g., knowledge graphs) from text
//! - **Provider-agnostic**: Trait-based design supports OpenAI, Anthropic, Ollama, local models, etc.
//! - **Configuration**: Flexible configuration with sensible defaults
//!
//! # Retry Logic
//!
//! This crate does not include built-in retry logic in the trait. Instead, use the
//! generic `retry_with_backoff` utility from `cognee-utils`:
//!
//! ```ignore
//! use cognee_llm::{Llm, LlmConfig, LlmProvider, LlmError};
//! use cognee_utils::retry::{retry_with_backoff, RetryConfig, RetryDecision};
//! use schemars::JsonSchema;
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Serialize, Deserialize, JsonSchema)]
//! struct ExtractedData {
//!     entities: Vec<String>,
//!     relationships: Vec<(String, String, String)>,
//! }
//!
//! let config = LlmConfig::new(LlmProvider::OpenAI, "gpt-4")
//!     .with_api_key("sk-...")
//!     .with_temperature(0.0);
//!
//! let llm: Box<dyn Llm> = create_llm(config)?;
//!
//! // With retry logic
//! let retry_config = RetryConfig::new(3, 100, 5000);
//! let data: ExtractedData = retry_with_backoff(
//!     retry_config,
//!     || llm.create_structured_output(
//!         "Alice told Bob to bring the documents.",
//!         "Extract entities and relationships from the text.",
//!         None,
//!     ),
//!     |error| match error {
//!         LlmError::NetworkError(_) | LlmError::RateLimitExceeded(_) => RetryDecision::Retry,
//!         LlmError::ContentPolicyViolation(_) | LlmError::AuthenticationError(_) => RetryDecision::Abort,
//!         _ => RetryDecision::Retry,
//!     },
//! ).await?;
//! ```

pub mod adapters;
pub mod config;
pub mod dynamic_model;
pub mod error;
pub mod factory;
pub mod llm_trait;
#[cfg(feature = "mock")]
pub mod mock;
pub mod prompts;
pub mod responses_client;
pub mod schema;
pub mod transcriber;
pub mod types;

pub use adapters::OpenAIAdapter;
pub use config::{LlmConfig, LlmProvider};
pub use dynamic_model::{DynamicGraphModel, GraphModelError, graph_schema_to_graph_model};
pub use error::{LlmError, LlmResult};
pub use factory::build_openai_compatible_adapter;
pub use llm_trait::{Llm, LlmExt};
pub use responses_client::{OpenAIResponsesClient, ResponsesClient, ResponsesRequest};
pub use schema::{
    build_schema_prompt, generate_json_schema, generate_json_schema_string, graph_model_to_schema,
    graph_model_to_schema_string,
};
pub use transcriber::{Transcriber, TranscriptionOutput, validate_audio_format};
pub use types::{GenerationOptions, GenerationResponse, Message, MessageRole, TokenUsage};
