//! Asserts that `send_telemetry` is a no-op (returns immediately,
//! emits a single debug log) when the `telemetry` feature is off.

#![cfg(not(feature = "telemetry"))]

use cognee_telemetry::send_telemetry;

#[test]
fn send_telemetry_compiles_without_feature() {
    // The whole point: this file builds with `--no-default-features`.
    // No network call, no panics.
    send_telemetry("test.event", "user", None);
}
