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

    assert!(
        adapter.flush().await.expect("flush must not be an Err"),
        "an unobstructed checkpoint must report that it happened"
    );

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
    assert!(
        adapter.flush().await.expect("flush"),
        "the checkpoint under test must actually have run"
    );

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
    assert!(adapter.flush().await.expect("first flush"));
    let main_after_first = len(&db_path);
    assert!(adapter.flush().await.expect("second flush"));
    assert!(adapter.flush().await.expect("third flush"));
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
    assert!(
        adapter
            .flush()
            .await
            .expect("flush on a closed adapter must be Ok, not an error"),
        "a closed adapter was checkpointed by the close itself, so it is durable"
    );
}

/// The trait object route — what `ComponentManager` and the cognify op hold —
/// reaches the same implementation.
#[tokio::test]
#[serial]
async fn flush_through_the_trait_object() {
    let (adapter, db_path, _dir) = warm().await;
    let main_before = len(&db_path);

    let erased: std::sync::Arc<dyn GraphDBTrait> = std::sync::Arc::new(adapter);
    assert!(
        erased.flush().await.expect("flush via GraphDBTrait"),
        "the trait method must report the outcome, not just dispatch"
    );

    assert!(
        len(&db_path) > main_before,
        "the trait method must not be the default no-op for ladybug"
    );
    erased.close().await.expect("close");
}

/// Flushing next to live reads: never a stall, never a false claim.
///
/// This is the end-to-end shape of the two sites this PR adds — the end of a
/// cognify and Android's `onPause` — both of which land at moments when an Ask
/// is plausibly mid-flight. lbug cannot checkpoint under an in-flight read:
/// `TransactionManager::checkpoint()` holds the mutex a read-only transaction
/// needs in order to `commit()`, so it waits out its full 5 s timeout with every
/// query blocked behind it and then throws. The adapter therefore declines
/// rather than starting one.
///
/// Whether any individual attempt here hits that path is a race, so it is not
/// asserted. What is asserted holds either way, and is what a caller depends on:
/// no call stalls, no call returns `Err`, and the data is checkpointed by the
/// end. The deterministic `false` case is pinned by the unit test
/// `flush_declines_instead_of_stalling_while_a_read_is_in_flight`, which holds
/// the gate directly.
#[tokio::test]
#[serial]
async fn flushing_next_to_live_reads_never_stalls_or_lies() {
    let (adapter, db_path, _dir) = warm().await;
    let main_before = len(&db_path);

    // `get_graph_data` over 500 nodes on a background task, hammered so that a
    // read is in flight across the flush. The adapter's gate is private, so
    // this goes through the public API exactly as a concurrent Ask would.
    let adapter = std::sync::Arc::new(adapter);
    let reader = {
        let adapter = std::sync::Arc::clone(&adapter);
        tokio::task::spawn_blocking(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("runtime");
            for _ in 0..200 {
                rt.block_on(adapter.get_graph_data()).expect("read");
            }
        })
    };

    // Whatever each individual attempt found, none of them may have stalled and
    // none may have lied: every `true` must come with a checkpoint that really
    // landed, and the call must always return.
    let mut declined = false;
    let started = std::time::Instant::now();
    while !reader.is_finished() {
        if !adapter.flush().await.expect("flush must never be an Err") {
            declined = true;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(30),
            "flushes under concurrent reads must not stall"
        );
    }
    reader.await.expect("reader");

    // The run is not required to hit the declining path — that is a race — but
    // when it does, the store must not have claimed durability, and either way
    // a final quiet flush must succeed and land.
    if declined {
        assert!(
            adapter.flush().await.expect("final flush"),
            "a quiet flush after the reads must go through"
        );
    }
    assert!(
        len(&db_path) > main_before,
        "the data must be checkpointed by the end regardless"
    );
    std::sync::Arc::try_unwrap(adapter)
        .map_err(|_| "reader still holds the adapter")
        .expect("sole owner")
        .close()
        .await
        .expect("close");
}
