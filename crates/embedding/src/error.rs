use thiserror::Error;

#[derive(Error, Debug)]
pub enum EmbeddingError {
    #[error("Model load error: {0}")]
    ModelLoadError(String),

    #[error("Tokenizer error: {0}")]
    TokenizerError(String),

    #[error("Inference error: {0}")]
    InferenceError(String),

    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

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

pub type EmbeddingResult<T> = Result<T, EmbeddingError>;
