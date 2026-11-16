use thiserror::Error;

#[derive(Debug, Error)]
pub enum ChunkingError {
    #[error("Invalid chunk size: {0} (must be > 0)")]
    InvalidChunkSize(usize),

    #[error("Storage error: {0}")]
    StorageError(String),

    #[error("Invalid UTF-8 content: {0}")]
    InvalidUtf8(String),

    #[error("tokenizer error: {0}")]
    TokenizerError(String),

    /// A chunker was configured with values it cannot work with.
    #[error("invalid chunker configuration: {0}")]
    InvalidConfig(String),

    /// The document's content is not what the chunker expects (e.g. the
    /// JSON-list chunker on something that is not a JSON list).
    #[error("invalid chunker input: {0}")]
    InvalidInput(String),

    /// A chunker produced a chunk over the token budget and, like its Python
    /// counterpart, refuses to emit it.
    #[error("Chunk of {size} tokens is larger than the maximum of {max} tokens; {hint}")]
    ChunkTooLarge {
        size: usize,
        max: usize,
        hint: &'static str,
    },
}
