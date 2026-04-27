//! `POST /api/v1/cognify` — run the cognify knowledge-graph extraction pipeline.
//! `GET  /api/v1/cognify/subscribe/{pipeline_run_id}` — WebSocket live stream.
//!
//! Python parity: `cognee/api/v1/cognify/routers/get_cognify_router.py`.

use std::collections::HashMap;

use axum::{
    Json, Router,
    extract::{Path, State, WebSocketUpgrade},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use cognee_database::IngestDb;
use serde_json::json;
use uuid::Uuid;

use crate::auth::AuthenticatedUser;
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

    // ── Per-dataset fan-out ────────────────────────────────────────────────────
    let mut response: CognifyResponseDTO = HashMap::new();
    let mut errors: Vec<(Uuid, String, String)> = Vec::new(); // (id, name, msg)

    for (dataset_id, dataset_name) in dataset_pairs {
        // The actual cognify work (LLM + graph + vector) requires components not
        // yet wired in the http-server (cognee-lib cycle prevention).
        // This is a documented blocking gap: the stub returns Ok(()) immediately.
        // TODO(P5): wire real cognify() call once ComponentHandles gains LLM/graph/vector handles.
        let work = box_pipeline_future(async move {
            // Blocking gap stub — always succeeds.
            Ok::<(), std::io::Error>(())
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
            let graph_payload = state
                .components()
                .map(|c| {
                    // Blocking gap: the real formatted_graph_data is a TODO(P5).
                    // The stub on ComponentHandles already returns the right shape.
                    let _ = c;
                    json!({"nodes": [], "edges": []})
                })
                .unwrap_or(json!({}));

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
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode, header},
    };
    use serde_json::json;
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

    #[tokio::test]
    async fn post_cognify_with_dataset_ids_dispatches() {
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
        // No DB wired — dataset lookup returns id.to_string() as name → succeeds.
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn post_cognify_with_dataset_names_no_db_dispatches() {
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
        // No DB wired — deterministic uuid5 name mapping used → succeeds.
        assert_eq!(resp.status(), StatusCode::OK);
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
            "run_in_background": false
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
}
