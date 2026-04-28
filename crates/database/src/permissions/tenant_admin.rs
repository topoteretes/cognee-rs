//! Tenant-admin helpers reused by the permissions router and the SeaORM
//! `PermissionsRepository` impl.
//!
//! Mirrors Python's `has_user_management_permission` from
//! `cognee/modules/users/permissions/methods/has_user_management_permission.py`.

use sea_orm::DatabaseConnection;
use sea_orm::prelude::*;
use uuid::Uuid;

use crate::entities::{role, tenant, user_role};
use crate::permissions::PermissionsError;
use crate::types::DatabaseError;
use crate::uuid_hex;

/// Role names that grant tenant-admin privileges (in addition to
/// `tenants.owner_id`). Matches Python's
/// `USER_MANAGEMENT_ALLOWED_ROLE_NAMES = {"admin"}`.
pub const USER_MANAGEMENT_ALLOWED_ROLE_NAMES: &[&str] = &["admin"];

/// Returns `true` when `user_id` is the tenant owner OR has any role in
/// `USER_MANAGEMENT_ALLOWED_ROLE_NAMES` for `tenant_id`.
pub async fn is_tenant_admin(
    db: &DatabaseConnection,
    user_id: Uuid,
    tenant_id: Uuid,
) -> Result<bool, PermissionsError> {
    let tenant_hex = uuid_hex::to_hex(tenant_id);
    let user_hex = uuid_hex::to_hex(user_id);

    // Owner gate.
    let tenant_row = tenant::Entity::find_by_id(tenant_hex.clone())
        .one(db)
        .await
        .map_err(|e| PermissionsError::Database(DatabaseError::QueryError(e.to_string())))?
        .ok_or_else(|| {
            PermissionsError::EntityNotFound(format!("Tenant '{tenant_id}' not found"))
        })?;

    if tenant_row.owner_id == user_hex {
        return Ok(true);
    }

    // Role gate: user holds any role in this tenant whose name is in the allow-list.
    let admin_roles = role::Entity::find()
        .filter(role::Column::TenantId.eq(tenant_hex))
        .filter(role::Column::Name.is_in(USER_MANAGEMENT_ALLOWED_ROLE_NAMES.iter().copied()))
        .all(db)
        .await
        .map_err(|e| PermissionsError::Database(DatabaseError::QueryError(e.to_string())))?;

    if admin_roles.is_empty() {
        return Ok(false);
    }

    let role_ids: Vec<String> = admin_roles.into_iter().map(|r| r.id).collect();

    let count = user_role::Entity::find()
        .filter(user_role::Column::UserId.eq(user_hex))
        .filter(user_role::Column::RoleId.is_in(role_ids))
        .count(db)
        .await
        .map_err(|e| PermissionsError::Database(DatabaseError::QueryError(e.to_string())))?;

    Ok(count > 0)
}

/// Alias for [`is_tenant_admin`], kept so reviewers can grep against the
/// Python source. Same semantics.
pub async fn has_user_management_permission(
    db: &DatabaseConnection,
    user_id: Uuid,
    tenant_id: Uuid,
) -> Result<bool, PermissionsError> {
    is_tenant_admin(db, user_id, tenant_id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::initialize;
    use chrono::Utc;
    use sea_orm::{Database, Set};

    async fn fresh_db() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("connect in-memory sqlite");
        initialize(&db).await.expect("migrate");
        db
    }

    async fn seed_tenant(
        db: &DatabaseConnection,
        owner_id: Uuid,
        tenant_id: Uuid,
    ) -> (String, String) {
        use crate::entities::{principal, tenant, user};
        let now = Utc::now();
        let owner_hex = uuid_hex::to_hex(owner_id);
        let tenant_hex = uuid_hex::to_hex(tenant_id);

        // owner principal+user
        let _ = principal::Entity::insert(principal::ActiveModel {
            id: Set(owner_hex.clone()),
            principal_type: Set("user".into()),
            created_at: Set(now),
            updated_at: Set(None),
        })
        .exec(db)
        .await;

        let _ = user::Entity::insert(user::ActiveModel {
            id: Set(owner_hex.clone()),
            email: Set(format!("u-{owner_id}@example.com")),
            hashed_password: Set("".into()),
            is_active: Set(true),
            is_superuser: Set(false),
            is_verified: Set(true),
            tenant_id: Set(Some(tenant_hex.clone())),
            created_at: Set(now),
            updated_at: Set(None),
        })
        .exec(db)
        .await;

        // tenant principal+tenant
        let _ = principal::Entity::insert(principal::ActiveModel {
            id: Set(tenant_hex.clone()),
            principal_type: Set("tenant".into()),
            created_at: Set(now),
            updated_at: Set(None),
        })
        .exec(db)
        .await;
        let _ = tenant::Entity::insert(tenant::ActiveModel {
            id: Set(tenant_hex.clone()),
            name: Set(format!("tenant-{tenant_id}")),
            owner_id: Set(owner_hex.clone()),
            created_at: Set(now),
            updated_at: Set(None),
        })
        .exec(db)
        .await;
        (owner_hex, tenant_hex)
    }

    async fn seed_user(db: &DatabaseConnection, user_id: Uuid) -> String {
        use crate::entities::{principal, user};
        let now = Utc::now();
        let hex = uuid_hex::to_hex(user_id);
        let _ = principal::Entity::insert(principal::ActiveModel {
            id: Set(hex.clone()),
            principal_type: Set("user".into()),
            created_at: Set(now),
            updated_at: Set(None),
        })
        .exec(db)
        .await;
        let _ = user::Entity::insert(user::ActiveModel {
            id: Set(hex.clone()),
            email: Set(format!("u-{user_id}@example.com")),
            hashed_password: Set("".into()),
            is_active: Set(true),
            is_superuser: Set(false),
            is_verified: Set(true),
            tenant_id: Set(None),
            created_at: Set(now),
            updated_at: Set(None),
        })
        .exec(db)
        .await;
        hex
    }

    async fn seed_role(
        db: &DatabaseConnection,
        role_id: Uuid,
        tenant_hex: &str,
        name: &str,
    ) -> String {
        use crate::entities::{principal, role};
        let now = Utc::now();
        let hex = uuid_hex::to_hex(role_id);
        let _ = principal::Entity::insert(principal::ActiveModel {
            id: Set(hex.clone()),
            principal_type: Set("role".into()),
            created_at: Set(now),
            updated_at: Set(None),
        })
        .exec(db)
        .await;
        let _ = role::Entity::insert(role::ActiveModel {
            id: Set(hex.clone()),
            name: Set(name.into()),
            tenant_id: Set(tenant_hex.to_string()),
            created_at: Set(now),
            updated_at: Set(None),
        })
        .exec(db)
        .await;
        hex
    }

    #[tokio::test]
    async fn owner_is_admin() {
        let db = fresh_db().await;
        let owner = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        seed_tenant(&db, owner, tenant).await;
        assert!(is_tenant_admin(&db, owner, tenant).await.unwrap());
    }

    #[tokio::test]
    async fn non_owner_no_role_is_not_admin() {
        let db = fresh_db().await;
        let owner = Uuid::new_v4();
        let other = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        seed_tenant(&db, owner, tenant).await;
        seed_user(&db, other).await;
        assert!(!is_tenant_admin(&db, other, tenant).await.unwrap());
    }

    #[tokio::test]
    async fn non_owner_with_admin_role_is_admin() {
        let db = fresh_db().await;
        let owner = Uuid::new_v4();
        let other = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        let role_id = Uuid::new_v4();
        let (_, tenant_hex) = seed_tenant(&db, owner, tenant).await;
        seed_user(&db, other).await;
        seed_role(&db, role_id, &tenant_hex, "admin").await;
        // Assign role
        use crate::entities::user_role;
        user_role::Entity::insert(user_role::ActiveModel {
            user_id: Set(uuid_hex::to_hex(other)),
            role_id: Set(uuid_hex::to_hex(role_id)),
            created_at: Set(Utc::now()),
        })
        .exec(&db)
        .await
        .unwrap();
        assert!(is_tenant_admin(&db, other, tenant).await.unwrap());
    }

    #[tokio::test]
    async fn non_owner_with_non_admin_role_is_not_admin() {
        let db = fresh_db().await;
        let owner = Uuid::new_v4();
        let other = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        let role_id = Uuid::new_v4();
        let (_, tenant_hex) = seed_tenant(&db, owner, tenant).await;
        seed_user(&db, other).await;
        seed_role(&db, role_id, &tenant_hex, "viewer").await;
        use crate::entities::user_role;
        user_role::Entity::insert(user_role::ActiveModel {
            user_id: Set(uuid_hex::to_hex(other)),
            role_id: Set(uuid_hex::to_hex(role_id)),
            created_at: Set(Utc::now()),
        })
        .exec(&db)
        .await
        .unwrap();
        assert!(!is_tenant_admin(&db, other, tenant).await.unwrap());
    }
}
