//! Module-level statics: logging, OTLP telemetry, product analytics.
//!
//! These mirror the neon binding's `logging.rs`, `telemetry_analytics.rs`,
//! `telemetry_otlp.rs`, and `default_subscriber.rs`. Logging and OTLP are
//! process config, resolved through the path every binding shares
//! ([`cognee_bindings_common::process_config`]): defaults < environment <
//! the optional options JSON passed from Java. The default stderr
//! subscriber is installed once from [`JNI_OnLoad`](crate::JNI_OnLoad) via
//! [`install_default_subscriber`], reserving a reload slot so `initOtlp()` can
//! swap a real OTEL layer in without re-initialising the global subscriber.

use std::sync::{Mutex, Once, OnceLock};

use jni::JNIEnv;
use jni::objects::{JClass, JString};
use jni::sys::jboolean;
use tracing_subscriber::{Registry, fmt, layer::SubscriberExt, reload, util::SubscriberInitExt};

use cognee_bindings_common::process_config;
use cognee_observability::{
    BoxedTelemetryLayer, TelemetryGuard, init_telemetry, is_tracing_enabled,
};

use crate::guard_void;

// --- setupLogging (port of cognee-ts-neon/src/logging.rs) ---

static LOG_GUARDS: OnceLock<Mutex<Option<cognee_logging::LogGuards>>> = OnceLock::new();

/// Read the optional options-JSON argument, throwing `VALIDATION_ERROR` on
/// malformed JSON.
fn options_arg(env: &mut JNIEnv, options_json: &JString) -> Option<serde_json::Value> {
    match crate::args::arg_json(env, options_json) {
        Ok(v) => Some(v),
        Err(e) => {
            crate::errors::throw_cognee_exception(env, e.code(), &e.to_string());
            None
        }
    }
}

/// `setupLogging(String optionsJson)` — env < options, idempotent.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_setupLogging<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    options_json: JString<'l>,
) {
    guard_void(&mut env, |env| {
        let slot = LOG_GUARDS.get_or_init(|| Mutex::new(None));
        // lock poison is unrecoverable
        let mut lock = slot.lock().expect("lock poison is unrecoverable");
        if lock.is_some() {
            return; // idempotent
        }
        let Some(options) = options_arg(env, &options_json) else {
            return;
        };
        match process_config::logging_config(Some(&options)) {
            Ok(cfg) => {
                let guards = cognee_logging::init_logging(
                    cfg,
                    std::iter::empty::<cognee_logging::BoxedLayer>(),
                );
                *lock = Some(guards);
            }
            Err(e) => {
                crate::errors::throw_cognee_exception(env, "VALIDATION_ERROR", &e);
            }
        }
    })
}

// --- initTelemetry / analytics switch (port of telemetry_analytics.rs) ---

/// Shared arming logic; also called from `JNI_OnLoad` so the host-SDK
/// sentinel (`COGNEE_HOST_SDK`) is authoritative for any binding-hosted
/// `send_telemetry` call. Arming only ever *adds* suppression — it never
/// enables emission. Idempotent; returns whether analytics are enabled.
pub(crate) fn arm_analytics() -> bool {
    cognee_telemetry::env::arm_binding_emission();
    cognee_telemetry::is_enabled()
}

/// `initTelemetry() -> boolean` — arm product analytics and report whether
/// they are enabled (ON unless switched off, TELEMETRY_DISABLED /
/// ENV∈{test,dev}, or COGNEE_HOST_SDK).
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_initTelemetry<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
) -> jboolean {
    // Boolean return: default `false` (0) on panic.
    crate::guard_jlong(&mut env, |_env| arm_analytics() as jni::sys::jlong) as jboolean
}

/// `setTelemetryEnabled(boolean)` — process-wide analytics switch; wins over
/// the environment.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_setTelemetryEnabled<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    enabled: jboolean,
) {
    guard_void(&mut env, |_env| cognee_telemetry::set_enabled(enabled != 0))
}

/// `isTelemetryEnabled() -> boolean`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_isTelemetryEnabled<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
) -> jboolean {
    crate::guard_jlong(&mut env, |_env| {
        cognee_telemetry::is_enabled() as jni::sys::jlong
    }) as jboolean
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
#[allow(
    clippy::disallowed_methods,
    reason = "env-only by design: read at module load, before user code can configure the binding"
)]
pub(crate) fn install_default_subscriber() {
    INIT.call_once(|| {
        if std::env::var_os("COGNEE_BINDING_SUPPRESS_LOGS")
            .filter(|v| !v.is_empty())
            .is_some()
        {
            return;
        }

        // RUST_LOG > LOG_LEVEL > the default filter, resolved by the logging
        // config's boundary like every other logging setting. Only the level
        // is read, so a malformed, unrelated logging variable cannot drop it,
        // and Python level names (`WARNING`) are mapped.
        let level = cognee_logging::LoggingConfig::level_filter_from_env();
        let filter = cognee_logging::env_filter(level.as_deref());

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

/// `initOtlp(String optionsJson)` — install OTLP export (idempotent). Settings
/// resolve as the binding default service name `cognee.java-binding` < env <
/// options; the environment is never modified. Ports telemetry_otlp.rs +
/// default_subscriber.rs.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_initOtlp<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    options_json: JString<'l>,
) {
    guard_void(&mut env, |env| {
        let slot = OTEL_GUARD.get_or_init(|| Mutex::new(None));
        // lock poison is unrecoverable
        let mut lock = slot.lock().expect("lock poison is unrecoverable");
        if lock.is_some() {
            return; // idempotent
        }

        let Some(options) = options_arg(env, &options_json) else {
            return;
        };
        let settings = match process_config::otel_settings(SERVICE_NAME_DEFAULT, Some(&options)) {
            Ok(settings) => settings,
            Err(e) => {
                crate::errors::throw_cognee_exception(env, "VALIDATION_ERROR", &e);
                return;
            }
        };
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
