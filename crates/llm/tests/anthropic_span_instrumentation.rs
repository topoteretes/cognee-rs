//! Span attribute integration tests for the Anthropic adapter using
//! `httpmock` (no real API calls).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test code: panics are acceptable"
)]
//!
//! Verifies that `AnthropicAdapter::generate` emits the `llm.api_call` span
//! with the right `cognee.llm.model` and `cognee.llm.provider` fields, so
//! Anthropic traffic is visible to telemetry on par with the OpenAI adapter
//! (see `openai_span_instrumentation.rs`).

use cognee_llm::{AnthropicAdapter, Llm, Message, MessageRole};
use cognee_test_utils::SpanCapture;
use httpmock::prelude::*;

fn build_adapter(server: &MockServer, model: &str) -> AnthropicAdapter {
    AnthropicAdapter::new(model, "test-key", Some(server.base_url()))
        .expect("construct AnthropicAdapter")
        // Disable retries: we only want a single mocked HTTP exchange per test.
        .with_network_retries(0)
}

#[tokio::test]
async fn call_api_records_cognee_llm_model_and_provider() {
    let server = MockServer::start_async().await;
    let _m = server
        .mock_async(|when, then| {
            when.method(POST).path("/messages");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id": "msg_test",
                        "type": "message",
                        "role": "assistant",
                        "model": "claude-3-5-sonnet-20241022",
                        "content": [
                            {"type": "text", "text": "hi"}
                        ],
                        "stop_reason": "end_turn",
                        "usage": {"input_tokens": 10, "output_tokens": 1}
                    }"#,
                );
        })
        .await;

    let capture = SpanCapture::install();
    let adapter = build_adapter(&server, "claude-3-5-sonnet-20241022");

    let resp = adapter
        .generate(
            vec![Message {
                role: MessageRole::User,
                content: "hello".to_string(),
            }],
            None,
        )
        .await
        .expect("generate succeeds against mocked server");
    assert_eq!(resp.content, "hi");

    let spans = capture.spans();
    let s = spans
        .iter()
        .find(|s| s.name == "llm.api_call")
        .expect("expected llm.api_call span");
    assert_eq!(
        s.field_str("cognee.llm.model").as_deref(),
        Some("claude-3-5-sonnet-20241022"),
    );
    assert_eq!(
        s.field_str("cognee.llm.provider").as_deref(),
        Some("anthropic"),
    );
}
