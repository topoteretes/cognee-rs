//! What the in-flight ceiling is allowed to count — `httpmock`, no real API.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test code: panics are acceptable"
)]
//!
//! `cognee_llm::in_flight` bounds concurrent LLM *sockets*, the analogue of the
//! connection-pool limit every Python HTTP client sets. A request waiting for
//! the pacer to admit it holds no socket, so it must hold no permit either.
//!
//! The ordering used to be the other way round — permit first, then
//! `Pacer::admit()` — on the reasoning that a permit taken after admission
//! would be "held across a pacing sleep". That has it backwards: acquiring
//! first is exactly what holds a permit across the sleep. With the pool
//! saturated and a provider overload episode open for its 900s cooldown, every
//! other LLM caller in the process would block in `acquire_in_flight()` — which
//! has no timeout — behind requests that were only sleeping.
//!
//! This test pins the ordering from the outside: a ceiling of one, a request
//! parked in the pacer, and the assertion that the one permit is still free.
//! It must own its process, because the in-flight semaphore is a first-call-wins
//! `OnceLock` — hence a test file of its own, with a single case in it.

use std::sync::Arc;
use std::time::Duration;

use cognee_llm::adapters::OpenAIAdapter;
use cognee_llm::in_flight::{acquire_in_flight, init_llm_in_flight};
use cognee_llm::{Llm, Message, MessageRole};
use cognee_utils::pacing::Pacer;
use httpmock::prelude::*;

/// The single permit this process's ceiling is installed with.
const CEILING: usize = 1;

/// Long enough that a request which loses the pacer's only token waits
/// effectively forever, so "still parked" needs no timing tolerance.
const PACING_INTERVAL: Duration = Duration::from_secs(3600);

/// How long the probe waits for the permit before calling it held.
///
/// Generous: on the correct ordering the permit is free immediately, so this
/// only ever elapses when the permit really is held by the parked request.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

fn user_msg() -> Vec<Message> {
    vec![Message {
        role: MessageRole::User,
        content: "hello".to_string(),
    }]
}

fn ok_response() -> &'static str {
    r#"{"id":"x","object":"chat.completion","created":1,"model":"m",
        "choices":[{"index":0,"message":{"role":"assistant","content":"hi"},
        "finish_reason":"stop"}]}"#
}

#[tokio::test]
async fn a_request_waiting_on_the_pacer_holds_no_in_flight_permit() {
    let semaphore = init_llm_in_flight(CEILING);
    assert_eq!(
        semaphore.available_permits(),
        CEILING,
        "this file must own the process-wide ceiling; another test in the same \
         binary installed one first"
    );

    let server = MockServer::start_async().await;
    let endpoint = server.mock(|when, then| {
        when.method(POST).path("/chat/completions");
        then.status(200)
            .header("content-type", "application/json")
            .body(ok_response());
    });

    // One token per hour, pacing on unconditionally: the first call is admitted
    // straight away and drains the bucket, the second is left owing a token it
    // will not be granted for an hour.
    let pacer = Arc::new(Pacer::new(1, PACING_INTERVAL, true, false));
    let adapter = Arc::new(
        OpenAIAdapter::new("gpt-4o-mini", "test-key", Some(server.base_url()))
            .expect("adapter builds")
            .with_pacer(pacer),
    );

    adapter
        .generate(user_msg(), None)
        .await
        .expect("the first call is admitted immediately and answered by the mock");

    let parked = tokio::spawn({
        let adapter = Arc::clone(&adapter);
        async move { adapter.generate(user_msg(), None).await }
    });

    // Let the second call reach `admit()` and block there. The mock's hit count
    // is the proof it did: a request that got past pacing would have sent.
    tokio::time::sleep(Duration::from_millis(500)).await;
    endpoint.assert_calls(1);
    assert!(
        !parked.is_finished(),
        "the second call must still be parked in the pacer for this test to mean \
         anything"
    );

    // The probe. The ceiling is one, the parked request is the only other
    // caller, and it holds no socket — so the permit must be available.
    let permit = tokio::time::timeout(PROBE_TIMEOUT, acquire_in_flight())
        .await
        .expect(
            "the in-flight permit is held by a request that is only sleeping in \
             the pacer: acquiring before `Pacer::admit()` makes the ceiling count \
             waiters as sockets, so an overload episode stalls every other LLM \
             caller in the process",
        );
    assert!(
        permit.is_some(),
        "a ceiling was installed, so the acquire must yield a real permit"
    );

    parked.abort();
}
