//! Errors surfaced by [`crate::init_telemetry`].

use thiserror::Error;

/// Errors returned during OpenTelemetry SDK initialization.
///
/// Variants will be filled in by task 04.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TelemetryInitError {
    /// Placeholder so the enum is non-empty until task 04 lands real
    /// variants (exporter build failures, header parse errors, etc.).
    #[error("OTEL initialization not yet implemented")]
    NotImplemented,
}
