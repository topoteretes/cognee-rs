//! P1 migration: add `hashed_password` + `is_verified` to `users`;
//! create `user_api_key` table.
//!
//! Idempotent — uses `IF NOT EXISTS` / `add_column_if_not_exists` so it is safe
//! to run against a Python-seeded SQLite/Postgres file.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // (a) Add hashed_password column to users (NOT NULL DEFAULT '' so existing rows
        //     get an empty string; callers must set a real hash before use).
        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .add_column_if_not_exists(
                        ColumnDef::new(Users::HashedPassword)
                            .text()
                            .not_null()
                            .default(""),
                    )
                    .to_owned(),
            )
            .await?;

        // (b) Add is_verified column to users (NOT NULL DEFAULT TRUE — matches Python
        //     cognee override where newly registered users start as verified).
        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .add_column_if_not_exists(
                        ColumnDef::new(Users::IsVerified)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .to_owned(),
            )
            .await?;

        // (c) Create user_api_key table.
        // `api_key` is intentionally NOT unique — matches Python's schema (256-bit
        // entropy makes collisions astronomically unlikely; adding a UNIQUE constraint
        // would break Python-compat DB writes).
        manager
            .create_table(
                Table::create()
                    .table(UserApiKey::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(UserApiKey::Id)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(UserApiKey::UserId).text().not_null())
                    .col(ColumnDef::new(UserApiKey::ApiKey).text().not_null())
                    .col(ColumnDef::new(UserApiKey::Label).text())
                    .col(ColumnDef::new(UserApiKey::Name).text())
                    .col(
                        ColumnDef::new(UserApiKey::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(UserApiKey::ExpiresAt).timestamp_with_time_zone())
                    // FK: user_id → principals.id ON DELETE CASCADE
                    .foreign_key(
                        ForeignKey::create()
                            .from(UserApiKey::Table, UserApiKey::UserId)
                            .to(Principals::Table, Principals::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Index on user_id for efficient per-user key lookups.
        manager
            .create_index(
                Index::create()
                    .name("idx_user_api_key_user_id")
                    .table(UserApiKey::Table)
                    .col(UserApiKey::UserId)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(UserApiKey::Table).to_owned())
            .await?;
        // Note: SeaORM does not support DROP COLUMN on SQLite in alter_table; we
        // leave hashed_password / is_verified on users during rollback — acceptable
        // because the down migration is only used in dev/test environments.
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Principals {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Users {
    Table,
    HashedPassword,
    IsVerified,
}

#[derive(DeriveIden)]
enum UserApiKey {
    Table,
    Id,
    UserId,
    ApiKey,
    Label,
    Name,
    CreatedAt,
    ExpiresAt,
}
