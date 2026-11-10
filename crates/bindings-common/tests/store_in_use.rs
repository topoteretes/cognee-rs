#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Two live handles on one set of storage directories in one process.
//!
//! The embedded graph (Ladybug) cannot detect a second open of its file from
//! the same process — its lock is a process-owned POSIX record lock — and two
//! instances on one file destroy each other's writes. So the adapter claims
//! the path, and the second handle's first operation must fail with the
//! binding code `STORE_IN_USE`, naming the path, instead of opening next to
//! the first. Closing the first handle frees the path.
#![cfg(feature = "ladybug")]

use std::path::Path;
use std::sync::Arc;

use cognee::config::Settings;
use cognee_bindings_common::HandleState;

/// A hermetic handle with every store under `dir` (see `handle_close.rs`).
fn handle_under(dir: &Path) -> Arc<HandleState> {
    let settings = Settings {
        sections: [
            (
                "relational.url".to_string(),
                serde_json::json!(format!(
                    "sqlite://{}?mode=rwc",
                    dir.join("cognee.db").display()
                )),
            ),
            (
                "embedding.provider".to_string(),
                serde_json::json!("mock".to_owned()),
            ),
            (
                "llm.api_key".to_string(),
                serde_json::json!("sk-test".to_owned()),
            ),
        ]
        .into(),
        data_root_directory: dir.join("data").to_string_lossy().into_owned(),
        system_root_directory: dir.join("sys").to_string_lossy().into_owned(),
        ..Settings::default()
    };
    Arc::new(HandleState::from_settings(settings))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_live_handle_on_one_store_fails_with_store_in_use() {
    let dir = tempfile::tempdir().unwrap();
    let first = handle_under(dir.path());
    first.services().await.expect("the first handle warms");

    let second = handle_under(dir.path());
    let err = match second.services().await {
        Ok(_) => panic!("a second live handle on the same graph must be refused"),
        Err(err) => err,
    };
    assert_eq!(err.code(), "STORE_IN_USE", "{err}");
    let message = err.detailed_message();
    assert!(message.contains("STORE_IN_USE"), "{message}");
    assert!(
        message.contains("sys"),
        "the message names the path: {message}"
    );

    // Closing the first handle releases the store; the second warms.
    first.close().await;
    second
        .services()
        .await
        .expect("the store is free once the first handle is closed");
    second.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn handles_on_different_stores_coexist() {
    let a_dir = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let a = handle_under(a_dir.path());
    let b = handle_under(b_dir.path());
    a.services().await.unwrap();
    b.services().await.unwrap();
    a.close().await;
    b.close().await;
}
