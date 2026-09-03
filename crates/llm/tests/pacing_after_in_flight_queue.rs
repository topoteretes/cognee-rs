//! Pacing must survive the in-flight queue — `httpmock`, no real API.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test code: panics are acceptable"
)]
//!
//! `cognee_utils::pacing`'s contract is that a caller is admitted *immediately
//! before* its HTTP send. The in-flight ceiling breaks that on its own: a caller
//! clears `admit()` and then waits for a permit, and the wait is unbounded.
//!
//! Pacing is off by default, so the fast path lets a whole fan-out past the
//! pacer in one go; they then pile up on the semaphore. The first reply is a
//! 429, `record_overload` opens the 900s episode — and every caller already past
//! the pacer still fires one unpaced send at a provider that has just said it is
//! overloaded. One unpaced attempt each, which is exactly the burst that opened
//! the episode.
//!
//! The adapters therefore admit twice: once before the queue (the only place a
//! caller can be paced without holding a permit) and once after it, the second
//! taken only when the first was the free fast path so an attempt still costs
//! exactly one token.
//!
//! This test pins the second admission from the outside. It parks a call in the
//! in-flight queue while pacing is off, opens an episode and drains the bucket
//! behind its back, then frees the permit and asserts the call still does not
//! send. It must own its process, because the in-flight semaphore is a
//! first-call-wins `OnceLock` — hence a test file of its own, with a single case
//! in it.

use std::sync::Arc;
use std::time::Duration;

use cognee_llm::adapters::OpenAIAdapter;
use cognee_llm::in_flight::{acquire_in_flight, init_llm_in_flight};
use cognee_llm::{Llm, Message, MessageRole};
use cognee_utils::pacing::Pacer;
use httpmock::prelude::*;

/// The single permit this process's ceiling is installed with. One permit makes
/// "the queue is occupied" exact: the test holds it, so nothing else can send.
const CEILING: usize = 1;

/// One token per hour: once the episode is open and the test has spent the
/// bucket's only token, a caller admitted a second time waits effectively
/// forever, so "still parked" needs no timing tolerance.
const PACING_INTERVAL: Duration = Duration::from_secs(3600);

/// How long to let the call sit before checking on it. Generous: on the correct
/// ordering nothing is ever sent, so this only costs wall-clock time.
const SETTLE: Duration = Duration::from_millis(500);

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
async fn an_episode_opened_while_queued_still_paces_the_send() {
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

    // Pacing off (`LLM_RATE_LIMIT_ENABLED=false`, the default) but reactive
    // (`AUTO_RATE_LIMIT=true`), which is the configuration the failure needs:
    // the fast path admits without taking a token, and a provider 429 can still
    // open an episode afterwards.
    let pacer = Arc::new(Pacer::new(1, PACING_INTERVAL, false, true));
    let adapter = Arc::new(
        OpenAIAdapter::new("gpt-4o-mini", "test-key", Some(server.base_url()))
            .expect("adapter builds")
            .with_pacer(Arc::clone(&pacer)),
    );

    // Occupy the ceiling, so the call below gets past the pacer and then stops
    // in the queue — which is the window the bug lives in.
    let held = acquire_in_flight().await;
    assert!(
        held.is_some(),
        "a ceiling was installed, so the acquire must yield a real permit"
    );

    let queued = tokio::spawn({
        let adapter = Arc::clone(&adapter);
        async move { adapter.generate(user_msg(), None).await }
    });

    tokio::time::sleep(SETTLE).await;
    endpoint.assert_calls(0);
    assert!(
        !queued.is_finished(),
        "the call must be parked in the in-flight queue for this test to mean \
         anything"
    );

    // What a concurrent 429 does, without needing a second adapter to produce
    // it: the episode opens behind the queued caller's back. Then spend the
    // bucket's only token, so anything admitted from here on waits an hour.
    pacer.record_overload("test");
    assert!(
        pacer.admit().await,
        "the episode is open, so this admission must take the bucket's token"
    );

    // Free the queue. The call now holds a permit — and must be paced again
    // before it sends, because the pacer never saw it once the episode opened.
    drop(held);
    tokio::time::sleep(SETTLE).await;

    assert!(
        !queued.is_finished(),
        "the call cleared admission on the fast path, then waited on the \
         semaphore while an overload episode opened. Admitting only before the \
         queue lets it send unpaced at a provider that has just reported \
         overload — one such send per queued caller, which is the burst that \
         opens the episode in the first place. It must be admitted again \
         immediately before the send."
    );
    endpoint.assert_calls(0);

    queued.abort();
}
