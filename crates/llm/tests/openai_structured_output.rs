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

use cognee_llm::{Llm, LlmError, LlmExt, OpenAIAdapter};
use httpmock::prelude::*;
use serde_json::json;

/// A structured-output target with two required fields. `type` is the field the
/// live regression (real OpenAI, gpt-4o-mini) intermittently omitted under tool
/// calling, aborting cognify with `missing field \`type\``.
#[derive(Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
struct Node {
    name: String,
    r#type: String,
}

/// Body of an OpenAI tool-call response whose `arguments` is `payload`
/// (already a JSON string). Helper to keep the mock bodies readable.
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

#[tokio::test]
async fn typed_validation_failure_retries_with_corrective_and_succeeds() {
    // Regression: under tool calling (no strict schema) the model can return
    // well-formed JSON that omits a required field. The first response omits
    // `type`; deserializing into `Node` fails with `missing field \`type\``.
    // The adapter must re-ask with a corrective instruction and succeed on the
    // second, complete response — instead of aborting the pipeline.
    let server = MockServer::start_async().await;

    // Attempt 1: no corrective marker in the body yet → incomplete payload.
    let incomplete = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_excludes("failed validation");
            then.status(200)
                .header("content-type", "application/json")
                .body(tool_call_response(r#"{"name":"Alice"}"#));
        })
        .await;

    // Attempt 2: the corrective instruction (carrying the validation error) is
    // now present in the request body → complete payload.
    let complete = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("failed validation");
            then.status(200)
                .header("content-type", "application/json")
                .body(tool_call_response(r#"{"name":"Alice","type":"Person"}"#));
        })
        .await;

    let adapter = OpenAIAdapter::new("gpt-4o-mini", "test-key", Some(server.base_url()))
        .unwrap()
        .with_network_retries(0)
        .with_structured_output_retries(3);

    let node: Node = adapter
        .create_structured_output("some input", "extract a node", None)
        .await
        .expect("second (complete) response must satisfy the validator");

    assert_eq!(node.name, "Alice");
    assert_eq!(node.r#type, "Person");
    // Exactly one retry fired: one incomplete hit, one corrective hit.
    incomplete.assert_calls_async(1).await;
    complete.assert_calls_async(1).await;
}

#[tokio::test]
async fn typed_validation_failure_exhausts_retries_and_surfaces_error() {
    // Every attempt returns well-formed JSON that still omits the required
    // `type` field. After exhausting `structured_output_retries`, the adapter
    // must surface a `DeserializationError` naming the missing field rather
    // than silently returning an invalid object.
    let server = MockServer::start_async().await;
    let m = server
        .mock_async(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .body(tool_call_response(r#"{"name":"Alice"}"#));
        })
        .await;

    let adapter = OpenAIAdapter::new("gpt-4o-mini", "test-key", Some(server.base_url()))
        .unwrap()
        .with_network_retries(0)
        .with_structured_output_retries(3);

    let err = adapter
        .create_structured_output::<Node>("some input", "extract a node", None)
        .await
        .expect_err("all responses omit a required field → must fail");

    match err {
        LlmError::DeserializationError(msg) => {
            assert!(
                msg.contains("missing field `type`"),
                "error should name the missing required field, got: {msg}"
            );
        }
        other => panic!("expected DeserializationError, got: {other:?}"),
    }
    // All three attempts were made (no early success).
    m.assert_calls_async(3).await;
}
