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
//! - that the first probe for a schema is single-flighted, which is what makes
//!   that per-schema claim true under cognify's concurrency rather than only for
//!   a serial caller;
//! - that a model *refusal* is neither an empty answer nor a reason to demote;
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
                .body_includes("\"required\":[\"foo\",\"bar\"]")
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
async fn auto_sends_a_constrained_request_first() {
    let server = MockServer::start_async().await;
    let strict = strict_mock(&server, 200, USABLE).await;
    let non_strict = non_strict_mock(&server, 200, USABLE).await;
    let tools = tools_mock(&server, USABLE).await;

    let result = adapter(&server, StructuredOutputMode::Auto)
        .create_structured_output_raw("input text", "system prompt", &schema(), None)
        .await;

    assert!(result.is_ok(), "auto answers: {result:?}");
    assert_eq!(
        strict.calls_async().await,
        1,
        "auto starts with strict json_schema"
    );
    assert_eq!(
        non_strict.calls_async().await,
        0,
        "strict answered, no demotion"
    );
    assert_eq!(tools.calls_async().await, 0, "the cascade is not reached");
}

#[tokio::test]
async fn the_schema_title_names_the_request_in_every_mode() {
    let server = MockServer::start_async().await;
    let mut titled = schema();
    titled["title"] = json!("Person");
    // Both constrained rungs carry the title and are refused, so the cascade runs.
    let constrained = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("\"json_schema\"")
                .body_includes("\"name\":\"Person\"");
            then.status(501).body("{}");
        })
        .await;
    let tools = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("\"tools\"")
                .body_includes("\"function\":{\"name\":\"Person\"}");
            then.status(200)
                .header("content-type", "application/json")
                .body(USABLE);
        })
        .await;

    let result = adapter(&server, StructuredOutputMode::Auto)
        .create_structured_output_raw("input text", "system prompt", &titled, None)
        .await;

    assert!(result.is_ok(), "{result:?}");
    assert_eq!(
        constrained.calls_async().await,
        2,
        "strict, then non-strict"
    );
    assert_eq!(
        tools.calls_async().await,
        1,
        "the tool and its forced choice"
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
async fn an_unusable_answer_leaves_the_mode_at_once() {
    // A refusal (like an empty, unparseable or invalid answer) is not
    // re-asked in constrained-decoding mode even with retries to spare: the
    // cascade takes over after one request. Needs `structured_output_retries`
    // above 1, or the mode would fall through after one attempt anyway.
    let server = MockServer::start_async().await;
    let refused = r#"{"id":"x","object":"chat.completion","created":1,"model":"m",
        "choices":[{"index":0,"message":{"role":"assistant","content":null,
            "refusal":"I will not do that."},
          "finish_reason":"stop"}]}"#;
    let constrained = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("\"json_schema\"");
            then.status(200)
                .header("content-type", "application/json")
                .body(refused);
        })
        .await;
    let tools = tools_mock(&server, USABLE).await;
    let llm = OpenAIAdapter::new("gpt-4o-mini", "test-key", Some(server.base_url()))
        .unwrap()
        .with_network_retries(0)
        .with_structured_output_retries(2)
        .with_structured_output_mode(StructuredOutputMode::JsonSchema);

    let result = llm
        .create_structured_output_raw("input text", "system prompt", &schema(), None)
        .await;

    assert_eq!(result.unwrap(), json!({"foo": "bar"}));
    assert_eq!(constrained.calls_async().await, 1, "no constrained re-ask");
    assert_eq!(tools.calls_async().await, 1, "the cascade answered");
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_callers_share_one_probe() {
    // Cognify extracts chunks concurrently through one shared adapter with one
    // shared schema. Without single-flighting the first probe, every task in the
    // first wave reads "not yet tried" and walks its own ladder, so a refusing
    // endpoint takes two rejections *per task* rather than two in total — and
    // the per-schema claim in the docs would be false under the exact workload
    // the feature is for.
    //
    // The delay makes the race deterministic: it holds the first probe on the
    // wire long enough that the other tasks are guaranteed to arrive while the
    // reservation is held.
    //
    // The probe **succeeds** here rather than 501ing, which is what makes this a
    // real test. Against a refusing endpoint, "one strict request" is also what
    // six *serialised* calls would produce — the first records `Unsupported` and
    // the rest skip — so the assertion would hold with no single-flighting at
    // all. With a succeeding probe the three outcomes separate: six strict
    // requests means no coordination (every task sent its own), one strict plus
    // five cascade calls means the reservation held, and anything else means the
    // tasks never actually overlapped.
    let server = MockServer::start_async().await;
    let strict = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("\"json_schema\"")
                .body_includes("\"strict\":true");
            then.status(200)
                .header("content-type", "application/json")
                .delay(std::time::Duration::from_millis(300))
                .body(USABLE);
        })
        .await;
    let non_strict = non_strict_mock(&server, 200, USABLE).await;
    let tools = tools_mock(&server, USABLE).await;
    let llm = std::sync::Arc::new(adapter(&server, StructuredOutputMode::JsonSchema));

    let mut tasks = Vec::new();
    for _ in 0..6 {
        let llm = std::sync::Arc::clone(&llm);
        tasks.push(tokio::spawn(async move {
            llm.create_structured_output_raw("input text", "system prompt", &schema(), None)
                .await
        }));
    }
    for task in tasks {
        assert_eq!(
            task.await.unwrap().unwrap(),
            json!({"foo": "bar"}),
            "every concurrent caller still gets an answer",
        );
    }

    assert_eq!(
        strict.calls_async().await,
        1,
        "exactly one task probes; six would mean no single-flighting",
    );
    assert_eq!(
        non_strict.calls_async().await,
        0,
        "the probe succeeded, so the ladder never advanced",
    );
    assert_eq!(
        tools.calls_async().await,
        5,
        "the rest of the first wave uses the cascade — this is the documented \
         cost of reserving the probe, and it is paid once",
    );

    // Once the probe has landed, the answer is in the memo and every later call
    // gets constrained decoding without coordinating.
    llm.create_structured_output_raw("input text", "system prompt", &schema(), None)
        .await
        .unwrap();
    assert_eq!(
        strict.calls_async().await,
        2,
        "a later call sends strict directly",
    );
    assert_eq!(
        tools.calls_async().await,
        5,
        "and does not touch the cascade"
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_settled_schema_that_starts_being_refused_coordinates_its_second_rung() {
    // The endpoint changing under a *discovered* schema — an Azure api-version
    // rollover, a gateway upgrade. The reservation is not taken in a settled
    // state (that is what lets the steady state run at full concurrency), so
    // every in-flight caller sends the rung that has just started failing.
    //
    // That first burst is irreducible: nothing can know the endpoint changed
    // until a request comes back refused. What the per-rung reservation buys is
    // that the burst is not then *doubled* on the way down the ladder — the
    // demotion moves the level into a discovering state, and from there exactly
    // one caller continues while the rest fall through to the cascade.
    let server = MockServer::start_async().await;
    let accepting = strict_mock(&server, 200, USABLE).await;
    let tools = tools_mock(&server, USABLE).await;
    let llm = std::sync::Arc::new(adapter(&server, StructuredOutputMode::JsonSchema));

    // Settle the schema at `Strict`.
    llm.create_structured_output_raw("input text", "system prompt", &schema(), None)
        .await
        .unwrap();
    assert_eq!(accepting.calls_async().await, 1);

    // Now the endpoint stops accepting that shape.
    accepting.delete_async().await;
    let refusing = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .body_includes("\"json_schema\"")
                .body_includes("\"strict\":true");
            then.status(501)
                .header("content-type", "application/json")
                .delay(std::time::Duration::from_millis(300))
                .body("{}");
        })
        .await;
    let non_strict = non_strict_mock(&server, 501, "{}").await;

    let mut tasks = Vec::new();
    for _ in 0..6 {
        let llm = std::sync::Arc::clone(&llm);
        tasks.push(tokio::spawn(async move {
            llm.create_structured_output_raw("input text", "system prompt", &schema(), None)
                .await
        }));
    }
    for task in tasks {
        assert_eq!(task.await.unwrap().unwrap(), json!({"foo": "bar"}));
    }

    assert_eq!(
        refusing.calls_async().await,
        6,
        "the first rung's burst is irreducible — the change is only observable by sending",
    );
    assert_eq!(
        non_strict.calls_async().await,
        1,
        "but the demoted rung is coordinated, so the burst is not doubled",
    );
    assert_eq!(
        tools.calls_async().await,
        6,
        "all six are still answered; the settling call never needed the cascade",
    );
}

#[tokio::test]
async fn an_unrelated_bad_request_does_not_demote() {
    // A 400 that says nothing about the request shape — an oversized chunk, a
    // malformed message — must not be remembered as "this endpoint refuses
    // constrained decoding". That call is lost either way, but the memo is
    // monotonic and process-wide, so demoting on it would strip constrained
    // decoding from every *later* extraction in a long-lived server, including
    // the smaller chunks that would have succeeded. A silent quality
    // regression, not a lost call.
    let server = MockServer::start_async().await;
    let strict = strict_mock(
        &server,
        400,
        r#"{"error":{"message":"This model's maximum context length is 8192 tokens","code":"context_length_exceeded"}}"#,
    )
    .await;
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
        "the second call still asks for strict — nothing was learned about the shape",
    );
    assert_eq!(
        non_strict.calls_async().await,
        0,
        "and the ladder never advanced",
    );
    assert_eq!(tools.calls_async().await, 2, "each call used the cascade");
}

#[tokio::test]
async fn a_non_object_root_skips_the_mode_entirely() {
    // `response_format.json_schema` requires an object root, so a bare
    // `{"type": "string"}` — which two HTTP routes ask for — is invalid by
    // construction. Spending both rungs discovering that is pure waste, and
    // there is no endpoint behaviour to remember afterwards.
    let server = MockServer::start_async().await;
    let strict = strict_mock(&server, 200, USABLE).await;
    let non_strict = non_strict_mock(&server, 200, USABLE).await;
    let tools = tools_mock(&server, USABLE).await;

    let result = adapter(&server, StructuredOutputMode::JsonSchema)
        .create_structured_output_raw(
            "input text",
            "system prompt",
            &json!({"type": "string"}),
            None,
        )
        .await;

    assert!(result.is_ok(), "the cascade answers: {result:?}");
    assert_eq!(
        strict.calls_async().await + non_strict.calls_async().await,
        0,
        "no constrained request is sent for a schema that cannot carry one",
    );
    assert_eq!(tools.calls_async().await, 1);

    // A root that merely *has* `properties` without declaring `type: "object"`
    // is also skipped. `validate_summary_schema` accepts such a schema, and
    // `properties` alone does not assert the instance is an object — the strict
    // rewrite preserves the omission, so the probe would be refused anyway.
    // Synthesising the missing type instead would narrow a schema its author
    // chose to leave open.
    let result = adapter(&server, StructuredOutputMode::JsonSchema)
        .create_structured_output_raw(
            "input text",
            "system prompt",
            &json!({"properties": {"foo": {"type": "string"}}}),
            None,
        )
        .await;

    assert!(result.is_ok(), "the cascade answers: {result:?}");
    assert_eq!(
        strict.calls_async().await + non_strict.calls_async().await,
        0,
        "an undeclared object root is not an object root",
    );
    assert_eq!(tools.calls_async().await, 2);
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
