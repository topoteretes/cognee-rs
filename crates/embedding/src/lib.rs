//! Multi-provider text-embedding engine (ONNX, OpenAI-compatible, Ollama, Mock).

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

#[cfg(feature = "onnx")]
pub use config::OnnxEmbeddingConfig;
#[cfg(feature = "onnx")]
pub use download::{ModelUrls, download_model, ensure_model_exists, ensure_tokenizer_exists};
#[cfg(feature = "onnx")]
pub use onnx::OnnxEmbeddingEngine;
