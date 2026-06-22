#![cfg(any())] // cognee-http-server gated on oss-split branch (T2-move §4 S2).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for `GET /api/v1/ontologies` and `POST /api/v1/ontologies`.
//!
//! Full ontology upload requires wired `OntologyManager` (filesystem + DB).
//! Tests here cover auth guards and multipart validation.

mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

use support::{
    bearer_header, build_auth_required_test_state, build_auth_test_state, seed_user, test_router,
};

// ─── GET /api/v1/ontologies ───────────────────────────────────────────────────

/// No auth → 401.
#[tokio::test]
async fn test_list_ontologies_no_auth_returns_401() {
    let (state, _) = build_auth_required_test_state().await;
    let app = test_router(state).await;

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/ontologies")
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ─── POST /api/v1/ontologies ──────────────────────────────────────────────────

/// No auth → 401.
#[tokio::test]
async fn test_upload_ontology_no_auth_returns_401() {
    let (state, _) = build_auth_required_test_state().await;
    let app = test_router(state).await;

    let boundary = "ontoboundary";
    let body_str = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"ontology_file\"; filename=\"test.owl\"\r\nContent-Type: application/rdf+xml\r\n\r\n<rdf/>\r\n--{boundary}--\r\n"
    );

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/ontologies")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body_str))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// Upload with non-`.owl` extension → 400 with canonical error message.
#[tokio::test]
async fn test_upload_ontology_non_owl_extension_returns_400() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "onto_user@example.com", "Str0ng!Pass#1").await;
    let auth_header = bearer_header(&user, &state);
    let app = test_router(state).await;

    let boundary = "ontoboundary2";
    let body_str = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"ontology_key\"\r\n\r\nmykey\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"ontology_file\"; filename=\"test.txt\"\r\nContent-Type: text/plain\r\n\r\nhello\r\n--{boundary}--\r\n"
    );

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/ontologies")
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
        "non-.owl extension must return 400"
    );
    let body = support::body_json(resp).await;
    assert!(
        body["error"]
            .as_str()
            .map(|s| s.contains(".owl"))
            .unwrap_or(false),
        "error message must mention .owl: {body}"
    );
}

/// Upload with `ontology_key` starting with `[` → 400.
#[tokio::test]
async fn test_upload_ontology_invalid_key_prefix_returns_400() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "onto_key@example.com", "Str0ng!Pass#1").await;
    let auth_header = bearer_header(&user, &state);
    let app = test_router(state).await;

    let boundary = "ontoboundary3";
    let body_str = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"ontology_key\"\r\n\r\n[evil]\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"ontology_file\"; filename=\"test.owl\"\r\nContent-Type: application/rdf+xml\r\n\r\n<rdf/>\r\n--{boundary}--\r\n"
    );

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/ontologies")
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
        "ontology_key starting with '[' must return 400"
    );
}

/// Upload with no `ontology_file` part → 400.
#[tokio::test]
async fn test_upload_ontology_missing_file_returns_400() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "onto_nofile@example.com", "Str0ng!Pass#1").await;
    let auth_header = bearer_header(&user, &state);
    let app = test_router(state).await;

    let boundary = "ontoboundary4";
    // Only key, no file.
    let body_str = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"ontology_key\"\r\n\r\nmykey\r\n--{boundary}--\r\n"
    );

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/ontologies")
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
        "missing ontology_file must return 400"
    );
}
