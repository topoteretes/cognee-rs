use cognee_embedding::{EmbeddingEngine, MockEmbeddingEngine};
use cognee_llm::Llm;
use cognee_llm::OpenAIAdapter;
use cognee_llm::mock::{MissPolicy, RecordingLlm, ReplayLlm};
use std::sync::Arc;

/// Read a required environment variable, loading `.env` first (idempotent).
///
/// Accepts Python-compatible canonical names (`LLM_API_KEY`, `LLM_ENDPOINT`,
/// `LLM_MODEL`) as fallbacks for the legacy Rust test aliases (`OPENAI_TOKEN`,
/// `OPENAI_URL`, `OPENAI_MODEL`) so that a single `.env` with the canonical
/// names works for both the CLI and integration tests.
pub fn require_env(var_name: &str) -> String {
    let _ = dotenv::dotenv();

    // Legacy alias → canonical fallback mapping
    let canonical_fallback = match var_name {
        "OPENAI_TOKEN" => Some("LLM_API_KEY"),
        "OPENAI_URL" => Some("LLM_ENDPOINT"),
        "OPENAI_MODEL" => Some("LLM_MODEL"),
        _ => None,
    };

    if let Ok(v) = std::env::var(var_name)
        && !v.is_empty()
    {
        return v;
    }
    if let Some(canonical) = canonical_fallback
        && let Ok(v) = std::env::var(canonical)
        && !v.is_empty()
    {
        return v;
    }
    panic!("❌ Required environment variable '{var_name}' is not set")
}

pub fn create_adapter_from_env() -> Arc<OpenAIAdapter> {
    let base_url = require_env("OPENAI_URL");
    let api_token = require_env("OPENAI_TOKEN");
    let model = std::env::var("LLM_MODEL")
        .or_else(|_| std::env::var("OPENAI_MODEL"))
        .unwrap_or_else(|_| "gpt-4o-mini".to_string());

    Arc::new(
        OpenAIAdapter::new(model, api_token, Some(base_url))
            .unwrap_or_else(|e| panic!("❌ Failed to create OpenAI adapter: {e}")),
    )
}

/// Resolve a named cassette under this crate's `tests/fixtures/cassettes/`.
#[allow(dead_code)]
pub fn cassette_path(name: &str) -> String {
    format!(
        "{}/tests/fixtures/cassettes/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// LLM for an integration test: offline replay when `COGNEE_TEST_REPLAY=1`
/// (MissPolicy::Error so a stale cassette fails loudly), recording when
/// `COGNEE_RECORD_LLM=1`, else the real adapter. See crates/cognify for the
/// full rationale (Approach E).
#[allow(dead_code)]
pub fn create_llm_from_env(cassette_name: &str) -> Arc<dyn Llm> {
    let cassette = cassette_path(cassette_name);
    if std::env::var("COGNEE_TEST_REPLAY").is_ok_and(|v| !v.is_empty()) {
        let replay = ReplayLlm::from_path(&cassette)
            .unwrap_or_else(|e| panic!("❌ Failed to load cassette {cassette}: {e}"))
            .with_miss_policy(MissPolicy::Error);
        return Arc::new(replay);
    }
    let adapter = create_adapter_from_env();
    if std::env::var("COGNEE_RECORD_LLM").is_ok_and(|v| !v.is_empty()) {
        return Arc::new(RecordingLlm::new(adapter, cassette));
    }
    adapter
}

/// Deterministic in-process embedding engine (sha256(text), 384 dims) for
/// cassette-replay tests that only need embeddings to exist and be
/// reproducible. NOT for tests asserting real semantic similarity/ranking.
#[allow(dead_code)]
pub fn create_deterministic_embedding_engine() -> Arc<dyn EmbeddingEngine> {
    Arc::new(MockEmbeddingEngine::deterministic(384))
}
