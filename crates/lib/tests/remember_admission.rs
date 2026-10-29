#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Host admission for automatic improves (Python
//! `register_auto_improve_admission`): a declined improve is reported as
//! `improve_skipped`, the data is stored either way, and an allowed or
//! absent check changes nothing. The check is process-wide, so every case
//! lives in one test.

use std::sync::Arc;

use cognee::api::remember::{
    AutoImproveContext, clear_auto_improve_admission, register_auto_improve_admission, remember,
};
use cognee_cognify::CognifyConfig;
use cognee_database::{DatabaseConnection, IngestDb, connect, initialize};
use cognee_embedding::MockEmbeddingEngine;
use cognee_graph::MockGraphDB;
use cognee_ingestion::AddPipeline;
use cognee_models::DataInput;
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
    add_pipeline: Arc<AddPipeline>,
    graph_db: Arc<MockGraphDB>,
    vector_db: Arc<MockVectorDB>,
    embedding_engine: Arc<MockEmbeddingEngine>,
    ontology: Arc<dyn OntologyResolver>,
    session_store: Arc<dyn SessionStore>,
    session_manager: Arc<SessionManager>,
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
                cognee::core::RayonThreadPool::with_default_threads().unwrap(),
            ))
            .with_graph_db(graph_db.clone() as Arc<dyn cognee_graph::GraphDBTrait>)
            .with_vector_db(vector_db.clone() as Arc<dyn cognee_vector::VectorDB>)
            .with_database(db.clone()),
    );
    let embedding_engine = Arc::new(MockEmbeddingEngine::new(16));
    let ontology: Arc<dyn OntologyResolver> = Arc::new(NoOpOntologyResolver::new());
    let session_store: Arc<dyn SessionStore> = Arc::new(FsSessionStore::new(sess_dir.path()));
    let session_manager = Arc::new(SessionManager::new(Arc::clone(&session_store)));
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
    }
}

async fn run(
    h: &Harness,
    session_id: Option<&str>,
    self_improvement: bool,
) -> cognee::api::RememberResult {
    remember(
        vec![DataInput::Text("Alice works at Acme.".to_string())],
        "ds_admission",
        session_id,
        self_improvement,
        Uuid::new_v4(),
        None,
        Arc::clone(&h.add_pipeline),
        Arc::new(
            MockLlm::new(vec![r#"{"nodes":[],"edges":[]}"#.to_string(); 8])
                .with_summary_response(r#"{"summary":"s","description":"d"}"#.to_string()),
        ),
        Arc::clone(&h.storage),
        h.graph_db.clone() as Arc<_>,
        h.vector_db.clone() as Arc<_>,
        h.embedding_engine.clone() as Arc<_>,
        Some(h.db.clone()),
        Some(Arc::clone(&h.session_store)),
        Some(Arc::clone(&h.session_manager)),
        Arc::clone(&h.ontology),
        Arc::new(CognifyConfig::default()),
    )
    .await
    .expect("remember")
}

#[tokio::test]
async fn admission_check_skips_the_automatic_improve() {
    let h = make_harness().await;

    // The check sees the session the improve would bridge, and declines it.
    let seen = Arc::new(std::sync::Mutex::new(Vec::<AutoImproveContext>::new()));
    let seen_in_check = Arc::clone(&seen);
    register_auto_improve_admission(Arc::new(move |ctx: AutoImproveContext| {
        seen_in_check.lock().unwrap().push(ctx);
        Box::pin(async { Some("insufficient_credits".to_string()) })
    }));

    let session = run(&h, Some("sess-admission"), true).await;
    assert_eq!(
        session.improve_skipped.as_deref(),
        Some("insufficient_credits")
    );
    assert!(session.improve.is_none() && session.improve_error.is_none());
    assert_eq!(session.to_dict()["improve_skipped"], "insufficient_credits");

    let permanent = run(&h, None, true).await;
    // The data was added and cognified all the same.
    assert_eq!(permanent.items_processed, 1);
    assert_eq!(
        permanent.improve_skipped.as_deref(),
        Some("insufficient_credits")
    );
    assert!(permanent.improve.is_none());
    assert!(permanent.error.is_none());

    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].session_id.as_deref(), Some("sess-admission"));
        assert_eq!(seen[0].session_ids, vec!["sess-admission".to_string()]);
        assert!(seen[0].dataset_id.is_some());
        assert_eq!(seen[1].session_id, None);
        assert!(seen[1].session_ids.is_empty());
        assert_eq!(seen[1].dataset_name, "ds_admission");
        assert!(seen[1].dataset_id.is_some());
    }

    // With self_improvement off the check is never asked.
    let before = seen.lock().unwrap().len();
    let off = run(&h, None, false).await;
    assert!(off.improve_skipped.is_none() && off.improve.is_none());
    assert_eq!(seen.lock().unwrap().len(), before);

    // An empty reason is no usable answer: the improve runs.
    register_auto_improve_admission(Arc::new(|_| Box::pin(async { Some("  ".to_string()) })));
    let blank = run(&h, None, true).await;
    assert!(blank.improve_skipped.is_none());
    assert!(blank.improve.is_some());

    // A check that allows it changes nothing, and so does clearing it.
    register_auto_improve_admission(Arc::new(|_| Box::pin(async { None })));
    assert!(run(&h, None, true).await.improve.is_some());
    clear_auto_improve_admission();
    let cleared = run(&h, None, true).await;
    assert!(cleared.improve.is_some() && cleared.improve_skipped.is_none());
}
