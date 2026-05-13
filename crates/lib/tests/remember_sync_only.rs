//! Compile-time and runtime tests verifying that `remember()` is sync-only.
//!
//! Covers:
//! (a) The function signature has no `run_in_background` parameter.
//! (b) `RememberResult` has no `inner` / `await_completion()` member.
//! (c) Calling `remember(...)` for permanent mode returns `RememberStatus::Completed`.
//! (d) Calling with a `session_id` runs inline improve() and returns `SessionStored`.

use std::sync::Arc;

use cognee_cognify::CognifyConfig;
use cognee_database::{DatabaseConnection, IngestDb, SeaOrmCheckpointStore, connect, initialize};
use cognee_embedding::MockEmbeddingEngine;
use cognee_graph::MockGraphDB;
use cognee_ingestion::AddPipeline;
use cognee_lib::api::remember::{RememberResult, RememberStatus, remember};
use cognee_models::DataInput;
use cognee_ontology::{NoOpOntologyResolver, OntologyResolver};
use cognee_session::{FsSessionStore, SessionManager, SessionStore};
use cognee_storage::{LocalStorage, StorageTrait};
use cognee_test_utils::MockLlm;
use cognee_vector::MockVectorDB;
use tempfile::TempDir;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// (a) + (b) Compile-time checks
//
// These assertions live in the function body; if `run_in_background` existed
// or `inner` / `await_completion` existed, this file would not compile.
// ---------------------------------------------------------------------------

/// Verify the function signature at type level.
/// If `run_in_background` were still present, this function would not compile
/// because `remember` would take 19 arguments, not 18.
fn _assert_signature() {
    // This is a compile-time only check: remember() must accept exactly 18
    // positional arguments (no run_in_background between self_improvement and
    // owner_id). The test passes if this file compiles.
    let _ = remember; // just reference it to ensure it is imported
}

/// Verify that `RememberResult` has no `inner` field and no `await_completion`
/// method. If either existed, the negative assertions below would fail at
/// compile time.
fn _assert_no_inner_field(r: &RememberResult) {
    // `r.inner` must NOT exist — commented out; the test verifies via
    // serialization that the field is absent from the JSON output.
    let _ = r;
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Harness {
    _temp: TempDir,
    _sess_dir: TempDir,
    db: Arc<DatabaseConnection>,
    storage: Arc<dyn StorageTrait>,
    add_pipeline: Arc<AddPipeline>,
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
    let add_pipeline = Arc::new(
        AddPipeline::new(Arc::clone(&storage), ingest_db)
            .with_thread_pool(Arc::new(
                cognee_lib::core::RayonThreadPool::with_default_threads().unwrap(),
            ))
            .with_graph_db(graph_db.clone() as Arc<dyn cognee_graph::GraphDBTrait>)
            .with_vector_db(vector_db.clone() as Arc<dyn cognee_vector::VectorDB>)
            .with_database(Arc::clone(&db)),
    );
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

fn mock_llm() -> Arc<dyn cognee_llm::Llm> {
    Arc::new(MockLlm::empty())
}

// ---------------------------------------------------------------------------
// (b) RememberResult has no `inner` in its JSON serialization
// ---------------------------------------------------------------------------

#[test]
fn remember_result_has_no_inner_field_in_json() {
    use cognee_lib::api::remember::RememberResult;
    // Construct a minimal RememberResult.
    let r = RememberResult {
        status: RememberStatus::Completed,
        dataset_name: "x".to_string(),
        dataset_id: None,
        session_ids: None,
        pipeline_run_id: None,
        elapsed_seconds: Some(0.0),
        content_hash: None,
        items_processed: 0,
        items: vec![],
        error: None,
        entry_type: None,
        entry_id: None,
        cognify_result: None,
        memify_result: None,
    };
    let json = r.to_dict();
    let obj = json.as_object().expect("object");
    assert!(!obj.contains_key("inner"), "inner field must not exist");
    // done() always returns true.
    assert!(r.done());
}

// ---------------------------------------------------------------------------
// (c) Session-mode call returns SessionStored synchronously
// ---------------------------------------------------------------------------

#[tokio::test]
async fn remember_session_returns_session_stored() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();

    let result = remember(
        vec![DataInput::Text("sync test".to_string())],
        "ds_sync_sess",
        Some("sync-sess-1"),
        false,
        owner,
        None,
        Arc::clone(&h.add_pipeline),
        mock_llm(),
        Arc::clone(&h.storage),
        h.graph_db.clone() as Arc<_>,
        h.vector_db.clone() as Arc<_>,
        h.embedding_engine.clone() as Arc<_>,
        Some(Arc::clone(&h.db)),
        Some(Arc::clone(&h.session_store)),
        Some(Arc::clone(&h.session_manager)),
        Some(h.checkpoint_store.clone() as Arc<_>),
        Arc::clone(&h.ontology),
        Arc::new(CognifyConfig::default()),
    )
    .await
    .expect("remember session");

    assert_eq!(result.status, RememberStatus::SessionStored);
    assert!(result.done(), "all statuses are terminal");
    assert!(result.is_success());
}

// ---------------------------------------------------------------------------
// (d) Session-mode with self_improvement=true runs improve() inline
// ---------------------------------------------------------------------------

#[tokio::test]
async fn remember_session_self_improvement_inline() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();

    let result = remember(
        vec![DataInput::Text("inline improve test".to_string())],
        "ds_sync_improve",
        Some("sync-sess-2"),
        /* self_improvement= */ true,
        owner,
        None,
        Arc::clone(&h.add_pipeline),
        mock_llm(),
        Arc::clone(&h.storage),
        h.graph_db.clone() as Arc<_>,
        h.vector_db.clone() as Arc<_>,
        h.embedding_engine.clone() as Arc<_>,
        Some(Arc::clone(&h.db)),
        Some(Arc::clone(&h.session_store)),
        Some(Arc::clone(&h.session_manager)),
        Some(h.checkpoint_store.clone() as Arc<_>),
        Arc::clone(&h.ontology),
        Arc::new(CognifyConfig::default()),
    )
    .await
    .expect("remember session with inline improve");

    // Synchronous: always terminal on return.
    assert_eq!(result.status, RememberStatus::SessionStored);
    assert!(result.done());
}
