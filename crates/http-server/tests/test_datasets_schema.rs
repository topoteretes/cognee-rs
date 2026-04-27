//! Integration tests for `GET /api/v1/datasets/{id}/schema` and
//! `PUT /api/v1/datasets/{id}/schema`.
//!
//! The schema endpoints are blocking gaps (`get_dataset_configuration` and
//! schema upsert not yet in the Rust data model). GET returns a placeholder
//! `{"graph_schema": null, "custom_prompt": null}` and PUT returns `{"status": "ok"}`.

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

// ─── GET /schema ──────────────────────────────────────────────────────────────

/// No auth → 401.
#[tokio::test]
async fn test_get_schema_no_auth_returns_401() {
    let (state, _) = build_auth_required_test_state().await;
    let app = test_router(state).await;

    let dataset_id = uuid::Uuid::new_v4();
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/datasets/{dataset_id}/schema"))
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// Authenticated → placeholder `{"graph_schema": null, "custom_prompt": null}`.
#[tokio::test]
async fn test_get_schema_placeholder_response() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "schema_get@example.com", "Str0ng!Pass#1").await;
    let auth_header = bearer_header(&user, &state);
    let app = test_router(state).await;

    let dataset_id = uuid::Uuid::new_v4();
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/datasets/{dataset_id}/schema"))
        .header("Authorization", auth_header)
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert!(
        body["graph_schema"].is_null(),
        "placeholder must return null graph_schema: {body}"
    );
    assert!(
        body["custom_prompt"].is_null(),
        "placeholder must return null custom_prompt: {body}"
    );
}

// ─── PUT /schema ──────────────────────────────────────────────────────────────

/// No auth → 401.
#[tokio::test]
async fn test_put_schema_no_auth_returns_401() {
    let (state, _) = build_auth_required_test_state().await;
    let app = test_router(state).await;

    let dataset_id = uuid::Uuid::new_v4();
    let req = Request::builder()
        .method("PUT")
        .uri(format!("/api/v1/datasets/{dataset_id}/schema"))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"graph_schema":{}}"#))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// Authenticated with a valid JSON body → 200 `{"status": "ok"}` (placeholder).
/// The real implementation (P5) needs tenants_rbac + schema columns.
#[tokio::test]
async fn test_put_schema_placeholder_returns_ok() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "schema_put@example.com", "Str0ng!Pass#1").await;
    let auth_header = bearer_header(&user, &state);
    let app = test_router(state).await;

    let dataset_id = uuid::Uuid::new_v4();
    let req = Request::builder()
        .method("PUT")
        .uri(format!("/api/v1/datasets/{dataset_id}/schema"))
        .header("Authorization", auth_header)
        .header("content-type", "application/json")
        .body(Body::from(r#"{"graph_schema":{"type":"object"}}"#))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["status"], "ok", "unexpected body: {body}");
}
