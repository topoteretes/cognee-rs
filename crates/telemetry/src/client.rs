//! Process-wide reqwest client for `send_telemetry`.
//!
//! A new `reqwest::Client` triggers rustls trust-store load and
//! certificate parsing on every call. With telemetry firing from API
//! endpoints, that overhead would dominate latency. The standard fix
//! is a process-wide singleton via `once_cell::sync::Lazy`.
//!
//! Pool sizing is intentionally small (2 idle per host) because the
//! proxy is a single host and large pools waste sockets in
//! long-running CLI processes.
//!
//! The client carries no timeout of its own: each request sets the timeout of
//! the process [`crate::TelemetryConfig`] in force when it is sent, so a
//! [`crate::configure`] call takes effect without rebuilding the client.

use once_cell::sync::Lazy;
use reqwest::Client;

#[allow(
    clippy::expect_used,
    reason = "reqwest ClientBuilder with default params cannot fail"
)]
static HTTP: Lazy<Client> = Lazy::new(|| {
    Client::builder()
        .pool_max_idle_per_host(2)
        .user_agent(concat!(
            "cognee-rust/",
            env!("CARGO_PKG_VERSION"),
            " (send_telemetry)"
        ))
        .build()
        .expect("reqwest client builder cannot fail with default params")
});

/// Borrow the process-wide reqwest client.
pub fn client() -> &'static Client {
    &HTTP
}
