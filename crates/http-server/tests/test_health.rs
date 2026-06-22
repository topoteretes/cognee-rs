#![cfg(any())] // cognee-http-server gated on oss-split branch (T2-move §4 S2).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for `GET /health` and `GET /health/detailed`.

mod support;

use std::sync::Arc;

use axum::http::StatusCode;
use cognee_http_server::{
    AppState, HttpServerConfig, build_router,
    routers::health::{HealthChecker, HealthStatus, MockHealthChecker},
};

/// Helper: build a router backed by a specific health checker.
async fn router_with_checker(checker: impl HealthChecker + 'static) -> axum::Router {
    let cfg = HttpServerConfig {
        cors_allowed_origins: vec![],
        ..HttpServerConfig::default()
    };
    let mut state = AppState::build(cfg).await.expect("state");
    state.health = Some(Arc::new(checker));
    build_router(state).await.expect("router")
}

// ── GET /health (shallow) ──────────────────────────────────────────────────────

#[tokio::test]
async fn test_health_shallow_healthy_returns_200_ready() {
    let app = router_with_checker(MockHealthChecker::with_status(HealthStatus::Healthy)).await;
    let resp = support::oneshot_get(app, "/health").await;

    assert_eq!(resp.status(), StatusCode::OK);
    let body = support::body_json(resp).await;
    assert_eq!(body["status"], "ready");
    assert_eq!(body["health"], "healthy");
    assert!(body["version"].is_string());
}

#[tokio::test]
async fn test_health_shallow_unhealthy_returns_503() {
    let app = router_with_checker(MockHealthChecker::with_status(HealthStatus::Unhealthy)).await;
    let resp = support::oneshot_get(app, "/health").await;

    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = support::body_json(resp).await;
    assert_eq!(body["status"], "not ready");
    assert_eq!(body["health"], "unhealthy");
}

/// Python parity guard: DEGRADED on the shallow probe stays 200.
#[tokio::test]
async fn test_health_shallow_degraded_stays_200() {
    let app = router_with_checker(MockHealthChecker::with_status(HealthStatus::Degraded)).await;
    let resp = support::oneshot_get(app, "/health").await;

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "DEGRADED must remain 200 on shallow probe (Python parity)"
    );
    let body = support::body_json(resp).await;
    assert_eq!(body["health"], "degraded");
    assert_eq!(body["status"], "ready");
}

/// Checker failure on shallow: body must use key `reason` (not `error`).
#[tokio::test]
async fn test_health_shallow_checker_error_uses_reason_key() {
    let app = router_with_checker(MockHealthChecker::failing("injected failure")).await;
    let resp = support::oneshot_get(app, "/health").await;

    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = support::body_json(resp).await;
    assert_eq!(body["status"], "not ready");
    assert!(body["reason"].is_string(), "`reason` key expected: {body}");
    assert!(
        body.get("error").is_none(),
        "`error` key must NOT appear on shallow: {body}"
    );
}

// ── GET /health/detailed ───────────────────────────────────────────────────────

#[tokio::test]
async fn test_health_detailed_healthy_returns_200_with_components() {
    let app = router_with_checker(MockHealthChecker::with_status(HealthStatus::Healthy)).await;
    let resp = support::oneshot_get(app, "/health/detailed").await;

    assert_eq!(resp.status(), StatusCode::OK);
    let body = support::body_json(resp).await;
    assert_eq!(body["status"], "healthy");

    // All four critical component keys must be present.
    for key in &["relational_db", "vector_db", "graph_db", "file_storage"] {
        assert!(
            body["components"][key].is_object(),
            "missing component {key} in: {body}"
        );
    }
}

/// Python parity guard: DEGRADED on the detailed probe flips to 503.
#[tokio::test]
async fn test_health_detailed_degraded_returns_503() {
    let app = router_with_checker(MockHealthChecker::with_status(HealthStatus::Degraded)).await;
    let resp = support::oneshot_get(app, "/health/detailed").await;

    assert_eq!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "DEGRADED must flip to 503 on detailed probe (Python parity)"
    );
    let body = support::body_json(resp).await;
    assert_eq!(body["status"], "degraded");
}

/// Checker failure on detailed: body must use key `error` (not `reason`) and
/// status value `"unhealthy"` (not `"not ready"`).
#[tokio::test]
async fn test_health_detailed_checker_error_uses_error_key() {
    let app = router_with_checker(MockHealthChecker::failing("db panic")).await;
    let resp = support::oneshot_get(app, "/health/detailed").await;

    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = support::body_json(resp).await;
    assert_eq!(body["status"], "unhealthy");
    assert!(body["error"].is_string(), "`error` key expected: {body}");
    assert!(
        body.get("reason").is_none(),
        "`reason` key must NOT appear on detailed: {body}"
    );
}
