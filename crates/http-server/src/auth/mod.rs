//! Slim OSS auth module. The full closed `auth/` subtree (JWT, cookie,
//! API key, register/reset/verify, password hashing, mailer,
//! superuser-only extractor, AuthContext, etc.) lives in the closed
//! `cognee-http-cloud` crate alongside the auth router family. OSS
//! keeps only the in-extractor types and the default-user fallback.

mod extractor;

pub use extractor::{
    AuthMethod, AuthenticatedUser, OptionalAuthenticatedUser, default_user_from_state,
};
