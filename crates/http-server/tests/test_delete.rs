#![cfg(any())] // cognee-http-server gated on oss-split branch (T2-move §4 S2).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for `DELETE /api/v1/delete` (deprecated endpoint).
//!
//! Verifies: auth guard, deprecation headers on every response (success and error),
//! and 409 catch-all error shape.

mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

use support::{
    bearer_header, build_auth_required_test_state, build_auth_test_state, seed_user, test_router,
};

// ─── auth guard ──────────────────────────────────────────────────────────────

/// No auth → 401.
#[tokio::test]
async fn test_deprecated_delete_no_auth_returns_401() {
    let (state, _) = build_auth_required_test_state().await;
    let app = test_router(state).await;

    let data_id = uuid::Uuid::new_v4();
    let dataset_id = uuid::Uuid::new_v4();

    let req = Request::builder()
        .method("DELETE")
        .uri(format!(
            "/api/v1/delete?data_id={data_id}&dataset_id={dataset_id}"
        ))
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ─── deprecation headers ──────────────────────────────────────────────────────

/// Every response (success or error) must carry RFC 8594 deprecation headers.
///
/// With no backends wired the handler returns 409 (error path). We still
/// assert the Deprecation header is present — verifying the header helper runs
/// on the error path too.
#[tokio::test]
async fn test_deprecated_delete_error_response_has_deprecation_headers() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "del_dep@example.com", "Str0ng!Pass#1").await;
    let auth_header = bearer_header(&user, &state);
    let app = test_router(state).await;

    let data_id = uuid::Uuid::new_v4();
    let dataset_id = uuid::Uuid::new_v4();

    let req = Request::builder()
        .method("DELETE")
        .uri(format!(
            "/api/v1/delete?data_id={data_id}&dataset_id={dataset_id}"
        ))
        .header("Authorization", auth_header)
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");

    // Must carry Deprecation header regardless of success/error.
    let deprecation = resp.headers().get("Deprecation");
    assert!(
        deprecation.is_some(),
        "Deprecation header must be present on every response"
    );
    assert_eq!(
        deprecation.unwrap().to_str().unwrap(),
        "true",
        "Deprecation header must be 'true'"
    );

    // Sunset header.
    let sunset = resp.headers().get("Sunset");
    assert!(
        sunset.is_some(),
        "Sunset header must be present on every response"
    );

    // Link header pointing to successor.
    let link = resp.headers().get("Link");
    assert!(
        link.is_some(),
        "Link header must be present on every response"
    );
    let link_val = link.unwrap().to_str().unwrap();
    assert!(
        link_val.contains("successor-version"),
        "Link header must reference successor-version: {link_val}"
    );
}

/// Error response must have 409 status and `{"error": ...}` shape.
#[tokio::test]
async fn test_deprecated_delete_no_backends_returns_409_with_error_key() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "del_err@example.com", "Str0ng!Pass#1").await;
    let auth_header = bearer_header(&user, &state);
    let app = test_router(state).await;

    let data_id = uuid::Uuid::new_v4();
    let dataset_id = uuid::Uuid::new_v4();

    let req = Request::builder()
        .method("DELETE")
        .uri(format!(
            "/api/v1/delete?data_id={data_id}&dataset_id={dataset_id}"
        ))
        .header("Authorization", auth_header)
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::CONFLICT, "error must be 409");
    let body = support::body_json(resp).await;
    assert!(
        body["error"].is_string(),
        "`error` key must be present: {body}"
    );
}

/// Missing required query params → 400 or 422 from axum's `Query` extractor.
///
/// Axum's `Query` extractor returns 400 for missing required params (the struct
/// itself is not marked `#[serde(default)]`). We accept either 400 or 422 here
/// since both indicate a client error.
#[tokio::test]
async fn test_deprecated_delete_missing_params_returns_client_error() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "del_noq@example.com", "Str0ng!Pass#1").await;
    let auth_header = bearer_header(&user, &state);
    let app = test_router(state).await;

    let req = Request::builder()
        .method("DELETE")
        .uri("/api/v1/delete")
        .header("Authorization", auth_header)
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert!(
        resp.status().is_client_error(),
        "missing required params must return 4xx, got: {}",
        resp.status()
    );
}
