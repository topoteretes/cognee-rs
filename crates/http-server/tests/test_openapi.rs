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
