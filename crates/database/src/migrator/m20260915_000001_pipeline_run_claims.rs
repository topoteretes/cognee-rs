//! Adds `pipeline_run_claims` — the atomic exclusive-run claim behind
//! `PipelineRunRepository::try_claim_pipeline_run`.
//!
//! Dated one day after the baseline so it sorts (and therefore applies) after
//! it; the table is independent, so the ordering only matters for determinism.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // The composite primary key IS the mutual exclusion: two concurrent
        // inserts for the same `(dataset_id, pipeline_name)` race in the
        // database and exactly one commits.
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
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(PipelineRunClaims::Table).to_owned())
            .await?;
        Ok(())
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
