//! Tenant isolation integration tests.
//!
//! Verifies that multi-tenant boundaries are correctly enforced: datasets,
//! data records, and ID generation all respect tenant_id scoping.
//!
//! Each test is instantiated twice: once with SQLite and once with PostgreSQL.
//! The PostgreSQL variant is skipped automatically when `DB_PROVIDER` is not
//! set to `"postgres"` in the environment.

use cognee_core::RayonThreadPool;
use cognee_database::{IngestDb, connect, initialize, ops};
use cognee_graph::MockGraphDB;
use cognee_ingestion::AddPipeline;
use cognee_models::DataInput;
use cognee_storage::{LocalStorage, StorageTrait};
use cognee_vector::MockVectorDB;
use std::sync::Arc;
use tempfile::TempDir;
use uuid::Uuid;

/// Build a SQLite URL backed by a file inside `dir`, creating the file first.
fn sqlite_db_url(dir: &TempDir) -> String {
    let db_path = dir.path().join("cognee.db");
    std::fs::File::create(&db_path).expect("sqlite db file should be created");
    format!("sqlite://{}", db_path.display())
}

/// Build a fresh `AddPipeline` backed by the given database URL and
/// `LocalStorage` inside `dir`. Returns the pipeline plus a shared database
/// handle for post-test assertions.
async fn make_pipeline(
    dir: &TempDir,
    db_url: &str,
) -> (
    AddPipeline,
    Arc<cognee_database::DatabaseConnection>,
    Arc<LocalStorage>,
) {
    let db = connect(db_url).await.expect("connect");
    initialize(&db).await.expect("initialize");
    let db = Arc::new(db);

    let storage = Arc::new(LocalStorage::new(dir.path().join("storage")));
    storage.initialize().await.expect("storage.initialize");

    let pipeline = AddPipeline::new(
        storage.clone() as Arc<dyn StorageTrait>,
        db.clone() as Arc<dyn IngestDb>,
    )
    .with_thread_pool(Arc::new(RayonThreadPool::with_default_threads().unwrap()))
    .with_graph_db(Arc::new(MockGraphDB::new()))
    .with_vector_db(Arc::new(MockVectorDB::new()))
    .with_database(Arc::clone(&db));
    (pipeline, db, storage)
}

// ---------------------------------------------------------------------------
// C2.1 — Same dataset name, different tenants
// ---------------------------------------------------------------------------

async fn impl_same_dataset_name_different_tenants(db_url: &str) {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, database, _storage) = make_pipeline(&dir, db_url).await;
    let owner = Uuid::new_v4();
    let tenant1 = Some(Uuid::new_v4());
    let tenant2 = Some(Uuid::new_v4());

    pipeline
        .add(
            vec![DataInput::Text("content for tenant 1".to_string())],
            "AI",
            owner,
            tenant1,
        )
        .await
        .expect("add for tenant 1");

    pipeline
        .add(
            vec![DataInput::Text("content for tenant 2".to_string())],
            "AI",
            owner,
            tenant2,
        )
        .await
        .expect("add for tenant 2");

    let ds1 = ops::datasets::get_dataset_by_name(&database, "AI", owner, tenant1)
        .await
        .expect("get ds tenant1")
        .expect("dataset for tenant1 should exist");
    let ds2 = ops::datasets::get_dataset_by_name(&database, "AI", owner, tenant2)
        .await
        .expect("get ds tenant2")
        .expect("dataset for tenant2 should exist");

    assert_ne!(
        ds1.id, ds2.id,
        "same dataset name under different tenants must produce different dataset IDs"
    );
    assert_eq!(
        ds1.tenant_id, tenant1,
        "tenant1 dataset should have correct tenant_id"
    );
    assert_eq!(
        ds2.tenant_id, tenant2,
        "tenant2 dataset should have correct tenant_id"
    );
}

#[tokio::test]
async fn same_dataset_name_different_tenants_sqlite() {
    let dir = TempDir::new().expect("tempdir for url");
    let url = sqlite_db_url(&dir);
    impl_same_dataset_name_different_tenants(&url).await;
}

#[tokio::test]
async fn same_dataset_name_different_tenants_pg() {
    let Some(url) = cognee_test_utils::pg_test_url() else {
        return;
    };
    impl_same_dataset_name_different_tenants(&url).await;
}

// ---------------------------------------------------------------------------
// C2.2 — Tenant ID flows through pipeline
// ---------------------------------------------------------------------------

async fn impl_tenant_id_flows_through_pipeline(db_url: &str) {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, database, _storage) = make_pipeline(&dir, db_url).await;
    let owner = Uuid::new_v4();
    let tenant_id = Some(Uuid::new_v4());

    let result = pipeline
        .add(
            vec![DataInput::Text("tenant-scoped data".to_string())],
            "scoped_ds",
            owner,
            tenant_id,
        )
        .await
        .expect("add with tenant_id");

    assert_eq!(
        result[0].tenant_id, tenant_id,
        "tenant_id must be set on the Data record"
    );

    let ds = ops::datasets::get_dataset_by_name(&database, "scoped_ds", owner, tenant_id)
        .await
        .expect("get dataset")
        .expect("dataset should exist");
    assert_eq!(
        ds.tenant_id, tenant_id,
        "tenant_id must be set on the Dataset record"
    );
}

#[tokio::test]
async fn tenant_id_flows_through_pipeline_sqlite() {
    let dir = TempDir::new().expect("tempdir for url");
    let url = sqlite_db_url(&dir);
    impl_tenant_id_flows_through_pipeline(&url).await;
}

#[tokio::test]
async fn tenant_id_flows_through_pipeline_pg() {
    let Some(url) = cognee_test_utils::pg_test_url() else {
        return;
    };
    impl_tenant_id_flows_through_pipeline(&url).await;
}

// ---------------------------------------------------------------------------
// C2.3 — Same content, different tenants → separate Data IDs
// ---------------------------------------------------------------------------

async fn impl_same_content_different_tenants_separate_data(db_url: &str) {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, _database, _storage) = make_pipeline(&dir, db_url).await;
    let owner = Uuid::new_v4();
    let tenant1 = Some(Uuid::new_v4());
    let tenant2 = Some(Uuid::new_v4());

    let text = "Identical content across tenants.";

    let r1 = pipeline
        .add(
            vec![DataInput::Text(text.to_string())],
            "ds_t1",
            owner,
            tenant1,
        )
        .await
        .expect("add tenant1");

    let r2 = pipeline
        .add(
            vec![DataInput::Text(text.to_string())],
            "ds_t2",
            owner,
            tenant2,
        )
        .await
        .expect("add tenant2");

    assert_ne!(
        r1[0].id, r2[0].id,
        "same content with different tenant_ids must produce different data IDs"
    );

    assert_eq!(
        r1[0].content_hash, r2[0].content_hash,
        "content hash should be identical regardless of tenant_id"
    );

    assert_eq!(r1[0].tenant_id, tenant1);
    assert_eq!(r2[0].tenant_id, tenant2);
}

#[tokio::test]
async fn same_content_different_tenants_separate_data_sqlite() {
    let dir = TempDir::new().expect("tempdir for url");
    let url = sqlite_db_url(&dir);
    impl_same_content_different_tenants_separate_data(&url).await;
}

#[tokio::test]
async fn same_content_different_tenants_separate_data_pg() {
    let Some(url) = cognee_test_utils::pg_test_url() else {
        return;
    };
    impl_same_content_different_tenants_separate_data(&url).await;
}
