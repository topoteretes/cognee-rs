//! `POST /api/v1/remember` — run the remember pipeline (add + cognify + optional memify).
//!
//! Python parity: `cognee/api/v1/remember/routers/get_remember_router.py`.
//!
//! The endpoint accepts a multipart form body with optional file parts (`data`)
//! and optional text fields (`datasetName`, `datasetId`, `node_set`,
//! `run_in_background`, `custom_prompt`, `chunks_per_batch`).

use axum::{
    Json, Router,
    extract::{Multipart, State},
    http::StatusCode,
    response::IntoResponse,
    routing::post,
};
use cognee_database::IngestDb;
use cognee_ingestion::{AddParams, AddPipeline};
use cognee_models::DataInput;
use uuid::Uuid;

use crate::auth::AuthenticatedUser;
use crate::dto::remember::{
    RememberFormDTO, RememberResultDTO, UploadedFilePart, WireRememberStatus,
};
use crate::error::ApiError;
use crate::multipart::{MultipartOpts, UploadGuard, check_filename_traversal, parse_multipart};
use crate::pipelines::dispatch::{DispatchOutcome, box_pipeline_future, dispatch_pipeline};
use crate::state::AppState;

// ─── parse_remember_multipart ─────────────────────────────────────────────────

/// Parse the multipart body into a [`RememberFormDTO`] + uploaded file parts.
async fn parse_remember_multipart(
    multipart: Multipart,
    request_id: &str,
) -> Result<(RememberFormDTO, Vec<UploadedFilePart>, UploadGuard), ApiError> {
    let opts = MultipartOpts::default();
    let parsed = parse_multipart(multipart, &opts, request_id).await?;
    let guard = UploadGuard::new(parsed.spool_dir.clone());

    let mut form = RememberFormDTO::default();
    let mut files: Vec<UploadedFilePart> = Vec::new();

    // ── text fields ───────────────────────────────────────────────────────────
    if let Some(vals) = parsed.fields.get("datasetName")
        && let Some(v) = vals.first()
    {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            form.dataset_name = Some(trimmed.to_owned());
        }
    }

    if let Some(vals) = parsed.fields.get("datasetId")
        && let Some(v) = vals.first()
    {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            match Uuid::parse_str(trimmed) {
                Ok(id) => {
                    form.dataset_id = crate::dto::util::DatasetIdRef(Some(id));
                }
                Err(_) => {
                    return Err(ApiError::BadRequest("Invalid datasetId UUID".into()));
                }
            }
        }
    }

    if let Some(vals) = parsed.fields.get("node_set") {
        let non_empty: Vec<String> = vals
            .iter()
            .filter(|s| !s.trim().is_empty())
            .cloned()
            .collect();
        form.node_set = if non_empty.is_empty() {
            None
        } else {
            Some(non_empty)
        };
    }

    if let Some(vals) = parsed.fields.get("run_in_background")
        && let Some(v) = vals.first()
    {
        let v = v.trim().to_ascii_lowercase();
        form.run_in_background = Some(v == "true" || v == "1");
    }

    if let Some(vals) = parsed.fields.get("custom_prompt")
        && let Some(v) = vals.first()
    {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            form.custom_prompt = Some(trimmed.to_owned());
        }
    }

    // Python parity: `session_id: Optional[str] = Form(default=None, examples=[""])`
    // (`get_remember_router.py:34`). Empty string is treated as `None`.
    if let Some(vals) = parsed.fields.get("session_id")
        && let Some(v) = vals.first()
    {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            form.session_id = Some(trimmed.to_owned());
        }
    }

    if let Some(vals) = parsed.fields.get("chunks_per_batch")
        && let Some(v) = vals.first()
        && let Ok(n) = v.trim().parse::<u32>()
    {
        form.chunks_per_batch = Some(n);
    }

    // ── file parts ────────────────────────────────────────────────────────────
    if let Some(spooled) = parsed.files.get("data") {
        for sf in spooled {
            if let Some(ref fname) = sf.filename {
                check_filename_traversal(fname)?;
            }
            files.push(UploadedFilePart {
                file_name: sf.filename.clone(),
                content_type: sf.content_type.clone(),
                temp_path: sf.path.clone(),
                byte_count: sf.byte_count,
            });
        }
    }

    Ok((form, files, guard))
}

// ─── post_remember ────────────────────────────────────────────────────────────

/// `POST /api/v1/remember`
///
/// Accepts a multipart form body with optional file parts and dispatches the
/// remember pipeline (add + cognify + optional memify).
///
/// Either `datasetName` or `datasetId` must be supplied.
///
/// On error in blocking mode, returns `409 Conflict` per Python parity
/// (unlike cognify/memify which return 500).
pub async fn post_remember(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    multipart: Multipart,
) -> Result<impl IntoResponse, ApiError> {
    // Capture wall-clock start to populate `RememberResultDTO.elapsed_seconds`
    // (Python parity — `RememberResult.elapsed_seconds`).
    let started = std::time::Instant::now();
    let request_id = Uuid::new_v4().to_string();
    let (form, files, _guard) = parse_remember_multipart(multipart, &request_id).await?;

    // TODO(P5): forward `form.session_id` to the real
    // `cognee_lib::api::remember::remember()` call once ComponentHandles
    // gains all required handles. Currently the cognify+memify path is a
    // stub (see comment near `dispatch_pipeline` below); session_id is
    // captured by the parser but not yet wired through the dispatch site.
    let _session_id = form.session_id.clone();

    // ── Dataset resolution ────────────────────────────────────────────────────
    let db = state.components().map(|c| c.database.clone());
    let dataset_id_opt = form.dataset_id.as_option();

    let (dataset_id, dataset_name) = if let Some(id) = dataset_id_opt {
        let name = if let Some(ref db) = db {
            match db.get_dataset(id).await {
                Ok(Some(ds)) => ds.name,
                Ok(None) => {
                    return Err(ApiError::NotFound(format!("Dataset {id} not found")));
                }
                Err(e) => {
                    return Err(ApiError::Internal(anyhow::anyhow!(
                        "DB error looking up dataset {id}: {e}"
                    )));
                }
            }
        } else {
            id.to_string()
        };
        (id, name)
    } else if let Some(ref name) = form.dataset_name {
        let id = if let Some(ref db) = db {
            match db.get_dataset_by_name(name, user.id, user.tenant_id).await {
                Ok(Some(ds)) => ds.id,
                Ok(None) => {
                    // Python auto-creates the dataset on remember — generate a new id.
                    Uuid::new_v4()
                }
                Err(e) => {
                    return Err(ApiError::Internal(anyhow::anyhow!(
                        "DB error looking up dataset '{name}': {e}"
                    )));
                }
            }
        } else {
            Uuid::new_v5(&Uuid::NAMESPACE_OID, name.as_bytes())
        };
        (id, name.clone())
    } else {
        // Python raises HTTPException(400, detail="...") → body uses "detail" key.
        return Err(ApiError::BadRequest(
            "Either datasetId or datasetName must be provided.".into(),
        ));
    };

    let run_in_background = form.run_in_background.unwrap_or(false);

    // ── Build data inputs for the add phase ───────────────────────────────────
    let mut inputs: Vec<DataInput> = Vec::new();
    for part in &files {
        let path = part.temp_path.to_string_lossy().to_string();
        inputs.push(DataInput::FilePath(format!("file://{path}")));
    }
    if inputs.is_empty() {
        inputs.push(DataInput::Text(String::new()));
    }

    // ── Add phase (only if DB + storage wired) ────────────────────────────────
    let pipeline_run_id = if let Some(components) = state.components() {
        let db_arc = components.database.clone();
        let storage_arc = components.storage.clone();

        let add_params = AddParams {
            node_set: form.node_set.clone(),
            ..AddParams::default()
        };

        let pipeline = AddPipeline::new(
            storage_arc,
            db_arc.clone() as std::sync::Arc<dyn cognee_database::IngestDb>,
        )
        .with_acl_db(db_arc as std::sync::Arc<dyn cognee_database::AclDb>);

        // Run add synchronously — errors map to 409 {"error": "An error occurred
        // during remember."} per Python parity (not {"detail": "..."}).
        // Python catches all exceptions here and returns JSONResponse({"error": ...}).
        pipeline
            .add_with_params(inputs, &dataset_name, user.id, user.tenant_id, &add_params)
            .await
            .map_err(|_e| {
                ApiError::DeprecatedConflict("An error occurred during remember.".into())
            })?;

        // ── Cognify + memify stub dispatch ─────────────────────────────────────
        // Same blocking-gap pattern as cognify.rs — stub until LLM/graph/vector
        // are wired via ComponentHandles.
        // TODO(P5): wire real remember() call once ComponentHandles gains all handles.
        let work = box_pipeline_future(async move { Ok::<(), std::io::Error>(()) });

        let outcome = dispatch_pipeline(
            &state,
            &user,
            "remember_pipeline",
            Some(dataset_id),
            run_in_background,
            work,
        )
        .await
        .map_err(|_e| ApiError::DeprecatedConflict("An error occurred during remember.".into()))?;

        match outcome {
            DispatchOutcome::Blocking { outcome } => outcome.run_id,
            DispatchOutcome::Background { handle } => handle.run_id,
        }
    } else {
        // No components wired — dispatch stub only.
        let work = box_pipeline_future(async move { Ok::<(), std::io::Error>(()) });

        let outcome = dispatch_pipeline(
            &state,
            &user,
            "remember_pipeline",
            Some(dataset_id),
            run_in_background,
            work,
        )
        .await
        .map_err(|_e| ApiError::DeprecatedConflict("An error occurred during remember.".into()))?;

        match outcome {
            DispatchOutcome::Blocking { outcome } => outcome.run_id,
            DispatchOutcome::Background { handle } => handle.run_id,
        }
    };

    // Decision 15: HTTP wire emits Python-parity lowercase strings.
    // Background dispatch → `running`; blocking → `completed`. The
    // `errored` and `session_stored` variants are produced once the real
    // `remember()` call is wired in P5.
    let status = if run_in_background {
        WireRememberStatus::Running
    } else {
        WireRememberStatus::Completed
    };

    // TODO(P5): once the real `cognee_lib::api::remember::remember()` call
    // is wired through ComponentHandles, populate `session_ids`,
    // `content_hash`, and `items` from the returned `RememberResult`. Until
    // then, only locally-known fields are set.
    let result = RememberResultDTO {
        status,
        pipeline_run_id: Some(pipeline_run_id),
        dataset_id: Some(dataset_id),
        dataset_name,
        items_processed: files.len() as u32,
        elapsed_seconds: Some(started.elapsed().as_secs_f64()),
        session_ids: None,
        content_hash: None,
        items: None,
        error: None,
    };

    Ok((StatusCode::OK, Json(result)))
}

// ─── router ──────────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new().route("/", post(post_remember))
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::Body, http::Request};

    async fn test_state() -> AppState {
        AppState::build(crate::config::HttpServerConfig::default())
            .await
            .expect("AppState::build")
    }

    #[tokio::test]
    async fn router_mounts_at_root() {
        // Just verify the router builds without panicking.
        let state = test_state().await;
        let _app: Router = Router::new().merge(router()).with_state(state);
    }

    /// A POST with a non-multipart content type should return 400 (bad request).
    ///
    /// axum's `Multipart` extractor rejects requests whose `Content-Type` is not
    /// `multipart/form-data` with a 400 response.
    #[tokio::test]
    async fn post_without_multipart_content_type_returns_400() {
        use tower::ServiceExt;
        let state = test_state().await;
        // Mount at "/" so the route is `/`.
        let app = Router::new().merge(router()).with_state(state);

        let req = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{}"#))
            .unwrap();

        let resp = app.oneshot(req).await.expect("oneshot");
        // axum rejects non-multipart bodies for a Multipart extractor with 400.
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// Python parity: remember's catch-all 409 body uses `{"error": "..."}`.
    /// Python uses `JSONResponse({"error": ...}, status_code=409)` not
    /// `HTTPException(409, detail=...)`, so the key is "error" not "detail".
    /// See remember.md §2.1 error table.
    #[tokio::test]
    async fn remember_catch_all_409_uses_error_key() {
        use crate::error::ApiError;
        use axum::body::to_bytes;

        // Build the response directly from the DeprecatedConflict variant.
        let resp = ApiError::DeprecatedConflict("An error occurred during remember.".into())
            .into_response();

        assert_eq!(resp.status(), StatusCode::CONFLICT);

        let body_bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

        // Must use "error" key, not "detail".
        assert_eq!(
            v["error"], "An error occurred during remember.",
            "remember catch-all must use 'error' key"
        );
        assert!(
            v.get("detail").is_none(),
            "remember catch-all must NOT have 'detail' key"
        );
    }

    /// Python parity: remember's validation 400 body uses `{"detail": "..."}`.
    /// Python uses `HTTPException(400, detail="...")`.
    /// See remember.md §2.1 error table.
    #[tokio::test]
    async fn remember_validation_400_uses_detail_key() {
        use crate::error::ApiError;
        use axum::body::to_bytes;

        let resp = ApiError::BadRequest("Either datasetId or datasetName must be provided.".into())
            .into_response();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let body_bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(
            v["detail"], "Either datasetId or datasetName must be provided.",
            "remember validation must use 'detail' key per Python HTTPException parity"
        );
        assert!(
            v.get("error").is_none(),
            "remember validation must NOT use 'error' key"
        );
    }
}
