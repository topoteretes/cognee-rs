//! Permission-gate helpers used across the write-path routers.
//!
//! OSS keeps only the trait-based `AclDb` shim. The blanket
//! `impl AclDb for DatabaseConnection`, the full 8-step
//! `PermissionsRepository::user_can` resolution
//! (`tenants.md §5.1`), and the `permissions` router live in the
//! closed `cognee-http-cloud` / `cognee-access-control` crates.
//!
//! `REQUIRE_AUTHORIZATION=false|0|no` short-circuits to `Ok(())` (Python's
//! `ENABLE_BACKEND_ACCESS_CONTROL=false` parity). When the env var is
//! left at its default and no `acl_db` is wired on the
//! [`ComponentHandles`] (the pure-OSS case), permission checks pass
//! through — there is no ACL backend to consult.

use uuid::Uuid;

use crate::components::ComponentHandles;
use crate::error::ApiError;

/// Dispatch on the `ComponentHandles`: when an `acl_db` impl is wired
/// (closed builds), delegate to it; otherwise allow the operation
/// because OSS does not bundle an ACL backend.
pub async fn check_permission_via_handles(
    handles: &ComponentHandles,
    user_id: Uuid,
    dataset_id: Uuid,
    perm: &str,
) -> Result<(), ApiError> {
    if is_authorization_disabled() {
        return Ok(());
    }
    let Some(ref acl) = handles.acl_db else {
        // OSS bundle: no ACL backend wired — allow the operation. The
        // closed `cognee-http-cloud` crate installs a real `AclDb`
        // implementation via the `RouterBuilder`.
        return Ok(());
    };
    let allowed = acl
        .has_permission_with_roles(user_id, dataset_id, perm)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("ACL check failed: {e}")))?;
    if allowed {
        Ok(())
    } else {
        Err(ApiError::Forbidden(format!(
            "No {perm} permission on dataset {dataset_id}"
        )))
    }
}

/// Returns `true` when authorization is required (i.e. not explicitly disabled).
///
/// Mirrors Python's `ENABLE_BACKEND_ACCESS_CONTROL` env var.
pub fn is_authorization_required() -> bool {
    !is_authorization_disabled()
}

/// Returns `true` when `REQUIRE_AUTHORIZATION` is explicitly disabled.
fn is_authorization_disabled() -> bool {
    matches!(
        std::env::var("REQUIRE_AUTHORIZATION")
            .as_deref()
            .unwrap_or("true"),
        "false" | "0" | "no"
    )
}
