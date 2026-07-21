//! Regression tests for issue #67: `setLlmMaxCompletionTokens(N)` must cap the
//! LLM request made by `recall`/`search`.
//!
//! The completion retrievers construct with `generation_options: None`, so the
//! request they trigger carries *no* per-call [`GenerationOptions`]. Before the
//! fix that option-less path hardcoded `max_tokens: 16384`
//! (`GenerationOptions::default()`), ignoring the configured value entirely — on
//! a provider with a lower hard ceiling (e.g. Groq's 8192) the request 400'd
//! regardless of `setLlmMaxCompletionTokens`. The fix threads the config value
//! into the adapter's `default_max_tokens`, applied precisely to the option-less
//! path, while an explicit per-call `max_tokens` still wins.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test code — panics are acceptable"
)]

use cognee_llm::adapters::OpenAIAdapter;
use cognee_llm::{GenerationOptions, Llm, Message, MessageRole};
use httpmock::prelude::*;

const CHAT_RESPONSE: &str = r#"{
    "id": "chatcmpl-test",
    "object": "chat.completion",
    "created": 1700000000,
    "model": "gpt-4o-mini",
    "choices": [
        {
            "index": 0,
            "message": {"role": "assistant", "content": "ok"},
            "finish_reason": "stop"
        }
    ]
}"#;

fn user_msg() -> Vec<Message> {
    vec![Message {
        role: MessageRole::User,
        content: "hello".to_string(),
    }]
}

/// The option-less path (what `recall`/`search` retrievers use) must send the
/// configured `default_max_tokens`, not the hardcoded 16384. This is the global
/// completion ceiling that lets a user stay under a provider's hard `max_tokens`
/// limit — the case that reproduced the issue.
#[tokio::test]
async fn option_less_generate_uses_configured_default_max_tokens() {
    let server = MockServer::start_async().await;
    // Mock only matches when the body carries the configured cap; a hit proves
    // the request used 4096, not the old hardcoded 16384.
    let mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .json_body_includes(r#"{"max_tokens": 4096}"#);
            then.status(200)
                .header("content-type", "application/json")
                .body(CHAT_RESPONSE);
        })
        .await;

    let adapter = OpenAIAdapter::new("gpt-4o-mini", "test-key", Some(server.base_url()))
        .unwrap()
        .with_network_retries(0)
        .with_default_max_tokens(Some(4096));

    // `None` options == the retriever construction (`generation_options: None`).
    adapter
        .generate(user_msg(), None)
        .await
        .expect("generate succeeds against mock requiring max_tokens=4096");

    mock.assert_calls_async(1).await;
}

/// An explicit per-call `max_tokens` must win over the configured default,
/// matching Python's `{**llm_args, **kwargs}` precedence.
#[tokio::test]
async fn explicit_max_tokens_overrides_configured_default() {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .json_body_includes(r#"{"max_tokens": 1024}"#);
            then.status(200)
                .header("content-type", "application/json")
                .body(CHAT_RESPONSE);
        })
        .await;

    let adapter = OpenAIAdapter::new("gpt-4o-mini", "test-key", Some(server.base_url()))
        .unwrap()
        .with_network_retries(0)
        .with_default_max_tokens(Some(4096));

    adapter
        .generate(
            user_msg(),
            Some(GenerationOptions {
                max_tokens: Some(1024),
                ..GenerationOptions::default()
            }),
        )
        .await
        .expect("explicit max_tokens=1024 must be sent, overriding the 4096 default");

    mock.assert_calls_async(1).await;
}

/// An explicit `max_tokens: None` must send *no* cap — the cognify extraction
/// paths rely on this to avoid truncating structured JSON — even when a config
/// default is set. Proven by asserting the request omits both token-cap keys.
#[tokio::test]
async fn explicit_none_max_tokens_sends_no_cap() {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_excludes("max_tokens")
                .body_excludes("max_completion_tokens");
            then.status(200)
                .header("content-type", "application/json")
                .body(CHAT_RESPONSE);
        })
        .await;

    let adapter = OpenAIAdapter::new("gpt-4o-mini", "test-key", Some(server.base_url()))
        .unwrap()
        .with_network_retries(0)
        .with_default_max_tokens(Some(4096));

    adapter
        .generate(
            user_msg(),
            Some(GenerationOptions {
                max_tokens: None,
                ..GenerationOptions::default()
            }),
        )
        .await
        .expect("explicit max_tokens=None must send no token cap");

    mock.assert_calls_async(1).await;
}

/// A stray `setLlmMaxCompletionTokens(0)` must not break `recall`/`search`: a
/// `0` default is meaningless and providers reject `max_tokens: 0`, so it is
/// coerced to "no cap" rather than written to the wire.
#[tokio::test]
async fn zero_default_max_tokens_sends_no_cap() {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_excludes("max_tokens")
                .body_excludes("max_completion_tokens");
            then.status(200)
                .header("content-type", "application/json")
                .body(CHAT_RESPONSE);
        })
        .await;

    let adapter = OpenAIAdapter::new("gpt-4o-mini", "test-key", Some(server.base_url()))
        .unwrap()
        .with_network_retries(0)
        .with_default_max_tokens(Some(0));

    adapter
        .generate(user_msg(), None)
        .await
        .expect("a 0 default must send no cap, not max_tokens=0");

    mock.assert_calls_async(1).await;
}

/// Structured-output extraction must NOT inherit the config `default_max_tokens`
/// (that cap targets user-facing answer length via `generate`). Applying it here
/// would truncate tool-call JSON and silently break internal structured calls
/// like feedback detection. An option-less structured call keeps the historical
/// default cap (16384), independent of the configured answer cap.
#[tokio::test]
async fn structured_output_ignores_configured_default_max_tokens() {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                // Historical default, NOT the small configured answer cap (256).
                .json_body_includes(r#"{"max_tokens": 16384}"#);
            then.status(200)
                .header("content-type", "application/json")
                .body(tool_call_response(r#"{"name":"Alice"}"#));
        })
        .await;

    let adapter = OpenAIAdapter::new("gpt-4o-mini", "test-key", Some(server.base_url()))
        .unwrap()
        .with_network_retries(0)
        .with_default_max_tokens(Some(256));

    let schema = serde_json::json!({
        "type": "object",
        "properties": { "name": { "type": "string" } },
        "required": ["name"],
    });

    adapter
        .create_structured_output_with_messages_raw(user_msg(), &schema, None)
        .await
        .expect("structured extraction must send the 16384 default, not the 256 answer cap");

    mock.assert_calls_async(1).await;
}

/// Builds the tool-call chat-completion body shape returned by an OpenAI-style
/// structured-output response (mirrors `openai_structured_output.rs`).
fn tool_call_response(payload_json: &str) -> String {
    let escaped = serde_json::to_string(payload_json).expect("string escapes");
    format!(
        r#"{{"id":"x","object":"chat.completion","created":1,"model":"m",
            "choices":[{{"index":0,"message":{{"role":"assistant","tool_calls":[
                {{"id":"c1","type":"function","function":{{
                    "name":"extract_structured_data","arguments":{escaped}
                }}}}
            ]}},"finish_reason":"tool_calls"}}]}}"#
    )
}
