//! Email verification service (request-verify-token / verify).

use cognee_database::{AuthUser, UpdateUserPayload};
use uuid::Uuid;

use super::{
    context::AuthContext,
    jwt::{decode_verify_jwt, encode_verify_jwt},
    mailer::Mailer,
};
use crate::error::ApiError;

/// Look up user by email; if found, active, and not yet verified, mint a verify
/// JWT and call the mailer.  Always returns `Ok(())` to prevent enumeration.
pub async fn request_verify_token(
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
        Some(u) if u.is_active && !u.is_verified => u,
        _ => return Ok(()), // not found / inactive / already verified — silent
    };

    let token = match encode_verify_jwt(user.id, &user.email, ctx) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("encode_verify_jwt failed: {e}");
            return Ok(());
        }
    };

    if let Err(e) = mailer.send_email_verify(&user, &token).await {
        tracing::warn!("send_email_verify failed: {e}");
    }

    Ok(())
}

/// Validate a verify token and set `is_verified=true`.
///
/// Returns the updated user record (fastapi-users parity: POST /verify returns UserRead).
pub async fn verify_user(token: &str, ctx: &AuthContext) -> Result<AuthUser, ApiError> {
    let claims = decode_verify_jwt(token, ctx).map_err(|_| ApiError::VerifyUserBadToken)?;

    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| ApiError::VerifyUserBadToken)?;

    let user = ctx
        .user_repo
        .find_by_id(user_id)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?
        .ok_or(ApiError::VerifyUserBadToken)?;

    if !user.is_active {
        return Err(ApiError::VerifyUserBadToken);
    }

    // Verify email fingerprint: token's email must still match stored email.
    if let Some(email_claim) = &claims.email
        && email_claim != &user.email
    {
        return Err(ApiError::VerifyUserBadToken);
    }

    if user.is_verified {
        return Err(ApiError::VerifyUserAlreadyVerified);
    }

    let updated = ctx
        .user_repo
        .update(
            user_id,
            UpdateUserPayload {
                is_verified: Some(true),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;

    Ok(updated)
}
