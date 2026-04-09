use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(SessionQaEntries::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SessionQaEntries::Id)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(SessionQaEntries::SessionId)
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(SessionQaEntries::UserId).text())
                    .col(ColumnDef::new(SessionQaEntries::Question).text().not_null())
                    .col(ColumnDef::new(SessionQaEntries::Answer).text().not_null())
                    .col(ColumnDef::new(SessionQaEntries::Context).text())
                    .col(
                        ColumnDef::new(SessionQaEntries::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_session_qa_session_id")
                    .table(SessionQaEntries::Table)
                    .col(SessionQaEntries::SessionId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_session_qa_session_user")
                    .table(SessionQaEntries::Table)
                    .col(SessionQaEntries::SessionId)
                    .col(SessionQaEntries::UserId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(SessionQaEntries::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum SessionQaEntries {
    Table,
    Id,
    SessionId,
    UserId,
    Question,
    Answer,
    Context,
    CreatedAt,
}
