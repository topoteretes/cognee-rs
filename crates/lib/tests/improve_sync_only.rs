//! Tests verifying that `improve()` is sync-only (no `run_in_background`).
//!
//! (a) Signature has no `run_in_background`.
//! (b) When sessions are supplied, all four stages run (or gracefully skip
//!     when backends are missing).
//! (c) When sessions are not supplied, only Stage 3 (memify) runs.

use std::sync::Arc;

use cognee_cognify::CognifyConfig;
use cognee_database::{DatabaseConnection, IngestDb, SeaOrmCheckpointStore, connect, initialize};
use cognee_embedding::MockEmbeddingEngine;
use cognee_graph::MockGraphDB;
use cognee_ingestion::AddPipeline;
use cognee_lib::api::improve::{ImproveParams, improve};
use cognee_ontology::{NoOpOntologyResolver, OntologyResolver};
use cognee_session::{FsSessionStore, ImproveLockGuard, SessionManager, SessionStore};
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

// ---------------------------------------------------------------------------
// (a) Compile-time check: ImproveParams exhaustive construction
//
// `ImproveParams` now has 23 fields (18 from LIB-04, three v2 fields from E-05:
// extraction_tasks, enrichment_tasks, data; and two new parity fields:
// build_global_context_index, run_in_background). The struct literal below must
// list every field so the compiler catches future additions.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// (b) With sessions supplied, stage 3 (memify) always runs; stage 1/2/4 may
//     skip gracefully when the dataset does not yet exist in the graph.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn improve_with_sessions_runs_at_least_memify() {
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
        session_ids: Some(vec![session_id.to_string()]),
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

    // Stage 3 (memify) always runs.
    assert!(
        r.stages_run.contains(&"memify".to_string()),
        "expected memify stage; got {:?}",
        r.stages_run
    );
}

// ---------------------------------------------------------------------------
// (c) Without sessions, only Stage 3 runs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn improve_without_sessions_runs_only_memify() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let llm: Arc<dyn cognee_llm::Llm> = Arc::new(MockLlm::empty());
    let config = CognifyConfig::default();

    let r = improve(ImproveParams {
        dataset_name: "ds_no_sess".to_string(),
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

    assert_eq!(
        r.stages_run,
        vec!["memify".to_string()],
        "without sessions only stage 3 should run"
    );
    assert!(r.memify_result.is_some());
}

// ---------------------------------------------------------------------------
// (d) improve() returns an empty/default result when the single target
//     session is already being improved (lock held by another caller).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn improve_skips_when_locked() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let llm: Arc<dyn cognee_llm::Llm> = Arc::new(MockLlm::empty());
    let config = CognifyConfig::default();

    let session_id = "locked-session";

    // Simulate another caller already holding the improve lock for this session.
    let _held_guard = ImproveLockGuard::acquire(session_id)
        .expect("should be able to acquire lock when no other holder exists");

    // Now try to improve the same session — it should no-op.
    let r = improve(ImproveParams {
        dataset_name: "ds_locked".to_string(),
        session_ids: Some(vec![session_id.to_string()]),
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

    // When skipped due to lock, improve() returns the default result with no
    // stages run.
    assert!(
        r.stages_run.is_empty(),
        "improve should run no stages when locked; got {:?}",
        r.stages_run
    );

    // After the held guard drops, the same session can be improved again.
    drop(_held_guard);
    let guard2 = ImproveLockGuard::acquire(session_id);
    assert!(
        guard2.is_some(),
        "lock should be releaseable and re-acquirable after the held guard drops"
    );
}
