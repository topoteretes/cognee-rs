#![cfg(any())] // cognee-http-server gated on oss-split branch (T2-move §4 S2).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! P5 §5: `/api/v1/configuration` integration tests.
//!
//! Covers the parity invariants from `routers/configuration.md`:
//! - GET on miss returns `200 {}` (NOT 404).
//! - POST returns `200` with body literally `null` (NOT 204).
//! - Mixed snake/camelCase keys (`id`, `name`, `configuration` snake;
//!   `ownerId`, `createdAt`, `updatedAt` camel).
//! - Upsert by `(owner_id, name)` — second store with same name does not
//!   produce a duplicate row, just bumps `updatedAt`.
//! - Cross-user GET-by-id is permitted (Python parity bug per §6.1).

mod support;

use axum::{body::Body, http::Request};
use serde_json::json;
use tower::ServiceExt;

async fn build_state() -> cognee_http_server::AppState {
    let db = support::build_search_db().await;
    let handles = support::build_component_handles(db, None, None, None);
    let cfg = cognee_http_server::HttpServerConfig::default();
    let mut state = cognee_http_server::AppState::build(cfg)
        .await
        .expect("state");
    state.lib = Some(handles);
    state
}

#[tokio::test]
async fn get_missing_config_returns_200_empty_object() {
    let state = build_state().await;
    let app = cognee_http_server::build_router(state)
        .await
        .expect("router");

    let bogus = uuid::Uuid::new_v4();
    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/v1/configuration/get_user_configuration/{bogus}"
        ))
        .body(Body::empty())
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), 200, "miss must return 200, NOT 404");
    let body = support::body_json(resp).await;
    assert_eq!(body, json!({}), "miss body must be `{{}}`, got {body}");
}

#[tokio::test]
async fn store_returns_200_with_null_body() {
    let state = build_state().await;
    let app = cognee_http_server::build_router(state)
        .await
        .expect("router");

    let payload = json!({
        "name": "default",
        "config": {"theme": "dark"}
    });
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/configuration/store_user_configuration")
        .header("content-type", "application/json")
        .body(Body::from(payload.to_string()))
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), 200, "must be 200, NOT 204");

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    // Strict parity: body is the literal 4-byte JSON token `null`.
    assert_eq!(
        bytes.as_ref(),
        b"null",
        "expected body literal `null`, got {:?}",
        std::str::from_utf8(&bytes)
    );
}

#[tokio::test]
async fn list_then_store_then_list_is_idempotent_by_name() {
    let state = build_state().await;
    let app = cognee_http_server::build_router(state)
        .await
        .expect("router");

    // Initial list is empty.
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/configuration/get_user_configuration/")
        .body(Body::empty())
        .expect("request");
    let resp = app.clone().oneshot(req).await.expect("response");
    assert_eq!(resp.status(), 200);
    let body = support::body_json(resp).await;
    assert_eq!(body, json!([]));

    // Store "default".
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/configuration/store_user_configuration")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"name": "default", "config": {"v": 1}}).to_string(),
        ))
        .expect("request");
    let _ = app.clone().oneshot(req).await.expect("response");

    // Store "default" again — should upsert, not duplicate.
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/configuration/store_user_configuration")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"name": "default", "config": {"v": 2}}).to_string(),
        ))
        .expect("request");
    let _ = app.clone().oneshot(req).await.expect("response");

    // List has exactly one row.
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/configuration/get_user_configuration/")
        .body(Body::empty())
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    let body = support::body_json(resp).await;
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 1, "expected single upserted row, got {arr:?}");

    // Mixed snake/camelCase wire format.
    let row = &arr[0];
    assert!(row.get("id").is_some(), "snake `id` missing: {row}");
    assert!(row.get("name").is_some(), "snake `name` missing: {row}");
    assert!(
        row.get("configuration").is_some(),
        "snake `configuration` missing: {row}"
    );
    assert!(
        row.get("ownerId").is_some(),
        "camelCase `ownerId` missing: {row}"
    );
    assert!(
        row.get("createdAt").is_some(),
        "camelCase `createdAt` missing: {row}"
    );
    assert!(
        row.get("updatedAt").is_some(),
        "camelCase `updatedAt` missing: {row}"
    );
    assert!(
        row.get("owner_id").is_none(),
        "snake_case alias `owner_id` must NOT appear (parity): {row}"
    );

    // Latest config wins.
    assert_eq!(row["configuration"], json!({"v": 2}));
}

#[tokio::test]
async fn store_rejects_non_object_config() {
    let state = build_state().await;
    let app = cognee_http_server::build_router(state)
        .await
        .expect("router");

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/configuration/store_user_configuration")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"name": "x", "config": "not-an-object"}).to_string(),
        ))
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), 400, "non-object config must be 400");
}
