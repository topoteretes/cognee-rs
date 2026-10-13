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

#[test]
fn process_switch_reports_disabled_without_feature() {
    // The switch exists in both feature states; without the feature nothing
    // can ever be sent, so it reports `false` whatever it is set to.
    cognee_telemetry::set_enabled(true);
    assert!(!cognee_telemetry::is_enabled());
    cognee_telemetry::set_llm_api_key(Some("sk-ignored"));
    assert_eq!(cognee_telemetry::ids::get_api_key_tracking_id(), "");
}
