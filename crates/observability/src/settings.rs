//! Input struct for [`crate::init_telemetry`].
//!
//! Defined here (rather than re-using `cognee_lib::config::Settings`) so
//! that this crate sits at the bottom of the workspace dependency graph
//! and does not pull in `cognee-lib`. `cognee-lib` constructs a
//! `TelemetrySettings` from its own `Settings` in task 05.

/// Subset of cognee settings required to initialize OpenTelemetry.
///
/// Keeping this struct minimal lets us evolve `cognee-lib::Settings`
/// without breaking the observability ABI.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct TelemetrySettings {
    /// Mirrors `Settings.cognee_tracing_enabled`.
    pub tracing_enabled: bool,
    /// Mirrors `Settings.otel_service_name`.
    pub service_name: String,
    /// Mirrors `Settings.otel_exporter_otlp_endpoint`.
    pub exporter_otlp_endpoint: String,
    /// Mirrors `Settings.otel_exporter_otlp_headers`.
    pub exporter_otlp_headers: String,
}
