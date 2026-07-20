//! Cognee product-analytics client (`send_telemetry`).
//!
//! Mirrors Python's `cognee.shared.utils.send_telemetry`. Fires a
//! single fire-and-forget HTTP POST to `https://test.prometh.ai` for
//! every public API call so the cognee maintainers have an aggregate
//! view of how the SDK is exercised.
//!
//! Enabled by default (locked decision 1 — Python parity). The runtime
//! check happens **before** any identity derivation or HTTP code path,
//! so disabling at runtime costs zero.
//!
//! See [`docs/observability/send_telemetry.md`][user-doc] for the full
//! operator-facing reference (payload schema, salt rotation, privacy
//! notes, troubleshooting).
//!
//! [user-doc]: https://github.com/topoteretes/cognee-rs/blob/main/docs/observability/send_telemetry.md
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
//! At compile time: build `cognee` (or any consumer) with
//! `--no-default-features`. [`send_telemetry`] and
//! [`try_send_telemetry`] still exist in the public surface but are
//! compiled to noop bodies — no `reqwest`, no `tokio` runtime
//! fallback, no PBKDF2 cost.
//!
//! # Environment variables
//!
//! | Var | Default | Effect |
//! |---|---|---|
//! | `TELEMETRY_DISABLED` | unset | Any non-empty value disables. Read on every call. |
//! | `ENV` | unset | If `test` or `dev`, disables. Read on every call. |
//! | `LLM_API_KEY` | unset | Source of `api_key_tracking_id` (locked decision 11 — read at every event-emission, never cached). |
//! | `TRACKING_ID` | unset | Override `anonymous_id`. |
//! | `TELEMETRY_API_KEY_TRACKING_SALT` | `cognee.telemetry.api-key-tracking.v1` | PBKDF2 salt override (locked decision 12). |
//! | `TELEMETRY_REQUEST_TIMEOUT` | `5` | HTTP timeout in seconds. Clamped to `[1, 60]`. Read once per process. |
//!
//! # Logging
//!
//! All diagnostics use the `cognee.telemetry` tracing target. Enable
//! with `RUST_LOG=cognee.telemetry=debug`.

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
///
/// Callers should pass a `Value::Object` — non-object values are
/// silently dropped at sanitization time with a `cognee.telemetry`
/// debug log. Reserved keys (`time`, `user_id`, `anonymous_id`,
/// `persistent_id`, `api_key_tracking_id`, `api_key_hash`,
/// `sdk_runtime`, `cognee_version`) MUST NOT appear in the object.
#[cfg(feature = "telemetry")]
pub use serde_json::Value as PropertyValue;

/// Placeholder property type used when the `telemetry` feature is
/// disabled. Replaced by `serde_json::Value` once the feature is on.
///
/// Public-API callers should hold values as `Option<PropertyValue>`
/// so the same code compiles in both feature states.
#[cfg(not(feature = "telemetry"))]
pub type PropertyValue = ();

/// Errors returned by [`try_send_telemetry`].
///
/// In practice, [`try_send_telemetry`] always returns `Ok(())` today —
/// transport, serialization and proxy errors are swallowed at debug
/// level (`cognee.telemetry` target) to preserve fire-and-forget
/// semantics. The error variant exists so future failure modes
/// (e.g. backpressure rejection) can be surfaced without a breaking
/// change.
#[derive(Debug, Error)]
pub enum TelemetryError {
    /// The dispatcher could not acquire a tokio runtime and the
    /// fallback runtime build failed. Practically unreachable.
    #[error("could not bootstrap a tokio runtime to dispatch event")]
    NoRuntime,
}

/// Reference type for the `user_id` field. Accepts a `Uuid`, a
/// `&str`, or `Option<Uuid>` via the [`From`] impls below; callers
/// rarely construct this directly.
///
/// The chosen variant is serialized to a string in the wire payload:
/// [`UserIdRef::Uuid`] becomes the canonical hyphenated UUID,
/// [`UserIdRef::Symbolic`] passes through as-is, and
/// [`UserIdRef::None`] becomes the empty string.
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

/// Format a `tenant_id` for the telemetry wire payload, mirroring
/// Python `str(user.tenant_id) if user.tenant_id else "Single User Tenant"`.
///
/// Lifecycle emitters (pipeline, task, search) thread an
/// `Option<Uuid>` through the runtime context and call this helper at
/// the emission site so the on-the-wire string is byte-for-byte
/// identical to the Python implementation when no tenant has been
/// configured.
#[inline]
pub fn tenant_id_for_telemetry(tenant_id: Option<Uuid>) -> String {
    match tenant_id {
        Some(id) => id.to_string(),
        None => "Single User Tenant".to_string(),
    }
}

/// Returns the cognee crate version string for use in analytics
/// payloads. Matches Python's `cognee.__version__`.
///
/// Equivalent to `env!("CARGO_PKG_VERSION")` evaluated inside the
/// `cognee-telemetry` crate. The workspace pins all cognee crates to
/// the same version via `version.workspace = true`, so the value is
/// the same as `cognee`'s reported version. Lifecycle emitters in
/// `cognee-core` and elsewhere should call this accessor instead of
/// inlining `env!("CARGO_PKG_VERSION")`, which would otherwise expand
/// to the calling crate's version.
///
/// Always available — does not depend on the `telemetry` feature
/// flag.
#[inline]
pub fn cognee_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Fire-and-forget product-analytics event.
///
/// Returns immediately; the HTTP POST is dispatched on a detached
/// tokio task with a 5-second (configurable via
/// `TELEMETRY_REQUEST_TIMEOUT`) total timeout. Errors are swallowed
/// at debug level on the `cognee.telemetry` tracing target. When
/// called outside a tokio runtime, falls back to a one-shot
/// single-thread runtime (locked decision 5) and logs a `warn`-level
/// notice — behaviour is correct (the event still fires) but
/// indicates a perf-improvement opportunity.
///
/// No-op when:
/// - the `telemetry` cargo feature is disabled at compile time
///   (function still exists but compiles to an empty body, so
///   consuming code stays binary-compatible across feature flips),
/// - `TELEMETRY_DISABLED` is set to a non-empty value at runtime,
/// - `ENV` is `"test"` or `"dev"`.
///
/// # Environment variables
///
/// | Var | Default | Effect |
/// |---|---|---|
/// | `TELEMETRY_DISABLED` | unset | Any non-empty value disables. |
/// | `ENV` | unset | If `test` or `dev`, disables. |
/// | `LLM_API_KEY` | unset | Hashed into `api_key_tracking_id` (read at every call). |
/// | `TRACKING_ID` | unset | Override `anonymous_id`. |
/// | `TELEMETRY_API_KEY_TRACKING_SALT` | (well-known default) | Override PBKDF2 salt. |
/// | `TELEMETRY_REQUEST_TIMEOUT` | `5` | Total HTTP timeout (seconds), clamped `[1, 60]`. |
///
/// See the [user-facing
/// guide](https://github.com/topoteretes/cognee-rs/blob/main/docs/observability/send_telemetry.md)
/// for the full reference.
pub fn send_telemetry<'a>(
    event_name: &str,
    user_id: impl Into<UserIdRef<'a>>,
    additional_properties: Option<PropertyValue>,
) {
    let _ = try_send_telemetry(event_name, user_id, additional_properties);
}

/// Same as [`send_telemetry`] but returns
/// `Result<(), TelemetryError>` for callers that want to know whether
/// dispatch was attempted.
///
/// The `Ok(())` return does **not** mean the proxy received the
/// payload — it means the dispatch was scheduled. Transport failures
/// are still swallowed at debug level on the `cognee.telemetry`
/// target (mirrors Python's fire-and-forget semantics).
///
/// In current builds this function always returns `Ok(())`. The
/// [`TelemetryError`] variant exists so future failure modes
/// (e.g. backpressure rejection) can be surfaced without a breaking
/// change to the signature. Honours the same opt-out and runtime
/// fallback semantics as [`send_telemetry`]; see that function's
/// rustdoc for env-var details.
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
