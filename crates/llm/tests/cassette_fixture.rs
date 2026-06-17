//! Smoke test for the committed performance-benchmark cassette fixture.
//!
//! The offline mock benchmark (`scripts/perf/run_mock_bench.sh`) replays the
//! committed `scripts/perf/fixtures/cassette.json` so the full pipeline runs with
//! no API key. This test guards that fixture: it must remain a `LlmCassette` that
//! `LlmCassette::load` accepts, so a malformed or accidentally-truncated commit
//! is caught here rather than at benchmark time.
#![cfg(feature = "mock")]

use std::path::PathBuf;

use cognee_llm::mock::LlmCassette;

/// Absolute path to the workspace fixture, derived from this crate's manifest dir
/// (`crates/llm`) so the test is independent of the current working directory.
fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/perf/fixtures/cassette.json")
}

#[test]
fn committed_cassette_fixture_parses() {
    let path = fixture_path();
    let cassette = LlmCassette::load(&path)
        .unwrap_or_else(|e| panic!("failed to load committed cassette {}: {e}", path.display()));

    // Format contract: version 1, recorded model present, and at least one
    // recorded response (a benchmark replay needs real entries to hit on).
    assert_eq!(cassette.version, 1, "cassette version must be 1");
    assert!(
        !cassette.model.trim().is_empty(),
        "cassette must record the model it was captured against"
    );
    assert!(
        !cassette.entries.is_empty(),
        "committed cassette must contain at least one recorded entry"
    );
}
