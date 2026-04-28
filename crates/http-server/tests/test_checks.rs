//! Integration tests for `POST /api/v1/checks/connection`.

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

#[tokio::test]
async fn missing_x_api_key_returns_400_with_missing_error_name() {
    let state = support::build_test_state().await;
    let app = support::test_router(state).await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/checks/connection")
        .body(Body::empty())
        .expect("req");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = support::body_json(resp).await;
    assert_eq!(body["name"], "CloudApiKeyMissingError");
    assert!(body["detail"].as_str().unwrap_or("").contains("API key"));
}

#[tokio::test]
async fn empty_x_api_key_returns_400() {
    let state = support::build_test_state().await;
    let app = support::test_router(state).await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/checks/connection")
        .header("X-Api-Key", "")
        .body(Body::empty())
        .expect("req");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = support::body_json(resp).await;
    assert_eq!(body["name"], "CloudApiKeyMissingError");
}

/// When the cloud is unreachable the handler must return 503 with the
/// Python-typo `CloudConnnectionError` (three n's).
#[tokio::test]
async fn upstream_failure_returns_503_with_typo_error_name() {
    // Point the cloud URL at an unroutable address so reqwest fails fast.
    // SAFETY: single-threaded test; we restore the env var afterwards.
    let saved = std::env::var("COGNEE_CLOUD_URL").ok();
    unsafe {
        std::env::set_var("COGNEE_CLOUD_URL", "http://127.0.0.1:1");
    }

    let state = support::build_test_state().await;
    let app = support::test_router(state).await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/checks/connection")
        .header("X-Api-Key", "fake-key")
        .body(Body::empty())
        .expect("req");
    let resp = app.oneshot(req).await.expect("response");
    let status = resp.status();
    let body = support::body_json(resp).await;

    // Restore env first (regardless of assertions below).
    unsafe {
        match saved {
            Some(v) => std::env::set_var("COGNEE_CLOUD_URL", v),
            None => std::env::remove_var("COGNEE_CLOUD_URL"),
        }
    }

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    // sic — replicated Python typo (three n's).
    assert_eq!(body["name"], "CloudConnnectionError");
    assert!(body["detail"].as_str().unwrap_or("").contains("cloud"));
}
