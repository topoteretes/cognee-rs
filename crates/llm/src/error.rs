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
        "Unsupported audio format: {0}. Supported formats: flac, mp3, mp4, mpeg, mpga, m4a, ogg, wav, webm"
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

/// LiteLLM's three budget-rejection sentences, matched whole — Python's
/// `_BUDGET_SENTENCE_RE` (`cognee/infrastructure/llm/exceptions.py`):
///
/// 1. `Budget has been exceeded! [Scope=...] Current cost: X, Max budget: Y`
/// 2. `ExceededBudget: [End ]User=<id> over budget. Spend=X, Budget=Y`
/// 3. `LiteLLM {Virtual Key|End User}: <id>, exceeded budget for model=<m>`
///
/// Matching the whole sentence rather than fragments keeps a document that
/// merely mentions a budget from classifying; the bounded gaps keep the scan
/// linear.
static BUDGET_SENTENCE_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    #[allow(clippy::expect_used, reason = "the pattern is a compile-time constant")]
    regex::Regex::new(concat!(
        r"(?is)Budget has been exceeded!.{0,200}?Max budget:\s*[\d.]+",
        r"|ExceededBudget:.{0,200}?Budget=[\d.]+",
        r"|LiteLLM (?:Virtual Key|End User):.{0,200}?exceeded budget for model=\S{1,100}",
    ))
    .expect("budget sentence pattern compiles")
});

/// The rendered prefixes of this crate's and the cognify crate's typed
/// payment-required errors (`LlmError::PaymentRequired`,
/// `CognifyError::PaymentRequired`). A stage that had to render one into text
/// still classifies, as Python's `is_budget_exhausted_record` matches the
/// recorded `LLMPaymentRequiredError` class name.
const PAYMENT_REQUIRED_RENDERINGS: [&str; 2] = ["LLM payment required: ", "Payment required: "];

/// Whether a *rendered* failure signals LLM budget or payment exhaustion —
/// Python's `is_budget_exhausted_record`, for an error that only survived as
/// text (a stage result's `error`, a cognify failure-report entry).
///
/// Two signals: the rendering of a typed payment-required error, then one of
/// LiteLLM's whole budget sentences.
#[must_use]
pub fn is_budget_exhausted_message(text: &str) -> bool {
    PAYMENT_REQUIRED_RENDERINGS
        .iter()
        .any(|prefix| text.contains(prefix))
        || BUDGET_SENTENCE_RE.is_match(text)
}

#[cfg(test)]
mod budget_message_tests {
    use super::*;

    #[test]
    fn typed_renderings_classify() {
        assert!(is_budget_exhausted_message(
            &LlmError::PaymentRequired("out of credit".into()).to_string()
        ));
        assert!(is_budget_exhausted_message(
            "cognify of the trace feedback failed: LLM payment required: 402"
        ));
        assert!(!is_budget_exhausted_message(
            &LlmError::RateLimitExceeded("slow down".into()).to_string()
        ));
    }

    // Python `test_budget_sentence_*`: the three LiteLLM shapes, whole.
    #[test]
    fn litellm_sentences_classify() {
        for text in [
            "Budget has been exceeded! Current cost: 1.2, Max budget: 1.0",
            "ExceededBudget: User=abc over budget. Spend=3.1, Budget=3.0",
            "LiteLLM Virtual Key: sk-123, exceeded budget for model=gpt-4o",
            "wrapped: litellm.BudgetExceededError: budget has been exceeded! \
             [Scope=team] Current cost: 9, max budget: 5",
        ] {
            assert!(is_budget_exhausted_message(text), "{text}");
        }
    }

    #[test]
    fn fragments_do_not_classify() {
        for text in [
            "the team exceeded its budget last quarter",
            "Budget has been exceeded! but no figures follow",
            "Max budget: 5",
        ] {
            assert!(!is_budget_exhausted_message(text), "{text}");
        }
    }
}
