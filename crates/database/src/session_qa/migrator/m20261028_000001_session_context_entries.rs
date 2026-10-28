//! `session_context_entries`: the session-scoped context rows (Python's
//! `cache_session_context` table) that hold active guidance entries and the
//! internal state rows (improve watermarks) of a session.
//!
//! One row per `(user_id, session_id, entry_id)`: `entry_id` is promoted from
//! the payload's `"id"` so an update targets one row directly, and `seq` keeps
//! reads in insertion order. The payload is the JSON-encoded row object.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(SessionContextEntries::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SessionContextEntries::Id)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(SessionContextEntries::SessionId)
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(SessionContextEntries::UserId).text())
                    .col(
                        ColumnDef::new(SessionContextEntries::EntryId)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SessionContextEntries::Seq)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SessionContextEntries::Payload)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SessionContextEntries::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SessionContextEntries::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_session_context_session_user_seq")
                    .table(SessionContextEntries::Table)
                    .if_not_exists()
                    .col(SessionContextEntries::SessionId)
                    .col(SessionContextEntries::UserId)
                    .col(SessionContextEntries::Seq)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(SessionContextEntries::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum SessionContextEntries {
    Table,
    Id,
    SessionId,
    UserId,
    EntryId,
    Seq,
    Payload,
    CreatedAt,
    UpdatedAt,
}
