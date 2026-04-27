//! `/api/v1/users` router (me / by-id CRUD).
//!
//! Python source: fastapi-users users router + cognee customizations.
//! Mounted under `/api/v1/users` in `build_router`.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::get,
};
use uuid::Uuid;

use crate::{
    auth::{AuthenticatedUser, RequireSuperuser, users_service},
    dto::users::{UserReadDTO, UserUpdatePayloadDTO},
    error::ApiError,
    middleware::validation::Json as ValidatedJson,
    state::AppState,
};

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn to_dto(user: cognee_database::AuthUser) -> UserReadDTO {
    UserReadDTO {
        id: user.id,
        email: user.email,
        is_active: user.is_active,
        is_superuser: user.is_superuser,
        is_verified: user.is_verified,
        tenant_id: user.tenant_id,
    }
}

// ─── Handlers ────────────────────────────────────────────────────────────────

/// `GET /api/v1/users/me`
///
/// Returns the caller's full `UserReadDTO`. Auth: required.
async fn get_me(user: AuthenticatedUser) -> Json<UserReadDTO> {
    Json(UserReadDTO {
        id: user.id,
        email: user.email,
        is_active: user.is_active,
        is_superuser: user.is_superuser,
        is_verified: user.is_verified,
        tenant_id: user.tenant_id,
    })
}

/// `PATCH /api/v1/users/me`
///
/// Updates the caller's own record with `safe=True` semantics (drops is_active/is_superuser/is_verified).
/// Auth: required.
async fn patch_me(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    ValidatedJson(payload): ValidatedJson<UserUpdatePayloadDTO>,
) -> Result<Json<UserReadDTO>, ApiError> {
    let Some(auth) = state.auth.as_ref() else {
        return Err(ApiError::Internal(anyhow::anyhow!(
            "Auth context not configured"
        )));
    };

    // Fetch current user row for the update call
    let current = users_service::get_by_id(auth, user.id).await?;

    let updated = users_service::update(
        auth,
        &current,
        payload.email,
        payload.password,
        None, // safe=True: drop is_active
        None, // safe=True: drop is_superuser
        None, // safe=True: drop is_verified
        true, // safe=True
    )
    .await?;

    Ok(Json(to_dto(updated)))
}

/// `GET /api/v1/users/{id}`
///
/// Returns a user by ID. Superuser only.
async fn get_by_id(
    State(state): State<AppState>,
    RequireSuperuser(_caller): RequireSuperuser,
    Path(id): Path<Uuid>,
) -> Result<Json<UserReadDTO>, ApiError> {
    let Some(auth) = state.auth.as_ref() else {
        return Err(ApiError::Internal(anyhow::anyhow!(
            "Auth context not configured"
        )));
    };

    let user = users_service::get_by_id(auth, id).await?;
    Ok(Json(to_dto(user)))
}

/// `PATCH /api/v1/users/{id}`
///
/// Updates a user with `safe=False` semantics (allows is_active/is_superuser/is_verified).
/// Superuser only.
async fn patch_by_id(
    State(state): State<AppState>,
    RequireSuperuser(_caller): RequireSuperuser,
    Path(id): Path<Uuid>,
    ValidatedJson(payload): ValidatedJson<UserUpdatePayloadDTO>,
) -> Result<Json<UserReadDTO>, ApiError> {
    let Some(auth) = state.auth.as_ref() else {
        return Err(ApiError::Internal(anyhow::anyhow!(
            "Auth context not configured"
        )));
    };

    let current = users_service::get_by_id(auth, id).await?;

    let updated = users_service::update(
        auth,
        &current,
        payload.email,
        payload.password,
        payload.is_active,    // safe=False: allowed
        payload.is_superuser, // safe=False: allowed
        payload.is_verified,  // safe=False: allowed
        false,                // safe=False
    )
    .await?;

    Ok(Json(to_dto(updated)))
}

/// `DELETE /api/v1/users/{id}`
///
/// Deletes a user. Returns 204 No Content.
/// Superuser only. Rejects the default user.
async fn delete_by_id(
    State(state): State<AppState>,
    RequireSuperuser(_caller): RequireSuperuser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let Some(auth) = state.auth.as_ref() else {
        return Err(ApiError::Internal(anyhow::anyhow!(
            "Auth context not configured"
        )));
    };

    users_service::delete_by_id(auth, id).await?;

    Ok(StatusCode::NO_CONTENT)
}

// ─── Router ───────────────────────────────────────────────────────────────────

/// Router for the users CRUD endpoints.
/// Must be nested under `/api/v1/users` in `build_router`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/me", get(get_me).patch(patch_me))
        .route(
            "/{id}",
            get(get_by_id).patch(patch_by_id).delete(delete_by_id),
        )
}
