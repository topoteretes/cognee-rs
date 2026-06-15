//! P5 PermissionsRepository trait + lightweight DTOs.
//!
//! The trait surface is documented in `tenants.md §9` and consumed by the
//! HTTP server's `/api/v1/permissions` router. The SeaORM-backed
//! implementation lives in [`sea_orm_impl`].

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use thiserror::Error;
use uuid::Uuid;

use crate::types::DatabaseError;

pub mod sea_orm_impl;
pub mod tenant_admin;

pub use sea_orm_impl::SeaOrmPermissionsRepository;
pub use tenant_admin::{
    USER_MANAGEMENT_ALLOWED_ROLE_NAMES, has_user_management_permission, is_tenant_admin,
};

/// Lightweight role projection. `description` is reserved for forward-compat
/// with Python's `getattr(role, "description", None)` — Python's column does
/// not exist today so we always emit `None` when serializing.
#[derive(Debug, Clone)]
pub struct Role {
    pub id: Uuid,
    pub name: String,
    pub tenant_id: Uuid,
    pub description: Option<String>,
    pub user_count: usize,
}

/// Tenant projection.
#[derive(Debug, Clone)]
pub struct Tenant {
    pub id: Uuid,
    pub name: String,
    pub owner_id: Option<Uuid>,
}

/// User projection. Carries only the fields the listing endpoints surface.
#[derive(Debug, Clone)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub roles: Vec<Role>,
}

/// `principal_configuration` row projection.
#[derive(Debug, Clone)]
pub struct PrincipalConfiguration {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub name: String,
    pub configuration: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: Option<DateTime<Utc>>,
}

/// PermissionsRepository surface error.
#[derive(Debug, Error)]
pub enum PermissionsError {
    #[error("not found: {0}")]
    EntityNotFound(String),

    #[error("already exists: {0}")]
    EntityAlreadyExists(String),

    #[error("permission denied: {0}")]
    PermissionDenied(String),

    #[error("validation: {0}")]
    Validation(String),

    #[error("database error: {0}")]
    Database(#[from] DatabaseError),
}

/// PermissionsRepository — full RBAC CRUD + 8-step `user_can` resolution.
///
/// Surface specified in `tenants.md §9`. SeaORM-backed implementation in
/// [`sea_orm_impl::SeaOrmPermissionsRepository`].
#[async_trait]
pub trait PermissionsRepository: Send + Sync {
    /// 8-step resolution per `tenants.md §5.1`.
    async fn user_can(
        &self,
        user_id: Uuid,
        dataset_id: Uuid,
        perm: &str,
    ) -> Result<bool, PermissionsError>;

    /// Datasets visible to the user with the given permission.
    async fn visible_datasets(
        &self,
        user_id: Uuid,
        perm: &str,
    ) -> Result<Vec<Uuid>, PermissionsError>;

    /// Insert into `acls` (idempotent).
    async fn grant_acl(
        &self,
        principal_id: Uuid,
        dataset_id: Uuid,
        perm: &str,
    ) -> Result<(), PermissionsError>;

    /// Delete the matching `acls` row.
    async fn revoke_acl(
        &self,
        principal_id: Uuid,
        dataset_id: Uuid,
        perm: &str,
    ) -> Result<(), PermissionsError>;

    /// Insert a new role under `tenant_id`. Returns the new role ID.
    async fn create_role(&self, tenant_id: Uuid, name: &str) -> Result<Uuid, PermissionsError>;

    /// Insert a new tenant + side effects (set caller current tenant + add
    /// user_tenants membership row). Returns the new tenant ID.
    async fn create_tenant(&self, name: &str, owner_id: Uuid) -> Result<Uuid, PermissionsError>;

    /// Insert into `user_roles`.
    async fn assign_role(&self, user_id: Uuid, role_id: Uuid) -> Result<(), PermissionsError>;

    /// Delete from `user_roles`.
    async fn revoke_role(&self, user_id: Uuid, role_id: Uuid) -> Result<(), PermissionsError>;

    /// Insert into `user_tenants`.
    async fn add_user_to_tenant(
        &self,
        user_id: Uuid,
        tenant_id: Uuid,
    ) -> Result<(), PermissionsError>;

    /// Cascade-delete a user's roles + ACLs in the tenant, then their
    /// `user_tenants` row. Rejects removing the tenant owner.
    async fn remove_user_from_tenant(
        &self,
        user_id: Uuid,
        tenant_id: Uuid,
    ) -> Result<(), PermissionsError>;

    /// `UPDATE users SET tenant_id = ? WHERE id = ?`. Verifies membership
    /// when `tenant_id.is_some()`.
    async fn select_current_tenant(
        &self,
        user_id: Uuid,
        tenant_id: Option<Uuid>,
    ) -> Result<(), PermissionsError>;

    async fn list_my_tenants(&self, user_id: Uuid) -> Result<Vec<Tenant>, PermissionsError>;

    async fn list_tenant_roles(&self, tenant_id: Uuid) -> Result<Vec<Role>, PermissionsError>;

    async fn list_users_in_role(
        &self,
        tenant_id: Uuid,
        role_id: Uuid,
    ) -> Result<Vec<User>, PermissionsError>;

    async fn list_user_roles(
        &self,
        tenant_id: Uuid,
        user_id: Uuid,
    ) -> Result<Vec<Role>, PermissionsError>;

    async fn list_users_in_tenant(&self, tenant_id: Uuid) -> Result<Vec<User>, PermissionsError>;

    async fn is_tenant_admin(
        &self,
        user_id: Uuid,
        tenant_id: Uuid,
    ) -> Result<bool, PermissionsError>;

    /// Verifies the user owns the tenant.
    async fn tenant_owner(&self, tenant_id: Uuid) -> Result<Option<Uuid>, PermissionsError>;

    /// Look up the user's current tenant (`users.tenant_id`).
    async fn current_tenant(&self, user_id: Uuid) -> Result<Option<Uuid>, PermissionsError>;

    /// Return the tenant a role belongs to, or `None` if the role does not exist.
    async fn role_tenant_id(&self, role_id: Uuid) -> Result<Option<Uuid>, PermissionsError>;

    /// Cascade-delete a role and all its associated data:
    /// 1. `user_roles` rows for the role (remove all memberships)
    /// 2. `acls` rows where `principal_id == role_id` (remove role-held ACLs)
    /// 3. the `roles` row
    /// 4. the `principals` row (roles are principals)
    ///
    /// Mirrors Python's `delete_role.py` cascade. Idempotent when the role does
    /// not exist (returns `Ok(())`).
    async fn delete_role(&self, role_id: Uuid) -> Result<(), PermissionsError>;
}
