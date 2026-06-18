//! `setup_telemetry()` PyO3 entrypoint (gap 07 task 05).
//!
//! Argument-less, idempotent installer that composes
//! [`cognee_observability::init_telemetry`] on top of the default
//! subscriber installed by [`crate::default_subscriber`].
//!
//! Behaviour:
//!
//! 1. Apply binding-specific `OTEL_SERVICE_NAME` default
//!    (`cognee.python-binding`) when the env var is unset/empty — gap
//!    07 decision 8.
//! 2. Build [`cognee_observability::EnvSettingsView::from_env`] and
//!    short-circuit when [`cognee_observability::is_tracing_enabled`]
//!    returns `false` (no `COGNEE_TRACING_ENABLED`, no
//!    `OTEL_EXPORTER_OTLP_ENDPOINT`). A noop guard is stashed so
//!    repeat calls remain cheap.
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

use std::sync::{Mutex, OnceLock};

use cognee_observability::{EnvSettingsView, TelemetryGuard, init_telemetry, is_tracing_enabled};
use pyo3::prelude::*;
use tracing_subscriber::Registry;

use crate::default_subscriber::OTEL_RELOAD_HANDLE;

static OTEL_GUARD: OnceLock<Mutex<Option<TelemetryGuard>>> = OnceLock::new();

const SERVICE_NAME_DEFAULT: &str = "cognee.python-binding";

/// Initialise OpenTelemetry export from environment variables.
///
/// Reads `COGNEE_TRACING_ENABLED`, `OTEL_EXPORTER_OTLP_ENDPOINT`,
/// `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_SERVICE_NAME` and related
/// `OTEL_*` env vars. When neither `COGNEE_TRACING_ENABLED=true` nor a
/// non-empty `OTEL_EXPORTER_OTLP_ENDPOINT` is set, the function returns
/// successfully without installing anything (no-config = no-op).
///
/// Idempotent: the first non-noop call wins; subsequent calls return
/// `None` without touching the installed provider.
#[pyfunction]
pub fn setup_telemetry() -> PyResult<()> {
    let slot = OTEL_GUARD.get_or_init(|| Mutex::new(None));
    // lock poison is unrecoverable
    #[allow(clippy::expect_used, reason = "lock poison is unrecoverable")]
    let mut lock = slot.lock().expect("lock poison is unrecoverable");
    if lock.is_some() {
        return Ok(());
    }

    // Decision 8: apply binding-specific default service name.
    apply_default_service_name(SERVICE_NAME_DEFAULT);

    let settings = EnvSettingsView::from_env();
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

/// Apply binding-specific default `OTEL_SERVICE_NAME` when unset.
///
/// The user's explicit env var always wins — this only patches the
/// default so dashboards can distinguish embedded use from
/// `cognee-cli` / `cognee-http-server` traces. Gap 07 decision 8.
fn apply_default_service_name(default: &str) {
    let current = std::env::var("OTEL_SERVICE_NAME").unwrap_or_default();
    if current.is_empty() {
        // SAFETY: `set_var` is `unsafe` in Rust 2024 because env
        // mutation is process-global UB if concurrent with other
        // env reads/writes. We mutate once per process at the
        // top of `setup_telemetry()`, guarded by the idempotent
        // OnceLock slot, before reading the env into
        // `EnvSettingsView::from_env`. Hosts that call this from
        // multiple threads still funnel through the mutex above.
        unsafe {
            std::env::set_var("OTEL_SERVICE_NAME", default);
        }
    }
}
