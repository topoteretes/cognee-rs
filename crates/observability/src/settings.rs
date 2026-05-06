//! Read-only view of the observability-relevant subset of `Settings`.
//!
//! Defined here (not in `cognee-lib`) to avoid a hard dependency on the
//! umbrella crate. `cognee-lib::Settings` implements this trait in a
//! sibling task so HTTP middleware and other upstream callers can drive
//! [`crate::init_telemetry`] without going through `cognee-lib`.

/// Borrow-only adapter over the OTEL fields of cognee `Settings`.
///
/// All accessors return `&str` / `bool` so implementations can avoid
/// cloning. `Send + Sync` so the trait object can travel across async
/// task boundaries.
pub trait SettingsView: Send + Sync {
    /// Mirrors `Settings.cognee_tracing_enabled`.
    fn tracing_enabled(&self) -> bool;
    /// Mirrors `Settings.otel_service_name`.
    fn service_name(&self) -> &str;
    /// Mirrors `Settings.otel_exporter_otlp_endpoint`.
    fn otlp_endpoint(&self) -> &str;
    /// Mirrors `Settings.otel_exporter_otlp_headers`.
    fn otlp_headers(&self) -> &str;
    /// Mirrors `Settings.otel_exporter_otlp_protocol`.
    fn otlp_protocol(&self) -> &str;
    /// Mirrors `Settings.otel_span_processor`.
    fn span_processor(&self) -> &str;
    /// Mirrors `Settings.otel_traces_sampler`.
    fn traces_sampler(&self) -> &str;
    /// Mirrors `Settings.otel_traces_sampler_arg`.
    fn traces_sampler_arg(&self) -> &str;
}
