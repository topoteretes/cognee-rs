//! Stage 2 integration tests — `persist_sessions_in_knowledge_graph`.
//!
//! The pipeline takes Q&A text from sessions, adds it to a dataset with the
//! `user_sessions_from_cache` node_set tag, and runs `cognify()` to extract
//! entities / relationships. The LLM is mocked so the test is deterministic.
#![cfg(feature = "testing")]

use std::sync::Arc;

use cognee_cognify::CognifyConfig;
use cognee_cognify::memify::persist_sessions::{
    USER_SESSIONS_NODE_SET, persist_sessions_in_knowledge_graph,
};
use cognee_database::ops::datasets as ds_ops;
use cognee_database::{DatabaseConnection, IngestDb, connect, initialize};
use cognee_embedding::MockEmbeddingEngine;
use cognee_graph::MockGraphDB;
use cognee_ingestion::AddPipeline;
use cognee_llm::Llm;
use cognee_ontology::{NoOpOntologyResolver, OntologyResolver};
use cognee_session::{FsSessionStore, SessionStore};
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
    }
}

#[tokio::test]
async fn persist_empty_sessions_returns_zero() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let llm: Arc<dyn Llm> = Arc::new(MockLlm::empty());
    let config = CognifyConfig::default();

    let res = persist_sessions_in_knowledge_graph(
        &["no_such_session".to_string()],
        "ds_empty",
        owner,
        None,
        Arc::clone(&h.session_store),
        &h.add_pipeline,
        llm,
        Arc::clone(&h.storage),
        h.graph_db.clone() as Arc<_>,
        h.vector_db.clone() as Arc<_>,
        h.embedding_engine.clone() as Arc<_>,
        Some(Arc::clone(&h.db)),
        Arc::clone(&h.ontology),
        &config,
    )
    .await
    .unwrap();

    assert_eq!(res.sessions_persisted, 0);
    assert_eq!(res.sessions_skipped, 1);
    assert_eq!(res.sessions_failed, 0);
}

#[tokio::test]
async fn persist_tags_nodes_with_user_sessions_node_set() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let user_id = owner.to_string();
    let session_id = "session_persist";

    // Seed 1 Q&A in the session.
    h.session_store
        .create_qa_entry(
            session_id,
            Some(&user_id),
            "Who is Alice?",
            "Alice is a software engineer.",
            None,
        )
        .await
        .unwrap();

    // Mock LLM returns an empty KG (sufficient to drive the pipeline).
    // The cognify pipeline then exercises chunking, fact extraction,
    // graph add, etc. Returning empty relationships keeps the test fast
    // and deterministic without needing a realistic extraction model.
    let llm: Arc<dyn Llm> = Arc::new(MockLlm::new(vec![
        r#"{"nodes":[],"relationships":[]}"#.to_string(),
    ]));
    let config = CognifyConfig::default();

    let _ = persist_sessions_in_knowledge_graph(
        &[session_id.to_string()],
        "ds_persist",
        owner,
        None,
        Arc::clone(&h.session_store),
        &h.add_pipeline,
        llm,
        Arc::clone(&h.storage),
        h.graph_db.clone() as Arc<_>,
        h.vector_db.clone() as Arc<_>,
        h.embedding_engine.clone() as Arc<_>,
        Some(Arc::clone(&h.db)),
        Arc::clone(&h.ontology),
        &config,
    )
    .await
    .unwrap();

    // The important side-effect: the add() phase tagged the Data row with
    // `user_sessions_from_cache`. Cognify may succeed or fail internally
    // depending on the mock backends, but the Stage-2 invariant is that
    // the session text was ingested with the correct `node_set`.
    let ds = ds_ops::get_dataset_by_name(h.db.as_ref(), "ds_persist", owner, None)
        .await
        .unwrap()
        .expect("dataset exists after persist_sessions");
    let data_items = ds_ops::get_dataset_data(h.db.as_ref(), ds.id)
        .await
        .unwrap();
    assert!(
        !data_items.is_empty(),
        "dataset should have at least one data row"
    );
    let has_tag = data_items.iter().any(|d| {
        d.node_set
            .as_deref()
            .map(|s| s.contains(USER_SESSIONS_NODE_SET))
            .unwrap_or(false)
    });
    assert!(
        has_tag,
        "expected at least one Data row tagged with {USER_SESSIONS_NODE_SET}"
    );
}
