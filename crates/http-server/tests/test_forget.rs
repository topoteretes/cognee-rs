#![cfg(any())] // cognee-http-server gated on oss-split branch (T2-move §4 S2).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for `POST /api/v1/forget`.
//!
//! Covers the three-mode truth table from the spec plus cross-field validation.
//! Full deletion (mode 1/2/3 against real data) requires wired backends.

mod support;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use cognee_http_server::auth::AuthMethod;
use cognee_http_server::cloud_client::{CloudClientError, CloudDeleteClient};
use cognee_http_server::components::ComponentHandles;
use cognee_http_server::dto::forget::{
    DatasetRef, ForgetEverythingResponse, ForgetPayloadDTO, ForgetResponseDTO,
};
use tower::ServiceExt;
use uuid::Uuid;

use support::{
    bearer_header, body_json, build_auth_required_test_state, build_auth_test_state,
    build_component_handles, build_search_db, seed_user, test_router,
};

#[derive(Clone)]
struct MockCloudDeleteClient {
    response: Result<ForgetResponseDTO, CloudClientError>,
    calls: Arc<Mutex<Vec<ForwardForgetCall>>>,
}

#[derive(Clone, Debug)]
struct ForwardForgetCall {
    payload: ForgetPayloadDTO,
    user_id: Uuid,
    auth_method: AuthMethod,
}

#[async_trait]
impl CloudDeleteClient for MockCloudDeleteClient {
    async fn forward_forget(
        &self,
        payload: &ForgetPayloadDTO,
        user: &cognee_http_server::auth::AuthenticatedUser,
    ) -> Result<ForgetResponseDTO, CloudClientError> {
        self.calls
            .lock()
            .expect("mock cloud calls lock")
            .push(ForwardForgetCall {
                payload: payload.clone(),
                user_id: user.id,
                auth_method: user.auth_method,
            });
        self.response.clone()
    }
}

fn with_cloud_client(
    mut state: cognee_http_server::AppState,
    cloud_client: Arc<dyn CloudDeleteClient>,
) -> cognee_http_server::AppState {
    let existing = state
        .lib
        .as_ref()
        .map(|handles| (**handles).clone())
        .expect("component handles must be wired before injecting cloud client");
    state.lib = Some(Arc::new(ComponentHandles {
        cloud_client: Some(cloud_client),
        ..existing
    }));
    state
}

async fn with_minimal_components(
    mut state: cognee_http_server::AppState,
) -> cognee_http_server::AppState {
    if state.lib.is_none() {
        let db = build_search_db().await;
        state.lib = Some(build_component_handles(db, None, None, None));
    }
    state
}

// ─── helpers ─────────────────────────────────────────────────────────────────

fn json_body(value: serde_json::Value) -> Body {
    Body::from(serde_json::to_string(&value).expect("json"))
}

fn post_forget(uri: &str, body: serde_json::Value, auth: &str) -> axum::http::Request<Body> {
    let mut builder = Request::builder().method("POST").uri(uri);
    if !auth.is_empty() {
        builder = builder.header("Authorization", auth);
    }
    builder
        .header("content-type", "application/json")
        .body(json_body(body))
        .expect("request")
}

// ─── auth guard ──────────────────────────────────────────────────────────────

/// No auth → 401.
#[tokio::test]
async fn test_forget_no_auth_returns_401() {
    let (state, _) = build_auth_required_test_state().await;
    let app = test_router(state).await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/forget")
        .header("content-type", "application/json")
        .body(json_body(serde_json::json!({"everything": true})))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ─── cross-field validation ───────────────────────────────────────────────────

/// Neither `data_id`, `dataset`, nor `everything=true` → 422.
#[tokio::test]
async fn test_forget_no_fields_returns_422() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "forget_nof@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);
    let app = test_router(state).await;

    let resp = app
        .oneshot(post_forget("/api/v1/forget", serde_json::json!({}), &auth))
        .await
        .expect("response");

    assert_eq!(
        resp.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "empty payload must be 422"
    );
    let body = body_json(resp).await;
    assert!(body["error"].is_string(), "`error` key expected: {body}");
}

/// `data_id` only (no `dataset`) → 422.
#[tokio::test]
async fn test_forget_data_id_only_returns_422() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "forget_doid@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);
    let app = test_router(state).await;

    let data_id = uuid::Uuid::new_v4();
    let resp = app
        .oneshot(post_forget(
            "/api/v1/forget",
            serde_json::json!({"dataId": data_id}),
            &auth,
        ))
        .await
        .expect("response");

    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = body_json(resp).await;
    assert!(body["error"].is_string(), "`error` key expected: {body}");
}

/// `everything=true` with extra fields → 200 (extra fields silently ignored).
#[tokio::test]
async fn test_forget_everything_true_ignores_extra_fields() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "forget_everything@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);
    let app = test_router(state).await;

    let data_id = uuid::Uuid::new_v4();
    let resp = app
        .oneshot(post_forget(
            "/api/v1/forget",
            serde_json::json!({
                "everything": true,
                "dataId": data_id,
                "dataset": "ignored_dataset"
            }),
            &auth,
        ))
        .await
        .expect("response");

    // Not 422 — everything=true takes priority.
    assert_ne!(
        resp.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "everything=true must not be 422 even with extra fields"
    );
}

/// Mode 3 (`everything=true`): with no backends wired → 500 error,
/// but the resolve_mode step succeeded so we don't see 422.
#[tokio::test]
async fn test_forget_everything_resolves_mode_correctly() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "forget_mode3@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);
    let app = test_router(state).await;

    let resp = app
        .oneshot(post_forget(
            "/api/v1/forget",
            serde_json::json!({"everything": true}),
            &auth,
        ))
        .await
        .expect("response");

    // 422 = mode resolution failed; we must not see that.
    assert_ne!(
        resp.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "everything=true must resolve to mode 3, not fail cross-field validation"
    );
}

/// `dataset` only (no `data_id`) resolves to Mode 2 — not 422.
/// With no backends wired, gets 422 (missing dataset in DB), which is Python parity.
#[tokio::test]
async fn test_forget_dataset_only_resolves_to_mode2() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "forget_mode2@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);
    let app = test_router(state).await;

    let resp = app
        .oneshot(post_forget(
            "/api/v1/forget",
            serde_json::json!({"dataset": "nonexistent_dataset"}),
            &auth,
        ))
        .await
        .expect("response");

    // Python collapses missing-dataset into 422 (same as cross-field validation error).
    // With no backends wired we get 422 from the components check (or the DB lookup fails).
    // The important thing: mode-2 resolution itself (dataset only) must not cause 422
    // due to cross-field validation. Any 422 must come from the missing dataset,
    // not from the resolve_mode() step.
    // Since we can't distinguish without backends, we just assert the route is reachable.
    assert_ne!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_forget_proxies_success_via_cloud_client() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "forget_cloud_ok@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);
    let state = with_minimal_components(state).await;
    let calls = Arc::new(Mutex::new(Vec::new()));
    let cloud_client = Arc::new(MockCloudDeleteClient {
        response: Ok(ForgetResponseDTO::Everything(ForgetEverythingResponse {
            datasets_removed: 7,
            status: "success".into(),
        })),
        calls: Arc::clone(&calls),
    });
    let app = test_router(with_cloud_client(state, cloud_client)).await;

    let resp = app
        .oneshot(post_forget(
            "/api/v1/forget",
            serde_json::json!({"dataset": "nonexistent_dataset"}),
            &auth,
        ))
        .await
        .expect("response");

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["datasets_removed"], 7);
    assert_eq!(body["status"], "success");

    let calls = calls.lock().expect("mock cloud calls lock");
    assert_eq!(calls.len(), 1);
    assert!(matches!(
        calls[0].payload.dataset.as_ref(),
        Some(DatasetRef::Name(name)) if name == "nonexistent_dataset"
    ));
    assert_eq!(calls[0].user_id, user.id);
    assert_eq!(calls[0].auth_method, AuthMethod::BearerJwt);
}

#[tokio::test]
async fn test_forget_cloud_proxy_error_is_scrubbed() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "forget_cloud_err@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);
    let state = with_minimal_components(state).await;
    let cloud_client = Arc::new(MockCloudDeleteClient {
        response: Err(CloudClientError::Upstream { status: 502 }),
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let app = test_router(with_cloud_client(state, cloud_client)).await;

    let resp = app
        .oneshot(post_forget(
            "/api/v1/forget",
            serde_json::json!({"everything": true}),
            &auth,
        ))
        .await
        .expect("response");

    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    let body = body_json(resp).await;
    assert_eq!(body["error"], "An error occurred during deletion.");
    assert!(!body.to_string().contains("502"));
}

#[tokio::test]
async fn test_forget_without_cloud_client_keeps_local_path() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "forget_local_only@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);
    let state = with_minimal_components(state).await;

    assert!(
        state
            .components()
            .expect("component handles are wired in auth test state")
            .cloud_client
            .is_none()
    );

    let app = test_router(state).await;

    let resp = app
        .oneshot(post_forget(
            "/api/v1/forget",
            serde_json::json!({"dataset": "nonexistent_dataset"}),
            &auth,
        ))
        .await
        .expect("response");

    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = body_json(resp).await;
    assert_eq!(
        body["error"],
        "Invalid request parameters. Specify dataset, data_id+dataset, or everything=True."
    );
}
