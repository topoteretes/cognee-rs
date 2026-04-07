//! Tenant isolation integration tests.
//!
//! Verifies that multi-tenant boundaries are correctly enforced: datasets,
//! data records, and ID generation all respect tenant_id scoping.

use cognee_database::{IngestDb, connect, initialize, ops};
use cognee_ingestion::AddPipeline;
use cognee_models::DataInput;
use cognee_storage::{LocalStorage, StorageTrait};
use std::sync::Arc;
use tempfile::TempDir;
use uuid::Uuid;

/// Build a fresh `AddPipeline` backed by a real SQLite database and
/// `LocalStorage` inside `dir`. Returns the pipeline plus a shared database
/// handle for post-test assertions.
async fn make_pipeline(
    dir: &TempDir,
) -> (
    AddPipeline,
    Arc<cognee_database::DatabaseConnection>,
    Arc<LocalStorage>,
) {
    let db_path = dir.path().join("cognee.db");
    std::fs::File::create(&db_path).expect("sqlite db file should be created");
    let db_url = format!("sqlite://{}", db_path.display());

    let db = connect(&db_url).await.expect("connect");
    initialize(&db).await.expect("initialize");
    let db = Arc::new(db);

    let storage = Arc::new(LocalStorage::new(dir.path().join("storage")));
    storage.initialize().await.expect("storage.initialize");

    let pipeline = AddPipeline::new(
        storage.clone() as Arc<dyn StorageTrait>,
        db.clone() as Arc<dyn IngestDb>,
    );
    (pipeline, db, storage)
}

// ---------------------------------------------------------------------------
// C2.1 — Same dataset name, different tenants
// ---------------------------------------------------------------------------

#[tokio::test]
async fn same_dataset_name_different_tenants() {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, database, _storage) = make_pipeline(&dir).await;
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

    // Each tenant has its own dataset with the same name
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

// ---------------------------------------------------------------------------
// C2.2 — Tenant ID flows through pipeline
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tenant_id_flows_through_pipeline() {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, database, _storage) = make_pipeline(&dir).await;
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

    // Verify tenant_id on the returned Data record
    assert_eq!(
        result[0].tenant_id, tenant_id,
        "tenant_id must be set on the Data record"
    );

    // Verify tenant_id on the Dataset record
    let ds = ops::datasets::get_dataset_by_name(&database, "scoped_ds", owner, tenant_id)
        .await
        .expect("get dataset")
        .expect("dataset should exist");
    assert_eq!(
        ds.tenant_id, tenant_id,
        "tenant_id must be set on the Dataset record"
    );
}

// ---------------------------------------------------------------------------
// C2.3 — Same content, different tenants → separate Data IDs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn same_content_different_tenants_separate_data() {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, _database, _storage) = make_pipeline(&dir).await;
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

    // Different tenant_ids → different Data IDs (generate_data_id incorporates tenant_id)
    assert_ne!(
        r1[0].id, r2[0].id,
        "same content with different tenant_ids must produce different data IDs"
    );

    // Content hash is tenant-independent (content-only hashing)
    assert_eq!(
        r1[0].content_hash, r2[0].content_hash,
        "content hash should be identical regardless of tenant_id"
    );

    // Each record carries its own tenant_id
    assert_eq!(r1[0].tenant_id, tenant1);
    assert_eq!(r2[0].tenant_id, tenant2);
}
