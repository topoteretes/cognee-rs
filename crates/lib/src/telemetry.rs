//! Telemetry surface for embedders.
//!
//! Re-exports the public API of [`cognee_observability`] (OTEL setup,
//! gap 01) and [`cognee_telemetry`] (`send_telemetry` product-analytics
//! client, gap 02) so consumers reach both through the same
//! `cognee::telemetry` namespace.
//!
//! When the `telemetry` cargo feature is on, observability re-exports
//! pull in `cognee-observability` and the `send_telemetry` re-exports
//! pull in the real `cognee-telemetry` impl. When the feature is off,
//! observability re-exports vanish and the `send_telemetry` re-exports
//! resolve to the noop bodies inside `cognee-telemetry` (so
//! `cognee_telemetry::send_telemetry` and
//! `cognee_telemetry::TelemetryError` are always available — they
//! exist regardless of feature state in the leaf crate).
//!
//! # Turning product analytics off
//!
//! ```no_run
//! // Process-wide; wins over TELEMETRY_DISABLED / ENV in the environment.
//! cognee::telemetry::set_enabled(false);
//! assert!(!cognee::telemetry::is_enabled());
//! ```

// --- gap 01: OTEL/observability surface (feature-gated) ---------------------

#[cfg(feature = "telemetry")]
pub use cognee_observability::{
    BoxedTelemetryLayer, SettingsView, TelemetryGuard, TelemetryInitError, already_instrumented,
    init_telemetry, is_tracing_enabled, parse_otlp_headers,
};

#[cfg(feature = "telemetry")]
use crate::config::Settings;

#[cfg(feature = "telemetry")]
impl SettingsView for Settings {
    fn tracing_enabled(&self) -> bool {
        self.cognee_tracing_enabled
    }

    fn service_name(&self) -> &str {
        &self.otel_service_name
    }

    fn otlp_endpoint(&self) -> &str {
        &self.otel_exporter_otlp_endpoint
    }

    fn otlp_headers(&self) -> &str {
        &self.otel_exporter_otlp_headers
    }

    fn otlp_protocol(&self) -> &str {
        &self.otel_exporter_otlp_protocol
    }

    fn span_processor(&self) -> &str {
        &self.otel_span_processor
    }

    fn traces_sampler(&self) -> &str {
        &self.otel_traces_sampler
    }

    fn traces_sampler_arg(&self) -> &str {
        &self.otel_traces_sampler_arg
    }

    fn deployment_environment(&self) -> &str {
        &self.otel_deployment_environment
    }
}

// --- gap 02: send_telemetry product-analytics surface (always available) ----
//
// `cognee_telemetry::{send_telemetry, try_send_telemetry, TelemetryError,
// UserIdRef, PropertyValue}` are exported by the leaf crate in BOTH
// feature states (the leaf crate switches between real and noop bodies
// internally, but the symbols are stable). Re-export them unconditionally
// so callers compile under `--no-default-features` and can name
// `cognee::telemetry::TelemetryError`.

pub use cognee_telemetry::{
    PropertyValue, TelemetryError, UserIdRef, send_telemetry, try_send_telemetry,
};

// --- process-wide product-analytics switch (always available) ---------------
//
// Product analytics are process config: one `TelemetryConfig` governs every
// event the process sends. Env (`TELEMETRY_DISABLED`, `ENV=test|dev`, ...) is
// the default, read when an event is sent; `configure` / `set_enabled` from
// code win over it, `reset_to_env` drops them. Without the
// `telemetry` feature these still exist and `is_enabled()` is always `false`.
pub use cognee_telemetry::{
    LlmKeyRegistration, TelemetryConfig, configure, current_config, flush, is_enabled,
    reset_to_env, set_enabled, set_llm_api_key,
};

#[cfg(feature = "telemetry")]
pub use cognee_telemetry::{env, ids, payload, sanitize};
