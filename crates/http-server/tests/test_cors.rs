//! Integration tests for CORS behaviour.
//!
//! Sends `OPTIONS` preflight requests and asserts on the returned
//! `Access-Control-*` headers.

mod support;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};

/// Helper: build a `OPTIONS /health` preflight with the given `Origin`.
fn preflight(origin: &str) -> Request<Body> {
    Request::builder()
        .method(Method::OPTIONS)
        .uri("/health")
        .header("origin", origin)
        .header("access-control-request-method", "GET")
        .body(Body::empty())
        .expect("request")
}

/// An allowlisted origin must receive the matching `Access-Control-Allow-Origin`
/// and `Access-Control-Allow-Credentials: true` response headers.
#[tokio::test]
async fn test_cors_allowlisted_origin_is_echoed() {
    let state = support::build_test_state_with_cors(vec!["http://example.test".into()]).await;
    let app = support::test_router(state).await;

    let resp = support::oneshot_request(app, preflight("http://example.test")).await;

    // tower-http CorsLayer responds to a valid preflight with 200 or 204.
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::NO_CONTENT,
        "unexpected status {status}"
    );

    let acao = resp
        .headers()
        .get("access-control-allow-origin")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert_eq!(
        acao, "http://example.test",
        "ACAO header should echo the origin"
    );

    let acac = resp
        .headers()
        .get("access-control-allow-credentials")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert_eq!(acac, "true", "ACAC must be true");
}

/// `Access-Control-Allow-Methods` must contain the documented set.
#[tokio::test]
async fn test_cors_allow_methods_contains_required_set() {
    let state = support::build_test_state_with_cors(vec!["http://example.test".into()]).await;
    let app = support::test_router(state).await;

    let resp = support::oneshot_request(app, preflight("http://example.test")).await;

    let acam = resp
        .headers()
        .get("access-control-allow-methods")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_uppercase();

    for method in &["GET", "POST", "PUT", "DELETE", "OPTIONS"] {
        assert!(
            acam.contains(method),
            "ACAM header '{acam}' must contain {method}"
        );
    }
}

/// A non-allowlisted origin must NOT be echoed back in `Access-Control-Allow-Origin`.
#[tokio::test]
async fn test_cors_non_allowlisted_origin_is_not_echoed() {
    let state = support::build_test_state_with_cors(vec!["http://example.test".into()]).await;
    let app = support::test_router(state).await;

    let resp = support::oneshot_request(app, preflight("http://evil.test")).await;

    let acao = resp
        .headers()
        .get("access-control-allow-origin")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();

    assert_ne!(
        acao, "http://evil.test",
        "non-allowlisted origin must not be echoed: {acao}"
    );
}
