//! `LLM_STRUCTURED_OUTPUT_MODE` — pinning the structured-output request shape.
//!
//! The adapter's default is a three-mode cascade (native `tools` → legacy
//! `functions` → `response_format: json_object`) because nothing in the
//! OpenAI-compatible protocol says which shape a server understands. On an
//! endpoint that answers none of them — a vLLM deployment started without
//! `--enable-auto-tool-choice --tool-call-parser` — every call pays all three.
//!
//! These tests pin down what a pin actually does:
//!
//! - only the pinned shape reaches the wire, and the other two are never sent;
//! - a pin bypasses the miss probe, so the one permitted mode is still attempted
//!   after the probe would have tripped (otherwise a pinned adapter would stop
//!   sending anything at all);
//! - exhausting a pinned mode fails naming the pin, rather than reporting a
//!   generic cascade exhaustion that sends the reader hunting a provider fault;
//! - `auto` is untouched — the cascade still cascades.
//!
//! Mock discrimination follows `openai_structured_output.rs`:
//! `body_includes("\"tools\"")` + `body_excludes` for the other two shapes
//! identifies mode 1, `body_includes("\"functions\"")` mode 2, and
//! `body_includes("json_object")` mode 3.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test code — panics are acceptable"
)]

use cognee_llm::{Llm, OpenAIAdapter, StructuredOutputMode};
use httpmock::prelude::*;
use serde_json::json;

/// Prose in `content`, no `tool_calls` — what a parser-less server returns.
const PROSE: &str = r#"{"id":"x","object":"chat.completion","created":1,"model":"m",
    "choices":[{"index":0,"message":{"role":"assistant",
        "content":"Sure! Here is the node you asked for."},
      "finish_reason":"stop"}]}"#;

/// A parseable payload echoed in `content`. Accepted by the tool-calling and
/// JSON modes, which both fall back to `content`.
const USABLE: &str = r#"{"id":"x","object":"chat.completion","created":1,"model":"m",
    "choices":[{"index":0,"message":{"role":"assistant",
        "content":"{\"foo\":\"bar\"}"},
      "finish_reason":"stop"}]}"#;

/// A payload the legacy mode accepts. Unlike the tool-calling path, legacy
/// `functions` requires a *native* `function_call` — JSON echoed in `content`
/// does not satisfy it — so a legacy-answering server needs its own fixture.
const USABLE_LEGACY: &str = r#"{"id":"x","object":"chat.completion","created":1,"model":"m",
    "choices":[{"index":0,"message":{"role":"assistant",
        "function_call":{"name":"extract","arguments":"{\"foo\":\"bar\"}"}},
      "finish_reason":"function_call"}]}"#;

fn schema() -> serde_json::Value {
    json!({"type":"object","properties":{"foo":{"type":"string"}}})
}

/// The three per-mode mocks, each answering `body`.
async fn mocks<'a>(
    server: &'a MockServer,
    tools_body: &str,
    legacy_body: &str,
    json_body: &str,
) -> (httpmock::Mock<'a>, httpmock::Mock<'a>, httpmock::Mock<'a>) {
    let tools = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("\"tools\"")
                .body_excludes("\"functions\"")
                .body_excludes("json_object");
            then.status(200)
                .header("content-type", "application/json")
                .body(tools_body);
        })
        .await;
    let legacy = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("\"functions\"")
                .body_excludes("json_object");
            then.status(200)
                .header("content-type", "application/json")
                .body(legacy_body);
        })
        .await;
    let json_mode = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("json_object");
            then.status(200)
                .header("content-type", "application/json")
                .body(json_body);
        })
        .await;
    (tools, legacy, json_mode)
}

fn adapter(server: &MockServer, mode: StructuredOutputMode) -> OpenAIAdapter {
    OpenAIAdapter::new("gpt-4o-mini", "test-key", Some(server.base_url()))
        .unwrap()
        .with_network_retries(0)
        .with_structured_output_retries(1)
        .with_structured_output_mode(mode)
}

#[tokio::test]
async fn json_pin_sends_only_json_mode() {
    let server = MockServer::start_async().await;
    // Both other modes would answer usefully if they were sent. The point is
    // that they are not: a pin is not a fallback order, it is an exclusion.
    let (tools, legacy, json_mode) = mocks(&server, USABLE, USABLE, USABLE).await;

    let result = adapter(&server, StructuredOutputMode::Json)
        .create_structured_output_raw("input text", "system prompt", &schema(), None)
        .await;

    assert!(result.is_ok(), "json mode answers: {result:?}");
    assert_eq!(tools.calls_async().await, 0, "tool-calling must not be sent");
    assert_eq!(legacy.calls_async().await, 0, "legacy must not be sent");
    assert_eq!(
        json_mode.calls_async().await,
        1,
        "json mode is the only send"
    );
}

#[tokio::test]
async fn tools_pin_sends_only_tool_calling() {
    let server = MockServer::start_async().await;
    let (tools, legacy, json_mode) = mocks(&server, USABLE, USABLE, USABLE).await;

    let result = adapter(&server, StructuredOutputMode::Tools)
        .create_structured_output_raw("input text", "system prompt", &schema(), None)
        .await;

    assert!(result.is_ok(), "tool mode answers: {result:?}");
    assert_eq!(tools.calls_async().await, 1, "tool-calling is the only send");
    assert_eq!(legacy.calls_async().await, 0, "legacy must not be sent");
    assert_eq!(
        json_mode.calls_async().await,
        0,
        "json mode must not be sent"
    );
}

#[tokio::test]
async fn functions_pin_sends_only_legacy() {
    let server = MockServer::start_async().await;
    let (tools, legacy, json_mode) = mocks(&server, USABLE, USABLE_LEGACY, USABLE).await;

    let result = adapter(&server, StructuredOutputMode::Functions)
        .create_structured_output_raw("input text", "system prompt", &schema(), None)
        .await;

    assert!(result.is_ok(), "legacy mode answers: {result:?}");
    assert_eq!(tools.calls_async().await, 0, "tool-calling must not be sent");
    assert_eq!(legacy.calls_async().await, 1, "legacy is the only send");
    assert_eq!(
        json_mode.calls_async().await,
        0,
        "json mode must not be sent"
    );
}

#[tokio::test]
async fn an_exhausted_pin_names_the_pin_and_never_falls_through() {
    let server = MockServer::start_async().await;
    // Tool calling is unanswerable; JSON mode *would* work. Under `auto` the
    // call would succeed via the cascade — under a pin it must fail instead,
    // and say why, or the operator has no way to tell a misconfigured pin from
    // a broken provider.
    let (tools, legacy, json_mode) = mocks(&server, PROSE, USABLE, USABLE).await;

    let err = adapter(&server, StructuredOutputMode::Tools)
        .create_structured_output_raw("input text", "system prompt", &schema(), None)
        .await
        .expect_err("a pinned mode that cannot answer must fail, not fall through");

    let msg = err.to_string();
    assert!(
        msg.contains("tools") && msg.contains("LLM_STRUCTURED_OUTPUT_MODE"),
        "the error must name the pin and the knob that set it, got: {msg}"
    );
    assert!(
        tools.calls_async().await >= 1,
        "the pinned mode was attempted"
    );
    assert_eq!(legacy.calls_async().await, 0, "no fall-through to legacy");
    assert_eq!(json_mode.calls_async().await, 0, "no fall-through to json");
}

#[tokio::test]
async fn a_pin_keeps_sending_after_the_probe_would_have_tripped() {
    let server = MockServer::start_async().await;
    // Tool calling never answers, which is exactly what trips the miss probe
    // under `auto`. Pinned, the probe must not apply: skipping the only
    // permitted mode would mean sending no request at all and failing every
    // later call without ever contacting the provider.
    let (tools, legacy, json_mode) = mocks(&server, PROSE, USABLE, USABLE).await;
    let pinned = adapter(&server, StructuredOutputMode::Tools);

    // Well past ModeProbe::MISS_THRESHOLD (3).
    for i in 0..6 {
        let _ = pinned
            .create_structured_output_raw("input text", "system prompt", &schema(), None)
            .await;
        assert_eq!(
            tools.calls_async().await,
            i + 1,
            "call {i} must still send the pinned mode",
        );
    }
    assert_eq!(legacy.calls_async().await, 0, "still no legacy");
    assert_eq!(json_mode.calls_async().await, 0, "still no json");
}

#[tokio::test]
async fn auto_still_cascades() {
    let server = MockServer::start_async().await;
    // Regression guard on the default: this knob must be opt-in, so an adapter
    // left at `auto` behaves exactly as it did before the knob existed —
    // tool calling attempted, then fall-through until something answers.
    let (tools, legacy, json_mode) = mocks(&server, PROSE, PROSE, USABLE).await;

    let result = adapter(&server, StructuredOutputMode::Auto)
        .create_structured_output_raw("input text", "system prompt", &schema(), None)
        .await;

    assert!(result.is_ok(), "the cascade reaches json mode: {result:?}");
    assert!(tools.calls_async().await >= 1, "tool calling was attempted");
    assert!(legacy.calls_async().await >= 1, "legacy was attempted");
    assert!(json_mode.calls_async().await >= 1, "json mode answered");
}

#[tokio::test]
async fn the_default_adapter_is_auto() {
    let server = MockServer::start_async().await;
    // `with_structured_output_mode` is never called here: an adapter built
    // directly — tests, embedders, downstream users of the crate — must keep
    // the cascade rather than silently pinning anything.
    let (tools, _legacy, json_mode) = mocks(&server, PROSE, PROSE, USABLE).await;

    let result = OpenAIAdapter::new("gpt-4o-mini", "test-key", Some(server.base_url()))
        .unwrap()
        .with_network_retries(0)
        .with_structured_output_retries(1)
        .create_structured_output_raw("input text", "system prompt", &schema(), None)
        .await;

    assert!(result.is_ok(), "default cascades to json mode: {result:?}");
    assert!(
        tools.calls_async().await >= 1,
        "the default must still try tool calling first"
    );
    assert!(json_mode.calls_async().await >= 1, "and reach json mode");
}
