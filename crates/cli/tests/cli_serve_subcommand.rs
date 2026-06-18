#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! CLI tests for the `serve` and `disconnect` subcommands.
//!
//! The subcommands are only present when the `cloud` feature is enabled,
//! which it is by default. Tests that exercise a live `serve` path spin up
//! a `mockito` HTTP server for `/health` so no real Cognee instance is
//! required.
//!
//! Each test uses a temp dir for `HOME` so on-disk credential writes do
//! not touch the developer's real `~/.cognee/`.

#![cfg(feature = "cloud")]

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;
use tempfile::TempDir;

fn make_cmd(home: &Path) -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("cognee-cli"));
    command.env("HOME", home);
    // Use the same tempdir for XDG so `config` reads/writes do not leak.
    command.env("XDG_CONFIG_HOME", home);
    // Make sure the process starts from a clean slate — no inherited
    // COGNEE_SERVICE_URL / COGNEE_API_KEY overrides from the dev shell.
    command.env_remove("COGNEE_SERVICE_URL");
    command.env_remove("COGNEE_API_KEY");
    command
}

#[test]
fn serve_help_lists_subcommand() {
    let home = TempDir::new().expect("tempdir");
    make_cmd(home.path())
        .arg("serve")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--url"))
        .stdout(predicate::str::contains("--api-key"));
}

#[test]
fn disconnect_help_lists_subcommand() {
    let home = TempDir::new().expect("tempdir");
    make_cmd(home.path())
        .arg("disconnect")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--wipe-credentials"));
}

#[test]
fn serve_direct_url_succeeds_with_health_check_ok() {
    // Spin up a mockito server with /health → 200.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");
    let (server_url, _keep_alive) = rt.block_on(async {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("GET", "/health")
            .with_status(200)
            .create_async()
            .await;
        // Return the URL and keep the server alive for the spawned CLI's
        // lifetime by holding the server in the test thread.
        (server.url(), server)
    });

    let home = TempDir::new().expect("tempdir");
    make_cmd(home.path())
        .arg("serve")
        .arg("--url")
        .arg(&server_url)
        .arg("--api-key")
        .arg("ck_test")
        .assert()
        .success()
        .stdout(predicate::str::contains("Connected to Cognee"));

    // Verify the credentials file was written under the temp HOME.
    let creds_path = home.path().join(".cognee").join("cloud_credentials.json");
    assert!(
        creds_path.exists(),
        "serve must persist credentials under $HOME"
    );
}

#[test]
fn disconnect_with_no_state_is_noop_success() {
    let home = TempDir::new().expect("tempdir");
    make_cmd(home.path()).arg("disconnect").assert().success();
}

#[test]
fn disconnect_wipe_credentials_removes_pre_seeded_file() {
    let home = TempDir::new().expect("tempdir");
    // Pre-seed a credentials file — the content doesn't have to validate
    // because `disconnect` only deletes the file, never parses it.
    let creds_dir = home.path().join(".cognee");
    std::fs::create_dir_all(&creds_dir).expect("mkdir .cognee");
    let creds_path = creds_dir.join("cloud_credentials.json");
    std::fs::write(
        &creds_path,
        br#"{"access_token":"","refresh_token":null,"expires_at":0.0,"service_url":"","api_key":"","management_url":"","tenant_id":"","tenant_name":"","email":""}"#,
    )
    .expect("seed creds file");
    assert!(creds_path.exists());

    make_cmd(home.path())
        .arg("disconnect")
        .arg("--wipe-credentials")
        .assert()
        .success();

    assert!(
        !creds_path.exists(),
        "disconnect --wipe-credentials must delete the creds file"
    );
}
