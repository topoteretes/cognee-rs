//! Real OTEL bring-up. Body lands in task 04.

use crate::{OtelInitError, OtelSettings, TelemetryGuard};

pub(crate) fn init(_settings: &OtelSettings) -> Result<TelemetryGuard, OtelInitError> {
    Ok(TelemetryGuard::noop())
}
