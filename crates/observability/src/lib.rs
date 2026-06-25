//! # cognee-observability
//!
//! OpenTelemetry tracing pipeline for Cognee-Rust. Bridges the existing
//! `tracing` instrumentation (60+ `#[tracing::instrument]` sites across
//! the workspace) into an OTLP exporter so spans flow to a collector.
//!
//! This crate is the single home for OTEL configuration, OTLP exporter
//! construction, the `tracing-opentelemetry` bridge layer, and the RAII
//! [`TelemetryGuard`] that flushes pending spans on drop.
//!
//! ## Activation
//!
//! Tracing is activated when **either** of:
//! - `Settings.cognee_tracing_enabled == true`
//!   (env: `COGNEE_TRACING_ENABLED=true`)
//! - `Settings.otel_exporter_otlp_endpoint` is non-empty
//!   (env: `OTEL_EXPORTER_OTLP_ENDPOINT=https://...`)
//!
//! Either path triggers the same provider setup. This mirrors Python's
//! `is_tracing_enabled()` lazy-init semantics in
//! `cognee/modules/observability/trace_context.py`.
//!
//! ## Programmatic init
//!
//! Drive [`init_telemetry`] from a [`SettingsView`] implementation. The
//! [`EnvSettingsView`] adapter reads the standard env vars directly, so
//! callers that don't want to depend on `cognee-lib` can still bring up
//! the pipeline:
//!
//! ```ignore
//! use cognee_observability::{init_telemetry, EnvSettingsView, TelemetryGuard};
//! use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, Registry};
//!
//! let settings = EnvSettingsView::from_env();
//!
//! let (otel_layer, guard): (_, TelemetryGuard) =
//!     init_telemetry::<Registry>(&settings).expect("telemetry init");
//!
//! Registry::default()
//!     .with(otel_layer)
//!     .with(tracing_subscriber::EnvFilter::from_default_env())
//!     .with(tracing_subscriber::fmt::layer())
//!     .init();
//!
//! // Hold `guard` for the lifetime of your process; dropping it
//! // calls `force_flush()` then `shutdown()` on the OTEL provider.
//! drop(guard);
//! ```
//!
//! Embedders that already use `cognee_lib::config::Settings` can pass it
//! directly — `Settings` implements [`SettingsView`].
//!
//! ## Configuration
//!
//! See [`docs/observability/opentelemetry.md`](https://github.com/topoteretes/cognee-rs/blob/main/docs/observability/opentelemetry.md)
//! for the full env-var reference and deployment recipes (Tempo, Honeycomb,
//! Dash0, in-cluster Collector).
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
