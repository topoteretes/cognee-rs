//! RAII handle returned by [`crate::init_otel`].

/// RAII handle that flushes and shuts down the global tracer provider on
/// drop.
///
/// Holding the guard for the lifetime of `main()` (CLI) or for as long as
/// `AppState` is alive (HTTP server) ensures the final batch of spans is
/// exported before the process exits.
///
/// The real `Drop` body lands in task 04; today this is a noop placeholder
/// that lets dependent crates compile.
#[must_use = "TelemetryGuard must be held for the lifetime of the process to flush spans on shutdown"]
pub struct TelemetryGuard {
    _private: (),
}

impl TelemetryGuard {
    /// Construct a noop guard. Used by the `not(feature = "telemetry")`
    /// branch and by tests.
    pub(crate) fn noop() -> Self {
        Self { _private: () }
    }
}
