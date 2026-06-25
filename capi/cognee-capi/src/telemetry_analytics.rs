//! `cognee_init_telemetry()` C entrypoint (gap 07 task 06).
//!
//! Argument-less, idempotent reporter of cognee product-analytics
//! emission state for this process (Python-SDK parity — analytics ON by
//! default).
//!
//! Note: `cg_init` / `cg_init_with_threads` already auto-arm analytics
//! (see `runtime.rs::arm_telemetry_analytics`), so emission is ON by
//! default without calling this function. `cognee_init_telemetry`
//! re-affirms the arm and reports the effective state:
//!
//! * `armed` unless `TELEMETRY_DISABLED` is set, `ENV` is
//!   `"test"`/`"dev"`, or `COGNEE_HOST_SDK` is set to any non-empty
//!   value.
//!
//! Idempotent via `OnceLock<Mutex<Option<bool>>>` (decision 12). It
//! calls [`cognee_telemetry::env::arm_binding_emission`] so the
//! `COGNEE_HOST_SDK` sentinel inside
//! [`cognee_telemetry::env::is_disabled`] applies to any future
//! `send_telemetry` calls originating from a binding path (decision
//! 10).

use std::ffi::c_int;
use std::sync::{Mutex, OnceLock};

static ARMED: OnceLock<Mutex<Option<bool>>> = OnceLock::new();

/// Arm cognee product-analytics emission for this process.
///
/// Default policy (Python-SDK parity): analytics are ON by default
/// (also auto-armed by `cg_init`). This call re-affirms the arm and
/// reports the effective state, which is armed unless the opt-outs
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

    // Arm unconditionally so is_disabled()'s COGNEE_HOST_SDK clause is
    // authoritative for binding-hosted emissions (decision 10). Arming
    // only ever *adds* suppression — it never enables emission — so it is
    // safe even when telemetry is otherwise disabled. Actual emission is
    // re-evaluated per event via is_disabled().
    cognee_telemetry::env::arm_binding_emission();
    let armed = !cognee_telemetry::env::is_disabled();
    *lock = Some(armed);
    if armed { 0 } else { 1 }
}
