//! Noop fallback used when the `telemetry` feature is disabled. Final
//! body lands in task 08.

use crate::{TelemetryGuard, TelemetryInitError, TelemetrySettings};

pub(crate) fn init(
    _settings: &TelemetrySettings,
) -> Result<TelemetryGuard, TelemetryInitError> {
    Ok(TelemetryGuard::noop())
}
