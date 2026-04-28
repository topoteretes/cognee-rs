//! P6 migration: create the `sync_operations` table that powers the cloud
//! sync router (`POST /api/v1/sync` / `GET /api/v1/sync/status`).
//!
//! Schema mirrors Python's [`SyncOperation`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/sync/models/SyncOperation.py)
//! 1:1.
//!
//! Notes:
//! - `run_id` is `TEXT` (string UUID) — not the row's primary key. Indexed
//!   uniquely so the lookup-by-run_id path is O(log n).
//! - `dataset_ids` and `dataset_names` are JSON arrays (stored as TEXT on
//!   SQLite via SeaORM's portable JSON type).
//! - `user_id` is indexed but **not** foreign-keyed — Python deliberately
//!   keeps sync history alive when the user row is deleted.
//! - `dataset_sync_hashes` carries `{dataset_id_str: {"uploaded": [...],
//!   "downloaded": [...]}}` lineage data.
//! - Idempotent: every step uses `if_not_exists` so re-running against an
//!   already-seeded DB is a no-op.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(SyncOperations::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SyncOperations::Id)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(SyncOperations::RunId).text().not_null())
                    .col(
                        ColumnDef::new(SyncOperations::Status)
                            .text()
                            .not_null()
                            .default("started"),
                    )
                    .col(
                        ColumnDef::new(SyncOperations::ProgressPercentage)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(ColumnDef::new(SyncOperations::DatasetIds).json())
                    .col(ColumnDef::new(SyncOperations::DatasetNames).json())
                    .col(ColumnDef::new(SyncOperations::UserId).text().not_null())
                    .col(
                        ColumnDef::new(SyncOperations::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(SyncOperations::StartedAt).timestamp_with_time_zone())
                    .col(ColumnDef::new(SyncOperations::CompletedAt).timestamp_with_time_zone())
                    .col(ColumnDef::new(SyncOperations::TotalRecordsToSync).integer())
                    .col(ColumnDef::new(SyncOperations::TotalRecordsToDownload).integer())
                    .col(ColumnDef::new(SyncOperations::TotalRecordsToUpload).integer())
                    .col(
                        ColumnDef::new(SyncOperations::RecordsDownloaded)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(SyncOperations::RecordsUploaded)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(SyncOperations::BytesDownloaded)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(SyncOperations::BytesUploaded)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(ColumnDef::new(SyncOperations::DatasetSyncHashes).json())
                    .col(ColumnDef::new(SyncOperations::ErrorMessage).text())
                    .col(
                        ColumnDef::new(SyncOperations::RetryCount)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_sync_operations_run_id")
                    .table(SyncOperations::Table)
                    .col(SyncOperations::RunId)
                    .unique()
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_sync_operations_user_id")
                    .table(SyncOperations::Table)
                    .col(SyncOperations::UserId)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(SyncOperations::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum SyncOperations {
    Table,
    Id,
    RunId,
    Status,
    ProgressPercentage,
    DatasetIds,
    DatasetNames,
    UserId,
    CreatedAt,
    StartedAt,
    CompletedAt,
    TotalRecordsToSync,
    TotalRecordsToDownload,
    TotalRecordsToUpload,
    RecordsDownloaded,
    RecordsUploaded,
    BytesDownloaded,
    BytesUploaded,
    DatasetSyncHashes,
    ErrorMessage,
    RetryCount,
}
