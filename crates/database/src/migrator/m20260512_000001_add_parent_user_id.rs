//! Add `parent_user_id` column to `users` table.
//!
//! Used by the SaaS layer to model "agent" users that inherit their parent's
//! subscription. Nullable — regular users have `parent_user_id = NULL`.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .add_column_if_not_exists(ColumnDef::new(Users::ParentUserId).text())
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // SQLite does not support DROP COLUMN via ALTER TABLE in SeaORM.
        // The column is nullable, so leaving it in place is harmless.
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Users {
    Table,
    ParentUserId,
}
