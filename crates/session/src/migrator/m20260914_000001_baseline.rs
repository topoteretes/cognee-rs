//! Single baseline migration for the session chain.
//!
//! Squashes all 3 prior incremental migrations into one `up()` that creates
//! the complete current session schema in a single pass. Produced for the
//! 0.1.0 release — there is no deployed schema to upgrade from.
//!
//! Tracked via `seaql_session_migrations` (separate from the relational chain).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // ── session_qa_entries ────────────────────────────────────────────────
        // Includes 4 feedback columns merged from `session_qa_feedback_fields`.
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
                    // Merged from `session_qa_feedback_fields` migration:
                    .col(ColumnDef::new(SessionQaEntries::FeedbackText).text())
                    .col(ColumnDef::new(SessionQaEntries::FeedbackScore).integer())
                    .col(ColumnDef::new(SessionQaEntries::UsedGraphElementIds).text())
                    .col(ColumnDef::new(SessionQaEntries::MemifyMetadata).text())
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

        // ── session_graph_context ─────────────────────────────────────────────
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

        // ── session_trace_steps ───────────────────────────────────────────────
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
        // Drop in reverse dependency order.
        manager
            .drop_table(Table::drop().table(SessionTraceSteps::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(SessionGraphContext::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(SessionQaEntries::Table).to_owned())
            .await?;
        Ok(())
    }
}

// ── Iden enums ────────────────────────────────────────────────────────────────

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
