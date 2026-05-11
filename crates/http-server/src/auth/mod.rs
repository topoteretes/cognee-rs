//! Authentication subsystem for the cognee HTTP server.
//!
//! Provides JWT encoding/decoding, cookie helpers, API-key generation and
//! lookup, password hashing (argon2id new / bcrypt legacy), the `AuthContext`
//! configuration struct, `AuthenticatedUser` / `RequireSuperuser` extractors,
//! the `Mailer` trait, and thin service modules for each auth endpoint group.
//!
//! The `context` module exposes `AuthContext` which is wired into `AppState::auth`
//! as `Option<Arc<AuthContext>>` by the server startup path.

pub mod api_key;
pub mod api_keys_service;
pub mod context;
pub mod cookie;
pub mod extractor;
pub mod jwt;
pub mod login;
pub mod mailer;
pub mod password;
pub mod register;
pub mod reset;
pub mod superuser;
pub mod users_service;
pub mod verify;

pub use context::{AuthContext, ExtraAuthValidator};
pub use extractor::{AuthMethod, AuthenticatedUser, OptionalAuthenticatedUser, RequireSuperuser};
pub use mailer::{
    ConsoleMailer, LoggingMailer, MailEvent, MailEventKind, Mailer, MailerError, SmtpMailer,
    build_default as build_default_mailer,
};
pub use superuser::SuperuserOnly;
