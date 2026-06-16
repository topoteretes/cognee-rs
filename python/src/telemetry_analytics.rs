//! `setup_telemetry_analytics()` PyO3 entrypoint (gap 07 task 06).
//!
//! Argument-less, idempotent installer that arms cognee
//! product-analytics emission for this Python process.
//!
//! Policy (Python-SDK parity — analytics ON by default):
//!
//! * `armed` unless `TELEMETRY_DISABLED` is set to any non-empty value,
//!   OR `ENV` is `"test"` / `"dev"`, OR `COGNEE_HOST_SDK` is set to any
//!   non-empty value.
//!
//! This mirrors the upstream `cognee` Python SDK, which emits telemetry
//! by default and only honours `TELEMETRY_DISABLED` / `ENV`. The
//! Rust-specific `COGNEE_HOST_SDK` sentinel additionally lets an
//! embedding host SDK suppress this binding's emissions to avoid
//! double-counting.
//!
//! Idempotent via the same `OnceLock<Mutex<Option<bool>>>` shape as
//! `setup_logging` / `setup_telemetry` (decision 12). The first call
//! latches the reported decision; subsequent calls return it unchanged.
//!
//! [`cognee_telemetry::env::arm_binding_emission`] is called
//! unconditionally so the `COGNEE_HOST_SDK` clause inside
//! [`cognee_telemetry::env::is_disabled`] is authoritative for any
//! binding-hosted `send_telemetry` call (decision 10). Arming only ever
//! *adds* suppression — it never enables emission — so it is safe even
//! when telemetry is otherwise disabled. Actual emission is re-evaluated
//! per event via `is_disabled()`.

use std::sync::{Mutex, OnceLock};

use pyo3::prelude::*;

static ARMED: OnceLock<Mutex<Option<bool>>> = OnceLock::new();

/// Arm cognee product-analytics emission from this Python process.
///
/// Default policy (Python-SDK parity): analytics are ON unless
/// `TELEMETRY_DISABLED` is set, `ENV` is `"test"`/`"dev"`, or
/// `COGNEE_HOST_SDK` is set.
///
/// Returns `True` if analytics are effective for this process,
/// `False` if an opt-out env var suppresses them. Idempotent —
/// repeated calls return the latched decision without re-evaluating
/// the environment.
#[pyfunction]
pub fn setup_telemetry_analytics() -> PyResult<bool> {
    Ok(arm())
}

/// Shared arming logic, callable from the `#[pymodule]` init so
/// analytics are armed automatically on import (Python-SDK parity)
/// without requiring the caller to invoke `setup_telemetry_analytics`.
pub(crate) fn arm() -> bool {
    let slot = ARMED.get_or_init(|| Mutex::new(None));
    // lock poison is unrecoverable
    let mut lock = slot.lock().expect("lock poison is unrecoverable");
    if let Some(armed) = *lock {
        return armed;
    }

    // Arm unconditionally so is_disabled()'s COGNEE_HOST_SDK clause is
    // authoritative for binding-hosted emissions (decision 10).
    cognee_telemetry::env::arm_binding_emission();
    let armed = !cognee_telemetry::env::is_disabled();
    *lock = Some(armed);
    armed
}
