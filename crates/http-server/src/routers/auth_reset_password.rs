//! `POST /api/v1/auth/forgot-password` and `POST /api/v1/auth/reset-password` handlers.
//!
//! Python source: fastapi-users reset-password router.
//! Mounted under `/api/v1/auth` in `build_router`.
//!
//! Both endpoints always return 202 + `null` body regardless of whether the
//! email/token is valid, to prevent user-existence enumeration.

use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use serde_json::Value;

use crate::{
    auth::reset::{forgot_password, reset_password},
    dto::auth_reset_password::{ForgotPasswordPayloadDTO, ResetPasswordPayloadDTO},
    error::ApiError,
    middleware::validation::Json as ValidatedJson,
    state::AppState,
};

// ─── Handlers ────────────────────────────────────────────────────────────────

/// `POST /api/v1/auth/forgot-password`
///
/// Sends a reset email (via Mailer) to the given address if a matching active
/// user exists. Always returns 202 + null to prevent enumeration.
/// Auth: none.
async fn post_forgot_password(
    State(state): State<AppState>,
    ValidatedJson(payload): ValidatedJson<ForgotPasswordPayloadDTO>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let Some(auth) = state.auth.as_ref() else {
        return Ok((StatusCode::ACCEPTED, Json(Value::Null)));
    };

    let mailer = state.mailer.as_ref();
    forgot_password(&payload.email, mailer, auth).await?;

    Ok((StatusCode::ACCEPTED, Json(Value::Null)))
}

/// `POST /api/v1/auth/reset-password`
///
/// Validates the reset token and sets the new password.
/// Returns 200 + null on success; 400 on bad/expired token or weak password.
/// Auth: none.
async fn post_reset_password(
    State(state): State<AppState>,
    ValidatedJson(payload): ValidatedJson<ResetPasswordPayloadDTO>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let Some(auth) = state.auth.as_ref() else {
        return Err(ApiError::Internal(anyhow::anyhow!(
            "Auth context not configured"
        )));
    };

    reset_password(&payload.token, &payload.password, auth).await?;

    Ok((StatusCode::OK, Json(Value::Null)))
}

// ─── Router ───────────────────────────────────────────────────────────────────

/// Router for the reset-password endpoints.
/// Must be nested under `/api/v1/auth` in `build_router`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/forgot-password", post(post_forgot_password))
        .route("/reset-password", post(post_reset_password))
}
