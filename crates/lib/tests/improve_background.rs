#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! `improve_in_background` (Python's `improve(run_in_background=True)`):
//! the dataset is resolved and the options validated in the caller, then the
//! run is spawned and the `running` result returned; `wait_for_background_tasks`
//! drains it. The pending count is process-global, so this file holds one test.

use std::sync::Arc;

use cognee::api::ApiError;
use cognee::api::improve::{ImproveConfig, OwnedImproveParams, improve_in_background_then};
use cognee_cognify::CognifyConfig;
use cognee_database::{AclDb, IngestDb, RelationalDb, connect, initialize};
use cognee_embedding::MockEmbeddingEngine;
use cognee_graph::MockGraphDB;
use cognee_ingestion::AddPipeline;
use cognee_models::Dataset;
use cognee_ontology::NoOpOntologyResolver;
use cognee_session::{FsSessionStore, SessionManager, SessionStore};
use cognee_storage::{LocalStorage, StorageTrait};
use cognee_test_utils::{MockAclDb, MockLlm};
use cognee_vector::MockVectorDB;
use tempfile::TempDir;
use tokio::sync::Semaphore;
use uuid::Uuid;

struct Harness {
    _temp: TempDir,
    db: Arc<dyn RelationalDb>,
    storage: Arc<dyn StorageTrait>,
    add_pipeline: Arc<AddPipeline>,
    session_store: Arc<dyn SessionStore>,
    session_manager: Arc<SessionManager>,
}

async fn harness() -> Harness {
    let temp = TempDir::new().unwrap();
    let db_path = temp.path().join("cognee.db");
    std::fs::File::create(&db_path).unwrap();
    let db = connect(&format!("sqlite://{}", db_path.display()))
        .await
        .unwrap();
    initialize(&db).await.unwrap();
    let db = Arc::new(db);
    let storage: Arc<dyn StorageTrait> = Arc::new(LocalStorage::new(temp.path().join("storage")));
    storage.initialize().await.unwrap();
    let ingest_db: Arc<dyn IngestDb> = db.clone();
    let add_pipeline = Arc::new(
        AddPipeline::new(Arc::clone(&storage), ingest_db)
            .with_thread_pool(Arc::new(
                cognee::core::RayonThreadPool::with_default_threads().unwrap(),
            ))
            .with_graph_db(Arc::new(MockGraphDB::new()) as Arc<dyn cognee_graph::GraphDBTrait>)
            .with_vector_db(Arc::new(MockVectorDB::new()) as Arc<dyn cognee_vector::VectorDB>)
            .with_database(db.clone()),
    );
    let session_store: Arc<dyn SessionStore> =
        Arc::new(FsSessionStore::new(temp.path().join("sessions")));
    let session_manager = Arc::new(SessionManager::new(Arc::clone(&session_store)));
    Harness {
        _temp: temp,
        db,
        storage,
        add_pipeline,
        session_store,
        session_manager,
    }
}

fn params(h: &Harness, owner: Uuid, name: &str, id: Option<Uuid>) -> OwnedImproveParams {
    OwnedImproveParams {
        dataset_name: name.to_string(),
        dataset_id: id,
        session_ids: Some(vec![String::new(), "s1".to_string()]),
        owner_id: owner,
        tenant_id: None,
        feedback_alpha: None,
        config: ImproveConfig::default(),
        personalization: cognee::search::PreferenceConfig::default(),
        auto_feedback: false,
        build_global_context_index: Some(false),
        review_conflicts: Some(false),
        build_truth_subspace: false,
        llm: Arc::new(MockLlm::empty()),
        storage: Arc::clone(&h.storage),
        graph_db: Arc::new(MockGraphDB::new()),
        vector_db: Arc::new(MockVectorDB::new()),
        embedding_engine: Arc::new(MockEmbeddingEngine::new(16)),
        ontology_resolver: Arc::new(NoOpOntologyResolver::new()),
        db: Some(h.db.clone()),
        session_store: Some(Arc::clone(&h.session_store)),
        session_manager: Some(Arc::clone(&h.session_manager)),
        add_pipeline: Some(Arc::clone(&h.add_pipeline)),
        dataset_locks: None,
        acl_db: None,
        cognify_config: CognifyConfig::default(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn background_improve_resolves_first_returns_running_and_drains() {
    let h = harness().await;
    let owner = Uuid::new_v4();

    // The run is held until released, so "returns at once" is observed while
    // it is provably still pending.
    let gate = Arc::new(Semaphore::new(0));
    let release = Arc::clone(&gate);
    let running =
        improve_in_background_then(params(&h, owner, "bg_ds", None), move |_| async move {
            let _permit = release.acquire().await.expect("gate open");
        })
        .await
        .expect("dispatched");
    assert!(running.background);
    assert!(!running.finished);
    assert_eq!(running.status().as_str(), "running");
    assert!(running.stages.is_empty());
    assert_eq!(running.dataset_name, "bg_ds");
    assert_eq!(running.session_ids, vec!["s1".to_string()]);
    // The name was resolved (here: created for the caller) before dispatch.
    let created = IngestDb::get_dataset_by_name_in_tenant(&*h.db, "bg_ds", owner, None)
        .await
        .unwrap()
        .expect("the named dataset is created for the caller");
    assert_eq!(running.dataset_id, Some(created.id));
    assert_eq!(created.owner_id, owner);

    assert!(cognee::pending_background_tasks() >= 1);
    assert!(
        !cognee::wait_for_background_tasks(Some(std::time::Duration::from_millis(50))).await,
        "the held run is still pending"
    );
    gate.add_permits(1);
    assert!(cognee::wait_for_background_tasks(Some(std::time::Duration::from_secs(60))).await);
    assert_eq!(cognee::pending_background_tasks(), 0);

    // An existing name resolves to the same dataset, by its stored id.
    let again = improve_in_background_then(params(&h, owner, "bg_ds", None), |_| async {})
        .await
        .expect("dispatched");
    assert_eq!(again.dataset_id, Some(created.id));
    assert!(cognee::wait_for_background_tasks(Some(std::time::Duration::from_secs(60))).await);

    // Errors before dispatch reach the caller and spawn nothing.
    let unknown = Uuid::new_v4();
    let err = improve_in_background_then(params(&h, owner, "x", Some(unknown)), |_| async {})
        .await
        .expect_err("an unknown dataset id is refused");
    assert!(
        matches!(err, ApiError::InvalidArgument(ref m) if m.contains("not found")),
        "{err:?}"
    );

    let foreign = IngestDb::create_dataset(
        &*h.db,
        Dataset::new("foreign".to_string(), Uuid::new_v4(), None, Uuid::new_v4()),
    )
    .await
    .unwrap();
    let err = improve_in_background_then(params(&h, owner, "x", Some(foreign.id)), |_| async {})
        .await
        .expect_err("another owner's dataset is refused without an ACL grant");
    assert!(matches!(err, ApiError::PermissionDenied(_)), "{err:?}");

    let mut bad_alpha = params(&h, owner, "bg_ds", None);
    bad_alpha.feedback_alpha = Some(5.0);
    let err = improve_in_background_then(bad_alpha, |_| async {})
        .await
        .expect_err("an out-of-range alpha is refused");
    assert!(matches!(err, ApiError::InvalidArgument(_)), "{err:?}");
    assert_eq!(cognee::pending_background_tasks(), 0);

    // With an ACL the `write` grant decides, for an id and for a name alike;
    // a shared dataset (another owner, granted) is admitted by id.
    let acl = Arc::new(MockAclDb::new());
    let mut no_grant = params(&h, owner, "x", Some(foreign.id));
    no_grant.acl_db = Some(acl.clone() as Arc<dyn AclDb>);
    let err = improve_in_background_then(no_grant, |_| async {})
        .await
        .expect_err("no write grant");
    assert!(matches!(err, ApiError::PermissionDenied(_)), "{err:?}");
    let mut named_no_grant = params(&h, owner, "bg_ds", None);
    named_no_grant.acl_db = Some(acl.clone() as Arc<dyn AclDb>);
    let err = improve_in_background_then(named_no_grant, |_| async {})
        .await
        .expect_err("no write grant on the named dataset");
    assert!(matches!(err, ApiError::PermissionDenied(_)), "{err:?}");
    assert_eq!(cognee::pending_background_tasks(), 0);

    acl.grant_permission(owner, foreign.id, "write")
        .await
        .unwrap();
    let mut shared = params(&h, owner, "ignored", Some(foreign.id));
    shared.acl_db = Some(acl.clone() as Arc<dyn AclDb>);
    let running = improve_in_background_then(shared, |_| async {})
        .await
        .expect("a granted shared dataset is admitted");
    assert_eq!(running.dataset_id, Some(foreign.id));
    assert_eq!(running.dataset_name, "foreign", "the stored name wins");

    // A name created under an ACL is granted to its owner.
    let mut fresh = params(&h, owner, "acl_new", None);
    fresh.acl_db = Some(acl.clone() as Arc<dyn AclDb>);
    let running = improve_in_background_then(fresh, |_| async {})
        .await
        .expect("a new name is created and granted");
    let id = running.dataset_id.expect("resolved id");
    assert!(acl.has_permission(owner, id, "write").await.unwrap());
    assert!(cognee::wait_for_background_tasks(Some(std::time::Duration::from_secs(60))).await);
    assert_eq!(cognee::pending_background_tasks(), 0);
}
