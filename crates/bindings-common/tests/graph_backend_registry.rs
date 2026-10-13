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
use cognee::{
    BackendBuildContext, ComponentError, ComponentManager, ComponentRegistry, ConfigManager,
    GraphExtractorFactory,
};
use cognee_bindings_common::graph_backend::{
    GraphBackendFuture, available_graph_backend_kinds, check_graph_backend_registered,
    graph_backend_from_opts, register_graph_backend, registered_graph_backend_kinds,
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

/// A manager with default settings: the resolver tests never build services.
fn bare_cm() -> ComponentManager {
    ComponentManager::new(ConfigManager::new(Settings::default()))
}

/// Hermetic settings: every store under a temp dir, a dummy key so the strict
/// LLM resolve succeeds without network I/O, and the mock embedding provider
/// so warming never downloads.
fn settings_under(dir: &TempDir) -> Settings {
    let root = dir.path();
    Settings {
        sections: [
            (
                "embedding.provider".to_string(),
                serde_json::json!("mock".to_owned()),
            ),
            (
                "llm.api_key".to_string(),
                serde_json::json!("sk-test".to_owned()),
            ),
        ]
        .into(),
        data_root_directory: root.join("data").to_string_lossy().into_owned(),
        system_root_directory: root.join("sys").to_string_lossy().into_owned(),
        relational_db_url: format!("sqlite://{}?mode=rwc", root.join("cognee.db").display()),
        ..Settings::default()
    }
}

/// Hermetic services, built the way `handle_close.rs` builds them: every store
/// under a temp dir, a dummy key so the strict LLM resolve succeeds without
/// network I/O, and the mock embedding provider so warming never downloads.
async fn services_under(dir: &TempDir) -> Arc<CogneeServices> {
    HandleState::from_settings(settings_under(dir))
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
    let cm = bare_cm();
    let opts = json!({ "graphBackend": { "kind": "absent-kind-xyz", "threshold": 0.5 } });
    let err = graph_backend_from_opts(&cm, &opts)
        .await
        .err()
        .expect("unregistered kind");
    assert!(err.to_string().contains("absent-kind-xyz"), "{err}");

    // And the cheap pre-flight agrees, so `add_and_cognify` refuses before it
    // has ingested anything.
    let err = check_graph_backend_registered(&cm, &opts).expect_err("unregistered kind");
    assert!(err.to_string().contains("absent-kind-xyz"), "{err}");
}

/// A malformed selection is an error, never read as "no selection". Silently
/// dropping it is the same silent-LLM fallback by another route.
#[tokio::test(flavor = "multi_thread")]
async fn a_malformed_selection_is_refused_rather_than_ignored() {
    let cm = bare_cm();
    for opts in [
        json!({ "graphBackend": 7 }),
        json!({ "graphBackend": [] }),
        json!({ "graphBackend": { "threshold": 0.5 } }),
        json!({ "graphBackend": { "kind": 7 } }),
    ] {
        let err = graph_backend_from_opts(&cm, &opts)
            .await
            .err()
            .unwrap_or_else(|| panic!("{opts} must be refused, not ignored"));
        assert!(matches!(err, SdkError::Validation(_)), "{opts}: {err:?}");
        check_graph_backend_registered(&cm, &opts)
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
    let backend = graph_backend_from_opts(&bare_cm(), &opts)
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

    let err = graph_backend_from_opts(&bare_cm(), &json!({ "graphBackend": "test-boom" }))
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
        check_graph_backend_registered(&svc.component_manager, &opts)
            .unwrap_or_else(|e| panic!("{opts}: {e}"));
    }

    // And the existing overrides still land.
    let cfg = cognify_config_with_opts(&svc, &json!({ "chunkSize": 1234, "chunkOverlap": 7 }))
        .await
        .expect("config");
    assert_eq!(cfg.max_chunk_size, Some(1234));
    assert_eq!(cfg.chunk_overlap, 7);
    assert!(cfg.graph_backend.is_none());
}

// ---------------------------------------------------------------------------
// 4. Backends on the handle's component registry, and the configured default.
// ---------------------------------------------------------------------------

cognee_config_core::config_section! {
    /// A registry-registered backend's own setting.
    pub struct StubExtractorConfig in "graph_extraction.stub" {
        /// Name the stub extractor reports.
        pub label: String = "stub-default".to_string() => {
            env: [], alias: [], secret: false,
        },
    }
}

/// A registry factory: names its extractor after its configured label, and
/// after a per-call `label` when the spec carries one.
struct StubExtractorFactory;

#[async_trait::async_trait]
impl GraphExtractorFactory for StubExtractorFactory {
    fn provider(&self) -> &str {
        "stub-registry"
    }

    fn config_sections(&self) -> Vec<&'static cognee_config_core::SectionSchema> {
        vec![<StubExtractorConfig as cognee_config_core::ConfigSection>::schema()]
    }

    async fn build(
        &self,
        ctx: &BackendBuildContext,
        spec: &Value,
    ) -> Result<Arc<dyn ChunkGraphExtractor>, ComponentError> {
        let label = spec
            .get("label")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| ctx.section::<StubExtractorConfig>().label);
        Ok(Arc::new(StubExtractor(Box::leak(label.into_boxed_str()))))
    }
}

fn stub_registry() -> ComponentRegistry {
    let mut registry = ComponentRegistry::with_builtins();
    registry
        .register_graph_extractor(Arc::new(StubExtractorFactory))
        .expect("not the reserved kind");
    registry
}

/// A registry factory is reachable from the opts, its config section joins the
/// handle's schema, and its configured setting reaches the build.
#[tokio::test(flavor = "multi_thread")]
async fn a_registry_factory_serves_the_opt_and_registers_its_settings() {
    let dir = TempDir::new().expect("tempdir");
    let state = HandleState::from_settings_with_registry(settings_under(&dir), stub_registry());
    state
        .cm
        .config()
        .set("graph_extraction.stub.label", json!("configured"))
        .expect("the factory's key is known");
    let svc = state.services().await.expect("warm");

    let cfg = cognify_config_with_opts(&svc, &json!({ "graphBackend": "stub-registry" }))
        .await
        .expect("registered backend builds");
    let handle = cfg.graph_backend.as_ref().expect("extractor attached");
    assert_eq!(handle.0.name(), "configured");

    let opts = json!({ "graphBackend": { "kind": "stub-registry", "label": "per-call" } });
    let cfg = cognify_config_with_opts(&svc, &opts).await.expect("config");
    assert_eq!(
        cfg.graph_backend.as_ref().expect("attached").0.name(),
        "per-call"
    );

    assert!(
        available_graph_backend_kinds(&svc.component_manager).contains(&"stub-registry".to_owned())
    );
}

/// Without a per-call selection, `graph_extraction.backend` picks the
/// extractor; a per-call `llm` selects the built-in one regardless.
#[tokio::test(flavor = "multi_thread")]
async fn the_configured_default_applies_and_llm_overrides_it() {
    let dir = TempDir::new().expect("tempdir");
    let state = HandleState::from_settings_with_registry(settings_under(&dir), stub_registry());
    state
        .cm
        .config()
        .set("graph_extractor", json!("stub-registry"))
        .expect("the Python-compatible alias is known");
    let svc = state.services().await.expect("warm");

    let cfg = cognify_config_with_opts(&svc, &json!({}))
        .await
        .expect("config");
    assert_eq!(
        cfg.graph_backend
            .as_ref()
            .expect("the default backend")
            .0
            .name(),
        "stub-default"
    );

    let cfg = cognify_config_with_opts(&svc, &json!({ "graphBackend": "llm" }))
        .await
        .expect("config");
    assert!(
        cfg.graph_backend.is_none(),
        "`llm` is the built-in extractor"
    );
}

/// A misconfigured default is refused by the pre-flight, like a misspelled
/// per-call selection.
#[tokio::test(flavor = "multi_thread")]
async fn an_unregistered_default_is_refused() {
    let cm = bare_cm();
    cm.config()
        .set("graph_extraction.backend", json!("absent-default-xyz"))
        .expect("known key");
    let err = check_graph_backend_registered(&cm, &json!({})).expect_err("unregistered default");
    assert!(err.to_string().contains("absent-default-xyz"), "{err}");
}

/// `llm` names the built-in extractor; neither registry can take it.
#[test]
fn the_llm_kind_is_reserved() {
    fn any(_spec: Value) -> GraphBackendFuture {
        Box::pin(async { Ok(Arc::new(StubExtractor("x")) as Arc<dyn ChunkGraphExtractor>) })
    }
    assert!(register_graph_backend("llm", any).is_err());

    struct LlmNamed;
    #[async_trait::async_trait]
    impl GraphExtractorFactory for LlmNamed {
        fn provider(&self) -> &str {
            "LLM"
        }
        async fn build(
            &self,
            _ctx: &BackendBuildContext,
            _spec: &Value,
        ) -> Result<Arc<dyn ChunkGraphExtractor>, ComponentError> {
            unreachable!("never registered")
        }
    }
    let mut registry = ComponentRegistry::with_builtins();
    assert!(
        registry
            .register_graph_extractor(Arc::new(LlmNamed))
            .is_err()
    );
}

// ---------------------------------------------------------------------------
// 5. Every cognify-bearing operation honours the configured default.
// ---------------------------------------------------------------------------

/// `remember`, `improve` and `update` cognify as a sub-step, so they must
/// resolve `graph_extraction.backend` like `cognify` does — an operation that
/// skipped it would silently extract with the LLM where `cognify` refuses.
/// Each must refuse an unregistered default before writing or deleting
/// anything.
#[tokio::test(flavor = "multi_thread")]
async fn an_unregistered_default_is_refused_by_every_cognify_bearing_op() {
    let dir = TempDir::new().expect("tempdir");
    let state = HandleState::from_settings(settings_under(&dir));
    state
        .cm
        .config()
        .set("graph_extraction.backend", json!("absent-op-default-xyz"))
        .expect("known key");

    let input = json!({ "type": "text", "text": "Alice knows Bob." });

    let err = cognee_bindings_common::ops::memory::run_remember(
        &state,
        input.clone(),
        "remember_ds",
        &json!({}),
    )
    .await
    .expect_err("remember must refuse an unregistered default");
    assert!(matches!(err, SdkError::Validation(_)), "got {err:?}");
    assert!(err.to_string().contains("absent-op-default-xyz"), "{err}");

    let err = cognee_bindings_common::ops::memory::run_improve(
        &state,
        &json!({ "datasetName": "remember_ds" }),
    )
    .await
    .expect_err("improve must refuse an unregistered default");
    assert!(matches!(err, SdkError::Validation(_)), "got {err:?}");
    assert!(err.to_string().contains("absent-op-default-xyz"), "{err}");

    let err = cognee_bindings_common::ops::data::update(
        &state,
        &uuid::Uuid::new_v4().to_string(),
        input,
        "remember_ds",
        &json!({}),
    )
    .await
    .expect_err("update must refuse an unregistered default");
    assert!(matches!(err, SdkError::Validation(_)), "got {err:?}");
    assert!(err.to_string().contains("absent-op-default-xyz"), "{err}");

    // Refused before side effects: `remember` ingested nothing.
    let datasets = cognee_bindings_common::ops::datasets::list_datasets(&state)
        .await
        .expect("list datasets");
    assert_eq!(
        datasets.as_array().map(Vec::len),
        Some(0),
        "remember must not ingest before refusing: {datasets}"
    );
}

/// The sub-step resolution attaches the configured default backend, and a
/// per-call `graphBackend` still wins over it.
#[tokio::test(flavor = "multi_thread")]
async fn the_sub_step_config_carries_the_configured_default() {
    use cognee_bindings_common::ops::pipeline::cognify_config_with_graph_backend;

    let dir = TempDir::new().expect("tempdir");
    let state = HandleState::from_settings_with_registry(settings_under(&dir), stub_registry());
    state
        .cm
        .config()
        .set("graph_extractor", json!("stub-registry"))
        .expect("known alias");
    let svc = state.services().await.expect("warm");

    let cfg = cognify_config_with_graph_backend(&svc, &json!({ "chunkSize": 99 }))
        .await
        .expect("config");
    assert_eq!(
        cfg.graph_backend
            .as_ref()
            .expect("default backend")
            .0
            .name(),
        "stub-default"
    );
    assert_ne!(
        cfg.max_chunk_size,
        Some(99),
        "sub-step opts are not cognify chunking opts"
    );

    let cfg = cognify_config_with_graph_backend(&svc, &json!({ "graphBackend": "llm" }))
        .await
        .expect("config");
    assert!(cfg.graph_backend.is_none());

    // The library-side accessor the CLI uses resolves the same default.
    let backend = svc
        .component_manager
        .configured_graph_extractor()
        .await
        .expect("registered")
        .expect("a backend, not the LLM");
    assert_eq!(backend.name(), "stub-default");
}
