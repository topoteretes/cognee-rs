//! Integration tests for `GET /api/v1/datasets/{id}/graph`.
//!
//! Full graph rendering is a blocking gap (no `get_formatted_graph_data` function
//! in the Rust codebase). The endpoint returns `501 Not Implemented` as a
//! placeholder. Tests verify the placeholder behaviour and auth guard.

mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

use support::{
    bearer_header, build_auth_required_test_state, build_auth_test_state, seed_user, test_router,
};

/// No auth → 401.
#[tokio::test]
async fn test_get_graph_no_auth_returns_401() {
    let (state, _) = build_auth_required_test_state().await;
    let app = test_router(state).await;

    let dataset_id = uuid::Uuid::new_v4();
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/datasets/{dataset_id}/graph"))
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// With auth, gets past the auth layer to the 501 placeholder.
///
/// Once `get_formatted_graph_data` is implemented in `cognee-graph`, this test
/// should be updated to assert `200 GraphDTO` with `nodes` and `edges` arrays.
#[tokio::test]
async fn test_get_graph_authenticated_returns_501_placeholder() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "graph_user@example.com", "Str0ng!Pass#1").await;
    let auth_header = bearer_header(&user, &state);
    let app = test_router(state).await;

    let dataset_id = uuid::Uuid::new_v4();
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/datasets/{dataset_id}/graph"))
        .header("Authorization", auth_header)
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    // Placeholder — blocked on get_formatted_graph_data.
    assert_eq!(
        resp.status(),
        StatusCode::NOT_IMPLEMENTED,
        "graph endpoint must return 501 until get_formatted_graph_data is implemented"
    );
}
