//! Noop (`feature = "telemetry"` off) implementation of
//! `send_telemetry`. Compiled when the `telemetry` cargo feature is
//! disabled.
//!
//! The entry point becomes a no-op that emits a single
//! `tracing::debug!` line. The caller in `lib.rs` discards the
//! arguments before invoking us, so this function takes none —
//! avoiding any reference to `serde_json::Value` (which is gated on
//! the `telemetry` feature). Identity helpers in [`crate::ids`]
//! return empty strings.

/// No-op twin of the real flush, so the non-`telemetry` build compiles
/// the same call sites. Nothing was ever dispatched, so the queue is
/// always drained.
pub(crate) async fn flush_impl(_timeout: std::time::Duration) -> bool {
    true
}

pub(crate) fn send_telemetry_impl() {
    tracing::debug!(
        target: "cognee.telemetry",
        "send_telemetry called but telemetry feature disabled at compile time"
    );
}
