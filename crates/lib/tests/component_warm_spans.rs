#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Span coverage for engine construction (`ComponentManager::init_*`).
//!
//! A cold `warm()` spends most of its time building the six engines, and until
//! these spans existed that stretch emitted nothing at all: a stall inside
//! storage/database/graph construction produced zero bytes of output, so
//! diagnosing one meant reconstructing timings from surrounding log lines
//! instead of reading a trace (see the investigation on topoteretes/cognee-rs#115).
//!
//! Both tests are `#[serial]`. `SpanCapture` installs a process-global
//! subscriber and fans every span out to all live capture stores, so two tests
//! capturing at once in the same process see each other's spans. That is
//! harmless for "is this span present?" but fatal for the count assertion below,
//! and under a `cargo test` fallback (many tests per process, run in parallel)
//! it happens every time. Serializing keeps the guards' lifetimes disjoint.

use cognee::{ComponentManager, ConfigManager, PipelineContext, Settings};
use cognee_test_utils::SpanCapture;
use serial_test::serial;
use tempfile::TempDir;

/// A manager whose every on-disk artefact lands in a temp dir.
///
/// `Settings::default()` points at *relative* paths (`./.cognee_system`,
/// `./.data_storage`, `sqlite:./cognee.db?mode=rwc`), so building the storage and
/// database engines with the defaults writes into the crate directory and leaves
/// `cognee.db{,-shm,-wal}` behind in the working tree. Redirect all of it.
///
/// The returned `TempDir` must stay alive for the duration of the test — dropping
/// it deletes the directory out from under the engines.
fn cm() -> (ComponentManager, TempDir) {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    let settings = Settings {
        system_root_directory: root.join("system").to_string_lossy().into_owned(),
        data_root_directory: root.join("data").to_string_lossy().into_owned(),
        relational_db_url: format!(
            "sqlite:{}?mode=rwc",
            root.join("cognee.db").to_string_lossy()
        ),
        ..Settings::default()
    };
    (ComponentManager::new(ConfigManager::new(settings)), dir)
}

fn names(spans: &[cognee_test_utils::CapturedSpan]) -> Vec<String> {
    spans.iter().map(|s| s.name.clone()).collect()
}

/// Every engine constructor emits its span. `llm` is included deliberately: it
/// fails to build under default settings (no API key), and the `err` flag on the
/// `#[instrument]` means the span must still be recorded — a failed engine build
/// is exactly the case someone will be debugging.
#[tokio::test]
#[serial]
async fn each_component_construction_emits_a_span() {
    let capture = SpanCapture::install();
    let (cm, _dir) = cm();

    let _ = cm.storage().await;
    let _ = cm.database().await;
    let _ = cm.graph_db().await;
    let _ = cm.vector_db().await;
    let _ = cm.embedding_engine().await;
    // Errors under default settings (strict LLM resolution, no key).
    let llm = cm.llm().await;
    assert!(
        llm.is_err(),
        "precondition: default settings have no LLM key, so this build must fail \
         — the point of the assertion below is that the span fires anyway"
    );

    let spans = capture.spans();
    let seen = names(&spans);
    for component in [
        "storage",
        "database",
        "graph_db",
        "vector_db",
        "embedding_engine",
        "llm",
    ] {
        let expected = format!("cognee.component.{component}");
        assert!(
            spans.iter().any(|s| s.name == expected),
            "missing span {expected}; saw: {seen:?}",
        );
    }
}

/// Regression guard for *where* the spans live.
///
/// They sit on the private `init_*` constructors, not the public accessors,
/// because the accessors' `versioned_accessor!` fast path returns a cached `Arc`.
/// Instrumenting there would emit a span per cache hit — thousands per pipeline
/// run — burying the few that represent real work. If someone moves the
/// `#[instrument]` up to the accessors, this test fails.
#[tokio::test]
#[serial]
async fn cached_access_does_not_emit_a_second_span() {
    let capture = SpanCapture::install();
    let (cm, _dir) = cm();

    // First call constructs; the next two must be served from the cache.
    let first = cm.storage().await;
    assert!(first.is_ok(), "storage must build under default settings");
    let _ = cm.storage().await;
    let _ = cm.storage().await;

    let spans = capture.spans();
    let count = spans
        .iter()
        .filter(|s| s.name == "cognee.component.storage")
        .count();
    assert_eq!(
        count,
        1,
        "expected exactly one construction span across three accessor calls, \
         got {count} — is #[instrument] on the accessor instead of init_*? \
         saw: {:?}",
        names(&spans),
    );
}
