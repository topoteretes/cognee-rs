//! Harness-only Rust telemetry driver for the cross-SDK parity test.
//!
//! Calls `cognee::telemetry::send_telemetry("cognee.forget", ...)`
//! once with fixed args, then waits briefly so the detached dispatch
//! task finishes its POST before the process exits. Captured by the
//! Python proxy at `COGNEE_TELEMETRY_PROXY_URL_FOR_TESTS`.
//!
//! Honours decisions 2 (proxy URL override via the integration-test
//! env), 11 (the LLM API key hashed at emission time) and 12
//! (`TELEMETRY_API_KEY_TRACKING_SALT` override accepted by the identity
//! helpers when set).
//!
//! Product analytics hash the *resolved* LLM key, registered with
//! `set_llm_api_key` — `ComponentManager` does that for every handle. This
//! binary calls `send_telemetry` without a manager, so it resolves `Settings`
//! from the environment (as the CLI and bindings do) and registers the key
//! itself; `LLM_API_KEY` therefore still drives the parity comparison.
//!
//! See docs/telemetry/02/10-cross-sdk-parity.md §4.3.

use cognee::telemetry;
use std::time::Duration;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let settings = cognee::config::Settings::load_from_env();
    telemetry::set_llm_api_key(Some(&settings.llm().api_key));

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
