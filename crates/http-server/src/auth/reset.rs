//! Password reset service (forgot-password / reset-password).

use cognee_database::UpdateUserPayload;
use uuid::Uuid;

use super::{
    api_key::sha256_hex,
    context::AuthContext,
    jwt::{decode_reset_jwt, encode_reset_jwt},
    mailer::Mailer,
    password::{hash_new_password, validate_password},
};
use crate::error::ApiError;

/// Compute the password fingerprint used in the reset JWT.
///
/// Matches fastapi-users: `sha256(hashed_password)[..8]` hex chars.
pub fn password_fingerprint(hashed_password: &str) -> String {
    sha256_hex(hashed_password.as_bytes())[..8].to_owned()
}

/// Look up user by email; if found and active, mint a reset JWT and invoke mailer.
///
/// Always returns `Ok(())` to prevent email enumeration.
pub async fn forgot_password(
    email: &str,
    mailer: &dyn Mailer,
    ctx: &AuthContext,
) -> Result<(), ApiError> {
    let user = match ctx
        .user_repo
        .find_by_email(email)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?
    {
        Some(u) if u.is_active => u,
        _ => return Ok(()), // no user or inactive — silent
    };

    let fgpt = password_fingerprint(&user.hashed_password);
    let token = match encode_reset_jwt(user.id, fgpt, ctx) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("encode_reset_jwt failed: {e}");
            return Ok(());
        }
    };

    if let Err(e) = mailer.send_password_reset(&user, &token).await {
        tracing::warn!("send_password_reset failed: {e}");
    }

    Ok(())
}

/// Validate a reset token and set the new password.
pub async fn reset_password(
    token: &str,
    new_password: &str,
    ctx: &AuthContext,
) -> Result<(), ApiError> {
    // Decode the reset JWT (audience check enforced by the decoder)
    let claims = decode_reset_jwt(token, ctx).map_err(|_| ApiError::ResetPasswordBadToken)?;

    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| ApiError::ResetPasswordBadToken)?;

    let user = ctx
        .user_repo
        .find_by_id(user_id)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?
        .ok_or(ApiError::ResetPasswordBadToken)?;

    if !user.is_active {
        return Err(ApiError::ResetPasswordBadToken);
    }

    // Verify password fingerprint (fastapi-users parity: invalidates old tokens)
    if let Some(fgpt_claim) = &claims.password_fgpt {
        let expected_fgpt = password_fingerprint(&user.hashed_password);
        if fgpt_claim != &expected_fgpt {
            return Err(ApiError::ResetPasswordBadToken);
        }
    }

    // Validate and hash new password
    validate_password(new_password, &user.email)
        .map_err(|reason| ApiError::ResetPasswordInvalidPassword(reason.to_string()))?;

    let new_hash =
        hash_new_password(new_password).map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;

    ctx.user_repo
        .update(
            user_id,
            UpdateUserPayload {
                hashed_password: Some(new_hash),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;

    Ok(())
}
