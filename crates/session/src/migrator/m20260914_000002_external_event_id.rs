//! Add nullable external idempotency keys to Q&A and trace session entries.

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
                    .add_column(
                        ColumnDef::new(SessionQaEntries::ExternalEventId)
                            .text()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_session_qa_external_event")
                    .table(SessionQaEntries::Table)
                    .col(SessionQaEntries::SessionId)
                    .col(SessionQaEntries::ExternalEventId)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(SessionTraceSteps::Table)
                    .add_column(
                        ColumnDef::new(SessionTraceSteps::ExternalEventId)
                            .text()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_session_trace_external_event")
                    .table(SessionTraceSteps::Table)
                    .col(SessionTraceSteps::SessionId)
                    .col(SessionTraceSteps::ExternalEventId)
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
                    .name("idx_session_trace_external_event")
                    .table(SessionTraceSteps::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(SessionTraceSteps::Table)
                    .drop_column(SessionTraceSteps::ExternalEventId)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                Index::drop()
                    .name("idx_session_qa_external_event")
                    .table(SessionQaEntries::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(SessionQaEntries::Table)
                    .drop_column(SessionQaEntries::ExternalEventId)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum SessionQaEntries {
    Table,
    SessionId,
    ExternalEventId,
}

#[derive(DeriveIden)]
enum SessionTraceSteps {
    Table,
    SessionId,
    ExternalEventId,
}
