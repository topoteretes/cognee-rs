//! `httpmock` integration tests for the aggregate structured-output deadline on
//! the **native Anthropic** adapter (no real API calls).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test code: panics are acceptable"
)]
//!
//! The sibling of `openai_request_deadline.rs`. Until SDK-624 this adapter had
//! no `with_request_deadline` and no `with_http_timeouts` at all — the component
//! factory had nothing to call — so `LLM_REQUEST_DEADLINE_SECONDS`,
//! `LLM_REQUEST_TIMEOUT_SECONDS` and `LLM_CONNECT_TIMEOUT_SECONDS` were silently
//! inert on `LLM_PROVIDER=anthropic` while working on the OpenAI-compatible
//! path. The unbounded product they were supposed to cap is
//! `structured_output_retries x (network ladder, whose time floor keeps it
//! retrying) x request_timeout`.
//!
//! These pin the bound that replaces it:
//!
//! 1. a call that has already spent its budget stops instead of buying another
//!    re-ask, and says so in terms an operator can act on;
//! 2. the budget is opt-in, so constructing an adapter directly does not
//!    silently acquire a time limit;
//! 3. a budget large enough not to bind leaves a successful call alone;
//! 4. `0` on the HTTP timeouts means "no limit", not "fail instantly";
//! 5. the budget bounds the *transport* retry ladder inside one attempt, not
//!    merely the gaps between attempts.
//!
//! Each mock responds immediately or with a few tens of milliseconds of delay,
//! so these run in milliseconds and never sleep for the production defaults.

use std::time::Duration;

use cognee_llm::{AnthropicAdapter, GenerationOptions, Llm, Message, MessageRole};
use httpmock::prelude::*;

const MODEL: &str = "claude-sonnet-4-20250514";

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

/// A well-formed reply carrying no `tool_use` block, so every attempt fails the
/// "did not contain the forced tool_use block" arm and the loop keeps re-asking
/// — the shape that made the aggregate time unbounded.
fn no_tool_use() -> String {
    r#"{"id":"m","type":"message","role":"assistant","model":"claude-sonnet-4-20250514",
        "content":[{"type":"text","text":"thinking out loud"}],
        "stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":4}}"#
        .to_string()
}

fn complete_tool_use() -> String {
    r#"{"id":"m","type":"message","role":"assistant","model":"claude-sonnet-4-20250514",
        "content":[{"type":"tool_use","name":"extract_structured_data",
                    "input":{"name":"ok"}}],
        "stop_reason":"tool_use","usage":{"input_tokens":10,"output_tokens":6}}"#
        .to_string()
}

/// Adapter with the retry ladder wound right down, so the test measures the
/// deadline and not the ladder.
fn adapter(base_url: String) -> AnthropicAdapter {
    AnthropicAdapter::new(MODEL, "test-key", Some(base_url))
        .expect("construct AnthropicAdapter")
        .with_network_retries(0)
        .with_structured_output_retries(3)
        .with_min_retry_elapsed(Duration::ZERO)
}

#[tokio::test]
async fn spent_budget_stops_the_re_ask_loop_with_an_actionable_error() {
    let server = MockServer::start_async().await;

    // Every attempt returns unusable output *and* takes long enough that the
    // budget below is spent after the first one.
    let endpoint = server
        .mock_async(|when, then| {
            when.method(POST).path("/messages");
            then.status(200)
                .header("content-type", "application/json")
                .delay(Duration::from_millis(120))
                .body(no_tool_use());
        })
        .await;

    let err = adapter(server.base_url())
        .with_request_deadline(Some(Duration::from_millis(50)))
        .create_structured_output_with_messages_raw(user_msg(), &schema(), None)
        .await
        .expect_err("a call past its budget must fail rather than keep re-asking");

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

    // The deadline stops *starting* new work. The first attempt is dispatched
    // (elapsed is ~0, inside the budget), its 120ms overruns the 50ms budget,
    // and the next attempt's head check cuts the call — so exactly one request
    // reaches the server, against the 3 the re-ask loop would have issued.
    // Asserted exactly rather than as an upper bound: a looser check would still
    // pass if the cut moved to a later attempt, which is the regression this
    // test exists to catch.
    assert_eq!(
        endpoint.calls_async().await,
        1,
        "the deadline must cut at the first attempt boundary after the budget is \
         spent; the unbounded loop would issue 3 requests"
    );
}

#[tokio::test]
async fn no_deadline_by_default_keeps_the_historical_re_ask_loop() {
    let server = MockServer::start_async().await;

    let endpoint = server
        .mock_async(|when, then| {
            when.method(POST).path("/messages");
            then.status(200)
                .header("content-type", "application/json")
                .delay(Duration::from_millis(20))
                .body(no_tool_use());
        })
        .await;

    // No `with_request_deadline` call: an adapter constructed directly must not
    // acquire a time limit it was never given. Only the component factory opts
    // in, from settings.
    //
    // Two re-asks, not the fixture's three: this is the one test here that pays
    // *real* inter-attempt backoff (8s base with equal jitter), since with no
    // deadline there is nothing to clamp the sleep to. One backoff is enough to
    // show the loop continued.
    let err = adapter(server.base_url())
        .with_structured_output_retries(2)
        .create_structured_output_with_messages_raw(user_msg(), &schema(), None)
        .await
        .expect_err("unusable output should still exhaust the re-ask budget");

    assert!(
        !err.to_string().contains("LLM_REQUEST_DEADLINE_SECONDS"),
        "an adapter with no configured budget must never report a deadline cut; \
         got: {err}"
    );
    assert!(
        endpoint.calls_async().await > 1,
        "without a deadline the loop must still spend its re-asks"
    );
}

#[tokio::test]
async fn a_budget_that_does_not_bind_leaves_a_successful_call_alone() {
    let server = MockServer::start_async().await;

    server
        .mock_async(|when, then| {
            when.method(POST).path("/messages");
            then.status(200)
                .header("content-type", "application/json")
                .body(complete_tool_use());
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
            when.method(POST).path("/messages");
            then.status(200)
                .header("content-type", "application/json")
                // Long enough that a zero duration handed to the HTTP client as
                // a real timeout would abort it.
                .delay(Duration::from_millis(150))
                .body(complete_tool_use());
        })
        .await;

    // `0` is the documented "no limit" escape hatch on all three time knobs, so
    // it must behave consistently across them — and across adapters. Passing
    // `Duration::ZERO` through to reqwest would instead time every request out
    // immediately.
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

#[tokio::test]
async fn spent_budget_stops_the_transport_retry_ladder_too() {
    let server = MockServer::start_async().await;

    // Persistent 500s: the transport ladder's stop condition is a dual floor
    // (attempts AND elapsed >= min_retry_elapsed), so with a non-zero floor it
    // keeps retrying with 8-128s backoff regardless of the caller's budget.
    // Guarding only the re-ask heads would leave this ladder unbounded.
    let endpoint = server
        .mock_async(|when, then| {
            when.method(POST).path("/messages");
            then.status(500)
                .header("content-type", "application/json")
                .delay(Duration::from_millis(80))
                .body(r#"{"type":"error","error":{"type":"api_error","message":"boom"}}"#);
        })
        .await;

    let started = std::time::Instant::now();
    let err = AnthropicAdapter::new(MODEL, "test-key", Some(server.base_url()))
        .expect("construct AnthropicAdapter")
        .with_structured_output_retries(2)
        // A real retry time floor, so the ladder wants to keep going.
        // Deliberately far larger than the budget below.
        .with_min_retry_elapsed(Duration::from_secs(30))
        .with_network_retries(10)
        .with_request_deadline(Some(Duration::from_millis(120)))
        .create_structured_output_with_messages_raw(user_msg(), &schema(), None)
        .await
        .expect_err("a spent budget must stop the retry ladder");

    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(20),
        "the ladder must abandon the call near its budget rather than serve its \
         own 30s time floor; took {elapsed:?}"
    );

    let msg = err.to_string();
    assert!(
        msg.contains("Timeout"),
        "abandoning mid-ladder must surface as a timeout so it is not mistaken \
         for an upstream failure; got: {msg}"
    );

    // Bounded, not merely eventually-terminating: without the ladder guard this
    // would climb toward network_retries (10) across both re-asks.
    let calls = endpoint.calls_async().await;
    assert!(
        calls <= 4,
        "the budget must cap transport attempts; made {calls}"
    );
}
