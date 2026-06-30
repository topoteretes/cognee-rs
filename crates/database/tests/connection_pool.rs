#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Regression tests for relational connection-pool sizing.
//!
//! The exclusive-in-memory-SQLite invariant must pin the pool to a single
//! connection, while file-backed SQLite must NOT be pinned (that would
//! needlessly serialize concurrent reads). sea-orm 1.1 exposes the underlying
//! sqlx pool, so the configured `max_connections` is directly assertable.
#![cfg(feature = "sqlite")]

use cognee_database::connect;

#[tokio::test]
async fn in_memory_sqlite_is_single_connection() {
    let db = connect("sqlite::memory:").await.expect("connect");
    assert_eq!(
        db.get_sqlite_connection_pool()
            .options()
            .get_max_connections(),
        1,
        "in-memory SQLite must be pinned to one connection",
    );
}

#[tokio::test]
async fn file_sqlite_allows_a_pool() {
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
}
