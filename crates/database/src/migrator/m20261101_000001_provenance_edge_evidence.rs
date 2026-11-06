//! Creates `provenance_edge_evidence`, the compact graph-edge evidence sidecar.
//!
//! Mirrors Python's alembic revision `f3a7b9c1d2e4`
//! (`add_provenance_edge_evidence`): same columns, same three indexes under the
//! same names, and a no-op when the table already exists, so whichever SDK
//! migrates a shared database second skips it. Ids are `text` holding
//! `uuid_hex`, like every other id column here. On SQLite that is exactly what
//! SQLAlchemy's `Uuid` stores (CHAR(32) lowercase hex), so the two SDKs read
//! and write each other's rows. On Postgres, Python's columns are native
//! `uuid`, so a table either SDK created is not writable by the other — the
//! same limit every Rust table has there (`m20260914_000001_baseline`); a
//! shared database is SQLite-only. No foreign keys, as in Python: graph
//! deletion must never depend on the sidecar.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const TABLE: &str = "provenance_edge_evidence";
const INDEXES: [(&str, &[&str]); 3] = [
    ("ix_prov_evidence_edge", &["dataset_id", "edge_id"]),
    (
        "ix_prov_evidence_source",
        &["dataset_id", "data_id", "chunk_id"],
    ),
    ("ix_prov_evidence_run", &["pipeline_run_id"]),
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new(TABLE))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("tenant_id")).text().null())
                    .col(ColumnDef::new(Alias::new("user_id")).text().not_null())
                    .col(ColumnDef::new(Alias::new("dataset_id")).text().not_null())
                    .col(ColumnDef::new(Alias::new("data_id")).text().not_null())
                    .col(ColumnDef::new(Alias::new("pipeline_run_id")).text().null())
                    .col(ColumnDef::new(Alias::new("chunk_id")).text().not_null())
                    .col(ColumnDef::new(Alias::new("chunk_index")).integer().null())
                    .col(ColumnDef::new(Alias::new("edge_id")).text().not_null())
                    .col(
                        ColumnDef::new(Alias::new("source_node_id"))
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("destination_node_id"))
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("relationship_name"))
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("evidence_kind"))
                            .string_len(32)
                            .not_null()
                            .default("extracted"),
                    )
                    .col(
                        ColumnDef::new(Alias::new("source_task"))
                            .string_len(255)
                            .null(),
                    )
                    .col(ColumnDef::new(Alias::new("confidence")).double().null())
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;
        for (name, columns) in INDEXES {
            let mut index = Index::create();
            index.if_not_exists().name(name).table(Alias::new(TABLE));
            for column in columns {
                index.col(Alias::new(*column));
            }
            manager.create_index(index.to_owned()).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new(TABLE))
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}
