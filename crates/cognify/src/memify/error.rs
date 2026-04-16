use thiserror::Error;

#[derive(Error, Debug)]
pub enum MemifyError {
    #[error("Graph DB error: {0}")]
    GraphDBError(String),

    #[error("Vector DB error: {0}")]
    VectorDBError(String),

    #[error("Embedding error: {0}")]
    EmbeddingError(String),

    #[error("Configuration error: {0}")]
    ConfigError(String),
}
