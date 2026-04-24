//! End-to-end tests for `improve()` orchestrator.
//!
//! These tests exercise the stage-gate logic (which stages run based on the
//! presence of `session_ids`, `run_in_background`, and available backends)
//! using mock storage/graph/vector/embedding/LLM backends. They do NOT
//! exercise the full LLM-driven cognify pipeline — that is covered by the
//! per-stage integration tests in `cognee-cognify`.

use std::sync::Arc;

use cognee_cognify::CognifyConfig;
use cognee_database::{DatabaseConnection, IngestDb, SeaOrmCheckpointStore, connect, initialize};
use cognee_embedding::MockEmbeddingEngine;
use cognee_graph::MockGraphDB;
use cognee_ingestion::AddPipeline;
use cognee_lib::api::improve::improve;
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
    let add_pipeline = AddPipeline::new(Arc::clone(&storage), ingest_db);

    let graph_db = Arc::new(MockGraphDB::new());
    let vector_db = Arc::new(MockVectorDB::new());
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

    let r = improve(
        "ds_memify",
        None,
        None,
        owner,
        None,
        0.1,
        false,
        llm,
        Arc::clone(&h.storage),
        h.graph_db.clone() as Arc<_>,
        h.vector_db.clone() as Arc<_>,
        h.embedding_engine.clone() as Arc<_>,
        Arc::clone(&h.ontology),
        Some(Arc::clone(&h.db)),
        Some(Arc::clone(&h.session_store)),
        Some(Arc::clone(&h.session_manager)),
        Some(&h.add_pipeline),
        Some(h.checkpoint_store.clone() as Arc<_>),
        &config,
    )
    .await
    .unwrap();

    assert_eq!(r.stages_run, vec!["memify".to_string()]);
    assert!(r.memify_result.is_some());
}

#[tokio::test]
async fn improve_with_run_in_background_skips_stage4() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let user_id = owner.to_string();
    let session_id = "sess-bg";

    // Seed a QA entry with feedback so stage 1 does something.
    let qa_id = h
        .session_store
        .create_qa_entry(session_id, Some(&user_id), "q", "a", None)
        .await
        .unwrap();
    h.session_manager
        .update_qa(
            Some(session_id),
            Some(&user_id),
            &qa_id,
            cognee_session::SessionQAUpdate {
                feedback_score: Some(Some(5)),
                used_graph_element_ids: Some(Some(cognee_session::UsedGraphElementIds {
                    node_ids: vec!["n1".to_string()],
                    edge_ids: vec![],
                })),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let llm: Arc<dyn cognee_llm::Llm> = Arc::new(MockLlm::empty());
    let config = CognifyConfig::default();

    let r = improve(
        "ds_bg",
        Some(vec![session_id.to_string()]),
        None,
        owner,
        None,
        0.1,
        true, // run_in_background
        llm,
        Arc::clone(&h.storage),
        h.graph_db.clone() as Arc<_>,
        h.vector_db.clone() as Arc<_>,
        h.embedding_engine.clone() as Arc<_>,
        Arc::clone(&h.ontology),
        Some(Arc::clone(&h.db)),
        Some(Arc::clone(&h.session_store)),
        Some(Arc::clone(&h.session_manager)),
        Some(&h.add_pipeline),
        Some(h.checkpoint_store.clone() as Arc<_>),
        &config,
    )
    .await
    .unwrap();

    assert!(!r.stages_run.contains(&"sync_graph_to_session".to_string()));
    assert!(r.stages_run.contains(&"apply_feedback_weights".to_string()));
    assert!(r.stages_run.contains(&"memify".to_string()));
}

#[tokio::test]
async fn improve_skips_stage1_when_session_backends_missing() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let llm: Arc<dyn cognee_llm::Llm> = Arc::new(MockLlm::empty());
    let config = CognifyConfig::default();

    // Provide session_ids but no session_store/manager — stages 1, 2, 4
    // should all be skipped (with warnings), Stage 3 still runs.
    let r = improve(
        "ds_nosess",
        Some(vec!["s1".to_string()]),
        None,
        owner,
        None,
        0.1,
        false,
        llm,
        Arc::clone(&h.storage),
        h.graph_db.clone() as Arc<_>,
        h.vector_db.clone() as Arc<_>,
        h.embedding_engine.clone() as Arc<_>,
        Arc::clone(&h.ontology),
        Some(Arc::clone(&h.db)),
        None,
        None,
        None,
        None,
        &config,
    )
    .await
    .unwrap();

    assert_eq!(r.stages_run, vec!["memify".to_string()]);
}
