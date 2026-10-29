//! Drops `session_graph_context`, the per-session graph snapshot table.
//!
//! Only the improve `sync_graph_to_session` stage wrote it, and only session
//! completions and the `graph_context` recall leg read it. Python deleted all
//! of that in `3401da38a` ("Remove graph session sync"), and Rust followed
//! (SDK-1039), so nothing uses the table any more.
//!
//! `up` is idempotent; `down` recreates the table and its index.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(SessionGraphContext::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
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
            .await
    }
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
