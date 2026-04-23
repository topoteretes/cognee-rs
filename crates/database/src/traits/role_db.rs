use async_trait::async_trait;
use cognee_models::Role;
use uuid::Uuid;

use crate::types::DatabaseError;

/// CRUD and membership operations for roles (scoped to a tenant).
#[async_trait]
pub trait RoleDb: Send + Sync {
    async fn create_role(&self, role: &Role) -> Result<Role, DatabaseError>;
    async fn get_role(&self, id: Uuid) -> Result<Option<Role>, DatabaseError>;
    async fn list_roles_in_tenant(&self, tenant_id: Uuid) -> Result<Vec<Role>, DatabaseError>;
    async fn assign_user_to_role(&self, user_id: Uuid, role_id: Uuid) -> Result<(), DatabaseError>;
    async fn remove_user_from_role(
        &self,
        user_id: Uuid,
        role_id: Uuid,
    ) -> Result<(), DatabaseError>;
    async fn get_user_roles(
        &self,
        user_id: Uuid,
        tenant_id: Uuid,
    ) -> Result<Vec<Role>, DatabaseError>;
}
