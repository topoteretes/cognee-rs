//! `AuthenticatedUser` + `OptionalAuthenticatedUser` + `RequireSuperuser`
//! extractors for axum handlers.
//!
//! Resolution order (matches Python):
//! 1. `X-Api-Key` header → lookup_api_key
//! 2. `Authorization: Bearer <jwt>` → decode_login_jwt
//! 3. Cookie `<cookie_name>=<jwt>` → decode_login_jwt
//! 4. If `require_authentication == false` → default user (id=all-zeros)
//! 5. Else → 401 Unauthorized

use axum::{
    extract::FromRequestParts,
    http::{HeaderMap, request::Parts},
};
use uuid::Uuid;

use super::{api_key::lookup_api_key, jwt::decode_login_jwt};
use crate::error::ApiError;
use crate::state::AppState;

// ─── AuthMethod ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMethod {
    ApiKey,
    BearerJwt,
    CookieJwt,
    DefaultUser,
}

// ─── AuthenticatedUser ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct AuthenticatedUser {
    pub id: Uuid,
    pub email: String,
    pub is_superuser: bool,
    pub is_verified: bool,
    pub is_active: bool,
    pub tenant_id: Option<Uuid>,
    pub auth_method: AuthMethod,
}

impl FromRequestParts<AppState> for AuthenticatedUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let Some(auth) = state.auth.as_ref() else {
            // No auth context wired — use default-user behaviour
            return default_user_from_state(state).await;
        };

        // ── 1. X-Api-Key header ──────────────────────────────────────────────
        if let Some(api_key_val) = parts.headers.get("X-Api-Key").and_then(|v| v.to_str().ok())
            && let Some(user) = lookup_api_key(api_key_val, auth).await
        {
            if !user.is_active {
                return Err(ApiError::LoginBadCredentials);
            }
            return Ok(Self {
                id: user.id,
                email: user.email,
                is_superuser: user.is_superuser,
                is_verified: user.is_verified,
                is_active: user.is_active,
                tenant_id: user.tenant_id,
                auth_method: AuthMethod::ApiKey,
            });
        }

        // ── 2. Authorization: Bearer <jwt> ───────────────────────────────────
        if let Some(bearer_jwt) = extract_bearer(&parts.headers)
            && let Ok(claims) = decode_login_jwt(bearer_jwt, auth)
            && let Ok(uid) = Uuid::parse_str(&claims.sub)
            && let Ok(Some(user)) = auth.user_repo.find_by_id(uid).await
        {
            if !user.is_active {
                return Err(ApiError::Unauthorized);
            }
            return Ok(Self {
                id: user.id,
                email: user.email,
                is_superuser: user.is_superuser,
                is_verified: user.is_verified,
                is_active: user.is_active,
                tenant_id: user.tenant_id,
                auth_method: AuthMethod::BearerJwt,
            });
        }

        // ── 3. Cookie <cookie_name>=<jwt> ────────────────────────────────────
        if let Some(cookie_jwt) = extract_cookie(&parts.headers, &auth.cookie_name)
            && let Ok(claims) = decode_login_jwt(cookie_jwt, auth)
            && let Ok(uid) = Uuid::parse_str(&claims.sub)
            && let Ok(Some(user)) = auth.user_repo.find_by_id(uid).await
        {
            if !user.is_active {
                return Err(ApiError::Unauthorized);
            }
            return Ok(Self {
                id: user.id,
                email: user.email,
                is_superuser: user.is_superuser,
                is_verified: user.is_verified,
                is_active: user.is_active,
                tenant_id: user.tenant_id,
                auth_method: AuthMethod::CookieJwt,
            });
        }

        // ── 4. Default user (require_authentication=false) ───────────────────
        if !auth.require_authentication {
            return default_user_from_state(state).await;
        }

        // ── 5. Reject ────────────────────────────────────────────────────────
        Err(ApiError::Unauthorized)
    }
}

async fn default_user_from_state(state: &AppState) -> Result<AuthenticatedUser, ApiError> {
    // Well-known default user (seeded in the migration)
    let default_id = Uuid::nil();
    // Try to look up via auth context repo if available
    if let Some(auth) = state.auth.as_ref()
        && let Ok(Some(user)) = auth.user_repo.find_by_id(default_id).await
    {
        return Ok(AuthenticatedUser {
            id: user.id,
            email: user.email,
            is_superuser: user.is_superuser,
            is_verified: user.is_verified,
            is_active: user.is_active,
            tenant_id: user.tenant_id,
            auth_method: AuthMethod::DefaultUser,
        });
    }
    // Fall back to a synthetic default user struct
    Ok(AuthenticatedUser {
        id: default_id,
        email: "default_user@example.com".to_owned(),
        is_superuser: true,
        is_verified: true,
        is_active: true,
        tenant_id: None,
        auth_method: AuthMethod::DefaultUser,
    })
}

// ─── OptionalAuthenticatedUser ────────────────────────────────────────────────

/// Same resolution as `AuthenticatedUser` but returns `None` instead of 401.
#[derive(Debug, Clone)]
pub struct OptionalAuthenticatedUser(pub Option<AuthenticatedUser>);

impl FromRequestParts<AppState> for OptionalAuthenticatedUser {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        match AuthenticatedUser::from_request_parts(parts, state).await {
            Ok(u) => Ok(Self(Some(u))),
            Err(_) => Ok(Self(None)),
        }
    }
}

// ─── RequireSuperuser ─────────────────────────────────────────────────────────

/// Wraps `AuthenticatedUser` and returns 403 if the user is not a superuser.
#[derive(Debug, Clone)]
pub struct RequireSuperuser(pub AuthenticatedUser);

impl FromRequestParts<AppState> for RequireSuperuser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let user = AuthenticatedUser::from_request_parts(parts, state).await?;
        if !user.is_superuser {
            return Err(ApiError::Forbidden("Forbidden".to_owned()));
        }
        Ok(Self(user))
    }
}

// ─── Header helpers ───────────────────────────────────────────────────────────

fn extract_bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
}

fn extract_cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let cookie_header = headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())?;
    for part in cookie_header.split(';') {
        let part = part.trim();
        if let Some(val) = part.strip_prefix(&format!("{name}=")) {
            return Some(val);
        }
    }
    None
}
