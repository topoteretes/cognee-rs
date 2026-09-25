//! Injection seam for closed-side authentication.
//!
//! The OSS http-server keeps no JWT/cookie/API-key state — those moved
//! to the closed `cognee-http-cloud` crate alongside the auth router
//! family. To let closed embedders plug their auth chain back into the
//! OSS `AuthenticatedUser` extractor, OSS stores an optional
//! `Arc<dyn AuthResolver>` on `AppState`.
//!
//! - `AuthResolver::resolve` is the full chain (Bearer → cookie → API key
//!   → optional `ExtraAuthValidator` hook). Returns `Some(user)` if any
//!   method succeeds, else `None` so the extractor can fall through to
//!   the default-user path (or 401 when `require_authentication=true`).
//! - `ExtraAuthValidator` is the narrower Auth0/OIDC-only hook the plan
//!   names explicitly. A closed embedder that only wants to add an Auth0
//!   hook (and not replace the whole chain) installs only this — the OSS
//!   `RouterBuilder::with_extra_validator(...)` wraps it in a default
//!   resolver that calls just the validator.

use std::sync::Arc;

use async_trait::async_trait;
use axum::http::HeaderMap;
use axum::http::request::Parts;

use crate::auth::AuthenticatedUser;

#[async_trait]
pub trait AuthResolver: Send + Sync + 'static {
    /// Attempt to authenticate the request. Return `None` to fall
    /// through to OSS default-user behaviour (when
    /// `require_authentication` is false) or to a 401 (when true).
    async fn resolve(&self, parts: &mut Parts) -> Option<AuthenticatedUser>;
}

#[async_trait]
pub trait ExtraAuthValidator: Send + Sync + 'static {
    /// Validate Auth0 / OIDC tokens (or any external auth source) given
    /// the request headers. Closed embedders inject this via
    /// `RouterBuilder::with_extra_validator(...)`.
    async fn validate(&self, headers: &HeaderMap) -> Option<AuthenticatedUser>;
}

/// Wrap an `ExtraAuthValidator` into an `AuthResolver` that performs only
/// the validator step.
pub fn resolver_from_validator(v: Arc<dyn ExtraAuthValidator>) -> Arc<dyn AuthResolver> {
    Arc::new(ExtraValidatorOnly { v })
}

struct ExtraValidatorOnly {
    v: Arc<dyn ExtraAuthValidator>,
}

#[async_trait]
impl AuthResolver for ExtraValidatorOnly {
    async fn resolve(&self, parts: &mut Parts) -> Option<AuthenticatedUser> {
        self.v.validate(&parts.headers).await
    }
}
