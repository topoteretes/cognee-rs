//! Dataset CRUD router — 11 endpoints.
//!
//! Python parity: `cognee/api/v1/datasets/routers/get_datasets_router.py`.
//! Rust delegation: direct use of `cognee_database::*` traits and
//! `cognee_delete::DeleteService`.

use std::collections::HashMap;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{delete, get, post, put},
};
use cognee_database::{
    AclDb, DatasetConfigDb, DeleteDb, IngestDb, PipelineRunStatus as DbPipelineRunStatus,
};
use cognee_delete::{DeleteMode, DeleteRequest, DeleteScope};
use cognee_models::Dataset;
use uuid::Uuid;

use crate::auth::AuthenticatedUser;
use crate::dto::datasets::{
    DataDTO, DatasetCreationPayload, DatasetDTO, DatasetSchemaPayloadDTO, DatasetSchemaResponseDTO,
    DatasetStatusQuery,
};
use crate::error::ApiError;
use crate::permissions::check_permission_via_handles;
use crate::responses::raw_file::serve_local_file;
use crate::state::AppState;

// ─── helpers ─────────────────────────────────────────────────────────────────

fn dataset_to_dto(ds: &Dataset) -> DatasetDTO {
    DatasetDTO {
        id: ds.id,
        name: ds.name.clone(),
        created_at: ds.created_at,
        updated_at: ds.updated_at,
        owner_id: ds.owner_id,
    }
}

// ─── 2.1  GET /  list_datasets ───────────────────────────────────────────────

/// `GET /api/v1/datasets` — list all datasets the caller can read.
pub async fn list_datasets(
    user: AuthenticatedUser,
    State(state): State<AppState>,
) -> Result<axum::response::Response, ApiError> {
    crate::telemetry::emit(
        "Datasets API Endpoint Invoked",
        user.id,
        serde_json::json!({ "endpoint": "GET /v1/datasets" }),
    );

    let db = state
        .components()
        .ok_or_else(|| {
            ApiError::Teapot("Error retrieving datasets: components not initialized".into())
        })?
        .database
        .clone();

    // Use AclDb to get authorized dataset IDs, then load each one.
    let dataset_ids = db
        .authorized_dataset_ids_with_roles(user.id, "read")
        .await
        .map_err(|e| ApiError::Teapot(format!("Error retrieving datasets: {e}")))?;

    let mut datasets = Vec::new();
    for id in dataset_ids {
        if let Some(ds) = db
            .get_dataset(id)
            .await
            .map_err(|e| ApiError::Teapot(format!("Error retrieving datasets: {e}")))?
        {
            datasets.push(dataset_to_dto(&ds));
        }
    }

    // If no ACL rows exist (fresh DB), fall back to listing by owner.
    if datasets.is_empty() {
        let owned = IngestDb::list_datasets_by_owner(&*db, user.id)
            .await
            .map_err(|e| ApiError::Teapot(format!("Error retrieving datasets: {e}")))?;
        datasets = owned.iter().map(dataset_to_dto).collect();
    }

    let body = serde_json::to_string(&datasets)
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("serialization error: {e}")))?;
    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(body))
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("response build error: {e}")))
}

// ─── 2.2  GET /status ────────────────────────────────────────────────────────

/// `GET /api/v1/datasets/status` — pipeline status for one or more datasets.
pub async fn get_dataset_status(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Query(query): Query<DatasetStatusQuery>,
) -> Result<Json<HashMap<String, String>>, ApiError> {
    if query.dataset.is_empty() {
        return Ok(Json(HashMap::new()));
    }

    crate::telemetry::emit(
        "Datasets API Endpoint Invoked",
        user.id,
        serde_json::json!({
            "endpoint": "GET /v1/datasets/status",
            "datasets": query.dataset.iter().map(|d| d.to_string()).collect::<Vec<String>>(),
        }),
    );

    let components = state.components().ok_or_else(|| {
        ApiError::WriteEnvelopeError("components not initialized".into(), StatusCode::CONFLICT)
    })?;
    let db = components.database.clone();

    let mut result: HashMap<String, String> = HashMap::new();

    for &dataset_id in &query.dataset {
        // PermissionsRepository::user_can per tenants.md §5.1.
        // Silently skip datasets the caller can't read.
        let has_access = check_permission_via_handles(components, user.id, dataset_id, "read")
            .await
            .is_ok();
        if !has_access && crate::permissions::is_authorization_required() {
            continue;
        }

        match db
            .get_latest_pipeline_status("cognify_pipeline", dataset_id)
            .await
        {
            Ok(Some(status)) => {
                let wire = match status {
                    DbPipelineRunStatus::Initiated => "DATASET_PROCESSING_INITIATED",
                    DbPipelineRunStatus::Started => "DATASET_PROCESSING_STARTED",
                    DbPipelineRunStatus::Completed => "DATASET_PROCESSING_COMPLETED",
                    DbPipelineRunStatus::Errored => "DATASET_PROCESSING_ERRORED",
                };
                result.insert(dataset_id.to_string(), wire.to_owned());
            }
            Ok(None) => {} // no run yet — omit from result
            Err(e) => {
                return Err(ApiError::WriteEnvelopeError(
                    e.to_string(),
                    StatusCode::CONFLICT,
                ));
            }
        }
    }

    Ok(Json(result))
}

// ─── 2.3  GET /{dataset_id}/data ─────────────────────────────────────────────

/// `GET /api/v1/datasets/{dataset_id}/data` — list data items in a dataset.
pub async fn get_dataset_data(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(dataset_id): Path<Uuid>,
) -> Result<axum::response::Response, ApiError> {
    crate::telemetry::emit(
        "Datasets API Endpoint Invoked",
        user.id,
        serde_json::json!({
            "endpoint": format!("GET /v1/datasets/{}/data", dataset_id),
            "dataset_id": dataset_id.to_string(),
        }),
    );

    let components = state.components().ok_or_else(|| {
        ApiError::ErrorMessageError(
            format!("Dataset ({dataset_id}) not found."),
            StatusCode::NOT_FOUND,
        )
    })?;
    let db = components.database.clone();

    check_permission_via_handles(components, user.id, dataset_id, "read").await?;

    let raw_data = DeleteDb::get_dataset_data(&*db, dataset_id)
        .await
        .map_err(|_| {
            ApiError::ErrorMessageError(
                format!("Dataset ({dataset_id}) not found."),
                StatusCode::NOT_FOUND,
            )
        })?;

    let dtos: Vec<DataDTO> = raw_data
        .iter()
        .map(|d| DataDTO {
            id: d.id,
            name: d.name.clone(),
            created_at: d.created_at,
            updated_at: d.updated_at,
            extension: d.extension.clone(),
            mime_type: d.mime_type.clone(),
            raw_data_location: d.raw_data_location.clone(),
            dataset_id: None, // not easily available without a join
        })
        .collect();

    let body = serde_json::to_string(&dtos)
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("serialization error: {e}")))?;
    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(body))
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("response build error: {e}")))
}

// ─── 2.4  GET /{dataset_id}/data/{data_id}/raw ───────────────────────────────

/// `GET /api/v1/datasets/{dataset_id}/data/{data_id}/raw` — stream the raw file.
pub async fn get_raw_data(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path((dataset_id, data_id)): Path<(Uuid, Uuid)>,
) -> Result<axum::response::Response, ApiError> {
    crate::telemetry::emit(
        "Datasets API Endpoint Invoked",
        user.id,
        serde_json::json!({
            "endpoint": format!("GET /v1/datasets/{}/data/{}/raw", dataset_id, data_id),
            "dataset_id": dataset_id.to_string(),
            "data_id": data_id.to_string(),
        }),
    );

    let components = state
        .components()
        .ok_or_else(|| ApiError::NotFound(format!("Dataset ({dataset_id}) not found.")))?;
    let db = components.database.clone();

    check_permission_via_handles(components, user.id, dataset_id, "read").await?;

    let data = IngestDb::get_data(&*db, data_id)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("DB error: {e}")))?
        .ok_or_else(|| ApiError::NotFound(format!("Data ({data_id}) not found.")))?;

    let raw_location = &data.raw_data_location;

    // URI-scheme dispatch (matches Python's urlparse logic).
    let scheme = extract_scheme(raw_location);

    match scheme {
        "" | "file" => {
            // Local file.
            let local_path = strip_file_prefix(raw_location);
            let path = std::path::Path::new(local_path);
            let download_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&data.name);
            let mime = if data.mime_type.is_empty() {
                mime_guess::from_path(path)
                    .first_or_octet_stream()
                    .to_string()
            } else {
                data.mime_type.clone()
            };
            serve_local_file(path, download_name, &mime).await
        }
        "s3" => {
            // S3 — not yet implemented.
            Err(ApiError::NotImplemented(
                "Storage scheme 's3' not supported for direct download.".into(),
            ))
        }
        other => Err(ApiError::NotImplemented(format!(
            "Storage scheme '{other}' not supported for direct download."
        ))),
    }
}

fn extract_scheme(uri: &str) -> &str {
    if let Some(pos) = uri.find("://") {
        let scheme = &uri[..pos];
        // Single-letter scheme = Windows drive letter — treat as local.
        if scheme.len() <= 1 { "" } else { scheme }
    } else {
        ""
    }
}

fn strip_file_prefix(uri: &str) -> &str {
    uri.strip_prefix("file://").unwrap_or(uri)
}

// ─── 2.5  GET /{dataset_id}/graph ────────────────────────────────────────────

/// `GET /api/v1/datasets/{dataset_id}/graph` — rendered knowledge graph.
///
/// Returns `200 OK` with the JSON shape
/// `{"nodes": [{id, label, type, properties}, ...], "edges": [{source, target, label}, ...]}`.
///
/// When the `graph_db` handle is not wired (e.g. test mode), the response is
/// the same shape with empty arrays — `{"nodes": [], "edges": []}` — to
/// preserve the wire contract for clients that never need to distinguish a
/// truly-empty graph from "backend not configured".
pub async fn get_dataset_graph(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(dataset_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // When backends are not wired (test mode), return the empty-graph
    // fallback so the response shape is stable. Mirrors `get_dataset_schema`.
    let Some(components) = state.components() else {
        return Ok(Json(serde_json::json!({"nodes": [], "edges": []})));
    };

    // Permission gate — mirrors the `/schema` endpoint above.
    if check_permission_via_handles(components, user.id, dataset_id, "read")
        .await
        .is_err()
    {
        return Err(ApiError::WriteEnvelopeError(
            "Dataset not found".into(),
            StatusCode::NOT_FOUND,
        ));
    }

    let snapshot = components
        .formatted_graph_data(Some(dataset_id), user.id)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("graph render failed: {e}")))?;

    Ok(Json(snapshot))
}

// ─── 2.6  GET /{dataset_id}/schema ───────────────────────────────────────────

/// `GET /api/v1/datasets/{dataset_id}/schema` — read graph schema + custom prompt.
pub async fn get_dataset_schema(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(dataset_id): Path<Uuid>,
) -> Result<Json<DatasetSchemaResponseDTO>, ApiError> {
    let components = state.components().ok_or_else(|| {
        ApiError::WriteEnvelopeError("Dataset not found".into(), StatusCode::NOT_FOUND)
    })?;

    check_permission_via_handles(components, user.id, dataset_id, "read")
        .await
        .map_err(|_| {
            ApiError::WriteEnvelopeError("Dataset not found".into(), StatusCode::NOT_FOUND)
        })?;

    let config = DatasetConfigDb::get_by_dataset_id(&*components.database, dataset_id)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("DB error: {e}")))?;

    Ok(Json(DatasetSchemaResponseDTO {
        graph_schema: config.as_ref().and_then(|c| c.graph_schema.clone()),
        custom_prompt: config.and_then(|c| c.custom_prompt),
    }))
}

// ─── 2.7  POST /  create_new_dataset ─────────────────────────────────────────

/// `POST /api/v1/datasets` — create a dataset (or return existing by name).
pub async fn create_new_dataset(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Json(payload): Json<DatasetCreationPayload>,
) -> Result<Json<DatasetDTO>, ApiError> {
    crate::telemetry::emit(
        "Datasets API Endpoint Invoked",
        user.id,
        serde_json::json!({ "endpoint": "POST /v1/datasets" }),
    );

    let db = state
        .components()
        .ok_or_else(|| {
            ApiError::Teapot("Error creating dataset: components not initialized".into())
        })?
        .database
        .clone();

    // Check if a dataset with this name already exists for the user.
    let existing = IngestDb::get_dataset_by_name(&*db, &payload.name, user.id, user.tenant_id)
        .await
        .map_err(|e| ApiError::Teapot(format!("Error creating dataset: {e}")))?;

    if let Some(ds) = existing {
        return Ok(Json(dataset_to_dto(&ds)));
    }

    // Create a new dataset.
    let new_id = cognee_ingestion::generate_dataset_id(&payload.name, user.id, user.tenant_id);
    let dataset = Dataset::new(payload.name, user.id, user.tenant_id, new_id);
    let created = db
        .create_dataset(dataset)
        .await
        .map_err(|e| ApiError::Teapot(format!("Error creating dataset: {e}")))?;

    // Grant read+write+share+delete ACLs to the owner.
    for perm in &["read", "write", "share", "delete"] {
        // Ensure principal exists first.
        let _ = db.ensure_principal(user.id, "user").await;
        if let Err(e) = db.grant_permission(user.id, created.id, perm).await {
            tracing::warn!("Failed to grant {perm} on dataset {}: {e}", created.id);
        }
    }

    Ok(Json(dataset_to_dto(&created)))
}

// ─── 2.8  PUT /{dataset_id}/schema ───────────────────────────────────────────

/// `PUT /api/v1/datasets/{dataset_id}/schema` — upsert graph schema + custom prompt.
pub async fn update_dataset_schema(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(dataset_id): Path<Uuid>,
    Json(payload): Json<DatasetSchemaPayloadDTO>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let components = state.components().ok_or_else(|| {
        ApiError::WriteEnvelopeError("Dataset not found".into(), StatusCode::NOT_FOUND)
    })?;

    check_permission_via_handles(components, user.id, dataset_id, "write")
        .await
        .map_err(|_| {
            ApiError::WriteEnvelopeError("Dataset not found".into(), StatusCode::NOT_FOUND)
        })?;

    let patch = cognee_database::DatasetConfigurationPatch {
        graph_schema: payload.graph_schema,
        custom_prompt: payload.custom_prompt,
    };

    DatasetConfigDb::upsert(&*components.database, dataset_id, patch)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("DB error: {e}")))?;

    Ok(Json(serde_json::json!({"status": "ok"})))
}

// ─── 2.9  DELETE /  delete_all_datasets ──────────────────────────────────────

/// `DELETE /api/v1/datasets` — delete every dataset the caller owns.
pub async fn delete_all_datasets(
    user: AuthenticatedUser,
    State(state): State<AppState>,
) -> Result<Json<Option<()>>, ApiError> {
    let components = state
        .components()
        .ok_or_else(|| ApiError::Internal(anyhow::anyhow!("components not initialized")))?;

    let db = components.database.clone();
    let delete_service = components.delete_service.clone();

    let datasets = IngestDb::list_datasets_by_owner(&*db, user.id)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("DB error: {e}")))?;

    for ds in datasets {
        let request = DeleteRequest {
            scope: DeleteScope::Dataset {
                owner_id: user.id,
                dataset_name: ds.name,
            },
            mode: DeleteMode::Hard,
        };
        if let Err(e) = delete_service.execute(&request).await {
            tracing::warn!("Failed to delete dataset {}: {e}", ds.id);
        }
    }

    Ok(Json(None))
}

// ─── 2.10  DELETE /{dataset_id}  delete_dataset ──────────────────────────────

/// `DELETE /api/v1/datasets/{dataset_id}` — empty (delete) one dataset.
pub async fn delete_dataset(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(dataset_id): Path<Uuid>,
) -> Result<Json<Option<()>>, ApiError> {
    crate::telemetry::emit(
        "Datasets API Endpoint Invoked",
        user.id,
        serde_json::json!({
            "endpoint": format!("DELETE /v1/datasets/{}", dataset_id),
            "dataset_id": dataset_id.to_string(),
        }),
    );

    let components = state
        .components()
        .ok_or_else(|| ApiError::NotFound(format!("Dataset ({dataset_id}) not accessible.")))?;

    let db = components.database.clone();
    let delete_service = components.delete_service.clone();

    check_permission_via_handles(components, user.id, dataset_id, "delete")
        .await
        .map_err(|_| ApiError::NotFound(format!("Dataset ({dataset_id}) not accessible.")))?;

    let dataset = db
        .get_dataset(dataset_id)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("DB error: {e}")))?
        .ok_or_else(|| ApiError::NotFound(format!("Dataset ({dataset_id}) not accessible.")))?;

    let request = DeleteRequest {
        scope: DeleteScope::Dataset {
            owner_id: user.id,
            dataset_name: dataset.name,
        },
        mode: DeleteMode::Hard,
    };
    delete_service
        .execute(&request)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("delete error: {e}")))?;

    Ok(Json(None))
}

// ─── 2.11  DELETE /{dataset_id}/data/{data_id} ───────────────────────────────

/// `DELETE /api/v1/datasets/{dataset_id}/data/{data_id}` — delete one data item.
pub async fn delete_data_item(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path((dataset_id, data_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::telemetry::emit(
        "Datasets API Endpoint Invoked",
        user.id,
        serde_json::json!({
            "endpoint": format!("DELETE /v1/datasets/{}/data/{}", dataset_id, data_id),
            "dataset_id": dataset_id.to_string(),
            "data_id": data_id.to_string(),
        }),
    );

    let components = state
        .components()
        .ok_or_else(|| ApiError::NotFound(format!("Dataset/Data ({data_id}) not accessible.")))?;

    let db = components.database.clone();
    let delete_service = components.delete_service.clone();

    check_permission_via_handles(components, user.id, dataset_id, "delete")
        .await
        .map_err(|_| ApiError::NotFound(format!("Dataset/Data ({data_id}) not accessible.")))?;

    let dataset = db
        .get_dataset(dataset_id)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("DB error: {e}")))?
        .ok_or_else(|| ApiError::NotFound(format!("Dataset/Data ({data_id}) not accessible.")))?;

    let request = DeleteRequest {
        scope: DeleteScope::Data {
            owner_id: user.id,
            data_id,
            dataset_name: Some(dataset.name),
            delete_dataset_if_empty: false,
        },
        mode: DeleteMode::Soft,
    };
    delete_service
        .execute(&request)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("delete error: {e}")))?;

    Ok(Json(serde_json::json!({"status": "success"})))
}

// ─── router ──────────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new()
        // 2.1 list datasets
        .route("/", get(list_datasets))
        // 2.7 create dataset
        .route("/", post(create_new_dataset))
        // 2.9 delete all datasets
        .route("/", delete(delete_all_datasets))
        // 2.2 status (must come before /{dataset_id} to avoid conflict)
        .route("/status", get(get_dataset_status))
        // 2.3 list data in dataset
        .route("/{dataset_id}/data", get(get_dataset_data))
        // 2.4 raw download
        .route("/{dataset_id}/data/{data_id}/raw", get(get_raw_data))
        // 2.5 graph
        .route("/{dataset_id}/graph", get(get_dataset_graph))
        // 2.6 schema GET
        .route("/{dataset_id}/schema", get(get_dataset_schema))
        // 2.8 schema PUT
        .route("/{dataset_id}/schema", put(update_dataset_schema))
        // 2.10 delete one dataset
        .route("/{dataset_id}", delete(delete_dataset))
        // 2.11 delete one data item
        .route("/{dataset_id}/data/{data_id}", delete(delete_data_item))
}
