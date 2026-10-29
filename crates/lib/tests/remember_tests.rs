#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for the `remember()` API.
//!
//! Covers:
//! * Session-mode storage (status=SessionStored, session_ids populated).
//! * Session-mode `self_improvement=true` → detached background bridge.
//! * `RememberResult` serde / display / `is_success` / `done` truth table.
//!
//! Permanent-mode end-to-end tests (which require a full cognify pipeline)
//! are covered by the per-stage integration tests in `cognee-cognify`
//! (`memify_persist_sessions`, `e2e_full_pipeline_memify`, etc.). Here we
//! exercise the orchestration surface using mock backends.

use std::sync::Arc;

use cognee::api::ImproveConfig;
use cognee::api::remember::{RememberItemInfo, RememberStatus, remember};
use cognee_cognify::CognifyConfig;
use cognee_database::{DatabaseConnection, IngestDb, SeaOrmCheckpointStore, connect, initialize};
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
        owner,
        None,
        Arc::clone(&h.add_pipeline),
        mock_llm(),
        Arc::clone(&h.storage),
        h.graph_db.clone() as Arc<_>,
        h.vector_db.clone() as Arc<_>,
        h.embedding_engine.clone() as Arc<_>,
        Some(h.db.clone()),
        Some(Arc::clone(&h.session_store)),
        Some(Arc::clone(&h.session_manager)),
        Some(h.checkpoint_store.clone() as Arc<_>),
        Arc::clone(&h.ontology),
        Arc::new(CognifyConfig::default()),
        Some(ImproveConfig::default()),
        cognee::search::PreferenceConfig::default(),
        false,
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

/// With session memory switched off (`session.caching`, Python's
/// `CACHING=false`), `remember(data, session_id)` still succeeds: Python's
/// `_add_to_session` only logs "session cache not available" and returns.
#[tokio::test]
async fn remember_session_with_caching_off_succeeds_without_storing() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let session_id = "sess-caching-off";
    let store: Arc<dyn SessionStore> = Arc::new(cognee_session::DisabledSessionStore);
    let manager = Arc::new(SessionManager::new(Arc::clone(&store)));

    let result = remember(
        vec![DataInput::Text("alpha beta gamma".to_string())],
        "ds_caching_off",
        Some(session_id),
        /* self_improvement= */ false,
        owner,
        None,
        Arc::clone(&h.add_pipeline),
        mock_llm(),
        Arc::clone(&h.storage),
        h.graph_db.clone() as Arc<_>,
        h.vector_db.clone() as Arc<_>,
        h.embedding_engine.clone() as Arc<_>,
        Some(h.db.clone()),
        Some(Arc::clone(&store)),
        Some(manager),
        Some(h.checkpoint_store.clone() as Arc<_>),
        Arc::clone(&h.ontology),
        Arc::new(CognifyConfig::default()),
        Some(ImproveConfig::default()),
        cognee::search::PreferenceConfig::default(),
        false,
    )
    .await
    .expect("remember with caching off must not fail");

    assert_eq!(result.status, RememberStatus::SessionStored);
    assert_eq!(
        result.session_ids.as_deref(),
        Some([session_id.to_string()].as_slice())
    );
    assert!(
        store
            .get_all_qa_entries(session_id, Some(&owner.to_string()))
            .await
            .expect("read")
            .is_empty()
    );
}

#[tokio::test]
async fn remember_session_self_improvement_runs_in_background() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let session_id = "sess-bridge";

    let result = remember(
        vec![DataInput::Text("bridge text".to_string())],
        "ds_bridge",
        Some(session_id),
        /* self_improvement= */ true,
        owner,
        None,
        Arc::clone(&h.add_pipeline),
        mock_llm(),
        Arc::clone(&h.storage),
        h.graph_db.clone() as Arc<_>,
        h.vector_db.clone() as Arc<_>,
        h.embedding_engine.clone() as Arc<_>,
        Some(h.db.clone()),
        Some(Arc::clone(&h.session_store)),
        Some(Arc::clone(&h.session_manager)),
        Some(h.checkpoint_store.clone() as Arc<_>),
        Arc::clone(&h.ontology),
        Arc::new(CognifyConfig::default()),
        Some(ImproveConfig::default()),
        cognee::search::PreferenceConfig::default(),
        false,
    )
    .await
    .expect("remember session with bridge");

    // SessionStored at once; the bridge runs as a detached task.
    assert_eq!(result.status, RememberStatus::SessionStored);
    assert_eq!(result.items_processed, 1);
    // done() is always true for any status.
    assert!(result.done());
    assert!(result.improve_launched);
    assert!(cognee::wait_for_background_tasks(None).await);
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
        owner,
        None,
        Arc::clone(&h.add_pipeline),
        mock_llm(),
        Arc::clone(&h.storage),
        h.graph_db.clone() as Arc<_>,
        h.vector_db.clone() as Arc<_>,
        h.embedding_engine.clone() as Arc<_>,
        Some(h.db.clone()),
        /* session_store */ None,
        /* session_manager */ None,
        None,
        Arc::clone(&h.ontology),
        Arc::new(CognifyConfig::default()),
        Some(ImproveConfig::default()),
        cognee::search::PreferenceConfig::default(),
        false,
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
        owner,
        None,
        Arc::clone(&h.add_pipeline),
        mock_llm(),
        Arc::clone(&h.storage),
        h.graph_db.clone() as Arc<_>,
        h.vector_db.clone() as Arc<_>,
        h.embedding_engine.clone() as Arc<_>,
        Some(h.db.clone()),
        Some(Arc::clone(&h.session_store)),
        Some(Arc::clone(&h.session_manager)),
        Some(h.checkpoint_store.clone() as Arc<_>),
        Arc::clone(&h.ontology),
        Arc::new(CognifyConfig::default()),
        Some(ImproveConfig::default()),
        cognee::search::PreferenceConfig::default(),
        false,
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
        Some("SessionStored")
    );
    assert_eq!(
        obj.get("dataset_name").and_then(|v| v.as_str()),
        Some("ds_display")
    );
    // cognify_result, memify_result are #[serde(skip)] — absent.
    assert!(!obj.contains_key("cognify_result"));
    assert!(!obj.contains_key("memify_result"));
    // inner is gone from the struct entirely.
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
// Automatic improve: gate and debounce. Ported from Python's
// `cognee/tests/unit/api/v1/remember/test_remember_auto_improve_debounce.py`.
// ---------------------------------------------------------------------------

/// One session `remember()` against the harness.
async fn remember_turn(
    h: &Harness,
    owner: Uuid,
    session_id: &str,
    text: &str,
    self_improvement: bool,
    improve_config: Option<ImproveConfig>,
) -> cognee::api::remember::RememberResult {
    remember(
        vec![DataInput::Text(text.to_string())],
        "ds_auto_improve",
        Some(session_id),
        self_improvement,
        owner,
        None,
        Arc::clone(&h.add_pipeline),
        mock_llm(),
        Arc::clone(&h.storage),
        h.graph_db.clone() as Arc<_>,
        h.vector_db.clone() as Arc<_>,
        h.embedding_engine.clone() as Arc<_>,
        Some(h.db.clone()),
        Some(Arc::clone(&h.session_store)),
        Some(Arc::clone(&h.session_manager)),
        Some(h.checkpoint_store.clone() as Arc<_>),
        Arc::clone(&h.ontology),
        Arc::new(CognifyConfig::default()),
        improve_config,
        cognee::search::PreferenceConfig::default(),
        false,
    )
    .await
    .expect("remember")
}

async fn context_rows(h: &Harness, owner: Uuid, session_id: &str) -> usize {
    h.session_manager
        .get_session_context_entries(Some(session_id), Some(&owner.to_string()))
        .await
        .expect("context rows")
        .len()
}

// test_debounce_fires_at_most_ceil_n_over_entries
#[tokio::test]
async fn remember_debounce_fires_every_n_entries() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let config = ImproveConfig {
        debounce_entries: 3,
        ..ImproveConfig::default()
    };
    let mut fired = Vec::new();
    for i in 0..7 {
        let result = remember_turn(
            &h,
            owner,
            "s-debounce",
            &format!("turn {i}"),
            true,
            Some(config.clone()),
        )
        .await;
        assert_eq!(result.status, RememberStatus::SessionStored);
        // Let the bridge finish before the next call, as the Python test does.
        assert!(cognee::wait_for_background_tasks(None).await);
        fired.push(result.improve_launched);
    }
    assert_eq!(fired, [true, false, false, true, false, false, true]);
    // Every call stored its entry, fired or not.
    let entries = h
        .session_store
        .get_all_qa_entries("s-debounce", Some(&owner.to_string()))
        .await
        .unwrap();
    assert_eq!(entries.len(), 7);
}

// test_default_config_fires_every_time_and_reads_nothing
#[tokio::test]
async fn remember_default_config_fires_every_time_and_writes_no_state() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    for i in 0..3 {
        let result = remember_turn(
            &h,
            owner,
            "s-default",
            &format!("turn {i}"),
            true,
            Some(ImproveConfig::default()),
        )
        .await;
        assert!(result.improve_launched);
    }
    assert!(cognee::wait_for_background_tasks(None).await);
    assert_eq!(context_rows(&h, owner, "s-default").await, 0);
}

// test_invalid_improve_config_never_blocks_ingestion
#[tokio::test]
async fn remember_with_invalid_improve_config_stores_and_skips_the_improve() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let result = remember_turn(&h, owner, "s-broken-config", "turn", true, None).await;
    assert_eq!(result.status, RememberStatus::SessionStored);
    assert!(!result.improve_launched);
    let entries = h
        .session_store
        .get_all_qa_entries("s-broken-config", Some(&owner.to_string()))
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
}

// test_admission_is_not_asked_when_no_automatic_improve_would_run (the
// gate half): `self_improvement=false` and `auto_enabled=false` launch
// nothing.
#[tokio::test]
async fn remember_without_self_improvement_or_auto_enabled_launches_nothing() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let opted_out = remember_turn(
        &h,
        owner,
        "s-opted-out",
        "turn",
        false,
        Some(ImproveConfig::default()),
    )
    .await;
    assert!(!opted_out.improve_launched);
    let switched_off = remember_turn(
        &h,
        owner,
        "s-switched-off",
        "turn",
        true,
        Some(ImproveConfig {
            auto_enabled: false,
            debounce_entries: 3,
            ..ImproveConfig::default()
        }),
    )
    .await;
    assert!(!switched_off.improve_launched);
    // The debounce was not consulted, so no state row was written.
    assert_eq!(context_rows(&h, owner, "s-switched-off").await, 0);
}

/// The detached improve really runs: once drained, its fatal stage has
/// persisted the session into the dataset, which `remember` itself never
/// creates in session mode.
#[tokio::test]
async fn remember_detached_improve_runs_to_completion() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let result = remember_turn(
        &h,
        owner,
        "s-bridge-runs",
        "TerraScout indexes nightly.",
        true,
        Some(ImproveConfig::default()),
    )
    .await;
    assert!(result.improve_launched);
    assert!(cognee::wait_for_background_tasks(None).await);
    let dataset = cognee_database::IngestDb::get_dataset_by_name_in_tenant(
        &*h.db,
        "ds_auto_improve",
        owner,
        None,
    )
    .await
    .unwrap();
    assert!(dataset.is_some(), "the improve persisted the session");
}
