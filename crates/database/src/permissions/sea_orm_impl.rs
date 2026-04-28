//! SeaORM-backed [`PermissionsRepository`] implementation.
//!
//! Implements the trait surface from `tenants.md §9` against the schema
//! created by `m20250201_000001_acl_tables.rs`,
//! `m20250422_000001_user_tenant_role_tables.rs`, and
//! `m20260428_000001_tenants_rbac.rs`.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use sea_orm::prelude::*;
use sea_orm::{DatabaseConnection, QuerySelect, Set};
use uuid::Uuid;

use crate::entities::{
    acl, dataset, permission, principal, role, role_default_permission, tenant,
    tenant_default_permission, user, user_default_permission, user_role, user_tenant,
};
use crate::permissions::{
    PermissionsError, PermissionsRepository, Role, Tenant, User, tenant_admin,
};
use crate::types::DatabaseError;
use crate::uuid_hex;

const PERMISSION_NAMES: &[&str] = &["read", "write", "delete", "share"];

/// SeaORM-backed `PermissionsRepository`. Holds an `Arc<DatabaseConnection>`.
pub struct SeaOrmPermissionsRepository {
    db: Arc<DatabaseConnection>,
}

impl SeaOrmPermissionsRepository {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    fn db(&self) -> &DatabaseConnection {
        self.db.as_ref()
    }

    /// Look up or upsert the canonical `permissions` row by name.
    /// Mirrors Python's `give_permission_on_dataset` behavior — the migration
    /// already seeds the four canonical rows, but we cover the case where
    /// the seed was rolled back.
    async fn ensure_permission_id(&self, name: &str) -> Result<String, PermissionsError> {
        if let Some(p) = permission::Entity::find()
            .filter(permission::Column::Name.eq(name))
            .one(self.db())
            .await
            .map_err(map_db)?
        {
            return Ok(p.id);
        }

        let id = uuid_hex::to_hex(Uuid::new_v4());
        let now = Utc::now();
        let model = permission::ActiveModel {
            id: Set(id.clone()),
            name: Set(name.into()),
            created_at: Set(now),
            updated_at: Set(None),
        };
        // Race-tolerant insert: if another caller raced us, find again.
        if let Err(e) = permission::Entity::insert(model).exec(self.db()).await {
            // Fall through and re-find.
            tracing::debug!("permission insert race ({name}): {e}; re-finding");
            let p = permission::Entity::find()
                .filter(permission::Column::Name.eq(name))
                .one(self.db())
                .await
                .map_err(map_db)?
                .ok_or_else(|| {
                    PermissionsError::EntityNotFound(format!(
                        "Permission '{name}' could not be created"
                    ))
                })?;
            return Ok(p.id);
        }
        Ok(id)
    }
}

fn map_db(e: sea_orm::DbErr) -> PermissionsError {
    PermissionsError::Database(DatabaseError::QueryError(e.to_string()))
}

fn validate_perm(perm: &str) -> Result<(), PermissionsError> {
    if !PERMISSION_NAMES.contains(&perm) {
        return Err(PermissionsError::Validation(format!(
            "Unknown permission '{perm}'; must be one of read|write|delete|share"
        )));
    }
    Ok(())
}

#[async_trait]
impl PermissionsRepository for SeaOrmPermissionsRepository {
    /// 8-step resolution per `tenants.md §5.1`. Short-circuits on first hit.
    ///
    /// Order: superuser → direct user ACL → user-default → tenant ACL →
    /// tenant-default → role ACL → role-default → ownership → DENY.
    async fn user_can(
        &self,
        user_id: Uuid,
        dataset_id: Uuid,
        perm: &str,
    ) -> Result<bool, PermissionsError> {
        validate_perm(perm)?;

        let db = self.db();
        let user_hex = uuid_hex::to_hex(user_id);
        let dataset_hex = uuid_hex::to_hex(dataset_id);

        // ── Step 1: is_superuser ─────────────────────────────────────────
        let user_row = user::Entity::find_by_id(user_hex.clone())
            .one(db)
            .await
            .map_err(map_db)?;

        let user_row = match user_row {
            Some(u) => u,
            None => return Ok(false),
        };

        if user_row.is_superuser {
            return Ok(true);
        }

        // Single round-trip UNION ALL covers steps 2..7. Branch 8 is the
        // implicit fall-through (zero rows = deny).
        let perm_row = permission::Entity::find()
            .filter(permission::Column::Name.eq(perm))
            .one(db)
            .await
            .map_err(map_db)?;

        let perm_id = match perm_row {
            Some(p) => p.id,
            None => return Ok(false),
        };

        // Resolve dataset's owner/tenant once for the visibility-required
        // branches (step 5 and 7).
        let dataset_row = dataset::Entity::find_by_id(dataset_hex.clone())
            .one(db)
            .await
            .map_err(map_db)?;

        // Step 2: direct user ACL
        let direct_user = acl::Entity::find()
            .filter(acl::Column::PrincipalId.eq(user_hex.clone()))
            .filter(acl::Column::DatasetId.eq(dataset_hex.clone()))
            .filter(acl::Column::PermissionId.eq(perm_id.clone()))
            .count(db)
            .await
            .map_err(map_db)?;
        if direct_user > 0 {
            return Ok(true);
        }

        // Step 3: user_default_permissions (requires visibility into the dataset's tenant).
        let user_default = user_default_permission::Entity::find()
            .filter(user_default_permission::Column::UserId.eq(user_hex.clone()))
            .filter(user_default_permission::Column::PermissionId.eq(perm_id.clone()))
            .count(db)
            .await
            .map_err(map_db)?;
        if user_default > 0 {
            // Visibility: user must be a member of the dataset's tenant
            // (or the dataset's owner is the user).
            if user_has_dataset_visibility(db, &user_hex, dataset_row.as_ref()).await? {
                return Ok(true);
            }
        }

        // Step 4: tenant-level ACL (any tenant the user is a member of).
        let user_tenants_rows = user_tenant::Entity::find()
            .filter(user_tenant::Column::UserId.eq(user_hex.clone()))
            .all(db)
            .await
            .map_err(map_db)?;

        let tenant_ids: Vec<String> = user_tenants_rows
            .iter()
            .map(|r| r.tenant_id.clone())
            .collect();

        if !tenant_ids.is_empty() {
            let tenant_acl = acl::Entity::find()
                .filter(acl::Column::PrincipalId.is_in(tenant_ids.clone()))
                .filter(acl::Column::DatasetId.eq(dataset_hex.clone()))
                .filter(acl::Column::PermissionId.eq(perm_id.clone()))
                .count(db)
                .await
                .map_err(map_db)?;
            if tenant_acl > 0 {
                return Ok(true);
            }
        }

        // Step 5: tenant_default_permissions (dataset must belong to one of the user's tenants).
        if let Some(ds) = dataset_row.as_ref()
            && let Some(ref ds_tenant) = ds.tenant_id
            && tenant_ids.contains(ds_tenant)
        {
            let tenant_default = tenant_default_permission::Entity::find()
                .filter(tenant_default_permission::Column::TenantId.eq(ds_tenant.clone()))
                .filter(tenant_default_permission::Column::PermissionId.eq(perm_id.clone()))
                .count(db)
                .await
                .map_err(map_db)?;
            if tenant_default > 0 {
                return Ok(true);
            }
        }

        // Step 6: role ACL (any role the user holds).
        let user_roles_rows = user_role::Entity::find()
            .filter(user_role::Column::UserId.eq(user_hex.clone()))
            .all(db)
            .await
            .map_err(map_db)?;

        let role_ids: Vec<String> = user_roles_rows.iter().map(|r| r.role_id.clone()).collect();

        if !role_ids.is_empty() {
            let role_acl = acl::Entity::find()
                .filter(acl::Column::PrincipalId.is_in(role_ids.clone()))
                .filter(acl::Column::DatasetId.eq(dataset_hex.clone()))
                .filter(acl::Column::PermissionId.eq(perm_id.clone()))
                .count(db)
                .await
                .map_err(map_db)?;
            if role_acl > 0 {
                return Ok(true);
            }

            // Step 7: role_default_permissions — role's tenant must cover dataset's tenant.
            if let Some(ds) = dataset_row.as_ref()
                && let Some(ref ds_tenant) = ds.tenant_id
            {
                // Find roles in the dataset's tenant that the user holds.
                let covering_roles = role::Entity::find()
                    .filter(role::Column::Id.is_in(role_ids))
                    .filter(role::Column::TenantId.eq(ds_tenant.clone()))
                    .all(db)
                    .await
                    .map_err(map_db)?;

                if !covering_roles.is_empty() {
                    let cov_ids: Vec<String> = covering_roles.into_iter().map(|r| r.id).collect();
                    let role_default = role_default_permission::Entity::find()
                        .filter(role_default_permission::Column::RoleId.is_in(cov_ids))
                        .filter(role_default_permission::Column::PermissionId.eq(perm_id.clone()))
                        .count(db)
                        .await
                        .map_err(map_db)?;
                    if role_default > 0 {
                        return Ok(true);
                    }
                }
            }
        }

        // Step 8: ownership — caller is the dataset's owner.
        if let Some(ds) = dataset_row
            && ds.owner_id == user_hex
        {
            return Ok(true);
        }

        Ok(false)
    }

    async fn visible_datasets(
        &self,
        user_id: Uuid,
        perm: &str,
    ) -> Result<Vec<Uuid>, PermissionsError> {
        validate_perm(perm)?;
        let db = self.db();
        let user_hex = uuid_hex::to_hex(user_id);

        // Superuser: every dataset.
        let user_row = user::Entity::find_by_id(user_hex.clone())
            .one(db)
            .await
            .map_err(map_db)?;
        let user_row = match user_row {
            Some(u) => u,
            None => return Ok(Vec::new()),
        };

        if user_row.is_superuser {
            let rows = dataset::Entity::find().all(db).await.map_err(map_db)?;
            return rows
                .into_iter()
                .map(|r| {
                    uuid_hex::from_hex(&r.id)
                        .map_err(|e| PermissionsError::Validation(e.to_string()))
                })
                .collect();
        }

        // Aggregate from each branch and de-dup. Mirrors the logical union of
        // §5.1 branches 2-8 (we just call user_can per dataset for correctness).
        let candidates = dataset::Entity::find().all(db).await.map_err(map_db)?;
        let mut visible: Vec<Uuid> = Vec::new();
        for ds in candidates {
            let ds_id = uuid_hex::from_hex(&ds.id)
                .map_err(|e| PermissionsError::Validation(e.to_string()))?;
            if self.user_can(user_id, ds_id, perm).await? {
                visible.push(ds_id);
            }
        }
        Ok(visible)
    }

    async fn grant_acl(
        &self,
        principal_id: Uuid,
        dataset_id: Uuid,
        perm: &str,
    ) -> Result<(), PermissionsError> {
        validate_perm(perm)?;
        let db = self.db();
        let perm_id = self.ensure_permission_id(perm).await?;
        let principal_hex = uuid_hex::to_hex(principal_id);
        let dataset_hex = uuid_hex::to_hex(dataset_id);

        // Skip if it already exists (Python's `give_permission_on_dataset` checks first).
        let existing = acl::Entity::find()
            .filter(acl::Column::PrincipalId.eq(principal_hex.clone()))
            .filter(acl::Column::PermissionId.eq(perm_id.clone()))
            .filter(acl::Column::DatasetId.eq(dataset_hex.clone()))
            .count(db)
            .await
            .map_err(map_db)?;
        if existing > 0 {
            return Ok(());
        }

        let now = Utc::now();
        let model = acl::ActiveModel {
            id: Set(uuid_hex::to_hex(Uuid::new_v4())),
            principal_id: Set(principal_hex),
            permission_id: Set(perm_id),
            dataset_id: Set(dataset_hex),
            created_at: Set(now),
            updated_at: Set(None),
        };
        acl::Entity::insert(model).exec(db).await.map_err(map_db)?;
        Ok(())
    }

    async fn revoke_acl(
        &self,
        principal_id: Uuid,
        dataset_id: Uuid,
        perm: &str,
    ) -> Result<(), PermissionsError> {
        validate_perm(perm)?;
        let db = self.db();
        let perm_id = self.ensure_permission_id(perm).await?;
        acl::Entity::delete_many()
            .filter(acl::Column::PrincipalId.eq(uuid_hex::to_hex(principal_id)))
            .filter(acl::Column::PermissionId.eq(perm_id))
            .filter(acl::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id)))
            .exec(db)
            .await
            .map_err(map_db)?;
        Ok(())
    }

    async fn create_role(&self, tenant_id: Uuid, name: &str) -> Result<Uuid, PermissionsError> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(PermissionsError::Validation("Role name is empty".into()));
        }
        let db = self.db();
        let tenant_hex = uuid_hex::to_hex(tenant_id);

        // Reject duplicate role within the tenant (UNIQUE (tenant_id, name)).
        let dup = role::Entity::find()
            .filter(role::Column::TenantId.eq(tenant_hex.clone()))
            .filter(role::Column::Name.eq(trimmed))
            .one(db)
            .await
            .map_err(map_db)?;
        if dup.is_some() {
            return Err(PermissionsError::EntityAlreadyExists(format!(
                "Role '{trimmed}' already exists in tenant"
            )));
        }

        let id = Uuid::new_v4();
        let id_hex = uuid_hex::to_hex(id);
        let now = Utc::now();

        principal::Entity::insert(principal::ActiveModel {
            id: Set(id_hex.clone()),
            principal_type: Set("role".into()),
            created_at: Set(now),
            updated_at: Set(None),
        })
        .exec(db)
        .await
        .map_err(map_db)?;

        role::Entity::insert(role::ActiveModel {
            id: Set(id_hex),
            name: Set(trimmed.to_string()),
            tenant_id: Set(tenant_hex),
            created_at: Set(now),
            updated_at: Set(None),
        })
        .exec(db)
        .await
        .map_err(map_db)?;

        Ok(id)
    }

    /// `tenants.owner_id` is currently NOT NULL in our schema. Caller must
    /// supply an `owner_id`. Three sequential commits (no transaction)
    /// per `routers/permissions.md §2.8` Python parity.
    async fn create_tenant(&self, name: &str, owner_id: Uuid) -> Result<Uuid, PermissionsError> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(PermissionsError::Validation("Tenant name is empty".into()));
        }
        let db = self.db();

        // Reject duplicate (UNIQUE on tenants.name).
        let dup = tenant::Entity::find()
            .filter(tenant::Column::Name.eq(trimmed))
            .one(db)
            .await
            .map_err(map_db)?;
        if dup.is_some() {
            return Err(PermissionsError::EntityAlreadyExists(format!(
                "Tenant '{trimmed}' already exists"
            )));
        }

        let id = Uuid::new_v4();
        let id_hex = uuid_hex::to_hex(id);
        let owner_hex = uuid_hex::to_hex(owner_id);
        let now = Utc::now();

        // 1. principal + tenants
        principal::Entity::insert(principal::ActiveModel {
            id: Set(id_hex.clone()),
            principal_type: Set("tenant".into()),
            created_at: Set(now),
            updated_at: Set(None),
        })
        .exec(db)
        .await
        .map_err(map_db)?;

        tenant::Entity::insert(tenant::ActiveModel {
            id: Set(id_hex.clone()),
            name: Set(trimmed.to_string()),
            owner_id: Set(owner_hex.clone()),
            created_at: Set(now),
            updated_at: Set(None),
        })
        .exec(db)
        .await
        .map_err(map_db)?;

        // 2. set users.tenant_id = new tenant for the caller
        if let Some(existing) = user::Entity::find_by_id(owner_hex.clone())
            .one(db)
            .await
            .map_err(map_db)?
        {
            let mut active: user::ActiveModel = existing.into();
            active.tenant_id = Set(Some(id_hex.clone()));
            active.updated_at = Set(Some(now));
            active.update(db).await.map_err(map_db)?;
        }

        // 3. user_tenants membership row
        let _ = user_tenant::Entity::insert(user_tenant::ActiveModel {
            user_id: Set(owner_hex),
            tenant_id: Set(id_hex),
            created_at: Set(now),
        })
        .exec(db)
        .await;

        Ok(id)
    }

    async fn assign_role(&self, user_id: Uuid, role_id: Uuid) -> Result<(), PermissionsError> {
        let db = self.db();
        let user_hex = uuid_hex::to_hex(user_id);
        let role_hex = uuid_hex::to_hex(role_id);

        // Reject duplicate.
        let dup = user_role::Entity::find()
            .filter(user_role::Column::UserId.eq(user_hex.clone()))
            .filter(user_role::Column::RoleId.eq(role_hex.clone()))
            .one(db)
            .await
            .map_err(map_db)?;
        if dup.is_some() {
            return Err(PermissionsError::EntityAlreadyExists(format!(
                "User '{user_id}' already has role '{role_id}'"
            )));
        }

        // Verify role exists; verify user is in the role's tenant.
        let role_row = role::Entity::find_by_id(role_hex.clone())
            .one(db)
            .await
            .map_err(map_db)?
            .ok_or_else(|| {
                PermissionsError::EntityNotFound(format!("Role '{role_id}' not found"))
            })?;

        // Verify user exists.
        let _user_row = user::Entity::find_by_id(user_hex.clone())
            .one(db)
            .await
            .map_err(map_db)?
            .ok_or_else(|| {
                PermissionsError::EntityNotFound(format!("User '{user_id}' not found"))
            })?;

        // Verify user is in the role's tenant.
        let membership = user_tenant::Entity::find()
            .filter(user_tenant::Column::UserId.eq(user_hex.clone()))
            .filter(user_tenant::Column::TenantId.eq(role_row.tenant_id.clone()))
            .one(db)
            .await
            .map_err(map_db)?;
        if membership.is_none() {
            return Err(PermissionsError::EntityNotFound(format!(
                "User '{user_id}' is not part of the role's tenant"
            )));
        }

        let now = Utc::now();
        user_role::Entity::insert(user_role::ActiveModel {
            user_id: Set(user_hex),
            role_id: Set(role_hex),
            created_at: Set(now),
        })
        .exec(db)
        .await
        .map_err(map_db)?;

        Ok(())
    }

    async fn revoke_role(&self, user_id: Uuid, role_id: Uuid) -> Result<(), PermissionsError> {
        let db = self.db();
        user_role::Entity::delete_many()
            .filter(user_role::Column::UserId.eq(uuid_hex::to_hex(user_id)))
            .filter(user_role::Column::RoleId.eq(uuid_hex::to_hex(role_id)))
            .exec(db)
            .await
            .map_err(map_db)?;
        Ok(())
    }

    async fn add_user_to_tenant(
        &self,
        user_id: Uuid,
        tenant_id: Uuid,
    ) -> Result<(), PermissionsError> {
        let db = self.db();
        let user_hex = uuid_hex::to_hex(user_id);
        let tenant_hex = uuid_hex::to_hex(tenant_id);

        // Verify both exist.
        let _ = tenant::Entity::find_by_id(tenant_hex.clone())
            .one(db)
            .await
            .map_err(map_db)?
            .ok_or_else(|| {
                PermissionsError::EntityNotFound(format!("Tenant '{tenant_id}' not found"))
            })?;
        let _ = user::Entity::find_by_id(user_hex.clone())
            .one(db)
            .await
            .map_err(map_db)?
            .ok_or_else(|| {
                PermissionsError::EntityNotFound(format!("User '{user_id}' not found"))
            })?;

        let dup = user_tenant::Entity::find()
            .filter(user_tenant::Column::UserId.eq(user_hex.clone()))
            .filter(user_tenant::Column::TenantId.eq(tenant_hex.clone()))
            .one(db)
            .await
            .map_err(map_db)?;
        if dup.is_some() {
            return Err(PermissionsError::EntityAlreadyExists(format!(
                "User '{user_id}' is already in tenant '{tenant_id}'"
            )));
        }

        user_tenant::Entity::insert(user_tenant::ActiveModel {
            user_id: Set(user_hex),
            tenant_id: Set(tenant_hex),
            created_at: Set(Utc::now()),
        })
        .exec(db)
        .await
        .map_err(map_db)?;

        Ok(())
    }

    async fn remove_user_from_tenant(
        &self,
        user_id: Uuid,
        tenant_id: Uuid,
    ) -> Result<(), PermissionsError> {
        let db = self.db();
        let user_hex = uuid_hex::to_hex(user_id);
        let tenant_hex = uuid_hex::to_hex(tenant_id);

        // Reject removing the tenant owner (Python's CogneeValidationError).
        let tenant_row = tenant::Entity::find_by_id(tenant_hex.clone())
            .one(db)
            .await
            .map_err(map_db)?
            .ok_or_else(|| {
                PermissionsError::EntityNotFound(format!("Tenant '{tenant_id}' not found"))
            })?;
        if tenant_row.owner_id == user_hex {
            return Err(PermissionsError::Validation(
                "Cannot remove tenant owner from their own tenant.".into(),
            ));
        }

        // Resolve role IDs in this tenant (for user_roles cascade).
        let tenant_roles = role::Entity::find()
            .filter(role::Column::TenantId.eq(tenant_hex.clone()))
            .all(db)
            .await
            .map_err(map_db)?;
        let role_ids: Vec<String> = tenant_roles.into_iter().map(|r| r.id).collect();

        if !role_ids.is_empty() {
            user_role::Entity::delete_many()
                .filter(user_role::Column::UserId.eq(user_hex.clone()))
                .filter(user_role::Column::RoleId.is_in(role_ids))
                .exec(db)
                .await
                .map_err(map_db)?;
        }

        // ACLs on datasets in this tenant.
        let tenant_datasets = dataset::Entity::find()
            .filter(dataset::Column::TenantId.eq(tenant_hex.clone()))
            .all(db)
            .await
            .map_err(map_db)?;
        let dataset_ids: Vec<String> = tenant_datasets.into_iter().map(|d| d.id).collect();
        if !dataset_ids.is_empty() {
            acl::Entity::delete_many()
                .filter(acl::Column::PrincipalId.eq(user_hex.clone()))
                .filter(acl::Column::DatasetId.is_in(dataset_ids))
                .exec(db)
                .await
                .map_err(map_db)?;
        }

        // user_tenants row.
        user_tenant::Entity::delete_many()
            .filter(user_tenant::Column::UserId.eq(user_hex))
            .filter(user_tenant::Column::TenantId.eq(tenant_hex))
            .exec(db)
            .await
            .map_err(map_db)?;
        Ok(())
    }

    async fn select_current_tenant(
        &self,
        user_id: Uuid,
        tenant_id: Option<Uuid>,
    ) -> Result<(), PermissionsError> {
        let db = self.db();
        let user_hex = uuid_hex::to_hex(user_id);

        let user_row = user::Entity::find_by_id(user_hex.clone())
            .one(db)
            .await
            .map_err(map_db)?
            .ok_or_else(|| {
                PermissionsError::EntityNotFound(format!("User '{user_id}' not found"))
            })?;

        match tenant_id {
            Some(tid) => {
                let tenant_hex = uuid_hex::to_hex(tid);
                let membership = user_tenant::Entity::find()
                    .filter(user_tenant::Column::UserId.eq(user_hex.clone()))
                    .filter(user_tenant::Column::TenantId.eq(tenant_hex.clone()))
                    .one(db)
                    .await
                    .map_err(map_db)?;
                if membership.is_none() {
                    return Err(PermissionsError::EntityNotFound(
                        "User is not part of the tenant.".into(),
                    ));
                }
                let mut active: user::ActiveModel = user_row.into();
                active.tenant_id = Set(Some(tenant_hex));
                active.updated_at = Set(Some(Utc::now()));
                active.update(db).await.map_err(map_db)?;
            }
            None => {
                let mut active: user::ActiveModel = user_row.into();
                active.tenant_id = Set(None);
                active.updated_at = Set(Some(Utc::now()));
                active.update(db).await.map_err(map_db)?;
            }
        }
        Ok(())
    }

    async fn list_my_tenants(&self, user_id: Uuid) -> Result<Vec<Tenant>, PermissionsError> {
        let db = self.db();
        let user_hex = uuid_hex::to_hex(user_id);
        let memberships = user_tenant::Entity::find()
            .filter(user_tenant::Column::UserId.eq(user_hex))
            .limit(50)
            .all(db)
            .await
            .map_err(map_db)?;
        let tenant_ids: Vec<String> = memberships.into_iter().map(|r| r.tenant_id).collect();
        if tenant_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = tenant::Entity::find()
            .filter(tenant::Column::Id.is_in(tenant_ids))
            .all(db)
            .await
            .map_err(map_db)?;
        rows.into_iter().map(model_to_tenant).collect()
    }

    async fn list_tenant_roles(&self, tenant_id: Uuid) -> Result<Vec<Role>, PermissionsError> {
        let db = self.db();
        let tenant_hex = uuid_hex::to_hex(tenant_id);
        let rows = role::Entity::find()
            .filter(role::Column::TenantId.eq(tenant_hex))
            .limit(50)
            .all(db)
            .await
            .map_err(map_db)?;

        let role_ids: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();

        // Compute per-role user counts.
        let mut counts: HashMap<String, usize> = HashMap::new();
        if !role_ids.is_empty() {
            let memberships = user_role::Entity::find()
                .filter(user_role::Column::RoleId.is_in(role_ids))
                .all(db)
                .await
                .map_err(map_db)?;
            for m in memberships {
                *counts.entry(m.role_id).or_insert(0) += 1;
            }
        }

        rows.into_iter()
            .map(|r| {
                let count = counts.get(&r.id).copied().unwrap_or(0);
                Ok(Role {
                    id: uuid_hex::from_hex(&r.id)
                        .map_err(|e| PermissionsError::Validation(e.to_string()))?,
                    name: r.name,
                    tenant_id: uuid_hex::from_hex(&r.tenant_id)
                        .map_err(|e| PermissionsError::Validation(e.to_string()))?,
                    description: None,
                    user_count: count,
                })
            })
            .collect()
    }

    async fn list_users_in_role(
        &self,
        tenant_id: Uuid,
        role_id: Uuid,
    ) -> Result<Vec<User>, PermissionsError> {
        let db = self.db();
        let tenant_hex = uuid_hex::to_hex(tenant_id);
        let role_hex = uuid_hex::to_hex(role_id);

        // Defensive: assert role belongs to tenant.
        let role_row = role::Entity::find_by_id(role_hex.clone())
            .one(db)
            .await
            .map_err(map_db)?
            .ok_or_else(|| {
                PermissionsError::EntityNotFound(format!("Role '{role_id}' not found"))
            })?;
        if role_row.tenant_id != tenant_hex {
            return Ok(Vec::new());
        }

        let memberships = user_role::Entity::find()
            .filter(user_role::Column::RoleId.eq(role_hex))
            .limit(50)
            .all(db)
            .await
            .map_err(map_db)?;
        let user_ids: Vec<String> = memberships.into_iter().map(|m| m.user_id).collect();
        if user_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = user::Entity::find()
            .filter(user::Column::Id.is_in(user_ids))
            .all(db)
            .await
            .map_err(map_db)?;
        rows.into_iter()
            .map(|r| {
                Ok(User {
                    id: uuid_hex::from_hex(&r.id)
                        .map_err(|e| PermissionsError::Validation(e.to_string()))?,
                    email: r.email,
                    roles: Vec::new(),
                })
            })
            .collect()
    }

    async fn list_user_roles(
        &self,
        tenant_id: Uuid,
        user_id: Uuid,
    ) -> Result<Vec<Role>, PermissionsError> {
        let db = self.db();
        let user_hex = uuid_hex::to_hex(user_id);
        let tenant_hex = uuid_hex::to_hex(tenant_id);

        let memberships = user_role::Entity::find()
            .filter(user_role::Column::UserId.eq(user_hex))
            .all(db)
            .await
            .map_err(map_db)?;
        let role_ids: Vec<String> = memberships.into_iter().map(|m| m.role_id).collect();
        if role_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = role::Entity::find()
            .filter(role::Column::Id.is_in(role_ids))
            .filter(role::Column::TenantId.eq(tenant_hex))
            .limit(50)
            .all(db)
            .await
            .map_err(map_db)?;
        rows.into_iter()
            .map(|r| {
                Ok(Role {
                    id: uuid_hex::from_hex(&r.id)
                        .map_err(|e| PermissionsError::Validation(e.to_string()))?,
                    name: r.name,
                    tenant_id: uuid_hex::from_hex(&r.tenant_id)
                        .map_err(|e| PermissionsError::Validation(e.to_string()))?,
                    description: None,
                    user_count: 0,
                })
            })
            .collect()
    }

    async fn list_users_in_tenant(&self, tenant_id: Uuid) -> Result<Vec<User>, PermissionsError> {
        let db = self.db();
        let tenant_hex = uuid_hex::to_hex(tenant_id);

        let memberships = user_tenant::Entity::find()
            .filter(user_tenant::Column::TenantId.eq(tenant_hex.clone()))
            .limit(50)
            .all(db)
            .await
            .map_err(map_db)?;
        let user_ids: Vec<String> = memberships.into_iter().map(|m| m.user_id).collect();
        if user_ids.is_empty() {
            return Ok(Vec::new());
        }

        let users = user::Entity::find()
            .filter(user::Column::Id.is_in(user_ids.clone()))
            .all(db)
            .await
            .map_err(map_db)?;

        // For each user, list their roles in this tenant.
        let mut result = Vec::with_capacity(users.len());
        for u in users {
            let user_uid = uuid_hex::from_hex(&u.id)
                .map_err(|e| PermissionsError::Validation(e.to_string()))?;
            let roles = self.list_user_roles(tenant_id, user_uid).await?;
            result.push(User {
                id: user_uid,
                email: u.email,
                roles,
            });
        }
        Ok(result)
    }

    async fn is_tenant_admin(
        &self,
        user_id: Uuid,
        tenant_id: Uuid,
    ) -> Result<bool, PermissionsError> {
        tenant_admin::is_tenant_admin(self.db(), user_id, tenant_id).await
    }

    async fn tenant_owner(&self, tenant_id: Uuid) -> Result<Option<Uuid>, PermissionsError> {
        let db = self.db();
        let row = tenant::Entity::find_by_id(uuid_hex::to_hex(tenant_id))
            .one(db)
            .await
            .map_err(map_db)?;
        match row {
            Some(t) => uuid_hex::from_hex(&t.owner_id)
                .map(Some)
                .map_err(|e| PermissionsError::Validation(e.to_string())),
            None => Ok(None),
        }
    }

    async fn role_tenant_id(&self, role_id: Uuid) -> Result<Option<Uuid>, PermissionsError> {
        let db = self.db();
        let row = role::Entity::find_by_id(uuid_hex::to_hex(role_id))
            .one(db)
            .await
            .map_err(map_db)?;
        match row {
            Some(r) => uuid_hex::from_hex(&r.tenant_id)
                .map(Some)
                .map_err(|e| PermissionsError::Validation(e.to_string())),
            None => Ok(None),
        }
    }

    async fn current_tenant(&self, user_id: Uuid) -> Result<Option<Uuid>, PermissionsError> {
        let db = self.db();
        let row = user::Entity::find_by_id(uuid_hex::to_hex(user_id))
            .one(db)
            .await
            .map_err(map_db)?;
        match row {
            Some(u) => match u.tenant_id.as_deref() {
                Some(s) => uuid_hex::from_hex(s)
                    .map(Some)
                    .map_err(|e| PermissionsError::Validation(e.to_string())),
                None => Ok(None),
            },
            None => Ok(None),
        }
    }
}

fn model_to_tenant(m: tenant::Model) -> Result<Tenant, PermissionsError> {
    let owner = uuid_hex::from_hex(&m.owner_id).ok();
    Ok(Tenant {
        id: uuid_hex::from_hex(&m.id).map_err(|e| PermissionsError::Validation(e.to_string()))?,
        name: m.name,
        owner_id: owner,
    })
}

/// User has visibility into the dataset's tenant when (a) the dataset is
/// owned by the user, OR (b) the user is a member of the dataset's tenant
/// (via `user_tenants`). Used by step 5 (`user_default_permissions`).
async fn user_has_dataset_visibility(
    db: &DatabaseConnection,
    user_hex: &str,
    dataset_row: Option<&dataset::Model>,
) -> Result<bool, PermissionsError> {
    let Some(ds) = dataset_row else {
        return Ok(false);
    };
    if ds.owner_id == user_hex {
        return Ok(true);
    }
    let Some(ref ds_tenant) = ds.tenant_id else {
        return Ok(false);
    };
    let count = user_tenant::Entity::find()
        .filter(user_tenant::Column::UserId.eq(user_hex.to_string()))
        .filter(user_tenant::Column::TenantId.eq(ds_tenant.clone()))
        .count(db)
        .await
        .map_err(map_db)?;
    Ok(count > 0)
}
