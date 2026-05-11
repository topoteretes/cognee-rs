//! `cognee_init_telemetry()` C entrypoint (gap 07 task 06).
//!
//! Argument-less, idempotent installer that arms cognee
//! product-analytics emission for this process subject to the
//! per-binding policy from gap 07 decision 11.
//!
//! Policy (C API is explicit-only — calling the function expresses
//! intent to opt in):
//!
//! * `armed` unless `TELEMETRY_DISABLED` is set, `ENV` is
//!   `"test"`/`"dev"`, or `COGNEE_HOST_SDK` is set to any non-empty
//!   value. The C binding has no upstream SDK convention, so it stays
//!   explicit: callers opt in by invoking `cognee_init_telemetry`;
//!   the function then defers to the standard env opt-outs.
//!
//! Idempotent via `OnceLock<Mutex<Option<bool>>>` (decision 12). When
//! the policy arms emission this calls
//! [`cognee_telemetry::env::arm_binding_emission`] so the
//! `COGNEE_HOST_SDK` sentinel inside
//! [`cognee_telemetry::env::is_disabled`] applies to any future
//! `send_telemetry` calls originating from a binding path (decision
//! 10).

use std::ffi::c_int;
use std::sync::{Mutex, OnceLock};

static ARMED: OnceLock<Mutex<Option<bool>>> = OnceLock::new();

/// Arm cognee product-analytics emission for this process.
///
/// Default policy (gap 07 decision 11): C bindings are explicit-only —
/// calling this function arms emission unless the same opt-outs
/// recognized by [`cognee_telemetry::env::is_disabled`] are set
/// (`TELEMETRY_DISABLED`, `ENV in {test, dev}`, or `COGNEE_HOST_SDK`
/// non-empty).
///
/// Returns:
/// * `0` — armed (analytics will fire on subsequent `send_telemetry`
///   calls — subject to runtime opt-out re-evaluation).
/// * `1` — not armed (the per-binding policy suppressed emission).
/// * `2` — internal lock poisoning (should not happen).
///
/// Safe to call multiple times. The first call latches the decision;
/// repeated calls return that same decision without re-evaluating the
/// environment.
#[unsafe(no_mangle)]
pub extern "C" fn cognee_init_telemetry() -> c_int {
    let slot = ARMED.get_or_init(|| Mutex::new(None));
    let mut lock = match slot.lock() {
        Ok(l) => l,
        // lock poison is unrecoverable
        Err(_) => return 2,
    };
    if let Some(armed) = *lock {
        return if armed { 0 } else { 1 };
    }

    let telemetry_disabled = std::env::var("TELEMETRY_DISABLED")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let env_test_or_dev = std::env::var("ENV")
        .map(|v| v == "test" || v == "dev")
        .unwrap_or(false);
    let host_sdk = std::env::var("COGNEE_HOST_SDK")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let armed = !(telemetry_disabled || env_test_or_dev || host_sdk);

    if armed {
        cognee_telemetry::env::arm_binding_emission();
    }
    *lock = Some(armed);
    if armed { 0 } else { 1 }
}
