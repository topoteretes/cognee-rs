use cognee_embedding::{EmbeddingEngine, MockEmbeddingEngine};
use cognee_llm::Llm;
use cognee_llm::mock::{MissPolicy, RecordingLlm, ReplayLlm};
use std::sync::Arc;

// LLM env helpers live in the shared `cognee-test-utils` crate so every test
// crate uses one implementation (consistent alias fallback + model default).
// Re-exported here (with the historical `create_adapter_from_env` name) so the
// many `test_utils::…` call sites in this crate's tests keep compiling.
// `fail_loudly_in_cassette_mode` also lives in the shared crate (single source
// of truth — see its docs there) and is re-exported here alongside the env helpers.
#[allow(unused_imports)]
pub use cognee_test_utils::{
    create_openai_adapter_from_env as create_adapter_from_env, fail_loudly_in_cassette_mode,
    llm_env_available, require_env,
};

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
