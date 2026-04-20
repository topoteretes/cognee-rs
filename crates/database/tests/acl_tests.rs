//! Integration tests for the ACL subsystem (principals, permissions, acls tables).

use cognee_database::{AclDb, connect, initialize, ops};
use cognee_models::Dataset;
use uuid::Uuid;

async fn setup_db() -> cognee_database::DatabaseConnection {
    let db = connect("sqlite::memory:").await.unwrap();
    initialize(&db).await.unwrap();
    db
}

/// Create a dataset directly in the DB and return its ID.
async fn create_dataset(
    db: &cognee_database::DatabaseConnection,
    name: &str,
    owner_id: Uuid,
) -> Uuid {
    let dataset = Dataset::new(name.to_string(), owner_id, None, Uuid::new_v4());
    let id = dataset.id;
    ops::datasets::create_dataset(db, dataset).await.unwrap();
    id
}

#[tokio::test]
async fn test_acl_migration_seeds_permissions() {
    let db = setup_db().await;

    // The migration should have seeded 4 permission rows.
    // Verify each one exists by trying to grant with it.
    for perm_name in &["read", "write", "delete", "share"] {
        let principal_id = Uuid::new_v4();
        let dataset_id = create_dataset(&db, &format!("ds_{perm_name}"), principal_id).await;
        ops::acl::ensure_principal(&db, principal_id, "user")
            .await
            .unwrap();
        ops::acl::grant_permission(&db, principal_id, dataset_id, perm_name)
            .await
            .unwrap_or_else(|e| {
                panic!(
                    "grant_permission should succeed for seeded permission '{}': {e}",
                    perm_name
                )
            });
    }
}

#[tokio::test]
async fn test_grant_and_check_permission() {
    let db = setup_db().await;
    let principal_id = Uuid::new_v4();
    let dataset_id = create_dataset(&db, "check_ds", principal_id).await;

    ops::acl::ensure_principal(&db, principal_id, "user")
        .await
        .unwrap();
    ops::acl::grant_permission(&db, principal_id, dataset_id, "delete")
        .await
        .unwrap();

    let has_perm = db
        .has_permission(principal_id, dataset_id, "delete")
        .await
        .unwrap();
    assert!(has_perm, "should have 'delete' permission after grant");

    // Should NOT have 'write' permission (not granted)
    let has_write = db
        .has_permission(principal_id, dataset_id, "write")
        .await
        .unwrap();
    assert!(!has_write, "should not have 'write' without explicit grant");
}

#[tokio::test]
async fn test_has_permission_returns_false_without_grant() {
    let db = setup_db().await;
    let principal_id = Uuid::new_v4();
    let dataset_id = create_dataset(&db, "no_grant_ds", principal_id).await;

    let has_perm = db
        .has_permission(principal_id, dataset_id, "delete")
        .await
        .unwrap();
    assert!(
        !has_perm,
        "should not have permission without any ACL entries"
    );
}

#[tokio::test]
async fn test_grant_is_idempotent() {
    let db = setup_db().await;
    let principal_id = Uuid::new_v4();
    let dataset_id = create_dataset(&db, "idempotent_ds", principal_id).await;

    ops::acl::ensure_principal(&db, principal_id, "user")
        .await
        .unwrap();

    // Grant twice — should not error
    ops::acl::grant_permission(&db, principal_id, dataset_id, "read")
        .await
        .unwrap();
    ops::acl::grant_permission(&db, principal_id, dataset_id, "read")
        .await
        .unwrap();

    // Should still have exactly one grant
    let authorized = db
        .authorized_dataset_ids(principal_id, "read")
        .await
        .unwrap();
    assert_eq!(
        authorized.len(),
        1,
        "idempotent grant should not create duplicates"
    );
}

#[tokio::test]
async fn test_authorized_dataset_ids() {
    let db = setup_db().await;
    let principal_id = Uuid::new_v4();
    let ds_a = create_dataset(&db, "auth_ds_a", principal_id).await;
    let ds_b = create_dataset(&db, "auth_ds_b", principal_id).await;
    let ds_c = create_dataset(&db, "auth_ds_c", principal_id).await;

    ops::acl::ensure_principal(&db, principal_id, "user")
        .await
        .unwrap();
    ops::acl::grant_permission(&db, principal_id, ds_a, "delete")
        .await
        .unwrap();
    ops::acl::grant_permission(&db, principal_id, ds_b, "delete")
        .await
        .unwrap();
    // ds_c gets only 'read', not 'delete'
    ops::acl::grant_permission(&db, principal_id, ds_c, "read")
        .await
        .unwrap();

    let mut authorized = db
        .authorized_dataset_ids(principal_id, "delete")
        .await
        .unwrap();
    authorized.sort();

    let mut expected = vec![ds_a, ds_b];
    expected.sort();

    assert_eq!(
        authorized, expected,
        "should return exactly the datasets with 'delete' permission"
    );
}

#[tokio::test]
async fn test_revoke_permission() {
    let db = setup_db().await;
    let principal_id = Uuid::new_v4();
    let dataset_id = create_dataset(&db, "revoke_ds", principal_id).await;

    ops::acl::ensure_principal(&db, principal_id, "user")
        .await
        .unwrap();
    ops::acl::grant_permission(&db, principal_id, dataset_id, "delete")
        .await
        .unwrap();

    assert!(
        db.has_permission(principal_id, dataset_id, "delete")
            .await
            .unwrap()
    );

    ops::acl::revoke_permission(&db, principal_id, dataset_id, "delete")
        .await
        .unwrap();

    assert!(
        !db.has_permission(principal_id, dataset_id, "delete")
            .await
            .unwrap(),
        "permission should be revoked"
    );
}

#[tokio::test]
async fn test_ensure_principal_is_idempotent() {
    let db = setup_db().await;
    let principal_id = Uuid::new_v4();

    ops::acl::ensure_principal(&db, principal_id, "user")
        .await
        .unwrap();
    ops::acl::ensure_principal(&db, principal_id, "user")
        .await
        .unwrap();

    // No error = idempotent.
}

#[tokio::test]
async fn test_grant_all_permissions_on_dataset() {
    let db = setup_db().await;
    let principal_id = Uuid::new_v4();
    let dataset_id = create_dataset(&db, "all_perms_ds", principal_id).await;

    ops::acl::grant_all_permissions_on_dataset(&db, principal_id, dataset_id)
        .await
        .unwrap();

    for perm in &["read", "write", "delete", "share"] {
        assert!(
            db.has_permission(principal_id, dataset_id, perm)
                .await
                .unwrap(),
            "should have '{}' permission after grant_all",
            perm
        );
    }
}

#[tokio::test]
async fn test_dataset_deletion_cascades_acls() {
    let db = setup_db().await;
    let principal_id = Uuid::new_v4();
    let dataset_id = create_dataset(&db, "cascade_acl_ds", principal_id).await;

    ops::acl::grant_all_permissions_on_dataset(&db, principal_id, dataset_id)
        .await
        .unwrap();

    // Verify permissions exist
    assert!(
        db.has_permission(principal_id, dataset_id, "delete")
            .await
            .unwrap()
    );

    // Delete the dataset
    ops::datasets::delete_dataset(&db, dataset_id)
        .await
        .unwrap();

    // ACL entries should be gone (FK CASCADE)
    assert!(
        !db.has_permission(principal_id, dataset_id, "delete")
            .await
            .unwrap(),
        "ACL should be cascade-deleted with the dataset"
    );
}
