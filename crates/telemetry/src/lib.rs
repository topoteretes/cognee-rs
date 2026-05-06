//! Cognee product-analytics client (`send_telemetry`).
//!
//! This crate ports Python's `cognee.shared.utils.send_telemetry` to
//! Rust. It implements:
//!
//! - Three-layer identity (`anonymous_id`, `persistent_id`,
//!   `api_key_tracking_id`).
//! - Recursive URL-sanitization of caller-supplied properties.
//! - Fire-and-forget HTTP POST to the Cognee proxy
//!   (`https://test.prometh.ai`).
//! - Env-var opt-out (`TELEMETRY_DISABLED`, `ENV in {test,dev}`).
//!
//! The full public surface and noop fallback are wired up in
//! `docs/telemetry/02/06-public-api-and-noop.md`.

#![deny(rust_2018_idioms)]
#![warn(missing_docs)]

use thiserror::Error;
use uuid::Uuid;

// `serde_json::Value` is only available when the `telemetry` feature is on
// (it is an optional dep). Re-export an alias `PropertyValue` so the public
// signature compiles in both feature states. In the noop branch the alias
// resolves to `()` so callers passing `None` still typecheck; downstream
// crates that pass real JSON values must enable the `telemetry` feature.
//
// Implementation note: the public `send_telemetry` signature is finalised
// in task 02-06 — the scaffold here uses the simplest type that compiles
// in both branches.

#[cfg(feature = "telemetry")]
mod real;

#[cfg(not(feature = "telemetry"))]
mod noop;

#[cfg(feature = "telemetry")]
pub use serde_json::Value as PropertyValue;

#[cfg(not(feature = "telemetry"))]
/// Placeholder property type used when the `telemetry` feature is
/// disabled. Replaced by `serde_json::Value` once the feature is on.
pub type PropertyValue = ();

// Modules that are always compiled (their bodies vary by feature
// state). Each has a `#[cfg]` split internally — see the per-task
// sub-docs for details.
/// Identity-layer helpers (`anonymous_id`, `persistent_id`,
/// `api_key_tracking_id`). See [`docs/telemetry/02/03-id-derivation.md`]
/// for the design.
pub mod ids;
pub mod sanitize {
    //! URL-sanitisation. Implementation lands in
    //! `docs/telemetry/02/04-payload-and-sanitize.md`.
}
pub mod payload {
    //! `TelemetryPayload` serde struct. Implementation lands in
    //! `docs/telemetry/02/04-payload-and-sanitize.md`.
}
pub mod env {
    //! Env-var parsing and opt-out checks. Implementation lands in
    //! `docs/telemetry/02/05-client-dispatch-and-optout.md`.
}

/// Errors returned by the telemetry surface.
#[derive(Debug, Error)]
pub enum TelemetryError {
    /// Returned when the dispatcher is called from a non-async
    /// context AND the runtime fallback fails to bootstrap.
    #[error("could not acquire a tokio runtime to dispatch event")]
    NoRuntime,
}

/// Reference type for the `user_id` field — accepts a `Uuid`, a
/// string slice (e.g. `"sdk"` for SDK-internal callers), or `None`
/// to skip the field entirely.
#[derive(Debug)]
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
/// Mirrors Python `cognee.shared.utils.send_telemetry`. Returns
/// immediately; the HTTP POST is dispatched on a detached tokio task
/// with a 5-second (configurable) total timeout. Errors are swallowed
/// at debug level. See task 02-05 for the dispatch semantics and
/// runtime-fallback behaviour.
pub fn send_telemetry<'a>(
    event_name: &str,
    user_id: impl Into<UserIdRef<'a>>,
    additional_properties: Option<PropertyValue>,
) {
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
}
