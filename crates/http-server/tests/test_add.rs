#![cfg(any())] // cognee-http-server gated on oss-split branch (T2-move §4 S2).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for `POST /api/v1/add`.
//!
//! These tests focus on the HTTP layer: authentication, request validation,
//! and cross-field validation. Full pipeline round-trips require wired backends
//! and are covered in the E2E test suite.

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

// ─── helpers ─────────────────────────────────────────────────────────────────

/// Build a minimal multipart body with just a `datasetName` field.
fn minimal_multipart(dataset_name: &str, boundary: &str) -> String {
    format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"datasetName\"\r\n\r\n{dataset_name}\r\n--{boundary}--\r\n"
    )
}

// ─── auth guard ──────────────────────────────────────────────────────────────

/// No auth header → 401.
#[tokio::test]
async fn test_add_no_auth_returns_401() {
    let (state, _) = build_auth_required_test_state().await;
    let app = test_router(state).await;

    let boundary = "testboundary123";
    let body_str = minimal_multipart("mydata", boundary);

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/add")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body_str))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "unauthenticated add must return 401"
    );
}

// ─── cross-field validation ───────────────────────────────────────────────────

/// Neither `datasetId` nor `datasetName` provided → 400 with canonical error message.
#[tokio::test]
async fn test_add_missing_dataset_fields_returns_400() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "add_user@example.com", "Str0ng!Pass#1").await;
    let auth_header = bearer_header(&user, &state);
    let app = test_router(state).await;

    let boundary = "testboundary456";
    // Send empty-string datasetName so it normalizes to None.
    let body_str = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"datasetName\"\r\n\r\n\r\n--{boundary}--\r\n"
    );

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/add")
        .header("Authorization", auth_header)
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body_str))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp).await;
    assert_eq!(
        body["error"], "Either datasetId or datasetName must be provided.",
        "unexpected error body: {body}"
    );
}

/// Filename with path traversal attempt → 400.
#[tokio::test]
async fn test_add_filename_traversal_returns_400() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "add_trav@example.com", "Str0ng!Pass#1").await;
    let auth_header = bearer_header(&user, &state);
    let app = test_router(state).await;

    let boundary = "testboundary789";
    let body_str = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"datasetName\"\r\n\r\nmyds\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"data\"; filename=\"../etc/passwd\"\r\nContent-Type: text/plain\r\n\r\nhello\r\n--{boundary}--\r\n"
    );

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/add")
        .header("Authorization", auth_header)
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body_str))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "traversal filename must be rejected with 400"
    );
}

/// `datasetId` that is not a valid UUID → 400.
#[tokio::test]
async fn test_add_invalid_dataset_id_returns_400() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "add_uuid@example.com", "Str0ng!Pass#1").await;
    let auth_header = bearer_header(&user, &state);
    let app = test_router(state).await;

    let boundary = "testboundary_uuid";
    let body_str = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"datasetId\"\r\n\r\nnot-a-uuid\r\n--{boundary}--\r\n"
    );

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/add")
        .header("Authorization", auth_header)
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body_str))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// Empty `node_set` list and `node_set=[""]` normalize to None — verified by
/// successful 500 (components not wired) rather than a validation error.
#[tokio::test]
async fn test_add_empty_node_set_does_not_reject() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "add_ns@example.com", "Str0ng!Pass#1").await;
    let auth_header = bearer_header(&user, &state);
    let app = test_router(state).await;

    let boundary = "testboundary_ns";
    // node_set=[""] — a single empty string.
    let body_str = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"datasetName\"\r\n\r\nns_test\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"node_set\"\r\n\r\n\r\n--{boundary}--\r\n"
    );

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/add")
        .header("Authorization", auth_header)
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body_str))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    // 400 would indicate bad node_set validation; 500 means we got past validation.
    assert_ne!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "empty node_set should not cause 400"
    );
}

/// A small `data` part whose body is an HTTP URL should parse as URL input,
/// not as invalid multipart. This auth-only state intentionally has no add
/// components wired, so a later 500 is acceptable; the boundary check is that
/// URL detection gets past request validation.
#[tokio::test]
async fn test_add_data_part_url_does_not_reject() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "add_url@example.com", "Str0ng!Pass#1").await;
    let auth_header = bearer_header(&user, &state);
    let app = test_router(state).await;

    let boundary = "testboundary_url";
    let local_url = "http://127.0.0.1:7777/page.html";
    let body_str = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"datasetName\"\r\n\r\nurl_ds\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"data\"; filename=\"url.txt\"\r\nContent-Type: text/plain\r\n\r\n{local_url}\r\n--{boundary}--\r\n"
    );

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/add")
        .header("Authorization", auth_header)
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body_str))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_ne!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "URL data part should pass add request validation"
    );
}
