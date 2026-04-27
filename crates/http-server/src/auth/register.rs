//! User registration service.

use cognee_database::{AuthUser, CreateUserPayload};
use uuid::Uuid;

use super::{
    context::AuthContext,
    mailer::Mailer,
    password::{hash_new_password, validate_password},
};
use crate::error::ApiError;

/// Create a new user with the given email and password.
///
/// Applies `safe=True` semantics: `is_active=true`, `is_superuser=false`,
/// `is_verified=true` regardless of what the caller sends.
pub async fn create_user(
    email: &str,
    password: &str,
    mailer: &dyn Mailer,
    ctx: &AuthContext,
) -> Result<AuthUser, ApiError> {
    // Check duplicate email
    if ctx
        .user_repo
        .find_by_email(email)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?
        .is_some()
    {
        return Err(ApiError::RegisterUserAlreadyExists);
    }

    // Validate password
    validate_password(password, email)
        .map_err(|reason| ApiError::RegisterInvalidPassword(reason.to_string()))?;

    // Hash password with argon2id
    let hashed = hash_new_password(password).map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;

    let user = ctx
        .user_repo
        .create(CreateUserPayload {
            id: Uuid::new_v4(),
            email: email.to_owned(),
            hashed_password: hashed,
            is_active: true,
            is_superuser: false,
            is_verified: true, // cognee default
            tenant_id: None,
        })
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;

    // Side effect: send welcome email (best-effort; mailer errors are logged not propagated)
    if let Err(e) = mailer.send_register_welcome(&user).await {
        tracing::warn!("send_register_welcome failed: {e}");
    }

    Ok(user)
}
