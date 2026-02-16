//! LLM trait definition for structured output generation.

use async_trait::async_trait;
use serde::{Serialize, de::DeserializeOwned};

use crate::error::LlmResult;
use crate::types::{GenerationOptions, GenerationResponse, Message};

/// Trait for LLM implementations supporting structured output generation.
///
/// Implementations can either perform API calls to remote LLM services
/// or use local models via ONNX Runtime or other inference engines.
///
/// # Retry Logic
///
/// This trait does not include built-in retry logic. Use the generic
/// `retry_with_backoff` function from `cognee-utils` to wrap LLM calls:
///
/// ```ignore
/// use cognee_utils::retry::{retry_with_backoff, RetryConfig, RetryDecision};
/// use cognee_llm::{Llm, LlmError};
///
/// let config = RetryConfig::new(3, 100, 5000);
///
/// let result = retry_with_backoff(
///     config,
///     || llm.create_structured_output(text, prompt, None),
///     |error| match error {
///         LlmError::NetworkError(_) | LlmError::RateLimitExceeded(_) => RetryDecision::Retry,
///         _ => RetryDecision::Abort,
///     },
/// ).await?;
/// ```
#[async_trait]
pub trait Llm: Send + Sync {
    /// Generate text completion from messages.
    ///
    /// # Arguments
    /// * `messages` - Conversation messages (system, user, assistant).
    /// * `options` - Generation options (temperature, max_tokens, etc.).
    ///
    /// # Returns
    /// Generated response with content and metadata.
    async fn generate(
        &self,
        messages: Vec<Message>,
        options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse>;

    /// Generate structured output from messages.
    ///
    /// This is the core method for extracting structured data (e.g., knowledge
    /// graphs) from text using LLM with JSON schema validation.
    ///
    /// Inspired by Python's Instructor library, this method:
    /// 1. Constructs a JSON schema from the response type `T`
    /// 2. Includes the schema in the system prompt or as a tool/function
    /// 3. Sends the request to the LLM
    /// 4. Parses and validates the JSON response into type `T`
    /// 5. Returns error on validation failures (caller should handle retries)
    ///
    /// # Type Parameters
    /// * `T` - Response model type (must implement Serialize + DeserializeOwned).
    ///
    /// # Arguments
    /// * `text_input` - User input text to process.
    /// * `system_prompt` - System prompt describing the task and output format.
    /// * `options` - Generation options.
    ///
    /// # Returns
    /// Structured data of type `T` extracted from LLM response.
    ///
    /// # Example
    /// ```ignore
    /// use cognee_llm::{Llm, Message};
    /// use serde::{Deserialize, Serialize};
    ///
    /// #[derive(Serialize, Deserialize)]
    /// struct KnowledgeGraph {
    ///     nodes: Vec<Node>,
    ///     edges: Vec<Edge>,
    /// }
    ///
    /// let llm: Box<dyn Llm> = ...;
    /// let graph: KnowledgeGraph = llm.create_structured_output(
    ///     "Alice told Bob to bring documents.",
    ///     "Extract a knowledge graph with nodes and edges.",
    ///     None,
    /// ).await?;
    /// ```
    async fn create_structured_output<T>(
        &self,
        text_input: &str,
        system_prompt: &str,
        options: Option<GenerationOptions>,
    ) -> LlmResult<T>
    where
        T: Serialize + DeserializeOwned + Send;

    /// Generate structured output with custom messages.
    ///
    /// Similar to `create_structured_output` but allows full control over
    /// the conversation history (useful for multi-turn interactions).
    ///
    /// # Type Parameters
    /// * `T` - Response model type.
    ///
    /// # Arguments
    /// * `messages` - Full conversation messages.
    /// * `options` - Generation options.
    ///
    /// # Returns
    /// Structured data of type `T`.
    async fn create_structured_output_with_messages<T>(
        &self,
        messages: Vec<Message>,
        options: Option<GenerationOptions>,
    ) -> LlmResult<T>
    where
        T: Serialize + DeserializeOwned + Send;

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
        4096 // Conservative default
    }
}
