//! Product-analytics entrypoints for the Neon binding.
//!
//! Analytics are ON by default (Python-SDK parity, opt-out) and the switch is
//! **process-wide**: one [`cognee_telemetry::TelemetryConfig`] governs every
//! event the Node process sends, whichever `Cognee` handle emits it.
//!
//! * `setTelemetryEnabled(enabled)` — the programmatic switch; wins over the
//!   environment, and disabling also drops events queued but not yet sent.
//! * `isTelemetryEnabled()` — whether an event sent now would be posted.
//! * `setupTelemetryAnalytics()` — arms the binding (also done on module load)
//!   and reports the same state as `isTelemetryEnabled()`.
//!
//! The environment is the default (`TELEMETRY_DISABLED`, `ENV` in {test,
//! dev}), read when an event is sent. `COGNEE_HOST_SDK` lets an embedding
//! host SDK suppress this binding's emissions (decision 10).

use neon::prelude::*;

/// Arm analytics for this process and report whether they are enabled.
pub fn setup_telemetry_analytics(mut cx: FunctionContext) -> JsResult<JsBoolean> {
    Ok(cx.boolean(arm()))
}

/// `setTelemetryEnabled(enabled: boolean)` — process-wide switch.
pub fn set_telemetry_enabled(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let enabled = cx.argument::<JsBoolean>(0)?.value(&mut cx);
    cognee_telemetry::set_enabled(enabled);
    Ok(cx.undefined())
}

/// `isTelemetryEnabled(): boolean`.
pub fn is_telemetry_enabled(mut cx: FunctionContext) -> JsResult<JsBoolean> {
    Ok(cx.boolean(cognee_telemetry::is_enabled()))
}

/// Shared arming logic, callable from `#[neon::main]` so analytics are
/// armed automatically on module load.
///
/// [`cognee_telemetry::env::arm_binding_emission`] makes the host-SDK
/// sentinel authoritative for this binding (decision 10). Arming only ever
/// *adds* suppression — it never enables emission.
pub(crate) fn arm() -> bool {
    cognee_telemetry::env::arm_binding_emission();
    cognee_telemetry::is_enabled()
}
