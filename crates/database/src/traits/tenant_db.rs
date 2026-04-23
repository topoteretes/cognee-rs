use async_trait::async_trait;
use cognee_models::{Tenant, User};
use uuid::Uuid;

use crate::types::DatabaseError;

/// CRUD and membership operations for tenants.
#[async_trait]
pub trait TenantDb: Send + Sync {
    async fn create_tenant(&self, tenant: &Tenant) -> Result<Tenant, DatabaseError>;
    async fn get_tenant(&self, id: Uuid) -> Result<Option<Tenant>, DatabaseError>;
    async fn list_tenants_for_user(&self, user_id: Uuid) -> Result<Vec<Tenant>, DatabaseError>;
    async fn add_user_to_tenant(&self, user_id: Uuid, tenant_id: Uuid)
    -> Result<(), DatabaseError>;
    async fn remove_user_from_tenant(
        &self,
        user_id: Uuid,
        tenant_id: Uuid,
    ) -> Result<(), DatabaseError>;

    /// Switch the user's active tenant. Validates the user is a member.
    /// Passing `None` reverts to the default single-user tenant.
    async fn select_tenant(
        &self,
        user_id: Uuid,
        tenant_id: Option<Uuid>,
    ) -> Result<User, DatabaseError>;
}
