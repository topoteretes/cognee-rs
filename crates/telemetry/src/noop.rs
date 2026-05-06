//! Noop (`feature = "telemetry"` off) implementation of
//! `send_telemetry`. Body lands in
//! `docs/telemetry/02/06-public-api-and-noop.md`.

pub(crate) fn send_telemetry_impl() {
    // No-op. Compiled when the `telemetry` feature is disabled.
}
