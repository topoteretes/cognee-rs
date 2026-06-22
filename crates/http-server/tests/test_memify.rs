#![cfg(any())] // cognee-http-server gated on oss-split branch (T2-move §4 S2).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for `POST /api/v1/memify`.
//!
//! Two flavours:
//!
//! 1. **Validation / wiring smoke tests** (no env required) — verify the route
//!    is mounted, auth gating works, and that the unit-level body-shape
//!    assertions live in `routers::memify::tests`.
//!
//! 2. **Blocking end-to-end test** — gated on `OPENAI_URL`, `OPENAI_TOKEN`,
//!    and `COGNEE_E2E_EMBED_MODEL_PATH`. Builds a real `ComponentHandles`
//!    (LocalStorage + sqlite + LadybugAdapter + QdrantAdapter +
//!    OnnxEmbeddingEngine + OpenAIAdapter + RayonThreadPool), seeds a dataset
//!    via `AddPipeline` + `cognify`, POSTs `/api/v1/memify`, and asserts that
//!    the `("Triplet", "text")` vector collection is non-empty afterwards —
//!    the downstream side effect that distinguishes a real memify run from
//!    the previous stub.

mod support;

use std::sync::Arc;

use axum::{
    Router,
    body::{self, Body},
    http::{Request, StatusCode, header},
};
use serde_json::json;
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;

use cognee_cognify::{ChunkStrategy, CognifyConfig, cognify as run_cognify};
use cognee_database::{IngestDb, NoopPipelineRunRepository, connect, initialize};

use cognee_graph::{GraphDBTrait, LadybugAdapter};
use cognee_http_server::components::ComponentHandles;
use cognee_http_server::{AppState, HttpServerConfig, build_router};
use cognee_ingestion::AddPipeline;
use cognee_llm::{Llm, OpenAIAdapter};
use cognee_models::DataInput;
use cognee_ontology::NoOpOntologyResolver;
use cognee_storage::{LocalStorage, StorageTrait};
use cognee_vector::{QdrantAdapter, VectorDB};

async fn test_app() -> Router {
    let state = AppState::build(HttpServerConfig::default())
        .await
        .expect("AppState::build");
    build_router(state).await.expect("build_router")
}

/// Without auth, `/memify` returns 401.
#[tokio::test]
async fn post_memify_no_auth_returns_401() {
    // Must use require_authentication=true; the default AppState allows anonymous users.
    let (state, _) = support::build_auth_required_test_state().await;
    let app = build_router(state).await.expect("build_router");

    let body = json!({ "dataset_name": "my_dataset" });
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/memify")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_string(&body).unwrap()))
        .unwrap();

    let resp = app.oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// Verify the route exists and mounts at /api/v1/memify.
#[tokio::test]
async fn post_memify_route_exists() {
    let app = test_app().await;

    // A JSON parse error (no body) should return 422 or 415, not 404.
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/memify")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.expect("oneshot");
    // 401 (auth required) or 422 (JSON parse error) — either proves route exists.
    assert_ne!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "route /api/v1/memify must exist"
    );
}

// ─── End-to-end blocking test ────────────────────────────────────────────────

/// Read an env var, loading `.env` first.  Matches the convention used by the
/// `cognee-cognify` integration tests so a single `.env` works for both.
fn maybe_env(name: &str) -> Option<String> {
    let _ = dotenv::dotenv();
    if let Ok(v) = std::env::var(name)
        && !v.is_empty()
    {
        return Some(v);
    }
    let canonical = match name {
        "OPENAI_TOKEN" => Some("LLM_API_KEY"),
        "OPENAI_URL" => Some("LLM_ENDPOINT"),
        "OPENAI_MODEL" => Some("LLM_MODEL"),
        _ => None,
    };
    canonical
        .and_then(|c| std::env::var(c).ok())
        .filter(|v| !v.is_empty())
}

/// Drive a full add → cognify → memify cycle through HTTP and assert that
/// triplets are indexed in the `("Triplet", "text")` vector collection.
///
/// This is the downstream side effect that distinguishes a real memify run
/// from the stubbed `Ok(())` future that used to live in `routers/memify.rs`.
#[tokio::test]
async fn post_memify_blocking_indexes_triplets() {
    // ── Env gate ─────────────────────────────────────────────────────────────
    let Some(openai_url) = maybe_env("OPENAI_URL") else {
        eprintln!("test_memify: skipping — OPENAI_URL not set");
        return;
    };
    let Some(openai_token) = maybe_env("OPENAI_TOKEN") else {
        eprintln!("test_memify: skipping — OPENAI_TOKEN not set");
        return;
    };
    let openai_model = maybe_env("OPENAI_MODEL").unwrap_or_else(|| "gpt-4o-mini".to_string());

    // ── Build backends ───────────────────────────────────────────────────────
    let temp_dir = TempDir::new().expect("temp dir");

    let Some((embedding_engine, embedding_dims)) =
        cognee_test_utils::create_test_embedding_engine().await
    else {
        eprintln!("test_memify: skipping — embedding engine unavailable");
        return;
    };

    let storage: Arc<dyn StorageTrait> =
        Arc::new(LocalStorage::new(temp_dir.path().join("storage")));
    storage.initialize().await.expect("storage.initialize");

    let db_path = temp_dir.path().join("cognee.db");
    std::fs::File::create(&db_path).expect("create sqlite db file");
    let db_url = format!("sqlite://{}", db_path.display());
    let db_conn = connect(&db_url).await.expect("connect");
    initialize(&db_conn).await.expect("initialize");
    let database = Arc::new(db_conn);

    let graph_path = temp_dir.path().join("graph").to_string_lossy().to_string();
    let graph_db: Arc<dyn GraphDBTrait> = Arc::new(
        LadybugAdapter::new(&graph_path)
            .await
            .expect("LadybugAdapter::new"),
    );
    graph_db.initialize().await.expect("graph_db.initialize");

    let vector_db: Arc<dyn VectorDB> = Arc::new(QdrantAdapter::new(
        temp_dir.path().join("qdrant"),
        embedding_dims,
    ));

    let llm: Arc<dyn Llm> = Arc::new(
        OpenAIAdapter::new(openai_model, openai_token, Some(openai_url))
            .expect("OpenAIAdapter::new"),
    );

    let thread_pool: Arc<dyn cognee_core::CpuPool> = Arc::new(
        cognee_core::RayonThreadPool::with_default_threads().expect("RayonThreadPool::new"),
    );

    // ── Seed the dataset via add + cognify so memify has triplets to index ──
    let owner_id = Uuid::nil();
    let dataset_name = "http_memify_blocking";

    let add_pipeline =
        AddPipeline::new(Arc::clone(&storage), database.clone() as Arc<dyn IngestDb>)
            .with_thread_pool(Arc::clone(&thread_pool))
            .with_graph_db(Arc::clone(&graph_db))
            .with_vector_db(Arc::clone(&vector_db))
            .with_database(Arc::clone(&database));

    add_pipeline
        .add(
            vec![DataInput::Text(
                "Alice met Bob in Paris. Bob then traveled to Berlin.".to_string(),
            )],
            dataset_name,
            owner_id,
            None,
        )
        .await
        .expect("add_pipeline.add");

    let dataset = cognee_database::ops::datasets::get_dataset_by_name(
        &database,
        dataset_name,
        owner_id,
        None,
    )
    .await
    .expect("get_dataset_by_name")
    .expect("dataset must exist after add");

    // Cognify the dataset directly via the library so the graph is populated
    // before we drive memify through HTTP.
    let data_items = cognee_database::ops::datasets::get_dataset_data(&database, dataset.id)
        .await
        .expect("get_dataset_data");

    let cognify_config = CognifyConfig::default().with_chunk_strategy(ChunkStrategy::Paragraph);
    let ontology_resolver: Arc<dyn cognee_ontology::OntologyResolver> =
        Arc::new(NoOpOntologyResolver::new());

    run_cognify(
        data_items,
        dataset.id,
        Some(owner_id),
        None,
        None,
        Arc::clone(&llm),
        Arc::clone(&storage),
        Arc::clone(&graph_db),
        Arc::clone(&vector_db),
        Arc::clone(&embedding_engine),
        Arc::clone(&database),
        NoopPipelineRunRepository::arc(),
        Arc::clone(&thread_pool),
        ontology_resolver,
        &cognify_config,
    )
    .await
    .expect("cognify seed");

    // ── Build AppState with backends wired into ComponentHandles ─────────────
    let delete_service = Arc::new(cognee_delete::DeleteService::new(
        Arc::clone(&storage),
        database.clone() as Arc<dyn cognee_database::DeleteDb>,
    ));
    let ontology_dir = tempfile::tempdir().expect("tmp ontology");
    let ontology_manager = Arc::new(cognee_ontology::OntologyManager::new(
        ontology_dir.path().to_path_buf(),
    ));
    Box::leak(Box::new(ontology_dir));

    let handles = Arc::new(ComponentHandles {
        database: Arc::clone(&database),
        acl_db: None,
        storage,
        delete_service,
        cloud_client: None,
        ontology_manager,
        search_orchestrator: None,
        llm: Some(llm),
        graph_db: Some(Arc::clone(&graph_db)),
        vector_db: Some(Arc::clone(&vector_db)),
        thread_pool: Some(thread_pool),
        embedding_engine: Some(embedding_engine),
        ontology_resolver: None,
        permissions: None,
        sync_ops: None,
        session_store: None,
        session_manager: None,
        checkpoint_store: None,
        responses_client: None,
        transcriber: None,
        notebook_runner: None,
    });

    // No auth context is wired, so the extractor falls back to the synthetic
    // default user at `Uuid::nil()` (see `auth/extractor.rs:default_user_from_state`),
    // which intentionally matches the `owner_id` used to seed the dataset.
    let cfg = HttpServerConfig {
        require_authentication: false,
        ..HttpServerConfig::default()
    };
    let mut state = AppState::build_with_db(cfg, Arc::clone(&database))
        .await
        .expect("AppState::build_with_db");
    state.lib = Some(handles);

    let app = build_router(state).await.expect("build_router");

    // ── Drive POST /api/v1/memify ───────────────────────────────────────────
    let body = json!({
        "datasetId": dataset.id.to_string(),
        "run_in_background": false,
    });
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/memify")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_string(&body).unwrap()))
        .unwrap();

    let resp = app.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let bytes = body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    let v: serde_json::Value = serde_json::from_slice(&bytes).expect("json");

    assert_eq!(
        status,
        StatusCode::OK,
        "expected 200 OK from /api/v1/memify, got {status} with body {v}"
    );

    assert_eq!(
        v["status"],
        "PipelineRunCompleted",
        "blocking memify must terminate as PipelineRunCompleted; body: {}",
        serde_json::to_string_pretty(&v).unwrap()
    );
    assert_eq!(v["dataset_id"], dataset.id.to_string());
    assert_eq!(v["dataset_name"], dataset_name);

    // ── Downstream assertion: triplets must be indexed in the vector DB ──────
    // The `("Triplet", "text")` collection is what `SearchType::TripletCompletion`
    // queries. A real memify run populates it; the previous stub left it empty.
    let triplet_size = graph_db
        .as_ref()
        .get_graph_data()
        .await
        .map(|(_, edges)| edges.len())
        .expect("graph_db.get_graph_data");
    assert!(
        triplet_size > 0,
        "graph must contain at least one edge after cognify+memify (got {triplet_size})"
    );

    let collection_size = vector_db
        .collection_size("Triplet", "text")
        .await
        .expect("vector_db.collection_size");
    assert!(
        collection_size > 0,
        "memify must populate the ('Triplet','text') vector collection \
         (got size {collection_size}); body: {}",
        serde_json::to_string_pretty(&v).unwrap()
    );
}
