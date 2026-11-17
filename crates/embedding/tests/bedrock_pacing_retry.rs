//! Every retry of a Bedrock embedding request is admitted through the
//! embedding pacer, not just the first attempt (SDK-669).
//!
//! The admission sits in `post_once`, which the retry loop re-enters, so a
//! regression that moved it up to `post_with_retry` or `invoke` (one token
//! for all attempts) would pass every other test.
//!
//! Its own binary, apart from `bedrock_pacing.rs`: the pacer is a process
//! global that the first install wins, and this test needs a refill interval
//! longer than the engine's first retry backoff (2 s plus 0-1 s of jitter).
//! With a shorter interval the bucket refills during the backoff and the
//! retry's admission is free, so the test could not tell it happened.

#![allow(
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use cognee_embedding::{
    BedrockEmbeddingEngine, EmbeddingConfig, EmbeddingEngine, EmbeddingProvider,
};

/// One token per 5 s, capacity one. Longer than the 2-3 s first backoff.
const INTERVAL: Duration = Duration::from_secs(5);

/// Slack for timer granularity; the bound is on the low side only.
const SLACK: Duration = Duration::from_millis(50);

#[tokio::test]
async fn a_retry_is_admitted_again() {
    let pacer = cognee_utils::pacing::init_embedding_pacer(1, INTERVAL, true);
    assert!(
        cognee_utils::pacing::embedding_pacer().is_some_and(|p| Arc::ptr_eq(&p, &pacer)),
        "the pacer this test configured must be the one installed"
    );

    let mut server = mockito::Server::new_async().await;
    // mockito serves the first matching mock that still has hits left, so the
    // throttle answers the first attempt and the success the retry.
    let throttled = server
        .mock("POST", "/model/amazon.titan-embed-text-v1/invoke")
        .with_status(429)
        .with_body(r#"{"message":"Too many requests"}"#)
        .expect(1)
        .create_async()
        .await;
    let ok = server
        .mock("POST", "/model/amazon.titan-embed-text-v1/invoke")
        .with_status(200)
        .with_body(r#"{"embedding":[1.0,0.0],"inputTextTokenCount":1}"#)
        .expect(1)
        .create_async()
        .await;

    let mut config = EmbeddingConfig {
        provider: EmbeddingProvider::Bedrock,
        model: "amazon.titan-embed-text-v1".to_string(),
        dimensions: 2,
        endpoint: Some(server.url()),
        api_key: Some("bedrock-api-key".to_string()),
        ..EmbeddingConfig::default()
    };
    config.aws.region = Some("us-east-1".to_string());
    // Default retry budget (128 s): the 429 must be retried.
    let engine = BedrockEmbeddingEngine::new(&config)
        .await
        .expect("engine builds with a pinned region and a bearer key");

    let start = Instant::now();
    let out = engine.embed(&["a"]).await.expect("the retry succeeds");
    let elapsed = start.elapsed();

    assert_eq!(out, vec![vec![1.0, 0.0]]);
    throttled.assert_async().await;
    ok.assert_async().await;
    // The first attempt takes the only token. The retry fires after 2-3 s of
    // backoff and must wait for the refill at 5 s. Unadmitted, it would finish
    // in under 3 s.
    assert!(
        elapsed + SLACK >= INTERVAL,
        "the retry must take a second token (>= {INTERVAL:?}), took {elapsed:?}"
    );
}
