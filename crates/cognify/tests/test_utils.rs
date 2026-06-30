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

#[allow(dead_code)]
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

/// In cassette-replay mode (`COGNEE_TEST_REPLAY` set), a pipeline error means a
/// cassette miss — a stale/edited prompt or schema whose input hash no longer
/// matches a recorded entry (`ReplayLlm` returns `LlmError::InvalidResponse`).
/// The `Err(e) => { eprintln!("Skipping…"); return }` blocks the e2e tests use
/// to tolerate a missing real LLM would otherwise swallow that miss and pass
/// with zero assertions. Call this in those blocks so replay misses fail loudly
/// (re-record via the record-cassettes workflow); outside replay mode it is a
/// no-op and the legitimate skip proceeds.
#[allow(dead_code)]
pub fn fail_loudly_on_replay_miss(what: &str, err: &impl std::fmt::Display) {
    if std::env::var("COGNEE_TEST_REPLAY").is_ok_and(|v| !v.is_empty()) {
        panic!(
            "{what} failed in replay mode — likely a stale/missing cassette entry; re-record cassettes. Error: {err}"
        );
    }
}

/// Resolve the on-disk path of a named cassette under this crate's
/// `tests/fixtures/cassettes/` directory.
#[allow(dead_code)]
pub fn cassette_path(name: &str) -> String {
    format!(
        "{}/tests/fixtures/cassettes/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// Build the `Llm` an integration test should use, choosing one of three modes
/// from the environment so the same test runs offline in CI and records locally:
///
/// - `COGNEE_TEST_REPLAY=1` → offline replay from the committed cassette
///   `tests/fixtures/cassettes/<name>.json`. Uses [`MissPolicy::Error`] so a
///   missing/stale entry fails loudly instead of silently returning an empty
///   graph (the `ReplayLlm` default). Needs no API credentials.
/// - `COGNEE_RECORD_LLM=1` → the real `OpenAIAdapter`, wrapped so every call is
///   recorded (and merged) into that cassette on drop. Needs credentials.
/// - neither → the plain real `OpenAIAdapter` (legacy behaviour).
///
/// Pair this with `MOCK_EMBEDDING=deterministic` for any test that also touches
/// the vector store, so embeddings are identical across record and replay.
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

/// A deterministic in-process embedding engine for cassette-replay tests.
///
/// Vectors are derived from `sha256(text)` at the workspace-default 384
/// dimensions, so they are byte-identical across record and replay without any
/// real embedding backend or network. Use this in place of
/// `cognee_test_utils::create_test_embedding_engine()` for tests that only need
/// embeddings to *exist* and be reproducible (graph structure, deletion,
/// cleanup). Do NOT use it for tests that assert on real semantic similarity or
/// vector ranking — those must keep a real embedding engine. Choosing it
/// explicitly (rather than a global `MOCK_EMBEDDING` env var) keeps mock and
/// real-embedding tests cleanly separated in the same CI run.
#[allow(dead_code)]
pub fn create_deterministic_embedding_engine() -> Arc<dyn EmbeddingEngine> {
    Arc::new(MockEmbeddingEngine::deterministic(384))
}
