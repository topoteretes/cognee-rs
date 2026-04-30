//! LIB-03 migration: create the `session_records` and `session_model_usage`
//! tables that back the v2 `/sessions` router (E-09 / E-10 / E-11 / E-12).
//!
//! Mirrors Python's SQLAlchemy models at
//! `cognee/modules/session_lifecycle/models.py:10-126`:
//!
//! - `session_records` has a composite primary key `(session_id, user_id)`
//!   plus four indexes (`user_id`, `dataset_id`, `last_activity_at`,
//!   `status`) used by the dashboard listing/aggregation queries.
//! - `session_model_usage` has a composite primary key
//!   `(session_id, user_id, model)`; no extra indexes — the PK covers
//!   every read path used by `cost_by_model`.
//!
//! UUIDs are stored as `text()` (32-char hex strings) to match the rest
//! of the schema; LIB-05's repository converts `uuid::Uuid` ↔ `String`
//! at the trait boundary.
//!
//! Idempotent: every step uses `if_not_exists()` so re-running against an
//! already-seeded DB is a no-op.
//!
//! No FKs to `users` / `datasets` — matches the loose-coupling style of
//! the existing schema (sync_operations, etc.) and keeps session records
//! alive across user/dataset deletion (matching Python).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(SessionRecords::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(SessionRecords::SessionId).text().not_null())
                    .col(ColumnDef::new(SessionRecords::UserId).text().not_null())
                    .col(ColumnDef::new(SessionRecords::DatasetId).text())
                    .col(
                        ColumnDef::new(SessionRecords::Status)
                            .text()
                            .not_null()
                            .default("running"),
                    )
                    .col(
                        ColumnDef::new(SessionRecords::StartedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SessionRecords::LastActivityAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(SessionRecords::EndedAt).timestamp_with_time_zone())
                    .col(
                        ColumnDef::new(SessionRecords::TokensIn)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(SessionRecords::TokensOut)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(SessionRecords::CostUsd)
                            .double()
                            .not_null()
                            .default(0.0),
                    )
                    .col(
                        ColumnDef::new(SessionRecords::ErrorCount)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(ColumnDef::new(SessionRecords::LastModel).text())
                    .primary_key(
                        Index::create()
                            .col(SessionRecords::SessionId)
                            .col(SessionRecords::UserId),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("ix_session_records_user_id")
                    .table(SessionRecords::Table)
                    .col(SessionRecords::UserId)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("ix_session_records_dataset_id")
                    .table(SessionRecords::Table)
                    .col(SessionRecords::DatasetId)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("ix_session_records_last_activity_at")
                    .table(SessionRecords::Table)
                    .col(SessionRecords::LastActivityAt)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("ix_session_records_status")
                    .table(SessionRecords::Table)
                    .col(SessionRecords::Status)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(SessionModelUsage::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SessionModelUsage::SessionId)
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(SessionModelUsage::UserId).text().not_null())
                    .col(ColumnDef::new(SessionModelUsage::Model).text().not_null())
                    .col(
                        ColumnDef::new(SessionModelUsage::TokensIn)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(SessionModelUsage::TokensOut)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(SessionModelUsage::CostUsd)
                            .double()
                            .not_null()
                            .default(0.0),
                    )
                    .col(
                        ColumnDef::new(SessionModelUsage::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(SessionModelUsage::SessionId)
                            .col(SessionModelUsage::UserId)
                            .col(SessionModelUsage::Model),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(SessionModelUsage::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(SessionRecords::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum SessionRecords {
    Table,
    SessionId,
    UserId,
    DatasetId,
    Status,
    StartedAt,
    LastActivityAt,
    EndedAt,
    TokensIn,
    TokensOut,
    CostUsd,
    ErrorCount,
    LastModel,
}

#[derive(DeriveIden)]
enum SessionModelUsage {
    Table,
    SessionId,
    UserId,
    Model,
    TokensIn,
    TokensOut,
    CostUsd,
    UpdatedAt,
}
