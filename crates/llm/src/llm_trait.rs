//! LLM trait definition for structured output generation.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::error::{LlmError, LlmResult};
use crate::schema::generate_json_schema;
use crate::types::{GenerationOptions, GenerationResponse, Message, MessageRole};

/// Object-safe base trait for LLM implementations.
///
/// Provides type-erased methods that work with `serde_json::Value` for JSON schemas
/// and responses. For ergonomic generic methods, see [`LlmExt`].
#[async_trait]
pub trait Llm: Send + Sync {
    /// Generate text completion from messages.
    async fn generate(
        &self,
        messages: Vec<Message>,
        options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse>;

    /// Generate structured output from text (type-erased).
    ///
    /// Takes a pre-built JSON schema and returns the raw JSON `Value`.
    /// Prefer using [`LlmExt::create_structured_output`] for typed access.
    async fn create_structured_output_raw(
        &self,
        text_input: &str,
        system_prompt: &str,
        json_schema: &Value,
        options: Option<GenerationOptions>,
    ) -> LlmResult<Value> {
        let messages = vec![
            Message {
                role: MessageRole::System,
                content: system_prompt.to_string(),
            },
            Message {
                role: MessageRole::User,
                content: text_input.to_string(),
            },
        ];
        self.create_structured_output_with_messages_raw(messages, json_schema, options)
            .await
    }

    /// Generate structured output from messages (type-erased).
    ///
    /// Takes a pre-built JSON schema and returns the raw JSON `Value`.
    /// Prefer using [`LlmExt::create_structured_output_with_messages`] for typed access.
    async fn create_structured_output_with_messages_raw(
        &self,
        messages: Vec<Message>,
        json_schema: &Value,
        options: Option<GenerationOptions>,
    ) -> LlmResult<Value>;

    /// Get the model identifier.
    fn model(&self) -> &str;

    /// Check if the LLM supports streaming.
    fn supports_streaming(&self) -> bool {
        false
    }

    /// Check if the LLM supports function calling / tool use.
    fn supports_function_calling(&self) -> bool {
        false
    }

    /// Get the maximum context length (in tokens) for this model.
    fn max_context_length(&self) -> u32 {
        4096
    }

    /// Describe the contents of an image using vision capabilities.
    ///
    /// Returns a text description of the image. The default implementation
    /// returns `LlmError::FeatureNotSupported` — override in adapters that
    /// support vision (e.g. OpenAI GPT-4o, Ollama llava).
    ///
    /// # Arguments
    /// * `image_bytes` — Raw image bytes (PNG, JPEG, WebP, GIF, etc.)
    /// * `mime_type` — MIME type string (must start with `"image/"`)
    /// * `options` — Optional generation parameters; if `None`, the
    ///   implementation should use hardcoded defaults matching the Python SDK
    ///   (max_tokens=300).
    async fn transcribe_image(
        &self,
        image_bytes: &[u8],
        mime_type: &str,
        options: Option<GenerationOptions>,
    ) -> LlmResult<String> {
        let _ = (image_bytes, mime_type, options);
        Err(LlmError::FeatureNotSupported(format!(
            "Vision is not supported by model: {}",
            self.model()
        )))
    }

    /// Whether this adapter supports image transcription.
    ///
    /// This is a best-effort heuristic based on the model name. A `true`
    /// return does not guarantee the API will accept vision requests; a
    /// `false` return does not prevent calling `transcribe_image` (which
    /// will return `FeatureNotSupported` from the default impl, or attempt
    /// the API call and surface a server error from a real adapter).
    fn supports_vision(&self) -> bool {
        false
    }
}

/// Extension trait providing generic convenience methods on top of [`Llm`].
/// Auto-implemented for all types that implement `Llm`.
#[async_trait]
pub trait LlmExt: Llm {
    /// Generate structured output from text input.
    ///
    /// Generates a JSON schema from `T`, calls the type-erased
    /// [`Llm::create_structured_output_raw`], and deserializes the result.
    async fn create_structured_output<T>(
        &self,
        text_input: &str,
        system_prompt: &str,
        options: Option<GenerationOptions>,
    ) -> LlmResult<T>
    where
        T: Serialize + DeserializeOwned + JsonSchema + Send,
    {
        let schema = generate_json_schema::<T>();
        let value = self
            .create_structured_output_raw(text_input, system_prompt, &schema, options)
            .await?;
        serde_json::from_value(value).map_err(|e| {
            LlmError::DeserializationError(format!(
                "Failed to deserialize structured output: {}",
                e
            ))
        })
    }

    /// Generate structured output from custom messages.
    ///
    /// Generates a JSON schema from `T`, calls the type-erased
    /// [`Llm::create_structured_output_with_messages_raw`], and deserializes the result.
    async fn create_structured_output_with_messages<T>(
        &self,
        messages: Vec<Message>,
        options: Option<GenerationOptions>,
    ) -> LlmResult<T>
    where
        T: Serialize + DeserializeOwned + JsonSchema + Send,
    {
        let schema = generate_json_schema::<T>();
        let value = self
            .create_structured_output_with_messages_raw(messages, &schema, options)
            .await?;
        serde_json::from_value(value).map_err(|e| {
            LlmError::DeserializationError(format!(
                "Failed to deserialize structured output: {}",
                e
            ))
        })
    }
}

impl<T: Llm + ?Sized> LlmExt for T {}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyLlm;

    #[async_trait]
    impl Llm for DummyLlm {
        async fn generate(
            &self,
            _: Vec<Message>,
            _: Option<GenerationOptions>,
        ) -> LlmResult<GenerationResponse> {
            unimplemented!()
        }
        async fn create_structured_output_with_messages_raw(
            &self,
            _: Vec<Message>,
            _: &Value,
            _: Option<GenerationOptions>,
        ) -> LlmResult<Value> {
            unimplemented!()
        }
        fn model(&self) -> &str {
            "dummy"
        }
    }

    #[tokio::test]
    async fn default_transcribe_image_returns_feature_not_supported() {
        let llm = DummyLlm;
        let result = llm.transcribe_image(b"fake-png", "image/png", None).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, LlmError::FeatureNotSupported(_)),
            "Expected FeatureNotSupported, got: {err:?}"
        );
    }

    #[test]
    fn default_supports_vision_returns_false() {
        let llm = DummyLlm;
        assert!(!llm.supports_vision());
    }
}
