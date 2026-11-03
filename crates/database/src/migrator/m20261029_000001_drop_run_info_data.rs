//! Removes the `data` key from `pipeline_runs.run_info`.
//!
//! Mirrors Python alembic `b7e4c2a9d6f1_drop_run_info_data` (COG-6089). Until
//! then both SDKs stored the run's data items in `run_info["data"]` on the
//! `STARTED` row and again on the terminal row, so the table grew with dataset
//! size times run rate, and nothing ever read the key. Python's writers stopped
//! storing it in `a88402500` and this revision strips it from stored rows; the
//! Rust writers stopped in the same change as this migration.
//!
//! Like Python's revision:
//! - only rows whose `run_info` is a JSON object holding a `data` key change;
//!   every other key (`error`, `cognify_failures`, …) is kept and no row is
//!   deleted;
//! - it is idempotent — a second run finds no such row, and a database Python
//!   already migrated is left as it is;
//! - `down` is a no-op: the removed values are what the revision exists to
//!   remove.
//!
//! The rewrite is one set-based `UPDATE` per backend (SQLite `json_remove`,
//! PostgreSQL `jsonb - 'data'`), filtered in SQL the way Python's
//! `data_key_filter` does. Rows whose `run_info` is not valid JSON, or is not
//! an object, are left untouched.
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let backend = conn.get_database_backend();
        if !pipeline_runs_exists(conn, backend).await? {
            return Ok(());
        }
        let sql = match backend {
            DbBackend::Sqlite => {
                "UPDATE pipeline_runs \
                 SET run_info = json_remove(run_info, '$.data') \
                 WHERE CASE WHEN json_valid(run_info) \
                   THEN json_type(run_info) = 'object' \
                        AND json_type(run_info, '$.data') IS NOT NULL \
                   ELSE 0 END"
            }
            DbBackend::Postgres => {
                "UPDATE pipeline_runs \
                 SET run_info = (run_info::jsonb - 'data')::json \
                 WHERE run_info IS NOT NULL \
                   AND jsonb_typeof(run_info::jsonb) = 'object' \
                   AND (run_info::jsonb -> 'data') IS NOT NULL"
            }
            other => {
                return Err(DbErr::Custom(format!(
                    "dropping run_info.data does not support the {other:?} backend"
                )));
            }
        };
        conn.execute(Statement::from_string(backend, sql)).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Nothing to restore: the removed values are gone on purpose (Python's
        // `downgrade` is a no-op too).
        Ok(())
    }
}

/// Whether `pipeline_runs` exists. Not `SchemaManager::has_table`, which
/// needs a sea-schema backend feature this crate does not enable.
async fn pipeline_runs_exists(
    conn: &impl ConnectionTrait,
    backend: DbBackend,
) -> Result<bool, DbErr> {
    let sql = match backend {
        DbBackend::Sqlite => {
            "SELECT COUNT(*) AS cnt FROM sqlite_master \
             WHERE type = 'table' AND name = 'pipeline_runs'"
        }
        DbBackend::Postgres => {
            "SELECT COUNT(*) AS cnt FROM information_schema.tables \
             WHERE table_schema = current_schema() AND table_name = 'pipeline_runs'"
        }
        other => {
            return Err(DbErr::Custom(format!(
                "dropping run_info.data does not support the {other:?} backend"
            )));
        }
    };
    Ok(conn
        .query_one(Statement::from_string(backend, sql))
        .await?
        .map(|row| row.try_get::<i64>("", "cnt"))
        .transpose()?
        .unwrap_or(0)
        > 0)
}
