//! Telemetry surface for embedders.
//!
//! Re-exports the public API of [`cognee_observability`] so that
//! consumers reach OTEL setup through the same `cognee_lib::<topic>`
//! pattern used for `storage`, `vector`, `graph`, etc.
//!
//! This module is only compiled when the `telemetry` cargo feature is
//! enabled on `cognee-lib`. With the feature off, the module does not
//! exist and `cognee-observability` is not linked into the build graph.

pub use cognee_observability::{
    BoxedTelemetryLayer, SettingsView, TelemetryGuard, TelemetryInitError, already_instrumented,
    init_telemetry, is_tracing_enabled, parse_otlp_headers,
};

use crate::config::Settings;

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
}
