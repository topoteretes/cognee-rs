//! Integration tests for `AuthorizedDeleteService` ACL enforcement.
//!
//! These tests verify that the ACL wrapper around `DeleteService` correctly
//! denies or allows operations based on the principal's permissions.

use std::sync::Arc;

use cognee_database::{self as database, AclDb, DatabaseConnection, DeleteDb};
use cognee_delete::{
    AuthorizedDeleteService, DeleteError, DeleteMode, DeleteRequest, DeleteScope, DeleteService,
};
use cognee_models::{Data, Dataset};
use cognee_storage::{MockStorage, StorageTrait};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create an in-memory SQLite database, run migrations, and return it
/// alongside a `MockStorage`.
async fn setup() -> (Arc<DatabaseConnection>, Arc<MockStorage>) {
    let db = database::connect("sqlite::memory:").await.unwrap();
    database::initialize(&db).await.unwrap();
    let storage = Arc::new(MockStorage::new());
    (Arc::new(db), storage)
}

/// Seed one dataset + one data item into the database, attach them together,
/// and write a small file into mock storage. Returns `(dataset_id, data_id)`.
async fn seed_dataset_with_data(
    db: &DatabaseConnection,
    storage: &MockStorage,
    owner_id: Uuid,
    dataset_name: &str,
) -> (Uuid, Uuid) {
    let dataset = Dataset::new(dataset_name.to_string(), owner_id, None, Uuid::new_v4());
    let dataset_id = dataset.id;
    database::ops::datasets::create_dataset(db, dataset)
        .await
        .unwrap();

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
    database::ops::data::create_data(db, data).await.unwrap();
    database::ops::datasets::attach_data_to_dataset(db, dataset_id, data_id)
        .await
        .unwrap();

    (dataset_id, data_id)
}

/// Grant `"delete"` permission on `dataset_id` to `principal_id`.
async fn grant_delete_permission(db: &DatabaseConnection, principal_id: Uuid, dataset_id: Uuid) {
    let acl: &dyn AclDb = db;
    acl.ensure_principal(principal_id, "user").await.unwrap();
    acl.grant_permission(principal_id, dataset_id, "delete")
        .await
        .unwrap();
}

/// Build an `AuthorizedDeleteService` from the shared database and storage.
fn build_authorized_service(
    db: &Arc<DatabaseConnection>,
    storage: &Arc<MockStorage>,
) -> AuthorizedDeleteService {
    let inner = DeleteService::new(
        storage.clone() as Arc<dyn cognee_storage::StorageTrait>,
        db.clone() as Arc<dyn DeleteDb>,
    );
    AuthorizedDeleteService::new(
        inner,
        db.clone() as Arc<dyn AclDb>,
        db.clone() as Arc<dyn DeleteDb>,
    )
}

// ---------------------------------------------------------------------------
// Test 1: ACL denied returns PermissionDenied
// ---------------------------------------------------------------------------

#[tokio::test]
async fn acl_denied_returns_permission_denied() {
    let (db, storage) = setup().await;
    let owner_id = Uuid::new_v4();

    let (dataset_id, data_id) =
        seed_dataset_with_data(&db, &storage, owner_id, "acl_denied_ds").await;

    let svc = build_authorized_service(&db, &storage);

    // The principal (owner_id) has NOT been granted "delete" permission.
    let result = svc
        .execute(
            &DeleteRequest {
                scope: DeleteScope::Data {
                    owner_id,
                    data_id,
                    dataset_name: Some("acl_denied_ds".to_string()),
                    delete_dataset_if_empty: false,
                },
                mode: DeleteMode::Soft,
            },
            owner_id,
        )
        .await;

    assert!(result.is_err(), "execute should fail without ACL grant");
    let err = result.unwrap_err();
    assert!(
        matches!(err, DeleteError::PermissionDenied(_)),
        "expected PermissionDenied, got: {err:?}"
    );

    // Verify data still exists (nothing was deleted).
    let data = database::ops::data::get_data(&db, data_id).await.unwrap();
    assert!(
        data.is_some(),
        "data should still exist after denied delete"
    );

    let ds = database::ops::datasets::get_dataset(&db, dataset_id)
        .await
        .unwrap();
    assert!(
        ds.is_some(),
        "dataset should still exist after denied delete"
    );
}

// ---------------------------------------------------------------------------
// Test 2: ACL granted allows deletion
// ---------------------------------------------------------------------------

#[tokio::test]
async fn acl_granted_allows_deletion() {
    let (db, storage) = setup().await;
    let owner_id = Uuid::new_v4();

    let (dataset_id, data_id) =
        seed_dataset_with_data(&db, &storage, owner_id, "acl_granted_ds").await;

    // Grant delete permission.
    grant_delete_permission(&db, owner_id, dataset_id).await;

    let svc = build_authorized_service(&db, &storage);

    let result = svc
        .execute(
            &DeleteRequest {
                scope: DeleteScope::Data {
                    owner_id,
                    data_id,
                    dataset_name: Some("acl_granted_ds".to_string()),
                    delete_dataset_if_empty: false,
                },
                mode: DeleteMode::Soft,
            },
            owner_id,
        )
        .await;

    assert!(
        result.is_ok(),
        "execute should succeed with ACL grant: {result:?}"
    );
    let result = result.unwrap();
    assert!(
        result.deleted_data >= 1,
        "should have deleted at least 1 data record, got: {}",
        result.deleted_data
    );

    // Verify data is gone.
    let data = database::ops::data::get_data(&db, data_id).await.unwrap();
    assert!(
        data.is_none(),
        "data should be deleted after authorized delete"
    );
}

// ---------------------------------------------------------------------------
// Test 3: preview() respects ACL
// ---------------------------------------------------------------------------

#[tokio::test]
async fn preview_respects_acl() {
    let (db, storage) = setup().await;
    let owner_id = Uuid::new_v4();

    let (_dataset_id, data_id) =
        seed_dataset_with_data(&db, &storage, owner_id, "preview_acl_ds").await;

    let svc = build_authorized_service(&db, &storage);

    // No permission granted — preview should be denied.
    let result = svc
        .preview(
            &DeleteRequest {
                scope: DeleteScope::Data {
                    owner_id,
                    data_id,
                    dataset_name: Some("preview_acl_ds".to_string()),
                    delete_dataset_if_empty: false,
                },
                mode: DeleteMode::Soft,
            },
            owner_id,
        )
        .await;

    assert!(result.is_err(), "preview should fail without ACL grant");
    let err = result.unwrap_err();
    assert!(
        matches!(err, DeleteError::PermissionDenied(_)),
        "expected PermissionDenied, got: {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Test 4: cross-user isolation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cross_user_isolation() {
    let (db, storage) = setup().await;
    let owner_a = Uuid::new_v4();
    let owner_b = Uuid::new_v4();

    // Seed data for both owners.
    let (dataset_a_id, data_a_id) =
        seed_dataset_with_data(&db, &storage, owner_a, "user_a_ds").await;
    let (dataset_b_id, data_b_id) =
        seed_dataset_with_data(&db, &storage, owner_b, "user_b_ds").await;

    // Grant delete permission to owner_a only.
    grant_delete_permission(&db, owner_a, dataset_a_id).await;
    // owner_b gets NO permission.

    let svc = build_authorized_service(&db, &storage);

    // owner_a deletes their own data — should succeed.
    let result_a = svc
        .execute(
            &DeleteRequest {
                scope: DeleteScope::Data {
                    owner_id: owner_a,
                    data_id: data_a_id,
                    dataset_name: Some("user_a_ds".to_string()),
                    delete_dataset_if_empty: false,
                },
                mode: DeleteMode::Soft,
            },
            owner_a,
        )
        .await;

    assert!(
        result_a.is_ok(),
        "owner_a should be able to delete their data: {result_a:?}"
    );
    assert!(
        result_a.unwrap().deleted_data >= 1,
        "owner_a should have deleted at least 1 data record"
    );

    // owner_b tries to delete their own data — should fail (no permission).
    let result_b = svc
        .execute(
            &DeleteRequest {
                scope: DeleteScope::Data {
                    owner_id: owner_b,
                    data_id: data_b_id,
                    dataset_name: Some("user_b_ds".to_string()),
                    delete_dataset_if_empty: false,
                },
                mode: DeleteMode::Soft,
            },
            owner_b,
        )
        .await;

    assert!(
        result_b.is_err(),
        "owner_b should be denied without ACL grant"
    );
    let err = result_b.unwrap_err();
    assert!(
        matches!(err, DeleteError::PermissionDenied(_)),
        "expected PermissionDenied for owner_b, got: {err:?}"
    );

    // Verify owner_b's data still exists.
    let data_b = database::ops::data::get_data(&db, data_b_id).await.unwrap();
    assert!(
        data_b.is_some(),
        "owner_b's data should still exist after denied delete"
    );

    let ds_b = database::ops::datasets::get_dataset(&db, dataset_b_id)
        .await
        .unwrap();
    assert!(
        ds_b.is_some(),
        "owner_b's dataset should still exist after denied delete"
    );

    // Also verify owner_a's data is actually gone.
    let data_a = database::ops::data::get_data(&db, data_a_id).await.unwrap();
    assert!(
        data_a.is_none(),
        "owner_a's data should be gone after successful delete"
    );
}
