//! End-to-end integration tests for `serve()` / `disconnect()`.
//!
//! These tests exercise the full round-trip through the process-wide
//! [`state`] singleton and the on-disk credential cache. `$HOME` is
//! overridden for the duration of each test so credential writes land in
//! a temp dir rather than the developer's real home.
//!
//! All tests hold a shared mutex because they mutate process-global
//! env-table + singleton state. `cargo test -p cognee-cloud` already runs
//! with `--test-threads=1` under the project harness, but the mutex keeps
//! them safe under `cargo test` invoked directly too.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test code — panics are acceptable failures"
)]

use std::path::Path;
use std::sync::{Arc, Mutex};

use cognee_cloud::credentials::{self as creds_mod, CloudCredentials};
use cognee_cloud::state::{clear_client, get_client, is_connected};
use cognee_cloud::{CloudClient, ServeConfig, disconnect, serve, serve_url};

// Integration tests share the same $HOME env var and CLOUD_CLIENT
// singleton. Serialise them even when the harness runs them in parallel.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Run `body` with `$HOME` pointed at `tmp`, holding [`ENV_LOCK`] for the
/// duration. Restores the previous `$HOME` (or removes it) on exit.
fn with_isolated_home<F, Fut>(tmp: &Path, body: F)
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let prev_home = std::env::var("HOME").ok();
    let prev_service_url = std::env::var("COGNEE_SERVICE_URL").ok();
    let prev_api_key = std::env::var("COGNEE_API_KEY").ok();

    // SAFETY: we hold ENV_LOCK for the full scope; no other integration
    // test can race us on the env table while this is running.
    unsafe {
        std::env::set_var("HOME", tmp);
        std::env::remove_var("COGNEE_SERVICE_URL");
        std::env::remove_var("COGNEE_API_KEY");
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build current-thread runtime for integration test");

    rt.block_on(async {
        clear_client().await;
        body().await;
        clear_client().await;
    });

    unsafe {
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match prev_service_url {
            Some(v) => std::env::set_var("COGNEE_SERVICE_URL", v),
            None => std::env::remove_var("COGNEE_SERVICE_URL"),
        }
        match prev_api_key {
            Some(v) => std::env::set_var("COGNEE_API_KEY", v),
            None => std::env::remove_var("COGNEE_API_KEY"),
        }
    }
}

#[test]
fn serve_direct_sets_client_in_state() {
    let tmp = tempfile::tempdir().expect("tempdir");
    with_isolated_home(tmp.path(), || async {
        let mut server = mockito::Server::new_async().await;
        let _health = server
            .mock("GET", "/health")
            .with_status(200)
            .create_async()
            .await;

        let cfg = ServeConfig::direct(server.url()).api_key("ck_test");
        let client = serve(cfg).await.expect("serve direct ok");
        assert_eq!(client.api_key, "ck_test");
        assert_eq!(client.service_url, server.url().trim_end_matches('/'));

        assert!(is_connected().await, "singleton must be populated");
        let installed = get_client().await.expect("client installed");
        assert!(
            Arc::ptr_eq(&installed, &client),
            "singleton must hold the returned Arc"
        );
    });
}

#[test]
fn disconnect_clears_client() {
    let tmp = tempfile::tempdir().expect("tempdir");
    with_isolated_home(tmp.path(), || async {
        let mut server = mockito::Server::new_async().await;
        let _health = server
            .mock("GET", "/health")
            .with_status(200)
            .create_async()
            .await;

        serve_url(server.url(), Some("ck_test"))
            .await
            .expect("serve ok");
        assert!(is_connected().await);

        disconnect(false).await.expect("disconnect ok");
        assert!(!is_connected().await, "client should be cleared");
        assert!(
            get_client().await.is_none(),
            "get_client must return None after disconnect"
        );
    });
}

#[test]
fn disconnect_wipe_credentials_removes_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    with_isolated_home(tmp.path(), || async {
        // Seed a credentials file via the real save() API.
        let creds = CloudCredentials {
            service_url: "http://example.com".into(),
            api_key: "ck_test".into(),
            email: "local".into(),
            ..CloudCredentials::default()
        };
        creds_mod::save(&creds).await.expect("seed creds");
        let path = creds_mod::credentials_path();
        assert!(path.exists(), "file should exist after seed");
        assert_eq!(
            path,
            tmp.path().join(".cognee").join("cloud_credentials.json"),
            "path must resolve relative to HOME"
        );

        // Install a client so the disconnect path runs through both
        // branches (clear singleton + wipe file).
        let client =
            CloudClient::new("http://example.com", "ck_test").expect("construct dummy client");
        cognee_cloud::state::set_client(client).await;
        assert!(is_connected().await);

        disconnect(true).await.expect("disconnect + wipe ok");
        assert!(!is_connected().await);
        assert!(!path.exists(), "credentials file should be removed");
    });
}

#[test]
fn serve_direct_health_check_failure_still_sets_client() {
    let tmp = tempfile::tempdir().expect("tempdir");
    with_isolated_home(tmp.path(), || async {
        // Mockito returns 500 on /health — serve must still succeed
        // (matches Python's `_serve_direct`: health-check failure is
        // logged as a warning but does not abort the connect).
        let mut server = mockito::Server::new_async().await;
        let _health = server
            .mock("GET", "/health")
            .with_status(500)
            .create_async()
            .await;

        let cfg = ServeConfig::direct(server.url()).api_key("ck_test");
        let client = serve(cfg).await.expect("serve must succeed despite 500");
        assert_eq!(client.api_key, "ck_test");
        assert!(is_connected().await);
    });
}
