//! Create the `dataset_configurations` table used by the dataset schema API.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
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
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(DatasetConfigurations::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
pub(crate) enum DatasetConfigurations {
    Table,
    Id,
    DatasetId,
    GraphSchema,
    CustomPrompt,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Datasets {
    Table,
    Id,
}
