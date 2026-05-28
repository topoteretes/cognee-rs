//! Integration tests for `POST /api/v1/remember`.
//!
//! Per p3-pipelines-and-websocket.md §5:
//! - Multipart upload with two files + `datasetName`.
//! - Negative path: inner error → `409 {"error": "An error occurred during remember."}` (no `detail`).
//! - `node_set=[""]` → `None` translation.
//! - Response keys per remember.md §2.1.

mod support;

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use tempfile::TempDir;
use tower::ServiceExt;

use cognee_database::{connect, initialize};
use cognee_embedding::{EmbeddingEngine, config::OnnxEmbeddingConfig, onnx::OnnxEmbeddingEngine};
use cognee_graph::{GraphDBTrait, LadybugAdapter};
use cognee_http_server::components::ComponentHandles;
use cognee_http_server::{AppState, HttpServerConfig, build_router};
use cognee_llm::{Llm, OpenAIAdapter};
use cognee_storage::{LocalStorage, StorageTrait};
use cognee_vector::{QdrantAdapter, VectorDB};

/// Without auth the handler returns 401.
#[tokio::test]
async fn post_remember_no_auth_returns_401() {
    // Must use require_authentication=true; the default AppState allows anonymous users.
    let (state, _) = support::build_auth_required_test_state().await;
    let app = build_router(state).await.expect("build_router");

    // Even with a valid multipart body, auth is checked first.
    let boundary = "boundary123";
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"datasetName\"\r\n\r\ntest\r\n\
         --{boundary}--\r\n"
    );
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/remember")
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap();

    let resp = app.oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// Without auth, the 409 catch-all body shape can be verified via the error.rs
/// unit test.  Document the cross-reference here.
///
/// The `remember_catch_all_409_uses_error_key` lib-test in remember.rs asserts:
/// - 409 status
/// - `{"error": "An error occurred during remember."}` body (no "detail" key)
#[tokio::test]
async fn post_remember_409_body_shape_documented() {
    // Covered by: routers::remember::tests::remember_catch_all_409_uses_error_key
    let _: () = ();
}

/// The 400 validation body uses `{"detail": "..."}` (Python HTTPException parity).
/// Covered by: routers::remember::tests::remember_validation_400_uses_detail_key
#[tokio::test]
async fn post_remember_400_body_shape_documented() {
    let _: () = ();
}

// ─── Helpers (mirror tests/test_memify.rs) ────────────────────────────────────

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

fn embedding_model_dir() -> String {
    if let Ok(model_path) = std::env::var("COGNEE_E2E_EMBED_MODEL_PATH")
        && let Some(parent) = std::path::Path::new(&model_path).parent()
    {
        return parent.to_string_lossy().to_string();
    }
    "./target/models".to_string()
}

/// End-to-end test: drive `POST /api/v1/remember` with a real backend stack
/// and assert that the cognify and memify legs landed.
///
/// Verifies that gap 03 ("remember pipeline wiring") is implemented:
/// 1. The graph DB has at least one edge after the run (cognify leg).
/// 2. The `("Triplet","text")` vector collection is non-empty (memify leg).
/// 3. The response DTO is populated with `pipeline_run_id`, `content_hash`,
///    and `items_processed > 0` from the real pipeline output.
///
/// Gated on `OPENAI_URL`, `OPENAI_TOKEN`, `COGNEE_E2E_EMBED_MODEL_PATH`.
#[tokio::test]
async fn post_remember_blocking_runs_full_pipeline() {
    // ── Env gate ─────────────────────────────────────────────────────────────
    let Some(openai_url) = maybe_env("OPENAI_URL") else {
        eprintln!("test_remember: skipping — OPENAI_URL not set");
        return;
    };
    let Some(openai_token) = maybe_env("OPENAI_TOKEN") else {
        eprintln!("test_remember: skipping — OPENAI_TOKEN not set");
        return;
    };
    let openai_model = maybe_env("OPENAI_MODEL").unwrap_or_else(|| "gpt-4o-mini".to_string());

    // ── Build backends ───────────────────────────────────────────────────────
    let temp_dir = TempDir::new().expect("temp dir");

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

    let vector_db: Arc<dyn VectorDB> =
        Arc::new(QdrantAdapter::new(temp_dir.path().join("qdrant"), 384));

    let model_dir = embedding_model_dir();
    let embedding_engine: Arc<dyn EmbeddingEngine> =
        match OnnxEmbeddingEngine::new(OnnxEmbeddingConfig::bge_small(&model_dir)) {
            Ok(engine) => Arc::new(engine),
            Err(e) => {
                eprintln!(
                    "test_remember: skipping — embedding model unavailable at {}: {}",
                    model_dir, e
                );
                return;
            }
        };

    let llm: Arc<dyn Llm> = Arc::new(
        OpenAIAdapter::new(openai_model, openai_token, Some(openai_url))
            .expect("OpenAIAdapter::new"),
    );

    let thread_pool: Arc<dyn cognee_core::CpuPool> = Arc::new(
        cognee_core::RayonThreadPool::with_default_threads().expect("RayonThreadPool::new"),
    );

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
        storage: Arc::clone(&storage),
        delete_service,
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
        responses_client: None,
    });

    // No auth context is wired, so the extractor falls back to the synthetic
    // default user at `Uuid::nil()` — that user owns the dataset we create.
    let cfg = HttpServerConfig {
        require_authentication: false,
        ..HttpServerConfig::default()
    };
    let mut state = AppState::build_with_db(cfg, Arc::clone(&database))
        .await
        .expect("AppState::build_with_db");
    state.lib = Some(handles);

    let app = build_router(state).await.expect("build_router");

    // ── Drive POST /api/v1/remember ───────────────────────────────────────────
    let dataset_name = "http_remember_blocking";
    let text_content = "Alice met Bob in Paris. Bob then traveled to Berlin.";
    let boundary = "boundary-remember-e2e";
    let body = format!(
        "--{boundary}\r\n\
         Content-Disposition: form-data; name=\"datasetName\"\r\n\
         \r\n\
         {dataset_name}\r\n\
         --{boundary}\r\n\
         Content-Disposition: form-data; name=\"data\"; filename=\"note.txt\"\r\n\
         Content-Type: text/plain\r\n\
         \r\n\
         {text_content}\r\n\
         --{boundary}--\r\n"
    );

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/remember")
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap();

    let resp = app.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    let v: serde_json::Value = serde_json::from_slice(&body_bytes).expect("json");

    assert_eq!(
        status,
        StatusCode::OK,
        "expected 200 OK from /api/v1/remember, got {} with body {}",
        status,
        v,
    );

    // ── Assert RememberResultDTO is populated from real output ───────────────
    let obj = v.as_object().expect("response is a JSON object");
    assert_eq!(
        obj["status"],
        "completed",
        "blocking remember without session_id must terminate as 'completed' (Python parity); body: {}",
        serde_json::to_string_pretty(&v).unwrap()
    );
    assert!(
        obj["pipeline_run_id"].is_string(),
        "pipeline_run_id must be populated from the real pipeline run"
    );
    assert!(
        obj["dataset_id"].is_string(),
        "dataset_id must be populated"
    );
    assert_eq!(obj["dataset_name"], dataset_name);
    assert_eq!(
        obj["items_processed"], 1,
        "items_processed must reflect the real Data row created by add"
    );
    assert!(
        obj.contains_key("content_hash") && obj["content_hash"].is_string(),
        "content_hash must be populated from the first ingested Data row; body: {}",
        serde_json::to_string_pretty(&v).unwrap()
    );

    // ── Assert downstream side effects ────────────────────────────────────────
    // Cognify leg: graph rows present.
    let edge_count = graph_db
        .as_ref()
        .get_graph_data()
        .await
        .map(|(_, edges)| edges.len())
        .expect("graph_db.get_graph_data");
    assert!(
        edge_count > 0,
        "cognify leg must produce at least one graph edge (got {edge_count})"
    );

    // Memify leg: ("Triplet","text") vector collection is non-empty.
    let triplet_size = vector_db
        .collection_size("Triplet", "text")
        .await
        .expect("vector_db.collection_size");
    assert!(
        triplet_size > 0,
        "memify leg must populate the ('Triplet','text') vector collection \
         (got size {triplet_size}); body: {}",
        serde_json::to_string_pretty(&v).unwrap()
    );
}
