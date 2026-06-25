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
    // Set TELEMETRY_DISABLED so we don't try to hit the network.
    // SAFETY: `#[serial]` orders this against every other env-mutating
    //   test in the crate; nothing else reads/writes
    //   TELEMETRY_DISABLED while this body runs.
    unsafe {
        std::env::set_var("TELEMETRY_DISABLED", "1");
    }
    send_telemetry("test.event", id, Some(json!({"k": "v"})));
}

#[test]
#[serial]
fn callable_with_str_user_id() {
    // SAFETY: see callable_with_uuid_user_id.
    unsafe {
        std::env::set_var("TELEMETRY_DISABLED", "1");
    }
    send_telemetry("test.event", "anonymous", None);
}

#[test]
#[serial]
fn callable_with_optional_uuid_user_id() {
    // SAFETY: see callable_with_uuid_user_id.
    unsafe {
        std::env::set_var("TELEMETRY_DISABLED", "1");
    }
    let id: Option<uuid::Uuid> = None;
    send_telemetry("test.event", id, None);
}
