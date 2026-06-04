//! Minimal Phase-1 error type for the SDK facade.
//!
//! The full typed-error marshalling story lives in Phase 8. For now we only
//! need a single error enum the facade (`services.rs`/`sdk.rs`) can return, plus
//! a helper to turn it into a thrown JS `Error` with a stable `code` field.

use neon::prelude::*;
use thiserror::Error;

use cognee_lib::ComponentError;

/// Errors surfaced by the SDK handle/facade during Phase 1.
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

    /// Invalid input from the JS boundary (bad shape / missing field / parse
    /// failure). Maps to a developer error, not an infrastructure failure.
    #[error("validation error: {0}")]
    Validation(String),

    /// A requested input variant or feature is recognised but not yet wired
    /// end-to-end (e.g. `s3` / recursive `dataItem` inputs).
    #[error("unsupported: {0}")]
    Unsupported(String),

    /// The native function was called but the required Cargo feature was not
    /// compiled into this build of `cognee-neon`. Use a build that includes
    /// the relevant feature (e.g. `visualization`, `cloud`).
    ///
    /// This variant is only constructed in `#[cfg(not(feature = "..."))]`
    /// bodies that are compiled out when the default features are active,
    /// so the dead-code lint is suppressed.
    #[allow(dead_code)]
    #[error("feature not built: {0}")]
    FeatureNotBuilt(String),
}

impl SdkError {
    /// Stable machine-readable code, mirroring the convention used by
    /// `error.rs` for the legacy engine errors.
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

/// Throw a JS `Error` carrying the message, a `code` property, and a `kind`
/// property from an [`SdkError`].
///
/// Both `code` and `kind` carry the same string value. `kind` is the stable
/// API identifier; `code` is kept as a backwards-compatible alias so existing
/// call-sites that check `e.code` continue to work.
pub fn throw_sdk_error<'cx, T>(cx: &mut impl Context<'cx>, err: SdkError) -> NeonResult<T> {
    let code = err.code();
    let msg = err.to_string();
    let js_err = cx.error(msg)?;
    let obj = js_err.downcast_or_throw::<JsObject, _>(cx)?;
    let code_val = cx.string(code);
    let kind_val = cx.string(code);
    obj.set(cx, "code", code_val)?;
    obj.set(cx, "kind", kind_val)?;
    cx.throw(js_err)
}
