//! Integration tests for `GET /`.

mod support;

use axum::http::StatusCode;

/// `GET /` must return 200 with exactly `{"message": "Hello, World, I am alive!"}`.
#[tokio::test]
async fn test_root_returns_hello_world() {
    let state = support::build_test_state().await;
    let app = support::test_router(state).await;
    let resp = support::oneshot_get(app, "/").await;

    assert_eq!(resp.status(), StatusCode::OK);
    let body = support::body_json(resp).await;
    assert_eq!(body["message"], "Hello, World, I am alive!");
}
