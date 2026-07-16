//! `httpmock` integration test for the Anthropic structured-output
//! truncation-retry budget (no real API calls).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test code: panics are acceptable"
)]
//!
//! When Anthropic returns `stop_reason == "max_tokens"` the tool input was cut
//! off mid-object. Re-asking with the *same* `max_tokens` would truncate again
//! at the same point, so the adapter must raise the budget toward the model's
//! documented output cap on the retry. This test pins that behavior: the first
//! request (a low configured ceiling) truncates, and the retry must arrive with
//! `max_tokens` raised to the model cap and then succeed.

use cognee_llm::{AnthropicAdapter, GenerationOptions, LlmExt};
use httpmock::prelude::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct Person {
    name: String,
    age: u32,
}

#[tokio::test]
async fn truncation_retry_raises_max_tokens_to_the_model_cap() {
    let server = MockServer::start_async().await;

    // First attempt is sent at the configured ceiling (1000). Return a
    // tool_use that is present and JSON-parseable but flagged truncated.
    let truncated = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/messages")
                .body_includes("\"max_tokens\":1000");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id": "msg_trunc",
                        "type": "message",
                        "role": "assistant",
                        "model": "claude-sonnet-4-20250514",
                        "content": [
                            {"type": "tool_use", "name": "extract_structured_data",
                             "input": {"name": "Ada", "age": 36}}
                        ],
                        "stop_reason": "max_tokens",
                        "usage": {"input_tokens": 10, "output_tokens": 1000}
                    }"#,
                );
        })
        .await;

    // The retry must arrive with max_tokens raised to the model cap (64000 for
    // Claude Sonnet 4), not the original 1000. Only then do we return a
    // complete object.
    let completed = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/messages")
                .body_includes("\"max_tokens\":64000");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id": "msg_ok",
                        "type": "message",
                        "role": "assistant",
                        "model": "claude-sonnet-4-20250514",
                        "content": [
                            {"type": "tool_use", "name": "extract_structured_data",
                             "input": {"name": "Ada Lovelace", "age": 36}}
                        ],
                        "stop_reason": "tool_use",
                        "usage": {"input_tokens": 10, "output_tokens": 20}
                    }"#,
                );
        })
        .await;

    // Sonnet 4 caps output at 64k; a 1000-token ceiling leaves headroom to raise.
    let adapter = AnthropicAdapter::new(
        "claude-sonnet-4-20250514",
        "test-key",
        Some(server.base_url()),
    )
    .expect("construct AnthropicAdapter")
    .with_max_completion_tokens(1000)
    .with_network_retries(0);

    let person: Person = adapter
        .create_structured_output(
            "Ada Lovelace was 36.",
            "Extract the person's name and age.",
            Some(GenerationOptions {
                temperature: Some(0.0),
                ..Default::default()
            }),
        )
        .await
        .expect("structured output should succeed after the budget is raised");

    assert_eq!(person.name, "Ada Lovelace");
    assert_eq!(person.age, 36);

    // Both exchanges must have happened exactly once: the truncated first ask
    // at the ceiling, then the raised retry at the model cap.
    truncated.assert_calls_async(1).await;
    completed.assert_calls_async(1).await;
}

#[tokio::test]
async fn truncation_at_the_model_cap_fails_terminally_without_looping() {
    let server = MockServer::start_async().await;

    // Claude 3.5 Sonnet caps output at 8192, which is also the default ceiling
    // that gets sent, so there is no headroom to raise. A truncation here must
    // fail immediately rather than re-ask at the same budget until
    // MaxRetriesExceeded.
    let truncated = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/messages")
                .body_includes("\"max_tokens\":8192");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id": "msg_trunc_cap",
                        "type": "message",
                        "role": "assistant",
                        "model": "claude-3-5-sonnet-20241022",
                        "content": [
                            {"type": "tool_use", "name": "extract_structured_data",
                             "input": {"name": "Ada", "age": 36}}
                        ],
                        "stop_reason": "max_tokens",
                        "usage": {"input_tokens": 10, "output_tokens": 8192}
                    }"#,
                );
        })
        .await;

    let adapter = AnthropicAdapter::new(
        "claude-3-5-sonnet-20241022",
        "test-key",
        Some(server.base_url()),
    )
    .expect("construct AnthropicAdapter")
    .with_network_retries(0);

    let err = adapter
        .create_structured_output::<Person>(
            "Ada Lovelace was 36.",
            "Extract the person's name and age.",
            None,
        )
        .await
        .expect_err("truncation at the model cap must fail, not loop");

    // Exactly one exchange: it did not re-ask at the same budget.
    truncated.assert_calls_async(1).await;
    assert!(
        err.to_string().contains("output cap"),
        "unexpected error: {err}"
    );
}
