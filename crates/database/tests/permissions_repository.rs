//! Integration tests for [`SeaOrmPermissionsRepository`].
//!
//! Covers the 8-step `user_can` resolution per `tenants.md §5.1` plus the
//! repository CRUD surface from `tenants.md §9`.

#![cfg(feature = "sqlite")]

use std::sync::Arc;

use chrono::Utc;
use cognee_database::permissions::{
    PermissionsError, PermissionsRepository, SeaOrmPermissionsRepository,
};
use cognee_database::{DatabaseConnection, connect, initialize};
use sea_orm::Set;
use sea_orm::prelude::*;
use uuid::Uuid;

async fn fresh_db() -> Arc<DatabaseConnection> {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("migrate");
    Arc::new(db)
}

fn to_hex(u: Uuid) -> String {
    u.simple().to_string()
}

async fn seed_user(db: &DatabaseConnection, user_id: Uuid, is_superuser: bool) {
    use cognee_database::entities::{principal, user};
    let now = Utc::now();
    let hex = to_hex(user_id);
    let _ = principal::Entity::insert(principal::ActiveModel {
        id: Set(hex.clone()),
        principal_type: Set("user".into()),
        created_at: Set(now),
        updated_at: Set(None),
    })
    .exec(db)
    .await;
    let _ = user::Entity::insert(user::ActiveModel {
        id: Set(hex),
        email: Set(format!("u-{user_id}@example.com")),
        hashed_password: Set("".into()),
        is_active: Set(true),
        is_superuser: Set(is_superuser),
        is_verified: Set(true),
        tenant_id: Set(None),
        created_at: Set(now),
        updated_at: Set(None),
    })
    .exec(db)
    .await;
}

async fn seed_tenant_with_owner(db: &DatabaseConnection, tenant_id: Uuid, owner_id: Uuid) {
    use cognee_database::entities::{principal, tenant};
    let now = Utc::now();
    let hex = to_hex(tenant_id);
    let _ = principal::Entity::insert(principal::ActiveModel {
        id: Set(hex.clone()),
        principal_type: Set("tenant".into()),
        created_at: Set(now),
        updated_at: Set(None),
    })
    .exec(db)
    .await;
    let _ = tenant::Entity::insert(tenant::ActiveModel {
        id: Set(hex),
        name: Set(format!("tenant-{tenant_id}")),
        owner_id: Set(to_hex(owner_id)),
        created_at: Set(now),
        updated_at: Set(None),
    })
    .exec(db)
    .await;
}

async fn seed_dataset(
    db: &DatabaseConnection,
    dataset_id: Uuid,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
) {
    use cognee_database::entities::dataset;
    let now = Utc::now();
    dataset::Entity::insert(dataset::ActiveModel {
        id: Set(to_hex(dataset_id)),
        name: Set(format!("ds-{dataset_id}")),
        owner_id: Set(to_hex(owner_id)),
        tenant_id: Set(tenant_id.map(to_hex)),
        created_at: Set(now),
        updated_at: Set(None),
    })
    .exec(db)
    .await
    .unwrap();
}

async fn user_tenants(db: &DatabaseConnection, user_id: Uuid, tenant_id: Uuid) {
    use cognee_database::entities::user_tenant;
    user_tenant::Entity::insert(user_tenant::ActiveModel {
        user_id: Set(to_hex(user_id)),
        tenant_id: Set(to_hex(tenant_id)),
        created_at: Set(Utc::now()),
    })
    .exec(db)
    .await
    .unwrap();
}

async fn perm_id(db: &DatabaseConnection, name: &str) -> String {
    use cognee_database::entities::permission;
    permission::Entity::find()
        .filter(permission::Column::Name.eq(name))
        .one(db)
        .await
        .unwrap()
        .expect("permission seed missing")
        .id
}

async fn grant_acl_raw(db: &DatabaseConnection, principal_id: Uuid, dataset_id: Uuid, perm: &str) {
    use cognee_database::entities::acl;
    let now = Utc::now();
    let pid = perm_id(db, perm).await;
    acl::Entity::insert(acl::ActiveModel {
        id: Set(to_hex(Uuid::new_v4())),
        principal_id: Set(to_hex(principal_id)),
        permission_id: Set(pid),
        dataset_id: Set(to_hex(dataset_id)),
        created_at: Set(now),
        updated_at: Set(None),
    })
    .exec(db)
    .await
    .unwrap();
}

// ── Step 1: superuser ───────────────────────────────────────────────────────
#[tokio::test]
async fn user_can_step1_superuser() {
    let db = fresh_db().await;
    let repo = SeaOrmPermissionsRepository::new(db.clone());
    let uid = Uuid::new_v4();
    let did = Uuid::new_v4();
    seed_user(&db, uid, true).await;
    seed_tenant_with_owner(&db, Uuid::new_v4(), Uuid::new_v4()).await;
    seed_dataset(&db, did, Uuid::new_v4(), None).await;
    assert!(repo.user_can(uid, did, "read").await.unwrap());
}

// ── Step 2: direct user ACL ─────────────────────────────────────────────────
#[tokio::test]
async fn user_can_step2_direct_user_acl() {
    let db = fresh_db().await;
    let repo = SeaOrmPermissionsRepository::new(db.clone());
    let uid = Uuid::new_v4();
    let did = Uuid::new_v4();
    seed_user(&db, uid, false).await;
    seed_dataset(&db, did, Uuid::new_v4(), None).await;
    grant_acl_raw(&db, uid, did, "read").await;
    assert!(repo.user_can(uid, did, "read").await.unwrap());
    // Different perm: deny.
    assert!(!repo.user_can(uid, did, "write").await.unwrap());
}

// ── Step 3: user_default_permissions ───────────────────────────────────────
#[tokio::test]
async fn user_can_step3_user_default() {
    use cognee_database::entities::user_default_permission;
    let db = fresh_db().await;
    let repo = SeaOrmPermissionsRepository::new(db.clone());
    let uid = Uuid::new_v4();
    let did = Uuid::new_v4();
    let tid = Uuid::new_v4();
    let owner = Uuid::new_v4();
    seed_user(&db, uid, false).await;
    seed_tenant_with_owner(&db, tid, owner).await;
    seed_dataset(&db, did, owner, Some(tid)).await;
    user_tenants(&db, uid, tid).await; // user is a member of dataset's tenant.
    let pid = perm_id(&db, "read").await;
    user_default_permission::Entity::insert(user_default_permission::ActiveModel {
        user_id: Set(to_hex(uid)),
        permission_id: Set(pid),
        created_at: Set(Utc::now()),
    })
    .exec(db.as_ref())
    .await
    .unwrap();
    assert!(repo.user_can(uid, did, "read").await.unwrap());
}

// ── Step 4: tenant ACL ──────────────────────────────────────────────────────
#[tokio::test]
async fn user_can_step4_tenant_acl() {
    let db = fresh_db().await;
    let repo = SeaOrmPermissionsRepository::new(db.clone());
    let uid = Uuid::new_v4();
    let tid = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let did = Uuid::new_v4();
    seed_user(&db, uid, false).await;
    seed_tenant_with_owner(&db, tid, owner).await;
    seed_dataset(&db, did, owner, Some(tid)).await;
    user_tenants(&db, uid, tid).await;
    grant_acl_raw(&db, tid, did, "read").await;
    assert!(repo.user_can(uid, did, "read").await.unwrap());
}

// ── Step 5: tenant_default_permissions ─────────────────────────────────────
#[tokio::test]
async fn user_can_step5_tenant_default() {
    use cognee_database::entities::tenant_default_permission;
    let db = fresh_db().await;
    let repo = SeaOrmPermissionsRepository::new(db.clone());
    let uid = Uuid::new_v4();
    let tid = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let did = Uuid::new_v4();
    seed_user(&db, uid, false).await;
    seed_tenant_with_owner(&db, tid, owner).await;
    seed_dataset(&db, did, owner, Some(tid)).await;
    user_tenants(&db, uid, tid).await;
    let pid = perm_id(&db, "read").await;
    tenant_default_permission::Entity::insert(tenant_default_permission::ActiveModel {
        tenant_id: Set(to_hex(tid)),
        permission_id: Set(pid),
        created_at: Set(Utc::now()),
    })
    .exec(db.as_ref())
    .await
    .unwrap();
    assert!(repo.user_can(uid, did, "read").await.unwrap());
}

// ── Step 6: role ACL ────────────────────────────────────────────────────────
#[tokio::test]
async fn user_can_step6_role_acl() {
    use cognee_database::entities::{principal, role, user_role};
    let db = fresh_db().await;
    let repo = SeaOrmPermissionsRepository::new(db.clone());
    let uid = Uuid::new_v4();
    let tid = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let did = Uuid::new_v4();
    let rid = Uuid::new_v4();
    seed_user(&db, uid, false).await;
    seed_tenant_with_owner(&db, tid, owner).await;
    seed_dataset(&db, did, owner, Some(tid)).await;

    let now = Utc::now();
    principal::Entity::insert(principal::ActiveModel {
        id: Set(to_hex(rid)),
        principal_type: Set("role".into()),
        created_at: Set(now),
        updated_at: Set(None),
    })
    .exec(db.as_ref())
    .await
    .unwrap();
    role::Entity::insert(role::ActiveModel {
        id: Set(to_hex(rid)),
        name: Set("editor".into()),
        tenant_id: Set(to_hex(tid)),
        created_at: Set(now),
        updated_at: Set(None),
    })
    .exec(db.as_ref())
    .await
    .unwrap();
    user_role::Entity::insert(user_role::ActiveModel {
        user_id: Set(to_hex(uid)),
        role_id: Set(to_hex(rid)),
        created_at: Set(now),
    })
    .exec(db.as_ref())
    .await
    .unwrap();

    grant_acl_raw(&db, rid, did, "read").await;
    assert!(repo.user_can(uid, did, "read").await.unwrap());
}

// ── Step 7: role_default_permissions ───────────────────────────────────────
#[tokio::test]
async fn user_can_step7_role_default() {
    use cognee_database::entities::{principal, role, role_default_permission, user_role};
    let db = fresh_db().await;
    let repo = SeaOrmPermissionsRepository::new(db.clone());
    let uid = Uuid::new_v4();
    let tid = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let did = Uuid::new_v4();
    let rid = Uuid::new_v4();
    seed_user(&db, uid, false).await;
    seed_tenant_with_owner(&db, tid, owner).await;
    seed_dataset(&db, did, owner, Some(tid)).await;
    let now = Utc::now();
    principal::Entity::insert(principal::ActiveModel {
        id: Set(to_hex(rid)),
        principal_type: Set("role".into()),
        created_at: Set(now),
        updated_at: Set(None),
    })
    .exec(db.as_ref())
    .await
    .unwrap();
    role::Entity::insert(role::ActiveModel {
        id: Set(to_hex(rid)),
        name: Set("editor".into()),
        tenant_id: Set(to_hex(tid)),
        created_at: Set(now),
        updated_at: Set(None),
    })
    .exec(db.as_ref())
    .await
    .unwrap();
    user_role::Entity::insert(user_role::ActiveModel {
        user_id: Set(to_hex(uid)),
        role_id: Set(to_hex(rid)),
        created_at: Set(now),
    })
    .exec(db.as_ref())
    .await
    .unwrap();
    let pid = perm_id(&db, "read").await;
    role_default_permission::Entity::insert(role_default_permission::ActiveModel {
        role_id: Set(to_hex(rid)),
        permission_id: Set(pid),
        created_at: Set(now),
    })
    .exec(db.as_ref())
    .await
    .unwrap();
    assert!(repo.user_can(uid, did, "read").await.unwrap());
}

// ── Step 8: ownership ───────────────────────────────────────────────────────
#[tokio::test]
async fn user_can_step8_ownership() {
    let db = fresh_db().await;
    let repo = SeaOrmPermissionsRepository::new(db.clone());
    let uid = Uuid::new_v4();
    let did = Uuid::new_v4();
    seed_user(&db, uid, false).await;
    seed_dataset(&db, did, uid, None).await;
    assert!(repo.user_can(uid, did, "read").await.unwrap());
    assert!(repo.user_can(uid, did, "delete").await.unwrap());
}

// ── Deny when no path matches ──────────────────────────────────────────────
#[tokio::test]
async fn user_can_deny_default() {
    let db = fresh_db().await;
    let repo = SeaOrmPermissionsRepository::new(db.clone());
    let uid = Uuid::new_v4();
    let did = Uuid::new_v4();
    seed_user(&db, uid, false).await;
    seed_dataset(&db, did, Uuid::new_v4(), None).await;
    assert!(!repo.user_can(uid, did, "read").await.unwrap());
}

// ── grant + revoke ACL round-trip ──────────────────────────────────────────
#[tokio::test]
async fn grant_and_revoke_acl() {
    let db = fresh_db().await;
    let repo = SeaOrmPermissionsRepository::new(db.clone());
    let uid = Uuid::new_v4();
    let did = Uuid::new_v4();
    seed_user(&db, uid, false).await;
    seed_dataset(&db, did, Uuid::new_v4(), None).await;
    repo.grant_acl(uid, did, "read").await.unwrap();
    assert!(repo.user_can(uid, did, "read").await.unwrap());
    // Idempotent.
    repo.grant_acl(uid, did, "read").await.unwrap();
    repo.revoke_acl(uid, did, "read").await.unwrap();
    assert!(!repo.user_can(uid, did, "read").await.unwrap());
}

// ── create_role + duplicate rejection ──────────────────────────────────────
#[tokio::test]
async fn create_role_and_reject_dup() {
    let db = fresh_db().await;
    let repo = SeaOrmPermissionsRepository::new(db.clone());
    let owner = Uuid::new_v4();
    let tid = Uuid::new_v4();
    seed_user(&db, owner, false).await;
    seed_tenant_with_owner(&db, tid, owner).await;
    let _ = repo.create_role(tid, "admin").await.unwrap();
    let dup = repo.create_role(tid, "admin").await;
    assert!(matches!(dup, Err(PermissionsError::EntityAlreadyExists(_))));
}

// ── create_tenant adds membership + sets current ──────────────────────────
#[tokio::test]
async fn create_tenant_membership_and_current() {
    let db = fresh_db().await;
    let repo = SeaOrmPermissionsRepository::new(db.clone());
    let owner = Uuid::new_v4();
    seed_user(&db, owner, false).await;
    let tid = repo.create_tenant("acme", owner).await.unwrap();
    // Caller's current tenant is tid.
    assert_eq!(repo.current_tenant(owner).await.unwrap(), Some(tid));
    // Caller is a member.
    let mine = repo.list_my_tenants(owner).await.unwrap();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].id, tid);
}

// ── select_current_tenant cross-tenant guard ───────────────────────────────
#[tokio::test]
async fn select_tenant_rejects_non_member() {
    let db = fresh_db().await;
    let repo = SeaOrmPermissionsRepository::new(db.clone());
    let uid = Uuid::new_v4();
    let other = Uuid::new_v4();
    let tid = Uuid::new_v4();
    seed_user(&db, uid, false).await;
    seed_tenant_with_owner(&db, tid, other).await;
    let res = repo.select_current_tenant(uid, Some(tid)).await;
    assert!(matches!(res, Err(PermissionsError::EntityNotFound(_))));
}

// ── remove_user_from_tenant cleans up + rejects owner ──────────────────────
#[tokio::test]
async fn remove_user_from_tenant_owner_rejected() {
    let db = fresh_db().await;
    let repo = SeaOrmPermissionsRepository::new(db.clone());
    let owner = Uuid::new_v4();
    seed_user(&db, owner, false).await;
    let tid = repo.create_tenant("acme", owner).await.unwrap();
    let res = repo.remove_user_from_tenant(owner, tid).await;
    assert!(matches!(res, Err(PermissionsError::Validation(_))));
}

#[tokio::test]
async fn migration_idempotent_seeds_four_permissions() {
    use cognee_database::entities::permission;
    let db = fresh_db().await;
    // Second initialize should be a no-op.
    initialize(db.as_ref()).await.unwrap();
    let count = permission::Entity::find().count(db.as_ref()).await.unwrap();
    assert_eq!(count, 4, "expected exactly 4 seeded permissions");
}
