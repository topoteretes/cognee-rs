//! `POST /api/v1/improve` — run the improve (memify) enrichment pipeline.
//!
//! Python parity: `cognee/api/v1/improve/routers/get_improve_router.py`.
//!
//! # 420 quirk
//!
//! Unlike cognify/memify which return 500 on pipeline error, `/improve` returns
//! **HTTP 420** with the raw `PipelineRunInfoDTO` body (not the canonical envelope).
//! This is a Python-side quirk preserved for wire parity.
//! See `ApiError::PipelineErrored { pipeline_source: Improve }` in `error.rs`.

use axum::{Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::post};
use cognee_cognify::memify::sync_graph_session::DEFAULT_MAX_LINES;
use cognee_cognify::{
    ChunkStrategy, CognifyConfig, MemifyConfig, apply_feedback_weights_pipeline,
    persist_sessions_in_knowledge_graph, run_memify, sync_graph_to_session,
};
use cognee_database::{IngestDb, NoopPipelineRunRepository};
use cognee_ingestion::AddPipeline;
use cognee_ontology::{NoOpOntologyResolver, OntologyResolver};
use std::sync::Arc;
use uuid::Uuid;

use crate::auth::AuthenticatedUser;
use crate::dto::improve::ImprovePayloadDTO;
use crate::dto::pipeline_run::PipelineRunInfoDTO;
use crate::error::{ApiError, PipelineErrorSource};
use crate::pipelines::dispatch::{DispatchOutcome, box_pipeline_future, dispatch_pipeline};
use crate::state::AppState;

// ─── post_improve ─────────────────────────────────────────────────────────────

/// `POST /api/v1/improve`
///
/// Runs the improve enrichment pipeline for a specific dataset. Either
/// `dataset_id` or `dataset_name` must be provided.
///
/// On pipeline error, returns **420** with the raw `PipelineRunInfoDTO`
/// (Python parity quirk — see module doc).
pub async fn post_improve(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Json(payload): Json<ImprovePayloadDTO>,
) -> Result<impl IntoResponse, ApiError> {
    // ── Resolve dataset ────────────────────────────────────────────────────────
    let dataset_id_opt = payload.dataset_id.as_option();
    let db = state.components().map(|c| c.database.clone());

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
        // Python raises HTTPException(400, detail="...") → body uses "detail" key.
        return Err(ApiError::BadRequest(
            "Either datasetId or datasetName must be provided.".into(),
        ));
    };

    let run_in_background = payload.run_in_background.unwrap_or(false);

    // ── Telemetry plumbing ─────────────────────────────────────────────────────
    // Mirrors Python's per-field telemetry on the improve handler
    // (`get_improve_router.py:67-74`). The five v2 payload fields are observed
    // here so cross-SDK harnesses can confirm they reach the handler.
    let session_ids_count = payload.session_ids.as_ref().map_or(0, |s| s.len());
    let extraction_tasks_count = payload.extraction_tasks.as_ref().map(|v| v.len());
    let enrichment_tasks_count = payload.enrichment_tasks.as_ref().map(|v| v.len());
    let node_name_count = payload.node_name.as_ref().map(|v| v.len());
    let has_data = payload.data.as_deref().is_some_and(|d| !d.is_empty());
    tracing::info!(
        session_ids_count,
        extraction_tasks_count = ?extraction_tasks_count,
        enrichment_tasks_count = ?enrichment_tasks_count,
        node_name_count = ?node_name_count,
        has_data,
        run_in_background,
        dataset_id = %dataset_id,
        "improve payload received"
    );

    // ── Dispatch ────────────────────────────────────────────────────────────────
    let payload_for_run = payload.clone();
    let components = state.lib.clone();
    let user_for_run = user.clone();
    let dataset_name_for_run = dataset_name.clone();

    let work = box_pipeline_future(async move {
        let components = components
            .as_ref()
            .ok_or_else(|| ImproveDispatchError("components not initialized".into()))?;
        run_real_improve(
            components.as_ref(),
            &user_for_run,
            dataset_id,
            &dataset_name_for_run,
            &payload_for_run,
        )
        .await
    });

    let outcome = dispatch_pipeline(
        &state,
        &user,
        "improve_pipeline",
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
                    // 420 quirk: return the raw PipelineRunInfoDTO, not the
                    // canonical {"error": ..., "detail": ...} envelope.
                    let run_info_dto = PipelineRunInfoDTO {
                        status: "PipelineRunErrored".into(),
                        pipeline_run_id: outcome.run_id,
                        dataset_id,
                        dataset_name,
                        payload: None,
                        error: Some(message),
                        data_ingestion_info: None,
                    };
                    return Err(ApiError::PipelineErrored {
                        pipeline_source: PipelineErrorSource::Improve,
                        run_info: serde_json::to_value(&run_info_dto)
                            .unwrap_or(serde_json::json!({})),
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

#[derive(Debug)]
struct ImproveDispatchError(String);

impl std::fmt::Display for ImproveDispatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ImproveDispatchError {}

async fn run_real_improve(
    components: &crate::components::ComponentHandles,
    user: &AuthenticatedUser,
    dataset_id: Uuid,
    dataset_name: &str,
    payload: &ImprovePayloadDTO,
) -> Result<(), ImproveDispatchError> {
    let graph_db = components
        .graph_db
        .clone()
        .ok_or_else(|| ImproveDispatchError("graph_db not wired in ComponentHandles".into()))?;
    let vector_db = components
        .vector_db
        .clone()
        .ok_or_else(|| ImproveDispatchError("vector_db not wired in ComponentHandles".into()))?;
    let embedding_engine = components.embedding_engine.clone().ok_or_else(|| {
        ImproveDispatchError("embedding_engine not wired in ComponentHandles".into())
    })?;
    let thread_pool = components
        .thread_pool
        .clone()
        .ok_or_else(|| ImproveDispatchError("thread_pool not wired in ComponentHandles".into()))?;

    let database = components.database.clone();
    let storage = components.storage.clone();
    let session_ids = payload
        .session_ids
        .as_ref()
        .filter(|ids| !ids.is_empty())
        .cloned();
    let has_sessions = session_ids.is_some();
    let feedback_alpha = 0.5f64;

    let ontology_resolver: Arc<dyn OntologyResolver> = components
        .ontology_resolver
        .clone()
        .unwrap_or_else(|| Arc::new(NoOpOntologyResolver::new()));

    // ---- Stage 1: Apply Feedback Weights ----
    if let Some(sids) = session_ids.as_ref() {
        match (
            components.session_store.as_ref(),
            components.session_manager.as_ref(),
        ) {
            (Some(store), Some(manager)) => {
                if let Err(e) = apply_feedback_weights_pipeline(
                    sids,
                    user.id,
                    feedback_alpha,
                    graph_db.as_ref(),
                    Arc::clone(store),
                    Arc::clone(manager),
                )
                .await
                {
                    tracing::warn!("improve stage 1 (feedback_weights) failed (non-fatal): {e}");
                }
            }
            _ => {
                tracing::warn!(
                    "improve stage 1: session_store and session_manager are required; skipping feedback_weights"
                );
            }
        }
    }

    // ---- Stage 2: Persist Session Q&A to Graph ----
    if let Some(sids) = session_ids.as_ref() {
        match (components.session_store.as_ref(), components.llm.as_ref()) {
            (Some(store), Some(llm)) => {
                let add_pipeline = AddPipeline::new(storage.clone(), database.clone())
                    .with_thread_pool(thread_pool.clone())
                    .with_graph_db(graph_db.clone())
                    .with_vector_db(vector_db.clone())
                    .with_database(database.clone())
                    .with_pipeline_run_repo(NoopPipelineRunRepository::arc());

                let mut cognify_config =
                    CognifyConfig::default().with_chunk_strategy(ChunkStrategy::Paragraph);
                if let Some(ref t) = components.transcriber {
                    cognify_config = cognify_config.with_transcriber(Arc::clone(t));
                }

                if let Err(e) = persist_sessions_in_knowledge_graph(
                    sids,
                    dataset_name,
                    user.id,
                    user.tenant_id,
                    Arc::clone(store),
                    &add_pipeline,
                    Arc::clone(llm),
                    storage.clone(),
                    graph_db.clone(),
                    vector_db.clone(),
                    embedding_engine.clone(),
                    database.clone(),
                    NoopPipelineRunRepository::arc(),
                    thread_pool.clone(),
                    Arc::clone(&ontology_resolver),
                    &cognify_config,
                )
                .await
                {
                    tracing::warn!("improve stage 2 (persist_sessions) failed (non-fatal): {e}");
                }
            }
            _ => {
                tracing::warn!(
                    "improve stage 2: session_store and llm are required; skipping persist_sessions"
                );
            }
        }
    }

    // ---- Stage 3: Default Enrichment (always) ----
    let memify_config = if let Some(names) = payload.node_name.clone() {
        MemifyConfig::default().with_node_name_filter(names)
    } else {
        MemifyConfig::default()
    };

    run_memify(
        graph_db.clone(),
        vector_db,
        embedding_engine,
        thread_pool,
        database.clone(),
        NoopPipelineRunRepository::arc(),
        Some(dataset_id),
        Some(user.id),
        user.tenant_id,
        &memify_config,
    )
    .await
    .map_err(|e| ImproveDispatchError(format!("memify failed: {e}")))?;

    // ---- Stage 4: Sync Graph to Session Cache ----
    if has_sessions {
        match (
            session_ids.as_ref(),
            components.session_manager.as_ref(),
            components.checkpoint_store.as_ref(),
        ) {
            (Some(sids), Some(manager), Some(checkpoint_store)) => {
                let user_id_str = user.id.to_string();
                for sid in sids {
                    if let Err(e) = sync_graph_to_session(
                        &user_id_str,
                        sid,
                        dataset_id,
                        database.as_ref(),
                        manager.as_ref(),
                        checkpoint_store.as_ref(),
                        DEFAULT_MAX_LINES,
                    )
                    .await
                    {
                        tracing::warn!(
                            session_id = sid,
                            "improve stage 4 failed for session (non-fatal): {e}"
                        );
                    }
                }
            }
            _ => {
                tracing::warn!(
                    "improve stage 4: session_manager and checkpoint_store are required; skipping sync_graph_to_session"
                );
            }
        }
    }

    Ok(())
}

// ─── router ──────────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new().route("/", post(post_improve))
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use axum::{
        Router,
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

    async fn post_improve_no_auth(
        State(state): State<AppState>,
        Json(payload): Json<ImprovePayloadDTO>,
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
        post_improve(user, State(state), Json(payload)).await
    }

    #[tokio::test]
    async fn post_improve_no_dataset_returns_bad_request() {
        let state = test_state().await;
        let app = Router::new()
            .route("/", post(post_improve_no_auth))
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

    #[tokio::test]
    async fn post_improve_with_dataset_name_dispatches() {
        let state = test_state().await;
        let app = Router::new()
            .route("/", post(post_improve_no_auth))
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

    #[tokio::test]
    async fn post_improve_with_dataset_id_dispatches() {
        let state = test_state().await;
        let app = Router::new()
            .route("/", post(post_improve_no_auth))
            .with_state(state);

        let dataset_id = Uuid::new_v4();
        let body = json!({ "datasetId": dataset_id.to_string(), "runInBackground": true });
        let req = Request::builder()
            .method("POST")
            .uri("/")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.expect("oneshot");
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn post_improve_background_returns_started() {
        let state = test_state().await;
        let app = Router::new()
            .route("/", post(post_improve_no_auth))
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

    /// Python parity: improve's validation 400 body uses `{"detail": "..."}`.
    /// Python uses `HTTPException(400, detail="...")`.
    /// See improve.md §2.1 error table.
    #[tokio::test]
    async fn post_improve_no_dataset_body_uses_detail_key() {
        use axum::body::to_bytes;
        let state = test_state().await;
        let app = Router::new()
            .route("/", post(post_improve_no_auth))
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
            v["detail"], "Either datasetId or datasetName must be provided.",
            "improve validation body must use 'detail' key per Python parity"
        );
        assert!(
            v.get("error").is_none(),
            "improve validation must NOT use 'error' key"
        );
    }

    /// Python parity: `/improve` returns HTTP 420 on `PipelineRunErrored` with
    /// the raw `PipelineRunInfoDTO` body (not the canonical envelope).
    /// This is the headline parity test for the phase.
    ///
    /// Since the no-op stub always succeeds, we exercise this via the error.rs
    /// unit test rather than triggering a real pipeline error here.
    /// The `test_improve_420.rs` integration test provides end-to-end coverage.
    #[tokio::test]
    async fn post_improve_420_via_error_response() {
        use crate::error::{ApiError, PipelineErrorSource};
        use axum::body::to_bytes;

        // Build the response directly from the ApiError variant to assert the
        // shape without needing a real pipeline failure.
        let run_info = serde_json::json!({
            "status": "PipelineRunErrored",
            "pipeline_run_id": "00000000-0000-0000-0000-000000000001",
            "dataset_id": "00000000-0000-0000-0000-000000000002",
            "dataset_name": "test_dataset",
            "error": "improve stub always succeeds in tests"
        });
        let resp = ApiError::PipelineErrored {
            pipeline_source: PipelineErrorSource::Improve,
            run_info: run_info.clone(),
        }
        .into_response();

        // Headline parity assertion: literal 420, not 500.
        assert_eq!(
            resp.status().as_u16(),
            420,
            "PipelineRunErrored from /improve must return HTTP 420"
        );

        let body_bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

        // Body is the raw PipelineRunInfoDTO, NOT the canonical envelope.
        assert_eq!(
            v["status"], "PipelineRunErrored",
            "body must be the raw run info object"
        );
        assert_ne!(
            v.get("error").and_then(|e| e.as_str()),
            Some("Pipeline run errored"),
            "body must NOT be the canonical error envelope"
        );
    }
}
