//! Cognee product-analytics client (`send_telemetry`).
//!
//! Mirrors Python's `cognee.shared.utils.send_telemetry`.
//!
//! # Quick start
//!
//! ```no_run
//! # #[cfg(feature = "telemetry")] {
//! use cognee_telemetry::send_telemetry;
//! use serde_json::json;
//!
//! send_telemetry(
//!     "cognee.forget",
//!     "user-id-string",
//!     Some(json!({ "endpoint": "POST /api/v1/forget" })),
//! );
//! # }
//! ```
//!
//! # Opt-out
//!
//! At runtime: `TELEMETRY_DISABLED=1` (any non-empty value) or
//! `ENV=test` / `ENV=dev`.
//!
//! At compile time: build `cognee-lib` (or any consumer) with
//! `--no-default-features`. The function still exists but becomes a
//! noop.

#![deny(rust_2018_idioms)]
#![warn(missing_docs)]

use thiserror::Error;
use uuid::Uuid;

#[cfg(feature = "telemetry")]
mod client;
#[cfg(not(feature = "telemetry"))]
mod noop;
#[cfg(feature = "telemetry")]
mod real;

pub mod env;
pub mod ids;
pub mod payload;
pub mod sanitize;

/// Property value type for `additional_properties`. Resolves to
/// `serde_json::Value` when the `telemetry` feature is on, or `()`
/// when it is off. Keep all public signatures referring to this alias
/// rather than `serde_json::Value` directly so the surface compiles
/// under `--no-default-features`.
#[cfg(feature = "telemetry")]
pub use serde_json::Value as PropertyValue;

/// Placeholder property type used when the `telemetry` feature is
/// disabled. Replaced by `serde_json::Value` once the feature is on.
#[cfg(not(feature = "telemetry"))]
pub type PropertyValue = ();

/// Errors returned by [`try_send_telemetry`].
#[derive(Debug, Error)]
pub enum TelemetryError {
    /// The dispatcher could not acquire a tokio runtime and the
    /// fallback runtime build failed. Practically unreachable.
    #[error("could not bootstrap a tokio runtime to dispatch event")]
    NoRuntime,
}

/// Reference type for the `user_id` field. Accepts a `Uuid`, a
/// `&str`, or `Option<Uuid>`.
#[derive(Debug, Clone)]
pub enum UserIdRef<'a> {
    /// A real cognee `User.id`.
    Uuid(Uuid),
    /// A symbolic identifier (e.g. `"sdk"`, `"anonymous"`).
    Symbolic(&'a str),
    /// No user attached.
    None,
}

impl From<Uuid> for UserIdRef<'_> {
    fn from(u: Uuid) -> Self {
        UserIdRef::Uuid(u)
    }
}
impl<'a> From<&'a str> for UserIdRef<'a> {
    fn from(s: &'a str) -> Self {
        UserIdRef::Symbolic(s)
    }
}
impl<'a> From<&'a String> for UserIdRef<'a> {
    fn from(s: &'a String) -> Self {
        UserIdRef::Symbolic(s.as_str())
    }
}
impl From<Option<Uuid>> for UserIdRef<'_> {
    fn from(o: Option<Uuid>) -> Self {
        match o {
            Some(u) => UserIdRef::Uuid(u),
            None => UserIdRef::None,
        }
    }
}

/// Fire-and-forget product-analytics event.
///
/// Returns immediately; the HTTP POST is dispatched on a detached
/// tokio task with a 5-second (configurable) total timeout. Errors
/// are swallowed at debug level. When called outside a tokio runtime,
/// falls back to a one-shot single-thread runtime (decision 5 — see
/// [`docs/telemetry/02-send-telemetry-analytics.md`]).
///
/// No-op when:
/// - the `telemetry` cargo feature is disabled at compile time,
/// - `TELEMETRY_DISABLED` is set to a non-empty value at runtime,
/// - `ENV` is `"test"` or `"dev"`.
pub fn send_telemetry<'a>(
    event_name: &str,
    user_id: impl Into<UserIdRef<'a>>,
    additional_properties: Option<PropertyValue>,
) {
    let _ = try_send_telemetry(event_name, user_id, additional_properties);
}

/// Same as [`send_telemetry`] but returns `Result<(), TelemetryError>`
/// for callers that want to know whether dispatch was attempted.
///
/// The `Ok(())` return does **not** mean the proxy received the
/// payload — it means the dispatch was scheduled. Transport failures
/// are still swallowed at debug level (mirrors Python's
/// fire-and-forget semantics).
pub fn try_send_telemetry<'a>(
    event_name: &str,
    user_id: impl Into<UserIdRef<'a>>,
    additional_properties: Option<PropertyValue>,
) -> Result<(), TelemetryError> {
    let user_id = user_id.into();
    #[cfg(feature = "telemetry")]
    {
        real::send_telemetry_impl(event_name, user_id, additional_properties);
    }
    #[cfg(not(feature = "telemetry"))]
    {
        // Drop borrowed/owned args explicitly so unused-variable lints
        // don't fire when the telemetry feature is off.
        let _ = (event_name, user_id, additional_properties);
        noop::send_telemetry_impl();
    }
    Ok(())
}
