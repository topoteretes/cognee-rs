//! Mailer trait and implementations.
//!
//! Three implementations are available:
//!
//! - [`LoggingMailer`] (default) — logs the token; mirrors Python's `on_after_*`
//!   hooks that use `logger.info(...)`. Do not use in production.
//!
//! - [`ConsoleMailer`] (tests) — captures events in a `Mutex<Vec<MailEvent>>`
//!   so tests can assert on the number of messages sent.
//!
//! - [`SmtpMailer`] — sends real emails via SMTP (`lettre`). Activated by
//!   setting `SMTP_HOST` (and `SMTP_FROM`) in the environment; see `smtp.rs`
//!   for the full env-var reference.
//!
//! ## Building the default mailer
//!
//! Library embedders that construct `AppState` directly can pass any
//! `Arc<dyn Mailer>` they choose — bring your own SES/SendGrid/etc. impl.
//! Server startup uses [`build_default`] which reads env vars.

mod console;
mod logging;
mod smtp;

pub use console::{ConsoleMailer, MailEvent, MailEventKind};
pub use logging::LoggingMailer;
pub use smtp::SmtpMailer;

use std::sync::Arc;

use async_trait::async_trait;
use cognee_database::AuthUser;

// ─── MailerError ─────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum MailerError {
    /// Failed to send a message (transport-layer error).
    #[error("mailer send error: {0}")]
    Send(String),

    /// Configuration error (missing env var, invalid address, etc.).
    #[error("mailer config error: {0}")]
    Config(String),

    /// SMTP transport error.
    #[error("mailer transport error: {0}")]
    Transport(String),
}

// ─── Mailer trait ─────────────────────────────────────────────────────────────

#[async_trait]
pub trait Mailer: Send + Sync + std::fmt::Debug {
    async fn send_register_welcome(&self, user: &AuthUser) -> Result<(), MailerError>;
    async fn send_password_reset(&self, user: &AuthUser, token: &str) -> Result<(), MailerError>;
    async fn send_email_verify(&self, user: &AuthUser, token: &str) -> Result<(), MailerError>;
}

// ─── build_default ────────────────────────────────────────────────────────────

/// Select the best mailer based on environment variables.
///
/// - If `SMTP_HOST` is set → construct `SmtpMailer::from_env()`. Construction
///   errors abort startup (the caller receives `Err`).
/// - Otherwise → `LoggingMailer` (logs tokens; safe default for dev/CI).
///
/// Library embedders that construct `AppState` directly bypass `build_default`
/// and can plug in any `Arc<dyn Mailer>` (their own SES/SendGrid/etc. impl).
pub fn build_default() -> Result<Arc<dyn Mailer>, MailerError> {
    if std::env::var("SMTP_HOST").is_ok() {
        Ok(Arc::new(SmtpMailer::from_env()?))
    } else {
        Ok(Arc::new(LoggingMailer))
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial]
    fn build_default_without_smtp_host_returns_logging_mailer() {
        // SAFETY: tests run single-threaded (no concurrent env access)
        unsafe { std::env::remove_var("SMTP_HOST") };
        let mailer = build_default().expect("build_default should not fail without SMTP_HOST");
        // LoggingMailer is returned; we can't downcast Arc<dyn Mailer> in a
        // meaningful way here, but the smoke test asserts no panic.
        let _ = mailer;
    }

    #[test]
    #[serial]
    fn build_default_with_smtp_host_but_missing_from_errors() {
        // SAFETY: tests run single-threaded (no concurrent env access)
        unsafe {
            std::env::set_var("SMTP_HOST", "smtp.example.com");
            std::env::remove_var("SMTP_FROM");
        }
        let err = build_default().unwrap_err();
        assert!(matches!(err, MailerError::Config(_)));
        // SAFETY: tests run single-threaded (no concurrent env access)
        unsafe { std::env::remove_var("SMTP_HOST") };
    }
}
