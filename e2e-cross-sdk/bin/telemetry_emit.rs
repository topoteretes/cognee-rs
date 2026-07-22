//! Harness-only Rust telemetry driver for the cross-SDK parity test.
//!
//! Calls `cognee::telemetry::send_telemetry("cognee.forget", ...)`
//! once with fixed args, then waits briefly so the detached dispatch
//! task finishes its POST before the process exits. Captured by the
//! Python proxy at `COGNEE_TELEMETRY_PROXY_URL_FOR_TESTS`.
//!
//! Honours decisions 2 (proxy URL override via the integration-test
//! env), 11 (`LLM_API_KEY` read at emission time inside `send_telemetry`),
//! and 12 (`COGNEE_TELEMETRY_API_KEY_SALT` override accepted by the
//! identity helpers when set). This binary itself does not touch
//! identity derivation — the leaf crate handles all of that.
//!
//! See docs/telemetry/02/10-cross-sdk-parity.md §4.3.

use cognee::telemetry;
use std::time::Duration;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let props = serde_json::json!({
        "target": "everything",
        "cognee_version": "cross-sdk-test",
    });

    telemetry::send_telemetry("cognee.forget", "cross-sdk-user", Some(props));

    // `send_telemetry` is fire-and-forget: it spawns a detached task on
    // the current runtime and returns immediately. Give that task a
    // bounded window to finish the HTTP POST against the in-cluster
    // mock proxy before tearing the runtime down. The cap matches the
    // default `request_timeout_secs()` (5s) plus a small slack.
    tokio::time::sleep(Duration::from_secs(6)).await;
}
