#![cfg(any())] // cognee-http-server gated on oss-split branch (T2-move §4 S2).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for `POST /api/v1/improve`.
//!
//! Per p3-pipelines-and-websocket.md §5:
//! - Blocking and background variants.
//! - `dataset_id="" + dataset_name="foo"` → name fallback path.
//! - 420 quirk covered separately in `test_improve_420.rs`.

mod support;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use serde_json::json;
use tower::ServiceExt;

use cognee_http_server::{AppState, HttpServerConfig, build_router};

async fn test_app() -> Router {
    let state = AppState::build(HttpServerConfig::default())
        .await
        .expect("AppState::build");
    build_router(state).await.expect("build_router")
}

/// Without auth, `/improve` returns 401.
#[tokio::test]
async fn post_improve_no_auth_returns_401() {
    // Must use require_authentication=true; the default AppState allows anonymous users.
    let (state, _) = support::build_auth_required_test_state().await;
    let app = build_router(state).await.expect("build_router");

    let body = json!({ "dataset_name": "my_dataset" });
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/improve")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_string(&body).unwrap()))
        .unwrap();

    let resp = app.oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// Verify the route mounts at /api/v1/improve.
#[tokio::test]
async fn post_improve_route_exists() {
    let app = test_app().await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/improve")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.expect("oneshot");
    assert_ne!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "route /api/v1/improve must exist"
    );
}

/// Validation body uses `{"detail": "..."}` per Python HTTPException parity.
/// Covered by: routers::improve::tests::post_improve_no_dataset_body_uses_detail_key
#[tokio::test]
async fn post_improve_validation_body_uses_detail_key_documented() {
    let _: () = ();
}

/// Gated: full improve test requires graph + vector backend.
#[tokio::test]
async fn post_improve_end_to_end_skips_without_openai() {
    if std::env::var("OPENAI_URL").is_err() {
        eprintln!(
            "test_improve: skipping end-to-end — OPENAI_URL not set \
             (set OPENAI_URL + OPENAI_TOKEN to run)"
        );
        return;
    }

    eprintln!(
        "test_improve: skipping end-to-end — set OPENAI_URL + OPENAI_TOKEN and \
         a fully wired component fixture to run this path"
    );
}

// ─── E-05 v2 payload field plumbing ──────────────────────────────────────────
//
// These tests verify that the five v2 fields added to `ImprovePayloadDTO`
// (`sessionIds`, `extractionTasks`, `enrichmentTasks`, `data`, `nodeName`) are
// accepted on the wire in both camelCase and snake_case forms. We run them in
// background mode so they assert payload parsing independently from backend
// component wiring.

/// `sessionIds` (camelCase) is accepted and the handler returns 200.
#[tokio::test]
async fn session_ids_accepted_camelcase() {
    let app = test_app().await;

    let body = json!({
        "sessionIds": ["s1", "s2"],
        "datasetName": "ds_session_camel",
        "runInBackground": true
    });
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/improve")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_string(&body).unwrap()))
        .unwrap();

    let resp = app.oneshot(req).await.expect("oneshot");
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "sessionIds (camelCase) should be accepted"
    );
}

/// `session_ids` (snake_case alias) is accepted and the handler returns 200.
#[tokio::test]
async fn session_ids_accepted_snake_case_alias() {
    let app = test_app().await;

    let body = json!({
        "session_ids": ["s1"],
        "dataset_name": "ds_session_snake",
        "run_in_background": true
    });
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/improve")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_string(&body).unwrap()))
        .unwrap();

    let resp = app.oneshot(req).await.expect("oneshot");
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "session_ids (snake_case alias) should be accepted"
    );
}

/// `extractionTasks` and `enrichmentTasks` are accepted in both wire forms.
#[tokio::test]
async fn extraction_tasks_and_enrichment_tasks_passed_through() {
    let app_camel = test_app().await;
    let body_camel = json!({
        "extractionTasks": ["t1"],
        "enrichmentTasks": ["e1"],
        "datasetName": "ds_tasks_camel",
        "runInBackground": true
    });
    let req_camel = Request::builder()
        .method("POST")
        .uri("/api/v1/improve")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_string(&body_camel).unwrap()))
        .unwrap();
    let resp_camel = app_camel.oneshot(req_camel).await.expect("oneshot");
    assert_eq!(
        resp_camel.status(),
        StatusCode::OK,
        "camelCase tasks fields should be accepted"
    );

    let app_snake = test_app().await;
    let body_snake = json!({
        "extraction_tasks": ["t1"],
        "enrichment_tasks": ["e1"],
        "dataset_name": "ds_tasks_snake",
        "run_in_background": true
    });
    let req_snake = Request::builder()
        .method("POST")
        .uri("/api/v1/improve")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_string(&body_snake).unwrap()))
        .unwrap();
    let resp_snake = app_snake.oneshot(req_snake).await.expect("oneshot");
    assert_eq!(
        resp_snake.status(),
        StatusCode::OK,
        "snake_case tasks aliases should be accepted"
    );
}

/// `nodeName` and its `node_name` snake_case alias are accepted.
#[tokio::test]
async fn node_name_camelcase_and_alias() {
    let app_camel = test_app().await;
    let body_camel = json!({
        "nodeName": ["n1", "n2"],
        "datasetName": "ds_node_camel",
        "runInBackground": true
    });
    let req_camel = Request::builder()
        .method("POST")
        .uri("/api/v1/improve")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_string(&body_camel).unwrap()))
        .unwrap();
    let resp_camel = app_camel.oneshot(req_camel).await.expect("oneshot");
    assert_eq!(
        resp_camel.status(),
        StatusCode::OK,
        "nodeName (camelCase) should be accepted"
    );

    let app_snake = test_app().await;
    let body_snake = json!({
        "node_name": ["n1"],
        "dataset_name": "ds_node_snake",
        "run_in_background": true
    });
    let req_snake = Request::builder()
        .method("POST")
        .uri("/api/v1/improve")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_string(&body_snake).unwrap()))
        .unwrap();
    let resp_snake = app_snake.oneshot(req_snake).await.expect("oneshot");
    assert_eq!(
        resp_snake.status(),
        StatusCode::OK,
        "node_name (snake_case alias) should be accepted"
    );
}

/// `data` (single-word, no rename) round-trips on the wire.
#[tokio::test]
async fn data_field_round_trip() {
    let app = test_app().await;
    let body = json!({
        "data": "some inline payload",
        "datasetName": "ds_data",
        "runInBackground": true
    });
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/improve")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_string(&body).unwrap()))
        .unwrap();

    let resp = app.oneshot(req).await.expect("oneshot");
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "data field should be accepted as-is"
    );
}

/// Blocking improve with no wired components surfaces the parity 420 status.
#[tokio::test]
async fn blocking_improve_without_components_returns_420() {
    let app = test_app().await;
    let body = json!({ "datasetName": "ds_blocking_420" });
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/improve")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_string(&body).unwrap()))
        .unwrap();

    let resp = app.oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status().as_u16(), 420);
}
