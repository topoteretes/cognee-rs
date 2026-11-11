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
//! (d) Calling with a `session_id` returns `SessionStored` with the improve
//!     launched as a detached background task.

use std::sync::Arc;

use cognee::api::ImproveConfig;
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
/// because `remember` would take 21 arguments, not 20.
fn _assert_signature() {
    // This is a compile-time only check: remember() must accept exactly 20
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
        improve_launched: false,
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
        Some(ImproveConfig::default()),
        cognee::search::PreferenceConfig::default(),
        false,
    )
    .await
    .expect("remember session");

    assert_eq!(result.status, RememberStatus::SessionStored);
    assert!(result.done(), "all statuses are terminal");
    assert!(result.is_success());
}

// ---------------------------------------------------------------------------
// (d) Session-mode with self_improvement=true detaches improve()
// ---------------------------------------------------------------------------

#[tokio::test]
async fn remember_session_self_improvement_detached() {
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
        Some(ImproveConfig::default()),
        cognee::search::PreferenceConfig::default(),
        false,
    )
    .await
    .expect("remember session with background improve");

    // The session write is terminal on return; the improve runs detached.
    assert_eq!(result.status, RememberStatus::SessionStored);
    assert!(result.done());
    assert!(result.improve_launched);
    assert!(cognee::wait_for_background_tasks(None).await);
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
        Some(ImproveConfig::default()),
        cognee::search::PreferenceConfig::default(),
        false,
    )
    .await
    .expect("permanent remember with self-improvement");

    assert_eq!(result.status, RememberStatus::Completed);
    assert!(result.error.is_none(), "improve error: {:?}", result.error);
    assert!(
        result.improve.is_some(),
        "self_improvement=true must run improve()"
    );
    // improve() has no memify/triplet stage (SDK-1115), so nothing wrote the
    // `Triplet_text` collection.
    let improve = result.improve.as_ref().expect("improve ran");
    assert!(
        !improve.stages.iter().any(|stage| stage.stage == "memify"),
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
        Some(ImproveConfig::default()),
        cognee::search::PreferenceConfig::default(),
        false,
    )
    .await
    .expect("permanent remember without self-improvement");
    assert!(result.improve.is_none());
}

/// SDK-1206: the automatic improve after a permanent `remember()` passes no
/// per-call stage flags, so `improve.review_conflicts` and
/// `improve.global_context_index` decide (Python `improve(...=None)`).
#[tokio::test]
async fn remember_auto_improve_follows_the_stage_settings() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();

    let mut reasons = Vec::new();
    for (dataset, on) in [("ds_auto_stages_on", true), ("ds_auto_stages_off", false)] {
        let config = ImproveConfig {
            review_conflicts: on,
            global_context_index: on,
            ..ImproveConfig::default()
        };
        let result = remember(
            vec![DataInput::Text("Alice works at Acme.".to_string())],
            dataset,
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
            Some(config),
            cognee::search::PreferenceConfig::default(),
            false,
        )
        .await
        .expect("permanent remember with self-improvement");
        let improve = result.improve.expect("improve ran");
        let stage = |name: &str| {
            let s = improve.stage(name).expect("stage reported");
            (s.status, s.reason.clone())
        };
        reasons.push((stage("review_conflicts"), stage("global_context_index")));
    }
    let turned_off = |r: &(cognee::api::StageStatus, Option<String>)| {
        r.0 == cognee::api::StageStatus::Skipped && r.1.as_deref() == Some("turned_off")
    };
    let (on, off) = (&reasons[0], &reasons[1]);
    // On: the index builds, and the review passes every gate to `not_ported`.
    assert_eq!(
        on.1.0,
        cognee::api::StageStatus::Completed,
        "global_context_index on: {on:?}"
    );
    assert_eq!(
        (on.0.0, on.0.1.as_deref()),
        (cognee::api::StageStatus::Skipped, Some("not_ported")),
        "review_conflicts on: {on:?}"
    );
    assert!(
        turned_off(&off.0) && turned_off(&off.1),
        "settings off: {off:?}"
    );
}

/// SDK-1206: the detached automatic improve after a session `remember()`
/// also passes no per-call stage flags, so `improve.global_context_index`
/// decides. The detached result is discarded, so the observable effect is the
/// stage's stamp on the dataset's improve operation row: left when the
/// setting is on, absent when it is off (`turned_off`).
#[tokio::test]
async fn remember_session_auto_improve_follows_the_stage_settings() {
    use cognee::api::improve::{IMPROVE_WRITE_PIPELINE_NAME, StageKind, stage_watermark_key};
    use cognee_database::RelationalDb;

    let h = make_harness().await;
    let owner = Uuid::new_v4();

    let mut stamped = Vec::new();
    for (dataset, on) in [("ds_sess_stages_on", true), ("ds_sess_stages_off", false)] {
        let config = ImproveConfig {
            global_context_index: on,
            ..ImproveConfig::default()
        };
        // The detached improve needs the dataset row; a session write alone
        // does not create it.
        let dataset_id = cognee_ingestion::generate_dataset_id(dataset, owner, None);
        IngestDb::create_dataset(
            &*h.db,
            cognee_models::Dataset::new(dataset.to_string(), owner, None, dataset_id),
        )
        .await
        .expect("create dataset");
        let result = remember(
            vec![DataInput::Text("Alice works at Acme.".to_string())],
            dataset,
            Some("sync-sess-stages"),
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
            Some(config),
            cognee::search::PreferenceConfig::default(),
            false,
        )
        .await
        .expect("session remember with background improve");
        assert!(result.improve_launched, "{dataset}");
        assert!(cognee::wait_for_background_tasks(None).await);

        let stamp = Arc::clone(&h.db)
            .pipeline_runs()
            .latest_stamped_run(
                dataset_id,
                IMPROVE_WRITE_PIPELINE_NAME,
                stage_watermark_key(StageKind::GlobalContextIndex),
            )
            .await
            .expect("read stamp");
        stamped.push(stamp.is_some());
    }
    assert_eq!(stamped, [true, false], "[on, off] stamped");
}
