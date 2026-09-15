//! The Bedrock retry floor must not be spent queueing — `httpmock`, no real API.
#![cfg(feature = "bedrock")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test code: panics are acceptable"
)]
//!
//! The Bedrock half of `retry_floor_excludes_queue_time.rs`, in a file of its
//! own for the same reason that one is: the in-flight semaphore is a
//! first-call-wins `OnceLock` installed once per process, and this case needs
//! exclusive use of its single permit.
//!
//! Until the floor existed this adapter deliberately carried no
//! `queued_for_permit` bookkeeping — there was no floor for the subtraction to
//! protect. `RetryBudget::is_exhausted` is
//! `attempts >= min_attempts && elapsed >= min_elapsed`, so a *larger* elapsed
//! can only ever end the ladder EARLIER: charging the in-flight queue wait
//! against a "keep retrying for at least this long" guarantee silently shortens
//! it, and a call that spent five minutes waiting for a permit would give up on
//! its attempt floor alone having barely retried. So the floor and the
//! subtraction had to land together, and this pins that they did.

use std::time::Duration;

use cognee_llm::adapters::bedrock::BedrockAdapter;
use cognee_llm::adapters::bedrock::aws::env::AwsInputs;
use cognee_llm::adapters::bedrock::converse::encode_model_id;
use cognee_llm::in_flight::{acquire_in_flight, init_llm_in_flight};
use cognee_llm::llm_trait::Llm;
use cognee_llm::types::Message;
use httpmock::prelude::*;

/// A bearer key short-circuits the credential ladder, so this never touches AWS.
const BEARER_KEY: &str = "test-bedrock-key";

const SONNET: &str = "eu.anthropic.claude-sonnet-4-5-20250929-v1:0";

/// One permit, so holding it is exactly "the queue is full".
const CEILING: usize = 1;

/// How long the test holds the permit before releasing it. Comfortably longer
/// than [`RETRY_FLOOR`], which is what makes the two behaviours
/// distinguishable: with queue time charged, the floor is already satisfied
/// when the first attempt starts.
const QUEUE_HOLD: Duration = Duration::from_millis(900);

/// The retry floor under test (`LLM_MIN_RETRY_SECONDS`).
const RETRY_FLOOR: Duration = Duration::from_millis(400);

#[tokio::test]
#[serial_test::serial]
async fn bedrock_does_not_spend_its_retry_floor_in_the_in_flight_queue() {
    init_llm_in_flight(CEILING);

    let server = MockServer::start_async().await;
    let endpoint = server
        .mock_async(|when, then| {
            when.method(POST)
                .path(format!("/model/{}/converse", encode_model_id(SONNET)));
            then.status(429)
                .header("content-type", "application/json")
                .body(r#"{"message":"ThrottlingException: Too many requests"}"#);
        })
        .await;

    let aws = AwsInputs {
        region: Some("eu-central-1".to_string()),
        bedrock_runtime_endpoint: Some(server.base_url()),
        ..AwsInputs::default()
    };
    let adapter = BedrockAdapter::new(SONNET, Some(BEARER_KEY), None, &aws)
        .await
        .expect("adapter builds offline under bearer auth")
        // An attempt floor of one, so the *only* thing that can keep the ladder
        // going past the first request is the elapsed floor — which is the
        // property under test.
        .with_network_retries(0)
        .with_min_retry_elapsed(RETRY_FLOOR);

    // Taken before the call starts, so its very first attempt is what queues.
    let held = acquire_in_flight().await;
    assert!(
        held.is_some(),
        "a ceiling was installed, so the acquire must yield a real permit"
    );

    let call =
        tokio::spawn(async move { adapter.generate(vec![Message::user("hello")], None).await });
    tokio::time::sleep(QUEUE_HOLD).await;
    assert!(
        !call.is_finished(),
        "the call must still be parked in the in-flight queue for this test to \
         mean anything"
    );
    endpoint.assert_calls_async(0).await;
    drop(held);

    call.await
        .expect("the request task must not panic")
        .expect_err("every attempt is throttled");

    assert_eq!(
        endpoint.calls_async().await,
        2,
        "the call made {} request(s): the {QUEUE_HOLD:?} it spent queued for an \
         in-flight permit was charged against its {RETRY_FLOOR:?} retry floor, \
         satisfying both halves of the stop condition before the first request \
         was even sent. That floor is a minimum-retry-duration guarantee rather \
         than a deadline, so counting queue time against it can only ever \
         abandon a call sooner",
        endpoint.calls_async().await,
    );
}
