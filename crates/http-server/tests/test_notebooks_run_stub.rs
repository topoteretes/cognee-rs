//! Integration tests for `POST /api/v1/notebooks/{id}/{cell_id}/run` (Stage-A stub).
//!
//! Covers:
//! - 401 when unauthenticated
//! - 404 when notebook does not exist (404 beats 501)
//! - 501 with `{"detail": "...", "code": "..."}` when notebook exists

mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::Value;
use tower::ServiceExt;
use uuid::Uuid;

use support::{
    bearer_header, build_auth_required_test_state, build_notebooks_state, seed_perm_user,
    test_router,
};

// ─── Auth guard ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn run_cell_no_auth_returns_401() {
    let (state, _) = build_auth_required_test_state().await;
    let app = test_router(state).await;

    let nb_id = Uuid::new_v4();
    let cell_id = Uuid::new_v4();
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/v1/notebooks/{nb_id}/{cell_id}/run"))
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"content":"print(1)"}"#))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ─── 404 beats 501 ───────────────────────────────────────────────────────────

#[tokio::test]
async fn run_cell_nonexistent_notebook_returns_404() {
    let (state, _) = build_notebooks_state().await;
    let user = seed_perm_user(&state, "run_404@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);
    let app = test_router(state).await;

    let fake_nb = Uuid::new_v4();
    let cell_id = Uuid::new_v4();
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/v1/notebooks/{fake_nb}/{cell_id}/run"))
        .header("Authorization", &auth)
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"content":"print(1)"}"#))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: Value = {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).expect("json")
    };
    assert_eq!(body["error"], "Notebook not found");
}

// ─── 501 stub ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn run_cell_existing_notebook_returns_501_with_body() {
    let (state, _) = build_notebooks_state().await;
    let user = seed_perm_user(&state, "run_501@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);

    // Create a notebook first.
    let app = test_router(state.clone()).await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/notebooks")
        .header("Authorization", &auth)
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"name":"Runnable","cells":[]}"#))
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let created: Value = {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).expect("json")
    };
    let nb_id = created["id"].as_str().expect("id").to_owned();

    // Now run a cell → 501.
    let app = test_router(state).await;
    let cell_id = Uuid::new_v4();
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/v1/notebooks/{nb_id}/{cell_id}/run"))
        .header("Authorization", &auth)
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"content":"print(42)"}"#))
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);

    // Verify exact byte output — field order: detail first, then code.
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    let body_str = std::str::from_utf8(&bytes).expect("utf8");
    assert_eq!(
        body_str,
        r#"{"detail":"Notebook cell execution is not implemented in this build","code":"NOTEBOOK_RUN_NOT_IMPLEMENTED"}"#,
    );
}

#[tokio::test]
async fn run_cell_missing_code_field_returns_400() {
    let (state, _) = build_notebooks_state().await;
    let user = seed_perm_user(&state, "run_400@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);

    // Create a notebook.
    let app = test_router(state.clone()).await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/notebooks")
        .header("Authorization", &auth)
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"name":"Runnable2","cells":[]}"#))
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    let nb_id: String = {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let v: Value = serde_json::from_slice(&bytes).expect("json");
        v["id"].as_str().expect("id").to_owned()
    };

    // Run with empty body → 400.
    let app = test_router(state).await;
    let cell_id = Uuid::new_v4();
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/v1/notebooks/{nb_id}/{cell_id}/run"))
        .header("Authorization", &auth)
        .header("Content-Type", "application/json")
        .body(Body::from("{}"))
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
