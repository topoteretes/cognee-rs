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

    /// HTTP 402 — billing/credit exhausted. Terminal: retrying can never make a
    /// billing failure succeed, so it is excluded from retry the way Python's
    /// `retry_if_not_exception_type` excludes `LLMPaymentRequiredError`.
    #[error("Payment required: {0}")]
    PaymentRequired(String),

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

impl LlmError {
    /// A stable, payload-free discriminant for logging.
    ///
    /// Several variants embed provider payloads verbatim — `DeserializationError`
    /// carries the raw Converse body, and `InvalidResponse` carries the raw HTTP
    /// body for a 400 / ValidationException. On the cognify path those are model
    /// output derived from the user's ingested documents, so `Display` must never
    /// reach a log sink. Log this instead.
    #[must_use]
    pub fn log_kind(&self) -> &'static str {
        match self {
            Self::ApiError { .. } => "api_error",
            Self::NetworkError { .. } => "network_error",
            Self::SerializationError { .. } => "serialization_error",
            Self::DeserializationError { .. } => "deserialization_error",
            Self::InvalidResponse { .. } => "invalid_response",
            Self::RateLimitExceeded { .. } => "rate_limit_exceeded",
            Self::ContentPolicyViolation { .. } => "content_policy_violation",
            Self::ModelNotFound { .. } => "model_not_found",
            Self::AuthenticationError { .. } => "authentication_error",
            Self::PaymentRequired { .. } => "payment_required",
            Self::Timeout { .. } => "timeout",
            Self::MaxRetriesExceeded { .. } => "max_retries_exceeded",
            Self::ConfigError { .. } => "config_error",
            Self::FeatureNotSupported { .. } => "feature_not_supported",
            Self::LocalModelError { .. } => "local_model_error",
            Self::InvalidAudioFormat { .. } => "invalid_audio_format",
        }
    }
}
