//! Integration tests for the `remember()` API.
//!
//! Covers:
//! * Session-mode storage (status=SessionStored, session_ids populated).
//! * Session-mode `self_improvement=true` → awaitable background bridge.
//! * `RememberResult` serde / display / `is_success` / `done` truth table.
//!
//! Permanent-mode end-to-end tests (which require a full cognify pipeline)
//! are covered by the per-stage integration tests in `cognee-cognify`
//! (`memify_persist_sessions`, `e2e_full_pipeline_memify`, etc.). Here we
//! exercise the orchestration surface using mock backends.

use std::sync::Arc;

use cognee_cognify::CognifyConfig;
use cognee_database::{DatabaseConnection, IngestDb, SeaOrmCheckpointStore, connect, initialize};
use cognee_embedding::MockEmbeddingEngine;
use cognee_graph::MockGraphDB;
use cognee_ingestion::AddPipeline;
use cognee_lib::api::remember::{RememberItemInfo, RememberStatus, remember};
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
    checkpoint_store: Arc<SeaOrmCheckpointStore>,
}

async fn make_harness() -> Harness {
    let temp = TempDir::new().expect("tempdir");
    let sess_dir = TempDir::new().expect("tempdir");
    let db_path = temp.path().join("cognee.db");
    std::fs::File::create(&db_path).expect("create db file");
    let url = format!("sqlite://{}", db_path.display());
    let db = connect(&url).await.expect("connect");
    initialize(&db).await.expect("init");
    let db = Arc::new(db);
    let storage: Arc<dyn StorageTrait> = Arc::new(LocalStorage::new(temp.path().join("storage")));
    storage.initialize().await.expect("storage init");

    let ingest_db: Arc<dyn IngestDb> = db.clone();
    let add_pipeline = Arc::new(AddPipeline::new(Arc::clone(&storage), ingest_db));

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

fn mock_llm() -> Arc<dyn cognee_llm::Llm> {
    Arc::new(MockLlm::empty())
}

// ---------------------------------------------------------------------------
// Session-mode tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn remember_session_stores_qa_entry() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let user_id = owner.to_string();
    let session_id = "sess-store-only";

    let result = remember(
        vec![DataInput::Text("alpha beta gamma".to_string())],
        "ds_store_only",
        Some(session_id),
        /* self_improvement= */ false,
        /* run_in_background= */ false,
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
    assert_eq!(
        result.session_ids.as_deref(),
        Some([session_id.to_string()].as_slice())
    );
    assert!(result.is_success());
    assert!(result.done());

    // Q&A entry should exist in the session store.
    let entries = h
        .session_store
        .get_all_qa_entries(session_id, Some(&user_id))
        .await
        .expect("get qa entries");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].answer, "alpha beta gamma");
    assert_eq!(entries[0].question, "");
}

#[tokio::test]
async fn remember_session_self_improvement_returns_awaitable() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let session_id = "sess-bridge";

    let result = remember(
        vec![DataInput::Text("bridge text".to_string())],
        "ds_bridge",
        Some(session_id),
        /* self_improvement= */ true,
        /* run_in_background= */ false,
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
    .expect("remember session with bridge");

    // Initial return carries SessionStored — the improve task is running in background.
    assert_eq!(result.status, RememberStatus::SessionStored);
    assert_eq!(result.items_processed, 1);

    // Awaiting completion drains the background improve task — should not propagate errors.
    let awaited = result
        .await_completion()
        .await
        .expect("await_completion succeeds");
    // Session-mode always ends as SessionStored regardless of improve inner result.
    assert_eq!(awaited.status, RememberStatus::SessionStored);
}

#[tokio::test]
async fn remember_session_requires_session_store() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();

    // Omit session_store — session mode should fail loud.
    let result = remember(
        vec![DataInput::Text("x".to_string())],
        "ds_nostore",
        Some("sess-nostore"),
        false,
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
        /* session_store */ None,
        /* session_manager */ None,
        None,
        Arc::clone(&h.ontology),
        Arc::new(CognifyConfig::default()),
    )
    .await;

    assert!(result.is_err(), "expected invalid-argument error");
}

// ---------------------------------------------------------------------------
// RememberResult introspection tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn display_and_to_dict_on_session_result() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let result = remember(
        vec![DataInput::Text("display test".to_string())],
        "ds_display",
        Some("sess-display"),
        false,
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
    .expect("remember");

    // Display — human-readable summary.
    let text = format!("{result}");
    assert!(text.contains("status=SessionStored"));
    assert!(text.contains("dataset=\"ds_display\""));
    assert!(text.contains("session_id=\"sess-display\""));

    // to_dict — JSON-serializable.
    let dict = result.to_dict();
    let obj = dict.as_object().expect("to_dict returns an object");
    assert_eq!(
        obj.get("status").and_then(|v| v.as_str()),
        Some("session_stored")
    );
    assert_eq!(
        obj.get("dataset_name").and_then(|v| v.as_str()),
        Some("ds_display")
    );
    // cognify_result, memify_result, inner are #[serde(skip)] — absent.
    assert!(!obj.contains_key("cognify_result"));
    assert!(!obj.contains_key("memify_result"));
    assert!(!obj.contains_key("inner"));
}

#[test]
fn remember_item_info_serde_roundtrip() {
    let info = RememberItemInfo {
        id: Some(Uuid::nil()),
        name: Some("foo.txt".to_string()),
        content_hash: Some("abcdef".to_string()),
        token_count: Some(42),
        data_size: Some(1024),
        mime_type: Some("text/plain".to_string()),
    };
    let j = serde_json::to_string(&info).expect("ser");
    let back: RememberItemInfo = serde_json::from_str(&j).expect("de");
    assert_eq!(back.token_count, Some(42));
    assert_eq!(back.data_size, Some(1024));
    assert_eq!(back.content_hash.as_deref(), Some("abcdef"));
}

// ---------------------------------------------------------------------------
// Permanent-mode background test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn remember_permanent_background_returns_running_then_completes() {
    // Exercise the permanent-mode background path: the initial result should
    // carry `status = Running` with no populated fields, and awaiting
    // completion should drive it to a terminal state (Completed or Errored).
    let h = make_harness().await;
    let owner = Uuid::new_v4();

    let result = remember(
        vec![DataInput::Text("background permanent text".to_string())],
        "ds_bg_perm",
        /* session_id= */ None,
        /* self_improvement= */ false,
        /* run_in_background= */ true,
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
    .expect("remember background");

    // Initial state: Running, not done, not success.
    assert_eq!(result.status, RememberStatus::Running);
    assert!(!result.done());
    assert!(!result.is_success());
    assert_eq!(result.dataset_name, "ds_bg_perm");
    assert!(result.dataset_id.is_none());
    assert!(result.items.is_empty());

    // Drive to completion.
    let awaited = result
        .await_completion()
        .await
        .expect("await_completion succeeds");
    assert!(awaited.done(), "status after await = {:?}", awaited.status);
    // Completed on success, Errored on pipeline failure — either is a valid
    // terminal state for this smoke test; both imply the background task
    // was drained and the shared inner state was refreshed.
    assert!(matches!(
        awaited.status,
        RememberStatus::Completed | RememberStatus::Errored
    ));
}
