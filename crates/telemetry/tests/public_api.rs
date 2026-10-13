//! Smoke tests that the public surface compiles and is callable in
//! both feature states. The real dispatch path is exercised by
//! `tests/dispatch_with_mockito.rs` in task 02-09.

#![cfg(feature = "telemetry")]

use cognee_telemetry::send_telemetry;
use serde_json::json;
use serial_test::serial;

#[test]
#[serial]
fn callable_with_uuid_user_id() {
    let id = uuid::Uuid::new_v4();
    // No assertion: this is a compile-time and "doesn't panic" check.
    // Switch analytics off so we don't try to hit the network.
    cognee_telemetry::set_enabled(false);
    send_telemetry("test.event", id, Some(json!({"k": "v"})));
}

#[test]
#[serial]
fn callable_with_str_user_id() {
    cognee_telemetry::set_enabled(false);
    send_telemetry("test.event", "anonymous", None);
}

#[test]
#[serial]
fn callable_with_optional_uuid_user_id() {
    cognee_telemetry::set_enabled(false);
    let id: Option<uuid::Uuid> = None;
    send_telemetry("test.event", id, None);
}

#[test]
#[serial]
fn process_switch_round_trips() {
    cognee_telemetry::set_enabled(false);
    assert!(!cognee_telemetry::is_enabled());
    cognee_telemetry::set_enabled(true);
    assert!(cognee_telemetry::is_enabled());
    cognee_telemetry::reset_to_env();
}
