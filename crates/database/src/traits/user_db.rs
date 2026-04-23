use async_trait::async_trait;
use cognee_models::User;
use uuid::Uuid;

use crate::types::DatabaseError;

/// CRUD operations for `User` rows.
#[async_trait]
pub trait UserDb: Send + Sync {
    async fn get_user(&self, id: Uuid) -> Result<Option<User>, DatabaseError>;
    async fn get_user_by_email(&self, email: &str) -> Result<Option<User>, DatabaseError>;
    async fn create_user(&self, user: &User) -> Result<User, DatabaseError>;
    async fn update_user(&self, user: &User) -> Result<User, DatabaseError>;
    async fn delete_user(&self, id: Uuid) -> Result<(), DatabaseError>;
    async fn list_users(&self, tenant_id: Option<Uuid>) -> Result<Vec<User>, DatabaseError>;
}
