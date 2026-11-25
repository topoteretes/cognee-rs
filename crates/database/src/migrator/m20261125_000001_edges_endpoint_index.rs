//! Adds `ix_edges_source_node_id_destination_node_id` to the `edges` ledger:
//! the index the per-document delete's edge exclusivity is planned through
//! (SDK-1694 O6).
//!
//! A data-scoped delete, a dataset delete and a run sweep decide whether an
//! edge row is the last claim on its edge by looking for another row of the
//! same `(source_node_id, destination_node_id, relationship_name)` triple — a
//! correlated `NOT EXISTS` per selected row. Before O6 they correlated on
//! `slug`, which `ix_edges_slug` (`m20260917_000001`) serves; the triple had
//! no index, so SQLite would scan the whole table once per selected row. The
//! two endpoint columns narrow a lookup to the rows of one edge (a handful:
//! one per producing document, and one per relationship between the same two
//! nodes); `relationship_name` is left out because it holds an edge's whole
//! text for a `contains` edge, which would bloat the index for no selectivity.
//!
//! Purely additive and harmless to a Python reader of the same database,
//! which indexes neither column. The name follows SQLAlchemy's default for a
//! multi-column index, as `m20260917_000001`'s do, and `IF NOT EXISTS` makes
//! a rerun a no-op.
use sea_orm::DbBackend;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const EDGES_INDEX: &str = "ix_edges_source_node_id_destination_node_id";

/// Reject backends whose dialect cannot express the `IF [NOT] EXISTS` guards,
/// as `m20260917_000001` does.
fn check_backend(manager: &SchemaManager) -> Result<(), DbErr> {
    match manager.get_database_backend() {
        DbBackend::Sqlite | DbBackend::Postgres => Ok(()),
        other => Err(DbErr::Custom(format!(
            "edge endpoint index migration does not support the {other:?} backend"
        ))),
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        check_backend(manager)?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name(EDGES_INDEX)
                    .table(Edges::Table)
                    .col(Edges::SourceNodeId)
                    .col(Edges::DestinationNodeId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        check_backend(manager)?;
        manager
            .drop_index(
                Index::drop()
                    .if_exists()
                    .name(EDGES_INDEX)
                    .table(Edges::Table)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Edges {
    Table,
    SourceNodeId,
    DestinationNodeId,
}
