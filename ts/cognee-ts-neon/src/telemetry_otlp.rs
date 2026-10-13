//! `setupTelemetry()` Neon entrypoint (gap 07 task 05).
//!
//! Mirrors the PyO3 [`crate::telemetry_otlp`] design for Node.js:
//! argument-less, idempotent, composes
//! [`cognee_observability::init_telemetry`] on top of the default
//! stderr subscriber installed by [`crate::default_subscriber`].
//!
//! Settings resolve through the path every binding shares
//! ([`cognee_bindings_common::process_config::otel_settings`]): the binding
//! default service name `cognee.node-binding` (gap 07 decision 8) < the
//! environment < the `options` argument. The environment is never modified.
//! Per decision 12, the [`TelemetryGuard`] is stashed in a
//! `OnceLock<Mutex<Option<…>>>` so it lives until process exit (no
//! `shutdownTelemetry` companion in v1).

use std::sync::{Mutex, OnceLock};

use cognee_observability::{TelemetryGuard, init_telemetry, is_tracing_enabled};
use neon::prelude::*;
use tracing_subscriber::Registry;

use crate::default_subscriber::OTEL_RELOAD_HANDLE;
use crate::json::read_opts;

static OTEL_GUARD: OnceLock<Mutex<Option<TelemetryGuard>>> = OnceLock::new();

const SERVICE_NAME_DEFAULT: &str = "cognee.node-binding";

/// Initialise OpenTelemetry export: binding default < env < `options`
/// (optional object keyed like the `otel_*` settings, snake_ or camelCase).
///
/// See `setup_telemetry` in the PyO3 binding for the full contract — the JS
/// implementation is equivalent except for the default service name and the
/// FFI return shape (`undefined` on success, JS exception on failure).
pub fn setup_telemetry(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let slot = OTEL_GUARD.get_or_init(|| Mutex::new(None));
    // lock poison is unrecoverable
    let mut lock = slot.lock().expect("lock poison is unrecoverable");
    if lock.is_some() {
        return Ok(cx.undefined());
    }

    let options = read_opts(&mut cx, 0)?;
    let settings = match cognee_bindings_common::process_config::otel_settings(
        SERVICE_NAME_DEFAULT,
        Some(&options),
    ) {
        Ok(settings) => settings,
        Err(err) => return cx.throw_error(err),
    };
    if !is_tracing_enabled(&settings) {
        *lock = Some(TelemetryGuard::noop());
        return Ok(cx.undefined());
    }

    let (layer, guard) = match init_telemetry::<Registry>(&settings) {
        Ok(pair) => pair,
        Err(err) => return cx.throw_error(format!("init_telemetry failed: {err}")),
    };

    if let Some(handle) = OTEL_RELOAD_HANDLE.get() {
        if let Err(err) = handle.modify(|opt| *opt = Some(layer)) {
            eprintln!("cognee-ts-neon: failed to install OTEL layer: {err}");
        }
    } else {
        eprintln!(
            "cognee-ts-neon: setupTelemetry() called but the default subscriber \
             is suppressed; OTLP export disabled for tracing::* spans"
        );
    }

    *lock = Some(guard);
    Ok(cx.undefined())
}
