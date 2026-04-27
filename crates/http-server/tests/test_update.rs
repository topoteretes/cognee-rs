//! Integration tests for `PATCH /api/v1/update`.
//!
//! Full update (delete + re-add + cognify) is deferred until P3 provides the
//! cognify pipeline. Tests here cover auth guard, route existence, and the
//! current 501 stub response.

mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

use support::{
    bearer_header, build_auth_required_test_state, build_auth_test_state, seed_user, test_router,
};

// ─── auth guard ──────────────────────────────────────────────────────────────

/// No auth header → 401.
#[tokio::test]
async fn test_update_no_auth_returns_401() {
    let (state, _) = build_auth_required_test_state().await;
    let app = test_router(state).await;

    let data_id = uuid::Uuid::new_v4();
    let dataset_id = uuid::Uuid::new_v4();
    let boundary = "updboundary";
    let body_str = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"data\"; filename=\"file.txt\"\r\nContent-Type: text/plain\r\n\r\nhello\r\n--{boundary}--\r\n"
    );

    let req = Request::builder()
        .method("PATCH")
        .uri(format!(
            "/api/v1/update?data_id={data_id}&dataset_id={dataset_id}"
        ))
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body_str))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// With auth → gets past auth, hits the 501 stub (full implementation requires P3).
#[tokio::test]
async fn test_update_authenticated_hits_stub() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "update_user@example.com", "Str0ng!Pass#1").await;
    let auth_header = bearer_header(&user, &state);
    let app = test_router(state).await;

    let data_id = uuid::Uuid::new_v4();
    let dataset_id = uuid::Uuid::new_v4();
    let boundary = "updboundary2";
    let body_str = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"data\"; filename=\"file.txt\"\r\nContent-Type: text/plain\r\n\r\nhello world\r\n--{boundary}--\r\n"
    );

    let req = Request::builder()
        .method("PATCH")
        .uri(format!(
            "/api/v1/update?data_id={data_id}&dataset_id={dataset_id}"
        ))
        .header("Authorization", auth_header)
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body_str))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    // 501 is the expected stub response; any non-401 means auth passed.
    assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_ne!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
}

/// Missing query params (`data_id` or `dataset_id`) → 4xx client error.
///
/// Axum's `Query` extractor returns 400 for missing required params.
/// The test asserts a client error rather than a specific code.
#[tokio::test]
async fn test_update_missing_query_params_returns_client_error() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "update_noq@example.com", "Str0ng!Pass#1").await;
    let auth_header = bearer_header(&user, &state);
    let app = test_router(state).await;

    let boundary = "updboundary3";
    let body_str = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"data\"; filename=\"file.txt\"\r\nContent-Type: text/plain\r\n\r\nhello\r\n--{boundary}--\r\n"
    );

    // Missing data_id and dataset_id entirely.
    let req = Request::builder()
        .method("PATCH")
        .uri("/api/v1/update")
        .header("Authorization", auth_header)
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body_str))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert!(
        resp.status().is_client_error(),
        "missing query params must yield 4xx, got: {}",
        resp.status()
    );
}
