#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! End-to-end tests for `improve()` orchestrator.
//!
//! These tests exercise the stage-gate logic (which stages run based on the
//! presence of `session_ids` and available backends) using mock
//! storage/graph/vector/embedding/LLM backends. They do NOT exercise the full
//! LLM-driven cognify pipeline — that is covered by the per-stage integration
//! tests in `cognee-cognify`.

use std::sync::Arc;

use cognee_cognify::CognifyConfig;
use cognee_database::{DatabaseConnection, IngestDb, SeaOrmCheckpointStore, connect, initialize};
use cognee_embedding::MockEmbeddingEngine;
use cognee_graph::MockGraphDB;
use cognee_ingestion::AddPipeline;
use cognee_lib::api::improve::{ImproveParams, improve};
use cognee_ontology::{NoOpOntologyResolver, OntologyResolver};
use cognee_session::{FsSessionStore, SessionManager, SessionStore};
use cognee_storage::{LocalStorage, StorageTrait};
use cognee_test_utils::MockLlm;
use cognee_vector::MockVectorDB;
use tempfile::TempDir;
use uuid::Uuid;

struct Harness {
    _temp: TempDir,
    _sess_dir: TempDir,
    db: Arc<DatabaseConnection>,
    storage: Arc<dyn StorageTrait>,
    add_pipeline: AddPipeline,
    graph_db: Arc<MockGraphDB>,
    vector_db: Arc<MockVectorDB>,
    embedding_engine: Arc<MockEmbeddingEngine>,
    ontology: Arc<dyn OntologyResolver>,
    session_store: Arc<dyn SessionStore>,
    session_manager: Arc<SessionManager>,
    checkpoint_store: Arc<SeaOrmCheckpointStore>,
}

async fn make_harness() -> Harness {
    let temp = TempDir::new().unwrap();
    let sess_dir = TempDir::new().unwrap();
    let db_path = temp.path().join("cognee.db");
    std::fs::File::create(&db_path).unwrap();
    let url = format!("sqlite://{}", db_path.display());
    let db = connect(&url).await.unwrap();
    initialize(&db).await.unwrap();
    let db = Arc::new(db);
    let storage: Arc<dyn StorageTrait> = Arc::new(LocalStorage::new(temp.path().join("storage")));
    storage.initialize().await.unwrap();

    let ingest_db: Arc<dyn IngestDb> = db.clone();
    let graph_db = Arc::new(MockGraphDB::new());
    let vector_db = Arc::new(MockVectorDB::new());
    let add_pipeline = AddPipeline::new(Arc::clone(&storage), ingest_db)
        .with_thread_pool(Arc::new(
            cognee_lib::core::RayonThreadPool::with_default_threads().unwrap(),
        ))
        .with_graph_db(graph_db.clone() as Arc<dyn cognee_graph::GraphDBTrait>)
        .with_vector_db(vector_db.clone() as Arc<dyn cognee_vector::VectorDB>)
        .with_database(Arc::clone(&db));
    let embedding_engine = Arc::new(MockEmbeddingEngine::new(16));
    let ontology: Arc<dyn OntologyResolver> = Arc::new(NoOpOntologyResolver::new());

    let session_store: Arc<dyn SessionStore> = Arc::new(FsSessionStore::new(sess_dir.path()));
    let session_manager = Arc::new(SessionManager::new(Arc::clone(&session_store)));

    let checkpoint_store = Arc::new(SeaOrmCheckpointStore::new(Arc::clone(&db)));

    Harness {
        _temp: temp,
        _sess_dir: sess_dir,
        db,
        storage,
        add_pipeline,
        graph_db,
        vector_db,
        embedding_engine,
        ontology,
        session_store,
        session_manager,
        checkpoint_store,
    }
}

#[tokio::test]
async fn improve_without_sessions_runs_only_memify() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let llm: Arc<dyn cognee_llm::Llm> = Arc::new(MockLlm::empty());
    let config = CognifyConfig::default();

    let r = improve(ImproveParams {
        dataset_name: "ds_memify".to_string(),
        session_ids: None,
        node_name: None,
        owner_id: owner,
        tenant_id: None,
        feedback_alpha: 0.1,
        llm,
        storage: Arc::clone(&h.storage),
        graph_db: h.graph_db.clone() as Arc<_>,
        vector_db: h.vector_db.clone() as Arc<_>,
        embedding_engine: h.embedding_engine.clone() as Arc<_>,
        ontology_resolver: Arc::clone(&h.ontology),
        db: Some(Arc::clone(&h.db)),
        session_store: Some(Arc::clone(&h.session_store)),
        session_manager: Some(Arc::clone(&h.session_manager)),
        add_pipeline: Some(&h.add_pipeline),
        checkpoint_store: Some(h.checkpoint_store.clone() as Arc<_>),
        cognify_config: &config,
        extraction_tasks: None,
        enrichment_tasks: None,
        data: None,
        build_global_context_index: false,
        run_in_background: false,
    })
    .await
    .unwrap();

    assert_eq!(r.stages_run, vec!["memify".to_string()]);
    assert!(r.memify_result.is_some());
}

#[tokio::test]
async fn improve_skips_stage1_when_session_backends_missing() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let llm: Arc<dyn cognee_llm::Llm> = Arc::new(MockLlm::empty());
    let config = CognifyConfig::default();

    // Provide session_ids but no session_store/manager — stages 1, 2, 4
    // should all be skipped (with warnings), Stage 3 still runs.
    // Stage 2b (persist_trace_steps) is gated on `has_sessions` so its name IS
    // pushed to stages_run even when it skips/no-ops due to missing backends,
    // matching Python's convention of recording every attempted stage.
    let r = improve(ImproveParams {
        dataset_name: "ds_nosess".to_string(),
        session_ids: Some(vec!["s1".to_string()]),
        node_name: None,
        owner_id: owner,
        tenant_id: None,
        feedback_alpha: 0.1,
        llm,
        storage: Arc::clone(&h.storage),
        graph_db: h.graph_db.clone() as Arc<_>,
        vector_db: h.vector_db.clone() as Arc<_>,
        embedding_engine: h.embedding_engine.clone() as Arc<_>,
        ontology_resolver: Arc::clone(&h.ontology),
        db: Some(Arc::clone(&h.db)),
        session_store: None,
        session_manager: None,
        add_pipeline: None,
        checkpoint_store: None,
        cognify_config: &config,
        extraction_tasks: None,
        enrichment_tasks: None,
        data: None,
        build_global_context_index: false,
        run_in_background: false,
    })
    .await
    .unwrap();

    assert_eq!(
        r.stages_run,
        vec!["persist_trace_steps".to_string(), "memify".to_string()],
        "with sessions, persist_trace_steps is always recorded even when backends are missing"
    );
}
