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
//! at the same point, so the adapter raises the budget toward the *effective*
//! output cap — `min(llm_max_completion_tokens ceiling, model cap)` — on the
//! retry, and fails terminally once that effective cap is reached (the ceiling
//! is an upper bound on every path, matching the Python reference). These tests
//! pin both behaviors: a caller request below the effective cap gets a raised
//! retry that succeeds, while a budget already at the effective cap fails
//! without looping.

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
async fn truncation_retry_raises_max_tokens_to_the_effective_cap() {
    let server = MockServer::start_async().await;

    // First attempt is sent at the caller-requested budget (1000), which is
    // below the effective cap so there is headroom to raise. Return a tool_use
    // that is present and JSON-parseable but flagged truncated.
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

    // The retry must arrive with max_tokens raised to the effective cap: 64000 =
    // min(ceiling 64000, Claude Sonnet 4 model cap 64000), not the original 1000.
    // Only then do we return a complete object.
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

    // Ceiling is set to the model cap (64000), and the caller requests only 1000,
    // so the effective cap is 64000 and there is headroom above the first budget
    // to raise into. (A ceiling *below* the model cap that binds the first budget
    // is the terminal case — see the test below.)
    let adapter = AnthropicAdapter::new(
        "claude-sonnet-4-20250514",
        "test-key",
        Some(server.base_url()),
    )
    .expect("construct AnthropicAdapter")
    .with_max_completion_tokens(64_000)
    .with_network_retries(0);

    let person: Person = adapter
        .create_structured_output(
            "Ada Lovelace was 36.",
            "Extract the person's name and age.",
            Some(GenerationOptions {
                temperature: Some(0.0),
                max_tokens: Some(1000),
                ..Default::default()
            }),
        )
        .await
        .expect("structured output should succeed after the budget is raised");

    assert_eq!(person.name, "Ada Lovelace");
    assert_eq!(person.age, 36);

    // Both exchanges must have happened exactly once: the truncated first ask at
    // the caller budget, then the raised retry at the effective cap.
    truncated.assert_calls_async(1).await;
    completed.assert_calls_async(1).await;
}

#[tokio::test]
async fn truncation_at_a_binding_ceiling_fails_terminally_without_raising() {
    // When `llm_max_completion_tokens` is the binding constraint (below the model
    // cap), the first request already goes out at the ceiling, so there is no
    // headroom to raise: the effective cap == the budget that just truncated. The
    // call must fail terminally rather than silently exceed the operator's cost
    // ceiling by jumping to the model cap. Matches the Python reference, where
    // `llm_max_completion_tokens` bounds every path and truncation is not
    // recovered by enlarging the budget.
    let server = MockServer::start_async().await;

    // Sonnet 4's model cap is 64000, but the ceiling is 1000, so the first (and
    // only) request goes out at 1000 and there is no room to raise.
    let truncated = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/messages")
                .body_includes("\"max_tokens\":1000");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id": "msg_trunc_ceiling",
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

    let adapter = AnthropicAdapter::new(
        "claude-sonnet-4-20250514",
        "test-key",
        Some(server.base_url()),
    )
    .expect("construct AnthropicAdapter")
    .with_max_completion_tokens(1000)
    .with_network_retries(0);

    let err = adapter
        .create_structured_output::<Person>(
            "Ada Lovelace was 36.",
            "Extract the person's name and age.",
            None,
        )
        .await
        .expect_err("a truncation at the binding ceiling must fail, not raise past it");

    // Exactly one exchange: it did not raise the budget past the ceiling.
    truncated.assert_calls_async(1).await;
    assert!(
        err.to_string().contains("output budget"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn truncation_at_the_model_cap_fails_terminally_without_looping() {
    let server = MockServer::start_async().await;

    // Claude 3.5 Sonnet caps output at 8192, which is also the default ceiling
    // that gets sent, so there is no headroom to raise. A max_tokens stop here
    // must fail immediately rather than re-ask at the same budget until
    // MaxRetriesExceeded — and, matching the Python reference (instructor rejects
    // any length-truncated structured response before validation), it fails even
    // though the tool input parses as a complete object: the truncation flag, not
    // shallow validity, decides.
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
        err.to_string().contains("output budget"),
        "unexpected error: {err}"
    );
}

/// A truncation that left **no** `tool_use` block at all — the budget was spent
/// on thinking or preamble text before the forced tool call began — is still a
/// truncation.
///
/// This is the second disguise a cut-off answer arrives in, and it is not
/// self-describing: the response is shaped exactly like a model that ignored
/// `tool_choice`. Deciding on the payload rather than on `stop_reason` filed it
/// under "did not contain the forced tool_use block" and re-asked at the same
/// `max_tokens`, which truncates at the same point — a full generation plus a
/// backoff per attempt until `MaxRetriesExceeded`. Sonnet 3.5's 8192 cap is also
/// the budget that was sent, so there is no headroom and the call must fail
/// terminally on the first response.
#[tokio::test]
async fn truncation_without_a_tool_use_block_is_reported_as_truncation() {
    let server = MockServer::start_async().await;

    let truncated = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/messages")
                .body_includes("\"max_tokens\":8192");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id": "msg_trunc_no_tool",
                        "type": "message",
                        "role": "assistant",
                        "model": "claude-3-5-sonnet-20241022",
                        "content": [
                            {"type": "text", "text": "Let me work through the passage first"}
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
        .expect_err("a truncation at the model cap must fail, not loop");

    // Exactly one exchange: it did not re-ask at the same budget.
    truncated.assert_calls_async(1).await;
    let message = err.to_string();
    assert!(
        message.contains("truncated") && message.contains("output budget"),
        "the error must name the cause and the budget that was hit: {message}",
    );
    assert!(
        !message.contains("forced tool_use block"),
        "a truncation must not be misfiled as a missing tool call: {message}",
    );
}

/// ...and with headroom below the effective cap, that same classification
/// repairs it: the re-ask goes out at the raised budget, which is the only body
/// the second mock matches — so a re-ask at the caller's 1000 would fail the
/// call counts, not merely the error text.
#[tokio::test]
async fn truncation_without_a_tool_use_block_is_re_asked_with_a_raised_budget() {
    let server = MockServer::start_async().await;

    let truncated = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/messages")
                .body_includes("\"max_tokens\":1000");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id": "msg_trunc_no_tool",
                        "type": "message",
                        "role": "assistant",
                        "model": "claude-sonnet-4-20250514",
                        "content": [
                            {"type": "text", "text": "Let me work through the passage first"}
                        ],
                        "stop_reason": "max_tokens",
                        "usage": {"input_tokens": 10, "output_tokens": 1000}
                    }"#,
                );
        })
        .await;

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

    let adapter = AnthropicAdapter::new(
        "claude-sonnet-4-20250514",
        "test-key",
        Some(server.base_url()),
    )
    .expect("construct AnthropicAdapter")
    .with_max_completion_tokens(64_000)
    .with_network_retries(0);

    let person: Person = adapter
        .create_structured_output(
            "Ada Lovelace was 36.",
            "Extract the person's name and age.",
            Some(GenerationOptions {
                temperature: Some(0.0),
                max_tokens: Some(1000),
                ..Default::default()
            }),
        )
        .await
        .expect("structured output should succeed after the budget is raised");

    assert_eq!(person.name, "Ada Lovelace");
    truncated.assert_calls_async(1).await;
    completed.assert_calls_async(1).await;
}
