//! Login helper: look up user by email, verify password, optionally re-hash.

use cognee_database::{AuthUser, UpdateUserPayload};

use super::{
    context::AuthContext,
    password::{VerifyOutcome, hash_new_password, verify_password},
};
use crate::error::ApiError;

/// Authenticate a user by email and password.
///
/// On success with a bcrypt hash, transparently re-hashes to argon2id and
/// updates the row (re-hash failure is best-effort — the login still succeeds).
pub async fn authenticate(
    email: &str,
    password: &str,
    ctx: &AuthContext,
) -> Result<AuthUser, ApiError> {
    let user = ctx
        .user_repo
        .find_by_email(email)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?
        .ok_or(ApiError::LoginBadCredentials)?;

    if !user.is_active {
        return Err(ApiError::LoginBadCredentials);
    }

    let outcome = verify_password(&user.hashed_password, password)
        .map_err(|_| ApiError::LoginBadCredentials)?;

    if outcome == VerifyOutcome::NeedsRehash {
        // Transparent bcrypt → argon2id upgrade (best-effort)
        match hash_new_password(password) {
            Ok(new_hash) => {
                if let Err(e) = ctx
                    .user_repo
                    .update(
                        user.id,
                        UpdateUserPayload {
                            hashed_password: Some(new_hash),
                            ..Default::default()
                        },
                    )
                    .await
                {
                    // Re-hash failure is non-fatal — log and continue.
                    tracing::warn!(
                        user_id = %user.id,
                        "Failed to re-hash bcrypt password to argon2id: {e}"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(user_id = %user.id, "Failed to compute argon2id hash: {e}");
            }
        }
    }

    Ok(user)
}
