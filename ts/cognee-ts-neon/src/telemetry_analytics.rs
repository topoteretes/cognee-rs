//! `setupTelemetryAnalytics()` Neon entrypoint (gap 07 task 06).
//!
//! Argument-less, idempotent installer that arms cognee
//! product-analytics emission for this Node.js process subject to the
//! per-binding policy from gap 07 decision 11.
//!
//! Local sovereign policy (Neon remains the canonical JS sender):
//!
//! * `armed` only when `COGNEE_PRODUCT_TELEMETRY_ENABLED` is a
//!   recognized explicit opt-in and no suppression variable applies.
//!
//! Idempotent via `OnceLock<Mutex<Option<bool>>>` (decision 12). When
//! the policy arms emission this calls
//! [`cognee_telemetry::env::arm_binding_emission`] so the
//! `COGNEE_HOST_SDK` sentinel inside
//! [`cognee_telemetry::env::is_disabled`] applies to any future
//! `send_telemetry` calls originating from a binding path (decision
//! 10).

use std::sync::{Mutex, OnceLock};

use neon::prelude::*;

static ARMED: OnceLock<Mutex<Option<bool>>> = OnceLock::new();

/// Arm cognee product-analytics emission for this Node.js process.
///
/// Default policy: OFF unless `COGNEE_PRODUCT_TELEMETRY_ENABLED` is a
/// recognized explicit opt-in; suppression variables remain authoritative.
///
/// Returns a JS boolean — `true` if analytics are effective for this
/// process, `false` if opt-in is absent/invalid or suppression applies.
pub fn setup_telemetry_analytics(mut cx: FunctionContext) -> JsResult<JsBoolean> {
    Ok(cx.boolean(arm()))
}

/// Shared arming logic, callable from `#[neon::main]` so analytics are
/// armed automatically on module load without requiring an explicit
/// `setupTelemetryAnalytics()` call.
///
/// [`cognee_telemetry::env::arm_binding_emission`] is called
/// unconditionally so the `COGNEE_HOST_SDK` clause inside
/// [`cognee_telemetry::env::is_disabled`] is authoritative for any
/// binding-hosted `send_telemetry` call (decision 10). Arming only ever
/// *adds* suppression — it never enables emission. Actual emission is
/// re-evaluated per event via `is_disabled()`.
pub(crate) fn arm() -> bool {
    let slot = ARMED.get_or_init(|| Mutex::new(None));
    // lock poison is unrecoverable
    let mut lock = slot.lock().expect("lock poison is unrecoverable");
    if let Some(armed) = *lock {
        return armed;
    }

    cognee_telemetry::env::arm_binding_emission();
    let armed = !cognee_telemetry::env::is_disabled();
    *lock = Some(armed);
    armed
}
