//! Structured-output behaviour tests for the OpenAI adapter using `httpmock`
//! (no real API calls). Cover the tool-calling retry/fallback semantics:
//!
//! - #6: an empty tool-call `arguments` string no longer shadows JSON echoed in
//!   `message.content` — the tool path uses the content on the first attempt.
//! - #4: a non-JSON tool-call response does not hard-error; the adapter falls
//!   through to the legacy function-calling / JSON-mode requests, which can
//!   still succeed (or surface the fallback's own error when all modes fail).
//! - #8: the JSON-mode fallback retries a non-empty-but-invalid payload instead
//!   of giving up after a single attempt.
//! - typed validation-retry: a well-formed response omitting a required field is
//!   re-asked with a corrective instruction and eventually surfaces an error.
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
async fn all_modes_failing_surfaces_the_fallback_error() {
    // #4: a non-JSON tool-call response must NOT hard-error out of the tool loop.
    // The adapter falls through to legacy function-calling and then JSON mode.
    // Here every mode fails: the tool call has invalid `arguments`, the message
    // also carries invalid non-blank `content`, and there is no `function_call`.
    // The surfaced error therefore comes from the JSON-mode fallback (carrying
    // its `content`), proving control fell through past tool calling.
    let server = MockServer::start_async().await;
    let _m = server
        .mock_async(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id":"x","object":"chat.completion","created":1,"model":"m",
                        "choices":[{"index":0,"message":{"role":"assistant",
                            "content":"definitely not json",
                            "tool_calls":[
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
        .expect_err("all modes fail → must surface an error");

    match err {
        LlmError::DeserializationError(msg) => {
            assert!(
                msg.contains("definitely not json"),
                "fell through to JSON mode; error should carry its content, got: {msg}"
            );
        }
        other => panic!("expected DeserializationError from JSON-mode fallback, got: {other:?}"),
    }
}

#[tokio::test]
async fn malformed_tool_call_falls_through_to_json_mode_and_succeeds() {
    // #4: the tool call returns invalid `arguments` (and no content), so tool
    // calling and legacy function-calling both fail. The JSON-mode request
    // (`response_format: json_object`) then returns a valid object → success.
    let server = MockServer::start_async().await;
    // Tool-calling + legacy requests: no `json_object` response_format.
    let non_json_mode = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_excludes("json_object");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id":"x","object":"chat.completion","created":1,"model":"m",
                        "choices":[{"index":0,"message":{"role":"assistant","tool_calls":[
                            {"id":"c1","type":"function","function":{
                                "name":"extract_structured_data",
                                "arguments":"{ not valid json"
                            }}
                        ]},"finish_reason":"tool_calls"}]
                    }"#,
                );
        })
        .await;
    // JSON-mode request: carries `response_format: {type: json_object}`.
    let json_mode = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("json_object");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id":"x","object":"chat.completion","created":1,"model":"m",
                        "choices":[{"index":0,"message":{"role":"assistant",
                            "content":"{\"foo\":\"bar\"}"
                        },"finish_reason":"stop"}]
                    }"#,
                );
        })
        .await;

    let adapter = OpenAIAdapter::new("gpt-4o-mini", "test-key", Some(server.base_url()))
        .unwrap()
        .with_network_retries(0)
        .with_structured_output_retries(2);

    let schema = json!({"type":"object","properties":{"foo":{"type":"string"}}});
    let value = adapter
        .create_structured_output_raw("input text", "system prompt", &schema, None)
        .await
        .expect("must fall through tool/legacy to JSON mode and parse content");

    assert_eq!(value, json!({"foo":"bar"}));
    assert!(
        non_json_mode.calls_async().await >= 1,
        "tool/legacy attempted"
    );
    json_mode.assert_calls_async(1).await;
}

#[tokio::test]
async fn empty_tool_call_arguments_use_content_on_tool_path() {
    // #6: the tool call carries an empty `arguments` string, but the same message
    // echoes valid JSON in `content`. The empty `arguments` must be treated as
    // absent so the content fallback engages *within the tool path* — resolved on
    // the first request, without falling through to the JSON-mode fallback.
    let server = MockServer::start_async().await;
    let m = server
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
                                "name":"extract_structured_data","arguments":"   "
                            }}]
                        },"finish_reason":"tool_calls"}]
                    }"#,
                );
        })
        .await;

    let adapter = OpenAIAdapter::new("gpt-4o-mini", "test-key", Some(server.base_url()))
        .unwrap()
        .with_network_retries(0)
        .with_structured_output_retries(3);

    let schema = json!({"type":"object","properties":{"foo":{"type":"string"}}});
    let value = adapter
        .create_structured_output_raw("input text", "system prompt", &schema, None)
        .await
        .expect("empty arguments must not shadow content");

    assert_eq!(value, json!({"foo":"bar"}));
    // Exactly one request: content used on the tool path, no retry/fallthrough.
    m.assert_calls_async(1).await;
}

#[tokio::test]
async fn json_mode_retries_nonblank_invalid_then_succeeds() {
    // #8: force the flow into JSON mode (tool/legacy produce no usable object),
    // where the first JSON-mode response is non-empty but invalid JSON. The
    // narrowed `is_blank`-only retry condition would give up after one attempt;
    // the fix retries with the corrective instruction and succeeds.
    let server = MockServer::start_async().await;
    // Tool + legacy: blank tool arguments, no content → fall through.
    let _non_json = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_excludes("json_object");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id":"x","object":"chat.completion","created":1,"model":"m",
                        "choices":[{"index":0,"message":{"role":"assistant","tool_calls":[
                            {"id":"c1","type":"function","function":{
                                "name":"extract_structured_data","arguments":""
                            }}
                        ]},"finish_reason":"tool_calls"}]
                    }"#,
                );
        })
        .await;
    // JSON mode, first attempt (no corrective marker yet): non-blank invalid.
    let json_bad = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("json_object")
                .body_excludes("Return ONLY one valid JSON object");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id":"x","object":"chat.completion","created":1,"model":"m",
                        "choices":[{"index":0,"message":{"role":"assistant",
                            "content":"Sure! Here is the JSON: {oops"
                        },"finish_reason":"stop"}]
                    }"#,
                );
        })
        .await;
    // JSON mode, corrective retry: valid object.
    let json_good = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("Return ONLY one valid JSON object");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id":"x","object":"chat.completion","created":1,"model":"m",
                        "choices":[{"index":0,"message":{"role":"assistant",
                            "content":"{\"foo\":\"bar\"}"
                        },"finish_reason":"stop"}]
                    }"#,
                );
        })
        .await;

    let adapter = OpenAIAdapter::new("gpt-4o-mini", "test-key", Some(server.base_url()))
        .unwrap()
        .with_network_retries(0)
        .with_structured_output_retries(3);

    let schema = json!({"type":"object","properties":{"foo":{"type":"string"}}});
    let value = adapter
        .create_structured_output_raw("input text", "system prompt", &schema, None)
        .await
        .expect("JSON mode must retry the invalid payload and then succeed");

    assert_eq!(value, json!({"foo":"bar"}));
    json_bad.assert_calls_async(1).await;
    json_good.assert_calls_async(1).await;
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
