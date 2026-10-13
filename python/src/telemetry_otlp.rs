//! `setup_telemetry()` PyO3 entrypoint (gap 07 task 05).
//!
//! Argument-less, idempotent installer that composes
//! [`cognee_observability::init_telemetry`] on top of the default
//! subscriber installed by [`crate::default_subscriber`].
//!
//! Behaviour:
//!
//! 1. Resolve the OTel settings through the path every binding shares
//!    ([`cognee_bindings_common::process_config::otel_settings`]):
//!    the binding default service name (`cognee.python-binding`, gap 07
//!    decision 8) < the environment (`COGNEE_TRACING_ENABLED`, `OTEL_*`,
//!    `ENV`) < the `options` dict. The environment is never modified.
//! 2. Short-circuit when [`cognee_observability::is_tracing_enabled`]
//!    returns `false` (tracing not enabled, no endpoint). A noop guard is
//!    stashed so repeat calls remain cheap.
//! 3. Otherwise call `init_telemetry::<Registry>(&settings)` to get a
//!    `(BoxedTelemetryLayer<Registry>, TelemetryGuard)` pair.
//! 4. Swap the layer into the reload slot exposed by
//!    [`crate::default_subscriber::OTEL_RELOAD_HANDLE`]. If the
//!    default subscriber was suppressed, log a single stderr warning
//!    so direct OTEL SDK callers still work but operators know
//!    `tracing::*` spans will not be exported.
//! 5. Stash the [`TelemetryGuard`] in
//!    `OTEL_GUARD: OnceLock<Mutex<Option<TelemetryGuard>>>` so the
//!    OTEL pipeline lives until process exit — gap 07 decision 12.
//!
//! OTLP export is process config: it is initialised once, before any handle
//! exists, so it does not read a handle's `Settings`; pass the same `otel_*`
//! keys in `options` instead.

use std::sync::{Mutex, OnceLock};

use cognee_observability::{TelemetryGuard, init_telemetry, is_tracing_enabled};
use pyo3::prelude::*;
use tracing_subscriber::Registry;

use crate::default_subscriber::OTEL_RELOAD_HANDLE;
use crate::json::py_to_serde;

static OTEL_GUARD: OnceLock<Mutex<Option<TelemetryGuard>>> = OnceLock::new();

const SERVICE_NAME_DEFAULT: &str = "cognee.python-binding";

/// Initialise OpenTelemetry export.
///
/// Settings resolve as binding default < environment (`COGNEE_TRACING_ENABLED`,
/// `OTEL_*`, `ENV`) < `options` (optional dict keyed like the `Settings`
/// fields: `cognee_tracing_enabled`, `otel_service_name`,
/// `otel_exporter_otlp_endpoint`, `otel_exporter_otlp_headers`,
/// `otel_exporter_otlp_protocol`, `otel_span_processor`,
/// `otel_traces_sampler`, `otel_traces_sampler_arg`,
/// `otel_deployment_environment`). When tracing is neither enabled nor given
/// an endpoint, the function returns successfully without installing anything
/// (no-config = no-op).
///
/// Idempotent: the first call wins; subsequent calls return `None` without
/// touching the installed provider.
#[pyfunction]
#[pyo3(signature = (options=None))]
pub fn setup_telemetry(options: Option<Bound<'_, PyAny>>) -> PyResult<()> {
    let slot = OTEL_GUARD.get_or_init(|| Mutex::new(None));
    // lock poison is unrecoverable
    #[allow(clippy::expect_used, reason = "lock poison is unrecoverable")]
    let mut lock = slot.lock().expect("lock poison is unrecoverable");
    if lock.is_some() {
        return Ok(());
    }

    // Decision 8: the binding-specific service name is a default, below the
    // environment and the options.
    let options = options.as_ref().map(py_to_serde).transpose()?;
    let settings = cognee_bindings_common::process_config::otel_settings(
        SERVICE_NAME_DEFAULT,
        options.as_ref(),
    )
    .map_err(pyo3::exceptions::PyValueError::new_err)?;
    if !is_tracing_enabled(&settings) {
        // Not configured — leave the reload slot empty and stash a
        // sentinel guard so a re-call short-circuits cheaply.
        *lock = Some(TelemetryGuard::noop());
        return Ok(());
    }

    let (layer, guard) = init_telemetry::<Registry>(&settings).map_err(|e| {
        pyo3::exceptions::PyRuntimeError::new_err(format!("init_telemetry failed: {e}"))
    })?;

    // Swap the OTEL layer into the reload slot. If the slot was never
    // set (COGNEE_BINDING_SUPPRESS_LOGS=1), warn and skip — the OTEL
    // TracerProvider is still active, but `tracing::*` events will
    // not be bridged into it.
    if let Some(handle) = OTEL_RELOAD_HANDLE.get() {
        if let Err(err) = handle.modify(|opt| *opt = Some(layer)) {
            eprintln!("cognee-python: failed to install OTEL layer: {err}");
        }
    } else {
        eprintln!(
            "cognee-python: setup_telemetry() called but the default subscriber \
             is suppressed; OTLP export disabled for tracing::* spans"
        );
    }

    *lock = Some(guard);
    Ok(())
}
