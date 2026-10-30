use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(SessionQaEntries::Table)
                    .add_column(ColumnDef::new(SessionQaEntries::UsedSessionContextIds).text())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(SessionQaEntries::Table)
                    .drop_column(SessionQaEntries::UsedSessionContextIds)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum SessionQaEntries {
    Table,
    UsedSessionContextIds,
}
