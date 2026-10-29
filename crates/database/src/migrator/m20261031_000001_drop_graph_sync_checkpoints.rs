//! Drops `graph_sync_checkpoints`, the improve graph-to-session checkpoint table.
//!
//! The table held one `(key, ts)` row per synced session so the improve
//! `sync_graph_to_session` stage could resume. Python deleted that stage in
//! `3401da38a` ("Remove graph session sync") and wrote nothing in its place;
//! Rust removed the stage in the same change (SDK-1039), so nothing reads or
//! writes the table any more.
//!
//! `up` is idempotent; `down` recreates the table so a rollback of this
//! revision leaves the schema the previous revision expects.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(GraphSyncCheckpoints::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(GraphSyncCheckpoints::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(GraphSyncCheckpoints::Key)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(GraphSyncCheckpoints::Ts)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum GraphSyncCheckpoints {
    Table,
    Key,
    Ts,
}
