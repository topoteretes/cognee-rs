#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! The `graphBackend` cognify opt: the registry seam that lets a non-Rust
//! caller pick the [`ChunkGraphExtractor`] merged in #239.
//!
//! Two properties carry the feature, and both are asserted end-to-end through
//! [`cognify_config_with_opts`] — the function every binding's cognify goes
//! through — rather than only against the registry in isolation:
//!
//! 1. A kind nothing is registered under **fails the call**. The failure mode
//!    this rules out is the expensive one: quietly building the graph with the
//!    LLM fact extractor, which is exactly what the caller opted out of and
//!    exactly what the code did before the seam existed (it ignored the key).
//! 2. A registered factory is **actually invoked**, and its extractor lands on
//!    the `CognifyConfig` the run will use.
//!
//! Plus the inertness property that makes this safe to merge with no backend in
//! the tree: with nothing registered, opts that do not mention `graphBackend`
//! produce byte-identical configuration.
//!
//! ## Registry hygiene
//!
//! The registry is process-wide and `cargo test` runs these in parallel
//! threads, so every test that registers uses a kind name unique to it, and no
//! assertion depends on the *set* of registered kinds (only on the one kind the
//! test named appearing in the message).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};
use tempfile::TempDir;

use cognee::cognify::{
    ChunkGraphExtractor, ChunkGraphResult, ChunkRef, ExtractionContext, GraphBackendError,
};
use cognee::config::Settings;
use cognee_bindings_common::graph_backend::{
    GraphBackendFuture, check_graph_backend_registered, graph_backend_from_opts,
    register_graph_backend, registered_graph_backend_kinds,
};
use cognee_bindings_common::ops::pipeline::cognify_config_with_opts;
use cognee_bindings_common::{CogneeServices, HandleState, SdkError};

// ---------------------------------------------------------------------------
// Fixtures.
// ---------------------------------------------------------------------------

/// A do-nothing extractor: these tests build a `CognifyConfig`, they never run
/// a pipeline through it, so `extract_graphs` is unreachable.
struct StubExtractor(&'static str);

#[async_trait::async_trait]
impl ChunkGraphExtractor for StubExtractor {
    fn name(&self) -> &str {
        self.0
    }

    async fn extract_graphs<'c, 'x>(
        &self,
        _chunks: &[ChunkRef<'c>],
        _ctx: &ExtractionContext<'x>,
    ) -> Result<Vec<ChunkGraphResult>, GraphBackendError> {
        unreachable!("these tests only build configuration, they never cognify")
    }
}

/// How many times each test's factory ran, so "was it invoked" is a fact rather
/// than an inference from the config.
static CALLS: AtomicUsize = AtomicUsize::new(0);

/// Hermetic services, built the way `handle_close.rs` builds them: every store
/// under a temp dir, a dummy key so the strict LLM resolve succeeds without
/// network I/O, and the mock embedding provider so warming never downloads.
async fn services_under(dir: &TempDir) -> Arc<CogneeServices> {
    let root = dir.path();
    let settings = Settings {
        llm_api_key: "sk-test".to_owned(),
        embedding_provider: "mock".to_owned(),
        data_root_directory: root.join("data").to_string_lossy().into_owned(),
        system_root_directory: root.join("sys").to_string_lossy().into_owned(),
        relational_db_url: format!("sqlite://{}?mode=rwc", root.join("cognee.db").display()),
        ..Settings::default()
    };
    HandleState::from_settings(settings)
        .services()
        .await
        .expect("warm")
}

// ---------------------------------------------------------------------------
// 1. An unregistered kind errors — it does NOT fall back to the LLM.
// ---------------------------------------------------------------------------

/// Before the seam, `cognify_config_with_opts` had no `graphBackend` branch at
/// all: this opts object went through untouched and cognify ran the LLM fact
/// extractor. That is the regression this asserts against, at the exact
/// function the bindings call.
#[tokio::test(flavor = "multi_thread")]
async fn an_unregistered_kind_fails_the_call_instead_of_using_the_llm() {
    let dir = TempDir::new().expect("tempdir");
    let svc = services_under(&dir).await;

    let opts = json!({ "graphBackend": "no-such-backend-xyz" });
    let err = cognify_config_with_opts(&svc, &opts)
        .await
        .expect_err("an unservable backend must fail the call");

    assert!(matches!(err, SdkError::Validation(_)), "got {err:?}");
    let msg = err.to_string();
    assert!(msg.contains("no-such-backend-xyz"), "{msg}");
    assert!(
        msg.contains("NOT run with the LLM extractor"),
        "the message must rule out the silent-fallback reading: {msg}"
    );
}

/// The same refusal one layer down, and for the object spelling of the opt.
#[tokio::test(flavor = "multi_thread")]
async fn an_unregistered_kind_is_refused_by_the_resolver_too() {
    let opts = json!({ "graphBackend": { "kind": "absent-kind-xyz", "threshold": 0.5 } });
    let err = graph_backend_from_opts(&opts)
        .await
        .err()
        .expect("unregistered kind");
    assert!(err.to_string().contains("absent-kind-xyz"), "{err}");

    // And the cheap pre-flight agrees, so `add_and_cognify` refuses before it
    // has ingested anything.
    let err = check_graph_backend_registered(&opts).expect_err("unregistered kind");
    assert!(err.to_string().contains("absent-kind-xyz"), "{err}");
}

/// A malformed selection is an error, never read as "no selection". Silently
/// dropping it is the same silent-LLM fallback by another route.
#[tokio::test(flavor = "multi_thread")]
async fn a_malformed_selection_is_refused_rather_than_ignored() {
    for opts in [
        json!({ "graphBackend": 7 }),
        json!({ "graphBackend": [] }),
        json!({ "graphBackend": { "threshold": 0.5 } }),
        json!({ "graphBackend": { "kind": 7 } }),
    ] {
        let err = graph_backend_from_opts(&opts)
            .await
            .err()
            .unwrap_or_else(|| panic!("{opts} must be refused, not ignored"));
        assert!(matches!(err, SdkError::Validation(_)), "{opts}: {err:?}");
        check_graph_backend_registered(&opts)
            .err()
            .unwrap_or_else(|| panic!("{opts} must be refused by the pre-flight too"));
    }
}

// ---------------------------------------------------------------------------
// 2. A registered factory is actually invoked.
// ---------------------------------------------------------------------------

fn invoked_factory(spec: Value) -> GraphBackendFuture {
    Box::pin(async move {
        CALLS.fetch_add(1, Ordering::SeqCst);
        // The factory receives the whole spec, tuning keys included — this is
        // the payload a real backend validates.
        assert_eq!(spec.get("kind").and_then(Value::as_str), Some("test-wired"));
        assert_eq!(spec.get("threshold").and_then(Value::as_f64), Some(0.25));
        Ok(Arc::new(StubExtractor("test-wired-extractor")) as Arc<dyn ChunkGraphExtractor>)
    })
}

/// End-to-end: registering a factory makes `{"graphBackend": …}` produce a
/// `CognifyConfig` carrying that extractor, which is what the run reads.
#[tokio::test(flavor = "multi_thread")]
async fn a_registered_factory_is_invoked_and_its_extractor_reaches_the_config() {
    register_graph_backend("test-wired", invoked_factory).expect("first registration");
    assert!(registered_graph_backend_kinds().contains(&"test-wired".to_owned()));

    let dir = TempDir::new().expect("tempdir");
    let svc = services_under(&dir).await;

    let before = CALLS.load(Ordering::SeqCst);
    let opts = json!({ "graphBackend": { "kind": "test-wired", "threshold": 0.25 } });
    let cfg = cognify_config_with_opts(&svc, &opts)
        .await
        .expect("registered backend builds");

    assert_eq!(
        CALLS.load(Ordering::SeqCst),
        before + 1,
        "the factory must actually run"
    );
    let handle = cfg
        .graph_backend
        .as_ref()
        .expect("the extractor must be attached to the config the run will use");
    assert_eq!(handle.0.name(), "test-wired-extractor");

    // The unrelated overrides still apply alongside it.
    let opts = json!({
        "graphBackend": { "kind": "test-wired", "threshold": 0.25 },
        "chunkSize": 321,
    });
    let cfg = cognify_config_with_opts(&svc, &opts).await.expect("config");
    assert_eq!(cfg.max_chunk_size, Some(321));
    assert!(cfg.graph_backend.is_some());
}

/// First writer wins, and the loser is handed back — the `set_handle_factory`
/// contract. A second crate silently replacing a live backend would be worse
/// than a rejected registration.
#[tokio::test(flavor = "multi_thread")]
async fn a_duplicate_registration_is_rejected_and_the_first_one_stands() {
    fn first(_spec: Value) -> GraphBackendFuture {
        Box::pin(async { Ok(Arc::new(StubExtractor("first")) as Arc<dyn ChunkGraphExtractor>) })
    }
    fn second(_spec: Value) -> GraphBackendFuture {
        Box::pin(async { Ok(Arc::new(StubExtractor("second")) as Arc<dyn ChunkGraphExtractor>) })
    }

    register_graph_backend("test-duplicate", first).expect("first registration");
    assert!(
        register_graph_backend("test-duplicate", second).is_err(),
        "a second registration under the same kind must be refused"
    );

    let opts = json!({ "graphBackend": "test-duplicate" });
    let backend = graph_backend_from_opts(&opts)
        .await
        .expect("resolves")
        .expect("a backend was asked for");
    assert_eq!(backend.name(), "first");
}

/// A factory's own failure propagates unchanged; it is not swallowed into a
/// fallback either.
#[tokio::test(flavor = "multi_thread")]
async fn a_factory_failure_propagates() {
    fn boom(_spec: Value) -> GraphBackendFuture {
        Box::pin(async { Err(SdkError::Runtime("model would not load".into())) })
    }
    register_graph_backend("test-boom", boom).expect("registration");

    let err = graph_backend_from_opts(&json!({ "graphBackend": "test-boom" }))
        .await
        .err()
        .expect("the factory failed");
    assert!(matches!(err, SdkError::Runtime(_)), "got {err:?}");
    assert!(err.to_string().contains("model would not load"), "{err}");
}

// ---------------------------------------------------------------------------
// 3. Inert for every existing caller.
// ---------------------------------------------------------------------------

/// No `graphBackend` key, or an explicit `null`, leaves the config exactly as
/// it was before this seam: no extractor attached, no error, nothing else
/// touched. This is every caller in the tree today.
#[tokio::test(flavor = "multi_thread")]
async fn opts_without_a_graph_backend_are_untouched() {
    let dir = TempDir::new().expect("tempdir");
    let svc = services_under(&dir).await;

    for opts in [
        Value::Null,
        json!({}),
        json!({ "graphBackend": null }),
        json!({ "chunkSize": 1234, "summarization": false, "triplet": true }),
    ] {
        let cfg = cognify_config_with_opts(&svc, &opts)
            .await
            .unwrap_or_else(|e| panic!("{opts} must not error: {e}"));
        assert!(
            cfg.graph_backend.is_none(),
            "{opts} must attach no extractor"
        );
        check_graph_backend_registered(&opts).unwrap_or_else(|e| panic!("{opts}: {e}"));
    }

    // And the existing overrides still land.
    let cfg = cognify_config_with_opts(&svc, &json!({ "chunkSize": 1234, "chunkOverlap": 7 }))
        .await
        .expect("config");
    assert_eq!(cfg.max_chunk_size, Some(1234));
    assert_eq!(cfg.chunk_overlap, 7);
    assert!(cfg.graph_backend.is_none());
}
