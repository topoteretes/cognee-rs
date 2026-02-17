pub mod config;
pub mod download;
pub mod engine;
pub mod error;
pub mod onnx;
pub mod utils;

// Re-export main types
pub use config::EmbeddingConfig;
pub use download::{ModelUrls, download_model, ensure_model_exists, ensure_tokenizer_exists};
pub use engine::EmbeddingEngine;
pub use error::{EmbeddingError, EmbeddingResult};
pub use onnx::OnnxEmbeddingEngine;
