//! Adds `data.system_metadata`, the cognee-owned route stamp of a data row.
//!
//! Python added the same nullable JSON column in alembic revision
//! `f2b4c6d8e0a1` (`add_system_metadata_to_data`): system-derived metadata
//! (the `source` that routes a row down the DLT or code-graph cognify route)
//! lives there, never in the user-writable `external_metadata`. Rust and
//! Python share one SQLite file and either SDK may migrate it first, so the
//! column is added only when missing (an existing one must still be nullable
//! — `add_column_if_missing` checks it).
//!
//! `text` holding JSON, like `external_metadata`: SQLAlchemy's `JSON` type is
//! stored as text on SQLite, so both SDKs read each other's values.
//!
//! Unlike Python's revision this does not move DLT stamps
//! (`external_metadata.source` of `"dlt"` / `"dlt_source"`) into the new
//! column: Rust's DLT route still reads them from `external_metadata`
//! (SDK-1053 decision 2), so moving them would break it. Nothing is
//! backfilled.
use sea_orm_migration::prelude::*;

use super::helpers::{add_column_if_missing, drop_column_if_present};

#[derive(DeriveMigrationName)]
pub struct Migration;

const TABLE: &str = "data";
const COLUMN: &str = "system_metadata";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let column = ColumnDef::new(Alias::new(COLUMN)).text().null().to_owned();
        add_column_if_missing(manager, TABLE, column).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        drop_column_if_present(manager, TABLE, COLUMN).await
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
    use sea_orm_migration::prelude::*;

    use super::{COLUMN, TABLE};
    use crate::migrator::{Migrator, helpers::has_column};

    /// The number of core migrations before this one.
    fn migrations_before() -> u32 {
        let position = crate::migrator::core_migrations()
            .iter()
            .position(|m| m.name() == "m20261118_000001_data_system_metadata")
            .expect("the migration is registered");
        <u32 as std::convert::TryFrom<usize>>::try_from(position).expect("the chain is short")
    }

    async fn memory_db() -> DatabaseConnection {
        Database::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite always connects")
    }

    async fn column_count(db: &DatabaseConnection) -> usize {
        db.query_all(Statement::from_string(
            db.get_database_backend(),
            format!("SELECT name FROM pragma_table_info('{TABLE}') WHERE name = '{COLUMN}'"),
        ))
        .await
        .expect("column probe")
        .len()
    }

    #[tokio::test]
    async fn up_adds_the_column_on_a_fresh_database() {
        let db = memory_db().await;
        Migrator::up(&db, None).await.expect("migrations run");
        let manager = SchemaManager::new(&db);
        assert!(has_column(&manager, TABLE, COLUMN).await.expect("probe"));
        assert_eq!(column_count(&db).await, 1);
    }

    /// Python's alembic `f2b4c6d8e0a1` may have added the column first: the
    /// migration keeps it (and its values) instead of failing with
    /// "duplicate column name".
    #[tokio::test]
    async fn up_keeps_a_column_python_added_first() {
        let db = memory_db().await;
        Migrator::up(&db, Some(migrations_before()))
            .await
            .expect("earlier migrations run");
        for sql in [
            "ALTER TABLE data ADD COLUMN system_metadata JSON".to_string(),
            "INSERT INTO data (id, name, raw_data_location, original_data_location, \
             extension, mime_type, content_hash, owner_id, created_at, token_count, \
             data_size, system_metadata) VALUES ('a', 'n', 'r', 'o', 'txt', 'text/plain', \
             'h', 'u', '2026-11-18T00:00:00Z', -1, -1, '{\"source\": \"code_repo\"}')"
                .to_string(),
        ] {
            db.execute(Statement::from_string(db.get_database_backend(), sql))
                .await
                .expect("simulated alembic statement");
        }

        Migrator::up(&db, None)
            .await
            .expect("the existing column is accepted");

        assert_eq!(column_count(&db).await, 1);
        let row = db
            .query_one(Statement::from_string(
                db.get_database_backend(),
                "SELECT system_metadata FROM data WHERE id = 'a'".to_string(),
            ))
            .await
            .expect("select runs")
            .expect("the row survives");
        let value: String = row.try_get("", COLUMN).expect("text value");
        assert_eq!(value, "{\"source\": \"code_repo\"}");
    }

    #[tokio::test]
    async fn down_drops_the_column() {
        let db = memory_db().await;
        Migrator::up(&db, None).await.expect("migrations run");
        let after = <u32 as std::convert::TryFrom<usize>>::try_from(
            crate::migrator::core_migrations().len(),
        )
        .expect("the chain is short")
            - migrations_before();
        Migrator::down(&db, Some(after))
            .await
            .expect("down to before this migration");
        let manager = SchemaManager::new(&db);
        assert!(!has_column(&manager, TABLE, COLUMN).await.expect("probe"));
    }
}
