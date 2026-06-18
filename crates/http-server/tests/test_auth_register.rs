#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for `POST /api/v1/auth/register`.

mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

use support::{body_json, build_auth_test_state, seed_user, test_router};

// ─── POST /api/v1/auth/register ──────────────────────────────────────────────

#[tokio::test]
async fn register_success_returns_201_and_user_read() {
    let (state, _events) = build_auth_test_state().await;
    let app = test_router(state).await;

    let body = serde_json::json!({
        "email": "newuser@example.com",
        "password": "Str0ng!Pass#1"
    });

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/auth/register")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).expect("serialize")))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::CREATED);

    let resp_body = body_json(resp).await;
    assert_eq!(resp_body["email"], "newuser@example.com");
    assert!(resp_body["id"].is_string());
    // safe=True: superuser must be false
    assert_eq!(resp_body["is_superuser"], false);
    assert_eq!(resp_body["is_active"], true);
}

#[tokio::test]
async fn register_duplicate_email_returns_400() {
    let (state, _events) = build_auth_test_state().await;
    seed_user(&state, "existing@example.com", "Str0ng!Pass#1").await;
    let app = test_router(state).await;

    let body = serde_json::json!({
        "email": "existing@example.com",
        "password": "Str0ng!Pass#1"
    });

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/auth/register")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).expect("serialize")))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let resp_body = body_json(resp).await;
    assert_eq!(resp_body["detail"], "REGISTER_USER_ALREADY_EXISTS");
}

#[tokio::test]
async fn register_invalid_password_email_substring_returns_400_with_reason() {
    let (state, _events) = build_auth_test_state().await;
    let app = test_router(state).await;

    // Password that contains the email as a substring — invalid per fastapi-users rules
    let body = serde_json::json!({
        "email": "user@example.com",
        "password": "user@example.com_suffix"
    });

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/auth/register")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).expect("serialize")))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let resp_body = body_json(resp).await;
    assert_eq!(resp_body["detail"]["code"], "REGISTER_INVALID_PASSWORD");
    assert!(resp_body["detail"]["reason"].is_string());
}

#[tokio::test]
async fn register_missing_email_field_returns_422() {
    let (state, _events) = build_auth_test_state().await;
    let app = test_router(state).await;

    // Missing `email` field — validation should reject
    let body = serde_json::json!({
        "password": "Str0ng!Pass#1"
    });

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/auth/register")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).expect("serialize")))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    // Either 400 (validation) or 422 (axum extractor rejection)
    assert!(
        resp.status() == StatusCode::BAD_REQUEST
            || resp.status() == StatusCode::UNPROCESSABLE_ENTITY,
        "expected 400 or 422 for missing email, got {}",
        resp.status()
    );
}

#[tokio::test]
async fn register_sends_welcome_mail_event() {
    let (state, events) = build_auth_test_state().await;
    let app = test_router(state).await;

    let body = serde_json::json!({
        "email": "mail_check@example.com",
        "password": "Str0ng!Pass#1"
    });

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/auth/register")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).expect("serialize")))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Verify that a RegisterWelcome mail event was captured
    use cognee_http_server::auth::mailer::MailEventKind;
    let locked = events.lock().expect("lock");
    assert!(
        locked
            .iter()
            .any(|e| e.kind == MailEventKind::RegisterWelcome),
        "expected RegisterWelcome mail event"
    );
}
