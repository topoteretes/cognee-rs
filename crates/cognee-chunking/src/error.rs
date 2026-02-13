use thiserror::Error;

#[derive(Debug, Error)]
pub enum ChunkingError {
    #[error("Invalid chunk size: {0} (must be > 0)")]
    InvalidChunkSize(usize),

    #[error("Storage error: {0}")]
    StorageError(String),

    #[error("Invalid UTF-8 content: {0}")]
    InvalidUtf8(String),
}
