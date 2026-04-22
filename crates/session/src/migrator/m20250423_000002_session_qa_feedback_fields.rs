use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Add feedback/tracking columns to session_qa_entries
        manager
            .alter_table(
                Table::alter()
                    .table(SessionQaEntries::Table)
                    .add_column(ColumnDef::new(SessionQaEntries::FeedbackText).text())
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(SessionQaEntries::Table)
                    .add_column(ColumnDef::new(SessionQaEntries::FeedbackScore).integer())
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(SessionQaEntries::Table)
                    .add_column(ColumnDef::new(SessionQaEntries::UsedGraphElementIds).text())
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(SessionQaEntries::Table)
                    .add_column(ColumnDef::new(SessionQaEntries::MemifyMetadata).text())
                    .to_owned(),
            )
            .await?;

        // Create session_graph_context table
        manager
            .create_table(
                Table::create()
                    .table(SessionGraphContext::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SessionGraphContext::Id)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(SessionGraphContext::SessionId)
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(SessionGraphContext::UserId).text())
                    .col(
                        ColumnDef::new(SessionGraphContext::Context)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SessionGraphContext::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_session_graph_ctx_session_user")
                    .table(SessionGraphContext::Table)
                    .col(SessionGraphContext::SessionId)
                    .col(SessionGraphContext::UserId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(SessionGraphContext::Table).to_owned())
            .await?;

        // SQLite does not support DROP COLUMN, so we leave the columns in place
        // on down-migration. For PostgreSQL / MySQL they could be removed.
        Ok(())
    }
}

#[derive(DeriveIden)]
enum SessionQaEntries {
    Table,
    FeedbackText,
    FeedbackScore,
    UsedGraphElementIds,
    MemifyMetadata,
}

#[derive(DeriveIden)]
enum SessionGraphContext {
    Table,
    Id,
    SessionId,
    UserId,
    Context,
    UpdatedAt,
}
