#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Tests verifying that `improve()` runs synchronously to completion and
//! records every registry stage.
//!
//! (b) With sessions supplied, every stage is recorded in registry order.
//! (c) Without sessions, every stage is recorded as skipped.
//! (d) An improve of a session another run holds waits, then runs every stage.

use std::sync::Arc;

use cognee::api::ApiError;
use cognee::api::improve::{
    DEFAULT_STAGES, ImproveParams, ImproveStatus, REASON_ABORTED_BY_FATAL_STAGE,
    REASON_NO_SESSION_IDS, StageStatus, improve,
};
use cognee_cognify::CognifyConfig;
use cognee_database::{DatabaseConnection, IngestDb, SeaOrmCheckpointStore, connect, initialize};
use cognee_embedding::MockEmbeddingEngine;
use cognee_graph::MockGraphDB;
use cognee_ingestion::AddPipeline;
use cognee_ontology::{NoOpOntologyResolver, OntologyResolver};
use cognee_session::{
    FsSessionStore, SessionManager, SessionStore, acquire_improve_lock_many, improve_lock_keys,
};
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
            cognee::core::RayonThreadPool::with_default_threads().unwrap(),
        ))
        .with_graph_db(graph_db.clone() as Arc<dyn cognee_graph::GraphDBTrait>)
        .with_vector_db(vector_db.clone() as Arc<dyn cognee_vector::VectorDB>)
        .with_database(db.clone());
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

// ---------------------------------------------------------------------------
// (b) With sessions supplied, every registry stage is recorded, in order.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn improve_with_sessions_records_every_stage_in_order() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let llm: Arc<dyn cognee_llm::Llm> = Arc::new(MockLlm::empty());
    let config = CognifyConfig::default();

    let session_id = "sync-sess-improve";
    let user_id = owner.to_string();

    // Seed a QA entry so there is something for stage 1 to act on.
    h.session_store
        .create_qa_entry(session_id, Some(&user_id), "q", "a", None)
        .await
        .unwrap();

    let r = improve(ImproveParams {
        dataset_name: "ds_with_sessions".to_string(),
        dataset_id: None,
        session_ids: Some(vec![session_id.to_string()]),
        node_name: None,
        owner_id: owner,
        tenant_id: None,
        feedback_alpha: None,
        config: cognee::api::ImproveConfig::default(),
        personalization_enabled: false,
        llm,
        storage: Arc::clone(&h.storage),
        graph_db: h.graph_db.clone() as Arc<_>,
        vector_db: h.vector_db.clone() as Arc<_>,
        embedding_engine: h.embedding_engine.clone() as Arc<_>,
        ontology_resolver: Arc::clone(&h.ontology),
        db: Some(h.db.clone()),
        session_store: Some(Arc::clone(&h.session_store)),
        session_manager: Some(Arc::clone(&h.session_manager)),
        add_pipeline: Some(&h.add_pipeline),
        checkpoint_store: Some(h.checkpoint_store.clone() as Arc<_>),
        dataset_locks: None,
        cognify_config: &config,
        extraction_tasks: None,
        enrichment_tasks: None,
        data: None,
        build_global_context_index: false,
        build_truth_subspace: false,
        run_in_background: false,
    })
    .await;

    // The queue-less mock LLM cannot answer cognify's summary call, so the
    // fatal persist_session_qa stage errors and aborts the run; the partial
    // result still records every stage, in order.
    let Err(ApiError::ImproveAborted { result: r, .. }) = r else {
        panic!("expected the fatal stage to abort the run, got {r:?}");
    };

    let names: Vec<&str> = r.stages.iter().map(|s| s.stage.as_str()).collect();
    let expected: Vec<&str> = DEFAULT_STAGES.iter().map(|s| s.name()).collect();
    assert_eq!(names, expected);
    assert!(r.finished && !r.background);
    assert_eq!(r.session_ids, [session_id]);
    // Stage 1 ran over the seeded Q&A entry; stage 2 failed and stopped it.
    assert_eq!(r.stages[0].status, StageStatus::Completed);
    assert_eq!(r.stages[1].status, StageStatus::Errored);
    assert!(
        r.stages[2..]
            .iter()
            .all(|s| s.reason.as_deref() == Some(REASON_ABORTED_BY_FATAL_STAGE))
    );
}

// ---------------------------------------------------------------------------
// (c) Without sessions, every stage is skipped
// ---------------------------------------------------------------------------

#[tokio::test]
async fn improve_without_sessions_skips_every_stage() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let llm: Arc<dyn cognee_llm::Llm> = Arc::new(MockLlm::empty());
    let config = CognifyConfig::default();

    let r = improve(ImproveParams {
        dataset_name: "ds_no_sess".to_string(),
        dataset_id: None,
        session_ids: None,
        node_name: None,
        owner_id: owner,
        tenant_id: None,
        feedback_alpha: None,
        config: cognee::api::ImproveConfig::default(),
        personalization_enabled: false,
        llm,
        storage: Arc::clone(&h.storage),
        graph_db: h.graph_db.clone() as Arc<_>,
        vector_db: h.vector_db.clone() as Arc<_>,
        embedding_engine: h.embedding_engine.clone() as Arc<_>,
        ontology_resolver: Arc::clone(&h.ontology),
        db: Some(h.db.clone()),
        session_store: Some(Arc::clone(&h.session_store)),
        session_manager: Some(Arc::clone(&h.session_manager)),
        add_pipeline: Some(&h.add_pipeline),
        checkpoint_store: Some(h.checkpoint_store.clone() as Arc<_>),
        dataset_locks: None,
        cognify_config: &config,
        extraction_tasks: None,
        enrichment_tasks: None,
        data: None,
        build_global_context_index: false,
        build_truth_subspace: false,
        run_in_background: false,
    })
    .await
    .unwrap();

    assert_eq!(r.stages.len(), DEFAULT_STAGES.len());
    for (stage, kind) in r.stages.iter().zip(DEFAULT_STAGES) {
        assert_eq!(stage.status, StageStatus::Skipped, "{}", stage.stage);
        if kind.needs_sessions() {
            assert_eq!(stage.reason.as_deref(), Some(REASON_NO_SESSION_IDS));
        }
    }
    assert_eq!(r.status(), ImproveStatus::Skipped);
}

// ---------------------------------------------------------------------------
// (d) improve() waits while another run holds its session, then runs every
//     stage (Python `test_improve_queue.py`).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn improve_waits_for_a_held_session_then_runs_every_stage() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let llm: Arc<dyn cognee_llm::Llm> = Arc::new(MockLlm::empty());
    let config = CognifyConfig::default();

    let session_id = "locked-session";

    // Another run holds this session's claim.
    let held = acquire_improve_lock_many(improve_lock_keys([session_id], &owner.to_string())).await;

    let run = improve(ImproveParams {
        dataset_name: "ds_locked".to_string(),
        dataset_id: None,
        session_ids: Some(vec![session_id.to_string()]),
        node_name: None,
        owner_id: owner,
        tenant_id: None,
        feedback_alpha: None,
        config: cognee::api::ImproveConfig::default(),
        personalization_enabled: false,
        llm,
        storage: Arc::clone(&h.storage),
        graph_db: h.graph_db.clone() as Arc<_>,
        vector_db: h.vector_db.clone() as Arc<_>,
        embedding_engine: h.embedding_engine.clone() as Arc<_>,
        ontology_resolver: Arc::clone(&h.ontology),
        db: Some(h.db.clone()),
        session_store: Some(Arc::clone(&h.session_store)),
        session_manager: Some(Arc::clone(&h.session_manager)),
        add_pipeline: Some(&h.add_pipeline),
        checkpoint_store: Some(h.checkpoint_store.clone() as Arc<_>),
        dataset_locks: None,
        cognify_config: &config,
        extraction_tasks: None,
        enrichment_tasks: None,
        data: None,
        build_global_context_index: false,
        build_truth_subspace: false,
        run_in_background: false,
    });
    tokio::pin!(run);

    // It waits instead of skipping (Python's #5566 queue).
    let waited = tokio::time::timeout(std::time::Duration::from_millis(200), &mut run).await;
    assert!(waited.is_err(), "the run must wait for the held session");

    // Released: it runs every stage.
    drop(held);
    let r = tokio::time::timeout(std::time::Duration::from_secs(30), run)
        .await
        .expect("the run proceeds once the session is released")
        .unwrap();
    assert_eq!(r.stages.len(), DEFAULT_STAGES.len(), "{:?}", r.stages);
    assert!(r.finished);
}
