//! Structured-output behaviour tests for the OpenAI adapter using `httpmock`
//! (no real API calls). Cover the tool-calling retry/fallback semantics:
//!
//! - #2: a non-empty but invalid tool-call `arguments` payload surfaces a clear
//!   `DeserializationError` (rather than being silently swallowed) once retries
//!   are exhausted.
//! - #7 / #4: a tool call with missing/empty `arguments` does not crash the
//!   response parse; the adapter retries and then falls through to the JSON-mode
//!   path, which succeeds.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test code — panics are acceptable"
)]

use cognee_llm::{Llm, LlmError, OpenAIAdapter};
use httpmock::prelude::*;
use serde_json::json;

#[tokio::test]
async fn malformed_tool_call_arguments_surface_deserialization_error() {
    let server = MockServer::start_async().await;
    let _m = server
        .mock_async(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id":"x","object":"chat.completion","created":1,"model":"m",
                        "choices":[{"index":0,"message":{"role":"assistant","tool_calls":[
                            {"id":"c1","type":"function","function":{
                                "name":"extract_structured_data",
                                "arguments":"{ this is not valid json"
                            }}
                        ]},"finish_reason":"tool_calls"}]
                    }"#,
                );
        })
        .await;

    let adapter = OpenAIAdapter::new("gpt-4o-mini", "test-key", Some(server.base_url()))
        .unwrap()
        .with_network_retries(0)
        .with_structured_output_retries(2);

    let schema = json!({"type":"object","properties":{"foo":{"type":"string"}}});
    let err = adapter
        .create_structured_output_raw("input text", "system prompt", &schema, None)
        .await
        .expect_err("malformed non-empty arguments must surface an error");

    match err {
        LlmError::DeserializationError(msg) => {
            assert!(
                msg.contains("this is not valid json"),
                "error should carry the raw payload, got: {msg}"
            );
        }
        other => panic!("expected DeserializationError, got: {other:?}"),
    }
}

#[tokio::test]
async fn empty_tool_call_arguments_fall_through_to_json_mode() {
    // The tool call carries no usable `arguments` (missing), but the same message
    // also echoes valid JSON in `content`. The tool-calling path finds only an
    // empty argument string (retries, does not crash on the missing field — #7),
    // then falls through to the JSON-mode path which parses `content`.
    let server = MockServer::start_async().await;
    let _m = server
        .mock_async(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id":"x","object":"chat.completion","created":1,"model":"m",
                        "choices":[{"index":0,"message":{
                            "role":"assistant",
                            "content":"{\"foo\":\"bar\"}",
                            "tool_calls":[{"id":"c1","type":"function","function":{
                                "name":"extract_structured_data"
                            }}]
                        },"finish_reason":"tool_calls"}]
                    }"#,
                );
        })
        .await;

    let adapter = OpenAIAdapter::new("gpt-4o-mini", "test-key", Some(server.base_url()))
        .unwrap()
        .with_network_retries(0)
        .with_structured_output_retries(1);

    let schema = json!({"type":"object","properties":{"foo":{"type":"string"}}});
    let value = adapter
        .create_structured_output_raw("input text", "system prompt", &schema, None)
        .await
        .expect("should fall through to JSON mode and parse content");

    assert_eq!(value, json!({"foo":"bar"}));
}
