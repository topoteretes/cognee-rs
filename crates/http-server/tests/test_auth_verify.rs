//! Integration tests for `POST /api/v1/auth/request-verify-token` and
//! `POST /api/v1/auth/verify`.
//!
//! Note: `seed_user` creates users with `is_verified=true` (cognee default).
//! Tests that need an unverified user must update the user directly via the
//! repo after seeding.

mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use cognee_database::UpdateUserPayload;
use tower::ServiceExt;

use support::{body_json, build_auth_test_state, seed_user, test_router};

// ─── POST /api/v1/auth/request-verify-token ──────────────────────────────────

/// Always returns 202 + null, even for unknown emails.
#[tokio::test]
async fn request_verify_token_unknown_email_returns_202() {
    let (state, _events) = build_auth_test_state().await;
    let app = test_router(state).await;

    let body = serde_json::json!({"email": "nobody@example.com"});
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/auth/request-verify-token")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).expect("serialize")))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
}

/// For an already-verified user, still returns 202 but does NOT send a token.
#[tokio::test]
async fn request_verify_token_already_verified_returns_202_no_event() {
    let (state, events) = build_auth_test_state().await;
    // seed_user creates users with is_verified=true
    seed_user(&state, "verified@example.com", "Str0ng!Pass#1").await;
    let app = test_router(state).await;

    let body = serde_json::json!({"email": "verified@example.com"});
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/auth/request-verify-token")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).expect("serialize")))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    // No EmailVerify event should have been fired (already verified → silent)
    use cognee_http_server::auth::mailer::MailEventKind;
    let locked = events.lock().expect("lock");
    assert!(
        !locked.iter().any(|e| e.kind == MailEventKind::EmailVerify),
        "should not send verify token to already-verified user"
    );
}

/// For an unverified user, returns 202 and fires an EmailVerify mail event.
#[tokio::test]
async fn request_verify_token_for_unverified_user_sends_event() {
    let (state, events) = build_auth_test_state().await;
    let user = seed_user(&state, "unverified@example.com", "Str0ng!Pass#1").await;

    // Manually mark user as unverified
    let auth = state.auth.as_ref().expect("auth ctx");
    auth.user_repo
        .update(
            user.id,
            UpdateUserPayload {
                is_verified: Some(false),
                ..Default::default()
            },
        )
        .await
        .expect("mark unverified");

    let app = test_router(state).await;

    let body = serde_json::json!({"email": "unverified@example.com"});
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/auth/request-verify-token")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).expect("serialize")))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    use cognee_http_server::auth::mailer::MailEventKind;
    let locked = events.lock().expect("lock");
    assert!(
        locked.iter().any(|e| e.kind == MailEventKind::EmailVerify),
        "expected EmailVerify mail event for unverified user"
    );
}

// ─── POST /api/v1/auth/verify ────────────────────────────────────────────────

/// Bad token returns 400 + VERIFY_USER_BAD_TOKEN.
#[tokio::test]
async fn verify_bad_token_returns_400() {
    let (state, _events) = build_auth_test_state().await;
    let app = test_router(state).await;

    let body = serde_json::json!({"token": "this.is.not.a.valid.jwt"});
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/auth/verify")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).expect("serialize")))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let resp_body = body_json(resp).await;
    assert_eq!(resp_body["detail"], "VERIFY_USER_BAD_TOKEN");
}

/// Full happy path: request token → verify → is_verified becomes true.
#[tokio::test]
async fn verify_full_flow() {
    let (state, events) = build_auth_test_state().await;
    let user = seed_user(&state, "verify_flow@example.com", "Str0ng!Pass#1").await;

    // Mark user as unverified first
    let auth = state.auth.as_ref().expect("auth ctx");
    auth.user_repo
        .update(
            user.id,
            UpdateUserPayload {
                is_verified: Some(false),
                ..Default::default()
            },
        )
        .await
        .expect("mark unverified");

    // 1. Request verify token
    {
        let app = test_router(state.clone()).await;
        let body = serde_json::json!({"email": "verify_flow@example.com"});
        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/auth/request-verify-token")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).expect("serialize")))
            .expect("request");
        let resp = app.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
    }

    // Extract the verify token
    let verify_token = {
        use cognee_http_server::auth::mailer::MailEventKind;
        let locked = events.lock().expect("lock");
        locked
            .iter()
            .find(|e| e.kind == MailEventKind::EmailVerify)
            .and_then(|e| e.token.clone())
            .expect("EmailVerify mail event with token")
    };

    // 2. Verify — should return 200 + UserReadDTO with is_verified=true
    {
        let app = test_router(state.clone()).await;
        let body = serde_json::json!({"token": verify_token});
        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/auth/verify")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).expect("serialize")))
            .expect("request");
        let resp = app.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);

        let resp_body = body_json(resp).await;
        assert_eq!(resp_body["email"], "verify_flow@example.com");
        assert_eq!(resp_body["is_verified"], true);
    }
}

/// Verifying an already-verified user returns 400 + VERIFY_USER_ALREADY_VERIFIED.
#[tokio::test]
async fn verify_already_verified_returns_400() {
    let (state, events) = build_auth_test_state().await;
    let user = seed_user(&state, "double_verify@example.com", "Str0ng!Pass#1").await;

    // Mark as unverified so we can get a valid token
    let auth = state.auth.as_ref().expect("auth ctx");
    auth.user_repo
        .update(
            user.id,
            UpdateUserPayload {
                is_verified: Some(false),
                ..Default::default()
            },
        )
        .await
        .expect("mark unverified");

    // Request verify token
    {
        let app = test_router(state.clone()).await;
        let body = serde_json::json!({"email": "double_verify@example.com"});
        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/auth/request-verify-token")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).expect("serialize")))
            .expect("request");
        let _ = app.oneshot(req).await.expect("response");
    }

    let verify_token = {
        use cognee_http_server::auth::mailer::MailEventKind;
        let locked = events.lock().expect("lock");
        locked
            .iter()
            .find(|e| e.kind == MailEventKind::EmailVerify)
            .and_then(|e| e.token.clone())
            .expect("EmailVerify mail event with token")
    };

    // First verify: succeeds
    {
        let app = test_router(state.clone()).await;
        let body = serde_json::json!({"token": &verify_token});
        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/auth/verify")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).expect("serialize")))
            .expect("request");
        let resp = app.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // Second verify with same token: must fail with ALREADY_VERIFIED
    {
        let app = test_router(state.clone()).await;
        let body = serde_json::json!({"token": &verify_token});
        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/auth/verify")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).expect("serialize")))
            .expect("request");
        let resp = app.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let resp_body = body_json(resp).await;
        assert_eq!(resp_body["detail"], "VERIFY_USER_ALREADY_VERIFIED");
    }
}
