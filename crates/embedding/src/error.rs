use thiserror::Error;

/// Error type for embedding engine operations.
#[derive(Error, Debug)]
pub enum EmbeddingError {
    /// Failed to load the embedding model.
    #[error("Model load error: {0}")]
    ModelLoadError(String),

    /// Tokenizer-related failure.
    #[error("Tokenizer error: {0}")]
    TokenizerError(String),

    /// Inference execution failed.
    #[error("Inference error: {0}")]
    InferenceError(String),

    /// Invalid or missing configuration.
    #[error("Configuration error: {0}")]
    ConfigError(String),

    /// An I/O error occurred.
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    /// The requested provider is not yet implemented.
    #[error("Provider not implemented: {0}")]
    NotImplemented(String),

    /// HTTP-level error (network failure, rate-limit 429, server 5xx).
    /// These are considered transient and will be retried by the engine.
    #[error("HTTP error: {0}")]
    HttpError(String),

    /// API-level error (4xx other than 429, unexpected response shape).
    /// These are not retried.
    #[error("API error: {0}")]
    ApiError(String),
}

/// Convenience `Result` alias for embedding engine operations.
pub type EmbeddingResult<T> = Result<T, EmbeddingError>;
