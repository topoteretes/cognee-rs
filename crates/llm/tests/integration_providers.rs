//! Gracefully-skipped integration tests for the OpenAI-compatible providers wired
//! in Tier 1 (issue #17): `ollama`, `mistral`, `gemini`, and `custom`.
//!
//! Each test builds its adapter through the same factory the runtime uses
//! (`build_openai_compatible_adapter`) and runs one real completion. A test is
//! **skipped** (early return, not a failure) when the provider's credentials are
//! absent, so CI stays green without secrets — mirroring `integration_openai.rs`.
//!
//! Per-provider environment variables (all optional; absent => skip):
//! - ollama:  `OLLAMA_LLM_API_KEY`,  `OLLAMA_LLM_MODEL`,  `OLLAMA_LLM_ENDPOINT`  (endpoint defaults to the local Ollama server)
//! - mistral: `MISTRAL_LLM_API_KEY`, `MISTRAL_LLM_MODEL`, `MISTRAL_LLM_ENDPOINT` (endpoint defaults to the Mistral API)
//! - gemini:  `GEMINI_LLM_API_KEY`,  `GEMINI_LLM_MODEL`,  `GEMINI_LLM_ENDPOINT`  (endpoint defaults to the Gemini OpenAI-compat shim)
//! - custom:  `CUSTOM_LLM_API_KEY`,  `CUSTOM_LLM_MODEL`,  `CUSTOM_LLM_ENDPOINT`  (endpoint is required)
//!
//! Run with: cargo test --package cognee-llm --test integration_providers

use cognee_llm::{GenerationOptions, Llm, Message, build_openai_compatible_adapter};

/// Read an env var (loading `.env` first), returning `None` when unset/empty.
fn env_opt(name: &str) -> Option<String> {
    let _ = dotenv::dotenv();
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// Build the adapter and run one tiny completion, or skip when creds are absent.
///
/// `endpoint` is `None` for providers with a built-in default (ollama/mistral/gemini)
/// and required for `custom`.
async fn run_provider_smoke(provider: &str, key_env: &str, model_env: &str, endpoint_env: &str) {
    let Some(api_key) = env_opt(key_env) else {
        eprintln!("⏭  skipping {provider}: {key_env} not set");
        return;
    };
    let Some(model) = env_opt(model_env) else {
        eprintln!("⏭  skipping {provider}: {model_env} not set");
        return;
    };
    let endpoint = env_opt(endpoint_env).unwrap_or_default();

    // `custom` has no default endpoint; skip rather than fail when it is missing.
    if (provider == "custom" || provider == "openai_compatible") && endpoint.is_empty() {
        eprintln!("⏭  skipping {provider}: {endpoint_env} not set (required for custom)");
        return;
    }

    let adapter = build_openai_compatible_adapter(provider, &model, &api_key, &endpoint, 3)
        .unwrap_or_else(|e| panic!("❌ {provider}: failed to build adapter: {e}"));

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
        .unwrap_or_else(|e| panic!("❌ {provider}: generation failed: {e}"));

    assert!(
        !result.content.is_empty(),
        "{provider}: response content should not be empty"
    );
    eprintln!("✅ {provider}: '{}'", result.content.trim());
}

#[tokio::test]
async fn ollama_completion_smoke() {
    run_provider_smoke(
        "ollama",
        "OLLAMA_LLM_API_KEY",
        "OLLAMA_LLM_MODEL",
        "OLLAMA_LLM_ENDPOINT",
    )
    .await;
}

#[tokio::test]
async fn mistral_completion_smoke() {
    run_provider_smoke(
        "mistral",
        "MISTRAL_LLM_API_KEY",
        "MISTRAL_LLM_MODEL",
        "MISTRAL_LLM_ENDPOINT",
    )
    .await;
}

#[tokio::test]
async fn gemini_completion_smoke() {
    run_provider_smoke(
        "gemini",
        "GEMINI_LLM_API_KEY",
        "GEMINI_LLM_MODEL",
        "GEMINI_LLM_ENDPOINT",
    )
    .await;
}

#[tokio::test]
async fn custom_completion_smoke() {
    run_provider_smoke(
        "custom",
        "CUSTOM_LLM_API_KEY",
        "CUSTOM_LLM_MODEL",
        "CUSTOM_LLM_ENDPOINT",
    )
    .await;
}
