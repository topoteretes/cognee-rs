use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // datasets
        manager
            .create_table(
                Table::create()
                    .table(Datasets::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Datasets::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Datasets::Name).text().not_null())
                    .col(ColumnDef::new(Datasets::OwnerId).uuid().not_null())
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

        // data
        manager
            .create_table(
                Table::create()
                    .table(Data::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Data::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Data::Name).text().not_null())
                    .col(ColumnDef::new(Data::RawDataLocation).text().not_null())
                    .col(ColumnDef::new(Data::OriginalDataLocation).text().not_null())
                    .col(ColumnDef::new(Data::Extension).text().not_null())
                    .col(ColumnDef::new(Data::MimeType).text().not_null())
                    .col(ColumnDef::new(Data::ContentHash).text().not_null())
                    .col(ColumnDef::new(Data::OwnerId).uuid().not_null())
                    .col(
                        ColumnDef::new(Data::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Data::UpdatedAt).timestamp_with_time_zone())
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

        // dataset_data (junction)
        manager
            .create_table(
                Table::create()
                    .table(DatasetData::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(DatasetData::DatasetId).uuid().not_null())
                    .col(ColumnDef::new(DatasetData::DataId).uuid().not_null())
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

        // queries (search history)
        manager
            .create_table(
                Table::create()
                    .table(Queries::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Queries::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Queries::QueryText).text().not_null())
                    .col(ColumnDef::new(Queries::QueryType).text().not_null())
                    .col(ColumnDef::new(Queries::UserId).uuid())
                    .col(
                        ColumnDef::new(Queries::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // results (search history)
        manager
            .create_table(
                Table::create()
                    .table(Results::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Results::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Results::QueryId).uuid().not_null())
                    .col(ColumnDef::new(Results::SerializedResult).text().not_null())
                    .col(ColumnDef::new(Results::UserId).uuid())
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

        // artifact_references
        manager
            .create_table(
                Table::create()
                    .table(ArtifactReferences::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ArtifactReferences::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(ArtifactReferences::OwnerId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ArtifactReferences::DatasetId)
                            .uuid()
                            .not_null(),
                    )
                    .col(ColumnDef::new(ArtifactReferences::DataId).uuid())
                    .col(
                        ColumnDef::new(ArtifactReferences::ArtifactKind)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ArtifactReferences::ArtifactId)
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(ArtifactReferences::CollectionName).text())
                    .col(
                        ColumnDef::new(ArtifactReferences::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(ArtifactReferences::Table, ArtifactReferences::DatasetId)
                            .to(Datasets::Table, Datasets::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(ArtifactReferences::Table, ArtifactReferences::DataId)
                            .to(Data::Table, Data::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_artifact_references_unique")
                    .table(ArtifactReferences::Table)
                    .col(ArtifactReferences::DatasetId)
                    .col(ArtifactReferences::DataId)
                    .col(ArtifactReferences::ArtifactKind)
                    .col(ArtifactReferences::ArtifactId)
                    .col(ArtifactReferences::CollectionName)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // nodes
        manager
            .create_table(
                Table::create()
                    .table(Nodes::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Nodes::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Nodes::Slug).uuid().not_null())
                    .col(ColumnDef::new(Nodes::UserId).uuid().not_null())
                    .col(ColumnDef::new(Nodes::DataId).uuid().not_null())
                    .col(ColumnDef::new(Nodes::DatasetId).uuid().not_null())
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
                    .foreign_key(
                        ForeignKey::create()
                            .from(Nodes::Table, Nodes::DataId)
                            .to(Data::Table, Data::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
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

        // edges
        manager
            .create_table(
                Table::create()
                    .table(Edges::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Edges::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Edges::Slug).uuid().not_null())
                    .col(ColumnDef::new(Edges::UserId).uuid().not_null())
                    .col(ColumnDef::new(Edges::DataId).uuid().not_null())
                    .col(ColumnDef::new(Edges::DatasetId).uuid().not_null())
                    .col(ColumnDef::new(Edges::SourceNodeId).uuid().not_null())
                    .col(ColumnDef::new(Edges::DestinationNodeId).uuid().not_null())
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
                    .foreign_key(
                        ForeignKey::create()
                            .from(Edges::Table, Edges::DataId)
                            .to(Data::Table, Data::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
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

        // pipeline_runs
        manager
            .create_table(
                Table::create()
                    .table(PipelineRuns::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(PipelineRuns::Id)
                            .uuid()
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
                            .uuid()
                            .not_null(),
                    )
                    .col(ColumnDef::new(PipelineRuns::PipelineName).text().not_null())
                    .col(ColumnDef::new(PipelineRuns::PipelineId).uuid().not_null())
                    .col(ColumnDef::new(PipelineRuns::DatasetId).uuid().not_null())
                    .col(ColumnDef::new(PipelineRuns::RunInfo).json())
                    .foreign_key(
                        ForeignKey::create()
                            .from(PipelineRuns::Table, PipelineRuns::DatasetId)
                            .to(Datasets::Table, Datasets::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
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

        // task_runs
        manager
            .create_table(
                Table::create()
                    .table(TaskRuns::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(TaskRuns::Id).uuid().not_null().primary_key())
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

        // graph_metrics
        manager
            .create_table(
                Table::create()
                    .table(GraphMetrics::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(GraphMetrics::Id)
                            .uuid()
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

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Drop in reverse dependency order
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
            .drop_table(Table::drop().table(ArtifactReferences::Table).to_owned())
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

#[derive(DeriveIden)]
enum Datasets {
    Table,
    Id,
    Name,
    OwnerId,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
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
enum ArtifactReferences {
    Table,
    Id,
    OwnerId,
    DatasetId,
    DataId,
    ArtifactKind,
    ArtifactId,
    CollectionName,
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
