//! Integration tests for `ConsoleMailer`.
//!
//! Verifies that `ConsoleMailer` captures events for `send_register_welcome`,
//! `send_password_reset`, and `send_email_verify` without actually sending email.

use cognee_database::AuthUser;
use cognee_http_server::auth::mailer::{ConsoleMailer, MailEventKind, Mailer};
use uuid::Uuid;

fn fake_user(email: &str) -> AuthUser {
    AuthUser {
        id: Uuid::new_v4(),
        email: email.to_owned(),
        hashed_password: String::new(),
        is_active: true,
        is_superuser: false,
        is_verified: true,
        tenant_id: None,
        parent_user_id: None,
        created_at: chrono::Utc::now(),
    }
}

#[tokio::test]
async fn console_mailer_captures_register_welcome() {
    let (mailer, events) = ConsoleMailer::new();
    let user = fake_user("welcome@example.com");

    mailer
        .send_register_welcome(&user)
        .await
        .expect("send_register_welcome");

    let captured = events.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].kind, MailEventKind::RegisterWelcome);
    assert_eq!(captured[0].user_id, user.id);
    assert!(captured[0].token.is_none());
}

#[tokio::test]
async fn console_mailer_captures_password_reset_with_token() {
    let (mailer, events) = ConsoleMailer::new();
    let user = fake_user("reset@example.com");

    mailer
        .send_password_reset(&user, "tok-abc123")
        .await
        .expect("send_password_reset");

    let captured = events.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].kind, MailEventKind::PasswordReset);
    assert_eq!(captured[0].token.as_deref(), Some("tok-abc123"));
}

#[tokio::test]
async fn console_mailer_captures_email_verify_with_token() {
    let (mailer, events) = ConsoleMailer::new();
    let user = fake_user("verify@example.com");

    mailer
        .send_email_verify(&user, "tok-verify99")
        .await
        .expect("send_email_verify");

    let captured = events.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].kind, MailEventKind::EmailVerify);
    assert_eq!(captured[0].token.as_deref(), Some("tok-verify99"));
}

#[tokio::test]
async fn console_mailer_accumulates_multiple_events() {
    let (mailer, events) = ConsoleMailer::new();
    let user = fake_user("multi@example.com");

    mailer.send_register_welcome(&user).await.expect("welcome");
    mailer
        .send_password_reset(&user, "t1")
        .await
        .expect("reset");
    mailer.send_email_verify(&user, "t2").await.expect("verify");

    let captured = events.lock().unwrap();
    assert_eq!(captured.len(), 3);
}
