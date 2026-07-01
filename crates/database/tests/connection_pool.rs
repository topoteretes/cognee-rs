#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Regression tests for relational connection-pool sizing and SQLite journaling.
//!
//! The exclusive-in-memory-SQLite invariant must pin the pool to a single
//! connection that is never reaped (the whole database lives inside it), while
//! file-backed SQLite must NOT be pinned (that would needlessly serialize
//! concurrent reads) and must run in WAL mode (the only journal mode where a
//! multi-connection pool actually buys reader/writer concurrency). sea-orm 1.1
//! exposes the underlying sqlx pool, so the configured options are directly
//! assertable.
#![cfg(feature = "sqlite")]

use cognee_database::connect;
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

#[tokio::test]
async fn in_memory_sqlite_is_single_connection() {
    let db = connect("sqlite::memory:").await.expect("connect");
    let opts = db.get_sqlite_connection_pool().options();
    assert_eq!(
        opts.get_max_connections(),
        1,
        "in-memory SQLite must be pinned to one connection",
    );
    // The entire database lives in this one connection, so reaping it would
    // discard all data and reconnect to a fresh empty DB. Both reapers off.
    assert_eq!(
        opts.get_idle_timeout(),
        None,
        "in-memory connection must not be idle-reaped",
    );
    assert_eq!(
        opts.get_max_lifetime(),
        None,
        "in-memory connection must not be expired by max-lifetime",
    );
}

#[tokio::test]
async fn file_sqlite_allows_a_pool_in_wal_mode() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("t.db").display());
    let db = connect(&url).await.expect("connect");

    assert!(
        db.get_sqlite_connection_pool()
            .options()
            .get_max_connections()
            > 1,
        "file-backed SQLite should not be pinned to a single connection",
    );

    // A multi-connection pool only pays off with WAL's reader/writer concurrency;
    // in rollback-journal mode the extra connections just contend for one lock.
    let journal_mode: String = db
        .query_one(Statement::from_string(
            DatabaseBackend::Sqlite,
            "PRAGMA journal_mode;",
        ))
        .await
        .unwrap()
        .expect("PRAGMA journal_mode returns a row")
        .try_get_by_index(0)
        .unwrap();
    assert_eq!(
        journal_mode.to_lowercase(),
        "wal",
        "file-backed SQLite should run in WAL mode",
    );
}
