#![cfg(any())] // cognee-http-server gated on oss-split branch (T2-move §4 S2).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for `GET/POST/PUT/DELETE /api/v1/notebooks`.
//!
//! Covers:
//! - 401 when not authenticated (require_authentication mode)
//! - List returns 2 tutorial notebooks on first call (seeding)
//! - Create → List → Update → Delete round-trip
//! - 404 returned as `{"error": "Notebook not found"}` (not `{"detail": ...}`)
//! - DELETE returns `200 {}` (not 204)
//! - Owner isolation: B cannot see/delete A's notebooks

mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use tower::ServiceExt;

use support::{
    bearer_header, body_json, build_auth_required_test_state, build_notebooks_state,
    seed_perm_user, test_router,
};

// ─── Auth guard ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn list_notebooks_no_auth_returns_401() {
    let (state, _) = build_auth_required_test_state().await;
    let app = test_router(state).await;

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/notebooks")
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn create_notebook_no_auth_returns_401() {
    let (state, _) = build_auth_required_test_state().await;
    let app = test_router(state).await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/notebooks")
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"name":"Test"}"#))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ─── Tutorial seeding on first GET /api/v1/notebooks ─────────────────────────

#[tokio::test]
async fn list_notebooks_seeds_two_tutorials_on_first_call() {
    let (state, _) = build_notebooks_state().await;
    let user = seed_perm_user(&state, "nb_list@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);
    let app = test_router(state).await;

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/notebooks")
        .header("Authorization", auth)
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);

    let body: Value = body_json(resp).await;
    let notebooks = body.as_array().expect("array");
    assert_eq!(notebooks.len(), 2, "should seed exactly 2 tutorials");

    // Both must have deletable = false.
    for nb in notebooks {
        assert_eq!(
            nb["deletable"], false,
            "tutorial notebooks are not deletable"
        );
    }
}

#[tokio::test]
async fn list_notebooks_second_call_does_not_duplicate_tutorials() {
    let (state, _) = build_notebooks_state().await;
    let user = seed_perm_user(&state, "nb_list2@example.com", "Str0ng!Pass#2").await;
    let auth = bearer_header(&user, &state);

    // First call.
    let app1 = test_router(state.clone()).await;
    let req1 = Request::builder()
        .method("GET")
        .uri("/api/v1/notebooks")
        .header("Authorization", &auth)
        .body(Body::empty())
        .expect("request");
    let resp1 = app1.oneshot(req1).await.expect("response");
    assert_eq!(resp1.status(), StatusCode::OK);
    let body1: Value = body_json(resp1).await;
    let count_first = body1.as_array().expect("array").len();

    // Second call — same state/DB.
    let app2 = test_router(state.clone()).await;
    let req2 = Request::builder()
        .method("GET")
        .uri("/api/v1/notebooks")
        .header("Authorization", &auth)
        .body(Body::empty())
        .expect("request");
    let resp2 = app2.oneshot(req2).await.expect("response");
    assert_eq!(resp2.status(), StatusCode::OK);
    let body2: Value = body_json(resp2).await;
    let count_second = body2.as_array().expect("array").len();

    assert_eq!(count_first, count_second, "seeding must be idempotent");
}

// ─── Create ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn create_notebook_returns_200_with_dto() {
    let (state, _) = build_notebooks_state().await;
    let user = seed_perm_user(&state, "nb_create@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);
    let app = test_router(state).await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/notebooks")
        .header("Authorization", &auth)
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"name":"My Notebook","cells":[]}"#))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);

    let body: Value = body_json(resp).await;
    assert_eq!(body["name"], "My Notebook");
    assert!(body["id"].is_string());
    assert_eq!(
        body["deletable"], true,
        "always deletable (Python bug parity)"
    );
}

#[tokio::test]
async fn create_notebook_missing_name_returns_400() {
    let (state, _) = build_notebooks_state().await;
    let user = seed_perm_user(&state, "nb_create_bad@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);
    let app = test_router(state).await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/notebooks")
        .header("Authorization", &auth)
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"cells":[]}"#))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ─── CRUD round-trip ─────────────────────────────────────────────────────────

#[tokio::test]
async fn notebooks_full_crud_round_trip() {
    let (state, _) = build_notebooks_state().await;
    let user = seed_perm_user(&state, "nb_crud@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);

    // Create
    let app = test_router(state.clone()).await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/notebooks")
        .header("Authorization", &auth)
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"name":"Round Trip","cells":[]}"#))
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let created: Value = body_json(resp).await;
    let nb_id = created["id"].as_str().expect("id").to_owned();

    // Update name
    let app = test_router(state.clone()).await;
    let put_body = json!({"name": "Renamed"}).to_string();
    let req = Request::builder()
        .method("PUT")
        .uri(format!("/api/v1/notebooks/{nb_id}"))
        .header("Authorization", &auth)
        .header("Content-Type", "application/json")
        .body(Body::from(put_body))
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let updated: Value = body_json(resp).await;
    assert_eq!(updated["name"], "Renamed");

    // Delete
    let app = test_router(state.clone()).await;
    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/api/v1/notebooks/{nb_id}"))
        .header("Authorization", &auth)
        .body(Body::empty())
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK, "DELETE returns 200 not 204");
    let del_body: Value = body_json(resp).await;
    assert_eq!(del_body, json!({}), "DELETE body is {{}}");
}

// ─── 404 shape ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn update_nonexistent_notebook_returns_404_error_key() {
    let (state, _) = build_notebooks_state().await;
    let user = seed_perm_user(&state, "nb_404@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);
    let app = test_router(state).await;

    let fake_id = uuid::Uuid::new_v4();
    let req = Request::builder()
        .method("PUT")
        .uri(format!("/api/v1/notebooks/{fake_id}"))
        .header("Authorization", &auth)
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"name":"X"}"#))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: Value = body_json(resp).await;
    assert_eq!(body["error"], "Notebook not found");
    assert!(
        body.get("detail").is_none(),
        "must use 'error' not 'detail'"
    );
}

#[tokio::test]
async fn delete_nonexistent_notebook_returns_404_error_key() {
    let (state, _) = build_notebooks_state().await;
    let user = seed_perm_user(&state, "nb_del404@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);
    let app = test_router(state).await;

    let fake_id = uuid::Uuid::new_v4();
    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/api/v1/notebooks/{fake_id}"))
        .header("Authorization", &auth)
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: Value = body_json(resp).await;
    assert_eq!(body["error"], "Notebook not found");
}

// ─── Owner isolation ──────────────────────────────────────────────────────────

#[tokio::test]
async fn user_b_cannot_see_user_a_notebook() {
    let (state, _) = build_notebooks_state().await;
    let user_a = seed_perm_user(&state, "nb_a@example.com", "Str0ng!Pass#1").await;
    let user_b = seed_perm_user(&state, "nb_b@example.com", "Str0ng!Pass#2").await;
    let auth_a = bearer_header(&user_a, &state);
    let auth_b = bearer_header(&user_b, &state);

    // A creates a notebook
    let app = test_router(state.clone()).await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/notebooks")
        .header("Authorization", &auth_a)
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"name":"A's Secret"}"#))
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let created: Value = body_json(resp).await;
    let nb_id = created["id"].as_str().expect("id").to_owned();

    // B tries to delete A's notebook → 404
    let app = test_router(state.clone()).await;
    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/api/v1/notebooks/{nb_id}"))
        .header("Authorization", &auth_b)
        .body(Body::empty())
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
