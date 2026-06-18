#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for `POST /api/v1/responses` (Stage B).
//!
//! Covers:
//! - 401 when unauthenticated
//! - Auth + validation envelopes are preserved
//! - The handler no longer returns 501 — instead surfaces a `500 {"detail": ...}`
//!   envelope when no `ResponsesClient` is wired (the integration tests
//!   exercise the success path against a real upstream).
//! - Content-Type is `application/json`

mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

use support::{
    bearer_header, body_json, build_auth_required_test_state, build_notebooks_state,
    seed_perm_user, test_router,
};

// ─── Auth guard ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn responses_no_auth_returns_401() {
    let (state, _) = build_auth_required_test_state().await;
    let app = test_router(state).await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/responses")
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"model":"cognee-v1","input":"hello"}"#))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ─── Stage B happy / unwired paths ───────────────────────────────────────────

/// With no `responses_client` wired the handler must NOT return 501 — that
/// would be a regression to Stage A. It instead surfaces a 500 with the
/// canonical `{"detail": "..."}` envelope.
#[tokio::test]
async fn responses_authenticated_no_responses_client_returns_500_not_501() {
    let (state, _) = build_notebooks_state().await;
    let user = seed_perm_user(&state, "resp_unwired@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);
    let app = test_router(state).await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/responses")
        .header("Authorization", &auth)
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"model":"cognee-v1","input":"hello"}"#))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_ne!(
        resp.status(),
        StatusCode::NOT_IMPLEMENTED,
        "Stage B handler must not return 501"
    );
    // When components are wired but the responses client is None we return 500.
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn responses_authenticated_no_responses_client_uses_detail_envelope() {
    let (state, _) = build_notebooks_state().await;
    let user = seed_perm_user(&state, "resp_envelope@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);
    let app = test_router(state).await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/responses")
        .header("Authorization", &auth)
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"model":"cognee-v1","input":"hello"}"#))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_ne!(resp.status(), StatusCode::NOT_IMPLEMENTED);
    let body = body_json(resp).await;
    assert!(
        body.get("detail").is_some(),
        "expected canonical detail envelope, got {body}"
    );
}

#[tokio::test]
async fn responses_authenticated_content_type_is_json() {
    let (state, _) = build_notebooks_state().await;
    let user = seed_perm_user(&state, "resp_ct@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);
    let app = test_router(state).await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/responses")
        .header("Authorization", &auth)
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"model":"cognee-v1","input":"hi"}"#))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.contains("application/json"), "content-type: {ct}");
}
