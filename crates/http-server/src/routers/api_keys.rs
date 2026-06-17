//! `/api/v1/auth/api-keys` router (list / create / delete).
//!
//! Python source: `cognee/api/v1/api_keys/routers/get_api_key_management_router.py`.
//! Mounted at `/api/v1/auth/api-keys` in `build_router`.
//!
//! **Error envelope quirk**: this router uses `{"error": {"message": "..."}}` for
//! application-level errors, unlike all other routers which use `{"detail": "..."}`.
//! This mirrors Python's direct `JSONResponse(status_code=400, content={"error": ...})`.

use axum::{
    Json, Router,
    extract::{Path, State},
    routing::{delete, get},
};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    auth::{AuthenticatedUser, api_keys_service},
    dto::api_keys::{ApiKeyCreatedDTO, ApiKeyCreationPayloadDTO, ApiKeyListItemDTO},
    error::ApiError,
    middleware::validation::Json as ValidatedJson,
    state::AppState,
};

// ─── Masked-key sentinel ─────────────────────────────────────────────────────

/// The 12-asterisk sentinel returned when `HASH_API_KEY=true`.
/// This exact value is part of the wire contract.
const MASKED_KEY: &str = "************";

// ─── Handlers ────────────────────────────────────────────────────────────────

/// `GET /api/v1/auth/api-keys`
async fn get_list(
    State(state): State<AppState>,
    user: AuthenticatedUser,
) -> Result<Json<Vec<ApiKeyListItemDTO>>, ApiError> {
    let Some(auth) = state.auth.as_ref() else {
        return Ok(Json(vec![]));
    };

    crate::telemetry::emit(
        "Api Key Management API Endpoint Invoked",
        user.id,
        serde_json::json!({ "endpoint": "GET /v1/auth/api-keys" }),
    );

    let keys = api_keys_service::list(auth, user.id).await?;

    let items: Vec<ApiKeyListItemDTO> = keys
        .into_iter()
        .map(|k| ApiKeyListItemDTO {
            key: if auth.hash_api_key {
                MASKED_KEY.to_owned()
            } else {
                k.api_key.clone()
            },
            label: k
                .label
                .clone()
                .unwrap_or_else(|| k.api_key[..8].to_owned() + "****"),
            name: k.name,
            id: k.id,
        })
        .collect();

    Ok(Json(items))
}

/// `POST /api/v1/auth/api-keys`
async fn post_create(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    ValidatedJson(payload): ValidatedJson<ApiKeyCreationPayloadDTO>,
) -> Result<Json<ApiKeyCreatedDTO>, ApiError> {
    let Some(auth) = state.auth.as_ref() else {
        return Err(ApiError::Internal(anyhow::anyhow!(
            "Auth context not configured"
        )));
    };

    crate::telemetry::emit(
        "Api Key Management API Endpoint Invoked",
        user.id,
        serde_json::json!({ "endpoint": "POST /v1/auth/api-keys" }),
    );

    let new_key = api_keys_service::create(auth, user.id, payload.name).await?;

    Ok(Json(ApiKeyCreatedDTO {
        key: new_key.raw_key,
        label: new_key.label,
        name: new_key.name,
        id: new_key.id,
    }))
}

/// `DELETE /api/v1/auth/api-keys/{api_key_id}`
///
/// Returns 200 + null on success.
/// Python quirk: missing key (or wrong owner) returns 500, not 404.
async fn delete_one(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(api_key_id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let Some(auth) = state.auth.as_ref() else {
        return Err(ApiError::Internal(anyhow::anyhow!(
            "Auth context not configured"
        )));
    };

    crate::telemetry::emit(
        "Api Key Management API Endpoint Invoked",
        user.id,
        serde_json::json!({ "endpoint": "DELETE /v1/auth/api-keys" }),
    );

    api_keys_service::delete(auth, user.id, api_key_id).await?;

    Ok(Json(Value::Null))
}

// ─── Router ───────────────────────────────────────────────────────────────────

/// Router for the api-keys management endpoints.
/// Must be mounted at `/api/v1/auth/api-keys` in `build_router`
/// via `.nest("/auth/api-keys", api_keys::router())`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(get_list).post(post_create))
        .route("/{api_key_id}", delete(delete_one))
}
