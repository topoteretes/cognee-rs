//! P7 migration: create the `notebooks` table.
//!
//! Idempotent against a Python-seeded DB — uses `if_not_exists()`.
//! No `tenant_id` column — notebooks are user-scoped only (see routers/notebooks.md §3).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
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
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Notebooks::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
pub(crate) enum Notebooks {
    Table,
    Id,
    OwnerId,
    Name,
    Cells,
    Deletable,
    CreatedAt,
}
