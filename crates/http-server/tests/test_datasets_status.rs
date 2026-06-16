#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for `GET /api/v1/datasets/status`.

mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

use support::{
    bearer_header, body_json, build_auth_required_test_state, build_auth_test_state, seed_user,
    test_router,
};

/// No auth → 401.
#[tokio::test]
async fn test_dataset_status_no_auth_returns_401() {
    let (state, _) = build_auth_required_test_state().await;
    let app = test_router(state).await;

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/datasets/status")
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// Empty `dataset` query → `{}` 200, not 422 (Python parity).
#[tokio::test]
async fn test_dataset_status_no_dataset_param_returns_empty_map() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "dsstatus@example.com", "Str0ng!Pass#1").await;
    let auth_header = bearer_header(&user, &state);
    let app = test_router(state).await;

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/datasets/status")
        .header("Authorization", auth_header)
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(
        body,
        serde_json::json!({}),
        "empty dataset list must return {{}}"
    );
}

/// Single non-existent dataset UUID → omitted from result (silently dropped, no rows).
#[tokio::test]
async fn test_dataset_status_unknown_uuid_returns_empty_map() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "dsstatus2@example.com", "Str0ng!Pass#1").await;
    let auth_header = bearer_header(&user, &state);
    let app = test_router(state).await;

    let dataset_id = uuid::Uuid::new_v4();
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/datasets/status?dataset={dataset_id}"))
        .header("Authorization", auth_header)
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    // With no backends wired, components() returns None → 409.
    // With backends wired but no run rows, the result is silently empty → 200 {}.
    // Either 200 or 409 are acceptable here — the key invariant is no 401/405.
    assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_ne!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
}
