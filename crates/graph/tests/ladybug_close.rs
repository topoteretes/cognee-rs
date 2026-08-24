#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Regression tests for `LadybugAdapter::close` — the embedded-graph half of
//! topoteretes/cognee-rs#132.
//!
//! The relational fix (#135) established that *dropping is not closing*. The
//! embedded graph has the same shape with a different sidecar: lbug keeps an
//! un-checkpointed `<db>.wal` next to the main file and holds a
//! `FileLockType::WRITE_LOCK` on the main file for as long as its descriptor is
//! open. Neither is released by letting the last `Arc` fall out of scope at an
//! unspecified time — measurably, `close()` is what unlinks the `.wal` (659_977 B
//! before the fix, 0 after) and folds it into the main file.
//!
//! Every assertion here is on the *awaited* `close()`, never on a post-`Drop`
//! sleep: the destructor's checkpoint took up to 1.5 s for an 817 KB WAL during
//! the audit, so a sleep-based assertion would be flaky in CI.
#![cfg(feature = "ladybug")]

use std::path::{Path, PathBuf};

use cognee_graph::{GraphDBTrait, LadybugAdapter, NodeData};
use serial_test::serial;
use tempfile::TempDir;

/// Number of nodes/edges written before the close. Enough to force lbug to grow
/// a real WAL rather than keeping everything in its page cache.
const NODES: usize = 500;

fn wal_path(db_path: &Path) -> PathBuf {
    let mut p = db_path.as_os_str().to_os_string();
    p.push(".wal");
    PathBuf::from(p)
}

/// Warm an adapter under a temp dir and write `NODES` nodes + `NODES - 10`
/// edges, so the store has un-checkpointed WAL content at teardown.
async fn warm() -> (LadybugAdapter, PathBuf, TempDir) {
    let dir = TempDir::new().expect("failed to create temp dir");
    let db_path = dir.path().join("test.db");
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

    let edges: Vec<cognee_graph::EdgeData> = (0..NODES.saturating_sub(10))
        .map(|i| {
            (
                format!("n{i}"),
                format!("n{}", i + 10),
                "links_to".to_string(),
                Default::default(),
            )
        })
        .collect();
    adapter.add_edges(&edges).await.expect("add_edges");

    (adapter, db_path, dir)
}

/// The core measurement: after `close()` the `.wal` is gone and its content has
/// been folded into the main database file.
///
/// Fails on the pre-fix code, where `close()` is the defaulted no-op and the
/// `.wal` is still on disk when it returns — measured at 659_977 B for this
/// workload. With the fix the `.wal` is gone and the main file has grown from
/// 4_096 B to 3_055_616 B, i.e. the WAL content was really folded in rather than
/// discarded.
#[tokio::test]
#[serial]
async fn close_checkpoints_and_unlinks_the_wal() {
    let (adapter, db_path, _dir) = warm().await;
    let wal = wal_path(&db_path);

    assert!(
        wal.exists(),
        "precondition: writes must leave an un-checkpointed WAL at {}",
        wal.display()
    );
    let main_before = std::fs::metadata(&db_path).expect("main db file").len();

    adapter.close().await.expect("close must succeed");

    assert!(
        !wal.exists(),
        "close() must checkpoint and unlink {} — still present ({} bytes)",
        wal.display(),
        std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0),
    );
    let main_after = std::fs::metadata(&db_path).expect("main db file").len();
    assert!(
        main_after > main_before,
        "the WAL content must land in the main file: {main_before} -> {main_after}"
    );
}

/// Post-close operations fail with a "closed" error instead of silently
/// reopening the store — the graph twin of the relational contract in #135.
#[tokio::test]
#[serial]
async fn operations_after_close_fail_with_a_closed_error() {
    let (adapter, _db_path, _dir) = warm().await;
    adapter.close().await.expect("close");

    let err = adapter
        .get_node("n1")
        .await
        .expect_err("a query after close must fail, not silently reopen");
    let msg = err.to_string();
    assert!(
        msg.contains("closed"),
        "error must name the closed state, got: {msg}"
    );

    // Writes too, not just reads.
    let err = adapter
        .add_node_raw(serde_json::json!({"id": "late", "name": "L", "type": "T"}))
        .await
        .expect_err("a write after close must fail");
    assert!(err.to_string().contains("closed"), "got: {err}");
}

/// Idempotent: a second `close()` is a no-op, so implicit and explicit teardown
/// tiers can both call it without ordering rules.
#[tokio::test]
#[serial]
async fn close_is_idempotent() {
    let (adapter, db_path, _dir) = warm().await;
    adapter.close().await.expect("first close");
    adapter.close().await.expect("second close must be a no-op");
    assert!(!wal_path(&db_path).exists());
}

/// A re-warm on the same path still succeeds after `close()`, and the data
/// written before the close is readable.
///
/// Unlike the three above, this one also passes *without* the fix: lbug happily
/// takes a second `Database` on a path it already holds open, so a double-open is
/// silent rather than an error. That is exactly why it is asserted here — the fix
/// must not convert a legitimate reopen into a failure, and the awaited
/// `spawn_blocking` drop inside `close()` is what stops the re-warm from racing
/// the previous adapter's checkpoint (`ComponentManager::close` doc, contract
/// note 4).
#[tokio::test]
#[serial]
async fn the_same_path_can_be_reopened_after_close() {
    let (adapter, db_path, _dir) = warm().await;
    adapter.close().await.expect("close");

    let reopened = LadybugAdapter::new(db_path.to_str().unwrap())
        .await
        .expect("reopening the closed path must succeed");
    reopened.initialize().await.expect("initialize the reopen");

    // The data written before the close survived the checkpoint.
    let node: Option<NodeData> = reopened.get_node("n1").await.expect("query the reopen");
    assert!(node.is_some(), "checkpointed data must be readable");

    reopened.close().await.expect("close the reopen");
}

/// `close()` guarantees exactly what its rustdoc now claims — closed to new work
/// and checkpointed — and nothing about the descriptor being free.
///
/// This pins the honest half in place. The stronger promise the doc used to make
/// ("drop the file descriptor and release lbug's write lock" by the time it
/// returns) cannot be kept: `db()` hands out owned `Arc<Database>` clones that an
/// in-flight query holds for its duration, so a close racing one drops a
/// reference rather than the last one. When no query is in flight the descriptor
/// *is* released, which is what `the_same_path_can_be_reopened_after_close`
/// covers; the two together are the whole contract.
#[tokio::test]
#[serial]
async fn close_reports_ok_and_refuses_new_work() {
    let (adapter, db_path, _dir) = warm().await;

    adapter.close().await.expect("close reports Ok");

    let err = adapter
        .get_node("n1")
        .await
        .expect_err("a closed adapter must refuse new queries");
    assert!(
        err.to_string().contains("closed"),
        "error should name the cause, got: {err}"
    );

    // The checkpoint half of the guarantee: the WAL was folded into the main
    // file rather than left next to it.
    assert!(
        !wal_path(&db_path).exists(),
        "close() must checkpoint the .wal away"
    );
}
