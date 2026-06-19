//! `POST /api/v1/auth/request-verify-token` and `POST /api/v1/auth/verify` handlers.
//!
//! Python source: fastapi-users verify router.
//! Mounted under `/api/v1/auth` in `build_router`.
//!
//! `/request-verify-token` always returns 202 + null to prevent enumeration.

use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use serde_json::Value;

use crate::{
    auth::verify::{request_verify_token, verify_user},
    dto::{
        auth_verify::{RequestVerifyTokenPayloadDTO, VerifyPayloadDTO},
        users::UserReadDTO,
    },
    error::ApiError,
    middleware::validation::Json as ValidatedJson,
    state::AppState,
};

// ─── Handlers ────────────────────────────────────────────────────────────────

/// `POST /api/v1/auth/request-verify-token`
///
/// Sends a verification email if the user exists, is active, and not yet verified.
/// Always returns 202 + null.
/// Auth: none.
async fn post_request_verify_token(
    State(state): State<AppState>,
    ValidatedJson(payload): ValidatedJson<RequestVerifyTokenPayloadDTO>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let Some(auth) = state.auth.as_ref() else {
        return Ok((StatusCode::ACCEPTED, Json(Value::Null)));
    };

    let mailer = state.mailer.as_ref();
    request_verify_token(&payload.email, mailer, auth).await?;

    Ok((StatusCode::ACCEPTED, Json(Value::Null)))
}

/// `POST /api/v1/auth/verify`
///
/// Validates the verify token, sets `is_verified=true`, and returns the updated UserRead.
/// Auth: none.
async fn post_verify(
    State(state): State<AppState>,
    ValidatedJson(payload): ValidatedJson<VerifyPayloadDTO>,
) -> Result<Json<UserReadDTO>, ApiError> {
    let Some(auth) = state.auth.as_ref() else {
        return Err(ApiError::Internal(anyhow::anyhow!(
            "Auth context not configured"
        )));
    };

    let user = verify_user(&payload.token, auth).await?;

    Ok(Json(UserReadDTO {
        id: user.id,
        email: user.email,
        is_active: user.is_active,
        is_superuser: user.is_superuser,
        is_verified: user.is_verified,
        tenant_id: user.tenant_id,
        parent_user_id: user.parent_user_id,
    }))
}

// ─── Router ───────────────────────────────────────────────────────────────────

/// Router for the verify endpoints.
/// Must be nested under `/api/v1/auth` in `build_router`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/request-verify-token", post(post_request_verify_token))
        .route("/verify", post(post_verify))
}
