//! Adopt Python's names for the two `nodes` composite indexes.
//!
//! Rust and Python share one relational database and either may migrate first.
//! The two SDKs declare the same two composite indexes on `nodes` under
//! different names — Rust's baseline creates `idx_nodes_dataset_slug` and
//! `idx_nodes_dataset_data`, while Python declares
//! `index_node_dataset_slug` / `index_node_dataset_data`
//! (`cognee/modules/graph/models/Node.py:44-45`, created in alembic
//! `84e5d08260d6`). A database both SDKs have touched therefore carries two
//! byte-identical indexes over the same columns, paying the write cost twice
//! and confusing anyone reading the schema.
//!
//! Python is the reference implementation and its names already ship, so Rust
//! moves. Note the divergence is only on these two: Python's newer revision
//! `aa753a730673` uses the `ix_<table>_<column>` form that Rust's
//! `pipeline_run_id` and slug indexes already match, and Python declares no
//! index on `edges` at all (`Edge.__table_args__` is commented out), so Rust's
//! edge indexes have no counterpart to align with.
//!
//! ## Create-then-drop rather than rename
//!
//! SQLite has no `ALTER INDEX ... RENAME TO`, and on Postgres that statement
//! errors when the target name already exists — which is exactly the state of
//! any database Python has already migrated. Creating under the new name and
//! dropping the old one is idempotent, needs no dialect-specific SQL, and
//! handles every starting state: Python-first (create is a no-op, Rust's
//! duplicate is removed), Rust-first (create adds the name, old one goes), and
//! fresh (create adds it, drop is a no-op).

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::DbBackend;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// `(old Rust name, Python name)` for the two `nodes` composites.
const RENAMES: [(&str, &str); 2] = [
    ("idx_nodes_dataset_slug", "index_node_dataset_slug"),
    ("idx_nodes_dataset_data", "index_node_dataset_data"),
];

/// Reject backends whose dialect cannot express the guards below.
///
/// Same reasoning as the slug-index migration: `CREATE INDEX IF NOT EXISTS`
/// and `DROP INDEX IF EXISTS` are supported by SQLite and Postgres and by
/// neither MySQL form. Fail loudly rather than emit DDL that parses
/// differently.
fn check_backend(manager: &SchemaManager) -> Result<(), DbErr> {
    match manager.get_database_backend() {
        DbBackend::Sqlite | DbBackend::Postgres => Ok(()),
        other => Err(DbErr::Custom(format!(
            "node index rename migration does not support the {other:?} backend"
        ))),
    }
}

/// Create `name` over `(dataset_id, <second>)`, then drop `drop_name`.
async fn recreate(
    manager: &SchemaManager<'_>,
    name: &str,
    second: Nodes,
    drop_name: &str,
) -> Result<(), DbErr> {
    manager
        .create_index(
            Index::create()
                .if_not_exists()
                .name(name)
                .table(Nodes::Table)
                .col(Nodes::DatasetId)
                .col(second)
                .to_owned(),
        )
        .await?;
    manager
        .drop_index(
            Index::drop()
                .if_exists()
                .name(drop_name)
                .table(Nodes::Table)
                .to_owned(),
        )
        .await
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        check_backend(manager)?;
        recreate(manager, RENAMES[0].1, Nodes::Slug, RENAMES[0].0).await?;
        recreate(manager, RENAMES[1].1, Nodes::DataId, RENAMES[1].0).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        check_backend(manager)?;
        // Restore the baseline's names. Reverse order of `up`.
        recreate(manager, RENAMES[1].0, Nodes::DataId, RENAMES[1].1).await?;
        recreate(manager, RENAMES[0].0, Nodes::Slug, RENAMES[0].1).await
    }
}

#[derive(DeriveIden)]
enum Nodes {
    Table,
    DatasetId,
    Slug,
    DataId,
}
