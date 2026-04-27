//! Permission-gate helpers for P2 handlers.
//!
//! Wraps [`cognee_database::AclDb::has_permission_with_roles`] into a single
//! async function that maps `false` → `ApiError::Forbidden`.
//!
//! Every call site in the P2 handlers is annotated with:
//! ```text
//! // TODO(P5): wire full PermissionsRepository once tenants_rbac migration lands
//! ```
//!
//! Until P5's `tenants_rbac` migration lands, a fresh SQLite DB has no ACL rows
//! and `has_permission_with_roles` returns `false` for every check.  Tests that
//! exercise permission-gated endpoints must either:
//!  1. Seed ACL rows via `db.grant_permission(user_id, dataset_id, perm)`.
//!  2. Set `REQUIRE_AUTHORIZATION=false` (mirrors Python's
//!     `ENABLE_BACKEND_ACCESS_CONTROL` env var) which bypasses the check.

use uuid::Uuid;

use cognee_database::{AclDb, DatabaseConnection};

use crate::error::ApiError;

/// Check that `user_id` has `perm` on `dataset_id`, using the full
/// role+tenant-aware resolution order.
///
/// Returns `Ok(())` when access is granted.
/// Returns `Err(ApiError::Forbidden(...))` when access is denied.
/// Returns `Err(ApiError::Internal(...))` on DB errors.
///
/// # Permission bypass
///
/// When the environment variable `REQUIRE_AUTHORIZATION` is set to `false`,
/// `0`, or `no` this function returns `Ok(())` immediately — matching Python's
/// `ENABLE_BACKEND_ACCESS_CONTROL=false` bypass.  Use only in development.
pub async fn check_permission(
    db: &DatabaseConnection,
    user_id: Uuid,
    dataset_id: Uuid,
    perm: &str,
) -> Result<(), ApiError> {
    // TODO(P5): wire full PermissionsRepository once tenants_rbac migration lands
    if is_authorization_disabled() {
        return Ok(());
    }

    let allowed = db
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
