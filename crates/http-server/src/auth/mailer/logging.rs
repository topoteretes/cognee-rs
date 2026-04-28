//! `LoggingMailer` — default mailer that logs tokens instead of sending emails.
//!
//! Matches Python's `on_after_*` hooks that call `logger.info(...)`.
//! **Warning**: in production this exposes reset/verify tokens in logs.
//! Wire `SmtpMailer` (env var `SMTP_HOST`) for real email delivery.

use async_trait::async_trait;
use cognee_database::AuthUser;

use super::{Mailer, MailerError};

#[derive(Debug)]
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
