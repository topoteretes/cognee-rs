//! Span attribute integration tests for the OpenAI adapter using
//! `httpmock` (no real API calls).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test code — panics are acceptable"
)]
//!
//! Verifies that:
//! - `OpenAIAdapter::generate` emits the `llm.api_call` span with the
//!   right `cognee.llm.model` and `cognee.llm.provider` fields.
//! - `OpenAIAdapter::transcribe_audio` emits the
//!   `llm.transcription_api_call` span.

use cognee_llm::{Llm, Message, MessageRole, OpenAIAdapter, Transcriber};
use cognee_test_utils::SpanCapture;
use httpmock::prelude::*;

fn build_adapter(server: &MockServer, model: &str) -> OpenAIAdapter {
    OpenAIAdapter::new(model, "test-key", Some(server.base_url()))
        .expect("construct OpenAIAdapter")
        // Disable retries: we only want a single mocked HTTP exchange per test.
        .with_network_retries(0)
}

#[tokio::test]
async fn call_api_records_cognee_llm_model_and_provider() {
    let server = MockServer::start_async().await;
    let _m = server
        .mock_async(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id": "chatcmpl-test",
                        "object": "chat.completion",
                        "created": 1700000000,
                        "model": "gpt-4o-mini",
                        "choices": [
                            {
                                "index": 0,
                                "message": {"role": "assistant", "content": "hi"},
                                "finish_reason": "stop"
                            }
                        ]
                    }"#,
                );
        })
        .await;

    let capture = SpanCapture::install();
    let adapter = build_adapter(&server, "gpt-4o-mini");

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
        Some("gpt-4o-mini"),
    );
    assert_eq!(
        s.field_str("cognee.llm.provider").as_deref(),
        Some("openai"),
    );
}

#[tokio::test]
async fn transcription_api_records_cognee_llm_model_and_provider() {
    let server = MockServer::start_async().await;
    let _m = server
        .mock_async(|when, then| {
            when.method(POST).path("/audio/transcriptions");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "text": "hello world",
                        "language": "en",
                        "duration": 1.0
                    }"#,
                );
        })
        .await;

    let capture = SpanCapture::install();
    let adapter = build_adapter(&server, "gpt-4o-mini");

    let result = adapter
        .transcribe_audio(b"\x00\x01\x02\x03", "wav", None, None)
        .await
        .expect("transcription succeeds");
    assert_eq!(result.text, "hello world");

    let spans = capture.spans();
    let s = spans
        .iter()
        .find(|s| s.name == "llm.transcription_api_call")
        .expect("expected llm.transcription_api_call span");
    // The transcription span carries the configured `transcription_model`,
    // which defaults to `whisper-1` unless `TRANSCRIPTION_MODEL` is set in
    // the environment. We don't pin the exact value (the env var may bleed
    // in during local development) — just that the field is present and
    // non-empty.
    let model = s
        .field_str("cognee.llm.model")
        .expect("transcription span must record cognee.llm.model");
    assert!(!model.is_empty(), "model field must be non-empty");
    assert_eq!(
        s.field_str("cognee.llm.provider").as_deref(),
        Some("openai"),
    );
}
