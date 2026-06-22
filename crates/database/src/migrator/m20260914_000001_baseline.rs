//! Single baseline migration for the relational chain.
//!
//! Squashes all 14 prior incremental migrations into one `up()` that creates
//! the complete current schema in a single pass. Produced for the 0.1.0
//! release — there is no deployed schema to upgrade from.
//!
//! The schema created here is identical to the schema that was produced by the
//! prior 14-migration chain. See `m20260914_000001_baseline.rs` for the full
//! table/index/FK inventory.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // ── datasets ─────────────────────────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(Datasets::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Datasets::Id).text().not_null().primary_key())
                    .col(ColumnDef::new(Datasets::Name).text().not_null())
                    .col(ColumnDef::new(Datasets::OwnerId).text().not_null())
                    .col(ColumnDef::new(Datasets::TenantId).text())
                    .col(
                        ColumnDef::new(Datasets::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Datasets::UpdatedAt).timestamp_with_time_zone())
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_datasets_owner_id")
                    .table(Datasets::Table)
                    .col(Datasets::OwnerId)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_datasets_tenant_id")
                    .table(Datasets::Table)
                    .col(Datasets::TenantId)
                    .to_owned(),
            )
            .await?;

        // ── data ─────────────────────────────────────────────────────────────
        // `importance_weight` merged in from `add_importance_weight` migration.
        manager
            .create_table(
                Table::create()
                    .table(Data::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Data::Id).text().not_null().primary_key())
                    .col(ColumnDef::new(Data::Name).text().not_null())
                    .col(ColumnDef::new(Data::RawDataLocation).text().not_null())
                    .col(ColumnDef::new(Data::OriginalDataLocation).text().not_null())
                    .col(ColumnDef::new(Data::Extension).text().not_null())
                    .col(ColumnDef::new(Data::MimeType).text().not_null())
                    .col(ColumnDef::new(Data::ContentHash).text().not_null())
                    .col(ColumnDef::new(Data::OwnerId).text().not_null())
                    .col(
                        ColumnDef::new(Data::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Data::UpdatedAt).timestamp_with_time_zone())
                    .col(ColumnDef::new(Data::Label).text())
                    .col(ColumnDef::new(Data::OriginalExtension).text())
                    .col(ColumnDef::new(Data::OriginalMimeType).text())
                    .col(ColumnDef::new(Data::LoaderEngine).text())
                    .col(ColumnDef::new(Data::RawContentHash).text())
                    .col(ColumnDef::new(Data::TenantId).text())
                    .col(ColumnDef::new(Data::ExternalMetadata).text())
                    .col(ColumnDef::new(Data::NodeSet).text())
                    .col(ColumnDef::new(Data::PipelineStatus).text())
                    .col(
                        ColumnDef::new(Data::TokenCount)
                            .big_integer()
                            .not_null()
                            .default(-1_i64),
                    )
                    .col(
                        ColumnDef::new(Data::DataSize)
                            .big_integer()
                            .not_null()
                            .default(-1_i64),
                    )
                    .col(ColumnDef::new(Data::LastAccessed).timestamp_with_time_zone())
                    // Merged from `add_importance_weight` migration:
                    .col(ColumnDef::new(Data::ImportanceWeight).double().null())
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_data_owner_id")
                    .table(Data::Table)
                    .col(Data::OwnerId)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_data_tenant_id")
                    .table(Data::Table)
                    .col(Data::TenantId)
                    .to_owned(),
            )
            .await?;

        // ── dataset_data (junction) ───────────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(DatasetData::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(DatasetData::DatasetId).text().not_null())
                    .col(ColumnDef::new(DatasetData::DataId).text().not_null())
                    .col(
                        ColumnDef::new(DatasetData::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(DatasetData::DatasetId)
                            .col(DatasetData::DataId),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(DatasetData::Table, DatasetData::DatasetId)
                            .to(Datasets::Table, Datasets::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(DatasetData::Table, DatasetData::DataId)
                            .to(Data::Table, Data::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // ── queries ───────────────────────────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(Queries::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Queries::Id).text().not_null().primary_key())
                    .col(ColumnDef::new(Queries::QueryText).text().not_null())
                    .col(ColumnDef::new(Queries::QueryType).text().not_null())
                    .col(ColumnDef::new(Queries::UserId).text())
                    .col(
                        ColumnDef::new(Queries::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // ── results ───────────────────────────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(Results::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Results::Id).text().not_null().primary_key())
                    .col(ColumnDef::new(Results::QueryId).text().not_null())
                    .col(ColumnDef::new(Results::SerializedResult).text().not_null())
                    .col(ColumnDef::new(Results::UserId).text())
                    .col(
                        ColumnDef::new(Results::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Results::Table, Results::QueryId)
                            .to(Queries::Table, Queries::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // ── nodes ─────────────────────────────────────────────────────────────
        // Note: `#[sea_orm(iden = "type")]` on `Nodes::NodeType` maps to
        // the column name "type" (not "node_type") for Python DB parity.
        manager
            .create_table(
                Table::create()
                    .table(Nodes::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Nodes::Id).text().not_null().primary_key())
                    .col(ColumnDef::new(Nodes::Slug).text().not_null())
                    .col(ColumnDef::new(Nodes::UserId).text().not_null())
                    .col(ColumnDef::new(Nodes::DataId).text().not_null())
                    .col(ColumnDef::new(Nodes::DatasetId).text().not_null())
                    .col(ColumnDef::new(Nodes::Label).text())
                    .col(ColumnDef::new(Nodes::NodeType).text().not_null())
                    .col(ColumnDef::new(Nodes::IndexedFields).json().not_null())
                    .col(ColumnDef::new(Nodes::Attributes).json())
                    .col(
                        ColumnDef::new(Nodes::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Nodes::Table, Nodes::DatasetId)
                            .to(Datasets::Table, Datasets::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    // No FK on data_id — Python has no FK here.
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_nodes_dataset_slug")
                    .table(Nodes::Table)
                    .col(Nodes::DatasetId)
                    .col(Nodes::Slug)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_nodes_dataset_data")
                    .table(Nodes::Table)
                    .col(Nodes::DatasetId)
                    .col(Nodes::DataId)
                    .to_owned(),
            )
            .await?;

        // ── edges ─────────────────────────────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(Edges::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Edges::Id).text().not_null().primary_key())
                    .col(ColumnDef::new(Edges::Slug).text().not_null())
                    .col(ColumnDef::new(Edges::UserId).text().not_null())
                    .col(ColumnDef::new(Edges::DataId).text().not_null())
                    .col(ColumnDef::new(Edges::DatasetId).text().not_null())
                    .col(ColumnDef::new(Edges::SourceNodeId).text().not_null())
                    .col(ColumnDef::new(Edges::DestinationNodeId).text().not_null())
                    .col(ColumnDef::new(Edges::RelationshipName).text().not_null())
                    .col(ColumnDef::new(Edges::Label).text())
                    .col(ColumnDef::new(Edges::Attributes).json())
                    .col(
                        ColumnDef::new(Edges::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Edges::Table, Edges::DatasetId)
                            .to(Datasets::Table, Datasets::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    // No FK on data_id — matches Python model.
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_edges_data_id")
                    .table(Edges::Table)
                    .col(Edges::DataId)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_edges_dataset_id")
                    .table(Edges::Table)
                    .col(Edges::DatasetId)
                    .to_owned(),
            )
            .await?;

        // ── pipeline_runs ─────────────────────────────────────────────────────
        // IMPORTANT: `dataset_id` is NULLABLE with NO FK — this is the final
        // state after `pipeline_run_dataset_nullable` rebuilt the table for
        // Python parity. Do NOT add NOT NULL or the FK here.
        manager
            .create_table(
                Table::create()
                    .table(PipelineRuns::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(PipelineRuns::Id)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(PipelineRuns::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(PipelineRuns::Status).text().not_null())
                    .col(
                        ColumnDef::new(PipelineRuns::PipelineRunId)
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(PipelineRuns::PipelineName).text().not_null())
                    .col(ColumnDef::new(PipelineRuns::PipelineId).text().not_null())
                    // nullable, no FK — Python parity:
                    .col(ColumnDef::new(PipelineRuns::DatasetId).text())
                    .col(ColumnDef::new(PipelineRuns::RunInfo).json())
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_pipeline_runs_pipeline_run_id")
                    .table(PipelineRuns::Table)
                    .col(PipelineRuns::PipelineRunId)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_pipeline_runs_pipeline_id")
                    .table(PipelineRuns::Table)
                    .col(PipelineRuns::PipelineId)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_pipeline_runs_dataset_id")
                    .table(PipelineRuns::Table)
                    .col(PipelineRuns::DatasetId)
                    .to_owned(),
            )
            .await?;

        // ── task_runs ─────────────────────────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(TaskRuns::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(TaskRuns::Id).text().not_null().primary_key())
                    .col(ColumnDef::new(TaskRuns::TaskName).text().not_null())
                    .col(
                        ColumnDef::new(TaskRuns::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(TaskRuns::Status).text().not_null())
                    .col(ColumnDef::new(TaskRuns::RunInfo).json())
                    .to_owned(),
            )
            .await?;

        // ── graph_metrics ─────────────────────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(GraphMetrics::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(GraphMetrics::Id)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(GraphMetrics::NumTokens).integer())
                    .col(ColumnDef::new(GraphMetrics::NumNodes).integer())
                    .col(ColumnDef::new(GraphMetrics::NumEdges).integer())
                    .col(ColumnDef::new(GraphMetrics::MeanDegree).double())
                    .col(ColumnDef::new(GraphMetrics::EdgeDensity).double())
                    .col(ColumnDef::new(GraphMetrics::NumConnectedComponents).integer())
                    .col(ColumnDef::new(GraphMetrics::SizesOfConnectedComponents).json())
                    .col(ColumnDef::new(GraphMetrics::NumSelfloops).integer())
                    .col(ColumnDef::new(GraphMetrics::Diameter).integer())
                    .col(ColumnDef::new(GraphMetrics::AvgShortestPathLength).double())
                    .col(ColumnDef::new(GraphMetrics::AvgClustering).double())
                    .col(
                        ColumnDef::new(GraphMetrics::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(GraphMetrics::UpdatedAt).timestamp_with_time_zone())
                    .to_owned(),
            )
            .await?;

        // ── Auth tables (principals, permissions, acls, tenants, users,
        //    roles, user_tenants, user_roles) moved to the closed
        //    `cognee-access-control::Migrator` (T2-move §4 S2). The closed
        //    migration runs after this baseline via the composed Migrator.

        // ── graph_sync_checkpoints ────────────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(GraphSyncCheckpoints::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(GraphSyncCheckpoints::Key)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(GraphSyncCheckpoints::Ts)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // ── Auth-adjacent tables (user_api_key, role/user/tenant_default_permissions,
        //    principal_configuration) moved to the closed
        //    `cognee-access-control::Migrator` (T2-move §4 S2).

        // ── sync_operations ───────────────────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(SyncOperations::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SyncOperations::Id)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(SyncOperations::RunId).text().not_null())
                    .col(
                        ColumnDef::new(SyncOperations::Status)
                            .text()
                            .not_null()
                            .default("started"),
                    )
                    .col(
                        ColumnDef::new(SyncOperations::ProgressPercentage)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(ColumnDef::new(SyncOperations::DatasetIds).json())
                    .col(ColumnDef::new(SyncOperations::DatasetNames).json())
                    .col(ColumnDef::new(SyncOperations::UserId).text().not_null())
                    .col(
                        ColumnDef::new(SyncOperations::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(SyncOperations::StartedAt).timestamp_with_time_zone())
                    .col(ColumnDef::new(SyncOperations::CompletedAt).timestamp_with_time_zone())
                    .col(ColumnDef::new(SyncOperations::TotalRecordsToSync).integer())
                    .col(ColumnDef::new(SyncOperations::TotalRecordsToDownload).integer())
                    .col(ColumnDef::new(SyncOperations::TotalRecordsToUpload).integer())
                    .col(
                        ColumnDef::new(SyncOperations::RecordsDownloaded)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(SyncOperations::RecordsUploaded)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(SyncOperations::BytesDownloaded)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(SyncOperations::BytesUploaded)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(ColumnDef::new(SyncOperations::DatasetSyncHashes).json())
                    .col(ColumnDef::new(SyncOperations::ErrorMessage).text())
                    .col(
                        ColumnDef::new(SyncOperations::RetryCount)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_sync_operations_run_id")
                    .table(SyncOperations::Table)
                    .col(SyncOperations::RunId)
                    .unique()
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_sync_operations_user_id")
                    .table(SyncOperations::Table)
                    .col(SyncOperations::UserId)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        // ── notebooks ─────────────────────────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(Notebooks::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Notebooks::Id)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Notebooks::OwnerId).text().not_null())
                    .col(ColumnDef::new(Notebooks::Name).text().not_null())
                    .col(
                        ColumnDef::new(Notebooks::Cells)
                            .json()
                            .not_null()
                            .default("[]"),
                    )
                    .col(
                        ColumnDef::new(Notebooks::Deletable)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(Notebooks::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_notebooks_owner_id")
                    .table(Notebooks::Table)
                    .col(Notebooks::OwnerId)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        // ── pipeline_run_payload_fields ────────────────────────────────────────
        // Both `created_at` and `updated_at` are NOT NULL. No FK.
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
            .await?;

        // ── session_records ───────────────────────────────────────────────────
        // No FKs — matches Python's loose-coupling style so records survive
        // user/dataset deletion.
        manager
            .create_table(
                Table::create()
                    .table(SessionRecords::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(SessionRecords::SessionId).text().not_null())
                    .col(ColumnDef::new(SessionRecords::UserId).text().not_null())
                    .col(ColumnDef::new(SessionRecords::DatasetId).text())
                    .col(
                        ColumnDef::new(SessionRecords::Status)
                            .text()
                            .not_null()
                            .default("running"),
                    )
                    .col(
                        ColumnDef::new(SessionRecords::StartedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SessionRecords::LastActivityAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(SessionRecords::EndedAt).timestamp_with_time_zone())
                    .col(
                        ColumnDef::new(SessionRecords::TokensIn)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(SessionRecords::TokensOut)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(SessionRecords::CostUsd)
                            .double()
                            .not_null()
                            .default(0.0),
                    )
                    .col(
                        ColumnDef::new(SessionRecords::ErrorCount)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(ColumnDef::new(SessionRecords::LastModel).text())
                    .primary_key(
                        Index::create()
                            .col(SessionRecords::SessionId)
                            .col(SessionRecords::UserId),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("ix_session_records_user_id")
                    .table(SessionRecords::Table)
                    .col(SessionRecords::UserId)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("ix_session_records_dataset_id")
                    .table(SessionRecords::Table)
                    .col(SessionRecords::DatasetId)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("ix_session_records_last_activity_at")
                    .table(SessionRecords::Table)
                    .col(SessionRecords::LastActivityAt)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("ix_session_records_status")
                    .table(SessionRecords::Table)
                    .col(SessionRecords::Status)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        // ── session_model_usage ───────────────────────────────────────────────
        // No extra indexes (PK covers all read paths). No FKs.
        manager
            .create_table(
                Table::create()
                    .table(SessionModelUsage::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SessionModelUsage::SessionId)
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(SessionModelUsage::UserId).text().not_null())
                    .col(ColumnDef::new(SessionModelUsage::Model).text().not_null())
                    .col(
                        ColumnDef::new(SessionModelUsage::TokensIn)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(SessionModelUsage::TokensOut)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(SessionModelUsage::CostUsd)
                            .double()
                            .not_null()
                            .default(0.0),
                    )
                    .col(
                        ColumnDef::new(SessionModelUsage::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(SessionModelUsage::SessionId)
                            .col(SessionModelUsage::UserId)
                            .col(SessionModelUsage::Model),
                    )
                    .to_owned(),
            )
            .await?;

        // ── dataset_configurations ────────────────────────────────────────────
        // FK is NAMED `fk_dataset_configurations_dataset_id` — preserve the name.
        manager
            .create_table(
                Table::create()
                    .table(DatasetConfigurations::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(DatasetConfigurations::Id)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(DatasetConfigurations::DatasetId)
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(DatasetConfigurations::GraphSchema).json())
                    .col(ColumnDef::new(DatasetConfigurations::CustomPrompt).text())
                    .col(
                        ColumnDef::new(DatasetConfigurations::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DatasetConfigurations::UpdatedAt).timestamp_with_time_zone(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_dataset_configurations_dataset_id")
                            .from(
                                DatasetConfigurations::Table,
                                DatasetConfigurations::DatasetId,
                            )
                            .to(Datasets::Table, Datasets::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("uq_dataset_configurations_dataset_id")
                    .table(DatasetConfigurations::Table)
                    .col(DatasetConfigurations::DatasetId)
                    .unique()
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        // Auth-table seeding (permissions, principals, acls, default user)
        // moved to the closed `cognee-access-control` auth migration
        // (T2-move §4 S2).
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Drop in reverse dependency order (dependants before their targets).
        manager
            .drop_table(Table::drop().table(DatasetConfigurations::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(SessionModelUsage::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(SessionRecords::Table).to_owned())
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(PipelineRunPayloadFields::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(Notebooks::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(SyncOperations::Table).to_owned())
            .await?;
        // Auth-table drops moved to the closed `cognee-access-control`
        // auth migration (T2-move §4 S2). The closed Migrator runs those
        // drops before this baseline's drops (since the composed migration
        // list adds the auth migration after the baseline; sea-orm's down
        // walks the list in reverse).
        manager
            .drop_table(Table::drop().table(GraphSyncCheckpoints::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(GraphMetrics::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(TaskRuns::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(PipelineRuns::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Edges::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Nodes::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Results::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Queries::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(DatasetData::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Data::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Datasets::Table).to_owned())
            .await?;
        Ok(())
    }
}

// ── Iden enums ────────────────────────────────────────────────────────────────

#[derive(DeriveIden)]
enum Datasets {
    Table,
    Id,
    Name,
    OwnerId,
    TenantId,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
#[allow(clippy::enum_variant_names)]
enum Data {
    Table,
    Id,
    Name,
    RawDataLocation,
    OriginalDataLocation,
    Extension,
    MimeType,
    ContentHash,
    OwnerId,
    CreatedAt,
    UpdatedAt,
    Label,
    OriginalExtension,
    OriginalMimeType,
    LoaderEngine,
    RawContentHash,
    TenantId,
    ExternalMetadata,
    NodeSet,
    PipelineStatus,
    TokenCount,
    DataSize,
    LastAccessed,
    ImportanceWeight,
}

#[derive(DeriveIden)]
enum DatasetData {
    Table,
    DatasetId,
    DataId,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Queries {
    Table,
    Id,
    QueryText,
    QueryType,
    UserId,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Results {
    Table,
    Id,
    QueryId,
    SerializedResult,
    UserId,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Nodes {
    Table,
    Id,
    Slug,
    UserId,
    DataId,
    DatasetId,
    Label,
    // Python's SQLAlchemy model uses column name "type" (not "node_type").
    // Override the default DeriveIden snake_case conversion.
    #[sea_orm(iden = "type")]
    NodeType,
    IndexedFields,
    Attributes,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Edges {
    Table,
    Id,
    Slug,
    UserId,
    DataId,
    DatasetId,
    SourceNodeId,
    DestinationNodeId,
    RelationshipName,
    Label,
    Attributes,
    CreatedAt,
}

#[derive(DeriveIden)]
enum PipelineRuns {
    Table,
    Id,
    CreatedAt,
    Status,
    PipelineRunId,
    PipelineName,
    PipelineId,
    DatasetId,
    RunInfo,
}

#[derive(DeriveIden)]
enum TaskRuns {
    Table,
    Id,
    TaskName,
    CreatedAt,
    Status,
    RunInfo,
}

#[derive(DeriveIden)]
enum GraphMetrics {
    Table,
    Id,
    NumTokens,
    NumNodes,
    NumEdges,
    MeanDegree,
    EdgeDensity,
    NumConnectedComponents,
    SizesOfConnectedComponents,
    NumSelfloops,
    Diameter,
    AvgShortestPathLength,
    AvgClustering,
    CreatedAt,
    UpdatedAt,
}

// Auth idens (Principals, Permissions, Acls, Tenants, Users, Roles,
// UserTenants, UserRoles, UserApiKey, RoleDefaultPermissions,
// UserDefaultPermissions, TenantDefaultPermissions, PrincipalConfiguration)
// moved to the closed `cognee-access-control` auth migration (T2-move §4 S2).

#[derive(DeriveIden)]
enum GraphSyncCheckpoints {
    Table,
    Key,
    Ts,
}

#[derive(DeriveIden)]
enum SyncOperations {
    Table,
    Id,
    RunId,
    Status,
    ProgressPercentage,
    DatasetIds,
    DatasetNames,
    UserId,
    CreatedAt,
    StartedAt,
    CompletedAt,
    TotalRecordsToSync,
    TotalRecordsToDownload,
    TotalRecordsToUpload,
    RecordsDownloaded,
    RecordsUploaded,
    BytesDownloaded,
    BytesUploaded,
    DatasetSyncHashes,
    ErrorMessage,
    RetryCount,
}

#[derive(DeriveIden)]
enum Notebooks {
    Table,
    Id,
    OwnerId,
    Name,
    Cells,
    Deletable,
    CreatedAt,
}

#[derive(DeriveIden)]
enum PipelineRunPayloadFields {
    Table,
    PipelineRunId,
    Key,
    Value,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum SessionRecords {
    Table,
    SessionId,
    UserId,
    DatasetId,
    Status,
    StartedAt,
    LastActivityAt,
    EndedAt,
    TokensIn,
    TokensOut,
    CostUsd,
    ErrorCount,
    LastModel,
}

#[derive(DeriveIden)]
enum SessionModelUsage {
    Table,
    SessionId,
    UserId,
    Model,
    TokensIn,
    TokensOut,
    CostUsd,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum DatasetConfigurations {
    Table,
    Id,
    DatasetId,
    GraphSchema,
    CustomPrompt,
    CreatedAt,
    UpdatedAt,
}
