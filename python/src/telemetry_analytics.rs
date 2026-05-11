//! `setup_telemetry_analytics()` PyO3 entrypoint (gap 07 task 06).
//!
//! Argument-less, idempotent installer that arms cognee
//! product-analytics emission for this Python process subject to the
//! per-binding policy from gap 07 decision 11.
//!
//! Policy (Python defers identity ownership to the upstream `cognee`
//! Python SDK):
//!
//! * `armed` only if `COGNEE_RUST_TELEMETRY=1` (or `true`, case
//!   insensitive) is set AND `COGNEE_HOST_SDK` is unset/empty.
//! * Otherwise stays OFF — the upstream Python SDK owns identity and
//!   emission.
//!
//! Idempotent via the same `OnceLock<Mutex<Option<bool>>>` shape as
//! `setup_logging` / `setup_telemetry` (decision 12). The first call
//! latches the decision; subsequent calls return it unchanged.
//!
//! When the policy arms emission this calls
//! [`cognee_telemetry::env::arm_binding_emission`] so the
//! `COGNEE_HOST_SDK` sentinel inside
//! [`cognee_telemetry::env::is_disabled`] applies to any future
//! `send_telemetry` calls originating from a binding path (decision
//! 10).

use std::sync::{Mutex, OnceLock};

use pyo3::prelude::*;

static ARMED: OnceLock<Mutex<Option<bool>>> = OnceLock::new();

/// Arm cognee product-analytics emission from this Python process.
///
/// Default policy (gap 07 decision 11): analytics stay OFF unless
/// `COGNEE_RUST_TELEMETRY=1` is set AND `COGNEE_HOST_SDK` is unset.
/// The upstream `cognee` Python SDK owns identity emission; this
/// binding defers to it.
///
/// Returns `True` if analytics were armed by this call (or a prior
/// call). Idempotent — repeated calls return the latched decision
/// without re-evaluating the environment.
#[pyfunction]
pub fn setup_telemetry_analytics() -> PyResult<bool> {
    let slot = ARMED.get_or_init(|| Mutex::new(None));
    // lock poison is unrecoverable
    let mut lock = slot.lock().expect("lock poison is unrecoverable");
    if let Some(armed) = *lock {
        return Ok(armed);
    }

    let opt_in = std::env::var("COGNEE_RUST_TELEMETRY")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let host_sdk = std::env::var("COGNEE_HOST_SDK")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let armed = opt_in && !host_sdk;

    if armed {
        cognee_telemetry::env::arm_binding_emission();
    }
    *lock = Some(armed);
    Ok(armed)
}
