//! Errors surfaced by [`crate::init_telemetry`].
//!
//! Only compiled with the `telemetry` feature on. The crate root provides
//! a unit-variant stub for the noop build so signatures stay stable.

use thiserror::Error;

/// Errors returned during OpenTelemetry SDK initialization.
#[derive(Debug, Error)]
pub enum TelemetryInitError {
    /// The OTLP exporter builder failed (network configuration,
    /// TLS, malformed endpoint, etc.).
    #[error("OTLP exporter build failed: {0}")]
    ExporterBuild(#[source] opentelemetry_otlp::ExporterBuildError),

    /// `OTEL_EXPORTER_OTLP_PROTOCOL` carried an unrecognised value.
    #[error("unknown OTEL_EXPORTER_OTLP_PROTOCOL: {0} (expected `grpc` or `http/protobuf`)")]
    UnknownProtocol(String),

    /// `OTEL_SPAN_PROCESSOR` carried an unrecognised value.
    #[error("unknown OTEL_SPAN_PROCESSOR: {0} (expected `batch` or `simple`)")]
    UnknownSpanProcessor(String),

    /// `OTEL_TRACES_SAMPLER` carried an unrecognised value.
    #[error("unknown OTEL_TRACES_SAMPLER: {0}")]
    UnknownSampler(String),

    /// A ratio-based sampler was selected but no `OTEL_TRACES_SAMPLER_ARG`
    /// ratio was provided.
    #[error("OTEL_TRACES_SAMPLER_ARG required for ratio-based samplers")]
    SamplerArgRequired,

    /// `OTEL_TRACES_SAMPLER_ARG` was provided but is not a valid 0.0..=1.0 ratio.
    #[error("invalid OTEL_TRACES_SAMPLER_ARG: {0} (expected 0.0..=1.0)")]
    InvalidSamplerArg(String),
}
