//! Integration tests for dataset CRUD endpoints.
//!
//! Tests here cover auth guards and routing correctness. Full CRUD round-trips
//! (list/create/delete) require wired ComponentHandles and are integration-level.

mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

use support::{
    bearer_header, build_auth_required_test_state, build_auth_test_state, seed_user, test_router,
};

// ─── GET /api/v1/datasets — list ─────────────────────────────────────────────

/// No auth → 401.
#[tokio::test]
async fn test_list_datasets_no_auth_returns_401() {
    let (state, _) = build_auth_required_test_state().await;
    let app = test_router(state).await;

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/datasets")
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// Authenticated with no backends wired → 418 (Teapot) with error message (Python parity).
#[tokio::test]
async fn test_list_datasets_no_components_returns_teapot() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "listds@example.com", "Str0ng!Pass#1").await;
    let auth_header = bearer_header(&user, &state);
    let app = test_router(state).await;

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/datasets")
        .header("Authorization", auth_header)
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    // Python maps DB errors to 418 (Teapot).
    assert_eq!(resp.status(), StatusCode::IM_A_TEAPOT);
}

// ─── POST /api/v1/datasets — create ──────────────────────────────────────────

/// No auth → 401.
#[tokio::test]
async fn test_create_dataset_no_auth_returns_401() {
    let (state, _) = build_auth_required_test_state().await;
    let app = test_router(state).await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/datasets")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"name":"test"}"#))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// Missing `name` field in JSON body → 422.
#[tokio::test]
async fn test_create_dataset_missing_name_returns_422() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "createds@example.com", "Str0ng!Pass#1").await;
    let auth_header = bearer_header(&user, &state);
    let app = test_router(state).await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/datasets")
        .header("Authorization", auth_header)
        .header("content-type", "application/json")
        .body(Body::from(r#"{}"#))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

// ─── DELETE /api/v1/datasets — delete all ────────────────────────────────────

/// No auth → 401.
#[tokio::test]
async fn test_delete_all_datasets_no_auth_returns_401() {
    let (state, _) = build_auth_required_test_state().await;
    let app = test_router(state).await;

    let req = Request::builder()
        .method("DELETE")
        .uri("/api/v1/datasets")
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ─── DELETE /api/v1/datasets/{id} — delete one ───────────────────────────────

/// No auth → 401.
#[tokio::test]
async fn test_delete_dataset_no_auth_returns_401() {
    let (state, _) = build_auth_required_test_state().await;
    let app = test_router(state).await;

    let dataset_id = uuid::Uuid::new_v4();
    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/api/v1/datasets/{dataset_id}"))
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ─── GET /api/v1/datasets/status ─────────────────────────────────────────────

/// No auth → 401.
#[tokio::test]
async fn test_dataset_status_no_auth_returns_401() {
    let (state, _) = build_auth_required_test_state().await;
    let app = test_router(state).await;

    let dataset_id = uuid::Uuid::new_v4();
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/datasets/status?dataset={dataset_id}"))
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// Empty `dataset` query parameter → `{}` (200), not 422.
#[tokio::test]
async fn test_dataset_status_empty_list_returns_200() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "status_empty@example.com", "Str0ng!Pass#1").await;
    let auth_header = bearer_header(&user, &state);
    let app = test_router(state).await;

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/datasets/status")
        .header("Authorization", auth_header)
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    // Empty dataset list → {} (Python parity — no 422).
    assert_eq!(resp.status(), StatusCode::OK);
    let body = support::body_json(resp).await;
    assert_eq!(body, serde_json::json!({}));
}
