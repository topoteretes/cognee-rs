//! `POST /api/v1/memify` — run the memify graph-enrichment pipeline.
//!
//! Python parity: `cognee/api/v1/memify/routers/get_memify_router.py`.

use std::sync::Arc;

use axum::{Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::post};
use cognee_cognify::{MemifyConfig, run_memify};
use cognee_database::{IngestDb, NoopPipelineRunRepository};
use uuid::Uuid;

use crate::auth::AuthenticatedUser;
use crate::components::ComponentHandles;
use crate::dto::memify::MemifyPayloadDTO;
use crate::dto::pipeline_run::PipelineRunInfoDTO;
use crate::error::{ApiError, PipelineErrorSource};
use crate::pipelines::dispatch::{DispatchOutcome, box_pipeline_future, dispatch_pipeline};
use crate::state::AppState;

// ─── post_memify ──────────────────────────────────────────────────────────────

/// `POST /api/v1/memify`
///
/// Runs the memify enrichment pipeline for a specific dataset. Either
/// `dataset_id` or `dataset_name` must be provided.
pub async fn post_memify(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Json(payload): Json<MemifyPayloadDTO>,
) -> Result<impl IntoResponse, ApiError> {
    // ── Resolve dataset ────────────────────────────────────────────────────────
    let dataset_id_opt = payload.dataset_id.as_option();
    let db = state.components().map(|c| c.database.clone());

    let (dataset_id, dataset_name) = if let Some(id) = dataset_id_opt {
        // UUID given — look up name.
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
    } else if let Some(ref name) = payload.dataset_name {
        let id = if let Some(ref db) = db {
            match db.get_dataset_by_name(name, user.id, user.tenant_id).await {
                Ok(Some(ds)) => ds.id,
                Ok(None) => {
                    return Err(ApiError::NotFound(format!("Dataset '{name}' not found")));
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
        // Python uses JSONResponse({"error": "..."}, status_code=400) here
        // (not HTTPException), so the body key is "error" not "detail".
        // See memify.md §2.1 error table.
        return Err(ApiError::OntologyEnvelope(
            "Either datasetId or datasetName must be provided.".into(),
            axum::http::StatusCode::BAD_REQUEST,
        ));
    };

    let run_in_background = payload.run_in_background.unwrap_or(false);

    // ── Build MemifyConfig from payload ────────────────────────────────────────
    // The current `MemifyPayloadDTO` only carries `dataset_name`, `dataset_id`,
    // and `run_in_background`. Python's full payload surface (extraction_tasks,
    // enrichment_tasks, node_name, node_type, data) is not yet plumbed into the
    // DTO; for now we use `MemifyConfig::default()`, matching the CLI's defaults.
    let memify_config = Arc::new(MemifyConfig::default());

    // ── Dispatch ────────────────────────────────────────────────────────────────
    let components = state.components();
    let user_for_run = user.clone();
    let config_for_run = Arc::clone(&memify_config);
    let components_owned = components.cloned();

    let work = box_pipeline_future(async move {
        let Some(components) = components_owned else {
            return Err(MemifyDispatchError(
                "Component handles not initialized; cannot run memify pipeline".to_string(),
            ));
        };
        run_real_memify(&components, &user_for_run, dataset_id, &config_for_run).await
    });

    let outcome = dispatch_pipeline(
        &state,
        &user,
        "memify_pipeline",
        Some(dataset_id),
        run_in_background,
        work,
    )
    .await?;

    // ── Map outcome → response ─────────────────────────────────────────────────
    let run_info = match outcome {
        DispatchOutcome::Blocking { outcome } => {
            use cognee_core::pipeline_run_registry::RunPhase;
            match outcome.phase {
                RunPhase::Completed | RunPhase::Pending => PipelineRunInfoDTO {
                    status: "PipelineRunCompleted".into(),
                    pipeline_run_id: outcome.run_id,
                    dataset_id,
                    dataset_name,
                    payload: None,
                    error: None,
                    data_ingestion_info: None,
                },
                RunPhase::Errored { message } => {
                    return Err(ApiError::PipelineErrored {
                        pipeline_source: PipelineErrorSource::Memify,
                        run_info: serde_json::json!({
                            "error": "Pipeline run errored",
                            "detail": message
                        }),
                    });
                }
                RunPhase::Running => PipelineRunInfoDTO {
                    status: "PipelineRunStarted".into(),
                    pipeline_run_id: outcome.run_id,
                    dataset_id,
                    dataset_name,
                    payload: None,
                    error: None,
                    data_ingestion_info: None,
                },
            }
        }
        DispatchOutcome::Background { handle } => PipelineRunInfoDTO {
            status: "PipelineRunStarted".into(),
            pipeline_run_id: handle.run_id,
            dataset_id,
            dataset_name,
            payload: None,
            error: None,
            data_ingestion_info: None,
        },
    };

    Ok((StatusCode::OK, Json(run_info)))
}

// ─── Memify execution helpers ────────────────────────────────────────────────

/// Boxed-future-compatible error type for the memify pipeline path.
///
/// `dispatch_pipeline` expects `Box<dyn Error + Send + Sync>`; this wrapper
/// carries the underlying message back to the registry so it surfaces in the
/// `RunPhase::Errored { message }` payload.
#[derive(Debug)]
struct MemifyDispatchError(String);

impl std::fmt::Display for MemifyDispatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for MemifyDispatchError {}

/// Drive a single dataset through the memify enrichment pipeline.
///
/// Picks up `GraphDBTrait` / `VectorDB` / `EmbeddingEngine` / `CpuPool`
/// handles from `ComponentHandles`, threads them through
/// [`cognee_cognify::run_memify`], and surfaces missing-handle / runtime
/// errors as `MemifyDispatchError` so the registry's
/// `RunPhase::Errored { message }` carries the underlying cause.
async fn run_real_memify(
    components: &ComponentHandles,
    user: &AuthenticatedUser,
    dataset_id: Uuid,
    config: &MemifyConfig,
) -> Result<(), MemifyDispatchError> {
    // ── Pull required backend handles ─────────────────────────────────────────
    let graph_db = components
        .graph_db
        .clone()
        .ok_or_else(|| MemifyDispatchError("graph_db not wired in ComponentHandles".into()))?;
    let vector_db = components
        .vector_db
        .clone()
        .ok_or_else(|| MemifyDispatchError("vector_db not wired in ComponentHandles".into()))?;
    let embedding_engine = components.embedding_engine.clone().ok_or_else(|| {
        MemifyDispatchError("embedding_engine not wired in ComponentHandles".into())
    })?;
    let thread_pool = components
        .thread_pool
        .clone()
        .ok_or_else(|| MemifyDispatchError("thread_pool not wired in ComponentHandles".into()))?;

    let database = components.database.clone();

    // Gap 08-07: the outer `dispatch_pipeline` already wires
    // `DefaultPipelineRunRegistry` (backed by `SeaOrmPipelineRunRepository`)
    // for the four-state `pipeline_runs` trail. Hand the inner memify a
    // no-op repo so its `DbPipelineWatcher` does not produce a second row-set.
    let pipeline_run_repo = NoopPipelineRunRepository::arc();

    run_memify(
        graph_db,
        vector_db,
        embedding_engine,
        thread_pool,
        database,
        pipeline_run_repo,
        Some(dataset_id),
        Some(user.id),
        user.tenant_id,
        config,
    )
    .await
    .map_err(|e| MemifyDispatchError(format!("memify failed: {e}")))?;

    Ok(())
}

// ─── router ──────────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new().route("/", post(post_memify))
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode, header},
    };
    use serde_json::json;
    use tower::ServiceExt;

    async fn test_state() -> AppState {
        AppState::build(crate::config::HttpServerConfig::default())
            .await
            .expect("AppState::build")
    }

    async fn post_memify_no_auth(
        State(state): State<AppState>,
        Json(payload): Json<MemifyPayloadDTO>,
    ) -> Result<impl IntoResponse, ApiError> {
        let user = AuthenticatedUser {
            id: Uuid::new_v4(),
            email: "test@example.com".into(),
            is_superuser: false,
            is_verified: true,
            is_active: true,
            tenant_id: Some(Uuid::new_v4()),
            auth_method: crate::auth::AuthMethod::DefaultUser,
        };
        post_memify(user, State(state), Json(payload)).await
    }

    #[tokio::test]
    async fn post_memify_no_dataset_returns_bad_request() {
        let state = test_state().await;
        let app = Router::new()
            .route("/", post(post_memify_no_auth))
            .with_state(state);

        let req = Request::builder()
            .method("POST")
            .uri("/")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{}"#))
            .unwrap();

        let resp = app.oneshot(req).await.expect("oneshot");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// Verifies the dataset-name → uuid5 resolution path dispatches without a DB.
    ///
    /// Uses `run_in_background=true` to assert the response shape without
    /// running the real memify pipeline (which would 500 without backends —
    /// see `post_memify_with_dataset_id_surfaces_missing_components`).
    #[tokio::test]
    async fn post_memify_with_dataset_name_no_db_dispatches() {
        let state = test_state().await;
        let app = Router::new()
            .route("/", post(post_memify_no_auth))
            .with_state(state);

        let body = json!({ "dataset_name": "my_dataset", "run_in_background": true });
        let req = Request::builder()
            .method("POST")
            .uri("/")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.expect("oneshot");
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// Verifies the dataset-id resolution path dispatches without a DB.
    ///
    /// Uses `run_in_background=true` to assert the dispatch wiring without
    /// running the real memify pipeline (which would 500 without backends).
    #[tokio::test]
    async fn post_memify_with_dataset_id_no_db_dispatches() {
        let state = test_state().await;
        let app = Router::new()
            .route("/", post(post_memify_no_auth))
            .with_state(state);

        let dataset_id = Uuid::new_v4();
        let body = json!({
            "datasetId": dataset_id.to_string(),
            "run_in_background": true
        });
        let req = Request::builder()
            .method("POST")
            .uri("/")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.expect("oneshot");
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// Blocking dispatch without backends wired now reaches the real memify
    /// path and surfaces the missing-component error through the
    /// `Pipeline run errored` envelope. Mirrors
    /// `routers::cognify::tests::post_cognify_with_dataset_ids_surfaces_missing_components`.
    #[tokio::test]
    async fn post_memify_with_dataset_id_surfaces_missing_components() {
        let state = test_state().await;
        let app = Router::new()
            .route("/", post(post_memify_no_auth))
            .with_state(state);

        let dataset_id = Uuid::new_v4();
        let body = json!({
            "datasetId": dataset_id.to_string(),
            "run_in_background": false
        });
        let req = Request::builder()
            .method("POST")
            .uri("/")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.expect("oneshot");
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(v["error"], "Pipeline run errored");
        let detail = v["detail"].as_str().unwrap_or_default();
        assert!(
            detail.contains("Component handles not initialized")
                || detail.contains("not wired in ComponentHandles"),
            "error detail must point at the missing-component path, got: {detail}"
        );
    }

    #[tokio::test]
    async fn post_memify_background_returns_started() {
        let state = test_state().await;
        let app = Router::new()
            .route("/", post(post_memify_no_auth))
            .with_state(state);

        let body = json!({
            "dataset_name": "my_dataset",
            "run_in_background": true
        });
        let req = Request::builder()
            .method("POST")
            .uri("/")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.expect("oneshot");
        assert_eq!(resp.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(v["status"], "PipelineRunStarted");
    }

    /// Python parity: memify's validation 400 body uses `{"error": "..."}` not
    /// `{"detail": "..."}`.  Python uses `JSONResponse({"error": ...}, status_code=400)`
    /// (not `HTTPException`), so the body key differs from remember/improve.
    /// See memify.md §2.1 error table.
    #[tokio::test]
    async fn post_memify_no_dataset_body_uses_error_key() {
        use axum::body::to_bytes;
        let state = test_state().await;
        let app = Router::new()
            .route("/", post(post_memify_no_auth))
            .with_state(state);

        let req = Request::builder()
            .method("POST")
            .uri("/")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{}"#))
            .unwrap();

        let resp = app.oneshot(req).await.expect("oneshot");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let body_bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(
            v["error"], "Either datasetId or datasetName must be provided.",
            "validation body must use 'error' key per Python parity"
        );
        assert!(
            v.get("detail").is_none(),
            "memify validation must NOT use 'detail' key"
        );
    }
}
