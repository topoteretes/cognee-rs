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
//! measured on. The unbounded product they cap was
//! `structured_output_retries x (network_retries + 1) x request_timeout` — five
//! hours at this adapter's own defaults. Since the retry floor landed the
//! transport ladder is no longer bounded by `network_retries + 1` either: it
//! also runs until `retry_min_elapsed` (240s by default), so the product the
//! deadline has to cap is strictly larger than that figure.
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
//!    merely the gaps between attempts;
//! 6. and — the reason that ladder was finally given the `retry_min_elapsed`
//!    floor every other adapter has — the budget still outranks that floor, so
//!    a "keep retrying for at least this long" guarantee cannot run away here.

use std::time::Duration;

use cognee_llm::adapters::bedrock::BedrockAdapter;
use cognee_llm::adapters::bedrock::aws::env::AwsInputs;
use cognee_llm::adapters::bedrock::converse::encode_model_id;
use cognee_llm::error::LlmError;
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

    // Persistent 500s: `call_converse`'s ladder backs off 8-128s between
    // attempts and stops only once BOTH its floors are met, so a generous
    // `network_retries` — never mind the 240s default time floor — keeps it
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

/// A deadline abort must stay a `Timeout` even when the structured loop has no
/// second attempt to fall into.
///
/// The transport `Timeout` used to land in the retryable arm of
/// `structured_output_impl`, which had two consequences: the adapter's own
/// control-plane message was spliced into the prompt as a corrective
/// instruction, and with `structured_output_retries == 1` — what
/// `LLM_MAX_RETRIES=0` floors to — the loop then fell out to
/// `MaxRetriesExceeded`, so callers classifying on the timeout variant saw the
/// wrong one. The sibling test above passes either way because its second
/// attempt re-derives the deadline at the loop head; only a single-attempt loop
/// exposes it. Caught reviewing PR #215.
#[tokio::test]
async fn a_deadline_abort_stays_a_timeout_with_a_single_structured_attempt() {
    let server = MockServer::start_async().await;

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

    let err = BedrockAdapter::new(SONNET, Some(BEARER_KEY), None, &aws)
        .await
        .expect("adapter builds offline under bearer auth")
        // What `LLM_MAX_RETRIES=0` becomes: floored to a single attempt, so a
        // transport abort has no re-ask to be reclassified by.
        .with_structured_output_retries(0)
        .with_network_retries(10)
        .with_request_deadline(Some(Duration::from_millis(120)))
        .create_structured_output_with_messages_raw(vec![Message::user("who?")], &schema(), None)
        .await
        .expect_err("a spent budget must fail the call");

    // On the VARIANT, not the rendered string. `MaxRetriesExceeded` interpolates
    // its inner error, so a wrapped deadline abort still renders the substring
    // "Timeout" and a `to_string().contains(..)` assertion passes either way —
    // which is exactly how the first draft of this test managed to pass against
    // the bug it was written to catch.
    assert!(
        matches!(err, LlmError::Timeout(_)),
        "a deadline abort must surface as the Timeout variant regardless of how \
         many structured attempts are configured, so callers can classify on it; \
         got: {err:?}"
    );
    assert!(
        err.to_string().contains("LLM_REQUEST_DEADLINE_SECONDS"),
        "and must still name the knob that produced it; got: {err}"
    );
    assert!(
        endpoint.calls_async().await >= 1,
        "the call must actually have been dispatched"
    );
}

// ── The transport retry floor, and the deadline that bounds it ──────────────
//
// `LLM_MIN_RETRY_SECONDS` reached every adapter except this one: its ladder was
// a plain attempt count, so a throttle window the OpenAI and Anthropic adapters
// ride out for 240s surfaced here as a terminal `MaxRetriesExceeded` after ~4
// attempts — and under the default whole-run rollback that sweeps the run's
// output. The floor is only safe to add because the aggregate deadline above
// bounds it, which is why these cases live here rather than in a file of their
// own: they are as much about the deadline still winning as about the floor
// existing at all.
//
// They pay REAL backoff. Unlike the OpenAI and Anthropic ladders this one never
// reads `Retry-After` (`BedrockHttpResponse` does not even carry headers), so
// the `Retry-After: 1` trick that keeps `retry_parity.rs` quick is unavailable.
// Instead every case runs at `with_network_retries(0)` — an attempt floor of
// ONE — so a single 4-8s backoff separates "the floor did nothing" from "the
// floor bought a retry", and the assertions can stay exact.

/// A Bedrock throttle, the shape that made this matter: `ThrottlingException`
/// arrives as HTTP 429, which the ladder classifies as retryable.
fn throttled(when: httpmock::When, then: httpmock::Then) {
    when.method(POST).path(converse_path(SONNET));
    then.status(429)
        .header("content-type", "application/json")
        .body(r#"{"message":"ThrottlingException: Too many requests"}"#);
}

/// An adapter whose ladder can only be extended by the time floor.
///
/// `network_retries(0)` is an attempt floor of one — the ladder's first request
/// already satisfies it — and a single structured attempt, so the transport
/// ladder is the only thing that can issue a second request.
async fn floor_adapter(server: &MockServer) -> BedrockAdapter {
    let aws = AwsInputs {
        region: Some("eu-central-1".to_string()),
        bedrock_runtime_endpoint: Some(server.base_url()),
        ..AwsInputs::default()
    };
    BedrockAdapter::new(SONNET, Some(BEARER_KEY), None, &aws)
        .await
        .expect("adapter builds offline under bearer auth")
        .with_network_retries(0)
        .with_structured_output_retries(1)
}

/// The attempt floor is met by the very first request, so a second one can only
/// come from the elapsed floor.
///
/// Exactly two, not "at least two": the first backoff is 4-8s against a 3s
/// floor, so the ladder must stop after the attempt that backoff paid for. A
/// third request would mean the floor is being measured against the wrong clock.
#[tokio::test]
async fn the_retry_floor_keeps_the_ladder_going_past_its_attempt_floor() {
    let server = MockServer::start_async().await;
    let endpoint = server.mock_async(throttled).await;

    let adapter = floor_adapter(&server)
        .await
        .with_min_retry_elapsed(Duration::from_secs(3));

    // Wrapped because the failure mode of a floor that is not honoured *as
    // configured* — the 240s default leaking in — is a four-minute test rather
    // than a red one. Correct behaviour finishes in under 10s.
    let err = tokio::time::timeout(
        Duration::from_secs(30),
        adapter.create_structured_output_with_messages_raw(
            vec![Message::user("who?")],
            &schema(),
            None,
        ),
    )
    .await
    .expect("a 3s floor must not keep the ladder running for the 240s default")
    .expect_err("the provider only ever throttles");

    assert!(
        matches!(err, LlmError::MaxRetriesExceeded(_)),
        "an exhausted ladder must surface as MaxRetriesExceeded, not as a \
         deadline cut; got: {err:?}"
    );
    assert_eq!(
        endpoint.calls_async().await,
        2,
        "the attempt floor (network_retries 0 => one attempt) was already met by \
         the first request, so the second can only have come from the 3s elapsed \
         floor — the guarantee that carries a call through a provider's throttle \
         window instead of failing the item and rolling the whole run back"
    );
}

/// `LLM_MIN_RETRY_SECONDS=0` is a documented escape hatch, and must reduce the
/// ladder to exactly the attempt count it was before the floor existed.
#[tokio::test]
async fn a_zero_retry_floor_leaves_the_ladder_a_plain_attempt_count() {
    let server = MockServer::start_async().await;
    let endpoint = server.mock_async(throttled).await;

    let adapter = floor_adapter(&server)
        .await
        .with_min_retry_elapsed(Duration::ZERO);

    let err = tokio::time::timeout(
        Duration::from_secs(20),
        adapter.create_structured_output_with_messages_raw(
            vec![Message::user("who?")],
            &schema(),
            None,
        ),
    )
    .await
    .expect("a zero floor must stop the ladder at once, not back off at all")
    .expect_err("the provider only ever throttles");

    assert!(
        matches!(err, LlmError::MaxRetriesExceeded(_)),
        "got: {err:?}"
    );
    assert_eq!(
        endpoint.calls_async().await,
        1,
        "with the floor disabled the attempt count is the whole stop condition, \
         and network_retries(0) is one attempt"
    );
}

/// The floor is a "keep retrying" guarantee with no upper bound of its own; the
/// aggregate deadline is what bounds it. That ordering is the entire reason a
/// floor is safe to add to this adapter, so it is pinned rather than assumed.
#[tokio::test]
async fn the_aggregate_deadline_still_outranks_the_retry_floor() {
    let server = MockServer::start_async().await;
    let endpoint = server.mock_async(throttled).await;

    let adapter = floor_adapter(&server)
        .await
        // Ten minutes of floor against half a second of budget: nothing but the
        // deadline can end this ladder.
        .with_min_retry_elapsed(Duration::from_secs(600))
        .with_request_deadline(Some(Duration::from_millis(500)));

    let started = std::time::Instant::now();
    let err = adapter
        .create_structured_output_with_messages_raw(vec![Message::user("who?")], &schema(), None)
        .await
        .expect_err("the provider only ever throttles");

    let elapsed = started.elapsed();
    // 3s, not a looser bound: the first backoff is 4-8s (equal jitter on the 8s
    // base), so anything at or above 4s means the top-of-loop guard slept the
    // whole ladder backoff instead of clamping it to the ~500ms that was left.
    // A 10s bound passes either way and would leave `delay.min(remaining)`
    // untested — the clamp is half of "the deadline outranks the floor", since
    // without it a single 128s backoff blows the budget on its own. Correct
    // behaviour here is two requests and one ~500ms sleep.
    assert!(
        elapsed < Duration::from_secs(3),
        "the deadline must cut the ladder near its budget rather than serve out \
         the 600s floor — and must clamp the backoff to the budget rather than \
         sleep the full 4-8s ladder gap; took {elapsed:?}"
    );
    // On the VARIANT: `MaxRetriesExceeded` interpolates its inner error, so a
    // wrapped timeout still renders the substring "Timeout".
    assert!(
        matches!(err, LlmError::Timeout(_)),
        "a deadline that outranks the floor must surface as Timeout so callers \
         can classify on it; got: {err:?}"
    );
    // A range, not `== 2`: the deadline instant is taken at
    // `structured_output_impl` entry, so on a loaded runner the first
    // round-trip plus signing can itself exceed the 500ms budget and the
    // top-of-loop guard abandons after one request. Both outcomes prove the
    // deadline outranks the floor — the ordering this test exists for — and
    // widening the budget instead would make the 4-8s first backoff
    // indistinguishable, which is the other half of what the elapsed bound
    // above pins.
    let calls = endpoint.calls_async().await;
    assert!(
        (1..=2).contains(&calls),
        "the top-of-loop guard clamps the first backoff to what is left of the \
         budget and lets the attempt it slept for run, then abandons: one \
         attempt plus at most the clamped retry, never the 600s floor's worth; \
         got {calls}"
    );
}
