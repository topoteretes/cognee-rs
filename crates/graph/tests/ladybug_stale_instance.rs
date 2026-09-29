#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Characterization test for the way an embedded graph loses everything at once.
//!
//! This asserts a **hazard, not a guarantee**. It pins the behaviour that
//! destroyed an on-device graph so that a change in it — ours or lbug's — is
//! noticed rather than discovered again from a user report.
//!
//! lbug allows a second `Database` on a path the process already has open, and
//! says nothing about it. The two instances have separate buffer managers but
//! share one `<db>.wal` on disk, and every checkpoint ends in `WAL::reset()`,
//! which *unlinks that file*. So whichever instance checkpoints last writes its
//! own view into the data file and deletes the log the other one's writes live
//! in. When the loser is the instance with the data, the data is simply gone.
//!
//! Ordering is what decides it. If the second instance opens *after* the first
//! has written, it replays the WAL and sees those writes, and its checkpoint
//! preserves them — that is the benign case, and why this is intermittent. This
//! test covers the other order: the second instance opens while the store is
//! still empty, so it never learns about anything written afterwards.
//!
//! Measured against the device that prompted this: `system/graph` recovered at
//! **16_384 B** with a **61-byte** `.wal` and 0 nodes, after a session that had
//! served 812 nodes and 2187 edges until the process died. The numbers this
//! test produces are the same ones.
//!
//! An engine rebuild is how the bad order arises in practice.
//! `LadybugAdapter::close`'s own contract says it does not guarantee the file
//! descriptor is free when it returns — an in-flight query holds an owned
//! `Arc<Database>` clone — so a rebuild can open instance two while instance one
//! is still alive, and lbug runs a checkpoint in `Database`'s destructor
//! whenever that last clone finally drops.
//!
//! The flush this crate now performs does not remove the hazard; it bounds it.
//! Once a checkpoint has folded writes into the data file they survive any
//! later stale-instance checkpoint, so what remains at risk is only what was
//! written since the last flush rather than the entire graph.
#![cfg(feature = "ladybug")]

use std::path::{Path, PathBuf};

use cognee_graph::{GraphDBTrait, LadybugAdapter};
use serial_test::serial;

const NODES: usize = 500;

fn wal_path(db: &Path) -> PathBuf {
    let mut p = db.as_os_str().to_os_string();
    p.push(".wal");
    PathBuf::from(p)
}

fn len(p: &Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

/// A second instance that opened before the writes checkpoints its empty view,
/// unlinking the WAL those writes live in.
#[tokio::test]
#[serial]
async fn a_stale_second_instance_can_destroy_newer_writes() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("graph");

    let live = LadybugAdapter::new(db.to_str().unwrap()).await.unwrap();
    live.initialize().await.unwrap();

    // The stale instance opens while the store is still empty, so it will never
    // learn about anything written from here on.
    let stale = LadybugAdapter::new(db.to_str().unwrap()).await.unwrap();
    stale.initialize().await.unwrap();
    let (seen, _) = stale.get_graph_data().await.unwrap();
    assert_eq!(
        seen.len(),
        0,
        "precondition: the stale instance opened empty"
    );

    let nodes: Vec<_> = (0..NODES)
        .map(|i| {
            serde_json::json!({
                "id": format!("n{i}"), "name": format!("N{i}"), "type": "TestNode",
                "properties": {"idx": i, "pad": "x".repeat(64)},
            })
        })
        .collect();
    live.add_nodes_raw(nodes).await.unwrap();

    let (in_session, _) = live.get_graph_data().await.unwrap();
    assert_eq!(
        in_session.len(),
        NODES,
        "the live instance serves its writes"
    );
    assert!(
        len(&wal_path(&db)) > 0,
        "and they are in the WAL, not the data file"
    );

    // The stale instance checkpoints. This is what a delayed drop of a replaced
    // engine does, via lbug's destructor, with no caller involved at all.
    stale.close().await.unwrap();
    assert_eq!(
        len(&wal_path(&db)),
        0,
        "the stale checkpoint unlinked the shared WAL"
    );

    // The process dies without the live instance ever checkpointing.
    std::mem::forget(live);

    let reopened = LadybugAdapter::new(db.to_str().unwrap()).await.unwrap();
    reopened.initialize().await.unwrap();
    let (after, _) = reopened.get_graph_data().await.unwrap();

    // The hazard, asserted so a change in it is loud. If this ever fails with
    // `after == NODES`, lbug (or we) started handling concurrent instances
    // safely and this file should become a guarantee instead of a warning.
    assert_eq!(
        after.len(),
        0,
        "characterization: writes the stale instance never saw are lost with the WAL"
    );
    reopened.close().await.unwrap();
}
