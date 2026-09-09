//! Bedrock feeds the dispatch pacer, like the other three adapters (SDK-612).
//!
//! SDK-503 wired a token bucket and a 900s overload cooldown into the
//! OpenAI-compatible, Anthropic and Azure adapters. Bedrock was left out, so a
//! Bedrock deployment ran with no client-side rate limiting at all: throttling
//! was retried with backoff but never opened an episode, and nothing paced the
//! next wave into a provider that had just said it was saturated.
//!
//! These cases pin the classification from the outside — which statuses open an
//! episode and, just as importantly, which do not. Testing only the positive
//! direction would pass just as well against a `record_overload` called
//! unconditionally.
//!
//! The pacer is injected with `with_pacer` rather than installed process-wide,
//! because `init_llm_pacer` is a first-call-wins `OnceLock` and these cases must
//! not need a process of their own. The adapter's resolved endpoint is pointed
//! at the mock server through `AwsInputs::bedrock_runtime_endpoint`, the same
//! way `bedrock_integration.rs` does it, so the real endpoint chain runs.
#![cfg(feature = "bedrock")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test code: panics are acceptable"
)]

use std::sync::Arc;
use std::time::Duration;

use cognee_llm::adapters::bedrock::BedrockAdapter;
use cognee_llm::adapters::bedrock::aws::env::AwsInputs;
use cognee_llm::adapters::bedrock::converse::encode_model_id;
use cognee_llm::llm_trait::Llm;
use cognee_llm::types::Message;
use cognee_utils::pacing::Pacer;
use httpmock::prelude::*;

/// A bearer key short-circuits the credential ladder, so these never touch AWS.
const BEARER_KEY: &str = "test-bedrock-key";
const NOVA_LITE: &str = "eu.amazon.nova-lite-v1:0";

/// Generous bucket: these cases assert *whether an episode opened*, never how
/// long a caller waited, so the rate must never be the thing under test.
fn reactive_pacer() -> Arc<Pacer> {
    Arc::new(Pacer::new(
        1_000,
        Duration::from_secs(1),
        // Pacing off by config: an episode can then only have been opened by the
        // adapter reacting to the response, which is the whole point.
        false,
        // `record_overload` is a no-op without this.
        true,
    ))
}

async fn adapter(server: &MockServer, pacer: Arc<Pacer>) -> BedrockAdapter {
    let aws = AwsInputs {
        region: Some("eu-central-1".to_string()),
        bedrock_runtime_endpoint: Some(server.base_url()),
        ..AwsInputs::default()
    };
    BedrockAdapter::new(NOVA_LITE, Some(BEARER_KEY), None, &aws)
        .await
        .expect("adapter builds offline under bearer auth")
        // One mocked exchange per case: no retry ladder to reason about.
        .with_network_retries(0)
        .with_structured_output_retries(1)
        .with_pacer(pacer)
}

fn converse_path() -> String {
    format!("/model/{}/converse", encode_model_id(NOVA_LITE))
}

/// Drive one `generate` against a mock replying with `status`, and report
/// whether the adapter opened an overload episode.
async fn episode_opened_for(status: u16, body: &'static str) -> bool {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(POST).path(converse_path());
            then.status(status)
                .header("content-type", "application/json")
                .body(body);
        })
        .await;

    let pacer = reactive_pacer();
    assert!(
        !pacer.is_paced(),
        "a fresh pacer must start with no episode open"
    );

    let _ = adapter(&server, pacer.clone())
        .await
        .generate(vec![Message::user("hello")], None)
        .await;

    pacer.is_paced()
}

/// Bedrock reports `ThrottlingException` as HTTP 429 — the signal the cooldown
/// exists for.
#[tokio::test]
async fn a_throttled_response_opens_an_overload_episode() {
    assert!(
        episode_opened_for(
            429,
            r#"{"message":"Too many requests, please wait before trying again."}"#
        )
        .await,
        "a 429 must open an episode so the next wave is paced"
    );
}

/// 503 is how a busy or not-yet-ready model presents. Same class of signal.
#[tokio::test]
async fn a_service_unavailable_response_opens_an_overload_episode() {
    assert!(
        episode_opened_for(503, r#"{"message":"Model is not ready for inference."}"#).await,
        "a 503 must open an episode"
    );
}

/// The negative direction, and the one that makes the two above mean something:
/// a request the provider rejected on its merits says nothing about load.
/// A `ValidationException` is terminal — re-sending cannot help — but pacing
/// every caller for 15 minutes over one malformed body would be a bad trade.
#[tokio::test]
async fn a_validation_error_leaves_the_pacer_closed() {
    assert!(
        !episode_opened_for(
            400,
            r#"{"message":"ValidationException: malformed input request."}"#
        )
        .await,
        "a 400 is not evidence of overload"
    );
}

/// And the ordinary path stays clear.
#[tokio::test]
async fn a_successful_response_leaves_the_pacer_closed() {
    let body = r#"{
        "output": { "message": { "role": "assistant", "content": [{ "text": "hi" }]}},
        "stopReason": "end_turn",
        "usage": { "inputTokens": 1, "outputTokens": 1, "totalTokens": 2 }
    }"#;
    assert!(
        !episode_opened_for(200, body).await,
        "a successful call must not open an episode"
    );
}
