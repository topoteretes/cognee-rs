//! `POST /api/v1/add` — ingest files into a dataset.
//!
//! Python parity: `cognee/api/v1/add/routers/get_add_router.py`.
//! Rust delegation: `cognee_ingestion::AddPipeline::add`.

use std::sync::Arc;

use axum::{
    Router,
    extract::{Multipart, State},
    http::StatusCode,
    routing::post,
};
use cognee_database::{IngestDb, NoopPipelineRunRepository};
use cognee_ingestion::{AddParams, AddPipeline, IngestionError};
use cognee_models::DataInput;
use serde_json::json;
use uuid::Uuid;

use crate::auth::AuthenticatedUser;
use crate::dto::add::{AddRequest, DataIngestionInfoDTO, PipelineRunInfoDTO, UploadedPart};
use crate::error::ApiError;
use crate::multipart::{MultipartOpts, UploadGuard, check_filename_traversal, parse_multipart};
use crate::state::AppState;

// ─── classify_add_error ───────────────────────────────────────────────────────

/// Status and `error` label for a failed `add` pipeline run.
///
/// Everything defaults to 500 / `"Pipeline run errored"`, except the cases that
/// are the *client's* fault. Today that is
/// `IngestionError::UnsupportedDocumentType`, raised at loader dispatch when no
/// loader is registered for the input's derived type — posting a PDF to a build
/// without a `pdf-*` feature, say. Reporting that as 500 told the caller the
/// server had broken when in fact the payload was unsupported.
///
/// # This is a deliberate divergence from Python, not a parity match
///
/// An earlier version of this comment claimed 415 "matches Python". It does
/// not, and the distinction is worth spelling out because the surface looks
/// like it should.
///
/// Python *defines* the right status — `IngestionError` overrides the 422 of
/// its `CogneeValidationError` base with `HTTP_415_UNSUPPORTED_MEDIA_TYPE`
/// (`cognee/modules/ingestion/exceptions/exceptions.py:10` against
/// `cognee/exceptions/exceptions.py:59`) — and `client.py`'s
/// `@app.exception_handler(CogneeApiError)` would honour it. But the add route
/// never lets it get there: `get_add_router.py` wraps the whole `cognee_add`
/// call in `except Exception` and returns
/// `500 {"error": "Internal server error", …}`, and its `PipelineRunErrored`
/// branch is likewise hard-coded to 500. Every `IngestionError` raise site sits
/// inside that `try`, and the route's OpenAPI `responses` map declares only
/// 400/403/422/500. So on the wire Python answers 500 here.
///
/// We answer 415 anyway: a media type this build cannot load is the caller's
/// problem, and 500 tells them to retry or page someone for a condition no
/// retry can fix. Python having written 415 into the exception and then
/// swallowed it reads as an oversight rather than an intended contract.
///
/// The cost is real and accepted: a client branching on 4xx-vs-5xx behaves
/// differently against the two servers. `e2e-cross-sdk` pins the divergence
/// with a `strict=True` xfail
/// (`test_add_unsupported_media_type_status_diverges`) so it is tracked, and so
/// that an XPASS tells us Python changed its handling.
///
/// # Chain walking
///
/// The error arrives wrapped — `ExecutionError::TaskFailed` carries the task's
/// error as its `#[source]` — so this walks the whole source chain rather than
/// inspecting only the outermost error. That chain is intact only because
/// `make_process_input_task` passes `IngestionError` through as a typed error
/// instead of flattening it to a string; see the note there.
fn classify_add_error(err: &(dyn std::error::Error + 'static)) -> (StatusCode, &'static str) {
    let mut current = Some(err);
    while let Some(e) = current {
        if let Some(IngestionError::UnsupportedDocumentType { .. }) =
            e.downcast_ref::<IngestionError>()
        {
            // The label moves with the status: a 415 carrying the 5xx
            // "Pipeline run errored" envelope would be internally inconsistent.
            return (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "Unsupported document type",
            );
        }
        current = e.source();
    }
    (StatusCode::INTERNAL_SERVER_ERROR, "Pipeline run errored")
}

// ─── parse_add_multipart ──────────────────────────────────────────────────────

/// Parse the multipart body into an [`AddRequest`].
///
/// Applies per-router validation:
/// - `data` parts: spool to disk; detect URL/S3 bodies.
/// - `datasetName` / `datasetId`: trim, normalize empty → `None`.
/// - `node_set`: collect repetitions; normalize `[""]` and `[]` → `None`.
async fn parse_add_multipart(
    multipart: Multipart,
    request_id: &str,
) -> Result<(AddRequest, UploadGuard), ApiError> {
    let opts = MultipartOpts::default();
    let parsed = parse_multipart(multipart, &opts, request_id).await?;
    let guard = UploadGuard::new(parsed.spool_dir.clone());

    let mut files: Vec<UploadedPart> = Vec::new();
    let mut dataset_name: Option<String> = None;
    let mut dataset_id: Option<Uuid> = None;
    let mut node_set_values: Vec<String> = Vec::new();

    // ── form fields ──────────────────────────────────────────────────────────
    // The HTTP form fields are camelCase (`datasetName` / `datasetId`), matching
    // the Python `get_add_router` surface. (The snake_case `dataset_name` is only
    // the SDK-internal parameter name, not the HTTP field.)
    if let Some(vals) = parsed.fields.get("datasetName")
        && let Some(v) = vals.first()
    {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            dataset_name = Some(trimmed.to_owned());
        }
    }
    if let Some(vals) = parsed.fields.get("datasetId")
        && let Some(v) = vals.first()
    {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            dataset_id = Some(
                Uuid::parse_str(trimmed)
                    .map_err(|_| ApiError::BadRequest("Invalid datasetId UUID".into()))?,
            );
        }
    }
    if let Some(vals) = parsed.fields.get("node_set") {
        node_set_values = vals.clone();
    }

    // ── file/data parts ───────────────────────────────────────────────────────
    if let Some(spooled) = parsed.files.get("data") {
        for sf in spooled {
            // Validate filename for traversal attempts.
            if let Some(ref fname) = sf.filename {
                check_filename_traversal(fname)?;
            }

            // Check if the part body is a URL/S3 string (< 4 KiB, valid scheme).
            let url_payload = if sf.byte_count < 4096 {
                // Read back the spooled bytes to check for URL scheme.
                let bytes = tokio::fs::read(&sf.path)
                    .await
                    .map_err(|e| ApiError::Internal(anyhow::anyhow!("spool read error: {e}")))?;
                if let Ok(s) = std::str::from_utf8(&bytes) {
                    let s = s.trim();
                    if s.starts_with("http://")
                        || s.starts_with("https://")
                        || s.starts_with("s3://")
                    {
                        // Remove the temp file — we'll use the URL directly.
                        let _ = tokio::fs::remove_file(&sf.path).await;
                        Some(s.to_owned())
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };

            files.push(UploadedPart {
                file_name: sf.filename.clone(),
                content_type: sf.content_type.clone(),
                temp_path: sf.path.clone(),
                byte_count: sf.byte_count,
                url_payload,
            });
        }
    }

    // ── node_set normalization ────────────────────────────────────────────────
    // `[""]` and `[]` both normalize to `None`.
    let normalized_node_set: Option<Vec<String>> = if node_set_values.is_empty()
        || (node_set_values.len() == 1 && node_set_values[0].is_empty())
    {
        None
    } else {
        let non_empty: Vec<String> = node_set_values
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect();
        if non_empty.is_empty() {
            None
        } else {
            Some(non_empty)
        }
    };

    Ok((
        AddRequest {
            files,
            dataset_name,
            dataset_id,
            node_set: normalized_node_set,
        },
        guard,
    ))
}

// ─── post_add handler ─────────────────────────────────────────────────────────

/// `POST /api/v1/add` — Ingest one or more files into a dataset.
pub async fn post_add(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    multipart: Multipart,
) -> Result<axum::response::Response, ApiError> {
    let request_id = Uuid::new_v4().to_string();
    let (req, _guard) = parse_add_multipart(multipart, &request_id).await?;

    // ── cross-field validation ────────────────────────────────────────────────
    if req.dataset_name.is_none() && req.dataset_id.is_none() {
        let body = json!({
            "error": "Either datasetId or datasetName must be provided.",
            "detail": null
        });
        let resp = axum::response::Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .header("Content-Type", "application/json")
            .body(axum::body::Body::from(
                #[allow(clippy::expect_used, reason = "invariant is upheld by construction")]
                serde_json::to_string(&body)
                    .expect("json serialization cannot fail for static structure"),
            ))
            .map_err(|e| ApiError::Internal(anyhow::anyhow!("response build error: {e}")))?;
        return Ok(resp);
    }

    // Resolve components from state.
    let Some(components) = state.components() else {
        return Err(ApiError::Internal(anyhow::anyhow!(
            "Component handles not initialized"
        )));
    };

    crate::telemetry::emit(
        "Add API Endpoint Invoked",
        user.id,
        json!({
            "endpoint": "POST /v1/add",
            "node_set": req.node_set,
        }),
    );

    let db = components.database.clone();
    let storage = components.storage.clone();

    // No upfront permission gate: add auto-creates datasets when missing.
    // Per-dataset write/share grants are issued by the ingest pipeline once
    // the dataset is materialised (PermissionsRepository resolution covers
    // subsequent endpoints).

    // Determine dataset name (prefer name over id).
    let dataset_name = if let Some(ref name) = req.dataset_name {
        name.clone()
    } else if let Some(dataset_id) = req.dataset_id {
        // Look up dataset name from DB.
        match db.get_dataset(dataset_id).await {
            Ok(Some(ds)) => ds.name,
            Ok(None) => {
                return Err(ApiError::NotFound(format!(
                    "Dataset {dataset_id} not found"
                )));
            }
            Err(e) => {
                return Err(ApiError::WriteEndpointError {
                    error: "Pipeline run errored".into(),
                    detail: Some(e.to_string()),
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                });
            }
        }
    } else {
        unreachable!("validated above")
    };

    // Build data inputs.
    let mut inputs: Vec<DataInput> = Vec::new();
    for part in &req.files {
        if let Some(ref url) = part.url_payload {
            inputs.push(DataInput::Url(url.clone()));
        } else {
            let path = part.temp_path.to_string_lossy().to_string();
            inputs.push(DataInput::FilePath(format!("file://{path}")));
        }
    }
    // If no files given, supply an empty dataset touch.
    if inputs.is_empty() {
        inputs.push(DataInput::Text(String::new()));
    }

    // Build and run the add pipeline.
    // LIB-06: `AddPipeline::add_with_params` now routes through
    // `cognee_core::pipeline::execute`, which requires graph/vector
    // backends + thread pool on the AddPipeline builder. Missing handles
    // surface as `ApiError::Internal` (the convenience function returns
    // `IngestionError::MissingBackend`).
    let Some(graph_db) = components.graph_db.clone() else {
        return Err(ApiError::Internal(anyhow::anyhow!(
            "graph_db not wired in ComponentHandles; cannot run add pipeline"
        )));
    };
    let Some(vector_db) = components.vector_db.clone() else {
        return Err(ApiError::Internal(anyhow::anyhow!(
            "vector_db not wired in ComponentHandles; cannot run add pipeline"
        )));
    };
    let Some(thread_pool) = components.thread_pool.clone() else {
        return Err(ApiError::Internal(anyhow::anyhow!(
            "thread_pool not wired in ComponentHandles; cannot run add pipeline"
        )));
    };

    // Gap 08-07: the HTTP path's `dispatch_pipeline` already wires a
    // `ScopedRunWatcher` via `DefaultPipelineRunRegistry` for the four-state
    // `pipeline_runs` trail. Hand `AddPipeline` a no-op repo so the inner
    // `DbPipelineWatcher` does not produce a second row-set.
    let mut pipeline = AddPipeline::new(storage, db.clone() as Arc<dyn IngestDb>)
        .with_thread_pool(thread_pool)
        .with_graph_db(graph_db)
        .with_vector_db(vector_db)
        .with_database(db.clone())
        .with_pipeline_run_repo(NoopPipelineRunRepository::arc());
    if let Some(acl) = components.acl_db.clone() {
        pipeline = pipeline.with_acl_db(acl);
    }

    let params = AddParams {
        node_set: req.node_set.clone(),
        ..AddParams::default()
    };

    // Classify before erasing: the typed error is only available here, and
    // `Box<dyn Error>` is not Send, so it cannot be held across the await
    // boundary. Both the status and the message are extracted in one go.
    let result = pipeline
        .add_with_params(inputs, &dataset_name, user.id, user.tenant_id, &params)
        .await
        .map_err(|e| {
            let (status, label) = classify_add_error(&*e);
            (status, label, e.to_string())
        });

    match result {
        Err((status, label, e)) => {
            let body = json!({
                "error": label,
                "detail": e
            });
            let resp = axum::response::Response::builder()
                .status(status)
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(
                    #[allow(clippy::expect_used, reason = "invariant is upheld by construction")]
                    serde_json::to_string(&body).expect("static json"),
                ))
                .map_err(|e| ApiError::Internal(anyhow::anyhow!("response build error: {e}")))?;
            Ok(resp)
        }
        Ok(data_items) => {
            // Look up the dataset id.
            let dataset_row = db
                .get_dataset_by_name(&dataset_name, user.id, user.tenant_id)
                .await
                .ok()
                .flatten();
            let ds_id = dataset_row.as_ref().map(|d| d.id).unwrap_or_else(Uuid::nil);

            let ingestion_info: Vec<DataIngestionInfoDTO> = data_items
                .iter()
                .map(|d| DataIngestionInfoDTO {
                    data_id: d.id,
                    content_hash: d.content_hash.clone(),
                    name: d.name.clone(),
                    extension: d.extension.clone(),
                    mime_type: d.mime_type.clone(),
                    raw_data_location: d.raw_data_location.clone(),
                })
                .collect();

            let run_info = PipelineRunInfoDTO {
                status: "PipelineRunCompleted".into(),
                pipeline_run_id: Uuid::new_v4(),
                dataset_id: ds_id,
                dataset_name: dataset_name.clone(),
                payload: None,
                error: None,
                data_ingestion_info: Some(ingestion_info),
            };

            let resp = axum::response::Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(
                    #[allow(clippy::expect_used, reason = "invariant is upheld by construction")]
                    serde_json::to_string(&run_info).expect("static json"),
                ))
                .map_err(|e| ApiError::Internal(anyhow::anyhow!("response build error: {e}")))?;
            Ok(resp)
        }
    }
}

// ─── router ──────────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new().route("/", post(post_add))
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use cognee_core::pipeline::ExecutionError;

    /// An unsupported document type is the caller's problem, not the server's.
    /// It used to surface as 500 because the router mapped every pipeline error
    /// to `INTERNAL_SERVER_ERROR` with the type erased to a string.
    ///
    /// 415 is a deliberate divergence from Python, which answers 500 on this
    /// route — see `classify_add_error` for why, and
    /// `e2e-cross-sdk`'s `test_add_unsupported_media_type_status_diverges`,
    /// which pins it.
    #[test]
    fn unsupported_document_type_maps_to_415() {
        let err = IngestionError::UnsupportedDocumentType {
            document_type: "pdf".to_string(),
        };
        assert_eq!(
            classify_add_error(&err),
            (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "Unsupported document type"
            ),
        );
    }

    /// The real error never arrives bare: the executor wraps the task's error
    /// in `ExecutionError::TaskFailed` as its `#[source]`. Classification must
    /// walk the chain, or the 415 would only ever fire in a unit test.
    #[test]
    fn unsupported_document_type_is_found_through_the_source_chain() {
        let wrapped = ExecutionError::TaskFailed {
            task_index: 0,
            attempts: 1,
            source: Box::new(IngestionError::UnsupportedDocumentType {
                document_type: "pdf".to_string(),
            }),
        };
        assert_eq!(
            classify_add_error(&wrapped).0,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "the classifier must look past the executor's wrapper"
        );
    }

    /// Everything else stays a 500 — this is a targeted exception, not a
    /// blanket downgrade of pipeline failures to client errors — and keeps the
    /// `"Pipeline run errored"` label Python uses for that case.
    #[test]
    fn other_errors_stay_500() {
        let other = IngestionError::MissingBackend { which: "graph_db" };
        assert_eq!(
            classify_add_error(&other),
            (StatusCode::INTERNAL_SERVER_ERROR, "Pipeline run errored"),
        );

        let io = std::io::Error::other("disk on fire");
        assert_eq!(classify_add_error(&io).0, StatusCode::INTERNAL_SERVER_ERROR);

        // A stringified error carrying the same text must NOT be treated as a
        // client error: classification is by type, not by message matching.
        let stringly = ExecutionError::TaskFailed {
            task_index: 0,
            attempts: 1,
            source: "Unsupported document type at ingest: pdf".into(),
        };
        assert_eq!(
            classify_add_error(&stringly).0,
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    /// The status and the body label move together. A 415 carrying the 5xx
    /// `"Pipeline run errored"` envelope would tell the client two different
    /// stories about the same failure.
    #[test]
    fn label_matches_the_status_class() {
        let (status, label) = classify_add_error(&IngestionError::UnsupportedDocumentType {
            document_type: "png".to_string(),
        });
        assert!(status.is_client_error());
        assert_ne!(label, "Pipeline run errored");
    }
}
