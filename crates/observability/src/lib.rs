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
//!   `opentelemetry-semantic-conventions`, `tracing-opentelemetry`,
//!   plus `tonic` and `http` for gRPC metadata construction. When
//!   enabled, [`init_telemetry`] builds a real `SdkTracerProvider`,
//!   installs it globally, and returns a guard that flushes on drop.
//!   When disabled, [`init_telemetry`] still compiles but returns an
//!   identity tracing layer plus a noop guard, so embedders can call it
//!   unconditionally.
//!
//! ## Feature-state contract
//!
//! [`init_telemetry`] returns `Ok((noop_layer, TelemetryGuard::noop()))`
//! whenever the process is not configured to export spans — specifically
//! when **either** (1) the `telemetry` cargo feature is **off** at compile
//! time, **or** (2) [`is_tracing_enabled`] returns `false` at runtime
//! (`COGNEE_TRACING_ENABLED` is not truthy and
//! `OTEL_EXPORTER_OTLP_ENDPOINT` is empty). On both paths the returned
//! layer is a boxed [`tracing_subscriber::layer::Identity`] that observes
//! nothing, and the guard's `Drop` runs no code. [`TelemetryGuard::noop`]
//! is publicly constructible for tests and embedders that want the same
//! shape without going through [`init_telemetry`]. This mirrors parent
//! [decision 6](../../../docs/telemetry/01-otel-otlp-export.md#design-decisions-locked)
//! (implicit activation: an endpoint alone is enough to opt in).

#![deny(missing_docs)]

mod guard;
mod headers;
mod init;
mod settings;

#[cfg(feature = "telemetry")]
mod error;

pub use guard::TelemetryGuard;
pub use headers::parse_otlp_headers;
pub use init::{BoxedTelemetryLayer, already_instrumented, init_telemetry, is_tracing_enabled};
pub use settings::{EnvSettingsView, SettingsView};

#[cfg(feature = "telemetry")]
pub use error::TelemetryInitError;

/// Stub `TelemetryInitError` exposed when the `telemetry` feature is off
/// so that the public signature of [`init_telemetry`] does not change
/// shape between builds. The variant is unreachable in practice — the
/// noop path always returns `Ok`.
#[cfg(not(feature = "telemetry"))]
#[derive(Debug, thiserror::Error)]
pub enum TelemetryInitError {
    /// Placeholder ensuring the enum is non-empty without the feature.
    #[error("cognee-observability built without `telemetry` feature")]
    FeatureDisabled,
}
