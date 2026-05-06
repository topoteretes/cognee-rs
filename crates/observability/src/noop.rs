//! Noop fallback used when the `telemetry` feature is disabled. Final
//! body lands in task 08.

use crate::{OtelInitError, OtelSettings, TelemetryGuard};

pub(crate) fn init(_settings: &OtelSettings) -> Result<TelemetryGuard, OtelInitError> {
    Ok(TelemetryGuard::noop())
}
