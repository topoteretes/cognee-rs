#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! The in-process guard against two live `LadybugAdapter`s on one file.
//!
//! lbug's file lock is a process-owned POSIX record lock, so it never refuses
//! a second `Database` on a path this process already has open, and two
//! instances on one file destroy each other's writes
//! (`tests/ladybug_stale_instance.rs`). The adapter claims the path in a
//! process-wide registry before it opens anything, so the second open fails
//! loudly with `GraphDBError::StoreInUse` instead.
#![cfg(feature = "ladybug")]

use cognee_graph::{GraphDBError, GraphDBTrait, LadybugAdapter};
use cognee_utils::STORE_IN_USE_CODE;

#[tokio::test]
async fn a_second_live_open_of_one_graph_is_refused_naming_the_path() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("graph");
    let first = LadybugAdapter::new(db.to_str().unwrap()).await.unwrap();
    first.initialize().await.unwrap();

    let Err(GraphDBError::StoreInUse(in_use)) = LadybugAdapter::new(db.to_str().unwrap()).await
    else {
        panic!("a second live open of the same graph must be refused");
    };
    assert_eq!(
        in_use.path(),
        dir.path().canonicalize().unwrap().join("graph")
    );
    let message = GraphDBError::StoreInUse(in_use).to_string();
    assert!(message.starts_with(STORE_IN_USE_CODE), "{message}");

    // The refused open touched nothing: the first adapter still serves.
    first.get_graph_data().await.unwrap();
    first.close().await.unwrap();
}

#[tokio::test]
async fn a_closed_graph_can_be_opened_again() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("graph");
    let first = LadybugAdapter::new(db.to_str().unwrap()).await.unwrap();
    first.initialize().await.unwrap();
    first.close().await.unwrap();

    // `first` is still alive (closed, not dropped): the claim follows the
    // database, not the adapter.
    let second = LadybugAdapter::new(db.to_str().unwrap()).await.unwrap();
    second.initialize().await.unwrap();
    second.close().await.unwrap();
    drop(first);
}

#[tokio::test]
async fn a_dropped_graph_can_be_opened_again() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("graph");
    let first = LadybugAdapter::new(db.to_str().unwrap()).await.unwrap();
    first.initialize().await.unwrap();
    drop(first);

    let second = LadybugAdapter::new(db.to_str().unwrap()).await.unwrap();
    second.initialize().await.unwrap();
    second.close().await.unwrap();
}

#[tokio::test]
async fn different_graphs_open_side_by_side() {
    let dir = tempfile::tempdir().unwrap();
    let a = LadybugAdapter::new(dir.path().join("a").to_str().unwrap())
        .await
        .unwrap();
    let b = LadybugAdapter::new(dir.path().join("b").to_str().unwrap())
        .await
        .unwrap();
    a.initialize().await.unwrap();
    b.initialize().await.unwrap();
    a.close().await.unwrap();
    b.close().await.unwrap();
}

#[tokio::test]
async fn in_memory_graphs_are_never_claimed() {
    let a = LadybugAdapter::new(":memory:").await.unwrap();
    let b = LadybugAdapter::new(":memory:").await.unwrap();
    a.initialize().await.unwrap();
    b.initialize().await.unwrap();
}
