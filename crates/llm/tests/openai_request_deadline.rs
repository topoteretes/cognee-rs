//! `httpmock` integration tests for the aggregate structured-output deadline
//! (no real API calls).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test code: panics are acceptable"
)]
//!
//! The reqwest client timeout bounds one HTTP *request*. Nothing composed those
//! into a bound on one logical *call*: structured extraction cascades through
//! three request shapes (tool calls, legacy functions, JSON mode), each
//! `structured_output_retries` deep, each attempt carrying a retry budget whose
//! time floor keeps it retrying for `min_retry_elapsed` before giving up.
//! Multiplied out, the designed worst case runs past an hour — which is how a
//! single extraction burned 45 minutes while every individual request completed
//! well inside its own timeout.
//!
//! These tests pin the bound that replaces it:
//!
//! 1. a call that has already spent its budget stops instead of entering the
//!    next cascade mode, and says so in terms an operator can act on;
//! 2. the budget is opt-in — an adapter built without one keeps the historical
//!    unbounded cascade, so constructing an adapter directly does not silently
//!    acquire a time limit;
//! 3. a budget large enough not to bind does not interfere with a call that
//!    succeeds normally;
//! 4. a timeout of `0` means "no limit" rather than "fail instantly".
//!
//! Each mock server responds immediately; the elapsed time comes from a
//! deliberately tiny budget plus a real (short) delay, so these run in
//! milliseconds and never sleep for the production defaults.

use std::time::Duration;

use cognee_llm::adapters::OpenAIAdapter;
use cognee_llm::{GenerationOptions, Llm, Message, MessageRole};
use httpmock::prelude::*;

fn user_msg() -> Vec<Message> {
    vec![Message {
        role: MessageRole::User,
        content: "extract".to_string(),
    }]
}

fn schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": { "name": { "type": "string" } },
        "required": ["name"]
    })
}

/// A response with no usable output, so every mode's attempt fails and the
/// cascade keeps going — the shape that made the aggregate time unbounded.
fn blank_response() -> String {
    r#"{"id":"x","object":"chat.completion","created":1,"model":"m",
        "choices":[{"index":0,"message":{"role":"assistant","content":""},
        "finish_reason":"stop"}]}"#
        .to_string()
}

fn complete_tool_call() -> String {
    let payload = serde_json::to_string(r#"{"name": "ok"}"#).expect("string escapes");
    format!(
        r#"{{"id":"x","object":"chat.completion","created":1,"model":"m",
            "choices":[{{"index":0,"message":{{"role":"assistant","tool_calls":[
                {{"id":"c1","type":"function","function":{{
                    "name":"extract_structured_data","arguments":{payload}
                }}}}
            ]}},"finish_reason":"tool_calls"}}]}}"#
    )
}

/// Adapter with retries wound right down, so the test measures the deadline and
/// not the retry ladder.
fn adapter(base_url: String) -> OpenAIAdapter {
    OpenAIAdapter::new("gpt-4o-mini", "test-key", Some(base_url))
        .unwrap()
        .with_network_retries(0)
        .with_structured_output_retries(2)
        .with_min_retry_elapsed(Duration::ZERO)
}

#[tokio::test]
async fn spent_budget_stops_the_cascade_with_an_actionable_error() {
    let server = MockServer::start_async().await;

    // Every attempt returns unusable output *and* takes long enough that the
    // budget below is spent after the first one. Without the deadline the
    // cascade would run all three modes.
    let endpoint = server
        .mock_async(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .delay(Duration::from_millis(120))
                .body(blank_response());
        })
        .await;

    let err = adapter(server.base_url())
        .with_request_deadline(Some(Duration::from_millis(50)))
        .create_structured_output_with_messages_raw(user_msg(), &schema(), None)
        .await
        .expect_err("a call past its budget must fail rather than keep cascading");

    let msg = err.to_string();
    assert!(
        msg.contains("Timeout"),
        "the aggregate cut must surface as a timeout, not as a parse or retry \
         failure that hides why the call stopped; got: {msg}"
    );
    assert!(
        msg.contains("LLM_REQUEST_DEADLINE_SECONDS"),
        "the error must name the knob that produced it so an operator can raise \
         it without reading the source; got: {msg}"
    );

    // The point of the deadline: it stops *starting* new work. The first attempt
    // is dispatched (elapsed is ~0, inside the budget), its 120ms overruns the
    // 50ms budget, and the very next attempt check cuts the call — so exactly
    // one request reaches the server, against the 6 the full 3-mode x 2-attempt
    // cascade would have issued. Asserted exactly rather than as an upper bound:
    // a looser check would still pass if the cut moved to a later mode, which is
    // the regression this test exists to catch.
    assert_eq!(
        endpoint.calls_async().await,
        1,
        "the deadline must cut at the first attempt boundary after the budget is \
         spent; the unbounded cascade would issue 6 requests"
    );
}

#[tokio::test]
async fn no_deadline_by_default_keeps_the_historical_cascade() {
    let server = MockServer::start_async().await;

    let endpoint = server
        .mock_async(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .delay(Duration::from_millis(20))
                .body(blank_response());
        })
        .await;

    // No `with_request_deadline` call: an adapter constructed directly must not
    // acquire a time limit it was never given. Only the component factory opts
    // in, from settings.
    let err = adapter(server.base_url())
        .create_structured_output_with_messages_raw(user_msg(), &schema(), None)
        .await
        .expect_err("unusable output should still exhaust the cascade");

    assert!(
        !err.to_string().contains("LLM_REQUEST_DEADLINE_SECONDS"),
        "an adapter with no configured budget must never report a deadline cut; \
         got: {err}"
    );
    assert!(
        endpoint.calls_async().await > 1,
        "without a deadline the cascade must still fall through its modes"
    );
}

#[tokio::test]
async fn a_budget_that_does_not_bind_leaves_a_successful_call_alone() {
    let server = MockServer::start_async().await;

    server
        .mock_async(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .body(complete_tool_call());
        })
        .await;

    let value = adapter(server.base_url())
        .with_request_deadline(Some(Duration::from_secs(30)))
        .create_structured_output_with_messages_raw(
            user_msg(),
            &schema(),
            Some(GenerationOptions::default()),
        )
        .await
        .expect("a generous budget must not interfere with a normal call");

    assert_eq!(value["name"], "ok");
}

#[tokio::test]
async fn zero_timeouts_mean_no_limit_not_instant_failure() {
    let server = MockServer::start_async().await;

    server
        .mock_async(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                // Long enough that a zero duration handed to the HTTP client as
                // a real timeout would abort it.
                .delay(Duration::from_millis(150))
                .body(complete_tool_call());
        })
        .await;

    // `0` is the documented "no limit" escape hatch on all three time knobs, so
    // it must behave consistently across them. Passing Duration::ZERO through to
    // reqwest would instead time every request out immediately, which is how an
    // operator generalising "0 disables it" from LLM_REQUEST_DEADLINE_SECONDS to
    // its two neighbours would silently stop all LLM traffic.
    let value = adapter(server.base_url())
        .with_http_timeouts(Duration::ZERO, Duration::ZERO)
        .create_structured_output_with_messages_raw(
            user_msg(),
            &schema(),
            Some(GenerationOptions::default()),
        )
        .await
        .expect("a zero timeout must lift the bound, not abort the request");

    assert_eq!(value["name"], "ok");
}
