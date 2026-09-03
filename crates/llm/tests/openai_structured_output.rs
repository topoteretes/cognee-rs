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

/// An endpoint with no tool-call parser must stop being sent tool calls.
///
/// vLLM started without `--enable-auto-tool-choice --tool-call-parser` answers
/// 200 with the text in `message.content` and never populates `tool_calls`. The
/// cascade used to be stateless per call, so every structured call re-paid the
/// full tools -> corrective -> legacy -> JSON ladder: 4.14 API calls per
/// extraction against 1.09 on an endpoint with a parser, with ~71% of cost
/// producing nothing. After `MISS_THRESHOLD` consecutive calls produce no
/// native tool call, mode 1 is skipped.
#[tokio::test]
async fn tool_calls_stop_after_an_endpoint_never_answers_one() {
    let server = MockServer::start_async().await;

    // Mode 1: tool-calling. `body_excludes("json_object")` keeps this mock off
    // the JSON-mode request, and `body_excludes("\"functions\"")` off the legacy
    // one. Answers with prose in `content` and no `tool_calls` — exactly what a
    // parser-less server does: HTTP 200, nothing usable.
    let tools_mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("\"tools\"")
                .body_excludes("\"functions\"")
                .body_excludes("json_object");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{"id":"x","object":"chat.completion","created":1,"model":"m",
                        "choices":[{"index":0,"message":{"role":"assistant",
                            "content":"Sure! Here is the node you asked for."},
                          "finish_reason":"stop"}]}"#,
                );
        })
        .await;

    // Mode 2: legacy `functions`/`function_call`. Also unanswerable here.
    let legacy_mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("\"functions\"")
                .body_excludes("json_object");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{"id":"x","object":"chat.completion","created":1,"model":"m",
                        "choices":[{"index":0,"message":{"role":"assistant",
                            "content":"still prose"},"finish_reason":"stop"}]}"#,
                );
        })
        .await;

    // Mode 3: JSON mode — the one this server can actually answer, so every
    // call still succeeds. The assertion is about how many *mode 1* requests
    // are sent, not about whether the call works.
    let json_mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("json_object");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{"id":"x","object":"chat.completion","created":1,"model":"m",
                        "choices":[{"index":0,"message":{"role":"assistant",
                            "content":"{\"foo\":\"bar\"}"},
                          "finish_reason":"stop"}]}"#,
                );
        })
        .await;

    let adapter = OpenAIAdapter::new("gpt-4o-mini", "test-key", Some(server.base_url()))
        .unwrap()
        .with_network_retries(0)
        .with_structured_output_retries(1);

    let schema = json!({"type":"object","properties":{"foo":{"type":"string"}}});

    // Three calls trip the probe.
    for i in 0..3 {
        adapter
            .create_structured_output_raw("input text", "system prompt", &schema, None)
            .await
            .unwrap_or_else(|e| panic!("call {i} should still succeed via JSON mode: {e}"));
    }
    // Exact counts, not `>=`: with one structured-output retry and no network
    // retries each call issues exactly one request per mode, so a looser bound
    // would let a regression in request *volume* — the whole point of this
    // change — pass unnoticed.
    let tools_after_priming = tools_mock.calls_async().await;
    assert_eq!(
        tools_after_priming, 3,
        "each of the first three calls must have tried tool calling exactly once"
    );
    let legacy_after_priming = legacy_mock.calls_async().await;
    assert_eq!(
        legacy_after_priming, 3,
        "the priming calls should have burned legacy mode exactly once each"
    );

    // The fourth call must skip mode 1 entirely.
    adapter
        .create_structured_output_raw("input text", "system prompt", &schema, None)
        .await
        .expect("fourth call still succeeds");

    assert_eq!(
        tools_mock.calls_async().await,
        tools_after_priming,
        "tool-calling mode must not be re-sent once the endpoint is known to lack a parser"
    );
    assert_eq!(
        json_mock.calls_async().await,
        4,
        "all four calls must be answered by JSON mode, one request each"
    );
    // Legacy `functions` needs a server-side parser too, so it must stop being
    // sent as well — otherwise the cascade is only a third shorter and the
    // per-extraction call count barely moves.
    assert_eq!(
        legacy_mock.calls_async().await,
        legacy_after_priming,
        "legacy function-call mode must also stop being re-sent to a parser-less endpoint"
    );
}

/// A tool-calling request that *errors* says nothing about whether the endpoint
/// has a parser, so it must not count towards skipping the mode.
///
/// Without this rule a brief gateway hiccup — three concurrent chunks in a
/// cognify fan-out getting an HTTP 400 or a connection reset — permanently
/// downgrades a perfectly healthy endpoint to the fallbacks, where JSON mode
/// sends only a `schema_to_example` template rather than the real schema.
#[tokio::test]
async fn transport_errors_do_not_count_as_missing_tool_support() {
    let server = MockServer::start_async().await;

    // Mode 1 errors outright on every call. The point is that an error is not
    // evidence about the endpoint's parser, so the mode must keep being tried —
    // not that it later recovers.
    let failing_tools = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("\"tools\"")
                .body_excludes("json_object");
            then.status(400)
                .header("content-type", "application/json")
                .body(r#"{"error":{"message":"bad gateway moment"}}"#);
        })
        .await;
    let json_fallback = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("json_object");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{"id":"x","object":"chat.completion","created":1,"model":"m",
                        "choices":[{"index":0,"message":{"role":"assistant",
                            "content":"{\"foo\":\"bar\"}"},
                          "finish_reason":"stop"}]}"#,
                );
        })
        .await;

    // Legacy mode sits between the two and must be mocked or it 404s. It
    // answers 200 with no `function_call`, i.e. unusable here.
    let _legacy = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("\"functions\"")
                .body_excludes("json_object");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{"id":"x","object":"chat.completion","created":1,"model":"m",
                        "choices":[{"index":0,"message":{"role":"assistant",
                            "content":"prose"},"finish_reason":"stop"}]}"#,
                );
        })
        .await;

    let adapter = OpenAIAdapter::new("gpt-4o-mini", "test-key", Some(server.base_url()))
        .unwrap()
        .with_network_retries(0)
        .with_structured_output_retries(1);
    let schema = json!({"type":"object","properties":{"foo":{"type":"string"}}});

    for i in 0..3 {
        adapter
            .create_structured_output_raw("input text", "system prompt", &schema, None)
            .await
            .unwrap_or_else(|e| panic!("call {i} should fall through to JSON mode: {e}"));
    }
    let after_errors = failing_tools.calls_async().await;
    assert_eq!(
        after_errors, 3,
        "three tool-call attempts errored, one per call"
    );

    // The endpoint is still tried, because nothing was ever *observed* about
    // its tool-call support.
    adapter
        .create_structured_output_raw("input text", "system prompt", &schema, None)
        .await
        .expect("fourth call succeeds via JSON mode");
    assert_eq!(
        failing_tools.calls_async().await,
        after_errors + 1,
        "an errored tool-call request must not count as evidence the endpoint lacks a parser"
    );
    assert_eq!(json_fallback.calls_async().await, 4);
}

/// An endpoint that echoes usable JSON in `content` is *answered* by mode 1 on
/// its first attempt, so mode 1 is the cheapest path there and must keep being
/// used even though no native tool call ever appears.
#[tokio::test]
async fn a_content_echoing_endpoint_keeps_using_tool_calling_mode() {
    let server = MockServer::start_async().await;
    let tools_mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("\"tools\"")
                .body_excludes("json_object");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{"id":"x","object":"chat.completion","created":1,"model":"m",
                        "choices":[{"index":0,"message":{"role":"assistant",
                            "content":"{\"foo\":\"bar\"}"},
                          "finish_reason":"stop"}]}"#,
                );
        })
        .await;

    let adapter = OpenAIAdapter::new("gpt-4o-mini", "test-key", Some(server.base_url()))
        .unwrap()
        .with_network_retries(0)
        .with_structured_output_retries(2);
    let schema = json!({"type":"object","properties":{"foo":{"type":"string"}}});

    for i in 0..6 {
        adapter
            .create_structured_output_raw("input text", "system prompt", &schema, None)
            .await
            .unwrap_or_else(|e| panic!("call {i} should succeed from content: {e}"));
    }

    assert_eq!(
        tools_mock.calls_async().await,
        6,
        "mode 1 answers this endpoint in one request; it must not be skipped"
    );
}

/// `consecutive_misses` must actually be consecutive: a call the mode answered
/// has to clear the count, or misses accumulate across arbitrarily many
/// intervening successes until a mode that keeps working gets skipped anyway.
///
/// Responses are keyed on the input text so one adapter can be walked through an
/// interleaved sequence. Two unusable calls, one answered, two more unusable:
/// with the reset that is a run of two, below the threshold. Without it, four.
#[tokio::test]
async fn a_call_the_mode_answers_resets_the_consecutive_miss_count() {
    let server = MockServer::start_async().await;

    // Unusable: prose in `content`, so mode 1 exhausts and records a miss.
    let tools_prose = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("\"tools\"")
                .body_includes("UNUSABLE")
                .body_excludes("json_object");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{"id":"x","object":"chat.completion","created":1,"model":"m",
                        "choices":[{"index":0,"message":{"role":"assistant",
                            "content":"no json here"},"finish_reason":"stop"}]}"#,
                );
        })
        .await;
    // Answered: valid JSON echoed in `content`. No native tool call, but the
    // mode did its job, so the miss count must go back to zero.
    let tools_ok = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("\"tools\"")
                .body_includes("ANSWERED")
                .body_excludes("json_object");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{"id":"x","object":"chat.completion","created":1,"model":"m",
                        "choices":[{"index":0,"message":{"role":"assistant",
                            "content":"{\"foo\":\"bar\"}"},
                          "finish_reason":"stop"}]}"#,
                );
        })
        .await;
    let _legacy = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("\"functions\"")
                .body_excludes("json_object");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{"id":"x","object":"chat.completion","created":1,"model":"m",
                        "choices":[{"index":0,"message":{"role":"assistant",
                            "content":"prose"},"finish_reason":"stop"}]}"#,
                );
        })
        .await;
    let _json = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("json_object");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{"id":"x","object":"chat.completion","created":1,"model":"m",
                        "choices":[{"index":0,"message":{"role":"assistant",
                            "content":"{\"foo\":\"bar\"}"},
                          "finish_reason":"stop"}]}"#,
                );
        })
        .await;

    let adapter = OpenAIAdapter::new("gpt-4o-mini", "test-key", Some(server.base_url()))
        .unwrap()
        .with_network_retries(0)
        .with_structured_output_retries(1);
    let schema = json!({"type":"object","properties":{"foo":{"type":"string"}}});
    let ask = async |text: &str| {
        adapter
            .create_structured_output_raw(text, "system prompt", &schema, None)
            .await
    };

    ask("UNUSABLE 1").await.expect("served by JSON mode");
    ask("UNUSABLE 2").await.expect("served by JSON mode");
    // This one clears the count.
    ask("ANSWERED").await.expect("answered by mode 1");
    ask("UNUSABLE 3").await.expect("served by JSON mode");
    ask("UNUSABLE 4").await.expect("served by JSON mode");

    let before = tools_prose.calls_async().await;
    assert_eq!(tools_ok.calls_async().await, 1, "one answered call");

    // Two misses since the reset is below MISS_THRESHOLD, so mode 1 is still
    // tried. Without the reset this is the fourth miss and it would be skipped.
    ask("UNUSABLE 5").await.expect("served by JSON mode");
    assert!(
        tools_prose.calls_async().await > before,
        "a call the mode answered must reset the count, so two later misses do not trip it"
    );
}

/// The `ValidationMiss` short-circuit claims the server "clearly speaks tool
/// calling". On an endpoint that only echoes `content`, that claim is false, so
/// an incomplete payload must fall through to the fallbacks instead of erroring.
///
/// Before the gate, the same request failed three times and then succeeded once
/// the probe tripped — byte-identical input, two different outcomes.
#[tokio::test]
async fn an_incomplete_content_payload_falls_through_instead_of_erroring() {
    let server = MockServer::start_async().await;
    // Mode 1 echoes an object missing the required `type` field.
    let _tools = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("\"tools\"")
                .body_excludes("json_object");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{"id":"x","object":"chat.completion","created":1,"model":"m",
                        "choices":[{"index":0,"message":{"role":"assistant",
                            "content":"{\"name\":\"Alice\"}"},
                          "finish_reason":"stop"}]}"#,
                );
        })
        .await;
    // JSON mode returns the complete object.
    let _json = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("json_object");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{"id":"x","object":"chat.completion","created":1,"model":"m",
                        "choices":[{"index":0,"message":{"role":"assistant",
                            "content":"{\"name\":\"Alice\",\"type\":\"Person\"}"},
                          "finish_reason":"stop"}]}"#,
                );
        })
        .await;

    // Legacy mode sits between the two and must be mocked or it 404s. It
    // answers 200 with no `function_call`, i.e. unusable here.
    let _legacy = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("\"functions\"")
                .body_excludes("json_object");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{"id":"x","object":"chat.completion","created":1,"model":"m",
                        "choices":[{"index":0,"message":{"role":"assistant",
                            "content":"prose"},"finish_reason":"stop"}]}"#,
                );
        })
        .await;

    let adapter = OpenAIAdapter::new("gpt-4o-mini", "test-key", Some(server.base_url()))
        .unwrap()
        .with_network_retries(0)
        .with_structured_output_retries(1);

    // Every call must behave the same way, including the first — the outcome
    // must not depend on how many calls came before it.
    for i in 0..5 {
        let node = adapter
            .create_structured_output::<Node>("input text", "system prompt", None)
            .await
            .unwrap_or_else(|e| panic!("call {i} must fall through to JSON mode, got: {e}"));
        assert_eq!(node.r#type, "Person", "call {i} resolved via JSON mode");
    }
}

/// The converse: an endpoint that does answer tool calls keeps being sent them.
/// Guards against the probe tripping on a healthy provider.
#[tokio::test]
async fn tool_calls_keep_being_sent_to_an_endpoint_that_answers_them() {
    let server = MockServer::start_async().await;
    let tools_mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("\"tools\"")
                .body_excludes("json_object");
            then.status(200)
                .header("content-type", "application/json")
                .body(tool_call_response(r#"{"foo":"bar"}"#));
        })
        .await;

    let adapter = OpenAIAdapter::new("gpt-4o-mini", "test-key", Some(server.base_url()))
        .unwrap()
        .with_network_retries(0)
        .with_structured_output_retries(2);

    let schema = json!({"type":"object","properties":{"foo":{"type":"string"}}});
    for i in 0..5 {
        adapter
            .create_structured_output_raw("input text", "system prompt", &schema, None)
            .await
            .unwrap_or_else(|e| panic!("call {i} should succeed: {e}"));
    }

    assert_eq!(
        tools_mock.calls_async().await,
        5,
        "a working tool-call endpoint must be sent exactly one tools request per call"
    );
}
