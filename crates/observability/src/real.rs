//! Real OTEL bring-up. Body lands in task 04.

use crate::{TelemetryGuard, TelemetryInitError, TelemetrySettings};

pub(crate) fn init(
    _settings: &TelemetrySettings,
) -> Result<TelemetryGuard, TelemetryInitError> {
    Ok(TelemetryGuard::noop())
}
