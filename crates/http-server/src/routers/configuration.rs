//! `/api/v1/configuration` — per-user named JSON blobs.
//!
//! Three endpoints per `routers/configuration.md §2`:
//! - `GET /get_user_configuration/` — list all of caller's named blobs.
//! - `GET /get_user_configuration/{config_id}` — fetch one by ID. Returns
//!   `200 {}` on miss (Python parity).
//! - `POST /store_user_configuration` — upsert by `(owner_id, name)`.
//!   Returns `200` with body `null` (NOT 204).
//!
//! Storage: `principal_configuration` table created in P5 migration.

use axum::{
    Json, Router,
    extract::{Path, State},
    routing::{get, post},
};
use chrono::Utc;
use cognee_database::DatabaseConnection;
use cognee_database::entities::principal_configuration as pc;
use sea_orm::Set;
use sea_orm::prelude::*;
use uuid::Uuid;

use crate::auth::AuthenticatedUser;
use crate::dto::configuration::{PrincipalConfigurationDTO, StorePrincipalConfigurationPayloadDTO};
use crate::error::ApiError;
use crate::state::AppState;

#[allow(clippy::result_large_err)]
fn db_from(state: &AppState) -> Result<&DatabaseConnection, ApiError> {
    Ok(state
        .components()
        .ok_or_else(|| ApiError::Internal(anyhow::anyhow!("components not initialized")))?
        .database
        .as_ref())
}

fn to_hex(u: Uuid) -> String {
    u.simple().to_string()
}

#[allow(clippy::result_large_err)]
fn from_hex(s: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(s).map_err(|e| ApiError::Internal(anyhow::anyhow!("invalid uuid hex: {e}")))
}

#[allow(clippy::result_large_err)]
fn model_to_dto(m: pc::Model) -> Result<PrincipalConfigurationDTO, ApiError> {
    Ok(PrincipalConfigurationDTO {
        id: from_hex(&m.id)?,
        owner_id: from_hex(&m.owner_id)?,
        name: m.name,
        configuration: m.configuration,
        created_at: m.created_at,
        updated_at: m.updated_at,
    })
}

#[tracing::instrument(skip(state), name = "cognee.api.configuration.list")]
pub async fn list_user_configurations(
    user: AuthenticatedUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<PrincipalConfigurationDTO>>, ApiError> {
    let db = db_from(&state)?;
    let owner_hex = to_hex(user.id);
    let rows = pc::Entity::find()
        .filter(pc::Column::OwnerId.eq(owner_hex))
        .all(db)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("DB error: {e}")))?;
    let body = rows
        .into_iter()
        .map(model_to_dto)
        .collect::<Result<_, _>>()?;
    Ok(Json(body))
}

#[tracing::instrument(skip(state), name = "cognee.api.configuration.get")]
pub async fn get_user_configuration(
    _user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(config_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db = db_from(&state)?;
    // NOTE: Python parity bug — no owner_id check here. Cross-user reads are
    // permitted. See `routers/configuration.md §6.1`.
    let row = pc::Entity::find_by_id(to_hex(config_id))
        .one(db)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("DB error: {e}")))?;
    match row {
        Some(m) => Ok(Json(m.configuration)),
        // Python returns `200 {}` on miss (NOT 404).
        None => Ok(Json(serde_json::json!({}))),
    }
}

#[tracing::instrument(skip(state, payload), name = "cognee.api.configuration.store")]
pub async fn store_user_configuration(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Json(payload): Json<StorePrincipalConfigurationPayloadDTO>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let trimmed = payload.name.trim();
    if trimmed.is_empty() {
        return Err(ApiError::BadRequest("Configuration name is empty".into()));
    }
    if !payload.config.is_object() {
        return Err(ApiError::BadRequest(
            "Configuration body must be a JSON object".into(),
        ));
    }

    let db = db_from(&state)?;
    let owner_hex = to_hex(user.id);

    // Python's SELECT-then-UPDATE-or-INSERT (no unique index per spec §6.2).
    let existing = pc::Entity::find()
        .filter(pc::Column::OwnerId.eq(owner_hex.clone()))
        .filter(pc::Column::Name.eq(trimmed))
        .one(db)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("DB error: {e}")))?;

    let now = Utc::now();
    if let Some(row) = existing {
        let mut active: pc::ActiveModel = row.into();
        active.configuration = Set(payload.config);
        active.updated_at = Set(Some(now));
        active
            .update(db)
            .await
            .map_err(|e| ApiError::Internal(anyhow::anyhow!("DB error: {e}")))?;
    } else {
        let model = pc::ActiveModel {
            id: Set(to_hex(Uuid::new_v4())),
            owner_id: Set(owner_hex),
            name: Set(trimmed.to_string()),
            configuration: Set(payload.config),
            created_at: Set(now),
            updated_at: Set(None),
        };
        pc::Entity::insert(model)
            .exec(db)
            .await
            .map_err(|e| ApiError::Internal(anyhow::anyhow!("DB error: {e}")))?;
    }

    // Python parity: 200 with body `null` (NOT 204).
    Ok(Json(serde_json::Value::Null))
}

pub fn router() -> Router<AppState> {
    Router::new()
        // Trailing-slash matters per `routers/configuration.md §2.1`.
        .route("/get_user_configuration/", get(list_user_configurations))
        .route(
            "/get_user_configuration/{config_id}",
            get(get_user_configuration),
        )
        .route("/store_user_configuration", post(store_user_configuration))
}
