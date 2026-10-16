//! Adds `dataset_id` (indexed) and `updated_at` to `queries` and `results`, so
//! search history records which dataset each row belongs to.
//!
//! Both columns are nullable: older rows and unscoped searches have no dataset,
//! and history rows are never updated. The index names are the ones Python's
//! alembic revision `e5a7b9c1d3f4` creates, so whichever SDK migrates a shared
//! database second skips them instead of adding duplicates.
use sea_orm_migration::prelude::*;

use super::helpers::{add_column_if_missing, drop_column_if_present};

#[derive(DeriveMigrationName)]
pub struct Migration;

const TABLES: [&str; 2] = ["queries", "results"];
const DATASET_ID: &str = "dataset_id";
const UPDATED_AT: &str = "updated_at";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for table in TABLES {
            // `text` holding `uuid_hex`, like every other id column here.
            let dataset_id = ColumnDef::new(Alias::new(DATASET_ID))
                .text()
                .null()
                .to_owned();
            add_column_if_missing(manager, table, dataset_id).await?;
            let updated_at = ColumnDef::new(Alias::new(UPDATED_AT))
                .timestamp_with_time_zone()
                .null()
                .to_owned();
            add_column_if_missing(manager, table, updated_at).await?;
            manager
                .create_index(
                    Index::create()
                        .if_not_exists()
                        .name(index_name(table))
                        .table(Alias::new(table))
                        .col(Alias::new(DATASET_ID))
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for table in TABLES {
            // Index first, then columns — the reverse of `up`.
            let index = Index::drop()
                .if_exists()
                .name(index_name(table))
                .table(Alias::new(table))
                .to_owned();
            manager.drop_index(index).await?;
            drop_column_if_present(manager, table, UPDATED_AT).await?;
            drop_column_if_present(manager, table, DATASET_ID).await?;
        }
        Ok(())
    }
}

/// SQLAlchemy's default name for a single-column index.
fn index_name(table: &str) -> String {
    format!("ix_{table}_{DATASET_ID}")
}
