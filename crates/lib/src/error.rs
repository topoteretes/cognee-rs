//! Error types for the cognee-lib crate.

use thiserror::Error;

/// Errors that can occur during component initialization or access.
#[derive(Debug, Error)]
pub enum ComponentError {
    #[error("Storage error: {0}")]
    Storage(String),

    #[error("Database error: {0}")]
    Database(String),

    #[error("Graph database error: {0}")]
    GraphDb(String),

    #[error("Vector database error: {0}")]
    VectorDb(String),

    #[error("Embedding engine error: {0}")]
    EmbeddingEngine(String),

    #[error("LLM error: {0}")]
    Llm(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
