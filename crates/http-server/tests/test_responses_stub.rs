//! Integration tests for `POST /api/v1/responses` (Stage-A 501 stub).
//!
//! Covers:
//! - 401 when unauthenticated
//! - 501 with `{"detail": "...", "code": "..."}` for any authenticated request
//! - Field order: detail before code (Python JSONResponse parity)
//! - Content-Type is `application/json`

mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

use support::{
    bearer_header, build_auth_required_test_state, build_notebooks_state, seed_perm_user,
    test_router,
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

// ─── 501 stub ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn responses_authenticated_returns_501() {
    let (state, _) = build_notebooks_state().await;
    let user = seed_perm_user(&state, "resp_stub@example.com", "Str0ng!Pass#1").await;
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
    assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
}

#[tokio::test]
async fn responses_501_field_order_detail_before_code() {
    let (state, _) = build_notebooks_state().await;
    let user = seed_perm_user(&state, "resp_order@example.com", "Str0ng!Pass#1").await;
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
    assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);

    // Field order is load-bearing: detail must come before code.
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    let body_str = std::str::from_utf8(&bytes).expect("utf8");
    assert_eq!(
        body_str,
        r#"{"detail":"OpenAI Responses API surface is not implemented in this build","code":"RESPONSES_NOT_IMPLEMENTED"}"#,
    );
}

#[tokio::test]
async fn responses_501_content_type_is_json() {
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
