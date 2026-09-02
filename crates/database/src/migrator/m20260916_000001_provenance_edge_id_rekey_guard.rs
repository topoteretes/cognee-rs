//! Refuses to open a relational database whose provenance edge ids predate the
//! NUL-stripping change in PR #149.
//!
//! #149 started stripping NUL bytes from an edge's text *before* deriving the
//! provenance edge id (`cognify::tasks::upsert_provenance`). Postgres never
//! stored those bytes to begin with — it rejects them outright, which is what
//! #149 fixed — but SQLite (the default backend) and Ladybug store them happily,
//! so any `edges` row written by an older build is keyed on `uuid5(raw text)`
//! while the same corpus now yields `uuid5(sanitized text)`.
//!
//! The two ids never collide, so `ON CONFLICT (id) DO UPDATE` does not match and
//! re-cognifying such a corpus *duplicates* every affected provenance edge
//! instead of updating it, leaving stale rows that nothing sweeps.
//!
//! Rewriting the ids in place is possible in principle but is not worth the
//! machinery: there is no production deployment on the affected builds, so this
//! fails loudly and tells the operator to recreate the database instead.
//!
//! # Why this cannot fire on a database that is actually fine
//!
//! Migrations run before any write, so on a fresh database the baseline has just
//! created `edges` and it is empty — the common case, and the guard passes.
//! Once it has passed it is recorded in `seaql_migrations` and never runs again,
//! so the rows a later `cognify` writes are irrelevant to it.
//!
//! Only `edges` is checked. Provenance *node* ids
//! (`cognify::tasks::provenance_node_id`) are derived from UUIDs alone, never
//! from edge text, so #149 did not move them.
//!
//! # Known false positives
//!
//! The guard cannot tell a pre-#149 Rust database from one whose `edges` rows
//! are already correctly keyed, so it refuses both:
//!
//! - a database written by a Rust build between #149 and this migration;
//! - a **Python**-written database that has been cognified. Python has always
//!   sanitized before hashing (`upsert_edges.py:41-57`), so those ids are fine
//!   — but the cross-SDK harness copies Python's SQLite file into a Rust
//!   workspace and lets the Rust migrator apply its whole set on top of the
//!   alembic-created schema, `seaql_migrations` starting empty. Every such flow
//!   today (`e2e-cross-sdk/harness/test_cross_read.py`,
//!   `test_cross_delete.py`, `test_readd_after_delete.py`) copies after Python
//!   `add` and before any `cognify`, so `edges` is empty and the guard passes.
//!   A future test that cognifies on the Python side *before* the copy would
//!   trip it; recreate the Rust-side database, or move the copy earlier.
//!
//! Both are accepted: there is no production deployment on the affected builds,
//! and recreating the database is cheap next to carrying a rewrite migration.
use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

/// What an operator sees when the guard fires. Kept as a constant so the test
/// below asserts the exact text the operator is shown.
const REKEY_MESSAGE: &str = "\
This database predates the provenance edge id change in PR #149 and cannot be migrated in place.

What changed: provenance edge ids and slugs are now derived from the edge's text *after* NUL \
bytes are stripped from it. SQLite and Ladybug store NUL bytes verbatim, so the `edges` rows \
already in this database are keyed on the unstripped text. The old and new ids never collide, \
so re-running `cognify` on the same corpus would insert duplicate provenance edges rather than \
updating the existing ones, and the stale rows would never be cleaned up.

What to do: delete this database and its graph and vector stores, then re-run `cognee add` \
followed by `cognee cognify` to rebuild them. No data is recoverable from the relational \
database alone, and there is no in-place upgrade path.

This check runs once, against the `edges` table, and passes for any database whose `edges` \
table is empty when it first runs — which is every database created from this build onward.";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // No `has_table` probe: `SchemaManager::has_table` panics unless
        // `sea-orm-migration` itself is built with the backend's sqlx feature,
        // which this crate does not turn on. It would buy nothing anyway — the
        // baseline creates `edges` and sorts before this migration, so the table
        // is always there by the time this runs.
        let db = manager.get_connection();
        // `LIMIT 1` rather than `COUNT(*)`: emptiness is the whole question, the
        // answer needs no decoding, and it does not scan a large table.
        let existing = db
            .query_one(Statement::from_string(
                db.get_database_backend(),
                "SELECT 1 FROM edges LIMIT 1",
            ))
            .await?;

        if existing.is_some() {
            return Err(DbErr::Migration(REKEY_MESSAGE.to_string()));
        }

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Nothing to undo: the guard only reads.
        Ok(())
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use sea_orm::{ConnectionTrait, Database, Statement};
    use sea_orm_migration::{MigrationName, MigratorTrait};

    use crate::migrator::Migrator;

    /// Plant one provenance edge, plus the `datasets` row its foreign key needs.
    async fn insert_provenance_edge(db: &sea_orm::DatabaseConnection) {
        for sql in [
            "INSERT INTO datasets (id, name, owner_id, created_at) \
             VALUES ('ds1', 'guard-test', 'u1', '2026-01-01')",
            "INSERT INTO edges (id, slug, user_id, data_id, dataset_id, source_node_id, \
             destination_node_id, relationship_name, created_at) \
             VALUES ('e1', 's1', 'u1', 'd1', 'ds1', 'n1', 'n2', 'contains', '2026-01-01')",
        ] {
            db.execute(Statement::from_string(
                db.get_database_backend(),
                sql.to_string(),
            ))
            .await
            .expect("planting a provenance edge must succeed");
        }
    }

    /// A database with provenance edges already in it must refuse to open, and
    /// must say why and what to do about it.
    #[tokio::test]
    async fn a_populated_edges_table_fails_the_migration() {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite always connects");

        // Stand up the schema the way a pre-#149 build would have, then plant a
        // provenance edge, then roll the migrator forward as if this build were
        // opening that database for the first time.
        Migrator::up(&db, None)
            .await
            .expect("a fresh database migrates cleanly");
        insert_provenance_edge(&db).await;
        db.execute(Statement::from_string(
            db.get_database_backend(),
            format!(
                "DELETE FROM seaql_migrations WHERE version = '{}'",
                super::Migration.name()
            ),
        ))
        .await
        .expect("un-apply the guard so it runs again");

        let err = Migrator::up(&db, None)
            .await
            .expect_err("a database with pre-existing provenance edges must be refused");
        let rendered = err.to_string();

        assert!(
            rendered.contains("PR #149"),
            "the operator must be told which change re-keyed the ids: {rendered}"
        );
        assert!(
            rendered.contains("delete this database"),
            "the operator must be told what to do: {rendered}"
        );
        assert_eq!(
            rendered,
            format!("Migration Error: {}", super::REKEY_MESSAGE),
            "the guard must surface the message verbatim"
        );
    }

    /// The common case: a database created from this build onward runs the
    /// guard against an empty `edges` table and passes, then never runs it
    /// again no matter how much it later writes.
    #[tokio::test]
    async fn a_fresh_database_passes_and_stays_passed() {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite always connects");

        Migrator::up(&db, None)
            .await
            .expect("a fresh database migrates cleanly");

        insert_provenance_edge(&db).await;

        Migrator::up(&db, None)
            .await
            .expect("an already-migrated database must not re-run the guard");
    }
}
