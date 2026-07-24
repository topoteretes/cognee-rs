#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Regression tests for `GET /api/v1/datasets/status`.
//!
//! The handler's `dataset: Vec<Uuid>` query used axum's default `Query`
//! (serde_urlencoded), which cannot deserialize a sequence from query params and
//! rejected every real request with HTTP 400 "invalid type: string …, expected a
//! sequence". Switching to the `axum_extra` (serde_html_form) Query extractor
//! makes both single and repeated `dataset` params deserialize into the Vec.

mod support;

use support::{body_json, build_p4_state, oneshot_get};

use cognee_http_server::build_router;

// A random dataset id with no pipeline run: the handler omits it from the map,
// so the response is `{}` — but only if the query deserializes at all (the bug
// made this a 400 before the fix).
const DATASET_A: &str = "11111111-1111-4111-8111-111111111111";
const DATASET_B: &str = "22222222-2222-4222-8222-222222222222";

#[tokio::test]
async fn single_dataset_param_is_accepted() {
    let state = build_p4_state(None, None, None).await;
    let app = build_router(state).await.expect("router");

    let resp = oneshot_get(app, &format!("/api/v1/datasets/status?dataset={DATASET_A}")).await;

    assert_eq!(
        resp.status(),
        200,
        "single ?dataset=<uuid> must deserialize (was 400 with serde_urlencoded)"
    );
    let body = body_json(resp).await;
    assert!(body.is_object(), "expected a JSON status map, got {body}");
    // No pipeline run for a random id -> omitted -> empty map.
    assert_eq!(body.as_object().unwrap().len(), 0);
}

#[tokio::test]
async fn repeated_dataset_params_are_accepted() {
    let state = build_p4_state(None, None, None).await;
    let app = build_router(state).await.expect("router");

    let resp = oneshot_get(
        app,
        &format!("/api/v1/datasets/status?dataset={DATASET_A}&dataset={DATASET_B}"),
    )
    .await;

    assert_eq!(
        resp.status(),
        200,
        "repeated ?dataset=a&dataset=b must deserialize into Vec<Uuid>"
    );
    assert!(body_json(resp).await.is_object());
}

#[tokio::test]
async fn absent_dataset_param_returns_empty_map() {
    let state = build_p4_state(None, None, None).await;
    let app = build_router(state).await.expect("router");

    let resp = oneshot_get(app, "/api/v1/datasets/status").await;

    assert_eq!(resp.status(), 200);
    let body = body_json(resp).await;
    assert_eq!(body.as_object().unwrap().len(), 0);
}
