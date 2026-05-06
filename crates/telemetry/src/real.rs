//! Real (`feature = "telemetry"`) implementation of `send_telemetry`.
//! Body lands in `docs/telemetry/02/05-client-dispatch-and-optout.md`.

use crate::{PropertyValue, UserIdRef};

pub(crate) fn send_telemetry_impl(
    _event_name: &str,
    _user_id: UserIdRef<'_>,
    _additional_properties: Option<PropertyValue>,
) {
    // Stub — replaced in task 02-05.
}
