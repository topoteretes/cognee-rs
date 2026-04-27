//! Shared test helpers for the cognee-http-server integration tests.
#![allow(dead_code)]

use axum::{Router, body::Body, http::Request};
use tower::ServiceExt;

use cognee_http_server::{AppState, HttpServerConfig, build_router};

/// Build an `AppState` suitable for tests.
///
/// Uses default config (localhost:0, MockHealthChecker, no auth).
pub async fn build_test_state() -> AppState {
    AppState::build(HttpServerConfig::default())
        .await
        .expect("AppState::build")
}

/// Build a test state with explicit CORS origins.
pub async fn build_test_state_with_cors(origins: Vec<String>) -> AppState {
    let cfg = HttpServerConfig {
        cors_allowed_origins: origins,
        ..HttpServerConfig::default()
    };
    AppState::build(cfg).await.expect("AppState::build")
}

/// Build the full router from a test state.
pub async fn test_router(state: AppState) -> Router {
    build_router(state).await.expect("build_router")
}

/// Fire a single GET request at the given path against the router.
pub async fn oneshot_get(app: Router, path: &str) -> axum::response::Response {
    let req = Request::builder()
        .method("GET")
        .uri(path)
        .body(Body::empty())
        .expect("request");
    app.oneshot(req).await.expect("response")
}

/// Fire a single request with explicit headers.
pub async fn oneshot_request(app: Router, req: Request<Body>) -> axum::response::Response {
    app.oneshot(req).await.expect("response")
}

/// Read the response body as parsed JSON.
pub async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    use axum::body::to_bytes;
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.expect("body");
    serde_json::from_slice(&bytes).expect("json")
}
