#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration test: `POST /api/v1/cognify` with `run_in_background=false`.
//!
//! Requires an OpenAI-compatible LLM endpoint (`OPENAI_URL` + `OPENAI_TOKEN`)
//! and a local ONNX embedding model (`COGNEE_E2E_EMBED_MODEL_PATH`). The test
//! skips gracefully when those env vars are absent, matching the project's
//! existing test convention.
//!
//! What this exercises end-to-end:
//! 1. Build a real `ComponentHandles` with `LocalStorage`, an in-memory
//!    SQLite DB, `LadybugAdapter`, `MockVectorDB`, `OnnxEmbeddingEngine`,
//!    `OpenAIAdapter`, and `RayonThreadPool`.
//! 2. Seed the dataset via `AddPipeline` (so the cognify dataset lookup
//!    finds matching rows).
//! 3. POST `/api/v1/cognify` with `run_in_background=false` and assert the
//!    response shape: `Map<dataset_id_str, PipelineRunInfoDTO>` with
//!    `status="PipelineRunCompleted"`.
//!
//! Without this test, the stub `Ok(())` future inside `post_cognify` could
//! silently mask a missing implementation — which was exactly the state of
//! the code before the wiring landed (see `routers/cognify.rs:run_real_cognify`).

mod support;

use std::sync::Arc;

use axum::{
    body::{self, Body},
    http::{Request, StatusCode, header},
};
use serde_json::json;
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;

use cognee_database::{IngestDb, connect, initialize};

use cognee_graph::{GraphDBTrait, LadybugAdapter};
use cognee_http_server::components::ComponentHandles;
use cognee_http_server::{AppState, HttpServerConfig, build_router};
use cognee_ingestion::AddPipeline;
use cognee_llm::{Llm, build_openai_compatible_adapter};
use cognee_models::DataInput;
use cognee_storage::{LocalStorage, StorageTrait};
use cognee_test_utils::MockVectorDB;
use cognee_vector::VectorDB;

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

#[tokio::test]
async fn post_cognify_blocking_executes_real_pipeline() {
    // ── Env gate ─────────────────────────────────────────────────────────────
    let Some(openai_url) = maybe_env("OPENAI_URL") else {
        eprintln!("test_cognify_blocking: skipping — OPENAI_URL not set");
        return;
    };
    let Some(openai_token) = maybe_env("OPENAI_TOKEN") else {
        eprintln!("test_cognify_blocking: skipping — OPENAI_TOKEN not set");
        return;
    };
    let openai_model = maybe_env("OPENAI_MODEL").unwrap_or_else(|| "gpt-4o-mini".to_string());

    // ── Build backends ───────────────────────────────────────────────────────
    let temp_dir = TempDir::new().expect("temp dir");

    let Some((embedding_engine, _embedding_dims)) =
        cognee_test_utils::create_test_embedding_engine().await
    else {
        eprintln!("test_cognify_blocking: skipping — embedding engine unavailable");
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

    // In-memory mock vector DB (qdrant extracted to closed cognee-vector-qdrant).
    let vector_db: Arc<dyn VectorDB> = Arc::new(MockVectorDB::new());

    // Route through the production factory (provider from env, default `openai`)
    // so litellm-style model prefixes are stripped exactly as in a real run.
    let provider = std::env::var("LLM_PROVIDER").unwrap_or_else(|_| "openai".to_string());
    let llm: Arc<dyn Llm> = Arc::new(
        build_openai_compatible_adapter(&provider, &openai_model, &openai_token, &openai_url, 3)
            .expect("build_openai_compatible_adapter"),
    );

    let thread_pool: Arc<dyn cognee_core::CpuPool> = Arc::new(
        cognee_core::RayonThreadPool::with_default_threads().expect("RayonThreadPool::new"),
    );

    // ── Seed the dataset via the add pipeline ────────────────────────────────
    // The HTTP extractor falls back to the synthetic default user when
    // `require_authentication=false`, deriving owner id as
    // `uuid5(NAMESPACE_OID, default_user_email)` to match Python parity
    // (see `auth/extractor.rs:default_user_from_state`). Seed with that
    // same derived id so the cognify request finds the dataset.
    let owner_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, "default_user@example.com".as_bytes());
    let dataset_name = "http_cognify_blocking";

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
        graph_db: Some(graph_db),
        vector_db: Some(vector_db),
        thread_pool: Some(thread_pool),
        embedding_engine: Some(embedding_engine),
        ontology_resolver: None,
        session_store: None,
        session_manager: None,
        checkpoint_store: None,
        responses_client: None,
        transcriber: None,
        notebook_runner: None,
    });

    // No auth context is wired, so the extractor falls back to the synthetic
    // default user whose id is `uuid5(NAMESPACE_OID, default_user_email)`
    // (see `auth/extractor.rs:default_user_from_state`), which intentionally
    // matches the `owner_id` used to seed the dataset above.
    let cfg = HttpServerConfig {
        require_authentication: false,
        ..HttpServerConfig::default()
    };
    let mut state = AppState::build_with_db(cfg, Arc::clone(&database))
        .await
        .expect("AppState::build_with_db");
    state.lib = Some(handles);

    let app = build_router(state).await.expect("build_router");

    // ── Drive POST /api/v1/cognify ──────────────────────────────────────────
    let body = json!({
        "dataset_ids": [dataset.id],
        "run_in_background": false,
    });
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/cognify")
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
        "expected 200 OK from /api/v1/cognify, got {status} with body {v}"
    );

    let entry = v.get(dataset.id.to_string()).unwrap_or_else(|| {
        panic!(
            "response must be keyed by dataset_id; got body: {}",
            serde_json::to_string_pretty(&v).unwrap()
        )
    });
    assert_eq!(
        entry["status"],
        "PipelineRunCompleted",
        "blocking cognify must terminate as PipelineRunCompleted; body: {}",
        serde_json::to_string_pretty(&v).unwrap()
    );
    assert_eq!(entry["dataset_id"], dataset.id.to_string());
    assert_eq!(entry["dataset_name"], dataset_name);
}
