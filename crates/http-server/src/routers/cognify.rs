//! `POST /api/v1/cognify` — run the cognify knowledge-graph extraction pipeline.
//! `GET  /api/v1/cognify/subscribe/{pipeline_run_id}` — WebSocket live stream.
//!
//! Python parity: `cognee/api/v1/cognify/routers/get_cognify_router.py`.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State, WebSocketUpgrade},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use cognee_cognify::{ChunkStrategy, CognifyConfig, cognify as run_cognify};
use cognee_database::{IngestDb, NoopPipelineRunRepository, UserDb, ops as db_ops};
use cognee_ontology::{
    NoOpOntologyResolver, OntologyFileInput, OntologyResolver, RdfLibOntologyResolver,
};
use serde_json::json;
use uuid::Uuid;

use crate::auth::AuthenticatedUser;
use crate::components::ComponentHandles;
use crate::dto::cognify::{CognifyPayloadDTO, CognifyResponseDTO, CognifyWsFrameDTO};
use crate::dto::pipeline_run::{PipelineRunInfoDTO, event_kind_to_python_string};
use crate::error::{ApiError, PipelineErrorSource};
use crate::pipelines::dispatch::{DispatchOutcome, box_pipeline_future, dispatch_pipeline};
use crate::state::AppState;

// ─── post_cognify ─────────────────────────────────────────────────────────────

/// `POST /api/v1/cognify`
///
/// Runs the cognify pipeline for one or more datasets belonging to the
/// authenticated user.
///
/// # Validation
///
/// At least one of `datasets` (name list) or `dataset_ids` (UUID list) must be
/// non-empty. When `dataset_ids` is provided, it takes precedence over
/// `datasets` (Python parity).
///
/// # Fan-out
///
/// Each resolved dataset gets its own `dispatch_pipeline` call. Results are
/// aggregated into `CognifyResponseDTO` (a `Map<dataset_id_str, PipelineRunInfoDTO>`).
pub async fn post_cognify(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Json(payload): Json<CognifyPayloadDTO>,
) -> Result<impl IntoResponse, ApiError> {
    // ── Validation ────────────────────────────────────────────────────────────
    let has_names = payload
        .datasets
        .as_deref()
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let has_ids = payload
        .dataset_ids
        .as_deref()
        .map(|v| !v.is_empty())
        .unwrap_or(false);

    if !has_names && !has_ids {
        // Python uses JSONResponse({"error": "..."}, status_code=400) — body key
        // is "error", not "detail".  See cognify.md §2.1 error table.
        return Err(ApiError::OntologyEnvelope(
            "No datasets or dataset_ids provided".into(),
            StatusCode::BAD_REQUEST,
        ));
    }

    crate::telemetry::emit(
        "Cognify API Endpoint Invoked",
        user.id,
        serde_json::json!({ "endpoint": "POST /v1/cognify" }),
    );

    // ── Resolve datasets ──────────────────────────────────────────────────────
    // Get the DB handle if available.
    let db = state.components().map(|c| c.database.clone());

    // Build the list of (dataset_id, dataset_name) pairs to process.
    let mut dataset_pairs: Vec<(Uuid, String)> = Vec::new();

    if has_ids {
        // `dataset_ids` override `datasets`.
        let ids = payload.dataset_ids.as_deref().unwrap_or(&[]);
        for &id in ids {
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
                // No DB wired — use id as name (stub path for tests).
                id.to_string()
            };
            dataset_pairs.push((id, name));
        }
    } else {
        // Use dataset names.
        let names = payload.datasets.as_deref().unwrap_or(&[]);
        for name in names {
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
                // No DB wired — generate deterministic id from name for stubs.
                Uuid::new_v5(&Uuid::NAMESPACE_OID, name.as_bytes())
            };
            dataset_pairs.push((id, name.clone()));
        }
    }

    let run_in_background = payload.run_in_background.unwrap_or(false);

    // Build a request-scoped ontology resolver from explicit payload keys.
    // If keys are provided and any key is unknown, return a non-200 error
    // instead of silently falling back to the no-op resolver.
    let request_ontology_resolver = if payload.ontology_key.is_some() {
        let manager = state
            .components()
            .ok_or_else(|| {
                ApiError::OntologyEnvelope(
                    "components not initialized".into(),
                    StatusCode::INTERNAL_SERVER_ERROR,
                )
            })?
            .ontology_manager
            .clone();

        resolve_request_ontology_resolver(&manager, user.id, payload.ontology_key.as_deref())
            .await?
    } else {
        None
    };

    // ── Build CognifyConfig from payload overrides ─────────────────────────────
    // Mirrors the CLI's defaults: ChunkStrategy::Paragraph, default
    // chunks_per_batch unless overridden, optional custom_prompt.
    let mut cognify_config = CognifyConfig::default().with_chunk_strategy(ChunkStrategy::Paragraph);
    if let Some(batch) = payload.chunks_per_batch {
        cognify_config = cognify_config.with_chunks_per_batch(batch.max(1) as usize);
    }
    if let Some(ref prompt) = payload.custom_prompt {
        cognify_config = cognify_config.with_custom_prompt(prompt.clone());
    }
    let cognify_config = Arc::new(cognify_config);

    // ── Per-dataset fan-out ────────────────────────────────────────────────────
    let mut response: CognifyResponseDTO = HashMap::new();
    let mut errors: Vec<(Uuid, String, String)> = Vec::new(); // (id, name, msg)

    for (dataset_id, dataset_name) in dataset_pairs {
        let components = state.components();
        let user_for_run = user.clone();
        let config_for_run = Arc::clone(&cognify_config);
        let ontology_resolver_for_run: Arc<dyn OntologyResolver> =
            if let Some(ref resolver) = request_ontology_resolver {
                Arc::clone(resolver)
            } else {
                components
                    .and_then(|c| c.ontology_resolver.clone())
                    .unwrap_or_else(|| Arc::new(NoOpOntologyResolver::new()))
            };
        let components_owned = components.cloned();

        let work = box_pipeline_future(async move {
            let Some(components) = components_owned else {
                return Err(CognifyDispatchError(
                    "Component handles not initialized; cannot run cognify pipeline".to_string(),
                ));
            };
            run_real_cognify(
                &components,
                &user_for_run,
                dataset_id,
                &config_for_run,
                ontology_resolver_for_run,
            )
            .await
        });

        match dispatch_pipeline(
            &state,
            &user,
            "cognify_pipeline",
            Some(dataset_id),
            run_in_background,
            work,
        )
        .await
        {
            Ok(DispatchOutcome::Blocking { outcome }) => {
                use cognee_core::pipeline_run_registry::RunPhase;
                match outcome.phase {
                    RunPhase::Completed | RunPhase::Pending => {
                        response.insert(
                            dataset_id.to_string(),
                            PipelineRunInfoDTO {
                                status: "PipelineRunCompleted".into(),
                                pipeline_run_id: outcome.run_id,
                                dataset_id,
                                dataset_name,
                                payload: None,
                                error: None,
                                data_ingestion_info: None,
                            },
                        );
                    }
                    RunPhase::Errored { message } => {
                        errors.push((dataset_id, dataset_name.clone(), message.clone()));
                        response.insert(
                            dataset_id.to_string(),
                            PipelineRunInfoDTO {
                                status: "PipelineRunErrored".into(),
                                pipeline_run_id: outcome.run_id,
                                dataset_id,
                                dataset_name,
                                payload: None,
                                error: Some(message),
                                data_ingestion_info: None,
                            },
                        );
                    }
                    RunPhase::Running => {
                        // Shouldn't happen for blocking runs, but handle gracefully.
                        response.insert(
                            dataset_id.to_string(),
                            PipelineRunInfoDTO {
                                status: "PipelineRunStarted".into(),
                                pipeline_run_id: outcome.run_id,
                                dataset_id,
                                dataset_name,
                                payload: None,
                                error: None,
                                data_ingestion_info: None,
                            },
                        );
                    }
                }
            }
            Ok(DispatchOutcome::Background { handle }) => {
                response.insert(
                    dataset_id.to_string(),
                    PipelineRunInfoDTO {
                        status: "PipelineRunStarted".into(),
                        pipeline_run_id: handle.run_id,
                        dataset_id,
                        dataset_name,
                        payload: None,
                        error: None,
                        data_ingestion_info: None,
                    },
                );
            }
            Err(e) => {
                return Err(e);
            }
        }
    }

    // ── Error aggregation (Python parity) ─────────────────────────────────────
    // If any dataset errored in blocking mode, return 500 with the aggregate.
    // Python raises on the first error; we surface all errors in the body.
    if !errors.is_empty() && !run_in_background {
        let error_detail = errors
            .iter()
            .map(|(id, name, msg)| format!("dataset {name} ({id}): {msg}"))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(ApiError::PipelineErrored {
            pipeline_source: PipelineErrorSource::Cognify,
            run_info: json!({
                "error": "Pipeline run errored",
                "detail": error_detail
            }),
        });
    }

    Ok((StatusCode::OK, Json(response)))
}

/// Build a per-request ontology resolver from explicit payload keys.
///
/// Guarantees user scoping (`user_id`) and key scoping (only provided keys).
/// Unknown keys are returned as a non-200 API error.
async fn resolve_request_ontology_resolver(
    manager: &cognee_ontology::OntologyManager,
    user_id: Uuid,
    ontology_keys: Option<&[String]>,
) -> Result<Option<Arc<dyn OntologyResolver>>, ApiError> {
    let Some(keys) = ontology_keys else {
        return Ok(None);
    };

    let normalized_keys: Vec<String> = keys
        .iter()
        .map(|k| k.trim().to_owned())
        .filter(|k| !k.is_empty())
        .collect();

    if normalized_keys.is_empty() {
        return Ok(None);
    }

    let key_refs: Vec<&str> = normalized_keys.iter().map(|k| k.as_str()).collect();
    let contents = manager
        .get_contents_batch(user_id, &key_refs)
        .await
        .map_err(|e| match e {
            cognee_ontology::OntologyError::NotFound(msg) => {
                ApiError::OntologyEnvelope(msg, StatusCode::NOT_FOUND)
            }
            cognee_ontology::OntologyError::InvalidFormat(msg) => {
                ApiError::OntologyEnvelope(msg, StatusCode::BAD_REQUEST)
            }
            other => {
                ApiError::OntologyEnvelope(other.to_string(), StatusCode::INTERNAL_SERVER_ERROR)
            }
        })?;

    let readers: Vec<Box<dyn std::io::Read>> = contents
        .into_iter()
        .map(|bytes| Box::new(Cursor::new(bytes)) as Box<dyn std::io::Read>)
        .collect();

    let resolver =
        RdfLibOntologyResolver::new(OntologyFileInput::Readers(readers)).map_err(|e| {
            ApiError::OntologyEnvelope(
                format!("Failed to construct ontology resolver: {e}"),
                StatusCode::BAD_REQUEST,
            )
        })?;

    if !resolver.is_loaded() {
        return Err(ApiError::OntologyEnvelope(
            "No valid ontology content could be loaded for the provided ontology keys".into(),
            StatusCode::BAD_REQUEST,
        ));
    }

    Ok(Some(Arc::new(resolver)))
}

// ─── Cognify execution helpers ───────────────────────────────────────────────

/// Boxed-future-compatible error type for the cognify pipeline path.
///
/// `dispatch_pipeline` expects `Box<dyn Error + Send + Sync>`; this wrapper
/// carries the underlying message back to the registry so it surfaces in the
/// `RunPhase::Errored { message }` payload.
#[derive(Debug)]
struct CognifyDispatchError(String);

impl std::fmt::Display for CognifyDispatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CognifyDispatchError {}

/// Drive a single dataset through the cognify pipeline.
///
/// Resolves the dataset's `Data` rows, picks up `Llm` / `EmbeddingEngine` /
/// `GraphDBTrait` / `VectorDB` / `CpuPool` handles from `ComponentHandles`,
/// and delegates to [`cognee_cognify::cognify`].
///
/// Surfaces missing-handle and runtime errors as `CognifyDispatchError` so the
/// registry's `RunPhase::Errored { message }` carries the underlying cause.
async fn run_real_cognify(
    components: &ComponentHandles,
    user: &AuthenticatedUser,
    dataset_id: Uuid,
    config: &CognifyConfig,
    ontology_resolver: Arc<dyn OntologyResolver>,
) -> Result<(), CognifyDispatchError> {
    // ── Pull required backend handles ─────────────────────────────────────────
    let llm = components
        .llm
        .clone()
        .ok_or_else(|| CognifyDispatchError("llm not wired in ComponentHandles".into()))?;
    let graph_db = components
        .graph_db
        .clone()
        .ok_or_else(|| CognifyDispatchError("graph_db not wired in ComponentHandles".into()))?;
    let vector_db = components
        .vector_db
        .clone()
        .ok_or_else(|| CognifyDispatchError("vector_db not wired in ComponentHandles".into()))?;
    let embedding_engine = components.embedding_engine.clone().ok_or_else(|| {
        CognifyDispatchError("embedding_engine not wired in ComponentHandles".into())
    })?;
    let thread_pool = components
        .thread_pool
        .clone()
        .ok_or_else(|| CognifyDispatchError("thread_pool not wired in ComponentHandles".into()))?;

    let storage = components.storage.clone();
    let database = components.database.clone();

    // Apply the audio transcriber when available (D3/T8).
    let effective_config;
    let config = if let Some(ref t) = components.transcriber {
        effective_config = config.clone().with_transcriber(Arc::clone(t));
        &effective_config
    } else {
        config
    };

    // ── Resolve dataset data rows ─────────────────────────────────────────────
    let data_items = db_ops::datasets::get_dataset_data(&database, dataset_id)
        .await
        .map_err(|e| {
            CognifyDispatchError(format!("failed to load data for dataset {dataset_id}: {e}"))
        })?;

    // Best-effort user_email lookup for provenance stamping (matches CLI).
    let user_email = database
        .get_user(user.id)
        .await
        .ok()
        .flatten()
        .map(|u| u.email);

    // Gap 08-07: the outer `dispatch_pipeline` already wires
    // `DefaultPipelineRunRegistry` (backed by `SeaOrmPipelineRunRepository`)
    // for the four-state `pipeline_runs` trail. Hand the inner cognify a
    // no-op repo so its `DbPipelineWatcher` does not produce a second row-set.
    let pipeline_run_repo = NoopPipelineRunRepository::arc();

    run_cognify(
        data_items,
        dataset_id,
        Some(user.id),
        user_email,
        user.tenant_id,
        llm,
        storage,
        graph_db,
        vector_db,
        embedding_engine,
        database,
        pipeline_run_repo,
        thread_pool,
        ontology_resolver,
        config,
    )
    .await
    .map_err(|e| CognifyDispatchError(format!("cognify failed: {e}")))?;

    Ok(())
}

// ─── ws_subscribe ─────────────────────────────────────────────────────────────

/// `GET /api/v1/cognify/subscribe/{pipeline_run_id}`
///
/// WebSocket live-stream of events for a cognify pipeline run.
///
/// Per Python parity ([websocket.md §9.2](../../../docs/http-server/websocket.md#92-why-we-accept-the-upgrade-before-auth)),
/// the HTTP upgrade is accepted *before* authentication. The request headers
/// (carrying the auth cookie) are captured before the upgrade and passed into
/// the async loop. Authentication happens post-upgrade inside `ws_loop`.
///
/// Terminal behaviour (strict Python parity):
/// - `PipelineRunCompleted` → send TEXT frame, then Close `1000`.
/// - `PipelineRunErrored` → send TEXT frame, **continue looping** (no close).
/// - `PipelineRunAlreadyCompleted` → send TEXT frame, **continue looping** (no close).
/// - Auth failure → Close `1008 "Unauthorized"`.
/// - Channel lag → Close `1011 "channel lagged"`.
pub async fn ws_subscribe(
    State(state): State<AppState>,
    Path(pipeline_run_id): Path<Uuid>,
    // Capture the request headers *before* the upgrade so we can authenticate
    // inside the WebSocket loop.  Python reads the cookie on the established
    // connection ([websocket.md §4](../../../docs/http-server/websocket.md#4-authentication)).
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |mut socket| async move {
        use axum::extract::ws::Message;

        // ── Post-upgrade authentication (Python parity) ────────────────────────
        // The auth cookie was present in the original HTTP upgrade headers.
        // Authenticate now that the WebSocket is established, matching Python's
        // `websocket.accept()` → cookie read → close on failure flow.
        if let Some(ref auth) = state.auth {
            use crate::auth::cookie::authenticate_from_cookie;
            if authenticate_from_cookie(&headers, auth).await.is_none() {
                // Any auth failure → close 1008 with reason "Unauthorized"
                // (literal UTF-8, not JSON per websocket.md §7).
                let _ = socket
                    .send(Message::Close(Some(axum::extract::ws::CloseFrame {
                        code: 1008,
                        reason: "Unauthorized".into(),
                    })))
                    .await;
                return;
            }
        }
        // Note: when state.auth is None (e.g. in unit tests), no auth is
        // performed — the handler behaves as if auth is disabled.

        // ── Subscribe to the registry ──────────────────────────────────────────
        // `subscribe` is infallible — returns a stream (possibly empty for
        // unknown run ids; the stream ends immediately in that case).
        use futures::StreamExt as _;
        let mut rx = state.pipelines.subscribe(pipeline_run_id);

        // ── Recover the run's `dataset_id` and `user_id` ───────────────────────
        // The registry trait does not expose the per-run handle, so we query the
        // pipeline_runs table directly via `SeaOrmPipelineRunRepository`. The
        // result is cached for the lifetime of this subscription so we don't
        // re-query on every event.
        let dataset_user = if let Some(components) = state.components() {
            use cognee_database::{PipelineRunRepository, SeaOrmPipelineRunRepository};
            let repo = SeaOrmPipelineRunRepository::new(components.database.clone());
            match repo.get_pipeline_run(pipeline_run_id).await {
                Ok(Some(run)) => {
                    // owner / user_id is not stored on the run row; fall back to
                    // a nil UUID — the formatter currently ignores user_id, but
                    // we still pass a real value if one becomes recoverable.
                    Some((run.dataset_id, Uuid::nil()))
                }
                Ok(None) | Err(_) => None,
            }
        } else {
            None
        };

        // ── Forward events ─────────────────────────────────────────────────────
        // Strict Python parity (websocket.md §6):
        // - PipelineRunCompleted → forward TEXT frame + Close 1000.
        // - PipelineRunErrored / PipelineRunAlreadyCompleted → forward TEXT frame,
        //   continue looping. Do NOT close on these — Python only closes on Completed.
        use cognee_core::pipeline_run_registry::RunEventKind;
        while let Some(event) = rx.next().await {
            let status = event_kind_to_python_string(&event.kind).to_owned();
            // `formatted_graph_data` is called on every event (including Yield) —
            // wasteful but matches Python parity (websocket.md §5.3).
            let graph_payload = if let Some(components) = state.components() {
                let (dataset_id, user_id) = dataset_user.unwrap_or((None, Uuid::nil()));
                components
                    .formatted_graph_data(dataset_id, user_id)
                    .await
                    .unwrap_or_else(|err| {
                        tracing::warn!(
                            "formatted_graph_data failed for run {}: {}",
                            pipeline_run_id,
                            err
                        );
                        json!({"nodes": [], "edges": []})
                    })
            } else {
                json!({})
            };

            let frame = CognifyWsFrameDTO {
                pipeline_run_id,
                status: status.clone(),
                payload: graph_payload,
            };

            if let Ok(text) = serde_json::to_string(&frame)
                && socket.send(Message::Text(text.into())).await.is_err()
            {
                // Client disconnected — stop forwarding.
                return;
            }

            match event.kind {
                RunEventKind::Completed => {
                    // Only PipelineRunCompleted closes the socket (Python parity).
                    let _ = socket
                        .send(Message::Close(Some(axum::extract::ws::CloseFrame {
                            code: 1000,
                            reason: "".into(),
                        })))
                        .await;
                    return;
                }
                // PipelineRunErrored and PipelineRunAlreadyCompleted are forwarded
                // but the loop continues — Python never closes on these statuses.
                // See websocket.md §6 and §6.1 "Do not close the WebSocket on
                // PipelineRunErrored."
                RunEventKind::Errored { .. } | RunEventKind::AlreadyCompleted => {
                    // Forward done; continue the loop.
                }
                _ => {
                    // PipelineRunStarted / PipelineRunYield — continue streaming.
                }
            }
        }
        // Channel closed (producer done or run evicted) — natural end.
    })
}

// ─── router ──────────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", post(post_cognify))
        .route("/subscribe/{pipeline_run_id}", get(ws_subscribe))
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
        body::Body,
        http::{Request, StatusCode, header},
    };
    use serde_json::json;
    use tempfile::tempdir;
    use tower::ServiceExt;

    /// Build a minimal app state without backends for validation tests.
    async fn test_state() -> AppState {
        AppState::build(crate::config::HttpServerConfig::default())
            .await
            .expect("AppState::build")
    }

    /// Build the cognify router with a test state (no auth required for POST
    /// because tests inject an `AuthenticatedUser` via the extractor's default
    /// path; here we test via the full tower stack which will fail auth — so we
    /// test the shape of 401 responses or use route-unit tests instead).
    ///
    /// For the POST handler we test the validation branch directly.
    #[tokio::test]
    async fn post_cognify_empty_body_returns_bad_request() {
        let state = test_state().await;
        let app = Router::new()
            .route("/", post(post_cognify_no_auth))
            .with_state(state);

        let req = Request::builder()
            .method("POST")
            .uri("/")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"datasets": [], "dataset_ids": []}"#))
            .unwrap();

        let resp = app.oneshot(req).await.expect("oneshot");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// Bypass auth for unit testing the handler logic.
    async fn post_cognify_no_auth(
        State(state): State<AppState>,
        Json(payload): Json<CognifyPayloadDTO>,
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
        post_cognify(user, State(state), Json(payload)).await
    }

    #[tokio::test]
    async fn post_cognify_null_datasets_returns_bad_request() {
        let state = test_state().await;
        let app = Router::new()
            .route("/", post(post_cognify_no_auth))
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

    /// Blocking dispatch without backends wired now reaches the real cognify
    /// path and surfaces the missing-component error through the
    /// `Pipeline run errored` envelope. This is the post-implementation
    /// behaviour — before the cognify wiring landed, the stub returned 200.
    #[tokio::test]
    async fn post_cognify_with_dataset_ids_surfaces_missing_components() {
        let state = test_state().await;
        let app = Router::new()
            .route("/", post(post_cognify_no_auth))
            .with_state(state);

        let dataset_id = Uuid::new_v4();
        let body = json!({
            "dataset_ids": [dataset_id],
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
    async fn post_cognify_with_dataset_names_surfaces_missing_components() {
        let state = test_state().await;
        let app = Router::new()
            .route("/", post(post_cognify_no_auth))
            .with_state(state);

        let body = json!({
            "datasets": ["my_dataset"],
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
    }

    #[tokio::test]
    async fn post_cognify_background_returns_started() {
        let state = test_state().await;
        let app = Router::new()
            .route("/", post(post_cognify_no_auth))
            .with_state(state);

        let dataset_id = Uuid::new_v4();
        let body = json!({
            "dataset_ids": [dataset_id],
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
        let entry = &v[dataset_id.to_string()];
        assert_eq!(entry["status"], "PipelineRunStarted");
    }

    /// Python parity: validation 400 body uses `{"error": "..."}` not `{"detail": "..."}`.
    /// Source: cognify.md §2.1 error table.
    #[tokio::test]
    async fn post_cognify_empty_datasets_body_uses_error_key() {
        let state = test_state().await;
        let app = Router::new()
            .route("/", post(post_cognify_no_auth))
            .with_state(state);

        let req = Request::builder()
            .method("POST")
            .uri("/")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{}"#))
            .unwrap();

        let resp = app.oneshot(req).await.expect("oneshot");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        // Must use "error" key, not "detail".
        assert_eq!(
            v["error"], "No datasets or dataset_ids provided",
            "validation body must use 'error' key per Python parity"
        );
        assert!(
            v.get("detail").is_none(),
            "validation body must NOT have 'detail' key"
        );
    }

    /// Python parity: `dataset_ids` overrides `datasets` — handler resolves only
    /// the UUID list (does not merge the two).
    ///
    /// Uses `run_in_background=true` to assert the response shape without
    /// running the real cognify pipeline (which would 500 without backends).
    #[tokio::test]
    async fn post_cognify_dataset_ids_overrides_datasets() {
        let state = test_state().await;
        let app = Router::new()
            .route("/", post(post_cognify_no_auth))
            .with_state(state);

        let dataset_id = Uuid::new_v4();
        let body = json!({
            // Both present — dataset_ids must win (Python parity).
            "datasets": ["name_that_should_be_ignored"],
            "dataset_ids": [dataset_id],
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
        // Response must be keyed by the UUID, not by "name_that_should_be_ignored".
        assert!(
            v.get(dataset_id.to_string()).is_some(),
            "response must be keyed by the UUID from dataset_ids"
        );
        assert!(
            v.get("name_that_should_be_ignored").is_none(),
            "datasets list must be ignored when dataset_ids is present"
        );
    }

    #[tokio::test]
    async fn resolve_request_ontology_resolver_unknown_key_returns_not_found() {
        let dir = tempdir().expect("tempdir");
        let manager = cognee_ontology::OntologyManager::new(dir.path());
        let user_id = Uuid::new_v4();

        let result = resolve_request_ontology_resolver(
            &manager,
            user_id,
            Some(&["missing-key".to_string()]),
        )
        .await;

        match result {
            Err(ApiError::OntologyEnvelope(msg, status)) => {
                assert_eq!(status, StatusCode::NOT_FOUND);
                assert!(msg.contains("missing-key") || msg.contains("not found"));
            }
            Err(other) => panic!("expected OntologyEnvelope 404, got {other:?}"),
            Ok(_) => panic!("expected error for missing ontology key"),
        }
    }

    #[tokio::test]
    async fn resolve_request_ontology_resolver_is_user_scoped() {
        let dir = tempdir().expect("tempdir");
        let manager = cognee_ontology::OntologyManager::new(dir.path());

        let user_with_upload = Uuid::new_v4();
        let other_user = Uuid::new_v4();
        let ontology = br#"@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<http://example.org/Vehicle> a owl:Class ;
    rdfs:label \"Vehicle\" .
"#;

        manager
            .upload(user_with_upload, "vehicles", "vehicles.ttl", ontology, None)
            .await
            .expect("upload ontology");

        let result = resolve_request_ontology_resolver(
            &manager,
            other_user,
            Some(&["vehicles".to_string()]),
        )
        .await;

        match result {
            Err(ApiError::OntologyEnvelope(_, status)) => {
                assert_eq!(status, StatusCode::NOT_FOUND);
            }
            Err(other) => panic!("expected OntologyEnvelope 404, got {other:?}"),
            Ok(_) => panic!("expected user-scoped key lookup to fail"),
        }
    }
}
