//! Gracefully-skipped live integration test for the native Anthropic adapter
//! (issue #17, Tier 2). Skips (early return, not a failure) when
//! `ANTHROPIC_API_KEY` is absent, so CI stays green without secrets.
//!
//! Environment variables (absent => skip):
//! - `ANTHROPIC_API_KEY` (required)
//! - `ANTHROPIC_MODEL`   (optional; defaults to a current Sonnet model)
//! - `ANTHROPIC_ENDPOINT`(optional; overrides the default Anthropic base URL)
//!
//! Run with: cargo test --package cognee-llm --test integration_anthropic

use cognee_llm::{AnthropicAdapter, GenerationOptions, Llm, LlmExt, Message};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

fn env_opt(name: &str) -> Option<String> {
    let _ = dotenv::dotenv();
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn adapter_or_skip(test: &str) -> Option<AnthropicAdapter> {
    let Some(api_key) = env_opt("ANTHROPIC_API_KEY") else {
        eprintln!("⏭  skipping {test}: ANTHROPIC_API_KEY not set");
        return None;
    };
    let model = env_opt("ANTHROPIC_MODEL").unwrap_or_else(|| "claude-3-5-sonnet-20241022".into());
    let endpoint = env_opt("ANTHROPIC_ENDPOINT");
    Some(
        AnthropicAdapter::new(model, api_key, endpoint)
            .unwrap_or_else(|e| panic!("❌ failed to build Anthropic adapter: {e}")),
    )
}

#[tokio::test]
async fn anthropic_generate_smoke() {
    let Some(adapter) = adapter_or_skip("anthropic_generate_smoke") else {
        return;
    };

    let result = adapter
        .generate(
            vec![
                Message::system("You are a helpful assistant."),
                Message::user("What is 2+2? Answer with just the number."),
            ],
            Some(GenerationOptions {
                temperature: Some(0.0),
                max_tokens: Some(16),
                ..Default::default()
            }),
        )
        .await
        .unwrap_or_else(|e| panic!("❌ anthropic generate failed: {e}"));

    assert!(!result.content.is_empty(), "response should not be empty");
    eprintln!("✅ anthropic generate: '{}'", result.content.trim());
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct Person {
    name: String,
    age: u32,
}

#[tokio::test]
async fn anthropic_structured_output_smoke() {
    let Some(adapter) = adapter_or_skip("anthropic_structured_output_smoke") else {
        return;
    };

    let person: Person = adapter
        .create_structured_output(
            "Ada Lovelace was 36.",
            "Extract the person's name and age.",
            Some(GenerationOptions {
                temperature: Some(0.0),
                max_tokens: Some(256),
                ..Default::default()
            }),
        )
        .await
        .unwrap_or_else(|e| panic!("❌ anthropic structured output failed: {e}"));

    assert!(!person.name.is_empty(), "name should be extracted");
    eprintln!("✅ anthropic structured: {} ({})", person.name, person.age);
}
