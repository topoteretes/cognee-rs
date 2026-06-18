#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests exercising the full `send_telemetry` dispatch
//! path against a mockito server. All HTTP traffic stays on
//! 127.0.0.1; the live proxy `https://test.prometh.ai` is NEVER
//! contacted from these tests.
//!
//! Soundness note: every test here is `#[serial]` and constructs an
//! `IsolatedEnv` that snapshots / restores the env vars touched. If a
//! test panics before the guard's `Drop` runs, `#[serial]` ensures the
//! next test still re-installs the same vars before observing them.

#![cfg(feature = "telemetry")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use mockito::Server;
use serde_json::Value;
use serial_test::serial;
use tempfile::TempDir;

use cognee_telemetry::send_telemetry;

/// Set of env vars touched by each test. Centralised so `IsolatedEnv`
/// is the single place that knows what to reset.
const ENV_VARS: &[&str] = &[
    "HOME",
    "TRACKING_ID",
    "LLM_API_KEY",
    "TELEMETRY_API_KEY_TRACKING_SALT",
    "TELEMETRY_DISABLED",
    "ENV",
    "TELEMETRY_REQUEST_TIMEOUT",
    "COGNEE_TELEMETRY_INTEGRATION_TEST",
    "COGNEE_TELEMETRY_PROXY_URL_FOR_TESTS",
];

/// Set up an isolated env: a temp HOME, a fixed TRACKING_ID, a fresh
/// LLM_API_KEY, and the mockito URL injected via the test override.
struct IsolatedEnv {
    _home: TempDir,
}

impl IsolatedEnv {
    fn install(server_url: &str) -> Self {
        let home = TempDir::new().expect("tempdir");
        // Workspace uses Rust edition 2024 where `set_var` /
        // `remove_var` are `unsafe`. `#[serial]` orders this against
        // every other env-mutating test in the binary.
        // SAFETY: serial section, no concurrent reader/writer of these
        //   vars while this body runs.
        unsafe {
            std::env::set_var("HOME", home.path());
            std::env::set_var("TRACKING_ID", "fixed-anon-12345");
            std::env::set_var("LLM_API_KEY", "sk-test-fixture");
            std::env::remove_var("TELEMETRY_API_KEY_TRACKING_SALT");
            std::env::remove_var("TELEMETRY_DISABLED");
            std::env::remove_var("ENV");
            std::env::remove_var("TELEMETRY_REQUEST_TIMEOUT");
            std::env::set_var("COGNEE_TELEMETRY_INTEGRATION_TEST", "1");
            std::env::set_var("COGNEE_TELEMETRY_PROXY_URL_FOR_TESTS", server_url);
        }
        // Wipe the persistent-id / anon-id caches so the new HOME
        // takes effect.
        cognee_telemetry::ids::__test_only_reset_caches();
        Self { _home: home }
    }
}

impl Drop for IsolatedEnv {
    fn drop(&mut self) {
        for k in ENV_VARS {
            // SAFETY: Drop runs inside the same `#[serial]` section as
            //   `install`, so no concurrent access exists.
            unsafe {
                std::env::remove_var(k);
            }
        }
    }
}

/// Wait up to `timeout` for `mock` to be hit at least once. Polls at
/// 25 ms intervals to keep flake low.
async fn wait_for_hit(mock: &mockito::Mock, timeout: Duration) -> bool {
    let start = tokio::time::Instant::now();
    while start.elapsed() < timeout {
        if mock.matched_async().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    false
}

#[tokio::test]
#[serial]
async fn schema_parity_against_reference() {
    let mut server = Server::new_async().await;

    // Capture the request body via `with_body_from_request`. mockito
    // 1.x does not expose a public `last_request()` accessor, so this
    // closure stashes the bytes for later inspection.
    let captured: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));
    let captured_for_cb = Arc::clone(&captured);

    let mock = server
        .mock("POST", "/")
        .with_status(200)
        .match_header("content-type", "application/json")
        .with_body_from_request(move |req| {
            if let Ok(body) = req.body() {
                // lock poison is unrecoverable
                *captured_for_cb.lock().unwrap() = Some(body.clone());
            }
            Vec::new()
        })
        .create_async()
        .await;
    let _env = IsolatedEnv::install(&server.url());

    send_telemetry(
        "cognee.forget",
        "user-id-string",
        Some(serde_json::json!({
            "target": "everything",
            "dataset": "",
            "data_id": "",
            "cognee_version": "0.1.0-test",
            "url": "https://example.com/private",
        })),
    );

    assert!(wait_for_hit(&mock, Duration::from_secs(5)).await);

    // Inspect the captured body.
    let body_bytes = {
        // lock poison is unrecoverable
        let guard = captured.lock().unwrap();
        guard.clone().expect("at least one request body captured")
    };
    let body: Value = serde_json::from_slice(&body_bytes).expect("json");

    // Top-level shape.
    assert_eq!(body["event_name"], "cognee.forget");
    assert_eq!(body["anonymous_id"], "fixed-anon-12345");

    // user_properties tuple.
    let up = &body["user_properties"];
    assert!(
        up["api_key_tracking_id"]
            .as_str()
            .expect("api_key_tracking_id is string")
            .starts_with("ak_")
    );
    assert_eq!(up["api_key_tracking_id"], up["api_key_hash"]);
    // persistent_id is non-empty. With TRACKING_ID set and a fresh
    // HOME, it is seeded from the anonymous id (current contract);
    // we only assert presence here. Length / uuid-shape is covered in
    // the unit tests under `ids_tests`.
    assert!(
        !up["persistent_id"]
            .as_str()
            .expect("persistent_id is string")
            .is_empty(),
        "persistent_id was empty"
    );

    // properties tuple, including the additional flatten.
    let p = &body["properties"];
    assert_eq!(p["sdk_runtime"], "rust");
    assert_eq!(p["target"], "everything");
    assert_eq!(p["cognee_version"], "0.1.0-test");

    // URL was sanitized via uuid5.
    let sanitized_url = p["url"].as_str().expect("url is string");
    assert!(
        uuid::Uuid::parse_str(sanitized_url).is_ok(),
        "expected uuid5, got {sanitized_url}"
    );
    assert_ne!(sanitized_url, "https://example.com/private");

    // time matches MM/DD/YYYY.
    let time_re = regex::Regex::new(r"^\d{2}/\d{2}/\d{4}$").expect("valid regex");
    assert!(
        time_re.is_match(p["time"].as_str().expect("time is string")),
        "unexpected time format: {}",
        p["time"]
    );

    mock.assert_async().await;
}

#[tokio::test]
#[serial]
async fn opt_out_via_telemetry_disabled() {
    let mut server = Server::new_async().await;
    let mock = server
        .mock("POST", "/")
        .with_status(200)
        .expect(0)
        .create_async()
        .await;
    let _env = IsolatedEnv::install(&server.url());
    // SAFETY: `#[serial]` orders this against every other env-mutating
    //   test; `_env`'s Drop will remove TELEMETRY_DISABLED on exit.
    unsafe {
        std::env::set_var("TELEMETRY_DISABLED", "1");
    }

    send_telemetry("cognee.forget", "user", None);

    // Wait a generous window to ensure no late dispatch sneaks
    // through.
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    mock.assert_async().await;
}

#[tokio::test]
#[serial]
async fn fire_and_forget_does_not_block_caller() {
    let mut server = Server::new_async().await;
    // Stall the response by sleeping in the chunked-body writer.
    // mockito 1.x exposes `with_chunked_body` for streaming callbacks.
    // The dispatcher is fire-and-forget so the caller must return
    // immediately regardless of how long the proxy takes.
    let mock = server
        .mock("POST", "/")
        .with_status(200)
        .with_chunked_body(|w| {
            std::thread::sleep(Duration::from_millis(2_000));
            w.write_all(b"{}")
        })
        .create_async()
        .await;
    let _env = IsolatedEnv::install(&server.url());
    // Clear LLM_API_KEY so the caller doesn't pay the PBKDF2 100k-iteration
    // cost (decision 11: hashed at every call, never cached). This test
    // measures the proxy-stall propagation only — body assembly cost is
    // covered elsewhere.
    // SAFETY: still inside the same `#[serial]` section as `_env`'s install.
    unsafe {
        std::env::remove_var("LLM_API_KEY");
    }

    let start = tokio::time::Instant::now();
    send_telemetry("cognee.forget", "user", None);
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_millis(100),
        "send_telemetry blocked the caller for {elapsed:?} \
         (proxy stalls 2s; caller should return immediately)"
    );
    // We don't care whether the request eventually completes — it's
    // fire-and-forget. The mockito assertion is intentionally not
    // checked here.
    let _ = mock;
}
