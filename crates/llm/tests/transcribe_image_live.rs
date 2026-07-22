//! Live integration test for `transcribe_image` against an OpenAI-compatible API.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test code — panics are acceptable"
)]
//!
//! Gated behind `#[ignore]` — requires `OPENAI_TOKEN` (and optionally `OPENAI_URL`,
//! `OPENAI_MODEL`) environment variables pointing at a vision-capable model.

use cognee_llm::Llm;
use cognee_llm::build_openai_compatible_adapter;

#[tokio::test]
#[ignore] // Requires OPENAI_TOKEN and a vision-capable model
async fn live_transcribe_image() {
    let api_key = std::env::var("OPENAI_TOKEN").expect("OPENAI_TOKEN required for live test");
    let base_url = std::env::var("OPENAI_URL").unwrap_or_default();
    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());

    // Route through the production factory (provider from env, default `openai`)
    // so litellm-style model prefixes are stripped exactly as in a real run.
    let provider = std::env::var("LLM_PROVIDER").unwrap_or_else(|_| "openai".to_string());
    let adapter = build_openai_compatible_adapter(&provider, &model, &api_key, &base_url, 3)
        .expect("Failed to create adapter for live vision test");

    // Minimal valid 1x1 red PNG
    let png_bytes: &[u8] = include_bytes!("fixtures/red_pixel.png");

    let result = adapter.transcribe_image(png_bytes, "image/png", None).await;
    assert!(result.is_ok(), "Vision call failed: {:?}", result.err());
    let text = result.unwrap();
    assert!(!text.is_empty(), "Vision returned empty description");
}
