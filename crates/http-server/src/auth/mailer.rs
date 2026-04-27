//! Mailer trait and implementations.
//!
//! `LoggingMailer` is the default for production (P1) — logs the token instead
//! of sending an email, matching Python's `on_after_forgot_password` hook that
//! uses `logger.info(...)`.
//!
//! `ConsoleMailer` is used in tests — captures events in a `Mutex<Vec<…>>`
//! so tests can assert on the number of messages sent.
//!
//! `SmtpMailer` is deferred to P7.

use async_trait::async_trait;
use cognee_database::AuthUser;

// ─── Mailer error ─────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum MailerError {
    #[error("mailer error: {0}")]
    Send(String),
}

// ─── Mailer trait ─────────────────────────────────────────────────────────────

#[async_trait]
pub trait Mailer: Send + Sync {
    async fn send_register_welcome(&self, user: &AuthUser) -> Result<(), MailerError>;
    async fn send_password_reset(&self, user: &AuthUser, token: &str) -> Result<(), MailerError>;
    async fn send_email_verify(&self, user: &AuthUser, token: &str) -> Result<(), MailerError>;
}

// ─── LoggingMailer ────────────────────────────────────────────────────────────

/// Default mailer — matches Python's `on_after_*` hooks that log the token.
///
/// **Warning**: In production, this exposes reset/verify tokens in logs.
/// Wire `SmtpMailer` (P7) for real email delivery.
pub struct LoggingMailer;

#[async_trait]
impl Mailer for LoggingMailer {
    async fn send_register_welcome(&self, user: &AuthUser) -> Result<(), MailerError> {
        tracing::info!(user_id = %user.id, "User registered: {}", user.email);
        Ok(())
    }

    async fn send_password_reset(&self, user: &AuthUser, token: &str) -> Result<(), MailerError> {
        tracing::info!(
            user_id = %user.id,
            "User {} has forgot their password. Reset token: {}",
            user.id,
            token
        );
        Ok(())
    }

    async fn send_email_verify(&self, user: &AuthUser, token: &str) -> Result<(), MailerError> {
        tracing::info!(
            user_id = %user.id,
            "User {} has requested email verification. Token: {}",
            user.id,
            token
        );
        Ok(())
    }
}

// ─── ConsoleMailer (tests) ────────────────────────────────────────────────────

/// Captured mail event for tests.
#[derive(Debug, Clone)]
pub struct MailEvent {
    pub kind: MailEventKind,
    pub user_id: uuid::Uuid,
    pub token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MailEventKind {
    RegisterWelcome,
    PasswordReset,
    EmailVerify,
}

/// Test mailer that records events instead of sending emails.
pub struct ConsoleMailer {
    events: std::sync::Arc<std::sync::Mutex<Vec<MailEvent>>>,
}

impl ConsoleMailer {
    pub fn new() -> (Self, std::sync::Arc<std::sync::Mutex<Vec<MailEvent>>>) {
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        (
            Self {
                events: events.clone(),
            },
            events,
        )
    }
}

impl Default for ConsoleMailer {
    fn default() -> Self {
        Self {
            events: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl Mailer for ConsoleMailer {
    async fn send_register_welcome(&self, user: &AuthUser) -> Result<(), MailerError> {
        self.events
            .lock()
            // lock poison is unrecoverable
            .unwrap()
            .push(MailEvent {
                kind: MailEventKind::RegisterWelcome,
                user_id: user.id,
                token: None,
            });
        Ok(())
    }

    async fn send_password_reset(&self, user: &AuthUser, token: &str) -> Result<(), MailerError> {
        self.events
            .lock()
            // lock poison is unrecoverable
            .unwrap()
            .push(MailEvent {
                kind: MailEventKind::PasswordReset,
                user_id: user.id,
                token: Some(token.to_owned()),
            });
        Ok(())
    }

    async fn send_email_verify(&self, user: &AuthUser, token: &str) -> Result<(), MailerError> {
        self.events
            .lock()
            // lock poison is unrecoverable
            .unwrap()
            .push(MailEvent {
                kind: MailEventKind::EmailVerify,
                user_id: user.id,
                token: Some(token.to_owned()),
            });
        Ok(())
    }
}
