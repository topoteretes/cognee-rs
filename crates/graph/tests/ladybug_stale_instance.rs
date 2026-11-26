#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Guarantee test for the way an embedded graph used to lose everything at once.
//!
//! lbug 0.14.1 allowed a second `Database` on a path the process already had
//! open, and said nothing about it. The two instances had separate buffer
//! managers but shared one `<db>.wal` on disk, and every checkpoint ends in
//! `WAL::reset()`, which *unlinks that file*. So whichever instance checkpointed
//! last wrote its own view into the data file and deleted the log the other
//! one's writes lived in. When the loser was the instance with the data, the
//! data was simply gone: measured on the device that prompted this,
//! `system/graph` recovered at 16_384 B with a 61-byte `.wal` and 0 nodes, after
//! a session that had served 812 nodes and 2187 edges.
//!
//! This file used to pin that hazard, and said it should become a guarantee the
//! day lbug handled it. lbug 0.23 does: a second read-write open of a path the
//! process still holds fails with "already open in this process".
//!
//! There are two guards now. `LadybugAdapter` claims its path in a process-wide
//! registry before opening (`cognee_utils::store_claim`), so a second adapter
//! fails first, with a typed `GraphDBError::StoreInUse`
//! (`tests/ladybug_store_in_use.rs`). This file drives `lbug::Database`
//! directly, to pin the engine's own refusal underneath that claim: if a future
//! lbug drops it, the shared-WAL hazard is back for any open that bypasses the
//! adapter.
#![cfg(feature = "ladybug")]

use std::path::Path;

use lbug::{Connection, Database, SystemConfig, Value};
use serial_test::serial;

const NODES: i64 = 500;

fn open(db: &Path) -> Result<Database, lbug::Error> {
    Database::new(db, SystemConfig::default())
}

fn count(db: &Database) -> i64 {
    let conn = Connection::new(db).unwrap();
    let mut rows = conn.query("MATCH (n:N) RETURN count(n)").unwrap();
    match rows.next().unwrap().unwrap().first() {
        Some(Value::Int64(n)) => *n,
        other => panic!("unexpected count value {other:?}"),
    }
}

/// A second read-write open of a store the process still holds is refused,
/// and the first instance keeps serving and persisting its writes.
#[tokio::test]
#[serial]
async fn a_second_live_instance_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("graph");

    let live = open(&db).unwrap();
    Connection::new(&live)
        .unwrap()
        .query("CREATE NODE TABLE N(id INT64, PRIMARY KEY(id))")
        .unwrap();

    let err = match open(&db) {
        Ok(_) => panic!(
            "lbug opened a second read-write instance on a path this process holds; \
             the shared-WAL hazard in this file's doc comment is back"
        ),
        Err(e) => e.to_string(),
    };
    assert!(
        err.contains("already open in this process"),
        "the refusal must name the cause, got: {err}"
    );

    Connection::new(&live)
        .unwrap()
        .query(&format!(
            "UNWIND range(0, {}) AS i CREATE (:N {{id: i}})",
            NODES - 1
        ))
        .unwrap();
    assert_eq!(count(&live), NODES, "the live instance serves its writes");
    drop(live);

    let reopened = open(&db).unwrap();
    assert_eq!(
        count(&reopened),
        NODES,
        "the writes survive a close and reopen"
    );
}
