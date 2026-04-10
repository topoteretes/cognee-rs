pub mod config;
pub mod download;
pub mod engine;
pub mod error;
pub mod onnx;
pub mod provider;
pub mod utils;

pub use config::{EmbeddingConfig, OnnxEmbeddingConfig};
pub use download::{ModelUrls, download_model, ensure_model_exists, ensure_tokenizer_exists};
pub use engine::EmbeddingEngine;
pub use error::{EmbeddingError, EmbeddingResult};
pub use onnx::OnnxEmbeddingEngine;
pub use provider::EmbeddingProvider;
