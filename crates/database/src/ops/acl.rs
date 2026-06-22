//! ACL trait-level helper operations.
//!
//! The direct-`DatabaseConnection` implementations that backed the
//! `AclDb` blanket impl moved into the closed `cognee-access-control`
//! crate as part of T2-move (oss-split-plan §4 S2): the auth entities
//! they depended on (`acl`, `permission`, `principal`, `user_role`,
//! `user_tenant`) no longer exist on the OSS schema.
//!
//! What remains here is the trait-only helper used by the OSS ingestion
//! pipeline (which still wires an `&dyn AclDb`) and the canonical
//! `PERMISSION_NAMES` list both halves of the split agree on.

use tracing::instrument;
use uuid::Uuid;

use crate::types::DatabaseError;

/// All permission names defined in the system.
pub const PERMISSION_NAMES: &[&str] = &["read", "write", "delete", "share"];

/// Grant all four permissions (read, write, delete, share) to a principal
/// on a dataset via the [`AclDb`](crate::traits::AclDb) trait.
///
/// Used by the ingestion pipeline to bless the dataset owner on every
/// `add` of a freshly-created dataset. Works with any `&dyn AclDb`
/// implementation, so OSS callers can pair it with `MockAclDb` (tests)
/// or with the closed `AccessControl` newtype (production cloud builds).
#[instrument(
    name = "cognee.db.relational.acl.grant_all_permissions_on_dataset_via_trait",
    level = "info",
    skip_all,
    err
)]
pub async fn grant_all_permissions_on_dataset_via_trait(
    acl_db: &dyn crate::traits::AclDb,
    principal_id: Uuid,
    dataset_id: Uuid,
) -> Result<(), DatabaseError> {
    acl_db.ensure_principal(principal_id, "user").await?;

    for perm_name in PERMISSION_NAMES {
        acl_db
            .grant_permission(principal_id, dataset_id, perm_name)
            .await?;
    }

    Ok(())
}
