//! The Bedrock embedding engine admits every InvokeModel request through the
//! process-wide embedding pacer (SDK-669).
//!
//! The Titan case is the one that matters: Titan does not batch, so one
//! `embed` of N texts is N requests. Admission must be per request — a gate
//! around the whole fan-out would let all N through on one token.
//!
//! This lives in its own test binary because the pacer is a process global
//! that the first install wins: in a shared binary another test could install
//! a different pacer first, or this one could throttle unrelated tests.

#![allow(
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]

use std::time::{Duration, Instant};

use cognee_embedding::{
    BedrockEmbeddingEngine, EmbeddingConfig, EmbeddingEngine, EmbeddingProvider,
};

/// One token per `INTERVAL`, capacity one: the first request is free and each
/// later one waits a full interval behind the one before it. 1 s is the
/// shortest interval `TokenBucket` accepts; it clamps anything below.
const INTERVAL: Duration = Duration::from_secs(1);

/// Slack for timer granularity; the bound is on the low side only, so a slow
/// machine cannot make it fail.
const SLACK: Duration = Duration::from_millis(50);

fn install_pacer() {
    let pacer = cognee_utils::pacing::init_embedding_pacer(1, INTERVAL, true);
    assert!(
        cognee_utils::pacing::embedding_pacer().is_some_and(|p| std::sync::Arc::ptr_eq(&p, &pacer)),
        "the pacer this test configured must be the one installed"
    );
}

/// Region pinned and a bearer key set, so neither the ambient region chain nor
/// the credential ladder runs. `150 / 36` gives a Titan fan-out of 4, so
/// without pacing three texts would go out together.
async fn engine_for(server_url: &str, model: &str) -> BedrockEmbeddingEngine {
    let mut config = EmbeddingConfig {
        provider: EmbeddingProvider::Bedrock,
        model: model.to_string(),
        dimensions: 2,
        endpoint: Some(server_url.to_string()),
        api_key: Some("bedrock-api-key".to_string()),
        batch_size: 36,
        max_concurrent_data_points: 150,
        ..EmbeddingConfig::default()
    };
    config.aws.region = Some("us-east-1".to_string());
    BedrockEmbeddingEngine::new(&config)
        .await
        .expect("engine builds with a pinned region and a bearer key")
        .with_retry_budget(Duration::ZERO)
}

#[tokio::test]
async fn each_titan_request_of_a_fan_out_is_admitted_separately() {
    install_pacer();
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/model/amazon.titan-embed-text-v1/invoke")
        .with_status(200)
        .with_body(r#"{"embedding":[1.0,0.0],"inputTextTokenCount":1}"#)
        .expect(3)
        .create_async()
        .await;
    let engine = engine_for(&server.url(), "amazon.titan-embed-text-v1").await;

    let start = Instant::now();
    let out = engine.embed(&["a", "b", "c"]).await.expect("embeds");
    let elapsed = start.elapsed();

    assert_eq!(out.len(), 3);
    mock.assert_async().await;
    // Three requests, one token each: at least two refills. One admission for
    // the whole call would finish in a few milliseconds.
    assert!(
        elapsed + SLACK >= INTERVAL * 2,
        "3 Titan requests must take 3 tokens (>= {:?}), took {elapsed:?}",
        INTERVAL * 2
    );
}

#[tokio::test]
async fn each_cohere_batch_is_admitted() {
    install_pacer();
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/model/cohere.embed-english-v3/invoke")
        .with_status(200)
        .with_body(r#"{"embeddings":[[1.0,0.0],[0.0,1.0]]}"#)
        .expect(2)
        .create_async()
        .await;
    let engine = engine_for(&server.url(), "cohere.embed-english-v3").await;

    let start = Instant::now();
    for _ in 0..2 {
        engine.embed(&["a", "b"]).await.expect("embeds");
    }
    let elapsed = start.elapsed();

    mock.assert_async().await;
    assert!(
        elapsed + SLACK >= INTERVAL,
        "2 Cohere batches must take 2 tokens (>= {INTERVAL:?}), took {elapsed:?}"
    );
}
