use std::sync::Arc;

use cognee_lib::add::HashAlgorithm;
use cognee_lib::add::build_add_pipeline;
use cognee_lib::api::{ApiError, DatasetRef, ForgetTarget, forget};
use cognee_lib::core::{NoopWatcher, Value, execute};
use cognee_lib::database::{DeleteDb, IngestDb, ops};
use cognee_lib::delete::{DeleteMode, DeleteRequest, DeleteScope, DeleteService};
use cognee_lib::models::{Data, DataInput, Dataset};
use cognee_lib::storage::StorageTrait;
use cognee_test_utils::{MockStorage, test_task_context};
use uuid::Uuid;

/// Downcast an `Arc<dyn Value>` to `&T` by going through the vtable.
fn downcast_ref<T: 'static>(v: &Arc<dyn Value>) -> &T {
    (**v)
        .as_any()
        .downcast_ref::<T>()
        .unwrap_or_else(|| panic!("expected {}", std::any::type_name::<T>()))
}

/// Run the add pipeline for one text input and return the resulting `Data`.
async fn add_text_to_dataset(
    storage: &Arc<dyn StorageTrait>,
    db: &Arc<cognee_lib::database::DatabaseConnection>,
    ctx: &Arc<cognee_lib::core::TaskContext>,
    dataset_name: &str,
    text: &str,
    owner_id: Uuid,
) -> Data {
    let pipeline = build_add_pipeline(
        Arc::clone(storage),
        db.clone() as Arc<dyn IngestDb>,
        HashAlgorithm::default(),
        dataset_name,
        owner_id,
        None,
    );

    let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(DataInput::Text(text.to_string()))];
    let results = execute(&pipeline, inputs, Arc::clone(ctx), &NoopWatcher)
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    downcast_ref::<Data>(&results[0]).clone()
}

/// Seed one dataset + one data item (using `MockStorage::store`, which writes
/// to the in-memory HashMap), attach them, and return `(dataset_id, data_id,
/// storage_location)`.
///
/// This mirrors the helper in `crates/delete/src/lib.rs` tests.
async fn seed_dataset_with_data(
    db: &cognee_lib::database::DatabaseConnection,
    storage: &MockStorage,
    owner_id: Uuid,
    dataset_name: &str,
) -> (Uuid, Uuid, String) {
    let dataset = Dataset::new(dataset_name.to_string(), owner_id, None, Uuid::new_v4());
    let dataset_id = dataset.id;
    ops::datasets::create_dataset(db, dataset).await.unwrap();

    let location = storage.store(b"test content", "test.txt").await.unwrap();

    let data_id = Uuid::new_v4();
    let data = Data::builder(
        data_id,
        "test.txt",
        &location,
        "file://test.txt",
        "txt",
        "text/plain",
        "hash_placeholder",
        owner_id,
    )
    .build();
    ops::data::create_data(db, data).await.unwrap();
    ops::datasets::attach_data_to_dataset(db, dataset_id, data_id)
        .await
        .unwrap();

    (dataset_id, data_id, location)
}

// ---------------------------------------------------------------------------
// G1.1 — dataset deletion removes data records
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dataset_deletion_removes_data_records() {
    let (_handle, ctx, db) = test_task_context().await;
    let storage = Arc::new(MockStorage::new());
    let owner_id = Uuid::new_v4();

    let data = add_text_to_dataset(
        &(storage.clone() as Arc<dyn StorageTrait>),
        &db,
        &ctx,
        "del_ds",
        "content to delete",
        owner_id,
    )
    .await;

    // Verify the data record exists before deletion.
    let found = ops::data::get_data(&db, data.id).await.unwrap();
    assert!(found.is_some(), "data record should exist before deletion");

    // Delete the dataset via DeleteService.
    let svc = DeleteService::new(
        storage.clone() as Arc<dyn StorageTrait>,
        db.clone() as Arc<dyn DeleteDb>,
    );
    let result = svc
        .execute(&DeleteRequest {
            scope: DeleteScope::Dataset {
                owner_id,
                dataset_name: "del_ds".to_string(),
            },
            mode: DeleteMode::Soft,
        })
        .await
        .expect("delete should succeed");

    assert_eq!(result.deleted_datasets, 1);
    assert_eq!(result.deleted_data, 1);

    // Verify the data record is gone.
    let gone = ops::data::get_data(&db, data.id).await.unwrap();
    assert!(gone.is_none(), "data record should be gone after deletion");
}

// ---------------------------------------------------------------------------
// G1.2 — dataset deletion removes storage files
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dataset_deletion_removes_storage_files() {
    let (_handle, _ctx, db) = test_task_context().await;
    let storage = Arc::new(MockStorage::new());
    let owner_id = Uuid::new_v4();

    let (_dataset_id, data_id, location) =
        seed_dataset_with_data(&db, &storage, owner_id, "storage_ds").await;

    // Verify the storage file exists before deletion.
    assert!(
        storage.exists(&location).await.unwrap(),
        "storage file should exist before deletion"
    );

    // Delete the dataset.
    let svc = DeleteService::new(
        storage.clone() as Arc<dyn StorageTrait>,
        db.clone() as Arc<dyn DeleteDb>,
    );
    let result = svc
        .execute(&DeleteRequest {
            scope: DeleteScope::Dataset {
                owner_id,
                dataset_name: "storage_ds".to_string(),
            },
            mode: DeleteMode::Soft,
        })
        .await
        .expect("delete should succeed");

    assert_eq!(result.deleted_datasets, 1);
    assert_eq!(result.deleted_data, 1);
    assert_eq!(result.deleted_storage_files, 1);

    // Verify the storage file is gone.
    assert!(
        !storage.exists(&location).await.unwrap(),
        "storage file should be removed after deletion"
    );

    // Verify the data record is also gone.
    let gone = ops::data::get_data(&db, data_id).await.unwrap();
    assert!(gone.is_none(), "data record should be gone after deletion");
}

// ---------------------------------------------------------------------------
// G1.3 — shared data preserved on partial delete
// ---------------------------------------------------------------------------

#[tokio::test]
async fn shared_data_preserved_on_partial_delete() {
    let (_handle, _ctx, db) = test_task_context().await;
    let storage = Arc::new(MockStorage::new());
    let owner_id = Uuid::new_v4();

    // Create two datasets manually and attach the same data record to both.
    let ds_a = Dataset::new("dataset_A".to_string(), owner_id, None, Uuid::new_v4());
    let ds_b = Dataset::new("dataset_B".to_string(), owner_id, None, Uuid::new_v4());
    let ds_a_id = ds_a.id;
    let ds_b_id = ds_b.id;
    ops::datasets::create_dataset(&db, ds_a).await.unwrap();
    ops::datasets::create_dataset(&db, ds_b).await.unwrap();

    let location = storage
        .store(b"shared content", "shared.txt")
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
        "shared_hash",
        owner_id,
    )
    .build();
    ops::data::create_data(&db, data).await.unwrap();
    ops::datasets::attach_data_to_dataset(&db, ds_a_id, data_id)
        .await
        .unwrap();
    ops::datasets::attach_data_to_dataset(&db, ds_b_id, data_id)
        .await
        .unwrap();

    // Delete dataset A only.
    let svc = DeleteService::new(
        storage.clone() as Arc<dyn StorageTrait>,
        db.clone() as Arc<dyn DeleteDb>,
    );
    let result = svc
        .execute(&DeleteRequest {
            scope: DeleteScope::Dataset {
                owner_id,
                dataset_name: "dataset_A".to_string(),
            },
            mode: DeleteMode::Soft,
        })
        .await
        .expect("delete should succeed");

    assert_eq!(result.deleted_datasets, 1);
    assert_eq!(
        result.deleted_data, 0,
        "data must NOT be deleted while still linked to dataset_B"
    );

    // The Data record should still exist.
    let still_exists = ops::data::get_data(&db, data_id).await.unwrap();
    assert!(
        still_exists.is_some(),
        "data record should survive because dataset_B still references it"
    );

    // The storage file should still exist.
    assert!(
        storage.exists(&location).await.unwrap(),
        "storage file should survive because data is still referenced"
    );
}

// ---------------------------------------------------------------------------
// FP2.1 — delete_dataset_if_empty=false preserves dataset
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_data_with_dataset_if_empty_false_preserves_dataset() {
    let (_handle, _ctx, db) = test_task_context().await;
    let storage = Arc::new(MockStorage::new());
    let owner_id = Uuid::new_v4();

    let (_dataset_id, data_id, _location) =
        seed_dataset_with_data(&db, &storage, owner_id, "keep_ds").await;

    // Delete the data item with delete_dataset_if_empty = false.
    let svc = DeleteService::new(
        storage.clone() as Arc<dyn StorageTrait>,
        db.clone() as Arc<dyn DeleteDb>,
    );
    let result = svc
        .execute(&DeleteRequest {
            scope: DeleteScope::Data {
                owner_id,
                data_id,
                dataset_name: Some("keep_ds".to_string()),
                delete_dataset_if_empty: false,
            },
            mode: DeleteMode::Soft,
        })
        .await
        .expect("delete should succeed");

    // Data should be deleted.
    assert_eq!(result.deleted_data, 1);

    // The data record should be gone.
    let gone = ops::data::get_data(&db, data_id).await.unwrap();
    assert!(gone.is_none(), "data record should be gone after deletion");

    // Dataset should still exist because delete_dataset_if_empty was false.
    assert_eq!(
        result.deleted_datasets, 0,
        "dataset should NOT be deleted when delete_dataset_if_empty is false"
    );
    let ds = ops::datasets::get_dataset_by_name(&db, "keep_ds", owner_id, None)
        .await
        .unwrap();
    assert!(
        ds.is_some(),
        "dataset should survive when delete_dataset_if_empty is false"
    );

    // Dataset should have zero data items.
    let remaining = ops::datasets::get_dataset_data(&db, _dataset_id)
        .await
        .unwrap();
    assert_eq!(
        remaining.len(),
        0,
        "dataset should have zero data items after data deletion"
    );
}

// ---------------------------------------------------------------------------
// FP2.2 — delete_dataset_if_empty=true removes dataset
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_data_with_dataset_if_empty_true_removes_dataset() {
    let (_handle, _ctx, db) = test_task_context().await;
    let storage = Arc::new(MockStorage::new());
    let owner_id = Uuid::new_v4();

    let (_dataset_id, data_id, _location) =
        seed_dataset_with_data(&db, &storage, owner_id, "remove_ds").await;

    // Delete the data item with delete_dataset_if_empty = true.
    let svc = DeleteService::new(
        storage.clone() as Arc<dyn StorageTrait>,
        db.clone() as Arc<dyn DeleteDb>,
    );
    let result = svc
        .execute(&DeleteRequest {
            scope: DeleteScope::Data {
                owner_id,
                data_id,
                dataset_name: Some("remove_ds".to_string()),
                delete_dataset_if_empty: true,
            },
            mode: DeleteMode::Soft,
        })
        .await
        .expect("delete should succeed");

    // Data should be deleted.
    assert_eq!(result.deleted_data, 1);

    // The data record should be gone.
    let gone = ops::data::get_data(&db, data_id).await.unwrap();
    assert!(gone.is_none(), "data record should be gone after deletion");

    // Dataset should also be deleted because it became empty.
    assert!(
        result.deleted_datasets >= 1,
        "dataset should be deleted when delete_dataset_if_empty is true and dataset is empty"
    );
    let ds = ops::datasets::get_dataset_by_name(&db, "remove_ds", owner_id, None)
        .await
        .unwrap();
    assert!(
        ds.is_none(),
        "dataset should be gone when delete_dataset_if_empty is true"
    );
}

// ---------------------------------------------------------------------------
// FP2.3 — delete_dataset_if_empty=true preserves non-empty dataset
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_data_with_dataset_if_empty_true_preserves_non_empty_dataset() {
    let (_handle, _ctx, db) = test_task_context().await;
    let storage = Arc::new(MockStorage::new());
    let owner_id = Uuid::new_v4();

    // Seed one dataset with one data item via the helper.
    let (dataset_id, data_id_1, _location_1) =
        seed_dataset_with_data(&db, &storage, owner_id, "two_item_ds").await;

    // Add a second data item to the same dataset manually.
    let location_2 = storage
        .store(b"second content", "second.txt")
        .await
        .unwrap();
    let data_id_2 = Uuid::new_v4();
    let data_2 = Data::builder(
        data_id_2,
        "second.txt",
        &location_2,
        "file://second.txt",
        "txt",
        "text/plain",
        "hash_second",
        owner_id,
    )
    .build();
    ops::data::create_data(&db, data_2).await.unwrap();
    ops::datasets::attach_data_to_dataset(&db, dataset_id, data_id_2)
        .await
        .unwrap();

    // Verify dataset has two data items before deletion.
    let before = ops::datasets::get_dataset_data(&db, dataset_id)
        .await
        .unwrap();
    assert_eq!(
        before.len(),
        2,
        "dataset should have two data items before deletion"
    );

    // Delete only the first data item with delete_dataset_if_empty = true.
    let svc = DeleteService::new(
        storage.clone() as Arc<dyn StorageTrait>,
        db.clone() as Arc<dyn DeleteDb>,
    );
    let result = svc
        .execute(&DeleteRequest {
            scope: DeleteScope::Data {
                owner_id,
                data_id: data_id_1,
                dataset_name: Some("two_item_ds".to_string()),
                delete_dataset_if_empty: true,
            },
            mode: DeleteMode::Soft,
        })
        .await
        .expect("delete should succeed");

    // First data item should be deleted.
    assert_eq!(result.deleted_data, 1);
    let gone = ops::data::get_data(&db, data_id_1).await.unwrap();
    assert!(gone.is_none(), "first data record should be gone");

    // Dataset should survive because the second data item is still present.
    assert_eq!(
        result.deleted_datasets, 0,
        "dataset should NOT be deleted because it still has data"
    );
    let ds = ops::datasets::get_dataset_by_name(&db, "two_item_ds", owner_id, None)
        .await
        .unwrap();
    assert!(
        ds.is_some(),
        "dataset should survive because it still has one data item"
    );

    // Remaining data count should be 1.
    let remaining = ops::datasets::get_dataset_data(&db, dataset_id)
        .await
        .unwrap();
    assert_eq!(
        remaining.len(),
        1,
        "dataset should have exactly one remaining data item"
    );

    // The remaining item should be the second one.
    assert_eq!(
        remaining[0].id, data_id_2,
        "remaining data should be the second item"
    );
}

// ---------------------------------------------------------------------------
// forget() API — resolve dataset by UUID
// ---------------------------------------------------------------------------

#[tokio::test]
async fn forget_by_dataset_uuid_succeeds() {
    let (_handle, _ctx, db) = test_task_context().await;
    let storage = Arc::new(MockStorage::new());
    let owner_id = Uuid::new_v4();

    let (dataset_id, data_id, _location) =
        seed_dataset_with_data(&db, &storage, owner_id, "uuid_ds").await;

    // Build a DeleteService.
    let svc = DeleteService::new(
        storage.clone() as Arc<dyn StorageTrait>,
        db.clone() as Arc<dyn DeleteDb>,
    );

    // Call the forget() API with DatasetRef::Id(dataset_id).
    let db_ref: Arc<dyn IngestDb> = db.clone();
    let result = forget(
        ForgetTarget::Dataset {
            dataset: DatasetRef::Id(dataset_id),
        },
        owner_id,
        &svc,
        Some(db_ref.as_ref()),
    )
    .await
    .expect("forget should succeed with DatasetRef::Id");

    assert_eq!(result.delete_result.deleted_datasets, 1);
    assert_eq!(result.delete_result.deleted_data, 1);
    assert!(
        result.target.starts_with("dataset:"),
        "label must include resolved name, got {:?}",
        result.target
    );

    // Data record should be gone.
    let gone = ops::data::get_data(&db, data_id).await.unwrap();
    assert!(gone.is_none(), "data record should be removed");
}

#[tokio::test]
async fn forget_by_dataset_uuid_missing_returns_err() {
    let (_handle, _ctx, db) = test_task_context().await;
    let storage = Arc::new(MockStorage::new());
    let owner_id = Uuid::new_v4();

    // Random UUID that does not correspond to any dataset.
    let missing_id = Uuid::new_v4();

    let svc = DeleteService::new(
        storage.clone() as Arc<dyn StorageTrait>,
        db.clone() as Arc<dyn DeleteDb>,
    );

    let db_ref: Arc<dyn IngestDb> = db.clone();
    let result = forget(
        ForgetTarget::Dataset {
            dataset: DatasetRef::Id(missing_id),
        },
        owner_id,
        &svc,
        Some(db_ref.as_ref()),
    )
    .await;

    match result {
        Err(ApiError::InvalidArgument(msg)) => {
            assert!(
                msg.contains(&missing_id.to_string()),
                "error message should reference the missing UUID, got: {msg}"
            );
        }
        Ok(_) => panic!("forget should have returned InvalidArgument"),
        Err(other) => panic!("expected InvalidArgument, got {other:?}"),
    }
}
