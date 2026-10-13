//! Product-analytics entrypoints for the PyO3 binding.
//!
//! Analytics are ON by default (Python-SDK parity, opt-out) and the switch is
//! **process-wide**: one [`cognee_telemetry::TelemetryConfig`] governs every
//! event the interpreter sends, whichever `Cognee` handle emits it.
//!
//! * `set_telemetry_enabled(enabled)` — the programmatic switch. Wins over the
//!   environment, and disabling also drops events queued but not yet sent.
//! * `is_telemetry_enabled()` — whether an event sent now would be posted.
//! * `setup_telemetry_analytics()` — arms the binding (also done on import)
//!   and reports the same state as `is_telemetry_enabled()`.
//!
//! The environment is the default: `TELEMETRY_DISABLED` (any non-empty value)
//! or `ENV` in {`test`, `dev`} disable, read when an event is sent (so a
//! `.env` loaded by the first handle counts). The Rust-specific `COGNEE_HOST_SDK` sentinel additionally lets an
//! embedding host SDK suppress this binding's emissions to avoid
//! double-counting (decision 10).

use pyo3::prelude::*;

/// Arm cognee product-analytics emission from this Python process and report
/// whether it is effective.
///
/// Returns `True` when an event sent now would be posted. Idempotent: arming
/// never changes the switch, so repeated calls agree until the switch is
/// flipped with `set_telemetry_enabled`.
#[pyfunction]
pub fn setup_telemetry_analytics() -> PyResult<bool> {
    Ok(arm())
}

/// Turn product analytics on or off for the whole process (all handles).
/// Wins over `TELEMETRY_DISABLED` / `ENV` in the environment.
#[pyfunction]
pub fn set_telemetry_enabled(enabled: bool) {
    cognee_telemetry::set_enabled(enabled);
}

/// Whether product analytics are currently enabled for this process.
#[pyfunction]
pub fn is_telemetry_enabled() -> bool {
    cognee_telemetry::is_enabled()
}

/// Shared arming logic, callable from the `#[pymodule]` init so
/// analytics are armed automatically on import (Python-SDK parity)
/// without requiring the caller to invoke `setup_telemetry_analytics`.
///
/// [`cognee_telemetry::env::arm_binding_emission`] makes the host-SDK
/// sentinel authoritative for this binding (decision 10). Arming only ever
/// *adds* suppression — it never enables emission.
pub(crate) fn arm() -> bool {
    cognee_telemetry::env::arm_binding_emission();
    cognee_telemetry::is_enabled()
}
