pub mod config;
pub mod engine;
pub mod error;
pub mod mock;
pub mod ollama;
pub mod openai_compatible;
pub mod provider;
pub mod utils;

#[cfg(feature = "onnx")]
pub mod download;
#[cfg(feature = "onnx")]
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
