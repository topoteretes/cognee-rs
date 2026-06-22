#![cfg(any())] // cognee-http-server gated on oss-split branch (T2-move §4 S2).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for `GET /api/v1/datasets/{id}/data/{did}/raw`.
//!
//! Full round-trips (add → fetch raw → assert bytes) require wired backends.
//! Tests here cover auth guards and routing.

mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

use support::{
    bearer_header, build_auth_required_test_state, build_auth_test_state, seed_user, test_router,
};

/// No auth → 401.
#[tokio::test]
async fn test_get_raw_no_auth_returns_401() {
    let (state, _) = build_auth_required_test_state().await;
    let app = test_router(state).await;

    let dataset_id = uuid::Uuid::new_v4();
    let data_id = uuid::Uuid::new_v4();

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/datasets/{dataset_id}/data/{data_id}/raw"))
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// Authenticated with no backends → fails past auth with a non-2xx code.
///
/// The exact code depends on the error path (404 if dataset not found, 500 if DB
/// unavailable). This test simply verifies the route is reachable past the auth
/// guard and no 401/405 is returned.
#[tokio::test]
async fn test_get_raw_authenticated_route_exists() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "raw_user@example.com", "Str0ng!Pass#1").await;
    let auth_header = bearer_header(&user, &state);
    let app = test_router(state).await;

    let dataset_id = uuid::Uuid::new_v4();
    let data_id = uuid::Uuid::new_v4();

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/datasets/{dataset_id}/data/{data_id}/raw"))
        .header("Authorization", auth_header)
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_ne!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "authenticated request must not return 401"
    );
    assert_ne!(
        resp.status(),
        StatusCode::METHOD_NOT_ALLOWED,
        "route must be registered"
    );
}
