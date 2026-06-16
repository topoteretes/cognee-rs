#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! P5 §5: `POST /api/v1/permissions/tenants/select` with body `{"tenant_id": null}`
//! must return the literal **JSON string `"None"`** in the response (Python parity
//! per `routers/permissions.md §2.9` / `§6.4`). Default `Option<Uuid>` would
//! emit JSON `null` — a custom serializer is required.
//!
//! Strict wire-parity check: assert the substring `"tenant_id":"None"` appears in
//! the response body.

mod support;

use axum::{body::Body, http::Request};
use tower::ServiceExt;

#[tokio::test]
async fn select_tenant_with_null_returns_string_none() {
    let db = support::build_search_db().await;
    let handles = support::build_component_handles(db, None, None, None);
    let cfg = cognee_http_server::HttpServerConfig::default();
    let mut state = cognee_http_server::AppState::build(cfg)
        .await
        .expect("state");
    state.lib = Some(handles);

    let app = cognee_http_server::build_router(state)
        .await
        .expect("router");

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/permissions/tenants/select")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"tenant_id": null}"#))
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), 200);

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    let s = String::from_utf8(bytes.to_vec()).expect("utf8");
    // Strict parity: the literal JSON string "None" — NOT JSON null.
    assert!(
        s.contains(r#""tenant_id":"None""#),
        "expected literal string \"None\" in body, got: {s}"
    );
    assert!(
        s.contains(r#""message":"Tenant selected.""#),
        "expected message field, got: {s}"
    );
}
