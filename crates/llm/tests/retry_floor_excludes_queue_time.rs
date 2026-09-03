//! The retry floor must not be spent queueing — `httpmock`, no real API.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test code: panics are acceptable"
)]
//!
//! `RetryBudget::is_exhausted` is `attempts >= min_attempts && elapsed >=
//! min_elapsed`, so a *larger* elapsed can only ever end the ladder **earlier**.
//! `min_elapsed` (`LLM_MIN_RETRY_SECONDS`) is a "keep retrying for at least this
//! long" resilience guarantee, not a deadline — it is what carries a call
//! through a provider's rate-limit window — so charging the in-flight queue wait
//! against it silently weakens it. A call that spent five minutes waiting for a
//! permit would give up on its attempt floor alone, having barely retried.
//!
//! Neither the Anthropic adapter nor the Responses client has any deadline
//! concept, so for them counting queue time buys nothing at all. Both cases here
//! hold the process's only in-flight permit for longer than the whole retry
//! floor before letting the call through: were queue time counted, the first
//! attempt would already satisfy both halves of the predicate and the call would
//! stop after a single request.
//!
//! Serial, and a file of its own: the in-flight semaphore is a first-call-wins
//! `OnceLock` installed once per process, and each case needs exclusive use of
//! its single permit.

use std::time::Duration;

use cognee_llm::adapters::AnthropicAdapter;
use cognee_llm::in_flight::{acquire_in_flight, init_llm_in_flight};
use cognee_llm::{Llm, Message, MessageRole, OpenAIResponsesClient, ResponsesClient};
use httpmock::prelude::*;

/// One permit, so holding it is exactly "the queue is full".
const CEILING: usize = 1;

/// How long the test holds the permit before releasing it. Comfortably longer
/// than [`RETRY_FLOOR`], which is what makes the two behaviours distinguishable:
/// with queue time charged, the floor is already satisfied when the first
/// attempt starts.
const QUEUE_HOLD: Duration = Duration::from_millis(900);

/// The retry floor under test (`LLM_MIN_RETRY_SECONDS`).
const RETRY_FLOOR: Duration = Duration::from_millis(400);

/// Retry gap the mock asks for. `retry-after-ms` replaces the 8s exponential
/// backoff outright, which is the only reason this test is quick.
const RETRY_AFTER_MS: &str = "40";

/// Attempts the ladder must make before the elapsed floor may stop it. One, so
/// the *only* thing that can keep the loop going past the first failure is the
/// elapsed floor — which is the property under test.
const MIN_ATTEMPTS: u32 = 1;

/// A ladder that honours the floor gets roughly `RETRY_FLOOR / RETRY_AFTER_MS`
/// attempts; one that charged the queue wait against it gets exactly one.
/// Asserted well below the former so scheduling jitter cannot flip the result.
const MIN_EXPECTED_CALLS: usize = 3;

/// A retryable failure that is *not* overload evidence, so no pacing episode
/// opens and the retry gap stays the one the header asks for.
const SERVER_ERROR_BODY: &str = r#"{"error":{"message":"server error"}}"#;

fn user_msg() -> Vec<Message> {
    vec![Message {
        role: MessageRole::User,
        content: "hello".to_string(),
    }]
}

#[tokio::test]
#[serial_test::serial]
async fn anthropic_does_not_spend_its_retry_floor_in_the_in_flight_queue() {
    init_llm_in_flight(CEILING);

    let server = MockServer::start_async().await;
    let endpoint = server.mock(|when, then| {
        when.method(POST).path("/messages");
        then.status(500)
            .header("retry-after-ms", RETRY_AFTER_MS)
            .body(SERVER_ERROR_BODY);
    });

    let adapter = AnthropicAdapter::new("claude-3-5-haiku", "test-key", Some(server.base_url()))
        .expect("adapter builds")
        .with_network_retries(MIN_ATTEMPTS)
        .with_min_retry_elapsed(RETRY_FLOOR);

    // Taken before the call starts, so its very first attempt is what queues.
    let held = acquire_in_flight().await;
    assert!(
        held.is_some(),
        "a ceiling was installed, so the acquire must yield a real permit"
    );

    let call = tokio::spawn(async move { adapter.generate(user_msg(), None).await });
    tokio::time::sleep(QUEUE_HOLD).await;
    assert!(
        !call.is_finished(),
        "the call must still be parked in the in-flight queue for this test to \
         mean anything"
    );
    endpoint.assert_calls(0);
    drop(held);

    call.await
        .expect("the request task must not panic")
        .expect_err("every attempt is answered with a 500");

    assert!(
        endpoint.calls() >= MIN_EXPECTED_CALLS,
        "the call made {} attempt(s): the {QUEUE_HOLD:?} it spent queued for an \
         in-flight permit was charged against its {RETRY_FLOOR:?} retry floor. \
         That floor is a minimum-retry-duration guarantee rather than a \
         deadline, so counting queue time against it can only ever abandon a \
         call sooner — and this adapter has no deadline the earlier clock could \
         serve",
        endpoint.calls(),
    );
}

#[tokio::test]
#[serial_test::serial]
async fn responses_client_does_not_spend_its_retry_floor_in_the_in_flight_queue() {
    init_llm_in_flight(CEILING);

    let server = MockServer::start_async().await;
    let endpoint = server.mock(|when, then| {
        when.method(GET).path("/responses/resp_1");
        then.status(500)
            .header("retry-after-ms", RETRY_AFTER_MS)
            .body(SERVER_ERROR_BODY);
    });

    let client = OpenAIResponsesClient::new("test-key", Some(server.base_url()))
        .expect("client builds")
        .with_network_retries(MIN_ATTEMPTS)
        .with_min_retry_elapsed(RETRY_FLOOR);

    // Taken before the call starts, so its very first attempt is what queues.
    let held = acquire_in_flight().await;
    assert!(
        held.is_some(),
        "a ceiling was installed, so the acquire must yield a real permit"
    );

    let call = tokio::spawn(async move { client.retrieve_response("resp_1").await });
    tokio::time::sleep(QUEUE_HOLD).await;
    assert!(
        !call.is_finished(),
        "the call must still be parked in the in-flight queue for this test to \
         mean anything"
    );
    endpoint.assert_calls(0);
    drop(held);

    call.await
        .expect("the request task must not panic")
        .expect_err("every attempt is answered with a 500");

    assert!(
        endpoint.calls() >= MIN_EXPECTED_CALLS,
        "the call made {} attempt(s): the {QUEUE_HOLD:?} it spent queued for an \
         in-flight permit was charged against its {RETRY_FLOOR:?} retry floor. \
         That floor is a minimum-retry-duration guarantee rather than a \
         deadline, so counting queue time against it can only ever abandon a \
         call sooner — and this client has no deadline the earlier clock could \
         serve",
        endpoint.calls(),
    );
}
