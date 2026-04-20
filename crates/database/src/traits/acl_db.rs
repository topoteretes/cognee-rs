use async_trait::async_trait;
use sea_orm::DatabaseConnection;
use uuid::Uuid;

use crate::ops::acl;
use crate::types::DatabaseError;

/// Access control list database trait.
///
/// Provides methods to check, grant, and revoke permissions on datasets
/// for principals (users, roles, tenants). All implementations must be
/// thread-safe for async multi-threaded usage.
#[async_trait]
pub trait AclDb: Send + Sync {
    /// Check if a principal has a specific permission on a dataset.
    ///
    /// Returns `true` if a matching ACL row exists (direct principal match).
    async fn has_permission(
        &self,
        principal_id: Uuid,
        dataset_id: Uuid,
        permission_name: &str,
    ) -> Result<bool, DatabaseError>;

    /// Return all dataset IDs for which the principal has the given permission.
    async fn authorized_dataset_ids(
        &self,
        principal_id: Uuid,
        permission_name: &str,
    ) -> Result<Vec<Uuid>, DatabaseError>;

    /// Grant a permission on a dataset to a principal.
    ///
    /// Idempotent: no-op if the grant already exists.
    async fn grant_permission(
        &self,
        principal_id: Uuid,
        dataset_id: Uuid,
        permission_name: &str,
    ) -> Result<(), DatabaseError>;

    /// Revoke a permission on a dataset from a principal.
    async fn revoke_permission(
        &self,
        principal_id: Uuid,
        dataset_id: Uuid,
        permission_name: &str,
    ) -> Result<(), DatabaseError>;

    /// Ensure a principal row exists (upsert by ID).
    async fn ensure_principal(
        &self,
        principal_id: Uuid,
        principal_type: &str,
    ) -> Result<(), DatabaseError>;
}

#[async_trait]
impl AclDb for DatabaseConnection {
    async fn has_permission(
        &self,
        principal_id: Uuid,
        dataset_id: Uuid,
        permission_name: &str,
    ) -> Result<bool, DatabaseError> {
        acl::has_permission(self, principal_id, dataset_id, permission_name).await
    }

    async fn authorized_dataset_ids(
        &self,
        principal_id: Uuid,
        permission_name: &str,
    ) -> Result<Vec<Uuid>, DatabaseError> {
        acl::authorized_dataset_ids(self, principal_id, permission_name).await
    }

    async fn grant_permission(
        &self,
        principal_id: Uuid,
        dataset_id: Uuid,
        permission_name: &str,
    ) -> Result<(), DatabaseError> {
        acl::grant_permission(self, principal_id, dataset_id, permission_name).await
    }

    async fn revoke_permission(
        &self,
        principal_id: Uuid,
        dataset_id: Uuid,
        permission_name: &str,
    ) -> Result<(), DatabaseError> {
        acl::revoke_permission(self, principal_id, dataset_id, permission_name).await
    }

    async fn ensure_principal(
        &self,
        principal_id: Uuid,
        principal_type: &str,
    ) -> Result<(), DatabaseError> {
        acl::ensure_principal(self, principal_id, principal_type).await
    }
}
