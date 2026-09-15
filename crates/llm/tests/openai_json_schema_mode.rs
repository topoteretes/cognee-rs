//! `LLM_STRUCTURED_OUTPUT_MODE=json_schema` — constrained decoding, and the
//! demotion ladder that makes it safe to turn on.
//!
//! The mode asks for `response_format: {"type": "json_schema", "json_schema":
//! {"strict": true, …}}`, which on a supporting backend constrains the sampler
//! itself rather than merely describing the target in the prompt. Python's
//! default `litellm_native` path sends the same request whenever litellm's model
//! table says the model supports it; Rust has no such table for an arbitrary
//! OpenAI-compatible base URL, so the knob is where the operator supplies it
//! (SDK-630).
//!
//! What these tests pin down:
//!
//! - the wire shape, including that the strict form really is the all-required /
//!   `additionalProperties: false` rewrite, which is what OpenAI requires and
//!   what the demotion ladder exists to survive;
//! - the ladder itself — strict → non-strict → out to the cascade — and that
//!   only a *shape* rejection (HTTP 400 / 501) moves it;
//! - that each step is remembered per schema, so a refusing endpoint pays one
//!   wasted request per distinct schema rather than one per call;
//! - that `auto` is untouched: no constrained request is ever sent under it.
//!
//! Two couplings worth naming, because a refactor elsewhere could break the
//! demotion silently and no other test would notice. `demotes_on_bad_request`
//! depends on a 400 arriving as `LlmError::InvalidResponse("Bad request: …")`,
//! the one arm `is_request_shape_rejection` matches by string;
//! `demotes_on_not_implemented_and_falls_through_to_the_cascade` depends on a
//! 501 being *terminal* in the transport layer, since a retried 501 would arrive
//! as `MaxRetriesExceeded`, which the cascade treats as fatal — the mode would
//! fail the call instead of falling back. Both are asserted here through
//! behaviour, not by reading the adapter's internals.
//!
//! Mock discrimination, following `openai_structured_output_mode.rs`:
//! `body_includes("\"json_schema\"")` identifies mode 0 (no other mode names it,
//! and the test schemas carry no `$schema` URL), split into two mocks by
//! `"strict":true` for the two rungs of the ladder.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test code — panics are acceptable"
)]

use cognee_llm::{Llm, OpenAIAdapter, StructuredOutputMode};
use httpmock::prelude::*;
use serde_json::json;

/// A parseable payload in `content` — what every mode here answers with.
/// Constrained decoding returns its document in `content`, exactly as JSON mode
/// does; there is no native tool-call envelope to unwrap.
const USABLE: &str = r#"{"id":"x","object":"chat.completion","created":1,"model":"m",
    "choices":[{"index":0,"message":{"role":"assistant",
        "content":"{\"foo\":\"bar\"}"},
      "finish_reason":"stop"}]}"#;

/// Two properties, one of them absent from `required`, so the strict rewrite has
/// something to do and the shape assertion below is not vacuous.
fn schema() -> serde_json::Value {
    json!({
        "type": "object",
        "required": ["foo"],
        "properties": {"foo": {"type": "string"}, "bar": {"type": "string"}},
    })
}

/// A second, structurally different schema — for the per-schema memo test.
fn other_schema() -> serde_json::Value {
    json!({"type": "object", "properties": {"baz": {"type": "string"}}})
}

fn adapter(server: &MockServer, mode: StructuredOutputMode) -> OpenAIAdapter {
    OpenAIAdapter::new("gpt-4o-mini", "test-key", Some(server.base_url()))
        .unwrap()
        .with_network_retries(0)
        .with_structured_output_retries(1)
        .with_structured_output_mode(mode)
}

/// The constrained request carrying `"strict":true`.
async fn strict_mock<'a>(server: &'a MockServer, status: u16, body: &str) -> httpmock::Mock<'a> {
    server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("\"json_schema\"")
                .body_includes("\"strict\":true");
            then.status(status)
                .header("content-type", "application/json")
                .body(body);
        })
        .await
}

/// The demoted constrained request: same envelope, no `strict`.
async fn non_strict_mock<'a>(
    server: &'a MockServer,
    status: u16,
    body: &str,
) -> httpmock::Mock<'a> {
    server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("\"json_schema\"")
                .body_excludes("\"strict\":true");
            then.status(status)
                .header("content-type", "application/json")
                .body(body);
        })
        .await
}

/// Mode 1 of the cascade — the demotion target.
async fn tools_mock<'a>(server: &'a MockServer, body: &str) -> httpmock::Mock<'a> {
    server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("\"tools\"")
                .body_excludes("\"json_schema\"");
            then.status(200)
                .header("content-type", "application/json")
                .body(body);
        })
        .await
}

#[tokio::test]
async fn sends_a_strict_constrained_request_and_nothing_else() {
    let server = MockServer::start_async().await;
    // Asserts the *content* of the strict request, not merely that one was sent:
    // `bar` was optional in the input schema and is required on the wire, and
    // every object node is closed. Without both, OpenAI rejects `strict: true`.
    // The conditions live on this mock rather than a second one because httpmock
    // routes a request to the first matching mock, so a duplicate matcher would
    // never be reached.
    let strict = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("\"type\":\"json_schema\"")
                .body_includes("\"name\":\"extract_structured_data\"")
                .body_includes("\"strict\":true")
                .body_includes("\"required\":[\"bar\",\"foo\"]")
                .body_includes("\"additionalProperties\":false");
            then.status(200)
                .header("content-type", "application/json")
                .body(USABLE);
        })
        .await;
    // The cascade would answer usefully if it were reached. The point is that it
    // is not: constrained decoding came first and worked.
    let tools = tools_mock(&server, USABLE).await;

    let result = adapter(&server, StructuredOutputMode::JsonSchema)
        .create_structured_output_raw("input text", "system prompt", &schema(), None)
        .await;

    assert_eq!(result.unwrap(), json!({"foo": "bar"}));
    assert_eq!(
        strict.calls_async().await,
        1,
        "the strict rewrite reached the wire and answered"
    );
    assert_eq!(
        tools.calls_async().await,
        0,
        "the cascade is not reached when mode 0 answers"
    );
}

#[tokio::test]
async fn auto_never_sends_a_constrained_request() {
    let server = MockServer::start_async().await;
    let strict = strict_mock(&server, 200, USABLE).await;
    let non_strict = non_strict_mock(&server, 200, USABLE).await;
    let tools = tools_mock(&server, USABLE).await;

    let result = adapter(&server, StructuredOutputMode::Auto)
        .create_structured_output_raw("input text", "system prompt", &schema(), None)
        .await;

    assert!(result.is_ok(), "auto still answers: {result:?}");
    assert_eq!(tools.calls_async().await, 1, "auto starts at the cascade");
    assert_eq!(
        strict.calls_async().await + non_strict.calls_async().await,
        0,
        "constrained decoding is opt-in — `auto` must not probe for it, because \
         the shapes that fail do so with a hard HTTP error",
    );
}

#[tokio::test]
async fn demotes_on_bad_request() {
    // A gateway that understands `response_format: json_schema` but rejects the
    // `strict` keyword — an older Azure api-version, some vLLM builds. One step
    // down the ladder is enough, and the cascade is never reached.
    //
    // This is also the test that pins the 400 → `InvalidResponse("Bad request:
    // …")` spelling the demotion recognises: reword that mapping without
    // updating `is_request_shape_rejection` and this goes red.
    let server = MockServer::start_async().await;
    let strict = strict_mock(
        &server,
        400,
        r#"{"error":{"message":"unknown parameter: strict"}}"#,
    )
    .await;
    let non_strict = non_strict_mock(&server, 200, USABLE).await;
    let tools = tools_mock(&server, USABLE).await;

    let result = adapter(&server, StructuredOutputMode::JsonSchema)
        .create_structured_output_raw("input text", "system prompt", &schema(), None)
        .await;

    assert_eq!(result.unwrap(), json!({"foo": "bar"}));
    assert_eq!(
        strict.calls_async().await,
        1,
        "the strict form is tried exactly once"
    );
    assert_eq!(
        non_strict.calls_async().await,
        1,
        "and demoted to the non-strict form"
    );
    assert_eq!(
        tools.calls_async().await,
        0,
        "which answered, so the cascade is not reached"
    );
}

#[tokio::test]
async fn demotes_on_not_implemented_and_falls_through_to_the_cascade() {
    // Baseten's `gpt-oss-120b`: HTTP 501 to any constrained request, whether or
    // not `strict` is set. Both rungs are spent, then the ordinary cascade runs
    // and answers.
    //
    // The 501 must be terminal in the transport layer for this to pass at all —
    // retried, it would arrive as `MaxRetriesExceeded`, which the cascade treats
    // as fatal, and the call would fail instead of falling back.
    let server = MockServer::start_async().await;
    let strict = strict_mock(&server, 501, r#"{"error":"Error making prediction"}"#).await;
    let non_strict = non_strict_mock(&server, 501, r#"{"error":"Error making prediction"}"#).await;
    let tools = tools_mock(&server, USABLE).await;

    let result = adapter(&server, StructuredOutputMode::JsonSchema)
        .create_structured_output_raw("input text", "system prompt", &schema(), None)
        .await;

    assert_eq!(
        result.unwrap(),
        json!({"foo": "bar"}),
        "a refusing endpoint still gets an answer — the mode is a preference, not a pin",
    );
    assert_eq!(strict.calls_async().await, 1);
    assert_eq!(non_strict.calls_async().await, 1);
    assert_eq!(tools.calls_async().await, 1, "the cascade took over");
}

#[tokio::test]
async fn remembers_the_demotion_per_schema() {
    let server = MockServer::start_async().await;
    let strict = strict_mock(&server, 501, "{}").await;
    let non_strict = non_strict_mock(&server, 501, "{}").await;
    let tools = tools_mock(&server, USABLE).await;
    // One adapter across all three calls: the memo lives on the endpoint, shared
    // across clones, so it must survive from one call to the next.
    let llm = adapter(&server, StructuredOutputMode::JsonSchema);

    for _ in 0..2 {
        llm.create_structured_output_raw("input text", "system prompt", &schema(), None)
            .await
            .unwrap();
    }

    assert_eq!(
        strict.calls_async().await,
        1,
        "the second call does not re-probe"
    );
    assert_eq!(non_strict.calls_async().await, 1);
    assert_eq!(
        tools.calls_async().await,
        2,
        "both calls were answered by the cascade"
    );

    // A *different* schema is probed on its own account: a refusal can be about
    // one schema's grammar rather than the endpoint, and an endpoint-wide flag
    // would throw away constrained decoding for every schema that works.
    llm.create_structured_output_raw("input text", "system prompt", &other_schema(), None)
        .await
        .unwrap();
    assert_eq!(
        strict.calls_async().await,
        2,
        "a new schema gets its own probe"
    );
}

#[tokio::test]
async fn a_refusal_is_not_an_empty_answer_and_does_not_demote() {
    // Structured outputs answer a declined request with `refusal` and a null
    // `content`. Two things must hold: the response still deserializes (the
    // field has to exist on the message struct, or serde sees only a null
    // content), and a refusal is about the *prompt*, not the request shape — so
    // the ladder must not move, and the next call still asks for strict.
    let server = MockServer::start_async().await;
    let refused = r#"{"id":"x","object":"chat.completion","created":1,"model":"m",
        "choices":[{"index":0,"message":{"role":"assistant","content":null,
            "refusal":"I can't help with that."},
          "finish_reason":"stop"}]}"#;
    let strict = strict_mock(&server, 200, refused).await;
    let non_strict = non_strict_mock(&server, 200, USABLE).await;
    let tools = tools_mock(&server, USABLE).await;
    let llm = adapter(&server, StructuredOutputMode::JsonSchema);

    let result = llm
        .create_structured_output_raw("input text", "system prompt", &schema(), None)
        .await;

    assert_eq!(
        result.unwrap(),
        json!({"foo": "bar"}),
        "the cascade still answers after the refusal exhausts mode 0",
    );
    assert_eq!(strict.calls_async().await, 1);
    assert_eq!(
        non_strict.calls_async().await,
        0,
        "a refusal is not evidence about the request shape",
    );
    assert_eq!(tools.calls_async().await, 1);

    llm.create_structured_output_raw("input text", "system prompt", &schema(), None)
        .await
        .unwrap();
    assert_eq!(
        strict.calls_async().await,
        2,
        "the memo was left alone, so the next call still asks for strict",
    );
}

#[tokio::test]
async fn a_non_shape_error_does_not_demote() {
    // 401 is terminal too, but it says nothing about whether the endpoint can
    // parse a constrained request — it never got that far. Demoting on it would
    // disable constrained decoding for the life of the process over a credential
    // blip, so the call falls through to the cascade with the memo untouched and
    // the next call probes again.
    let server = MockServer::start_async().await;
    let strict = strict_mock(&server, 401, r#"{"error":"invalid api key"}"#).await;
    let non_strict = non_strict_mock(&server, 200, USABLE).await;
    let tools = tools_mock(&server, USABLE).await;
    let llm = adapter(&server, StructuredOutputMode::JsonSchema);

    for _ in 0..2 {
        llm.create_structured_output_raw("input text", "system prompt", &schema(), None)
            .await
            .unwrap();
    }

    assert_eq!(
        strict.calls_async().await,
        2,
        "still the strict form on the second call"
    );
    assert_eq!(
        non_strict.calls_async().await,
        0,
        "the ladder never advanced — a 401 is not evidence about the request shape",
    );
    assert_eq!(
        tools.calls_async().await,
        2,
        "each call fell through to the cascade"
    );
}
