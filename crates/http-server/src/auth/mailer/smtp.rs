//! `SmtpMailer` — production email delivery via SMTP using `lettre`.
//!
//! Constructed via `SmtpMailer::from_env()`.  Required env vars:
//! - `SMTP_HOST` — SMTP server hostname (required)
//! - `SMTP_FROM` — `From:` header e.g. `"Cognee <noreply@cognee.ai>"` (required)
//!
//! Optional env vars:
//! - `SMTP_PORT` — default `465` (implicit TLS); `587` → STARTTLS; `25` → plaintext
//! - `SMTP_USER` / `SMTP_PASS` — SMTP credentials (anonymous if absent)
//! - `SMTP_RESET_LINK_TEMPLATE` — `{token}` placeholder, default `"https://app.cognee.ai/reset?token={token}"`
//! - `SMTP_VERIFY_LINK_TEMPLATE` — same shape, default `"https://app.cognee.ai/verify?token={token}"`

use async_trait::async_trait;
use cognee_database::AuthUser;
use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor, message::Mailbox,
    transport::smtp::client::Tls,
};
use std::sync::Arc;

use super::{Mailer, MailerError};

// ─── SmtpMailer ───────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct SmtpMailer {
    transport: Arc<AsyncSmtpTransport<Tokio1Executor>>,
    from: Mailbox,
    reset_link_template: String,
    verify_link_template: String,
}

impl SmtpMailer {
    /// Construct `SmtpMailer` from environment variables.
    ///
    /// Returns `Err(MailerError::Config)` when `SMTP_HOST` or `SMTP_FROM` are
    /// missing.  Other construction errors (invalid address, TLS setup) also
    /// surface as `MailerError::Config`.
    pub fn from_env() -> Result<Self, MailerError> {
        let host = std::env::var("SMTP_HOST")
            .map_err(|_| MailerError::Config("SMTP_HOST is required".into()))?;
        let from_str = std::env::var("SMTP_FROM")
            .map_err(|_| MailerError::Config("SMTP_FROM is required".into()))?;
        let from: Mailbox = from_str
            .parse()
            .map_err(|e| MailerError::Config(format!("invalid SMTP_FROM: {e}")))?;

        let port: u16 = std::env::var("SMTP_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(465);

        // Warn loudly on plaintext port 25.
        if port == 25 {
            tracing::warn!(
                "SMTP_PORT=25: using plaintext SMTP — credentials may be transmitted in clear text"
            );
        }

        let creds = match (std::env::var("SMTP_USER"), std::env::var("SMTP_PASS")) {
            (Ok(user), Ok(pass)) => Some(Credentials::new(user, pass)),
            _ => None,
        };

        let transport = build_transport(&host, port, creds)?;

        let reset_link_template = std::env::var("SMTP_RESET_LINK_TEMPLATE")
            .unwrap_or_else(|_| "https://app.cognee.ai/reset?token={token}".into());
        let verify_link_template = std::env::var("SMTP_VERIFY_LINK_TEMPLATE")
            .unwrap_or_else(|_| "https://app.cognee.ai/verify?token={token}".into());

        Ok(Self {
            transport: Arc::new(transport),
            from,
            reset_link_template,
            verify_link_template,
        })
    }

    fn format_link(&self, template: &str, token: &str) -> String {
        template.replace("{token}", token)
    }

    async fn send_message(
        &self,
        email: &str,
        subject: &str,
        body: String,
    ) -> Result<(), MailerError> {
        let to: Mailbox = email.parse().map_err(|e| {
            MailerError::Config(format!("invalid recipient address '{email}': {e}"))
        })?;

        let msg = Message::builder()
            .from(self.from.clone())
            .to(to)
            .subject(subject)
            .header(ContentType::TEXT_PLAIN)
            .body(body)
            .map_err(|e| MailerError::Transport(format!("failed to build message: {e}")))?;

        self.transport
            .send(msg)
            .await
            .map_err(|e| MailerError::Transport(e.to_string()))?;

        Ok(())
    }
}

fn build_transport(
    host: &str,
    port: u16,
    creds: Option<Credentials>,
) -> Result<AsyncSmtpTransport<Tokio1Executor>, MailerError> {
    let mut builder = match port {
        587 => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host),
        25 => Ok(
            AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host)
                .port(port)
                .tls(Tls::None),
        ),
        _ => AsyncSmtpTransport::<Tokio1Executor>::relay(host),
    }
    .map_err(|e| MailerError::Config(format!("failed to build SMTP transport: {e}")))?;

    if let Some(c) = creds {
        builder = builder.credentials(c);
    }

    // Only set port explicitly for non-standard configs.
    let transport = if port != 465 && port != 587 {
        builder.port(port).build()
    } else {
        builder.build()
    };

    Ok(transport)
}

#[async_trait]
impl Mailer for SmtpMailer {
    async fn send_register_welcome(&self, user: &AuthUser) -> Result<(), MailerError> {
        let body = format!(
            "Welcome to Cognee!\n\nYour account has been created for {}.\n\
             You can now start building knowledge graphs.\n\nHappy cognifying!",
            user.email
        );
        self.send_message(&user.email, "Welcome to Cognee", body)
            .await
    }

    async fn send_password_reset(&self, user: &AuthUser, token: &str) -> Result<(), MailerError> {
        let link = self.format_link(&self.reset_link_template, token);
        let body = format!(
            "Hello,\n\nYou requested a password reset for your Cognee account ({}).\n\
             Click the link below to reset your password:\n\n{}\n\n\
             If you did not request this, you can ignore this email.",
            user.email, link
        );
        self.send_message(&user.email, "Reset your Cognee password", body)
            .await
    }

    async fn send_email_verify(&self, user: &AuthUser, token: &str) -> Result<(), MailerError> {
        let link = self.format_link(&self.verify_link_template, token);
        let body = format!(
            "Hello,\n\nPlease verify your Cognee account ({}) by clicking the link below:\n\n{}\n\n\
             If you did not create this account, you can ignore this email.",
            user.email, link
        );
        self.send_message(&user.email, "Verify your Cognee email", body)
            .await
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn clear_smtp_env() {
        // SAFETY: tests run single-threaded (no concurrent env access)
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
    fn missing_smtp_host_returns_config_error() {
        clear_smtp_env();
        let err = SmtpMailer::from_env().unwrap_err();
        assert!(matches!(err, MailerError::Config(_)));
        assert!(err.to_string().contains("SMTP_HOST"));
    }

    #[test]
    #[serial]
    fn missing_smtp_from_returns_config_error() {
        clear_smtp_env();
        // SAFETY: tests run single-threaded (no concurrent env access)
        unsafe { std::env::set_var("SMTP_HOST", "smtp.example.com") };
        let err = SmtpMailer::from_env().unwrap_err();
        assert!(matches!(err, MailerError::Config(_)));
        assert!(err.to_string().contains("SMTP_FROM"));
        clear_smtp_env();
    }

    #[test]
    #[serial]
    fn valid_env_constructs_ok() {
        clear_smtp_env();
        // SAFETY: tests run single-threaded (no concurrent env access)
        unsafe {
            std::env::set_var("SMTP_HOST", "smtp.example.com");
            std::env::set_var("SMTP_FROM", "noreply@example.com");
        }
        // Construction builds the transport but does not connect.
        let result = SmtpMailer::from_env();
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        clear_smtp_env();
    }
}
