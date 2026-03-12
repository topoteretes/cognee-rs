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
