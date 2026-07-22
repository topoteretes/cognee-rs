//! Module-level statics: logging, OTLP telemetry, product analytics.
//!
//! These mirror the neon binding's `logging.rs`, `telemetry_analytics.rs`,
//! `telemetry_otlp.rs`, and `default_subscriber.rs`. The default stderr
//! subscriber is installed once from [`JNI_OnLoad`](crate::JNI_OnLoad) via
//! [`install_default_subscriber`], reserving a reload slot so `initOtlp()` can
//! swap a real OTEL layer in without re-initialising the global subscriber.

use std::sync::{Mutex, Once, OnceLock};

use jni::JNIEnv;
use jni::objects::JClass;
use jni::sys::jboolean;
use tracing_subscriber::{
    EnvFilter, Registry, fmt, layer::SubscriberExt, reload, util::SubscriberInitExt,
};

use cognee_observability::{
    BoxedTelemetryLayer, EnvSettingsView, TelemetryGuard, init_telemetry, is_tracing_enabled,
};

use crate::guard_void;

// --- setupLogging (port of cognee-ts-neon/src/logging.rs) ---

static LOG_GUARDS: OnceLock<Mutex<Option<cognee_logging::LogGuards>>> = OnceLock::new();

/// `setupLogging()` — env-driven, idempotent.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_setupLogging<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
) {
    guard_void(&mut env, |env| {
        let slot = LOG_GUARDS.get_or_init(|| Mutex::new(None));
        // lock poison is unrecoverable
        let mut lock = slot.lock().expect("lock poison is unrecoverable");
        if lock.is_some() {
            return; // idempotent
        }
        match cognee_logging::LoggingConfig::from_env() {
            Ok(cfg) => {
                let guards = cognee_logging::init_logging(
                    cfg,
                    std::iter::empty::<cognee_logging::BoxedLayer>(),
                );
                *lock = Some(guards);
            }
            Err(e) => {
                crate::errors::throw_cognee_exception(
                    env,
                    "RUNTIME_ERROR",
                    &format!("invalid logging config: {e}"),
                );
            }
        }
    })
}

// --- initTelemetry / analytics arming (port of telemetry_analytics.rs) ---

static ANALYTICS_ARMED: OnceLock<Mutex<Option<bool>>> = OnceLock::new();

/// Shared arming logic; also called from `JNI_OnLoad` so the `COGNEE_HOST_SDK`
/// clause inside `is_disabled` is authoritative for any binding-hosted
/// `send_telemetry` call. Arming only ever *adds* suppression — it never
/// enables emission. Idempotent.
pub(crate) fn arm_analytics() -> bool {
    let slot = ANALYTICS_ARMED.get_or_init(|| Mutex::new(None));
    // lock poison is unrecoverable
    let mut lock = slot.lock().expect("lock poison is unrecoverable");
    if let Some(armed) = *lock {
        return armed;
    }
    cognee_telemetry::env::arm_binding_emission();
    let armed = !cognee_telemetry::env::is_disabled();
    *lock = Some(armed);
    armed
}

/// `initTelemetry() -> boolean` — arm product analytics per the per-binding
/// policy (ON unless TELEMETRY_DISABLED / ENV∈{test,dev} / COGNEE_HOST_SDK).
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_initTelemetry<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
) -> jboolean {
    // Boolean return: default `false` (0) on panic.
    crate::guard_jlong(&mut env, |_env| arm_analytics() as jni::sys::jlong) as jboolean
}

// --- default subscriber (port of default_subscriber.rs) ---

static INIT: Once = Once::new();

/// Process-global reload handle for the OTEL telemetry layer slot. Set by
/// [`install_default_subscriber`]; consumed by `initOtlp()` to swap a real
/// `BoxedTelemetryLayer<Registry>` into the registry. `None` only when the
/// default subscriber was suppressed via `COGNEE_BINDING_SUPPRESS_LOGS`.
#[allow(clippy::type_complexity)]
static OTEL_RELOAD_HANDLE: OnceLock<
    reload::Handle<Option<BoxedTelemetryLayer<Registry>>, Registry>,
> = OnceLock::new();

/// Install the default stderr `fmt` subscriber (idempotent via [`Once`]).
///
/// Called from `JNI_OnLoad` before any native method runs — the equivalent of
/// neon's `#[neon::main]` hook. Returns silently when
/// `COGNEE_BINDING_SUPPRESS_LOGS` is set to any non-empty value, or when another
/// subscriber already claimed the global `tracing` slot (`try_init` semantics).
pub(crate) fn install_default_subscriber() {
    INIT.call_once(|| {
        if std::env::var_os("COGNEE_BINDING_SUPPRESS_LOGS")
            .filter(|v| !v.is_empty())
            .is_some()
        {
            return;
        }

        let filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new(cognee_logging::default_filter()));

        // Reserve a reload-capable slot for the OTEL layer, starting empty.
        let (otel_slot, handle) = reload::Layer::new(None::<BoxedTelemetryLayer<Registry>>);
        let _ = OTEL_RELOAD_HANDLE.set(handle);

        let _ = Registry::default()
            .with(otel_slot)
            .with(filter)
            .with(fmt::layer().with_writer(std::io::stderr).with_ansi(true))
            .try_init();
    });
}

// --- initOtlp (port of telemetry_otlp.rs) ---

static OTEL_GUARD: OnceLock<Mutex<Option<TelemetryGuard>>> = OnceLock::new();

const SERVICE_NAME_DEFAULT: &str = "cognee.java-binding";

/// Apply binding-specific default `OTEL_SERVICE_NAME` when unset.
fn apply_default_service_name(default: &str) {
    let current = std::env::var("OTEL_SERVICE_NAME").unwrap_or_default();
    if current.is_empty() {
        // SAFETY: single write, guarded by the OnceLock-protected slot in
        // `initOtlp`; mirrors python/src/telemetry_otlp.rs.
        unsafe {
            std::env::set_var("OTEL_SERVICE_NAME", default);
        }
    }
}

/// `initOtlp()` — install OTLP export from env (idempotent). Service-name
/// default `cognee.java-binding`. Ports telemetry_otlp.rs + default_subscriber.rs.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_initOtlp<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
) {
    guard_void(&mut env, |env| {
        let slot = OTEL_GUARD.get_or_init(|| Mutex::new(None));
        // lock poison is unrecoverable
        let mut lock = slot.lock().expect("lock poison is unrecoverable");
        if lock.is_some() {
            return; // idempotent
        }

        apply_default_service_name(SERVICE_NAME_DEFAULT);

        let settings = EnvSettingsView::from_env();
        if !is_tracing_enabled(&settings) {
            *lock = Some(TelemetryGuard::noop());
            return;
        }

        let (layer, guard) = match init_telemetry::<Registry>(&settings) {
            Ok(pair) => pair,
            Err(err) => {
                crate::errors::throw_cognee_exception(
                    env,
                    "RUNTIME_ERROR",
                    &format!("init_telemetry failed: {err}"),
                );
                return;
            }
        };

        if let Some(handle) = OTEL_RELOAD_HANDLE.get() {
            if let Err(err) = handle.modify(|opt| *opt = Some(layer)) {
                eprintln!("cognee-java-jni: failed to install OTEL layer: {err}");
            }
        } else {
            eprintln!(
                "cognee-java-jni: initOtlp() called but the default subscriber \
                 is suppressed; OTLP export disabled for tracing::* spans"
            );
        }

        *lock = Some(guard);
    })
}
