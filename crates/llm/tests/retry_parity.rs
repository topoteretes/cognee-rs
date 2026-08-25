//! `httpmock` integration tests for the Python-parity retry/resilience stack
//! (no real API calls).
//!
//! These pin the three behaviours that changed together:
//!
//! 1. **Error classification.** Auth, unknown model, billing, and a 429 that is
//!    really an exhausted quota are terminal — one request, no retry budget
//!    burned. A plain 429 or a 5xx is transient and retried.
//! 2. **Overload evidence.** A 429/503/529 from a real dispatch path opens an
//!    episode on the injected pacer, which is what flips the process from the
//!    unbounded fast path into paced mode.
//! 3. **The dual-floor stop condition.** Attempts alone do not end the loop.
//!
//! Every adapter here is built with `with_min_retry_elapsed(Duration::ZERO)`, so
//! the time floor is satisfied immediately and the tests assert on *request
//! counts* rather than waiting out a 240s budget. The 8s→128s backoff would
//! otherwise make even a two-retry test take half a minute, so the mocks send
//! `Retry-After: 1`, which the adapters honour in place of the ladder — that
//! keeps the suite quick *and* exercises the header path rather than bypassing
//! it. (`Retry-After: 0` would not work: a zero hint is deliberately ignored,
//! since "retry immediately" from a provider that just rate-limited us is how a
//! wide burst becomes a tight loop.)
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test code: panics are acceptable"
)]

use std::sync::Arc;
use std::time::Duration;

use cognee_llm::{AnthropicAdapter, Llm, LlmExt, Message, MessageRole, OpenAIAdapter};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A minimal one-message prompt; the content is irrelevant to every assertion
/// here, which is about status handling rather than payloads.
fn prompt() -> Vec<Message> {
    vec![Message {
        role: MessageRole::User,
        content: "hello".to_string(),
    }]
}
use cognee_utils::pacing::Pacer;
use httpmock::prelude::*;

/// A pacer that never paces of its own accord, so the only thing that can turn
/// pacing on during a test is provider evidence.
fn test_pacer() -> Arc<Pacer> {
    Arc::new(Pacer::with_cooldown(
        60,
        Duration::from_secs(60),
        false, // not enabled by config
        true,  // but do react to provider evidence
        Duration::from_secs(300),
    ))
}

fn openai_adapter(server: &MockServer, pacer: &Arc<Pacer>) -> OpenAIAdapter {
    OpenAIAdapter::new("gpt-4o-mini", "sk-test", Some(server.base_url()))
        .expect("adapter builds")
        .with_network_retries(3)
        .with_min_retry_elapsed(Duration::ZERO)
        .with_pacer(Arc::clone(pacer))
}

fn anthropic_adapter(server: &MockServer, pacer: &Arc<Pacer>) -> AnthropicAdapter {
    AnthropicAdapter::new("claude-3-5-sonnet", "sk-test", Some(server.base_url()))
        .expect("adapter builds")
        .with_network_retries(3)
        .with_min_retry_elapsed(Duration::ZERO)
        .with_pacer(Arc::clone(pacer))
}

const OPENAI_OK: &str = r#"{"id":"cmpl-1","object":"chat.completion","created":1,"model":"gpt-4o-mini","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#;

// ── Transient: retried, and evidence recorded ───────────────────────────────

#[tokio::test]
async fn a_rate_limit_is_retried_and_opens_an_overload_episode() {
    let server = MockServer::start_async().await;
    let pacer = test_pacer();

    // `Retry-After: 0` keeps the test fast without disabling the code path that
    // reads the header.
    let limited = server
        .mock_async(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(429)
                .header("retry-after", "1")
                .body("{\"error\":{\"message\":\"Rate limit reached\"}}");
        })
        .await;

    let adapter = openai_adapter(&server, &pacer);
    let result = adapter.generate(prompt(), None).await;

    assert!(result.is_err(), "an exhausted budget must surface an error");
    assert!(
        limited.calls_async().await >= 2,
        "a 429 is transient and must be retried, got {} request(s)",
        limited.calls_async().await
    );
    assert!(
        pacer.is_paced(),
        "a 429 must open an overload episode so later dispatches are paced"
    );
}

#[tokio::test]
async fn a_503_opens_an_episode_too() {
    let server = MockServer::start_async().await;
    let pacer = test_pacer();

    server
        .mock_async(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(503).header("retry-after", "1").body("busy");
        })
        .await;

    let _ = openai_adapter(&server, &pacer)
        .generate(prompt(), None)
        .await;
    assert!(
        pacer.is_paced(),
        "503 is in the overload set alongside 429 and 529"
    );
}

#[tokio::test]
async fn a_successful_call_is_not_retried_and_does_not_pace() {
    let server = MockServer::start_async().await;
    let pacer = test_pacer();

    let ok = server
        .mock_async(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .body(OPENAI_OK);
        })
        .await;

    let result = openai_adapter(&server, &pacer)
        .generate(prompt(), None)
        .await;
    assert!(result.is_ok(), "expected success, got {result:?}");
    assert_eq!(ok.calls_async().await, 1, "a success must not be retried");
    assert!(
        !pacer.is_paced(),
        "a clean run must leave the process on the unbounded fast path"
    );
}

// ── Terminal: one request, no retries ───────────────────────────────────────

async fn assert_terminal(status: u16, body: &str, label: &str) {
    let server = MockServer::start_async().await;
    let pacer = test_pacer();

    let mock = server
        .mock_async(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(status).body(body);
        })
        .await;

    let result = openai_adapter(&server, &pacer)
        .generate(prompt(), None)
        .await;

    assert!(result.is_err(), "{label} must fail");
    assert_eq!(
        mock.calls_async().await,
        1,
        "{label} is terminal — retrying can never help, so exactly one request"
    );
}

#[tokio::test]
async fn authentication_failure_is_terminal() {
    assert_terminal(401, "bad key", "401").await;
}

#[tokio::test]
async fn billing_failure_is_terminal() {
    assert_terminal(402, "payment required", "402").await;
}

#[tokio::test]
async fn unknown_model_is_terminal() {
    // Regression: the OpenAI adapter previously had no 404 arm and retried it,
    // unlike the Anthropic adapter and unlike Python's terminal NotFoundError.
    assert_terminal(404, "no such model", "404").await;
}

#[tokio::test]
async fn a_429_carrying_quota_exhaustion_is_terminal_despite_the_status() {
    // The status says "rate limit" but the body says the balance is gone. No
    // wait can fix that, so it must not consume the retry budget.
    assert_terminal(
        429,
        r#"{"error":{"code":"insufficient_quota","message":"You exceeded your quota"}}"#,
        "429 insufficient_quota",
    )
    .await;
}

#[tokio::test]
async fn a_429_that_is_a_genuine_rate_limit_is_not_treated_as_quota() {
    let server = MockServer::start_async().await;
    let pacer = test_pacer();

    let mock = server
        .mock_async(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(429)
                .header("retry-after", "1")
                // Deliberately the Gemini free-tier wording, which Python
                // excludes from its terminal patterns for exactly this reason.
                .body("You exceeded your current quota, please retry shortly");
        })
        .await;

    let _ = openai_adapter(&server, &pacer)
        .generate(prompt(), None)
        .await;
    assert!(
        mock.calls_async().await >= 2,
        "a recoverable per-minute limit must still be retried"
    );
}

// ── Anthropic parity ────────────────────────────────────────────────────────

#[tokio::test]
async fn anthropic_retries_a_rate_limit_and_records_evidence() {
    let server = MockServer::start_async().await;
    let pacer = test_pacer();

    let limited = server
        .mock_async(|when, then| {
            when.method(POST).path("/messages");
            then.status(429)
                .header("retry-after", "1")
                .body("slow down");
        })
        .await;

    let _ = anthropic_adapter(&server, &pacer)
        .generate(prompt(), None)
        .await;

    assert!(limited.calls_async().await >= 2, "429 must be retried");
    assert!(pacer.is_paced(), "429 must open an episode");
}

#[tokio::test]
async fn anthropic_treats_an_exhausted_credit_balance_as_terminal() {
    let server = MockServer::start_async().await;
    let pacer = test_pacer();

    let mock = server
        .mock_async(|when, then| {
            when.method(POST).path("/messages");
            then.status(429)
                .body(r#"{"error":{"message":"Your credit balance is too low"}}"#);
        })
        .await;

    let _ = anthropic_adapter(&server, &pacer)
        .generate(prompt(), None)
        .await;
    assert_eq!(
        mock.calls_async().await,
        1,
        "an exhausted prepaid balance is terminal, not a rate limit"
    );
}

// ── auto_rate_limit=false ───────────────────────────────────────────────────

#[tokio::test]
async fn evidence_is_ignored_when_auto_rate_limit_is_off() {
    let server = MockServer::start_async().await;
    let pacer = Arc::new(Pacer::new(60, Duration::from_secs(60), false, false));

    server
        .mock_async(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(429)
                .header("retry-after", "1")
                .body("slow down");
        })
        .await;

    let _ = openai_adapter(&server, &pacer)
        .generate(prompt(), None)
        .await;
    assert!(
        !pacer.is_paced(),
        "AUTO_RATE_LIMIT=false must leave dispatch unpaced even under 429s"
    );
}

// ── The transport budget must not compound across request modes ─────────────

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct Person {
    name: String,
}

#[tokio::test]
async fn an_exhausted_transport_budget_is_not_restarted_by_the_legacy_fallback() {
    // `generate_structured` tries tool calling, then legacy function calling,
    // then JSON mode. Each mode calls `send_chat_request`, which now carries a
    // time floor — so a persistently failing endpoint could burn the whole
    // budget once per mode. It must be spent once: MaxRetriesExceeded from the
    // transport layer is terminal, not a reason to try a different request shape.
    let server = MockServer::start_async().await;
    let pacer = test_pacer();

    let failing = server
        .mock_async(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(500).header("retry-after", "1").body("boom");
        })
        .await;

    let adapter = OpenAIAdapter::new("gpt-4o-mini", "sk-test", Some(server.base_url()))
        .expect("adapter builds")
        .with_network_retries(2)
        .with_structured_output_retries(2)
        .with_min_retry_elapsed(Duration::ZERO)
        .with_pacer(Arc::clone(&pacer));

    let result: Result<Person, _> = adapter
        .create_structured_output("extract", "you are a helper", None)
        .await;

    assert!(result.is_err(), "a persistent 500 must fail");
    assert_eq!(
        failing.calls_async().await,
        2,
        "the transport budget is spent once, not once per request mode"
    );
}
