use async_trait::async_trait;
use uuid::Uuid;

use crate::types::DatabaseError;

/// Access control list database trait.
///
/// Provides methods to check, grant, and revoke permissions on datasets
/// for principals (users, roles, tenants). All implementations must be
/// thread-safe for async multi-threaded usage.
///
/// The blanket `impl AclDb for DatabaseConnection` moved to the closed
/// `cognee-access-control` crate as part of T2-move (oss-split-plan §4
/// S2). OSS callers wire ACL through `MockAclDb` (tests) or through the
/// closed `AccessControl` newtype (production cloud builds).
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

    /// Check permission considering role and tenant inheritance.
    ///
    /// Resolution order (mirrors Python `get_all_user_permission_datasets`):
    /// 1. Direct user ACL
    /// 2. Tenant-level ACL for each tenant the user belongs to
    /// 3. Role-level ACL for each role the user holds in those tenants
    async fn has_permission_with_roles(
        &self,
        user_id: Uuid,
        dataset_id: Uuid,
        permission_name: &str,
    ) -> Result<bool, DatabaseError>;

    /// Return all dataset IDs the user can access via direct, tenant, or
    /// role grants. Deduplicates results.
    async fn authorized_dataset_ids_with_roles(
        &self,
        user_id: Uuid,
        permission_name: &str,
    ) -> Result<Vec<Uuid>, DatabaseError>;
}
