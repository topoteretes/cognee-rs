//! RAII guard that flushes and shuts down the OTEL pipeline on drop.
//!
//! The guard always exists, even when telemetry is disabled at compile
//! time (via the absent `telemetry` feature) or at runtime (no endpoint
//! configured). The disabled variant is a no-op so callers do not need
//! cfg-gating around the call site.

use std::time::Duration;

#[cfg(feature = "telemetry")]
use opentelemetry_sdk::trace::SdkTracerProvider;

const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// RAII handle that flushes and shuts down the global tracer provider on
/// drop.
///
/// Holding the guard for the lifetime of `main()` (CLI) or for as long as
/// `AppState` is alive (HTTP server) ensures the final batch of spans is
/// exported before the process exits.
#[must_use = "TelemetryGuard must be held for the lifetime of the process to flush spans on shutdown"]
pub struct TelemetryGuard {
    #[cfg(feature = "telemetry")]
    provider: Option<SdkTracerProvider>,
    timeout: Duration,
}

impl TelemetryGuard {
    /// Construct a noop guard. Drop is free.
    pub fn noop() -> Self {
        Self {
            #[cfg(feature = "telemetry")]
            provider: None,
            timeout: DEFAULT_SHUTDOWN_TIMEOUT,
        }
    }

    #[cfg(feature = "telemetry")]
    pub(crate) fn from_provider(provider: SdkTracerProvider) -> Self {
        Self {
            provider: Some(provider),
            timeout: DEFAULT_SHUTDOWN_TIMEOUT,
        }
    }

    /// Override the flush+shutdown budget (mostly useful in tests).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Test-only inspector: returns `true` when an SDK provider is held.
    #[cfg(all(feature = "telemetry", any(test, debug_assertions)))]
    pub fn has_provider(&self) -> bool {
        self.provider.is_some()
    }

    /// Test-only inspector: always `false` without `telemetry`.
    #[cfg(all(not(feature = "telemetry"), any(test, debug_assertions)))]
    pub fn has_provider(&self) -> bool {
        false
    }
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        #[cfg(feature = "telemetry")]
        {
            if let Some(provider) = self.provider.take() {
                if let Err(err) = provider.force_flush() {
                    tracing::warn!(
                        target: "cognee.observability",
                        ?err,
                        "OTEL force_flush failed during TelemetryGuard drop"
                    );
                }
                if let Err(err) = provider.shutdown_with_timeout(self.timeout) {
                    tracing::warn!(
                        target: "cognee.observability",
                        ?err,
                        "OTEL shutdown_with_timeout failed during TelemetryGuard drop"
                    );
                }
            }
        }
        // Without `telemetry`, dropping is free; the timeout field is
        // retained for signature stability.
        let _ = self.timeout;
    }
}
