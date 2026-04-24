use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
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
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(GraphSyncCheckpoints::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum GraphSyncCheckpoints {
    Table,
    Key,
    Ts,
}
