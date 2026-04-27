//! `SuperuserOnly` extractor — visualize-flavored superuser gate.
//!
//! Distinct from `RequireSuperuser` (defined in `extractor.rs`) which emits
//! the canonical `{"detail": "Forbidden"}` envelope. `SuperuserOnly` emits the
//! visualize-router-specific `{"error": "..."}` envelope per
//! `docs/http-server/routers/visualize.md` §2.2.
//!
//! Do NOT modify `RequireSuperuser`; the two coexist because they carry
//! different wire-compatibility constraints.

use axum::{extract::FromRequestParts, http::StatusCode, http::request::Parts};

use super::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

/// Wraps `AuthenticatedUser` with a `is_superuser == true` precondition.
///
/// On rejection, returns
/// `403 {"error": "Superuser privileges required for multi-user visualization"}`.
#[derive(Debug, Clone)]
pub struct SuperuserOnly(pub AuthenticatedUser);

impl FromRequestParts<AppState> for SuperuserOnly {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let user = AuthenticatedUser::from_request_parts(parts, state).await?;
        if !user.is_superuser {
            return Err(ApiError::VisualizeError(
                StatusCode::FORBIDDEN,
                "Superuser privileges required for multi-user visualization".to_string(),
            ));
        }
        Ok(Self(user))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::response::IntoResponse;
    use serde_json::Value;

    #[tokio::test]
    async fn test_non_superuser_rejection_envelope() {
        let err = ApiError::VisualizeError(
            StatusCode::FORBIDDEN,
            "Superuser privileges required for multi-user visualization".to_string(),
        );
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.expect("body");
        let v: Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(
            v["error"],
            "Superuser privileges required for multi-user visualization"
        );
        assert!(v.get("detail").is_none());
    }

    #[test]
    fn test_superuser_only_constructs_from_superuser_authenticated_user() {
        // Documents the success path: when the wrapped user has
        // `is_superuser == true`, `SuperuserOnly` retains the AuthenticatedUser.
        let admin = AuthenticatedUser {
            id: uuid::Uuid::nil(),
            email: "admin@example.com".to_string(),
            is_superuser: true,
            is_verified: true,
            is_active: true,
            tenant_id: None,
            auth_method: super::super::extractor::AuthMethod::DefaultUser,
        };
        let wrapper = SuperuserOnly(admin.clone());
        assert!(wrapper.0.is_superuser);
        assert_eq!(wrapper.0.id, admin.id);
    }
}
