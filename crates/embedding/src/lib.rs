//! Multi-provider text-embedding engine (ONNX, OpenAI-compatible, Ollama, Bedrock, Mock).

/// Embedding engine configuration.
pub mod config;
/// `EmbeddingEngine` trait definition.
pub mod engine;
/// Error types for embedding operations.
pub mod error;
/// Mock embedding engine for tests.
pub mod mock;
/// Ollama embedding engine implementation.
pub mod ollama;
/// OpenAI-compatible embedding engine implementation.
pub mod openai_compatible;
/// Embedding provider selection.
pub mod provider;
/// Shared utilities for embedding input sanitization and response handling.
pub mod utils;

// Module docs live in `bedrock/mod.rs`; an outer `///` here would merge with
// them and make their intra-doc links resolve in *this* module's scope.
#[cfg(feature = "bedrock")]
pub mod bedrock;

#[cfg(feature = "onnx")]
/// Lazy model and tokenizer download from HuggingFace Hub.
pub mod download;
#[cfg(feature = "onnx")]
/// ONNX Runtime-based local embedding engine.
pub mod onnx;

pub use config::EmbeddingConfig;
pub use engine::EmbeddingEngine;
pub use error::{EmbeddingError, EmbeddingResult};
pub use mock::{MockEmbeddingEngine, MockVectorMode};
pub use ollama::OllamaEmbeddingEngine;
pub use openai_compatible::OpenAICompatibleEmbeddingEngine;
pub use provider::EmbeddingProvider;
pub use utils::{handle_embedding_response, is_embeddable, sanitize_embedding_inputs};

#[cfg(feature = "bedrock")]
pub use bedrock::{BedrockEmbeddingEngine, BedrockEmbeddingFamily};

#[cfg(feature = "onnx")]
pub use config::OnnxEmbeddingConfig;
#[cfg(feature = "onnx")]
pub use download::{ModelUrls, download_model, ensure_model_exists, ensure_tokenizer_exists};
#[cfg(feature = "onnx")]
pub use onnx::OnnxEmbeddingEngine;
