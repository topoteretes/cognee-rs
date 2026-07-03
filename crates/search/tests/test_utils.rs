use cognee_embedding::{EmbeddingEngine, MockEmbeddingEngine};
use cognee_llm::Llm;
use cognee_llm::mock::{MissPolicy, RecordingLlm, ReplayLlm};
use std::sync::Arc;

// LLM env helpers live in the shared `cognee-test-utils` crate so every test
// crate uses one implementation (consistent alias fallback + model default).
// Re-exported here (with the historical `create_adapter_from_env` name) so the
// many `test_utils::…` call sites in this crate's tests keep compiling.
#[allow(unused_imports)]
pub use cognee_test_utils::{
    create_openai_adapter_from_env as create_adapter_from_env, llm_env_available, require_env,
};

/// In cassette-replay mode a pipeline error means a stale/missing cassette
/// entry; the `Err => eprintln + return` skip blocks would otherwise swallow it
/// and pass with zero assertions. Call this in those blocks so a replay miss
/// fails loudly (re-record cassettes); no-op outside replay mode.
#[allow(dead_code)]
pub fn fail_loudly_on_replay_miss(what: &str, err: &impl std::fmt::Display) {
    if std::env::var("COGNEE_TEST_REPLAY").is_ok_and(|v| !v.is_empty()) {
        panic!(
            "{what} failed in replay mode — likely a stale/missing cassette entry; re-record cassettes. Error: {err}"
        );
    }
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
