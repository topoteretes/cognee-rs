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

use once_cell::sync::Lazy;
use reqwest::Client;
use std::time::Duration;

use crate::env::request_timeout_secs;

#[allow(
    clippy::expect_used,
    reason = "reqwest ClientBuilder with default params cannot fail"
)]
static HTTP: Lazy<Client> = Lazy::new(|| {
    Client::builder()
        .timeout(Duration::from_secs(request_timeout_secs()))
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
///
/// The `Lazy` initialiser reads `request_timeout_secs()` **once** at
/// first use. Changing `TELEMETRY_REQUEST_TIMEOUT` mid-process does
/// not re-affect already-built clients — same as Python, where the
/// timeout is read once via `int(os.getenv(...))`.
pub fn client() -> &'static Client {
    &HTTP
}
