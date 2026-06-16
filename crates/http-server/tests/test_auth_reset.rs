#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for `POST /api/v1/auth/forgot-password` and
//! `POST /api/v1/auth/reset-password`.

mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

use support::{body_json, build_auth_test_state, seed_user, test_router};

// ─── POST /api/v1/auth/forgot-password ───────────────────────────────────────

/// `forgot-password` always returns 202 + null, even for unknown emails.
#[tokio::test]
async fn forgot_password_unknown_email_still_returns_202() {
    let (state, _events) = build_auth_test_state().await;
    let app = test_router(state).await;

    let body = serde_json::json!({"email": "nobody@example.com"});
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/auth/forgot-password")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).expect("serialize")))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
}

/// `forgot-password` for a real user should fire a PasswordReset mail event.
#[tokio::test]
async fn forgot_password_for_real_user_sends_reset_event() {
    let (state, events) = build_auth_test_state().await;
    seed_user(&state, "reset_me@example.com", "Str0ng!Pass#1").await;
    let app = test_router(state).await;

    let body = serde_json::json!({"email": "reset_me@example.com"});
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/auth/forgot-password")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).expect("serialize")))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    use cognee_http_server::auth::mailer::MailEventKind;
    let locked = events.lock().expect("lock");
    assert!(
        locked
            .iter()
            .any(|e| e.kind == MailEventKind::PasswordReset),
        "expected PasswordReset mail event"
    );
}

// ─── POST /api/v1/auth/reset-password ────────────────────────────────────────

/// `reset-password` with a garbage token returns 400 + RESET_PASSWORD_BAD_TOKEN.
#[tokio::test]
async fn reset_password_bad_token_returns_400() {
    let (state, _events) = build_auth_test_state().await;
    let app = test_router(state).await;

    let body = serde_json::json!({
        "token": "this.is.not.a.valid.jwt",
        "password": "NewStr0ng!Pass#2"
    });
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/auth/reset-password")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).expect("serialize")))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let resp_body = body_json(resp).await;
    assert_eq!(resp_body["detail"], "RESET_PASSWORD_BAD_TOKEN");
}

/// Full happy path: forgot-password → capture token → reset-password → login.
#[tokio::test]
async fn reset_password_full_flow() {
    let (state, events) = build_auth_test_state().await;
    seed_user(&state, "flow_reset@example.com", "Str0ng!Pass#1").await;

    // 1. Trigger forgot-password to obtain the reset token via mail event
    {
        let app = test_router(state.clone()).await;
        let body = serde_json::json!({"email": "flow_reset@example.com"});
        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/auth/forgot-password")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).expect("serialize")))
            .expect("request");
        let resp = app.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
    }

    // Extract the reset token from the captured mail event
    let reset_token = {
        use cognee_http_server::auth::mailer::MailEventKind;
        let locked = events.lock().expect("lock");
        locked
            .iter()
            .find(|e| e.kind == MailEventKind::PasswordReset)
            .and_then(|e| e.token.clone())
            .expect("PasswordReset mail event with token")
    };

    // 2. Reset the password
    {
        let app = test_router(state.clone()).await;
        let body = serde_json::json!({
            "token": reset_token,
            "password": "NewStr0ng!Pass#2"
        });
        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/auth/reset-password")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).expect("serialize")))
            .expect("request");
        let resp = app.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // 3. Login with new password must succeed
    {
        let app = test_router(state.clone()).await;
        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/auth/login")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(
                "username=flow_reset%40example.com&password=NewStr0ng!Pass%232",
            ))
            .expect("request");
        let resp = app.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let resp_body = body_json(resp).await;
        assert!(resp_body["access_token"].is_string());
    }

    // 4. Login with old password must fail
    {
        let app = test_router(state).await;
        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/auth/login")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(
                "username=flow_reset%40example.com&password=Str0ng!Pass%231",
            ))
            .expect("request");
        let resp = app.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}

/// Old reset token is invalidated after a successful reset (password_fgpt mismatch).
#[tokio::test]
async fn reset_password_old_token_rejected_after_reset() {
    let (state, events) = build_auth_test_state().await;
    seed_user(&state, "double_reset@example.com", "Str0ng!Pass#1").await;

    // First forgot-password
    {
        let app = test_router(state.clone()).await;
        let body = serde_json::json!({"email": "double_reset@example.com"});
        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/auth/forgot-password")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).expect("serialize")))
            .expect("request");
        let _ = app.oneshot(req).await.expect("response");
    }

    let first_token = {
        use cognee_http_server::auth::mailer::MailEventKind;
        let locked = events.lock().expect("lock");
        locked
            .iter()
            .find(|e| e.kind == MailEventKind::PasswordReset)
            .and_then(|e| e.token.clone())
            .expect("first token")
    };

    // Successfully reset with the first token
    {
        let app = test_router(state.clone()).await;
        let body = serde_json::json!({
            "token": &first_token,
            "password": "NewStr0ng!Pass#2"
        });
        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/auth/reset-password")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).expect("serialize")))
            .expect("request");
        let resp = app.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // Try to reuse the same token — must be rejected (fingerprint now stale)
    {
        let app = test_router(state.clone()).await;
        let body = serde_json::json!({
            "token": &first_token,
            "password": "YetAnother!Pass#3"
        });
        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/auth/reset-password")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).expect("serialize")))
            .expect("request");
        let resp = app.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let resp_body = body_json(resp).await;
        assert_eq!(resp_body["detail"], "RESET_PASSWORD_BAD_TOKEN");
    }
}
