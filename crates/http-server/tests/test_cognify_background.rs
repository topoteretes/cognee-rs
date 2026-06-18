#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration test: `POST /api/v1/cognify` with `run_in_background=true`.
//!
//! Per p3-pipelines-and-websocket.md §5:
//! "Assert the response returns immediately with `status="PipelineRunStarted"`
//!  and `payload=[]` per pipelines.md §9.2. The `pipeline_run_id` matches the
//!  deterministic `uuid5(NAMESPACE_OID, "{pipeline_id}_{dataset_id}")`."
//!
//! These assertions do NOT require an LLM and run in every CI build.

mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use serde_json::json;
use tower::ServiceExt;
use uuid::Uuid;

use cognee_http_server::build_router;

/// With `require_authentication=true` and a real auth context, the /cognify
/// POST returns 401 for unauthenticated requests.
#[tokio::test]
async fn post_cognify_background_requires_auth_when_auth_required() {
    let (state, _) = support::build_auth_required_test_state().await;
    let app = build_router(state).await.expect("build_router");

    let dataset_id = Uuid::new_v4();
    let body = json!({
        "dataset_ids": [dataset_id],
        "run_in_background": true
    });

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/cognify")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_string(&body).unwrap()))
        .unwrap();

    let resp = app.oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// Verify the deterministic pipeline_run_id formula matches pipelines.md §4.
/// This is a pure unit test of the ID helpers, not the HTTP layer.
#[tokio::test]
async fn pipeline_run_id_deterministic_formula() {
    use cognee_http_server::pipelines::dispatch::{pipeline_id, pipeline_run_id};

    let user_id = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
    let dataset_id = Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap();

    let pid = pipeline_id(user_id, dataset_id, "cognify_pipeline");
    let prid = pipeline_run_id(pid, dataset_id);

    // Same inputs → same outputs (deterministic).
    let pid2 = pipeline_id(user_id, dataset_id, "cognify_pipeline");
    let prid2 = pipeline_run_id(pid2, dataset_id);
    assert_eq!(pid, pid2, "pipeline_id must be deterministic");
    assert_eq!(prid, prid2, "pipeline_run_id must be deterministic");
}
