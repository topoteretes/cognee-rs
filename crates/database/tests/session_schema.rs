#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! The session migration chain: the removed graph-to-session sync's
//! `session_graph_context` table is dropped (Python `3401da38a`), the other
//! session tables stay.

use cognee_database::{SessionQaDb, connect};
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};

async fn table_exists(db: &DatabaseConnection, table: &str) -> bool {
    let rows = db
        .query_all(Statement::from_string(
            DatabaseBackend::Sqlite,
            format!("SELECT name FROM sqlite_master WHERE type='table' AND name='{table}'"),
        ))
        .await
        .expect("sqlite_master query");
    !rows.is_empty()
}

#[tokio::test]
async fn a_fresh_session_schema_has_no_graph_context_table() {
    let db = connect("sqlite::memory:").await.expect("connect");
    db.ensure_session_schema().await.expect("session schema");

    assert!(!table_exists(&db, "session_graph_context").await);
    for table in [
        "session_qa_entries",
        "session_trace_steps",
        "session_context_entries",
    ] {
        assert!(table_exists(&db, table).await, "{table} must exist");
    }
}

/// A database that still holds the table (and snapshots in it) from before
/// the removal loses it on the next schema run.
#[tokio::test]
async fn an_existing_graph_context_table_is_dropped() {
    let db = connect("sqlite::memory:").await.expect("connect");
    db.execute_unprepared(
        "CREATE TABLE session_graph_context (\
           id TEXT NOT NULL PRIMARY KEY, session_id TEXT NOT NULL, user_id TEXT, \
           context TEXT NOT NULL, updated_at TIMESTAMP NOT NULL); \
         INSERT INTO session_graph_context VALUES \
           ('default:s1', 's1', NULL, 'a -> b', '2026-01-01T00:00:00Z');",
    )
    .await
    .expect("seed the legacy table");

    db.ensure_session_schema().await.expect("session schema");
    db.ensure_session_schema().await.expect("idempotent");

    assert!(!table_exists(&db, "session_graph_context").await);
    assert!(table_exists(&db, "session_qa_entries").await);
}
