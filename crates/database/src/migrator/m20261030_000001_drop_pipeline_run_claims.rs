//! Drops `pipeline_run_claims`, the exclusive-run claim table.
//!
//! The claim was Rust-only: a database row that refused a second cognify on a
//! dataset (`PipelineAlreadyRunning`) and that a killed process left behind for
//! a day. Python serializes runs with an in-process per-dataset lock and
//! recovers abandoned runs at startup (`dc0b4ad6a`, `modules/cognify/recovery.py`);
//! Rust now does the same (`cognee_core::DatasetLocks`, `cognee_core::RunOwnership`),
//! so nothing reads or writes the table any more.
//!
//! `up` is idempotent; `down` recreates the table so a rollback of this
//! revision leaves the schema the previous revision expects.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(PipelineRunClaims::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(PipelineRunClaims::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(PipelineRunClaims::DatasetId)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(PipelineRunClaims::PipelineName)
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(PipelineRunClaims::ClaimId).text().not_null())
                    .col(
                        ColumnDef::new(PipelineRunClaims::ClaimedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(PipelineRunClaims::DatasetId)
                            .col(PipelineRunClaims::PipelineName),
                    )
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum PipelineRunClaims {
    Table,
    DatasetId,
    PipelineName,
    ClaimId,
    ClaimedAt,
}
