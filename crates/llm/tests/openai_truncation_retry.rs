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

/// A complete legacy `function_call` response (cascade mode 2).
fn legacy_function_call() -> String {
    let payload =
        serde_json::to_string(r#"{"name": "Alexander Graham Bell"}"#).expect("string escapes");
    format!(
        r#"{{"id":"x","object":"chat.completion","created":1,"model":"m",
            "choices":[{{"index":0,"message":{{"role":"assistant","function_call":{{
                "name":"extract_structured_data","arguments":{payload}
            }}}},"finish_reason":"function_call"}}]}}"#
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

/// A budget the *caller* chose is a constraint, not a suggestion: it must not be
/// silently re-issued at the ceiling. The HTTP `custom-prompt` route forwards
/// client-supplied budgets straight into structured output, so raising one would
/// bill a much larger completion than the client asked for.
#[tokio::test]
async fn caller_supplied_budget_is_not_raised_silently() {
    let server = MockServer::start_async().await;

    let tools = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes(r#""max_tokens":1000"#);
            then.status(200)
                .header("content-type", "application/json")
                .body(truncated_tool_call("length"));
        })
        .await;

    // Raising to the ceiling would look like this. It must never be issued.
    let raised = server
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

    let err = adapter(server.base_url())
        .create_structured_output_with_messages_raw(
            user_msg(),
            &schema(),
            Some(GenerationOptions {
                max_tokens: Some(1000),
                ..Default::default()
            }),
        )
        .await
        .expect_err("an explicit caller budget must not be overridden");

    let msg = err.to_string();
    assert!(
        msg.contains("truncated"),
        "must name truncation; got: {msg}"
    );
    assert!(
        msg.contains("1000"),
        "must name the caller's own budget; got: {msg}"
    );
    tools.assert_calls_async(1).await;
    raised.assert_calls_async(0).await;
}

/// `LLM_ARGS` is merged inside `call_api` and only fills *absent* keys, so the
/// budget on the wire can exceed anything visible in the request being built.
/// Writing the ceiling into the body would suppress that value — turning the
/// "raise" into a silent halving, which then re-truncates at the smaller budget
/// and reports an error recommending the very setting it just overrode.
#[tokio::test]
async fn llm_args_budget_is_not_clobbered_by_the_raise() {
    let server = MockServer::start_async().await;

    // LLM_ARGS asks for double the 16384 ceiling. The request cognify builds
    // carries no cap, so this is what actually reaches the provider.
    let mut extra = serde_json::Map::new();
    extra.insert("max_tokens".to_string(), serde_json::json!(32768));

    let uncapped = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes(r#""max_tokens":32768"#);
            then.status(200)
                .header("content-type", "application/json")
                .body(truncated_tool_call("length"));
        })
        .await;

    // The bug this pins: a retry sent at the 16384 ceiling, i.e. *below* the
    // budget that just truncated.
    let lowered = server
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

    let err = OpenAIAdapter::new("gpt-4o-mini", "test-key", Some(server.base_url()))
        .unwrap()
        .with_network_retries(0)
        .with_structured_output_retries(2)
        .with_extra_args(extra)
        .create_structured_output_with_messages_raw(
            user_msg(),
            &schema(),
            Some(GenerationOptions {
                max_tokens: None,
                ..Default::default()
            }),
        )
        .await
        .expect_err("32768 already exceeds the ceiling, so there is nothing to raise to");

    let msg = err.to_string();
    assert!(
        msg.contains("32768"),
        "the error must name the budget that actually truncated, not the ceiling; got: {msg}"
    );
    uncapped.assert_calls_async(1).await;
    lowered.assert_calls_async(0).await;
}

/// A raise established in tool-calling mode must be inherited by the later
/// cascade modes, which rebuild their bodies from `opts`. Without this, a single
/// structured-output attempt per mode (`LLM_MAX_RETRIES=1`) walks the whole
/// cascade at the budget that already truncated.
#[tokio::test]
async fn a_raised_budget_is_inherited_by_the_next_cascade_mode() {
    let server = MockServer::start_async().await;

    // One attempt per mode: the tools loop raises, then immediately runs out.
    let uncapped_tools = server
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

    // Legacy mode must arrive carrying the raised budget, not no cap at all.
    let legacy_raised = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes(r#""functions""#)
                .body_includes(format!(
                    r#""max_tokens":{}"#,
                    OpenAIAdapter::DEFAULT_MAX_COMPLETION_TOKENS
                ));
            then.status(200)
                .header("content-type", "application/json")
                .body(legacy_function_call());
        })
        .await;

    let value = OpenAIAdapter::new("gpt-4o-mini", "test-key", Some(server.base_url()))
        .unwrap()
        .with_network_retries(0)
        .with_structured_output_retries(1)
        .create_structured_output_with_messages_raw(
            user_msg(),
            &schema(),
            Some(GenerationOptions {
                max_tokens: None,
                ..Default::default()
            }),
        )
        .await
        .expect("legacy mode should succeed at the inherited budget");

    assert_eq!(value["name"], "Alexander Graham Bell");
    uncapped_tools.assert_calls_async(1).await;
    legacy_raised.assert_calls_async(1).await;
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
