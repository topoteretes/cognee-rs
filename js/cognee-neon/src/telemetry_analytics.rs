//! `setupTelemetryAnalytics()` Neon entrypoint (gap 07 task 06).
//!
//! Argument-less, idempotent installer that arms cognee
//! product-analytics emission for this Node.js process subject to the
//! per-binding policy from gap 07 decision 11.
//!
//! Policy (Neon is the canonical sender in the JS ecosystem — no
//! upstream JS cognee SDK to defer to):
//!
//! * `armed` unless `TELEMETRY_DISABLED` is set to any non-empty
//!   value, OR `ENV` is `"test"` / `"dev"`, OR `COGNEE_HOST_SDK` is
//!   set to any non-empty value.
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
/// Default policy (gap 07 decision 11): ON unless `TELEMETRY_DISABLED`
/// is set, `ENV` is `"test"`/`"dev"`, or `COGNEE_HOST_SDK` is set.
///
/// Returns a JS boolean — `true` if analytics were armed by this call
/// (or a prior call), `false` if the policy suppressed emission.
/// Idempotent.
pub fn setup_telemetry_analytics(mut cx: FunctionContext) -> JsResult<JsBoolean> {
    let slot = ARMED.get_or_init(|| Mutex::new(None));
    // lock poison is unrecoverable
    let mut lock = slot.lock().expect("lock poison is unrecoverable");
    if let Some(armed) = *lock {
        return Ok(cx.boolean(armed));
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
    Ok(cx.boolean(armed))
}
