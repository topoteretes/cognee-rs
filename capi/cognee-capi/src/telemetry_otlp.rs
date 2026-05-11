//! `cognee_init_otlp()` C entrypoint (gap 07 task 05).
//!
//! Initialises OpenTelemetry export from environment variables. The
//! function is argument-less by design: configuration flows through
//! `OTEL_*` and `COGNEE_TRACING_ENABLED` so that wrappers in Python,
//! Node, and C all expose the same env-driven surface.
//!
//! Behavioural matrix:
//!
//! | Env state | Return | Side effects |
//! |---|---|---|
//! | No `OTEL_EXPORTER_OTLP_ENDPOINT`, `COGNEE_TRACING_ENABLED` unset | `0` | None (idempotent noop) |
//! | Configured | `0` | OTEL `TracerProvider` installed; guard stashed |
//! | `init_telemetry` returns `Err` | `2` | Error logged to stderr |
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

use std::ffi::c_int;
use std::sync::{Mutex, OnceLock};

use cognee_observability::{EnvSettingsView, TelemetryGuard, init_telemetry, is_tracing_enabled};
use tracing_subscriber::Registry;

static OTEL_GUARD: OnceLock<Mutex<Option<TelemetryGuard>>> = OnceLock::new();

const SERVICE_NAME_DEFAULT: &str = "cognee.capi-binding";

/// Initialise OpenTelemetry export from environment variables.
///
/// Reads `COGNEE_TRACING_ENABLED`, `OTEL_EXPORTER_OTLP_ENDPOINT`,
/// `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_SERVICE_NAME` and related
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
    let slot = OTEL_GUARD.get_or_init(|| Mutex::new(None));
    let mut lock = match slot.lock() {
        Ok(l) => l,
        // lock poison is unrecoverable
        Err(_) => return 1,
    };
    if lock.is_some() {
        return 0;
    }

    apply_default_service_name(SERVICE_NAME_DEFAULT);

    let settings = EnvSettingsView::from_env();
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

/// Apply binding-specific default `OTEL_SERVICE_NAME` when unset.
fn apply_default_service_name(default: &str) {
    let current = std::env::var("OTEL_SERVICE_NAME").unwrap_or_default();
    if current.is_empty() {
        // SAFETY: `set_var` is `unsafe` in Rust 2024 because env
        // mutation is process-global UB if concurrent with other
        // env reads/writes. The OnceLock-guarded slot in
        // `cognee_init_otlp` ensures this runs at most once per
        // process, before `EnvSettingsView::from_env` reads the
        // env into the settings view.
        unsafe {
            std::env::set_var("OTEL_SERVICE_NAME", default);
        }
    }
}
