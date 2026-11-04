#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
use std::sync::Arc;

use cognee_database::{self as database, DatabaseConnection, DeleteDb};
use cognee_delete::{DeleteError, DeleteMode, DeleteRequest, DeleteScope, DeleteService};
use cognee_models::{Data, Dataset};
use cognee_storage::{MockStorage, StorageTrait};
use uuid::Uuid;

async fn setup() -> (Arc<DatabaseConnection>, Arc<MockStorage>) {
    let db = database::connect("sqlite::memory:").await.unwrap();
    database::initialize(&db).await.unwrap();
    let storage = Arc::new(MockStorage::new());
    (Arc::new(db), storage)
}

// ---------------------------------------------------------------------------
// Test 1: deleting a nonexistent data item succeeds (best-effort cleanup)
//
// Python parity (Item 3, B6.6): when no relational `Data` row exists, Rust
// performs a best-effort graph/vector cleanup and returns success instead of
// a Validation error.  This matches Python's behavior when callers use a
// custom graph model that writes graph nodes without a relational row.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_nonexistent_data_returns_error() {
    let (db, storage) = setup().await;

    let svc = DeleteService::new(
        storage.clone() as Arc<dyn StorageTrait>,
        db.clone() as Arc<dyn DeleteDb>,
    );

    let owner_id = Uuid::new_v4();
    let data_id = Uuid::new_v4();

    // Since Item 3, a missing Data row no longer errors — it succeeds with
    // best-effort cleanup (matching Python's custom-graph-model path).
    let result = svc
        .execute(&DeleteRequest {
            scope: DeleteScope::Data {
                owner_id,
                data_id,
                dataset_name: None,
                delete_dataset_if_empty: false,
                tenant_id: None,
            },
            mode: DeleteMode::Soft,
            memory_only: false,
        })
        .await;

    // Must succeed, not error.
    let delete_result = result.expect("missing data row should succeed with best-effort cleanup");
    // `deleted_data` may be 0 or 1 depending on whether the pipeline attempts
    // a relational DELETE even for the ghost id; either is acceptable.
    assert!(
        delete_result.deleted_data <= 1,
        "unexpected large deletion count: {}",
        delete_result.deleted_data
    );
    // No datasets should have been removed since no dataset row exists.
    assert_eq!(
        delete_result.deleted_datasets, 0,
        "no dataset rows should be deleted for a ghost data_id"
    );
}

// ---------------------------------------------------------------------------
// Test 2: deleting a nonexistent dataset returns a Validation error
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_nonexistent_dataset_returns_error() {
    let (db, storage) = setup().await;

    let svc = DeleteService::new(
        storage.clone() as Arc<dyn StorageTrait>,
        db.clone() as Arc<dyn DeleteDb>,
    );

    let owner_id = Uuid::new_v4();

    let result = svc
        .execute(&DeleteRequest {
            scope: DeleteScope::Dataset {
                owner_id,
                dataset_name: "no_such_ds".to_string(),
                tenant_id: None,
            },
            mode: DeleteMode::Soft,
            memory_only: false,
        })
        .await;

    let err = result.expect_err("should fail for nonexistent dataset");
    match &err {
        DeleteError::Validation(msg) => {
            assert!(
                msg.to_lowercase().contains("not found"),
                "expected 'not found' in message, got: {msg}"
            );
        }
        other => panic!("expected DeleteError::Validation, got: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Test 3: deleting data that is not attached to the specified dataset
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_data_not_in_specified_dataset_returns_error() {
    let (db, storage) = setup().await;

    let owner_id = Uuid::new_v4();

    // Create a real dataset.
    let dataset = Dataset::new("real_ds".to_string(), owner_id, None, Uuid::new_v4());
    database::ops::datasets::create_dataset(&db, dataset)
        .await
        .unwrap();

    // Create a real data item (not attached to the dataset).
    let location = storage
        .store(b"orphan content", "orphan.txt")
        .await
        .unwrap();
    let data_id = Uuid::new_v4();
    let data = Data::builder(
        data_id,
        "orphan.txt",
        &location,
        "file://orphan.txt",
        "txt",
        "text/plain",
        "orphan_hash",
        owner_id,
    )
    .build();
    database::ops::data::create_data(&db, data).await.unwrap();

    // Try to delete the data item scoped to the dataset it is NOT attached to.
    let svc = DeleteService::new(
        storage.clone() as Arc<dyn StorageTrait>,
        db.clone() as Arc<dyn DeleteDb>,
    );

    let result = svc
        .execute(&DeleteRequest {
            scope: DeleteScope::Data {
                owner_id,
                data_id,
                dataset_name: Some("real_ds".to_string()),
                delete_dataset_if_empty: false,
                tenant_id: None,
            },
            mode: DeleteMode::Soft,
            memory_only: false,
        })
        .await;

    let err = result.expect_err("should fail when data is not attached to the dataset");
    match &err {
        DeleteError::Validation(msg) => {
            assert!(
                msg.to_lowercase().contains("not attached"),
                "expected 'not attached' in message, got: {msg}"
            );
        }
        other => panic!("expected DeleteError::Validation, got: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Test 4: deleting a user with no datasets succeeds with zero deletions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_user_with_no_datasets_succeeds_with_zero_deletions() {
    let (db, storage) = setup().await;

    let svc = DeleteService::new(
        storage.clone() as Arc<dyn StorageTrait>,
        db.clone() as Arc<dyn DeleteDb>,
    );

    let owner_id = Uuid::new_v4();

    let result = svc
        .execute(&DeleteRequest {
            scope: DeleteScope::User {
                owner_id,
                tenant_id: None,
            },
            mode: DeleteMode::Soft,
            memory_only: false,
        })
        .await
        .expect("deleting a user with no datasets should succeed");

    assert_eq!(
        result.deleted_datasets, 0,
        "no datasets should be deleted for a new owner"
    );
    assert_eq!(
        result.deleted_data, 0,
        "no data should be deleted for a new owner"
    );
}

// ---------------------------------------------------------------------------
// Test 5: the shared-file reference count runs AFTER the row delete
//
// Python `delete_data_entity` commits the row delete, then
// `remove_data_file_if_unreferenced` counts. Two rows of the same content (in
// two datasets) share one stored file; when a concurrent delete removes the
// sibling row as this one goes, the count must see both gone and remove the
// file. Counting before the delete saw the sibling and leaked the file. A
// SQLite trigger stands in for the concurrent deleter: it removes the sibling
// the moment the row under test is deleted.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn shared_file_is_removed_when_sibling_row_goes_concurrently() {
    use sea_orm::{ConnectionTrait, Statement};

    let (db, storage) = setup().await;
    let owner_id = Uuid::new_v4();
    let location = storage.store(b"shared bytes", "shared.txt").await.unwrap();

    let mut ids = Vec::new();
    for name in ["ds_a", "ds_b"] {
        let dataset = Dataset::new(name.to_string(), owner_id, None, Uuid::new_v4());
        let dataset_id = dataset.id;
        database::ops::datasets::create_dataset(&db, dataset)
            .await
            .unwrap();
        let data_id = Uuid::new_v4();
        let data = Data::builder(
            data_id,
            "shared.txt",
            &location,
            "file://shared.txt",
            "txt",
            "text/plain",
            "same_hash",
            owner_id,
        )
        .dataset_id(dataset_id)
        .build();
        database::ops::data::create_data(&db, data).await.unwrap();
        database::ops::datasets::attach_data_to_dataset(&db, dataset_id, data_id)
            .await
            .unwrap();
        ids.push(data_id);
    }
    let (first, second) = (ids[0], ids[1]);

    let hex = |id: Uuid| cognee_database::uuid_hex::to_hex(id);
    db.execute(Statement::from_string(
        db.get_database_backend(),
        format!(
            "CREATE TRIGGER concurrent_sibling_delete AFTER DELETE ON data \
             WHEN old.id = '{}' BEGIN DELETE FROM data WHERE id = '{}'; END",
            hex(first),
            hex(second)
        ),
    ))
    .await
    .unwrap();

    let svc = DeleteService::new(
        storage.clone() as Arc<dyn StorageTrait>,
        db.clone() as Arc<dyn DeleteDb>,
    );
    let result = svc
        .execute(&DeleteRequest {
            scope: DeleteScope::Data {
                owner_id,
                data_id: first,
                dataset_name: Some("ds_a".to_string()),
                delete_dataset_if_empty: false,
                tenant_id: None,
            },
            mode: DeleteMode::Soft,
            memory_only: false,
        })
        .await
        .expect("delete succeeds");

    assert_eq!(result.deleted_data, 1);
    assert!(
        !storage.exists(&location).await.unwrap(),
        "no row references the file any more, so it must be removed"
    );
}
