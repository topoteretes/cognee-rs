use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(SessionTraceSteps::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SessionTraceSteps::TraceId)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(SessionTraceSteps::UserId).text().not_null())
                    .col(
                        ColumnDef::new(SessionTraceSteps::SessionId)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SessionTraceSteps::Seq)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SessionTraceSteps::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SessionTraceSteps::OriginFunction)
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(SessionTraceSteps::Status).text().not_null())
                    .col(
                        ColumnDef::new(SessionTraceSteps::MemoryQuery)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SessionTraceSteps::MemoryContext)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SessionTraceSteps::MethodParams)
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(SessionTraceSteps::MethodReturnValue).text())
                    .col(
                        ColumnDef::new(SessionTraceSteps::ErrorMessage)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SessionTraceSteps::SessionFeedback)
                            .text()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_session_trace_steps_user_session_seq")
                    .table(SessionTraceSteps::Table)
                    .col(SessionTraceSteps::UserId)
                    .col(SessionTraceSteps::SessionId)
                    .col(SessionTraceSteps::Seq)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(SessionTraceSteps::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum SessionTraceSteps {
    Table,
    TraceId,
    UserId,
    SessionId,
    Seq,
    CreatedAt,
    OriginFunction,
    Status,
    MemoryQuery,
    MemoryContext,
    MethodParams,
    MethodReturnValue,
    ErrorMessage,
    SessionFeedback,
}
