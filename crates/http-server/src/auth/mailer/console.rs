//! `ConsoleMailer` — test mailer that captures events in a `Mutex<Vec<MailEvent>>`.
//!
//! Tests can assert on the number of messages sent and their content.

use async_trait::async_trait;
use cognee_database::AuthUser;

use super::{Mailer, MailerError};

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
#[derive(Debug)]
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
