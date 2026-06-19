//! `POST /api/v1/auth/register` handler.
//!
//! Python source: `cognee/api/v1/users/routers/get_register_router.py`
//! (fastapi-users register router). Mounted under `/api/v1/auth` in `build_router`.

use axum::{Router, extract::State, http::StatusCode, routing::post};

use crate::{
    auth::register::create_user,
    dto::{auth_register::RegisterPayloadDTO, users::UserReadDTO},
    error::ApiError,
    middleware::validation::Json,
    state::AppState,
};

// ─── Handler ──────────────────────────────────────────────────────────────────

/// `POST /api/v1/auth/register`
///
/// Creates a new user and returns `UserReadDTO` on success.
/// Auth: none.
async fn post_register(
    State(state): State<AppState>,
    Json(payload): Json<RegisterPayloadDTO>,
) -> Result<(StatusCode, axum::Json<UserReadDTO>), ApiError> {
    let Some(auth) = state.auth.as_ref() else {
        return Err(ApiError::Internal(anyhow::anyhow!(
            "Auth context not configured"
        )));
    };

    let mailer = state.mailer.as_ref();

    let user = create_user(&payload.email, &payload.password, mailer, auth).await?;

    let dto = UserReadDTO {
        id: user.id,
        email: user.email,
        is_active: user.is_active,
        is_superuser: user.is_superuser,
        is_verified: user.is_verified,
        tenant_id: user.tenant_id,
        parent_user_id: user.parent_user_id,
    };

    Ok((StatusCode::CREATED, axum::Json(dto)))
}

// ─── Router ───────────────────────────────────────────────────────────────────

/// Router for the register endpoint.
/// Must be nested under `/api/v1/auth` in `build_router`.
pub fn router() -> Router<AppState> {
    Router::new().route("/register", post(post_register))
}
