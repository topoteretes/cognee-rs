//! `httpmock` integration tests for OpenAI structured-output truncation
//! detection (no real API calls).
//!
//! A `finish_reason` of `length` means the answer was cut off at the output
//! budget. Before this was detected, a cut-off tool call reached
//! `serde_json::from_str` as a mid-string-truncated payload, failed to parse,
//! was recorded as a *parse failure*, and therefore fell through the entire
//! three-mode cascade — tool calling, legacy functions, JSON mode — each of
//! which re-truncated at the same budget. The operator saw
//! `Deserialization error: EOF while parsing a string` and paid for three modes
//! of identical failure.
//!
//! These tests pin the two behaviours that replace it: a budget below the
//! ceiling is raised for the retry, and a budget already at the ceiling fails
//! terminally with an error naming truncation, without issuing the legacy or
//! JSON-mode requests.
//!
//! The `max_tokens: None` case is the one that matters in production: the
//! cognify fact-extraction and summarization call sites deliberately pass no cap
//! (Python parity), so the *provider's* default applies — 4096 on Baseten — and
//! nothing in the request records it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test code: panics are acceptable"
)]

use cognee_llm::adapters::OpenAIAdapter;
use cognee_llm::{GenerationOptions, Llm, Message, MessageRole};
use httpmock::prelude::*;

fn user_msg() -> Vec<Message> {
    vec![Message {
        role: MessageRole::User,
        content: "extract a person".to_string(),
    }]
}

fn schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": { "name": { "type": "string" } },
        "required": ["name"],
    })
}

/// A tool call whose `arguments` string was cut off mid-value, flagged with the
/// `finish_reason` the server reports when it hits the output budget.
fn truncated_tool_call(finish_reason: &str) -> String {
    // Deliberately unparseable in the same way a real truncation is: the JSON
    // string value is left open. This is what produced "EOF while parsing a
    // string" before truncation was detected.
    let cut_off = serde_json::to_string(r#"{"name": "Alexander Gra"#).expect("string escapes");
    format!(
        r#"{{"id":"x","object":"chat.completion","created":1,"model":"m",
            "choices":[{{"index":0,"message":{{"role":"assistant","tool_calls":[
                {{"id":"c1","type":"function","function":{{
                    "name":"extract_structured_data","arguments":{cut_off}
                }}}}
            ]}},"finish_reason":"{finish_reason}"}}]}}"#
    )
}

/// A complete, parseable tool call.
fn complete_tool_call() -> String {
    let payload =
        serde_json::to_string(r#"{"name": "Alexander Graham Bell"}"#).expect("string escapes");
    format!(
        r#"{{"id":"x","object":"chat.completion","created":1,"model":"m",
            "choices":[{{"index":0,"message":{{"role":"assistant","tool_calls":[
                {{"id":"c1","type":"function","function":{{
                    "name":"extract_structured_data","arguments":{payload}
                }}}}
            ]}},"finish_reason":"tool_calls"}}]}}"#
    )
}

/// A truncation that spent the whole budget on reasoning tokens: flagged
/// `length`, but with no content and no tool call at all.
const BLANK_TRUNCATED: &str = r#"{"id":"x","object":"chat.completion","created":1,"model":"m",
    "choices":[{"index":0,"message":{"role":"assistant","content":""},
    "finish_reason":"length"}],
    "usage":{"prompt_tokens":10,"completion_tokens":16384,"total_tokens":16394}}"#;

fn adapter(base_url: String) -> OpenAIAdapter {
    OpenAIAdapter::new("gpt-4o-mini", "test-key", Some(base_url))
        .unwrap()
        .with_network_retries(0)
        .with_structured_output_retries(2)
}

/// A caller-supplied budget below the ceiling has headroom, so the retry is sent
/// at the ceiling rather than re-asking at the budget that already truncated.
#[tokio::test]
async fn truncated_tool_call_raises_the_budget_to_the_ceiling() {
    let server = MockServer::start_async().await;

    let truncated = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes(r#""max_tokens":1000"#);
            then.status(200)
                .header("content-type", "application/json")
                .body(truncated_tool_call("length"));
        })
        .await;

    // The retry must arrive at the configured ceiling (16384), not the 1000 that
    // truncated. Only then is a complete object returned.
    let completed = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes(format!(
                    r#""max_tokens":{}"#,
                    OpenAIAdapter::DEFAULT_MAX_COMPLETION_TOKENS
                ));
            then.status(200)
                .header("content-type", "application/json")
                .body(complete_tool_call());
        })
        .await;

    let value = adapter(server.base_url())
        .create_structured_output_with_messages_raw(
            user_msg(),
            &schema(),
            Some(GenerationOptions {
                max_tokens: Some(1000),
                ..Default::default()
            }),
        )
        .await
        .expect("the raised retry must succeed");

    assert_eq!(value["name"], "Alexander Graham Bell");
    truncated.assert_calls_async(1).await;
    completed.assert_calls_async(1).await;
}

/// The production shape: cognify passes `max_tokens: None`, so the first request
/// carries no cap at all and the provider's own default silently applies. The
/// truncation must raise the budget to the ceiling and make it explicit.
#[tokio::test]
async fn truncation_under_the_provider_default_raises_to_the_ceiling() {
    let server = MockServer::start_async().await;

    // No `max_tokens` key at all — the request cognify actually sends.
    let uncapped = server
        .mock_async(|when, then| {
            when.method(POST).path("/chat/completions").is_true(|req| {
                let body = req.body_string();
                body.contains(r#""tools""#) && !body.contains("max_tokens")
            });
            then.status(200)
                .header("content-type", "application/json")
                .body(truncated_tool_call("length"));
        })
        .await;

    let completed = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes(format!(
                    r#""max_tokens":{}"#,
                    OpenAIAdapter::DEFAULT_MAX_COMPLETION_TOKENS
                ));
            then.status(200)
                .header("content-type", "application/json")
                .body(complete_tool_call());
        })
        .await;

    let value = adapter(server.base_url())
        .create_structured_output_with_messages_raw(
            user_msg(),
            &schema(),
            Some(GenerationOptions {
                max_tokens: None,
                ..Default::default()
            }),
        )
        .await
        .expect("an uncapped truncation must recover by sending an explicit ceiling");

    assert_eq!(value["name"], "Alexander Graham Bell");
    uncapped.assert_calls_async(1).await;
    completed.assert_calls_async(1).await;
}

/// The regression that matters most: at the ceiling there is no headroom, so the
/// call must fail with an error that names truncation — and must NOT fall through
/// to legacy function calling or JSON mode, which carry the same budget and would
/// truncate identically.
#[tokio::test]
async fn truncation_at_the_ceiling_fails_without_running_the_cascade() {
    let server = MockServer::start_async().await;

    // Option-less structured calls already send the ceiling, so there is no
    // headroom on the very first attempt.
    let tools = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes(r#""tools""#);
            then.status(200)
                .header("content-type", "application/json")
                .body(truncated_tool_call("length"));
        })
        .await;

    // Mode 2 of the cascade: legacy `functions`. Must never be issued.
    let legacy = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes(r#""functions""#);
            then.status(200)
                .header("content-type", "application/json")
                .body(complete_tool_call());
        })
        .await;

    // Mode 3 of the cascade: JSON mode. Must never be issued either.
    let json_mode = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes(r#""response_format""#);
            then.status(200)
                .header("content-type", "application/json")
                .body(complete_tool_call());
        })
        .await;

    let err = adapter(server.base_url())
        .create_structured_output_with_messages_raw(user_msg(), &schema(), None)
        .await
        .expect_err("a truncation at the ceiling is unrecoverable");

    let msg = err.to_string();
    assert!(
        msg.contains("truncated"),
        "the error must name truncation, not deserialization; got: {msg}"
    );
    assert!(
        msg.contains("LLM_MAX_COMPLETION_TOKENS"),
        "the error must say how to fix it; got: {msg}"
    );
    assert!(
        !msg.contains("EOF while parsing"),
        "the raw serde error must not be what surfaces; got: {msg}"
    );

    // One attempt, then a terminal error. No second mode, no third.
    tools.assert_calls_async(1).await;
    legacy.assert_calls_async(0).await;
    json_mode.assert_calls_async(0).await;
}

/// A truncation that consumed the budget entirely on reasoning tokens leaves no
/// content and no tool call. That must be reported as truncation rather than
/// slipping through as "no usable output" and cascading.
#[tokio::test]
async fn blank_truncated_response_is_reported_as_truncation() {
    let server = MockServer::start_async().await;

    let tools = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes(r#""tools""#);
            then.status(200)
                .header("content-type", "application/json")
                .body(BLANK_TRUNCATED);
        })
        .await;

    let err = adapter(server.base_url())
        .create_structured_output_with_messages_raw(user_msg(), &schema(), None)
        .await
        .expect_err("a blank truncation at the ceiling is still a truncation");

    assert!(err.to_string().contains("truncated"), "got: {err}",);
    tools.assert_calls_async(1).await;
}

/// Some OpenAI-compatible gateways front an Anthropic backend and echo its
/// `max_tokens` spelling of the stop reason. Treat it as truncation too.
#[tokio::test]
async fn gateway_max_tokens_finish_reason_is_treated_as_truncation() {
    let server = MockServer::start_async().await;

    let tools = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes(r#""tools""#);
            then.status(200)
                .header("content-type", "application/json")
                .body(truncated_tool_call("max_tokens"));
        })
        .await;

    let err = adapter(server.base_url())
        .create_structured_output_with_messages_raw(user_msg(), &schema(), None)
        .await
        .expect_err("the Anthropic spelling must be recognised");

    assert!(err.to_string().contains("truncated"), "got: {err}");
    tools.assert_calls_async(1).await;
}
