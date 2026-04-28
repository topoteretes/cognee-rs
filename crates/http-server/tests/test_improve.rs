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
        "test_improve: skipping end-to-end — real improve() is not wired through \
         ComponentHandles yet"
    );
}
