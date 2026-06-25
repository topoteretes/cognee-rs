//! Error types for LLM operations.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum LlmError {
    #[error("API request failed: {0}")]
    ApiError(String),

    #[error("Network error: {0}")]
    NetworkError(String),

    #[error("Serialization error: {0}")]
    SerializationError(String),

    #[error("Deserialization error: {0}")]
    DeserializationError(String),

    #[error("Invalid response format: {0}")]
    InvalidResponse(String),

    #[error("Rate limit exceeded: {0}")]
    RateLimitExceeded(String),

    #[error("Content policy violation: {0}")]
    ContentPolicyViolation(String),

    #[error("Model not found: {0}")]
    ModelNotFound(String),

    #[error("Authentication failed: {0}")]
    AuthenticationError(String),

    #[error("Timeout: {0}")]
    Timeout(String),

    #[error("Max retries exceeded: {0}")]
    MaxRetriesExceeded(String),

    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("Feature not supported: {0}")]
    FeatureNotSupported(String),

    #[error("Local model error: {0}")]
    LocalModelError(String),

    #[error(
        "Unsupported audio format: {0}. Supported formats: mp3, mp4, mpeg, mpga, m4a, wav, webm"
    )]
    InvalidAudioFormat(String),
}

/// Result type for LLM operations.
pub type LlmResult<T> = Result<T, LlmError>;
