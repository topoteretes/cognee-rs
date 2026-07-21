//! `setup_telemetry_analytics()` PyO3 entrypoint (gap 07 task 06).
//!
//! Argument-less, idempotent installer that arms cognee
//! product-analytics emission for this Python process.
//!
//! Local sovereign policy (analytics OFF by default):
//!
//! * `armed` only when `COGNEE_PRODUCT_TELEMETRY_ENABLED` is an explicit
//!   recognized opt-in and no suppression variable applies.
//!
//! This deliberately diverges from the upstream Python SDK's opt-out
//! policy. `TELEMETRY_DISABLED`, `ENV`, and the Rust-specific
//! `COGNEE_HOST_SDK` sentinel let an
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
/// Default policy: analytics are OFF unless
/// `COGNEE_PRODUCT_TELEMETRY_ENABLED` is a recognized explicit opt-in;
/// `TELEMETRY_DISABLED`, `ENV`, and `COGNEE_HOST_SDK` can still suppress.
///
/// Returns `True` if analytics are effective for this process,
/// `False` if opt-in is absent/invalid or a suppression applies. Idempotent —
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
    #[allow(clippy::expect_used, reason = "lock poison is unrecoverable")]
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
