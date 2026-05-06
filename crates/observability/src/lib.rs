//! OpenTelemetry SDK bring-up and `tracing` bridge for cognee.
//!
//! This crate is the single home for OTEL configuration, OTLP exporter
//! construction, the `tracing-opentelemetry` bridge layer, and the RAII
//! [`TelemetryGuard`] that flushes pending spans on drop.
//!
//! ## Feature flags
//!
//! - `telemetry` (off by default) — pulls in `opentelemetry`,
//!   `opentelemetry_sdk`, `opentelemetry-otlp`,
//!   `opentelemetry-semantic-conventions`, and `tracing-opentelemetry`.
//!   When enabled, [`init_telemetry`] builds a real `SdkTracerProvider`,
//!   installs it globally, and returns a guard that flushes on drop.
//!   When disabled, [`init_telemetry`] still compiles but returns an
//!   identity tracing layer plus a noop guard, so embedders can call it
//!   unconditionally.

#![deny(missing_docs)]

mod error;
mod guard;
pub mod settings;

#[cfg(feature = "telemetry")]
mod real;

#[cfg(not(feature = "telemetry"))]
mod noop;

pub use error::TelemetryInitError;
pub use guard::TelemetryGuard;
pub use settings::TelemetrySettings;

/// Initialize OpenTelemetry tracing for the current process.
pub fn init_telemetry(
    _settings: &TelemetrySettings,
) -> Result<TelemetryGuard, TelemetryInitError> {
    #[cfg(feature = "telemetry")]
    {
        real::init(_settings)
    }
    #[cfg(not(feature = "telemetry"))]
    {
        noop::init(_settings)
    }
}
