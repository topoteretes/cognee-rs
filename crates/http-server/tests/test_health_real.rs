//! Integration tests for the `RealHealthChecker` wired through
//! `AppState::install_real_health_checker`.
//!
//! These tests build a `ComponentHandles` backed by in-memory SQLite +
//! `LocalStorage` + `MockGraphDB` + `MockVectorDB`, then verify
//! `GET /health` and `GET /health/detailed` reflect the live state of
//! those backends (not the `MockHealthChecker` placeholder).

mod support;

use std::sync::Arc;

use axum::http::StatusCode;
use cognee_database::{DatabaseConnection, connect, initialize};
use cognee_delete::DeleteService;
use cognee_graph::{GraphDBTrait, MockGraphDB};
use cognee_http_server::{AppState, HttpServerConfig, build_router, components::ComponentHandles};
use cognee_ontology::OntologyManager;
use cognee_storage::{LocalStorage, StorageTrait};
use cognee_vector::{MockVectorDB, VectorDB};

/// Build a fully-populated `ComponentHandles` whose backends are all
/// healthy in-memory mocks.
async fn build_real_handles() -> (Arc<ComponentHandles>, Arc<DatabaseConnection>) {
    let db: Arc<DatabaseConnection> = Arc::new(
        connect("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    initialize(&db).await.expect("run migrations");

    let storage_dir = tempfile::tempdir().expect("tmp storage");
    let storage: Arc<dyn StorageTrait> =
        Arc::new(LocalStorage::new(storage_dir.path().to_path_buf()));
    Box::leak(Box::new(storage_dir));

    let delete_service = Arc::new(DeleteService::new(
        Arc::clone(&storage),
        db.clone() as Arc<dyn cognee_database::DeleteDb>,
    ));
    let ontology_dir = tempfile::tempdir().expect("tmp ontology");
    let ontology_manager = Arc::new(OntologyManager::new(ontology_dir.path().to_path_buf()));
    Box::leak(Box::new(ontology_dir));

    let graph_db: Arc<dyn GraphDBTrait> = Arc::new(MockGraphDB::new());
    let vector_db: Arc<dyn VectorDB> = Arc::new(MockVectorDB::new());

    let handles = Arc::new(ComponentHandles {
        database: Arc::clone(&db),
        storage,
        delete_service,
        cloud_client: None,
        ontology_manager,
        search_orchestrator: None,
        llm: None,
        embedding_engine: None,
        graph_db: Some(graph_db),
        vector_db: Some(vector_db),
        thread_pool: None,
        permissions: None,
        sync_ops: None,
        session_store: None,
        session_manager: None,
        checkpoint_store: None,
        ontology_resolver: None,
        responses_client: None,
        transcriber: None,
        notebook_runner: None,
    });

    (handles, db)
}

/// Build a router wired with the real health checker.
async fn router_with_real_checker(cfg: HttpServerConfig) -> axum::Router {
    let (handles, _db) = build_real_handles().await;
    let mut state = AppState::build(cfg).await.expect("state");
    state.lib = Some(handles);
    state.install_real_health_checker();
    build_router(state).await.expect("router")
}

// ── Happy path ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn real_checker_healthy_state_returns_200_with_components() {
    let cfg = HttpServerConfig {
        cors_allowed_origins: vec![],
        health_probe_timeout_ms: 1000,
        health_cache_ttl_ms: 0,
        ..HttpServerConfig::default()
    };
    let app = router_with_real_checker(cfg).await;
    let resp = support::oneshot_get(app, "/health/detailed").await;

    assert_eq!(resp.status(), StatusCode::OK);
    let body = support::body_json(resp).await;
    assert_eq!(body["status"], "healthy");

    for key in &["relational_db", "vector_db", "graph_db", "file_storage"] {
        let entry = &body["components"][key];
        assert!(entry.is_object(), "missing component {key} in {body}");
        assert_eq!(
            entry["status"], "healthy",
            "component {key} should be healthy, got {entry}"
        );
    }
}

#[tokio::test]
async fn real_checker_shallow_returns_ready_when_healthy() {
    let cfg = HttpServerConfig {
        cors_allowed_origins: vec![],
        health_probe_timeout_ms: 1000,
        health_cache_ttl_ms: 0,
        ..HttpServerConfig::default()
    };
    let app = router_with_real_checker(cfg).await;
    let resp = support::oneshot_get(app, "/health").await;

    assert_eq!(resp.status(), StatusCode::OK);
    let body = support::body_json(resp).await;
    assert_eq!(body["status"], "ready");
    assert_eq!(body["health"], "healthy");
}

// ── Degraded path: missing graph backend ─────────────────────────────────────

#[tokio::test]
async fn real_checker_missing_graph_returns_503_on_detailed() {
    // Build handles WITHOUT graph_db — should yield Unhealthy for graph
    // (critical) → 503 on detailed.
    let db: Arc<DatabaseConnection> = Arc::new(
        connect("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    initialize(&db).await.expect("run migrations");

    let storage_dir = tempfile::tempdir().expect("tmp storage");
    let storage: Arc<dyn StorageTrait> =
        Arc::new(LocalStorage::new(storage_dir.path().to_path_buf()));
    Box::leak(Box::new(storage_dir));
    let delete_service = Arc::new(DeleteService::new(
        Arc::clone(&storage),
        db.clone() as Arc<dyn cognee_database::DeleteDb>,
    ));
    let ontology_dir = tempfile::tempdir().expect("tmp ontology");
    let ontology_manager = Arc::new(OntologyManager::new(ontology_dir.path().to_path_buf()));
    Box::leak(Box::new(ontology_dir));
    let vector_db: Arc<dyn VectorDB> = Arc::new(MockVectorDB::new());

    let handles = Arc::new(ComponentHandles {
        database: Arc::clone(&db),
        storage,
        delete_service,
        cloud_client: None,
        ontology_manager,
        search_orchestrator: None,
        llm: None,
        embedding_engine: None,
        graph_db: None, // <- missing critical backend
        vector_db: Some(vector_db),
        thread_pool: None,
        permissions: None,
        sync_ops: None,
        session_store: None,
        session_manager: None,
        checkpoint_store: None,
        ontology_resolver: None,
        responses_client: None,
        transcriber: None,
        notebook_runner: None,
    });

    let cfg = HttpServerConfig {
        cors_allowed_origins: vec![],
        health_probe_timeout_ms: 1000,
        health_cache_ttl_ms: 0,
        ..HttpServerConfig::default()
    };
    let mut state = AppState::build(cfg).await.expect("state");
    state.lib = Some(handles);
    state.install_real_health_checker();
    let app = build_router(state).await.expect("router");

    let resp = support::oneshot_get(app, "/health/detailed").await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = support::body_json(resp).await;
    assert_eq!(body["status"], "unhealthy");
    assert_eq!(body["components"]["graph_db"]["status"], "unhealthy");
}

// ── Regression guard: ensure the synthetic Mock entries are no longer served
// when the real checker is installed. ───────────────────────────────────────

#[tokio::test]
async fn real_checker_replaces_synthetic_mock_entries() {
    let cfg = HttpServerConfig {
        cors_allowed_origins: vec![],
        health_probe_timeout_ms: 1000,
        health_cache_ttl_ms: 0,
        ..HttpServerConfig::default()
    };
    let app = router_with_real_checker(cfg).await;
    let resp = support::oneshot_get(app, "/health/detailed").await;

    let body = support::body_json(resp).await;
    // MockHealthChecker reports provider="mock" and details="mock health
    // check" for every component. RealHealthChecker must not.
    for key in &["relational_db", "vector_db", "graph_db", "file_storage"] {
        let entry = &body["components"][key];
        assert!(
            entry["provider"].as_str().unwrap_or("") != "mock",
            "{key} still served from MockHealthChecker (provider=mock)"
        );
        assert!(
            entry["details"].as_str().unwrap_or("") != "mock health check",
            "{key} still served from MockHealthChecker placeholder details"
        );
    }
}

// ── Cache behaviour ──────────────────────────────────────────────────────────

#[tokio::test]
async fn real_checker_cache_serves_repeat_requests() {
    let cfg = HttpServerConfig {
        cors_allowed_origins: vec![],
        health_probe_timeout_ms: 1000,
        health_cache_ttl_ms: 10_000,
        ..HttpServerConfig::default()
    };
    let app = router_with_real_checker(cfg).await;

    let first = support::oneshot_get(app.clone(), "/health/detailed").await;
    assert_eq!(first.status(), StatusCode::OK);
    let first_body = support::body_json(first).await;
    let second = support::oneshot_get(app, "/health/detailed").await;
    assert_eq!(second.status(), StatusCode::OK);
    let second_body = support::body_json(second).await;

    // The cached report carries the same timestamp.
    assert_eq!(first_body["timestamp"], second_body["timestamp"]);
}
