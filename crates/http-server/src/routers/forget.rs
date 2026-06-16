//! `POST /api/v1/forget` — unified deletion command.
//!
//! Python parity: `cognee/api/v1/forget/routers/get_forget_router.py`.
//! Rust delegation: `cognee_delete::DeleteService` (via `state.components()`).
//!
//! Three modes (selected by which fields are populated):
//!   - Mode 1: `data_id` + `dataset` → delete one data item.
//!   - Mode 2: `dataset` only → delete the entire dataset.
//!   - Mode 3: `everything=true` → delete everything the user owns.
//!
//! Python maps both cross-field validation errors *and* missing-dataset errors
//! to 422 with the same `{"error": "Invalid request parameters..."}` body.
//! We match that exactly.

use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use cognee_database::IngestDb;
use cognee_delete::{DeleteMode, DeleteRequest, DeleteScope};

use crate::auth::AuthenticatedUser;
use crate::cloud_client::CloudClientError;
use crate::dto::forget::{
    DatasetRef, ForgetDataItemResponse, ForgetDatasetResponse, ForgetEverythingResponse,
    ForgetMode, ForgetPayloadDTO, ForgetResponseDTO,
};
use crate::error::ApiError;
use crate::permissions::check_permission_via_handles;
use crate::state::AppState;

// ─── canonical error message (Python parity) ─────────────────────────────────

const INVALID_PARAMS_MSG: &str =
    "Invalid request parameters. Specify dataset, data_id+dataset, or everything=True.";

// ─── post_forget ─────────────────────────────────────────────────────────────

/// `POST /api/v1/forget` — Unified delete command (three modes).
pub async fn post_forget(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Json(payload): Json<ForgetPayloadDTO>,
) -> Result<Json<ForgetResponseDTO>, ApiError> {
    // ── Cross-field validation ────────────────────────────────────────────
    let mode = payload
        .resolve_mode()
        .map_err(|msg| ApiError::OntologyEnvelope(msg.into(), StatusCode::UNPROCESSABLE_ENTITY))?;

    crate::telemetry::emit(
        "Forget API Endpoint Invoked",
        user.id,
        serde_json::json!({ "endpoint": "POST /v1/forget" }),
    );

    // ── Resolve components ────────────────────────────────────────────────
    let components = state.components().ok_or_else(|| {
        ApiError::OntologyEnvelope(
            "An error occurred during deletion.".into(),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;

    let db = components.database.clone();
    let delete_service = components.delete_service.clone();

    if let Some(cloud_client) = components.cloud_client.as_ref() {
        return cloud_client
            .forward_forget(&payload, &user)
            .await
            .map(Json)
            .map_err(map_cloud_error);
    }

    match mode {
        // ── Mode 1: delete one data item ──────────────────────────────────
        ForgetMode::DataItem => {
            let data_id = payload
                .data_id
                .expect("resolve_mode guarantees data_id is Some in DataItem mode");
            let dataset_ref = payload
                .dataset
                .as_ref()
                .expect("resolve_mode guarantees dataset is Some in DataItem mode");

            // Resolve dataset.
            let dataset = resolve_dataset(&db, user.id, user.tenant_id, dataset_ref)
                .await
                .map_err(|_| {
                    ApiError::OntologyEnvelope(
                        INVALID_PARAMS_MSG.into(),
                        StatusCode::UNPROCESSABLE_ENTITY,
                    )
                })?;

            check_permission_via_handles(components, user.id, dataset.id, "delete")
                .await
                .map_err(|_| {
                    ApiError::OntologyEnvelope(
                        INVALID_PARAMS_MSG.into(),
                        StatusCode::UNPROCESSABLE_ENTITY,
                    )
                })?;

            let request = DeleteRequest {
                scope: DeleteScope::Data {
                    owner_id: user.id,
                    data_id,
                    dataset_name: Some(dataset.name.clone()),
                    delete_dataset_if_empty: false,
                },
                mode: DeleteMode::Soft,
            };

            delete_service.execute(&request).await.map_err(|e| {
                tracing::error!(error = %e, "forget mode-1 delete failed");
                ApiError::OntologyEnvelope(
                    "An error occurred during deletion.".into(),
                    StatusCode::INTERNAL_SERVER_ERROR,
                )
            })?;

            Ok(Json(ForgetResponseDTO::DataItem(ForgetDataItemResponse {
                data_id,
                dataset_id: dataset.id,
                status: "success".into(),
            })))
        }

        // ── Mode 2: delete entire dataset ─────────────────────────────────
        ForgetMode::Dataset => {
            let dataset_ref = payload
                .dataset
                .as_ref()
                .expect("resolve_mode guarantees dataset is Some in Dataset mode");

            let dataset = resolve_dataset(&db, user.id, user.tenant_id, dataset_ref)
                .await
                .map_err(|_| {
                    ApiError::OntologyEnvelope(
                        INVALID_PARAMS_MSG.into(),
                        StatusCode::UNPROCESSABLE_ENTITY,
                    )
                })?;

            check_permission_via_handles(components, user.id, dataset.id, "delete")
                .await
                .map_err(|_| {
                    ApiError::OntologyEnvelope(
                        INVALID_PARAMS_MSG.into(),
                        StatusCode::UNPROCESSABLE_ENTITY,
                    )
                })?;

            let request = DeleteRequest {
                scope: DeleteScope::Dataset {
                    owner_id: user.id,
                    dataset_name: dataset.name.clone(),
                },
                mode: DeleteMode::Soft,
            };

            delete_service.execute(&request).await.map_err(|e| {
                tracing::error!(error = %e, "forget mode-2 delete failed");
                ApiError::OntologyEnvelope(
                    "An error occurred during deletion.".into(),
                    StatusCode::INTERNAL_SERVER_ERROR,
                )
            })?;

            Ok(Json(ForgetResponseDTO::Dataset(ForgetDatasetResponse {
                dataset_id: dataset.id,
                status: "success".into(),
            })))
        }

        // ── Mode 3: delete everything the user owns ───────────────────────
        ForgetMode::Everything => {
            // No upfront permission check — the delete service filters by owner.
            let datasets = IngestDb::list_datasets_by_owner(&*db, user.id)
                .await
                .map_err(|e| {
                    tracing::error!(error = %e, "forget mode-3 list datasets failed");
                    ApiError::OntologyEnvelope(
                        "An error occurred during deletion.".into(),
                        StatusCode::INTERNAL_SERVER_ERROR,
                    )
                })?;

            let mut removed = 0usize;
            for ds in &datasets {
                let request = DeleteRequest {
                    scope: DeleteScope::Dataset {
                        owner_id: user.id,
                        dataset_name: ds.name.clone(),
                    },
                    mode: DeleteMode::Soft,
                };
                match delete_service.execute(&request).await {
                    Ok(_) => removed += 1,
                    Err(e) => {
                        tracing::warn!(
                            dataset_id = %ds.id,
                            error = %e,
                            "forget mode-3: failed to delete dataset"
                        );
                    }
                }
            }

            Ok(Json(ForgetResponseDTO::Everything(
                ForgetEverythingResponse {
                    datasets_removed: removed,
                    status: "success".into(),
                },
            )))
        }
    }
}

// ─── resolve_dataset helper ───────────────────────────────────────────────────

async fn resolve_dataset(
    db: &cognee_database::DatabaseConnection,
    owner_id: uuid::Uuid,
    tenant_id: Option<uuid::Uuid>,
    dataset_ref: &DatasetRef,
) -> Result<cognee_models::Dataset, ()> {
    use cognee_database::IngestDb;
    match dataset_ref {
        DatasetRef::Id(id) => IngestDb::get_dataset(db, *id)
            .await
            .ok()
            .flatten()
            .ok_or(()),
        DatasetRef::Name(name) => IngestDb::get_dataset_by_name(db, name, owner_id, tenant_id)
            .await
            .ok()
            .flatten()
            .ok_or(()),
    }
}

// ─── router ──────────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new().route("/", post(post_forget))
}

fn map_cloud_error(error: CloudClientError) -> ApiError {
    let (message, status) = match error {
        CloudClientError::Upstream { .. } | CloudClientError::MalformedResponse => (
            "An error occurred during deletion.",
            StatusCode::BAD_GATEWAY,
        ),
        CloudClientError::Unreachable => (
            "Deletion service is unavailable.",
            StatusCode::SERVICE_UNAVAILABLE,
        ),
    };

    tracing::error!(error = ?error, "forget cloud proxy failed");
    ApiError::OntologyEnvelope(message.into(), status)
}
