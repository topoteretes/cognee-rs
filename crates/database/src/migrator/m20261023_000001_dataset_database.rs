//! Adds `dataset_database` — where a dataset's own graph and vector databases
//! live under per-dataset storage (`ENABLE_BACKEND_ACCESS_CONTROL`, SDK-1162).
//!
//! Python's table (`DatasetDatabase.py`, alembic `76625596c5c3` and
//! `46a6ce2bd2b2`), so `if_not_exists`: a database Python migrated already has
//! it. Python also references `principals.id` from `owner_id`; `principals` is
//! not an OSS table (a downstream ACL layer owns it), so only the `datasets`
//! foreign key is declared here. Deleting a dataset removes its row through it.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(DatasetDatabase::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(DatasetDatabase::DatasetId)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(DatasetDatabase::OwnerId).text())
                    .col(
                        ColumnDef::new(DatasetDatabase::VectorDatabaseName)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DatasetDatabase::GraphDatabaseName)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DatasetDatabase::VectorDatabaseProvider)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DatasetDatabase::GraphDatabaseProvider)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DatasetDatabase::GraphDatasetDatabaseHandler)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DatasetDatabase::VectorDatasetDatabaseHandler)
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(DatasetDatabase::VectorDatabaseUrl).text())
                    .col(ColumnDef::new(DatasetDatabase::GraphDatabaseUrl).text())
                    .col(ColumnDef::new(DatasetDatabase::VectorDatabaseKey).text())
                    .col(ColumnDef::new(DatasetDatabase::GraphDatabaseKey).text())
                    .col(ColumnDef::new(DatasetDatabase::CogneeVersion).text())
                    .col(ColumnDef::new(DatasetDatabase::MigrationRevision).text())
                    .col(ColumnDef::new(DatasetDatabase::MigrationLastError).text())
                    .col(
                        ColumnDef::new(DatasetDatabase::GraphDatabaseConnectionInfo)
                            .json()
                            .not_null()
                            .default("{}"),
                    )
                    .col(
                        ColumnDef::new(DatasetDatabase::VectorDatabaseConnectionInfo)
                            .json()
                            .not_null()
                            .default("{}"),
                    )
                    .col(ColumnDef::new(DatasetDatabase::CreatedAt).timestamp_with_time_zone())
                    .col(ColumnDef::new(DatasetDatabase::UpdatedAt).timestamp_with_time_zone())
                    .foreign_key(
                        ForeignKey::create()
                            .from(DatasetDatabase::Table, DatasetDatabase::DatasetId)
                            .to(Datasets::Table, Datasets::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("ix_dataset_database_owner_id")
                    .table(DatasetDatabase::Table)
                    .col(DatasetDatabase::OwnerId)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(DatasetDatabase::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum DatasetDatabase {
    Table,
    DatasetId,
    OwnerId,
    VectorDatabaseName,
    GraphDatabaseName,
    VectorDatabaseProvider,
    GraphDatabaseProvider,
    GraphDatasetDatabaseHandler,
    VectorDatasetDatabaseHandler,
    VectorDatabaseUrl,
    GraphDatabaseUrl,
    VectorDatabaseKey,
    GraphDatabaseKey,
    CogneeVersion,
    MigrationRevision,
    MigrationLastError,
    GraphDatabaseConnectionInfo,
    VectorDatabaseConnectionInfo,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Datasets {
    Table,
    Id,
}
