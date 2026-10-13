//! `cognee_init_otlp()` / `cognee_init_otlp_with_options()` C entrypoints
//! (gap 07 task 05).
//!
//! Initialise OpenTelemetry export. OTLP is process config, resolved through
//! the path every binding shares
//! ([`cognee_bindings_common::process_config::otel_settings`]): the binding
//! default service name `cognee.capi-binding` < the environment
//! (`COGNEE_TRACING_ENABLED`, `OTEL_*`, `ENV`) < the options JSON. The
//! environment is never modified.
//!
//! Behavioural matrix:
//!
//! | Env state | Return | Side effects |
//! |---|---|---|
//! | No `OTEL_EXPORTER_OTLP_ENDPOINT`, `COGNEE_TRACING_ENABLED` unset | `0` | None (idempotent noop) |
//! | Configured | `0` | OTEL `TracerProvider` installed; guard stashed |
//! | `init_telemetry` returns `Err`, or invalid options | `2` | Error logged to stderr and `cg_last_error_message()` |
//! | `Mutex` poisoned | `1` | None — caller should treat as fatal |
//!
//! ## v1 limitation — `tracing` → OTLP not wired from C
//!
//! Unlike the PyO3 and Neon bindings, the C API does **not** install
//! its own `tracing::Subscriber` on load (see
//! [`crate::logging::cognee_setup_logging`] for the optional explicit
//! install). Composing the [`cognee_observability::BoxedTelemetryLayer`]
//! returned by [`init_telemetry`] therefore has nowhere to live —
//! there is no reload-capable registry slot equivalent to
//! [`crate::default_subscriber::OTEL_RELOAD_HANDLE`] on the PyO3 /
//! Neon side. The returned layer is dropped; the global OTEL
//! [`opentelemetry::global::tracer_provider`] is still installed, so
//! direct OTEL SDK API spans (`tracer.span_builder(...).start(...)`)
//! reach the collector, but `#[tracing::instrument]` annotations in
//! cognee-rust crates do not.
//!
//! A future v2 enhancement could install a reload-capable C-side
//! subscriber to mirror the PyO3 / Neon shape; tracked in
//! `docs/telemetry/07/05-binding-otlp-setup.md` §8.

use std::ffi::{c_char, c_int};
use std::sync::{Mutex, OnceLock};

use cognee_observability::{TelemetryGuard, init_telemetry, is_tracing_enabled};
use tracing_subscriber::Registry;

use crate::error::set_last_error;

static OTEL_GUARD: OnceLock<Mutex<Option<TelemetryGuard>>> = OnceLock::new();

const SERVICE_NAME_DEFAULT: &str = "cognee.capi-binding";

/// Parse an optional options-JSON C string. `NULL`, empty and `"null"` mean
/// no options. On error the message is stored for `cg_last_error_message()`.
///
/// # Safety
///
/// `ptr` must be `NULL` or a valid NUL-terminated string.
pub(crate) unsafe fn options_from_c(ptr: *const c_char) -> Result<Option<serde_json::Value>, ()> {
    if ptr.is_null() {
        return Ok(None);
    }
    // SAFETY: forwarded caller contract; non-null checked above.
    let raw = unsafe { std::ffi::CStr::from_ptr(ptr) };
    // Every failure stores its own message, so `cg_last_error_message()` never
    // reports a stale error from an earlier call.
    let raw = raw.to_str().map_err(|e| {
        set_last_error(format!("options JSON is not valid UTF-8: {e}"));
    })?;
    cognee_bindings_common::process_config::parse_optional_json(raw).map_err(|e| {
        set_last_error(format!("invalid options JSON: {e}"));
    })
}

/// Initialise OpenTelemetry export from environment variables.
///
/// Reads `COGNEE_TRACING_ENABLED`, `OTEL_EXPORTER_OTLP_ENDPOINT`,
/// `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_SERVICE_NAME`, `ENV` and related
/// `OTEL_*` env vars. If neither `COGNEE_TRACING_ENABLED=true` nor a
/// non-empty `OTEL_EXPORTER_OTLP_ENDPOINT` is present, returns 0
/// without installing anything (no-config is treated as success).
///
/// Returns:
///   * `0` — success (including idempotent re-call and no-config skip).
///   * `1` — internal lock poisoning (should not happen).
///   * `2` — observability init failure (collector unreachable, etc.).
///
/// Safe to call multiple times. The first non-noop call wins.
///
/// Unlike `cognee_setup_logging`, this function does **not** install a
/// `tracing` subscriber on its own — see the module-level
/// documentation for the v1 limitation.
#[unsafe(no_mangle)]
pub extern "C" fn cognee_init_otlp() -> c_int {
    init(None)
}

/// [`cognee_init_otlp`] with a JSON object of options that win over the
/// environment, keyed like the `otel_*` settings: `cognee_tracing_enabled`,
/// `otel_service_name`, `otel_exporter_otlp_endpoint`,
/// `otel_exporter_otlp_headers`, `otel_exporter_otlp_protocol`,
/// `otel_span_processor`, `otel_traces_sampler`, `otel_traces_sampler_arg`,
/// `otel_deployment_environment`. `NULL` means no options. The first call of
/// either function wins.
///
/// # Safety
///
/// `options_json` must be `NULL` or a valid NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cognee_init_otlp_with_options(options_json: *const c_char) -> c_int {
    // SAFETY: forwarded caller contract.
    match unsafe { options_from_c(options_json) } {
        Ok(options) => init(options.as_ref()),
        Err(()) => 2,
    }
}

fn init(options: Option<&serde_json::Value>) -> c_int {
    let slot = OTEL_GUARD.get_or_init(|| Mutex::new(None));
    let mut lock = match slot.lock() {
        Ok(l) => l,
        // lock poison is unrecoverable
        Err(_) => return 1,
    };
    if lock.is_some() {
        return 0;
    }

    let settings = match cognee_bindings_common::process_config::otel_settings(
        SERVICE_NAME_DEFAULT,
        options,
    ) {
        Ok(settings) => settings,
        Err(err) => {
            eprintln!("cognee_init_otlp: {err}");
            set_last_error(err);
            return 2;
        }
    };
    if !is_tracing_enabled(&settings) {
        *lock = Some(TelemetryGuard::noop());
        return 0;
    }

    let (_layer, guard) = match init_telemetry::<Registry>(&settings) {
        Ok(pair) => pair,
        Err(err) => {
            eprintln!("cognee_init_otlp: {err}");
            return 2;
        }
    };

    // NOTE: v1 limitation documented at the module level — the C
    // binding has no reload-capable subscriber to compose the layer
    // onto. The `TracerProvider` installed by `init_telemetry` still
    // routes direct OTEL SDK calls; `tracing::*` events from
    // cognee-rust crates are NOT bridged.
    let _ = _layer;

    *lock = Some(guard);
    0
}
