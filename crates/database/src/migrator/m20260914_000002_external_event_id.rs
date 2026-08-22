//! Add a nullable external idempotency key to dataset membership.
//!
//! Existing rows remain valid with `NULL`. The composite unique index permits
//! multiple legacy/null rows while ensuring one external event maps to at most
//! one data item inside a dataset.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(DatasetData::Table)
                    .add_column(ColumnDef::new(DatasetData::ExternalEventId).text().null())
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_dataset_data_external_event")
                    .table(DatasetData::Table)
                    .col(DatasetData::DatasetId)
                    .col(DatasetData::ExternalEventId)
                    .unique()
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("idx_dataset_data_external_event")
                    .table(DatasetData::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(DatasetData::Table)
                    .drop_column(DatasetData::ExternalEventId)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum DatasetData {
    Table,
    DatasetId,
    ExternalEventId,
}
