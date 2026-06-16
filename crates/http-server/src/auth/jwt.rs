//! JWT encode / decode helpers for login / reset / verify token kinds.
//!
//! All three token kinds use HS256 but different secrets and audiences,
//! so a reset token cannot be used for login and vice-versa.

use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::context::AuthContext;

// ─── Error type ──────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum JwtError {
    #[error("JWT encode error: {0}")]
    Encode(#[from] jsonwebtoken::errors::Error),

    #[error("JWT decode error: {0}")]
    Decode(String),
}

// ─── Claims ───────────────────────────────────────────────────────────────────

/// JWT payload.  `aud` is a `Vec<String>` because fastapi-users emits
/// `["fastapi-users:auth"]` (an array), not a plain string.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub aud: Vec<String>,
    pub exp: usize,
    pub iat: usize,
    /// Password fingerprint claim for reset tokens.
    /// SHA-256(hashed_password)[..8] so old tokens are invalid after a reset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password_fgpt: Option<String>,
    /// Email fingerprint for verify tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

// ─── Shared encoder ───────────────────────────────────────────────────────────

fn encode_with(
    secret: &str,
    audience: &[String],
    lifetime_secs: u64,
    sub: Uuid,
    extra_password_fgpt: Option<String>,
    extra_email: Option<String>,
) -> Result<String, JwtError> {
    let now = chrono::Utc::now().timestamp() as usize;
    let claims = Claims {
        sub: sub.to_string(),
        aud: audience.to_vec(),
        exp: now + lifetime_secs as usize,
        iat: now,
        password_fgpt: extra_password_fgpt,
        email: extra_email,
    };
    let header = Header::new(Algorithm::HS256);
    encode(
        &header,
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(JwtError::Encode)
}

fn decode_with(secret: &str, audience: &[String], token: &str) -> Result<Claims, JwtError> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_audience(audience);
    validation.validate_exp = true;
    validation.leeway = 0;
    decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .map(|td| td.claims)
    .map_err(|e| JwtError::Decode(e.to_string()))
}

// ─── Public API ───────────────────────────────────────────────────────────────

use secrecy::ExposeSecret;

pub fn encode_login_jwt(sub: Uuid, ctx: &AuthContext) -> Result<String, JwtError> {
    encode_with(
        ctx.login_secret.expose_secret(),
        &ctx.login_audience,
        ctx.login_lifetime.as_secs(),
        sub,
        None,
        None,
    )
}

pub fn decode_login_jwt(token: &str, ctx: &AuthContext) -> Result<Claims, JwtError> {
    decode_with(ctx.login_secret.expose_secret(), &ctx.login_audience, token)
}

/// Encode a reset JWT.  Includes `password_fgpt` so the token is invalidated
/// after a password change (fastapi-users parity).
pub fn encode_reset_jwt(
    sub: Uuid,
    password_fgpt: String,
    ctx: &AuthContext,
) -> Result<String, JwtError> {
    encode_with(
        ctx.reset_secret.expose_secret(),
        &ctx.reset_audience,
        ctx.reset_lifetime.as_secs(),
        sub,
        Some(password_fgpt),
        None,
    )
}

pub fn decode_reset_jwt(token: &str, ctx: &AuthContext) -> Result<Claims, JwtError> {
    decode_with(ctx.reset_secret.expose_secret(), &ctx.reset_audience, token)
}

/// Encode a verify JWT.  Includes `email` so the token is invalidated if the
/// user changes their email (fastapi-users parity).
pub fn encode_verify_jwt(sub: Uuid, email: &str, ctx: &AuthContext) -> Result<String, JwtError> {
    encode_with(
        ctx.verify_secret.expose_secret(),
        &ctx.verify_audience,
        ctx.verify_lifetime.as_secs(),
        sub,
        None,
        Some(email.to_owned()),
    )
}

pub fn decode_verify_jwt(token: &str, ctx: &AuthContext) -> Result<Claims, JwtError> {
    decode_with(
        ctx.verify_secret.expose_secret(),
        &ctx.verify_audience,
        token,
    )
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use crate::auth::context::tests::{NopApiKeyRepo, NopUserRepo};
    use crate::config::Environment;
    use std::sync::Arc;

    fn test_ctx() -> AuthContext {
        // Use dev mode so super_secret is accepted
        let cfg = crate::config::HttpServerConfig {
            env: Environment::Dev,
            ..Default::default()
        };
        AuthContext::from_env(&cfg, Arc::new(NopUserRepo), Arc::new(NopApiKeyRepo)).expect("ctx")
    }

    #[test]
    fn login_jwt_round_trip() {
        let ctx = test_ctx();
        let user_id = Uuid::new_v4();
        let token = encode_login_jwt(user_id, &ctx).expect("encode");
        let claims = decode_login_jwt(&token, &ctx).expect("decode");
        assert_eq!(claims.sub, user_id.to_string());
        assert_eq!(claims.aud, vec!["fastapi-users:auth"]);
    }

    #[test]
    fn reset_jwt_round_trip() {
        let ctx = test_ctx();
        let user_id = Uuid::new_v4();
        let token = encode_reset_jwt(user_id, "abcdef12".to_owned(), &ctx).expect("encode");
        let claims = decode_reset_jwt(&token, &ctx).expect("decode");
        assert_eq!(claims.sub, user_id.to_string());
        assert_eq!(claims.aud, vec!["fastapi-users:reset"]);
        assert_eq!(claims.password_fgpt.as_deref(), Some("abcdef12"));
    }

    #[test]
    fn verify_jwt_round_trip() {
        let ctx = test_ctx();
        let user_id = Uuid::new_v4();
        let token = encode_verify_jwt(user_id, "user@example.com", &ctx).expect("encode");
        let claims = decode_verify_jwt(&token, &ctx).expect("decode");
        assert_eq!(claims.sub, user_id.to_string());
        assert_eq!(claims.aud, vec!["fastapi-users:verify"]);
        assert_eq!(claims.email.as_deref(), Some("user@example.com"));
    }

    #[test]
    fn login_jwt_rejected_as_reset() {
        let ctx = test_ctx();
        let user_id = Uuid::new_v4();
        let token = encode_login_jwt(user_id, &ctx).expect("encode");
        // Trying to decode a login token with the reset decoder must fail.
        let result = decode_reset_jwt(&token, &ctx);
        assert!(
            result.is_err(),
            "login JWT must be rejected by reset decoder"
        );
    }

    #[test]
    fn login_jwt_rejected_as_verify() {
        let ctx = test_ctx();
        let user_id = Uuid::new_v4();
        let token = encode_login_jwt(user_id, &ctx).expect("encode");
        let result = decode_verify_jwt(&token, &ctx);
        assert!(
            result.is_err(),
            "login JWT must be rejected by verify decoder"
        );
    }
}
