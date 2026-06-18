#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for `SmtpMailer` construction and env-var handling.
//!
//! These tests do NOT send real emails — they verify:
//! - Construction fails cleanly when `SMTP_HOST` / `SMTP_FROM` are missing.
//! - Construction succeeds and produces a usable mailer when vars are set.
//! - `build_default` returns `LoggingMailer` when `SMTP_HOST` is absent.
//! - `build_default` returns `Err` when `SMTP_HOST` is set but `SMTP_FROM` is missing.
//!
//! Real SMTP delivery tests are `#[ignore]`d so they only run when explicitly
//! opted into (`cargo test -- --ignored`) with real credentials.
//!
//! IMPORTANT: env-var mutation makes these tests incompatible with parallel
//! execution.  The `serial_test` crate is used to serialise them.

use serial_test::serial;

use cognee_http_server::auth::build_default_mailer;
use cognee_http_server::auth::mailer::{MailerError, SmtpMailer};

// ─── SmtpMailer::from_env ────────────────────────────────────────────────────

fn clear_smtp_env() {
    // SAFETY: single-threaded (serial attribute ensures no concurrent env access)
    unsafe {
        std::env::remove_var("SMTP_HOST");
        std::env::remove_var("SMTP_FROM");
        std::env::remove_var("SMTP_PORT");
        std::env::remove_var("SMTP_USER");
        std::env::remove_var("SMTP_PASS");
    }
}

#[test]
#[serial]
fn smtp_missing_host_returns_config_error() {
    clear_smtp_env();
    let err = SmtpMailer::from_env().unwrap_err();
    assert!(matches!(err, MailerError::Config(_)));
    assert!(err.to_string().contains("SMTP_HOST"));
}

#[test]
#[serial]
fn smtp_missing_from_returns_config_error() {
    clear_smtp_env();
    // SAFETY: single-threaded
    unsafe { std::env::set_var("SMTP_HOST", "smtp.example.com") };
    let err = SmtpMailer::from_env().unwrap_err();
    assert!(matches!(err, MailerError::Config(_)));
    assert!(err.to_string().contains("SMTP_FROM"));
    clear_smtp_env();
}

#[test]
#[serial]
fn smtp_valid_env_constructs_ok() {
    clear_smtp_env();
    // SAFETY: single-threaded
    unsafe {
        std::env::set_var("SMTP_HOST", "smtp.example.com");
        std::env::set_var("SMTP_FROM", "noreply@example.com");
    }
    let result = SmtpMailer::from_env();
    assert!(result.is_ok(), "expected Ok, got {result:?}");
    clear_smtp_env();
}

// ─── build_default_mailer ────────────────────────────────────────────────────

#[test]
#[serial]
fn build_default_without_smtp_host_returns_logging_mailer() {
    clear_smtp_env();
    let mailer = build_default_mailer().expect("should succeed without SMTP_HOST");
    // Can't downcast Arc<dyn Mailer> — just verify it doesn't panic.
    let _ = mailer;
}

#[test]
#[serial]
fn build_default_with_smtp_host_but_no_from_errors() {
    clear_smtp_env();
    // SAFETY: single-threaded
    unsafe {
        std::env::set_var("SMTP_HOST", "smtp.example.com");
        std::env::remove_var("SMTP_FROM");
    }
    let err = build_default_mailer().unwrap_err();
    assert!(matches!(err, MailerError::Config(_)));
    clear_smtp_env();
}

// ─── Real SMTP delivery (ignored by default) ─────────────────────────────────

/// Send a real test email.
///
/// Run with:
/// ```
/// SMTP_HOST=smtp.example.com \
/// SMTP_FROM="Test <test@example.com>" \
/// TEST_EMAIL_TO=you@example.com \
/// cargo test -p cognee-http-server -- --ignored test_smtp_real_send
/// ```
#[tokio::test]
#[ignore = "requires real SMTP credentials — opt-in only"]
async fn test_smtp_real_send() {
    use cognee_database::AuthUser;
    use cognee_http_server::auth::mailer::Mailer;
    use uuid::Uuid;

    let to = std::env::var("TEST_EMAIL_TO").expect("set TEST_EMAIL_TO");
    let mailer = SmtpMailer::from_env().expect("valid SMTP config");

    let user = AuthUser {
        id: Uuid::new_v4(),
        email: to,
        hashed_password: String::new(),
        is_active: true,
        is_superuser: false,
        is_verified: false,
        tenant_id: None,
        parent_user_id: None,
        created_at: chrono::Utc::now(),
    };

    mailer.send_register_welcome(&user).await.expect("send");
    mailer
        .send_password_reset(&user, "test-reset-token")
        .await
        .expect("send");
    mailer
        .send_email_verify(&user, "test-verify-token")
        .await
        .expect("send");
}
