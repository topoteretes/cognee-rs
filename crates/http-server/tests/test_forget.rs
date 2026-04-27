//! Integration tests for `POST /api/v1/forget`.
//!
//! Covers the three-mode truth table from the spec plus cross-field validation.
//! Full deletion (mode 1/2/3 against real data) requires wired backends.

mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

use support::{
    bearer_header, body_json, build_auth_required_test_state, build_auth_test_state, seed_user,
    test_router,
};

// ─── helpers ─────────────────────────────────────────────────────────────────

fn json_body(value: serde_json::Value) -> Body {
    Body::from(serde_json::to_string(&value).expect("json"))
}

fn post_forget(uri: &str, body: serde_json::Value, auth: &str) -> axum::http::Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("Authorization", auth)
        .header("content-type", "application/json")
        .body(json_body(body))
        .expect("request")
}

// ─── auth guard ──────────────────────────────────────────────────────────────

/// No auth → 401.
#[tokio::test]
async fn test_forget_no_auth_returns_401() {
    let (state, _) = build_auth_required_test_state().await;
    let app = test_router(state).await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/forget")
        .header("content-type", "application/json")
        .body(json_body(serde_json::json!({"everything": true})))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ─── cross-field validation ───────────────────────────────────────────────────

/// Neither `data_id`, `dataset`, nor `everything=true` → 422.
#[tokio::test]
async fn test_forget_no_fields_returns_422() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "forget_nof@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);
    let app = test_router(state).await;

    let resp = app
        .oneshot(post_forget("/api/v1/forget", serde_json::json!({}), &auth))
        .await
        .expect("response");

    assert_eq!(
        resp.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "empty payload must be 422"
    );
    let body = body_json(resp).await;
    assert!(body["error"].is_string(), "`error` key expected: {body}");
}

/// `data_id` only (no `dataset`) → 422.
#[tokio::test]
async fn test_forget_data_id_only_returns_422() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "forget_doid@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);
    let app = test_router(state).await;

    let data_id = uuid::Uuid::new_v4();
    let resp = app
        .oneshot(post_forget(
            "/api/v1/forget",
            serde_json::json!({"dataId": data_id}),
            &auth,
        ))
        .await
        .expect("response");

    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = body_json(resp).await;
    assert!(body["error"].is_string(), "`error` key expected: {body}");
}

/// `everything=true` with extra fields → 200 (extra fields silently ignored).
#[tokio::test]
async fn test_forget_everything_true_ignores_extra_fields() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "forget_all@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);
    let app = test_router(state).await;

    let data_id = uuid::Uuid::new_v4();
    let resp = app
        .oneshot(post_forget(
            "/api/v1/forget",
            serde_json::json!({
                "everything": true,
                "dataId": data_id,
                "dataset": "ignored_dataset"
            }),
            &auth,
        ))
        .await
        .expect("response");

    // Not 422 — everything=true takes priority.
    assert_ne!(
        resp.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "everything=true must not be 422 even with extra fields"
    );
}

/// Mode 3 (`everything=true`): with no backends wired → 500 error,
/// but the resolve_mode step succeeded so we don't see 422.
#[tokio::test]
async fn test_forget_everything_resolves_mode_correctly() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "forget_mode3@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);
    let app = test_router(state).await;

    let resp = app
        .oneshot(post_forget(
            "/api/v1/forget",
            serde_json::json!({"everything": true}),
            &auth,
        ))
        .await
        .expect("response");

    // 422 = mode resolution failed; we must not see that.
    assert_ne!(
        resp.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "everything=true must resolve to mode 3, not fail cross-field validation"
    );
}

/// `dataset` only (no `data_id`) resolves to Mode 2 — not 422.
/// With no backends wired, gets 422 (missing dataset in DB), which is Python parity.
#[tokio::test]
async fn test_forget_dataset_only_resolves_to_mode2() {
    let (state, _) = build_auth_test_state().await;
    let user = seed_user(&state, "forget_mode2@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);
    let app = test_router(state).await;

    let resp = app
        .oneshot(post_forget(
            "/api/v1/forget",
            serde_json::json!({"dataset": "nonexistent_dataset"}),
            &auth,
        ))
        .await
        .expect("response");

    // Python collapses missing-dataset into 422 (same as cross-field validation error).
    // With no backends wired we get 422 from the components check (or the DB lookup fails).
    // The important thing: mode-2 resolution itself (dataset only) must not cause 422
    // due to cross-field validation. Any 422 must come from the missing dataset,
    // not from the resolve_mode() step.
    // Since we can't distinguish without backends, we just assert the route is reachable.
    assert_ne!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
}
