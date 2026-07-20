//! Minimal SDK error type for the bindings facade.
//!
//! Neon-specific helpers (`throw_sdk_error`, `throw_config_error`) stay in the
//! `cognee-ts-neon` crate because they require `neon::prelude::*`. This module
//! contains only the portable error enum that both JS and C bindings share.

use thiserror::Error;

use cognee::ComponentError;

/// Errors surfaced by the SDK handle/facade.
///
/// Every variant maps 1:1 to a stable machine-readable `code()` string (used by
/// the JS binding as `e.code`/`e.kind` and by the C binding as a
/// `CgErrorCode` SDK error code).
#[derive(Debug, Error)]
pub enum SdkError {
    /// An engine (storage / database / graph / vector / embedding / llm) failed
    /// to initialise through the `ComponentManager`.
    #[error("component error: {0}")]
    Component(#[from] ComponentError),

    /// A derived service (thread pool, session store, ontology resolver, …)
    /// failed to construct.
    #[error("service build error: {0}")]
    ServiceBuild(String),

    /// The relational user bootstrap (`get_or_create_default_user`) failed.
    #[error("user bootstrap error: {0}")]
    UserBootstrap(String),

    /// A runtime / infrastructure failure (e.g. building the tokio runtime).
    #[error("runtime error: {0}")]
    Runtime(String),

    /// Invalid input from the binding boundary (bad shape / missing field /
    /// parse failure). Maps to a developer error, not an infrastructure failure.
    #[error("validation error: {0}")]
    Validation(String),

    /// A requested input variant or feature is recognised but not yet wired
    /// end-to-end (e.g. `s3` / recursive `dataItem` inputs).
    #[error("unsupported: {0}")]
    Unsupported(String),

    /// The binding function was called but the required Cargo feature was not
    /// compiled into this build. Use a build that includes the relevant feature
    /// (e.g. `visualization`, `cloud`).
    ///
    /// This variant is only constructed in `#[cfg(not(feature = "..."))]` bodies
    /// that are compiled out when the default features are active, so the
    /// dead-code lint is suppressed.
    #[allow(dead_code)]
    #[error("feature not built: {0}")]
    FeatureNotBuilt(String),
}

impl SdkError {
    /// Stable machine-readable code.
    ///
    /// Mirrors the convention used by the legacy Neon engine errors and the C
    /// API error code enum. Values are stable across versions (append-only per
    /// decision D5).
    pub fn code(&self) -> &'static str {
        match self {
            SdkError::Component(_) => "COMPONENT_ERROR",
            SdkError::ServiceBuild(_) => "SERVICE_BUILD_ERROR",
            SdkError::UserBootstrap(_) => "USER_BOOTSTRAP_ERROR",
            SdkError::Runtime(_) => "RUNTIME_ERROR",
            SdkError::Validation(_) => "VALIDATION_ERROR",
            SdkError::Unsupported(_) => "UNSUPPORTED",
            SdkError::FeatureNotBuilt(_) => "FEATURE_NOT_BUILT",
        }
    }
}
