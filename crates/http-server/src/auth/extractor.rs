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
/// `require_authentication=false`.
///
/// **UUID5 content-addressing invariant** (Python parity): the returned
/// `id` MUST equal `Uuid::new_v5(&Uuid::NAMESPACE_OID, email.as_bytes())`
/// for the configured `default_user_email`. This matches
/// [`cognee::api::user::get_or_create_default_user`] and the Python
/// reference SDK (`uuid5(NAMESPACE_OID, email)`), so the HTTP server
/// produces the same owner id as the bindings/CLI for the same email.
/// Without this, data added via bindings (uuid5-derived owner) would
/// not be visible to queries via the HTTP server (previously hardcoded
/// `Uuid::nil()`), breaking the cross-SDK content-addressed UUID5
/// invariants asserted by `e2e-cross-sdk`.
pub fn default_user_from_state(state: &AppState) -> AuthenticatedUser {
    // `state.config` is a plain `Arc<HttpServerConfig>` — no lock guard
    // to drop. The clone is cheap and keeps this function synchronous.
    let email = state.config.default_user_email.clone();
    let id = Uuid::new_v5(&Uuid::NAMESPACE_OID, email.as_bytes());
    AuthenticatedUser {
        id,
        email,
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

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use crate::config::HttpServerConfig;

    /// `default_user_from_state` MUST derive the owner id as
    /// `uuid5(NAMESPACE_OID, default_user_email)` so it matches
    /// `cognee::api::user::get_or_create_default_user` and the
    /// Python reference SDK. Locks down the Python parity invariant
    /// referenced in Plan §7.
    #[tokio::test]
    async fn default_user_id_matches_uuid5_of_configured_email() {
        let cfg = HttpServerConfig {
            default_user_email: "alice@example.com".to_string(),
            ..HttpServerConfig::default()
        };
        let state = AppState::build(cfg)
            .await
            .expect("AppState::build with default config must succeed");

        let user = default_user_from_state(&state);

        let expected_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, "alice@example.com".as_bytes());
        assert_eq!(
            user.id, expected_id,
            "owner id must be uuid5(NAMESPACE_OID, email)"
        );
        assert_eq!(user.email, "alice@example.com");
        assert!(user.is_active);
        assert!(user.is_superuser);
        assert_eq!(user.auth_method, AuthMethod::DefaultUser);
        assert!(user.tenant_id.is_none());
    }

    /// The default-config email derivation must also match what the
    /// bindings/CLI compute via `get_or_create_default_user` for the
    /// out-of-the-box `default_user@example.com`.
    #[tokio::test]
    async fn default_user_id_for_default_email_is_stable() {
        let state = AppState::build(HttpServerConfig::default())
            .await
            .expect("AppState::build with default config must succeed");

        let user = default_user_from_state(&state);

        let expected_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, "default_user@example.com".as_bytes());
        assert_eq!(user.id, expected_id);
        assert_ne!(user.id, Uuid::nil(), "must not regress to the old nil-UUID");
    }
}
