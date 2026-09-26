#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Regression tests for `LadybugAdapter::flush` — checkpoint without close.
//!
//! `ladybug_close.rs` pins the teardown half: a `close()` checkpoints the `.wal`
//! into the main file and refuses further work. That is the wrong tool for a
//! running app. On Android the engine handle is process-scoped and deliberately
//! *not* closed when the Activity goes away (closing it under an in-flight
//! cognify shuts the pool beneath that run and wedges the dataset for 24 h), so
//! in practice a phone never closes the store at all — and therefore never
//! checkpoints it.
//!
//! What was left was a graph that lives entirely in an un-checkpointed `.wal`
//! from the first write until the process dies. lbug does replay that sidecar at
//! the next open, which is why this usually works; but the file is also what
//! lbug itself deletes at every checkpoint (`Checkpointer::writeCheckpoint` ends
//! in `WAL::reset()`), and a store whose main data file has never been written
//! has no second copy of anything. Measured on a device: 812 nodes and 2187
//! edges readable in-session, `system/graph` still 16_384 B — the catalog and
//! nothing else — and 0 nodes after the relaunch.
//!
//! `flush()` is what gives the data a second home while the handle stays warm.
//! The assertions below are therefore about *both* halves: the checkpoint really
//! happened, and the adapter is still serving afterwards.
#![cfg(feature = "ladybug")]

use std::path::{Path, PathBuf};

use cognee_graph::{GraphDBTrait, LadybugAdapter, NodeData};
use serial_test::serial;
use tempfile::TempDir;

/// Enough to force a real `.wal` rather than something that fits in lbug's page
/// cache.
const NODES: usize = 500;

fn wal_path(db_path: &Path) -> PathBuf {
    let mut p = db_path.as_os_str().to_os_string();
    p.push(".wal");
    PathBuf::from(p)
}

fn len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Warm an adapter and write `NODES` nodes, leaving them un-checkpointed.
async fn warm() -> (LadybugAdapter, PathBuf, TempDir) {
    let dir = TempDir::new().expect("failed to create temp dir");
    let db_path = dir.path().join("graph");
    let adapter = LadybugAdapter::new(db_path.to_str().unwrap())
        .await
        .expect("failed to create LadybugAdapter");
    adapter.initialize().await.expect("initialize");

    let nodes: Vec<_> = (0..NODES)
        .map(|i| {
            serde_json::json!({
                "id": format!("n{i}"),
                "name": format!("Node {i}"),
                "type": "TestNode",
                "properties": {"idx": i, "pad": "x".repeat(64)},
            })
        })
        .collect();
    adapter.add_nodes_raw(nodes).await.expect("add_nodes_raw");

    (adapter, db_path, dir)
}

/// The core measurement: `flush()` folds the `.wal` into the main database file,
/// and — unlike `close()` — leaves the adapter serving.
///
/// Fails before the fix, where no such method exists and the main file stays at
/// its initial size for the whole life of the process.
#[tokio::test]
#[serial]
async fn flush_checkpoints_without_closing() {
    let (adapter, db_path, _dir) = warm().await;

    let main_before = len(&db_path);
    let wal_before = len(&wal_path(&db_path));
    assert!(
        wal_before > 0,
        "precondition: writes must leave an un-checkpointed WAL"
    );

    adapter.flush().await.expect("flush must succeed");

    let main_after = len(&db_path);
    assert!(
        main_after > main_before,
        "the WAL content must land in the main file: {main_before} -> {main_after}"
    );
    assert!(
        len(&wal_path(&db_path)) < wal_before,
        "the checkpoint must drain the WAL"
    );

    // The half that distinguishes this from close(): the store still works.
    let node: Option<NodeData> = adapter
        .get_node("n1")
        .await
        .expect("reads must still work after a flush");
    assert!(node.is_some(), "flush must not lose the flushed data");
    adapter
        .add_node_raw(serde_json::json!({"id": "after", "name": "A", "type": "T"}))
        .await
        .expect("writes must still work after a flush");
}

/// The guarantee that matters on a phone: once flushed, the data no longer
/// depends on the `.wal` surviving.
///
/// The `.wal` is removed outright before the reopen. That is not a contrived
/// insult — it is what lbug does to its own WAL at every checkpoint, and losing
/// it is the difference between the device symptom (0 nodes) and a healthy
/// store. Before the fix this test reopens an empty graph.
#[tokio::test]
#[serial]
async fn flushed_data_does_not_depend_on_the_wal() {
    let (adapter, db_path, _dir) = warm().await;
    adapter.flush().await.expect("flush");

    // The process dies without a close: no destructor, no second checkpoint.
    std::mem::forget(adapter);
    std::fs::remove_file(wal_path(&db_path)).ok();

    let reopened = LadybugAdapter::new(db_path.to_str().unwrap())
        .await
        .expect("reopen");
    reopened.initialize().await.expect("initialize the reopen");
    let (nodes, _edges) = reopened.get_graph_data().await.expect("query the reopen");
    assert_eq!(
        nodes.len(),
        NODES,
        "flushed nodes must survive with no WAL to replay"
    );
    reopened.close().await.expect("close the reopen");
}

/// Idempotent and cheap to repeat, so a caller can flush at every quiet point
/// without tracking whether anything changed.
#[tokio::test]
#[serial]
async fn flush_is_idempotent() {
    let (adapter, db_path, _dir) = warm().await;
    adapter.flush().await.expect("first flush");
    let main_after_first = len(&db_path);
    adapter.flush().await.expect("second flush");
    adapter.flush().await.expect("third flush");
    assert_eq!(
        len(&db_path),
        main_after_first,
        "a flush with nothing to checkpoint must not rewrite the file"
    );
    adapter.close().await.expect("close");
}

/// A flush after a close is a no-op rather than an error: teardown tiers may
/// both fire, and the close has already checkpointed.
#[tokio::test]
#[serial]
async fn flush_after_close_is_a_no_op() {
    let (adapter, _db_path, _dir) = warm().await;
    adapter.close().await.expect("close");
    adapter
        .flush()
        .await
        .expect("flush on a closed adapter must be Ok, not an error");
}

/// The trait object route — what `ComponentManager` and the cognify op hold —
/// reaches the same implementation.
#[tokio::test]
#[serial]
async fn flush_through_the_trait_object() {
    let (adapter, db_path, _dir) = warm().await;
    let main_before = len(&db_path);

    let erased: std::sync::Arc<dyn GraphDBTrait> = std::sync::Arc::new(adapter);
    erased.flush().await.expect("flush via GraphDBTrait");

    assert!(
        len(&db_path) > main_before,
        "the trait method must not be the default no-op for ladybug"
    );
    erased.close().await.expect("close");
}
