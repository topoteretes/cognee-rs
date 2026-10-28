//! The LLM used when the configured provider has no API key.

use async_trait::async_trait;
use serde_json::Value;

use crate::error::{LlmError, LlmResult};
use crate::llm_trait::Llm;
use crate::types::{GenerationOptions, GenerationResponse, Message};

/// Stands in for an LLM whose provider needs an API key that is not set:
/// every call fails with "LLM API key is not set.", as Python's client does.
pub struct UnconfiguredLlm {
    provider: String,
}

impl UnconfiguredLlm {
    pub fn new(provider: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
        }
    }

    fn key_not_set(&self) -> LlmError {
        LlmError::ConfigError(format!(
            "LLM API key is not set. LLM provider '{}' requires one (set LLM_API_KEY).",
            self.provider
        ))
    }
}

#[async_trait]
impl Llm for UnconfiguredLlm {
    async fn generate(
        &self,
        _messages: Vec<Message>,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        Err(self.key_not_set())
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        _messages: Vec<Message>,
        _json_schema: &Value,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<Value> {
        Err(self.key_not_set())
    }

    fn model(&self) -> &str {
        ""
    }

    fn is_configured(&self) -> bool {
        false
    }

    async fn transcribe_image(
        &self,
        _image_bytes: &[u8],
        _mime_type: &str,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<String> {
        Err(self.key_not_set())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unconfigured_llm_reports_it() {
        assert!(!UnconfiguredLlm::new("openai").is_configured());
    }
}
