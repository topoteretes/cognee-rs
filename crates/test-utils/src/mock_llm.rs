//! Mock LLM implementation for deterministic testing.
//!
//! Returns canned responses from a queue, enabling unit tests for graph
//! extraction, summarisation, and other LLM-dependent pipeline stages
//! without requiring a real API endpoint.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "mock infrastructure — panics are acceptable"
)]

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::Value;

use cognee_llm::types::{GenerationOptions, GenerationResponse, Message};
use cognee_llm::{Llm, LlmError, LlmResult};

/// A test-only LLM that pops pre-loaded JSON responses from an internal queue.
///
/// # Usage
///
/// ```ignore
/// let mock = MockLlm::new(vec![
///     serde_json::json!({"nodes": [], "relationships": []}).to_string(),
/// ]);
/// let llm: Arc<dyn Llm> = Arc::new(mock);
/// ```
///
/// When the queue is exhausted, subsequent calls return an empty
/// `KnowledgeGraph`-shaped JSON object.
pub struct MockLlm {
    responses: Mutex<VecDeque<String>>,
    vision_responses: Mutex<VecDeque<String>>,
    model_name: String,
}

impl MockLlm {
    /// Create a new `MockLlm` pre-loaded with the given responses.
    ///
    /// Responses are returned in FIFO order.  Each string should be valid
    /// JSON matching whatever schema the caller expects.
    pub fn new(responses: Vec<String>) -> Self {
        Self {
            responses: Mutex::new(VecDeque::from(responses)),
            vision_responses: Mutex::new(VecDeque::new()),
            model_name: "mock-llm".to_string(),
        }
    }

    /// Create a `MockLlm` that always returns an empty knowledge graph.
    pub fn empty() -> Self {
        Self::new(vec![])
    }

    /// Pre-load vision responses for `transcribe_image` calls.
    pub fn with_vision_responses(self, responses: Vec<String>) -> Self {
        *self
            .vision_responses
            .lock()
            .expect("MockLlm vision lock poisoned") = VecDeque::from(responses); // lock poison is unrecoverable
        self
    }

    fn pop_response(&self) -> String {
        let mut queue = self.responses.lock().expect("MockLlm lock poisoned");
        queue
            .pop_front()
            .unwrap_or_else(|| r#"{"nodes":[],"relationships":[]}"#.to_string())
    }
}

#[async_trait]
impl Llm for MockLlm {
    async fn generate(
        &self,
        _messages: Vec<Message>,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        let content = self.pop_response();
        Ok(GenerationResponse {
            content,
            model: self.model_name.clone(),
            usage: None,
            finish_reason: Some("stop".to_string()),
        })
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        _messages: Vec<Message>,
        _json_schema: &Value,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<Value> {
        let raw = self.pop_response();
        serde_json::from_str(&raw).map_err(|e| {
            LlmError::DeserializationError(format!(
                "MockLlm: canned response is not valid JSON: {e}"
            ))
        })
    }

    fn model(&self) -> &str {
        &self.model_name
    }

    async fn transcribe_image(
        &self,
        _image_bytes: &[u8],
        _mime_type: &str,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<String> {
        let mut queue = self
            .vision_responses
            .lock()
            .expect("MockLlm vision lock poisoned"); // lock poison is unrecoverable
        queue.pop_front().ok_or_else(|| {
            LlmError::FeatureNotSupported("MockLlm: no vision responses queued".to_string())
        })
    }

    fn supports_vision(&self) -> bool {
        let queue = self
            .vision_responses
            .lock()
            .expect("MockLlm vision lock poisoned"); // lock poison is unrecoverable
        !queue.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn returns_queued_responses_in_order() {
        let mock = MockLlm::new(vec!["\"first\"".to_string(), "\"second\"".to_string()]);

        let r1 = mock.generate(vec![], None).await.unwrap();
        assert_eq!(r1.content, "\"first\"");

        let r2 = mock.generate(vec![], None).await.unwrap();
        assert_eq!(r2.content, "\"second\"");
    }

    #[tokio::test]
    async fn returns_empty_kg_when_queue_exhausted() {
        let mock = MockLlm::empty();
        let r = mock.generate(vec![], None).await.unwrap();
        assert!(r.content.contains("nodes"));
        assert!(r.content.contains("relationships"));
    }

    #[tokio::test]
    async fn structured_output_parses_canned_json() {
        let canned = json!({"nodes": [{"name": "Alice"}], "relationships": []});
        let mock = MockLlm::new(vec![canned.to_string()]);

        let schema = json!({}); // schema ignored by mock
        let val = mock
            .create_structured_output_with_messages_raw(vec![], &schema, None)
            .await
            .unwrap();

        assert_eq!(val["nodes"][0]["name"], "Alice");
    }

    #[tokio::test]
    async fn vision_returns_queued_response() {
        let mock = MockLlm::new(vec![]).with_vision_responses(vec!["A red square.".to_string()]);

        let result = mock.transcribe_image(b"fake", "image/png", None).await;
        assert_eq!(result.unwrap(), "A red square.");
    }

    #[tokio::test]
    async fn vision_returns_error_when_queue_empty() {
        let mock = MockLlm::empty();
        let result = mock.transcribe_image(b"fake", "image/png", None).await;
        assert!(matches!(result, Err(LlmError::FeatureNotSupported(_))));
    }

    #[test]
    fn supports_vision_reflects_queue_state() {
        let mock_no_vision = MockLlm::empty();
        assert!(!mock_no_vision.supports_vision());

        let mock_with_vision = MockLlm::new(vec![]).with_vision_responses(vec!["test".to_string()]);
        assert!(mock_with_vision.supports_vision());
    }
}
