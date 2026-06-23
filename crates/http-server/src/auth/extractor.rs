//! `AuthenticatedUser` + `OptionalAuthenticatedUser` extractors for axum
//! handlers — OSS slim variant.
//!
//! Resolution order:
//! 1. `AuthResolver` on `AppState` (closed-injected — JWT/cookie/API key
//!    chain or external validator) when present and it returns `Some`.
//! 2. If `require_authentication == false` → synthetic default user (id
//!    = all-zeros).
//! 3. Else → 401 Unauthorized.
//!
//! The OSS build keeps no JWT/cookie/API-key parsing state — those moved
//! into the closed `cognee-http-cloud` crate. Closed embedders install
//! an `AuthResolver` via `RouterBuilder::with_auth_resolver(...)` or an
//! `ExtraAuthValidator` via `RouterBuilder::with_extra_validator(...)`.

use axum::{extract::FromRequestParts, http::request::Parts};
use uuid::Uuid;

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
        if let Some(resolver) = state.auth_resolver.as_ref()
            && let Some(user) = resolver.resolve(parts).await
        {
            if !user.is_active {
                return Err(ApiError::LoginBadCredentials);
            }
            return Ok(user);
        }
        if state.config.require_authentication {
            Err(ApiError::Unauthorized)
        } else {
            Ok(default_user_from_state(state))
        }
    }
}

/// Synthetic default user used when no auth resolver is wired and
/// `require_authentication=false`. Matches the previous OSS behaviour
/// where the well-known nil-UUID user is returned without a DB lookup.
pub fn default_user_from_state(_state: &AppState) -> AuthenticatedUser {
    AuthenticatedUser {
        id: Uuid::nil(),
        email: "default_user@example.com".to_owned(),
        is_superuser: true,
        is_verified: true,
        is_active: true,
        tenant_id: None,
        auth_method: AuthMethod::DefaultUser,
    }
}

// ─── OptionalAuthenticatedUser ────────────────────────────────────────────────

/// Same resolution as `AuthenticatedUser` but never errors — returns
/// `None` when authentication fails (instead of 401).
#[derive(Debug, Clone)]
pub struct OptionalAuthenticatedUser(pub Option<AuthenticatedUser>);

impl FromRequestParts<AppState> for OptionalAuthenticatedUser {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        if let Some(resolver) = state.auth_resolver.as_ref()
            && let Some(user) = resolver.resolve(parts).await
        {
            if user.is_active {
                return Ok(Self(Some(user)));
            }
            return Ok(Self(None));
        }
        if state.config.require_authentication {
            Ok(Self(None))
        } else {
            Ok(Self(Some(default_user_from_state(state))))
        }
    }
}
