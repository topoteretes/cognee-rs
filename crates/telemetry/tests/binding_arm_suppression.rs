//! Wire-level proof of the binding-arm / `COGNEE_HOST_SDK` interaction
//! that the language bindings (PyO3, Neon, C-API) rely on.
//!
//! The per-binding policy tests (`python/tests`, `ts/__tests__`,
//! `capi/examples/init_telemetry_smoke.c`) only assert the *returned*
//! armed/not-armed bool. This test proves the actual behaviour on the
//! HTTP wire against a mockito server:
//!
//!   * Before arming, a host SDK (`COGNEE_HOST_SDK`) does NOT suppress (decision 10:
//!     the sentinel scopes to binding-armed emitters only) — a POST
//!     still fires.
//!   * After `arm_binding_emission()`, `COGNEE_HOST_SDK` suppresses —
//!     zero POSTs.
//!   * With the sentinel cleared, an armed process emits again.
//!
//! All traffic stays on 127.0.0.1; the live proxy is never contacted.
//! `arm_binding_emission()` sets a permanent process-global flag, so
//! this lives in its own test binary and runs as a single ordered
//! `#[serial]` test with forward-only phases.

#![cfg(feature = "telemetry")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
#![allow(
    clippy::disallowed_methods,
    reason = "tests drive configuration through the process env"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use mockito::Server;
use serial_test::serial;
use tempfile::TempDir;

use cognee_telemetry::env::{arm_binding_emission, is_binding_armed};
use cognee_telemetry::{TelemetryConfig, configure, send_telemetry};

struct IsolatedEnv {
    _home: TempDir,
    prev_home: Option<String>,
}

impl IsolatedEnv {
    fn install(server_url: &str) -> Self {
        let home = TempDir::new().expect("tempdir");
        let prev_home = std::env::var("HOME").ok();
        // SAFETY: Rust 2024 makes set_var unsafe; `#[serial]` orders this
        //   against every other env-mutating test in the binary.
        unsafe {
            std::env::set_var("HOME", home.path());
        }
        configure(TelemetryConfig {
            enabled: true,
            tracking_id: Some("fixed-anon-arm-test".into()),
            endpoint_override: Some(server_url.to_string()),
            ..TelemetryConfig::default()
        });
        cognee_telemetry::ids::__test_only_reset_caches();
        Self {
            _home: home,
            prev_home,
        }
    }
}

impl Drop for IsolatedEnv {
    fn drop(&mut self) {
        // SAFETY: same serial section as `install`.
        unsafe {
            match &self.prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
        cognee_telemetry::reset_to_env();
    }
}

/// Set (or clear) the host-SDK sentinel, keeping the rest of the config.
fn set_host_sdk(host: Option<&str>) {
    configure(TelemetryConfig {
        host_sdk: host.map(str::to_string),
        ..cognee_telemetry::current_config()
    });
}

/// Fire one event and return how many new POSTs landed within the
/// window. `expect_fire` selects the wait strategy: when a hit is
/// expected we poll up to 5s; when none is expected we wait a fixed
/// generous window so a late dispatch cannot sneak past undetected.
async fn delta_after_send(hits: &Arc<AtomicUsize>, expect_fire: bool) -> usize {
    let before = hits.load(Ordering::SeqCst);
    send_telemetry("cognee.test.arm", "user", None);
    if expect_fire {
        let start = tokio::time::Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            if hits.load(Ordering::SeqCst) > before {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    } else {
        tokio::time::sleep(Duration::from_millis(750)).await;
    }
    hits.load(Ordering::SeqCst) - before
}

#[tokio::test]
#[serial]
async fn host_sdk_suppresses_only_when_binding_armed() {
    let mut server = Server::new_async().await;
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_cb = Arc::clone(&hits);

    // A permissive mock: 200 for every POST, counting hits ourselves so
    // we are not bound to mockito's single-hit default expectation.
    let _mock = server
        .mock("POST", "/")
        .with_status(200)
        .expect_at_least(0)
        .with_body_from_request(move |_req| {
            hits_cb.fetch_add(1, Ordering::SeqCst);
            Vec::new()
        })
        .create_async()
        .await;

    let _env = IsolatedEnv::install(&server.url());

    // Phase A — NOT yet armed, COGNEE_HOST_SDK set: must still emit
    // (decision 10 — the sentinel only scopes to binding-armed emitters).
    assert!(!is_binding_armed(), "precondition: process starts unarmed");
    set_host_sdk(Some("python"));
    assert_eq!(
        delta_after_send(&hits, true).await,
        1,
        "unarmed + COGNEE_HOST_SDK should NOT suppress"
    );

    // Phase B — arm, COGNEE_HOST_SDK still set: must suppress.
    arm_binding_emission();
    assert!(is_binding_armed());
    assert_eq!(
        delta_after_send(&hits, false).await,
        0,
        "armed + COGNEE_HOST_SDK must suppress emission"
    );

    // Phase C — armed, sentinel cleared: emits again.
    set_host_sdk(None);
    assert_eq!(
        delta_after_send(&hits, true).await,
        1,
        "armed without COGNEE_HOST_SDK should emit"
    );
}
