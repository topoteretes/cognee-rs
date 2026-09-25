//! Per-endpoint product-analytics emission helper.
//!
//! Mirrors the `send_telemetry("… API Endpoint Invoked", user.id, {…})`
//! calls in the Python FastAPI routers
//! (`cognee/api/v1/*/routers/get_*_router.py`). Each `/api/v1/*` handler
//! calls [`emit`] near its top so the wire payload matches Python.
//!
//! The whole surface is gated by the `telemetry` cargo feature: with it
//! off (e.g. a `--no-default-features` build), [`emit`] compiles to a
//! noop and `cognee-telemetry` is not even a dependency. Callers pass a
//! fully-built `serde_json::Value::Object` of `additional_properties`
//! and never need a `#[cfg(...)]` of their own.
//!
//! `cognee_version` is intentionally **omitted** from the per-endpoint
//! properties: the base `send_telemetry` payload already carries it at
//! `properties.cognee_version`, so the on-the-wire field is present
//! exactly once and matches Python (which spreads it into
//! `additional_properties`).

use serde_json::Value;
use uuid::Uuid;

/// Emit a `"… API Endpoint Invoked"` analytics event for `user_id` with
/// the given `additional_properties` object.
///
/// No-op when the `telemetry` feature is disabled. Fire-and-forget —
/// returns immediately; transport errors are swallowed at debug level on
/// the `cognee.telemetry` tracing target.
#[cfg(feature = "telemetry")]
#[inline]
pub fn emit(event_name: &str, user_id: Uuid, additional_properties: Value) {
    cognee_telemetry::send_telemetry(event_name, user_id, Some(additional_properties));
}

/// No-op stand-in when the `telemetry` feature is disabled. Keeps router
/// handlers free of `#[cfg]` clutter.
#[cfg(not(feature = "telemetry"))]
#[inline]
pub fn emit(_event_name: &str, _user_id: Uuid, _additional_properties: Value) {}
