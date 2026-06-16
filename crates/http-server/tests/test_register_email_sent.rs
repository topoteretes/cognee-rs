#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration test: `POST /api/v1/auth/register` fires a welcome email via the
//! wired `Mailer`.
//!
//! Uses `ConsoleMailer` so no real SMTP connection is needed.  Verifies that
//! exactly one `RegisterWelcome` event is captured after a successful register.

mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

use cognee_http_server::auth::mailer::MailEventKind;
use support::{build_auth_test_state, test_router};

#[tokio::test]
async fn register_sends_welcome_email_via_mailer() {
    let (state, mail_events) = build_auth_test_state().await;
    let app = test_router(state).await;

    let body = r#"{"email":"welcome_email@example.com","password":"Str0ng!Pass#1"}"#;
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/auth/register")
        .header("Content-Type", "application/json")
        .body(Body::from(body))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::CREATED);

    let events = mail_events.lock().unwrap();
    assert_eq!(events.len(), 1, "exactly one email per registration");
    assert_eq!(
        events[0].kind,
        MailEventKind::RegisterWelcome,
        "should be a welcome email"
    );
    assert_eq!(events[0].token, None, "welcome email has no token");
}

#[tokio::test]
async fn register_duplicate_email_does_not_send_welcome_email() {
    let (state, mail_events) = build_auth_test_state().await;
    let app = test_router(state.clone()).await;

    let body = r#"{"email":"dup@example.com","password":"Str0ng!Pass#1"}"#;

    // First registration — succeeds.
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/auth/register")
        .header("Content-Type", "application/json")
        .body(Body::from(body))
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Second registration with same email — should fail.
    let app2 = test_router(state).await;
    let req2 = Request::builder()
        .method("POST")
        .uri("/api/v1/auth/register")
        .header("Content-Type", "application/json")
        .body(Body::from(body))
        .expect("request");
    let resp2 = app2.oneshot(req2).await.expect("response");
    assert_eq!(resp2.status(), StatusCode::BAD_REQUEST);

    // Only one email should have been sent.
    let events = mail_events.lock().unwrap();
    assert_eq!(
        events.len(),
        1,
        "only first registration fires welcome email"
    );
}
