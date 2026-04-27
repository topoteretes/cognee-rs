//! `POST /api/v1/users/get-user-id` handler.
//!
//! Resolves an email address to a user's UUID.
//! Python source: `cognee/api/v1/users/routers/get_user_id_by_email_router.py`.
//! Mounted under `/api/v1/users` in `build_router`.

use axum::{Json, Router, extract::State, routing::post};

use crate::{
    auth::AuthenticatedUser,
    dto::users_by_email::{GetUserIdPayloadDTO, GetUserIdResponseDTO},
    error::ApiError,
    middleware::validation::Json as ValidatedJson,
    state::AppState,
};

// ─── Handler ──────────────────────────────────────────────────────────────────

/// `POST /api/v1/users/get-user-id`
///
/// Returns `{"user_id": "<uuid>"}` on hit, 404 on miss.
/// Auth: required (any active user — no superuser gate).
async fn post_get_user_id(
    State(state): State<AppState>,
    _user: AuthenticatedUser,
    ValidatedJson(body): ValidatedJson<GetUserIdPayloadDTO>,
) -> Result<Json<GetUserIdResponseDTO>, ApiError> {
    let Some(auth) = state.auth.as_ref() else {
        return Err(ApiError::NotFound("User not found".to_owned()));
    };

    let user_id = auth
        .user_repo
        .find_id_by_email(&body.email)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?
        .ok_or_else(|| ApiError::NotFound("User not found".to_owned()))?;

    Ok(Json(GetUserIdResponseDTO { user_id }))
}

// ─── Router ───────────────────────────────────────────────────────────────────

/// Router for the get-user-id endpoint.
/// Must be nested under `/api/v1/users` in `build_router`.
pub fn router() -> Router<AppState> {
    Router::new().route("/get-user-id", post(post_get_user_id))
}
