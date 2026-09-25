#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for `GET /openapi.json`.

mod support;

use axum::http::StatusCode;

/// `GET /openapi.json` must return 200 with valid JSON.
#[tokio::test]
async fn test_openapi_json_returns_200() {
    let state = support::build_test_state().await;
    let app = support::test_router(state).await;
    let resp = support::oneshot_get(app, "/openapi.json").await;

    assert_eq!(resp.status(), StatusCode::OK);
}

/// The OpenAPI document must declare `BearerAuth` and `ApiKeyAuth` security schemes.
#[tokio::test]
async fn test_openapi_has_security_schemes() {
    let state = support::build_test_state().await;
    let app = support::test_router(state).await;
    let resp = support::oneshot_get(app, "/openapi.json").await;
    let body = support::body_json(resp).await;

    // Must parse as a JSON object without errors (already asserted by body_json).
    // Check security scheme keys.
    let schemes = &body["components"]["securitySchemes"];
    assert!(
        schemes["BearerAuth"].is_object(),
        "BearerAuth security scheme missing: {body}"
    );
    assert!(
        schemes["ApiKeyAuth"].is_object(),
        "ApiKeyAuth security scheme missing: {body}"
    );
}

/// The OpenAPI document info block must declare the expected title and version.
#[tokio::test]
async fn test_openapi_info() {
    let state = support::build_test_state().await;
    let app = support::test_router(state).await;
    let resp = support::oneshot_get(app, "/openapi.json").await;
    let body = support::body_json(resp).await;

    assert_eq!(body["info"]["title"], "Cognee API");
    assert_eq!(body["info"]["version"], "1.0.0");
}

/// P5 acceptance criterion: `GET /openapi.json` advertises the OSS-staying
/// `/api/v1/settings` path. The `/api/v1/configuration/...` and
/// `/api/v1/permissions/...` paths moved to the closed `cognee-http-cloud`
/// crate and are asserted in its own OpenAPI overlay test.
#[tokio::test]
async fn test_openapi_advertises_p5_paths() {
    let state = support::build_test_state().await;
    let app = support::test_router(state).await;
    let resp = support::oneshot_get(app, "/openapi.json").await;
    let body = support::body_json(resp).await;

    let paths = body["paths"]
        .as_object()
        .expect("openapi document must have a `paths` object");

    let required = "/api/v1/settings";
    assert!(
        paths.contains_key(required),
        "openapi `paths` must advertise `{required}`; only saw: {:?}",
        paths.keys().collect::<Vec<_>>()
    );
}
