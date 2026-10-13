//! Product-analytics C entrypoints.
//!
//! Analytics are ON by default (Python-SDK parity, opt-out) and the switch is
//! **process-wide**: one [`cognee_telemetry::TelemetryConfig`] governs every
//! event the process sends, whichever `CgSdk` handle emits it.
//!
//! * `cognee_set_telemetry_enabled(int)` — the programmatic switch; wins over
//!   the environment, and disabling also drops events queued but not yet
//!   sent.
//! * `cognee_is_telemetry_enabled()` — whether an event sent now would be
//!   posted.
//! * `cognee_init_telemetry()` — arms the binding (`cg_init` also does) and
//!   reports the same state.
//!
//! The environment is the default (`TELEMETRY_DISABLED`, `ENV` in {test,
//! dev}), read when an event is sent. `COGNEE_HOST_SDK` lets an embedding
//! host SDK suppress this binding's emissions (decision 10).

use std::ffi::c_int;

/// Arm cognee product-analytics emission for this process and report whether
/// analytics are enabled.
///
/// Returns:
/// * `0` — enabled (events will be sent).
/// * `1` — disabled (switched off, or an opt-out env var / host SDK
///   suppresses them).
///
/// Safe to call multiple times: arming never changes the switch, so repeated
/// calls agree until it is flipped with `cognee_set_telemetry_enabled`.
#[unsafe(no_mangle)]
pub extern "C" fn cognee_init_telemetry() -> c_int {
    cognee_telemetry::env::arm_binding_emission();
    if cognee_telemetry::is_enabled() { 0 } else { 1 }
}

/// Turn product analytics on (`enabled != 0`) or off (`0`) for the whole
/// process. Wins over `TELEMETRY_DISABLED` / `ENV` in the environment.
/// Always returns 0.
#[unsafe(no_mangle)]
pub extern "C" fn cognee_set_telemetry_enabled(enabled: c_int) -> c_int {
    cognee_telemetry::set_enabled(enabled != 0);
    0
}

/// `1` when product analytics are currently enabled for this process, `0`
/// otherwise.
#[unsafe(no_mangle)]
pub extern "C" fn cognee_is_telemetry_enabled() -> c_int {
    c_int::from(cognee_telemetry::is_enabled())
}
