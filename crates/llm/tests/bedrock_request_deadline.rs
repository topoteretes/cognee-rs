//! `httpmock` integration tests for the aggregate structured-output deadline on
//! the **native Bedrock Converse** adapter (no real API calls).
#![cfg(feature = "bedrock")]
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
//! inert on `LLM_PROVIDER=bedrock`, which is the provider the runaway was
//! measured on. The unbounded product they cap is
//! `structured_output_retries x (network_retries + 1) x request_timeout` — five
//! hours at this adapter's own defaults.
//!
//! These pin the bound that replaces it:
//!
//! 1. a call that has already spent its budget stops instead of buying another
//!    re-ask, and says so in terms an operator can act on;
//! 2. the budget is opt-in, so constructing an adapter directly does not
//!    silently acquire a time limit;
//! 3. a budget large enough not to bind leaves a successful call alone;
//! 4. `0` on the HTTP timeouts means "no limit", not "fail instantly" — and the
//!    rebuilt client still signs and reaches the endpoint, which is the part
//!    unique to this adapter, whose timeouts live on a transport it has to
//!    rebuild rather than on the adapter itself;
//! 5. the budget bounds the *transport* retry ladder inside one attempt, not
//!    merely the gaps between attempts.

use std::time::Duration;

use cognee_llm::adapters::bedrock::BedrockAdapter;
use cognee_llm::adapters::bedrock::aws::env::AwsInputs;
use cognee_llm::adapters::bedrock::converse::encode_model_id;
use cognee_llm::llm_trait::Llm;
use cognee_llm::types::{GenerationOptions, Message};
use httpmock::prelude::*;
use serde_json::{Value, json};

/// A bearer key short-circuits the §1.2 credential ladder before any lookup, so
/// these tests never touch AWS, `~/.aws` or IMDS.
const BEARER_KEY: &str = "test-bedrock-key";

/// Native structured output (caps row `anthropic.claude-sonnet-4-5-20250929-v1:0`).
const SONNET: &str = "eu.anthropic.claude-sonnet-4-5-20250929-v1:0";

fn converse_path(model: &str) -> String {
    format!("/model/{}/converse", encode_model_id(model))
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": { "name": { "type": "string" } },
        "required": ["name"]
    })
}

/// A well-formed Converse reply whose text is not a JSON object, so every
/// attempt fails validation and the loop keeps re-asking — the shape that made
/// the aggregate time unbounded. `stopReason` is a normal stop, so this is not
/// misfiled as a truncation.
fn unusable_answer() -> String {
    r#"{"output":{"message":{"content":[{"text":"no idea"}]}},
        "stopReason":"end_turn"}"#
        .to_string()
}

fn complete_answer() -> String {
    r#"{"output":{"message":{"content":[{"text":"{\"name\": \"ok\"}"}]}},
        "stopReason":"end_turn"}"#
        .to_string()
}

/// Build an adapter whose endpoint is the mock server, with the region supplied
/// explicitly so the chain stays hermetic on a machine with a populated
/// `~/.aws/config`. Retries are wound down so the tests measure the deadline and
/// not the ladder.
async fn adapter(server: &MockServer) -> BedrockAdapter {
    let aws = AwsInputs {
        region: Some("eu-central-1".to_string()),
        bedrock_runtime_endpoint: Some(server.base_url()),
        ..AwsInputs::default()
    };
    BedrockAdapter::new(SONNET, Some(BEARER_KEY), None, &aws)
        .await
        .expect("adapter builds offline under bearer auth")
        .with_network_retries(0)
        .with_structured_output_retries(3)
}

#[tokio::test]
async fn spent_budget_stops_the_re_ask_loop_with_an_actionable_error() {
    let server = MockServer::start_async().await;

    // Every attempt returns unusable output *and* takes long enough that the
    // budget below is spent after the first one.
    let endpoint = server
        .mock_async(|when, then| {
            when.method(POST).path(converse_path(SONNET));
            then.status(200)
                .header("content-type", "application/json")
                .delay(Duration::from_millis(120))
                .body(unusable_answer());
        })
        .await;

    let err = adapter(&server)
        .await
        .with_request_deadline(Some(Duration::from_millis(50)))
        .create_structured_output_with_messages_raw(vec![Message::user("who?")], &schema(), None)
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
    endpoint.assert_calls_async(1).await;
}

#[tokio::test]
async fn no_deadline_by_default_keeps_the_historical_re_ask_loop() {
    let server = MockServer::start_async().await;

    let endpoint = server
        .mock_async(|when, then| {
            when.method(POST).path(converse_path(SONNET));
            then.status(200)
                .header("content-type", "application/json")
                .body(unusable_answer());
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
    let err = adapter(&server)
        .await
        .with_structured_output_retries(2)
        .create_structured_output_with_messages_raw(vec![Message::user("who?")], &schema(), None)
        .await
        .expect_err("unusable output should still exhaust the re-ask budget");

    assert!(
        !err.to_string().contains("LLM_REQUEST_DEADLINE_SECONDS"),
        "an adapter with no configured budget must never report a deadline cut; \
         got: {err}"
    );
    endpoint.assert_calls_async(2).await;
}

#[tokio::test]
async fn a_budget_that_does_not_bind_leaves_a_successful_call_alone() {
    let server = MockServer::start_async().await;

    server
        .mock_async(|when, then| {
            when.method(POST).path(converse_path(SONNET));
            then.status(200)
                .header("content-type", "application/json")
                .body(complete_answer());
        })
        .await;

    let value = adapter(&server)
        .await
        .with_request_deadline(Some(Duration::from_secs(30)))
        .create_structured_output_with_messages_raw(
            vec![Message::user("who?")],
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
            when.method(POST).path(converse_path(SONNET));
            then.status(200)
                .header("content-type", "application/json")
                // Long enough that a zero duration handed to the HTTP client as
                // a real timeout would abort it.
                .delay(Duration::from_millis(150))
                .body(complete_answer());
        })
        .await;

    // Two things at once, because on this adapter they are the same code path:
    // `0` must mean "no limit" rather than "fail instantly", and the transport
    // rebuilt around the new client must still carry the auth resolved in
    // `new` — a rebuild that dropped it would 401 here rather than return a
    // value.
    let value = adapter(&server)
        .await
        .with_http_timeouts(Duration::ZERO, Duration::ZERO)
        .create_structured_output_with_messages_raw(
            vec![Message::user("who?")],
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

    // Persistent 500s: `call_converse`'s ladder is a plain attempt count with an
    // 8-128s backoff between attempts, so a generous `network_retries` keeps it
    // going long past the caller's budget. Guarding only the re-ask heads would
    // leave this ladder unbounded.
    let endpoint = server
        .mock_async(|when, then| {
            when.method(POST).path(converse_path(SONNET));
            then.status(500)
                .header("content-type", "application/json")
                .delay(Duration::from_millis(80))
                .body(r#"{"message":"internal failure"}"#);
        })
        .await;

    let aws = AwsInputs {
        region: Some("eu-central-1".to_string()),
        bedrock_runtime_endpoint: Some(server.base_url()),
        ..AwsInputs::default()
    };
    let started = std::time::Instant::now();
    let err = BedrockAdapter::new(SONNET, Some(BEARER_KEY), None, &aws)
        .await
        .expect("adapter builds offline under bearer auth")
        .with_structured_output_retries(2)
        .with_network_retries(10)
        .with_request_deadline(Some(Duration::from_millis(120)))
        .create_structured_output_with_messages_raw(vec![Message::user("who?")], &schema(), None)
        .await
        .expect_err("a spent budget must stop the retry ladder");

    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(20),
        "the ladder must abandon the call near its budget rather than climb its \
         own backoff schedule; took {elapsed:?}"
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
