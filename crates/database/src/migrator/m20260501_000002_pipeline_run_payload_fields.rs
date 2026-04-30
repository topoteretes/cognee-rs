//! LIB-06 migration: create the `pipeline_run_payload_fields` table.
//!
//! Backs the DB-backed default accumulator for the pipeline payload event
//! channel (Q-H). Composite primary key on `(pipeline_run_id, key)` provides
//! upsert semantics for concurrent task emits without lock contention across
//! different keys.
//!
//! No FK to `pipeline_runs` — matches the loose-coupling style of the
//! existing schema (`pipeline_run_id` is a UUID-as-string).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(PipelineRunPayloadFields::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(PipelineRunPayloadFields::PipelineRunId)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(PipelineRunPayloadFields::Key)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(PipelineRunPayloadFields::Value)
                            .json()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(PipelineRunPayloadFields::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(PipelineRunPayloadFields::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(PipelineRunPayloadFields::PipelineRunId)
                            .col(PipelineRunPayloadFields::Key),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_pipeline_run_payload_fields_run_id")
                    .table(PipelineRunPayloadFields::Table)
                    .col(PipelineRunPayloadFields::PipelineRunId)
                    .if_not_exists()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(PipelineRunPayloadFields::Table)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
pub(crate) enum PipelineRunPayloadFields {
    Table,
    PipelineRunId,
    Key,
    Value,
    CreatedAt,
    UpdatedAt,
}
