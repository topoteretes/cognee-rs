//! Drop `session_graph_context`: the per-session graph-knowledge snapshot
//! written by the removed graph-to-session sync (Python `3401da38a`, "Remove
//! graph session sync"). Nothing reads or writes it any more.
//!
//! `down` recreates the baseline table (empty).

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
                    .if_not_exists()
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
