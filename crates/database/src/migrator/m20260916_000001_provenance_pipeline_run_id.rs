//! Adds `nodes.pipeline_run_id` / `edges.pipeline_run_id` — the run that
//! created each provenance ownership row — plus an index on each.
//!
//! Nullable with no default and no backfill, deliberately: rows written before
//! this migration have no owning run, and `pipeline_run_id = :run` never
//! matches `NULL` in SQL, so they are permanently invisible to every
//! run-scoped select and delete rather than ambiguously owned by whichever run
//! sweeps next.
//!
//! No foreign key to `pipeline_runs`: that table holds one row per status
//! transition, so `pipeline_run_id` is a correlation id, not a key. Python has
//! no FK here either, and the baseline's `nodes.data_id` sets the precedent.
//!
//! The column and both index names are copied verbatim from Python's alembic
//! revision `aa753a730673`, which guards the same names with its own
//! existence checks. Rust and Python share one SQLite file and either SDK may
//! migrate it first, so both sides must skip what the other already created —
//! hence the guards below rather than a plain `ALTER TABLE`.
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const NODES_INDEX: &str = "ix_nodes_pipeline_run_id";
const EDGES_INDEX: &str = "ix_edges_pipeline_run_id";
const COLUMN: &str = "pipeline_run_id";

/// Does `table` already have `column`?
///
/// `SchemaManager::has_column` is unusable here: it dispatches on
/// *sea-orm-migration*'s own `sqlx-*` features, which this crate does not
/// enable (it only turns on `sea-orm/sqlx-sqlite` / `sea-orm/sqlx-postgres`),
/// so every arm is compiled out and the call panics with "feature is off".
async fn has_column(manager: &SchemaManager<'_>, table: &str, column: &str) -> Result<bool, DbErr> {
    let backend = manager.get_database_backend();
    let sql = match backend {
        DbBackend::Sqlite => {
            "SELECT COUNT(*) AS cnt FROM pragma_table_info(?) WHERE name = ?".to_string()
        }
        DbBackend::Postgres => "SELECT COUNT(*) AS cnt FROM information_schema.columns \
             WHERE table_schema = current_schema() AND table_name = $1 AND column_name = $2"
            .to_string(),
        other => {
            return Err(DbErr::Custom(format!(
                "pipeline_run_id migration does not support the {other:?} backend"
            )));
        }
    };
    let row = manager
        .get_connection()
        .query_one(Statement::from_sql_and_values(
            backend,
            sql,
            [table.into(), column.into()],
        ))
        .await?
        .ok_or_else(|| DbErr::Custom("column-existence probe returned no row".to_owned()))?;
    Ok(row.try_get::<i64>("", "cnt")? > 0)
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // `text`, not a native uuid type: every id column in this schema is
        // `text` holding `uuid_hex` (32 hex chars, no hyphens), which is what
        // Python's SQLAlchemy `UUID` renders as on SQLite.
        if !has_column(manager, "nodes", COLUMN).await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Nodes::Table)
                        .add_column(ColumnDef::new(Nodes::PipelineRunId).text().null())
                        .to_owned(),
                )
                .await?;
        }
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name(NODES_INDEX)
                    .table(Nodes::Table)
                    .col(Nodes::PipelineRunId)
                    .to_owned(),
            )
            .await?;

        if !has_column(manager, "edges", COLUMN).await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Edges::Table)
                        .add_column(ColumnDef::new(Edges::PipelineRunId).text().null())
                        .to_owned(),
                )
                .await?;
        }
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name(EDGES_INDEX)
                    .table(Edges::Table)
                    .col(Edges::PipelineRunId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Index first, then column — the reverse of `up`.
        manager
            .drop_index(
                Index::drop()
                    .if_exists()
                    .name(EDGES_INDEX)
                    .table(Edges::Table)
                    .to_owned(),
            )
            .await?;
        if has_column(manager, "edges", COLUMN).await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Edges::Table)
                        .drop_column(Edges::PipelineRunId)
                        .to_owned(),
                )
                .await?;
        }

        manager
            .drop_index(
                Index::drop()
                    .if_exists()
                    .name(NODES_INDEX)
                    .table(Nodes::Table)
                    .to_owned(),
            )
            .await?;
        if has_column(manager, "nodes", COLUMN).await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Nodes::Table)
                        .drop_column(Nodes::PipelineRunId)
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Nodes {
    Table,
    PipelineRunId,
}

#[derive(DeriveIden)]
enum Edges {
    Table,
    PipelineRunId,
}
