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
use std::sync::atomic::{AtomicUsize, Ordering};

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
///     serde_json::json!({"nodes": [], "edges": []}).to_string(),
/// ]);
/// let llm: Arc<dyn Llm> = Arc::new(mock);
/// ```
///
/// When the queue is exhausted, subsequent calls return an empty
/// `KnowledgeGraph`-shaped JSON object.
pub struct MockLlm {
    responses: Mutex<VecDeque<String>>,
    vision_responses: Mutex<VecDeque<String>>,
    /// Substrings that make a structured-output call fail instead of popping
    /// the queue. See [`MockLlm::with_failing_markers`].
    failing_markers: Vec<String>,
    /// Canned answer for calls whose schema declares a `summary` property. See
    /// [`MockLlm::with_summary_response`].
    summary_response: Option<String>,
    model_name: String,
    /// Structured-output calls served, successful or failed. Lets a test assert
    /// that a stage stopped scheduling work rather than merely reporting it.
    structured_calls: AtomicUsize,
}

impl MockLlm {
    /// How many structured-output calls this mock has served.
    ///
    /// Counts failed calls too: the point is how much work was *dispatched*,
    /// which is what distinguishes stopping early from carrying on and
    /// discarding the results.
    pub fn structured_calls(&self) -> usize {
        self.structured_calls.load(Ordering::Relaxed)
    }

    /// Create a new `MockLlm` pre-loaded with the given responses.
    ///
    /// Responses are returned in FIFO order.  Each string should be valid
    /// JSON matching whatever schema the caller expects.
    pub fn new(responses: Vec<String>) -> Self {
        Self {
            responses: Mutex::new(VecDeque::from(responses)),
            structured_calls: AtomicUsize::new(0),
            vision_responses: Mutex::new(VecDeque::new()),
            failing_markers: Vec::new(),
            summary_response: None,
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

    /// Fail any structured-output call whose message content contains one of
    /// these markers, *without* consuming a queued response.
    ///
    /// Marker matching is deterministic per chunk, which a FIFO queue cannot be
    /// once a stage dispatches its chunks concurrently: which chunk pops which
    /// response then depends on scheduling. Put the marker in the fixture text
    /// of the chunk that should fail.
    pub fn with_failing_markers(mut self, markers: Vec<String>) -> Self {
        self.failing_markers = markers;
        self
    }

    /// Answer any call whose JSON schema declares a `summary` property with
    /// this response instead of popping the queue.
    ///
    /// Lets one mock serve a whole pipeline, where graph extraction and
    /// summarization both call the LLM: extraction carries `KnowledgeGraph`'s
    /// schema (`nodes` / `edges`) and drains the queue, summarization carries
    /// `SummarizedContent`'s and gets this.
    pub fn with_summary_response(mut self, response: String) -> Self {
        self.summary_response = Some(response);
        self
    }

    /// Whether `schema` is the summarization schema — i.e. declares a
    /// top-level `summary` property.
    fn is_summary_schema(schema: &Value) -> bool {
        schema
            .get("properties")
            .and_then(|props| props.get("summary"))
            .is_some()
    }

    fn pop_response(&self) -> String {
        let mut queue = self.responses.lock().expect("MockLlm lock poisoned");
        queue
            .pop_front()
            .unwrap_or_else(|| r#"{"nodes":[],"edges":[]}"#.to_string())
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
        messages: Vec<Message>,
        json_schema: &Value,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<Value> {
        self.structured_calls.fetch_add(1, Ordering::Relaxed);
        if !self.failing_markers.is_empty() {
            let content: String = messages.iter().map(|m| m.content.as_str()).collect();
            if let Some(marker) = self
                .failing_markers
                .iter()
                .find(|marker| content.contains(marker.as_str()))
            {
                return Err(LlmError::ApiError(format!(
                    "MockLlm: simulated failure on marker {marker:?}"
                )));
            }
        }

        let raw = match &self.summary_response {
            Some(response) if Self::is_summary_schema(json_schema) => response.clone(),
            _ => self.pop_response(),
        };
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
        assert!(r.content.contains("edges"));
    }

    #[tokio::test]
    async fn structured_output_parses_canned_json() {
        let canned = json!({"nodes": [{"name": "Alice"}], "edges": []});
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

    #[tokio::test]
    async fn failing_markers_fail_without_consuming_the_queue() {
        let mock = MockLlm::new(vec![json!({"nodes": [], "edges": []}).to_string()])
            .with_failing_markers(vec!["BOOM".to_string()]);
        let schema = json!({});

        let failed = mock
            .create_structured_output_with_messages_raw(
                vec![Message::user("chunk one BOOM")],
                &schema,
                None,
            )
            .await;
        assert!(matches!(failed, Err(LlmError::ApiError(_))));

        // The queued response is still there for the next, non-matching call.
        let ok = mock
            .create_structured_output_with_messages_raw(
                vec![Message::user("chunk two")],
                &schema,
                None,
            )
            .await
            .unwrap();
        assert_eq!(ok["nodes"], json!([]));
    }

    #[tokio::test]
    async fn summary_schema_calls_bypass_the_queue() {
        let mock = MockLlm::new(vec![json!({"nodes": [], "edges": []}).to_string()])
            .with_summary_response(json!({"summary": "s", "description": "d"}).to_string());
        let summary_schema = json!({"properties": {"summary": {"type": "string"}}});
        let graph_schema = json!({"properties": {"nodes": {"type": "array"}}});

        let summary = mock
            .create_structured_output_with_messages_raw(vec![], &summary_schema, None)
            .await
            .unwrap();
        assert_eq!(summary["summary"], "s");

        // …and the graph call still gets the queued response.
        let graph = mock
            .create_structured_output_with_messages_raw(vec![], &graph_schema, None)
            .await
            .unwrap();
        assert_eq!(graph["nodes"], json!([]));
    }

    #[test]
    fn supports_vision_reflects_queue_state() {
        let mock_no_vision = MockLlm::empty();
        assert!(!mock_no_vision.supports_vision());

        let mock_with_vision = MockLlm::new(vec![]).with_vision_responses(vec!["test".to_string()]);
        assert!(mock_with_vision.supports_vision());
    }
}
