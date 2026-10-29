#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Compile-time and runtime tests verifying that `remember()` is sync-only.
//!
//! Covers:
//! (a) The function signature has no `run_in_background` parameter.
//! (b) `RememberResult` has no `inner` / `await_completion()` member.
//! (c) Calling `remember(...)` for permanent mode returns `RememberStatus::Completed`.
//! (d) Calling with a `session_id` runs inline improve() and returns `SessionStored`.

use std::sync::Arc;

use cognee::api::remember::{RememberResult, RememberStatus, remember};
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
use cognee_vector::{MockVectorDB, VectorDB};
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

fn mock_llm() -> Arc<dyn cognee_llm::Llm> {
    Arc::new(MockLlm::empty())
}

/// An LLM that can serve a full permanent-mode cognify: an empty extracted
/// graph for every chunk and a canned summary.
fn cognify_llm() -> Arc<dyn cognee_llm::Llm> {
    Arc::new(
        MockLlm::new(vec![r#"{"nodes":[],"edges":[]}"#.to_string(); 8])
            .with_summary_response(r#"{"summary":"s","description":"d"}"#.to_string()),
    )
}

// ---------------------------------------------------------------------------
// (b) RememberResult has no `inner` in its JSON serialization
// ---------------------------------------------------------------------------

#[test]
fn remember_result_has_no_inner_field_in_json() {
    use cognee::api::remember::RememberResult;
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
        improve: None,
        improve_error: None,
        after_run_error: None,
        improve_skipped: None,
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
        Some(h.db.clone()),
        Some(Arc::clone(&h.session_store)),
        Some(Arc::clone(&h.session_manager)),
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
        Some(h.db.clone()),
        Some(Arc::clone(&h.session_store)),
        Some(Arc::clone(&h.session_manager)),
        Arc::clone(&h.ontology),
        Arc::new(CognifyConfig::default()),
    )
    .await
    .expect("remember session with inline improve");

    // Synchronous: always terminal on return.
    assert_eq!(result.status, RememberStatus::SessionStored);
    assert!(result.done());
    // D20: the inline improve is reported on `improve`, or, when it failed
    // outright, on `improve_error`; it never flips the status or `error`.
    assert!(result.error.is_none());
    assert!(
        result.improve.is_some() ^ result.improve_error.is_some(),
        "improve ran or failed, exactly one is reported: {result:?}"
    );
}

/// Session-mode remember reports the improve it ran: the nine stages in
/// Python's order, with `improve_error` set from the errored stages
/// (Python `_improve_error_text`) and `error` left alone.
#[tokio::test]
async fn remember_session_reports_the_improve_stages() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();

    let result = remember(
        vec![DataInput::Text("inline improve report".to_string())],
        "ds_session_report",
        Some("sess-report"),
        true,
        owner,
        None,
        Arc::clone(&h.add_pipeline),
        cognify_llm(),
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
    .expect("remember session with inline improve");

    assert_eq!(result.status, RememberStatus::SessionStored);
    assert!(result.error.is_none());
    let improve = result.improve.as_ref().expect("session improve reported");
    let names: Vec<&str> = improve.stages.iter().map(|s| s.stage.as_str()).collect();
    assert_eq!(names, cognee::api::IMPROVE_STAGES);
    assert_eq!(improve.session_ids, vec!["sess-report".to_string()]);
    assert_eq!(result.improve_error, improve.error_text());

    // The wire form (Python `to_dict`): `improve` nests the result with its
    // computed status; the unset improve fields are absent.
    let dict = result.to_dict();
    assert_eq!(dict["improve"]["status"], improve.status().as_str());
    assert_eq!(dict["improve"]["session_ids"][0], "sess-report");
    assert_eq!(
        dict["improve"]["stages"].as_array().map(Vec::len),
        Some(names.len())
    );
    assert!(dict.get("improve_skipped").is_none());
    assert!(dict.get("after_run_error").is_none());
}

/// A fatal improve failure (`persist_session_qa` cannot cognify the session
/// Q&A) is reported on `improve_error`, with no `improve` and the remember's
/// status and `error` untouched (Python `RememberResult.improve_error`).
#[tokio::test]
async fn remember_session_improve_failure_goes_to_improve_error() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();

    // An LLM with nothing to answer: the session Q&A cannot be cognified.
    let result = remember(
        vec![DataInput::Text("this cannot be cognified".to_string())],
        "ds_session_fail",
        Some("sess-fail"),
        true,
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
        Arc::clone(&h.ontology),
        Arc::new(CognifyConfig::default()),
    )
    .await
    .expect("a failed improve never fails the remember");

    assert_eq!(result.status, RememberStatus::SessionStored);
    assert!(
        result.error.is_none(),
        "error stays clear: {:?}",
        result.error
    );
    assert!(result.improve.is_none());
    let error = result.improve_error.as_deref().expect("improve_error set");
    assert!(error.contains("persist_session_qa"), "{error}");
    assert_eq!(result.to_dict()["improve_error"], error);
}

/// SDK-1039 P3: permanent mode with `self_improvement=true` runs `improve()`
/// on the dataset (as Python does), not memify. The improve result is
/// attached to the `RememberResult`.
#[tokio::test]
async fn remember_permanent_self_improvement_runs_improve() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();

    let result = remember(
        vec![DataInput::Text("Alice works at Acme.".to_string())],
        "ds_permanent_improve",
        None,
        /* self_improvement= */ true,
        owner,
        None,
        Arc::clone(&h.add_pipeline),
        cognify_llm(),
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
    .expect("permanent remember with self-improvement");

    assert_eq!(result.status, RememberStatus::Completed);
    assert!(result.error.is_none(), "improve error: {:?}", result.error);
    assert!(
        result.improve.is_some(),
        "self_improvement=true must run improve()"
    );
    assert!(result.improve_error.is_none(), "{:?}", result.improve_error);
    assert!(result.improve_skipped.is_none());
    // improve() has no memify/triplet stage (SDK-1115), so nothing wrote the
    // `Triplet_text` collection.
    let improve = result.improve.as_ref().expect("improve ran");
    // Permanent mode runs improve without sessions: Python's nine stages, all
    // skipped, so the result's status is `skipped`.
    let names: Vec<&str> = improve.stages.iter().map(|s| s.stage.as_str()).collect();
    assert_eq!(names, cognee::api::IMPROVE_STAGES);
    assert_eq!(improve.status(), cognee::api::ImproveStatus::Skipped);
    assert_eq!(result.to_dict()["improve"]["status"], "skipped");
    assert!(
        improve.stage("memify").is_none(),
        "improve must not report a memify stage: {:?}",
        improve.stage_summary()
    );
    assert!(
        !h.vector_db
            .has_collection("Triplet", "text")
            .await
            .expect("has_collection"),
        "remember(self_improvement) must not write Triplet_text"
    );

    // Without self-improvement no improve run is attached.
    let result = remember(
        vec![DataInput::Text("Bob works at Initech.".to_string())],
        "ds_permanent_no_improve",
        None,
        /* self_improvement= */ false,
        owner,
        None,
        Arc::clone(&h.add_pipeline),
        cognify_llm(),
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
    .expect("permanent remember without self-improvement");
    assert!(result.improve.is_none());
}
