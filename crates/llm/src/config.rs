//! LLM configuration.

use serde::{Deserialize, Serialize};

/// LLM provider type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LlmProvider {
    OpenAI,
    LiteRt,
    Anthropic,
    Ollama,
    Gemini,
    Mistral,
    Bedrock,
    Local,
}

/// LLM configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    /// LLM provider.
    pub provider: LlmProvider,

    /// Model identifier (e.g., "gpt-4", "claude-3-opus").
    pub model: String,

    /// API key (if required by provider).
    pub api_key: Option<String>,

    /// API endpoint (custom endpoint for self-hosted models).
    pub endpoint: Option<String>,

    /// Default temperature.
    pub temperature: f32,

    /// Default max tokens.
    pub max_tokens: u32,

    /// Enable streaming responses.
    pub streaming: bool,

    /// Request timeout in seconds.
    pub timeout_seconds: u64,

    /// Maximum number of retries.
    pub max_retries: u32,

    /// Enable rate limiting.
    pub rate_limit_enabled: bool,

    /// Rate limit: requests per interval.
    pub rate_limit_requests: u32,

    /// Rate limit interval in seconds.
    pub rate_limit_interval_seconds: u64,

    /// Fallback model (if primary fails).
    pub fallback_model: Option<String>,

    /// Fallback API key.
    pub fallback_api_key: Option<String>,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: LlmProvider::OpenAI,
            model: "gpt-4".to_string(),
            api_key: None,
            endpoint: None,
            temperature: 0.0,
            max_tokens: 16384,
            streaming: false,
            timeout_seconds: 120,
            max_retries: 3,
            rate_limit_enabled: false,
            rate_limit_requests: 60,
            rate_limit_interval_seconds: 60,
            fallback_model: None,
            fallback_api_key: None,
        }
    }
}

impl LlmConfig {
    /// Create a new configuration with minimal required fields.
    pub fn new(provider: LlmProvider, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
            ..Default::default()
        }
    }

    /// Set API key.
    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    /// Set custom endpoint.
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }

    /// Set temperature.
    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = temperature;
        self
    }

    /// Set max tokens.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Set timeout.
    pub fn with_timeout_seconds(mut self, timeout_seconds: u64) -> Self {
        self.timeout_seconds = timeout_seconds;
        self
    }

    /// Set max retries.
    pub fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }
}
