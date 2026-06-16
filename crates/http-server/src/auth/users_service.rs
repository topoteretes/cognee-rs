//! User management service (get / update / delete).

use cognee_database::{AuthUser, UpdateUserPayload};
use uuid::Uuid;

use super::{
    context::AuthContext,
    password::{hash_new_password, validate_password},
};
use crate::error::ApiError;

/// Well-known default user ID (matches the seed in the migration).
pub const DEFAULT_USER_ID: &str = "00000000000000000000000000000000";

pub fn default_user_uuid() -> Uuid {
    #[allow(clippy::expect_used, reason = "invariant is upheld by construction")]
    Uuid::parse_str("00000000-0000-0000-0000-000000000000")
        .expect("default_user_uuid is a valid UUID v4 nil")
}

/// Get a user by ID; returns `ApiError::NotFound` if missing or malformed UUID.
pub async fn get_by_id(ctx: &AuthContext, id: Uuid) -> Result<AuthUser, ApiError> {
    ctx.user_repo
        .find_by_id(id)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?
        .ok_or_else(|| ApiError::NotFound("Not Found".to_owned()))
}

/// Update a user.
///
/// `safe = true` → strip `is_active`, `is_superuser`, `is_verified` (PATCH /me).
/// `safe = false` → allow all fields (PATCH /{id}, superuser only).
#[allow(clippy::too_many_arguments)]
pub async fn update(
    ctx: &AuthContext,
    user: &AuthUser,
    new_email: Option<String>,
    new_password: Option<String>,
    new_is_active: Option<bool>,
    new_is_superuser: Option<bool>,
    new_is_verified: Option<bool>,
    safe: bool,
) -> Result<AuthUser, ApiError> {
    let mut payload = UpdateUserPayload::default();

    // Email uniqueness check
    if let Some(ref email) = new_email {
        let existing_opt = ctx
            .user_repo
            .find_by_email(email)
            .await
            .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
        if let Some(existing) = existing_opt
            && existing.id != user.id
        {
            return Err(ApiError::UpdateUserEmailAlreadyExists);
        }
        payload.email = Some(email.clone());
    }

    // Password validation + hashing
    if let Some(ref pw) = new_password {
        let email_for_validation = new_email.as_deref().unwrap_or(&user.email);
        validate_password(pw, email_for_validation)
            .map_err(|reason| ApiError::UpdateUserInvalidPassword(reason.to_string()))?;
        let hash = hash_new_password(pw).map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
        payload.hashed_password = Some(hash);
    }

    // safe=True strips privileged fields
    if !safe {
        payload.is_active = new_is_active;
        payload.is_superuser = new_is_superuser;
        payload.is_verified = new_is_verified;
    }

    ctx.user_repo
        .update(user.id, payload)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))
}

/// Delete a user by ID.
///
/// Rejects deletion of the well-known default user.
pub async fn delete_by_id(ctx: &AuthContext, id: Uuid) -> Result<(), ApiError> {
    if id == default_user_uuid() {
        return Err(ApiError::Forbidden(
            "Cannot delete the default user".to_owned(),
        ));
    }
    // Confirm the user exists first (returns 404 for missing, not 500)
    get_by_id(ctx, id).await?;
    ctx.user_repo
        .delete_by_id(id)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))
}
