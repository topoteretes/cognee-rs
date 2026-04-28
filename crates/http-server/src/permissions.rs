//! Permission-gate helpers used across the write-path routers.
//!
//! P5 wires the full [`cognee_database::permissions::PermissionsRepository`]
//! 8-step resolution from `tenants.md §5.1`. When `ComponentHandles::permissions`
//! is `Some`, we delegate to `user_can`; otherwise we fall back to the legacy
//! 3-step `AclDb::has_permission_with_roles` shim that has been the P2 default.
//!
//! `REQUIRE_AUTHORIZATION=false|0|no` short-circuits to `Ok(())` (Python's
//! `ENABLE_BACKEND_ACCESS_CONTROL=false` parity).

use std::sync::Arc;

use cognee_database::permissions::PermissionsRepository;
use cognee_database::{AclDb, DatabaseConnection};
use uuid::Uuid;

use crate::components::ComponentHandles;
use crate::error::ApiError;

/// Check that `user_id` has `perm` on `dataset_id`.
///
/// Implements the legacy code-path (compatibility shim used by every existing
/// P2 call site). Backends that pass an `&dyn PermissionsRepository` should
/// prefer [`check_permission_via_repo`] instead.
pub async fn check_permission(
    db: &DatabaseConnection,
    user_id: Uuid,
    dataset_id: Uuid,
    perm: &str,
) -> Result<(), ApiError> {
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

/// Check `user_can` via the full 8-step `PermissionsRepository` resolution
/// (`tenants.md §5.1`).
///
/// This is the preferred call site once the repo is wired into
/// `ComponentHandles::permissions`. Returns `Ok(())` on allow,
/// `Err(ApiError::Forbidden)` on deny, `Err(ApiError::Internal)` on DB error.
pub async fn check_permission_via_repo(
    repo: &Arc<dyn PermissionsRepository>,
    user_id: Uuid,
    dataset_id: Uuid,
    perm: &str,
) -> Result<(), ApiError> {
    if is_authorization_disabled() {
        return Ok(());
    }

    let allowed = repo
        .user_can(user_id, dataset_id, perm)
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

/// Dispatch on the `ComponentHandles`: prefer the full
/// [`PermissionsRepository::user_can`] resolution (`tenants.md §5.1`) when
/// the slot is populated, otherwise fall back to the legacy
/// [`AclDb::has_permission_with_roles`] shim.
///
/// Used by the write-path routers (`add`, `datasets`, `delete`, `update`,
/// `forget`) so the call site stays a single line and the resolver depth
/// is decided by what's wired into the state.
pub async fn check_permission_via_handles(
    handles: &ComponentHandles,
    user_id: Uuid,
    dataset_id: Uuid,
    perm: &str,
) -> Result<(), ApiError> {
    if is_authorization_disabled() {
        return Ok(());
    }
    if let Some(ref repo) = handles.permissions {
        check_permission_via_repo(repo, user_id, dataset_id, perm).await
    } else {
        check_permission(&handles.database, user_id, dataset_id, perm).await
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
