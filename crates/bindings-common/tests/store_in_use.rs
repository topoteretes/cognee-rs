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

/// A config change after warm rebuilds every engine at the new version. The
/// rebuild must retire the handle's own stale embedded graph first instead of
/// meeting it as a second instance (which lbug would grant and the guard
/// refuses) — even for a key that has nothing to do with the store.
#[tokio::test(flavor = "multi_thread")]
async fn a_config_change_after_warm_rebuilds_onto_the_same_store() {
    let dir = tempfile::tempdir().unwrap();
    let state = handle_under(dir.path());
    let before = state.services().await.expect("first warm");
    drop(before);

    state
        .cm
        .config()
        .set("monitoring_tool", serde_json::json!("none"))
        .expect("a known key");
    let after = state
        .services()
        .await
        .expect("the rebuild retires the stale store and reopens it");
    after
        .graph_db
        .is_empty()
        .await
        .expect("the rebuilt graph serves");
    drop(after);
    state.close().await;
}

/// A config change while an operation still holds the services (a background
/// improve can hold them for a minute) rebuilds the bundle without failing
/// STORE_IN_USE: the unchanged store is kept and shared with the operation.
#[tokio::test(flavor = "multi_thread")]
async fn a_config_change_while_an_operation_holds_the_store_keeps_it() {
    let dir = tempfile::tempdir().unwrap();
    let state = handle_under(dir.path());
    let in_flight = state.services().await.expect("first warm");

    state
        .cm
        .config()
        .set("monitoring_tool", serde_json::json!("none"))
        .expect("a known key");
    let after = state
        .services()
        .await
        .expect("no STORE_IN_USE while the operation holds the store");
    assert!(
        std::sync::Arc::ptr_eq(&in_flight.graph_db, &after.graph_db),
        "the unchanged store is kept"
    );
    in_flight
        .graph_db
        .is_empty()
        .await
        .expect("the operation's store was not closed");
    drop((in_flight, after));
    state.close().await;
}

/// A store option change (same file) while an operation holds the services:
/// the open store keeps being served — no STORE_IN_USE, no second instance —
/// and the change applies once the operation lets go and the handle releases
/// the store.
#[tokio::test(flavor = "multi_thread")]
async fn a_store_change_applies_through_services_after_the_op_lets_go() {
    let dir = tempfile::tempdir().unwrap();
    let state = handle_under(dir.path());
    let in_flight = state.services().await.expect("first warm");
    let old = std::sync::Arc::downgrade(&in_flight.graph_db);

    state
        .cm
        .config()
        .set("graph.ladybug.max_db_size", serde_json::json!(1u64 << 33))
        .expect("a known key");
    let during = state.services().await.expect("no STORE_IN_USE");
    assert!(std::sync::Arc::ptr_eq(
        &during.graph_db,
        &in_flight.graph_db
    ));
    drop((during, in_flight));

    state.release().await;
    let after = state.services().await.expect("re-warm");
    assert!(
        old.upgrade().is_none(),
        "the old store was closed and dropped"
    );
    after.graph_db.is_empty().await.unwrap();
    drop(after);
    state.close().await;
}

/// A STORE_IN_USE raised mid-operation reaches the bindings inside another
/// error's message (`cognify failed: …`); it still carries its code, so Java
/// throws `STORE_IN_USE` and TypeScript a `StoreInUseError`.
#[test]
fn an_op_time_store_in_use_keeps_its_code() {
    let dir = tempfile::tempdir().unwrap();
    let _held = cognee_utils::StoreClaim::acquire(dir.path(), "ladybug graph").unwrap();
    let in_use = cognee_utils::StoreClaim::acquire(dir.path(), "ladybug graph").unwrap_err();
    let err = cognee_bindings_common::SdkError::Runtime(format!("cognify failed: {in_use}"));
    assert_eq!(err.code(), "STORE_IN_USE");
    assert_eq!(
        cognee_bindings_common::SdkError::Runtime("cognify failed: boom".into()).code(),
        "RUNTIME_ERROR"
    );
}
