//! P5 migration: add the three default-permission tables and the
//! `principal_configuration` table, plus the additive indexes from
//! `tenants.md §4`.
//!
//! All other RBAC tables (`principals`, `permissions`, `acls`, `tenants`,
//! `users`, `roles`, `user_tenants`, `user_roles`) and the seeded permission
//! rows are owned by the earlier migrations
//! (`m20250201_000001_acl_tables.rs`, `m20250422_000001_user_tenant_role_tables.rs`).
//!
//! The `down()` direction is a no-op per `tenants.md §11.2` — dropping these
//! tables is destructive on Python-seeded DBs.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // role_default_permissions
        manager
            .create_table(
                Table::create()
                    .table(RoleDefaultPermissions::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(RoleDefaultPermissions::RoleId)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(RoleDefaultPermissions::PermissionId)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(RoleDefaultPermissions::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(RoleDefaultPermissions::RoleId)
                            .col(RoleDefaultPermissions::PermissionId),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(
                                RoleDefaultPermissions::Table,
                                RoleDefaultPermissions::RoleId,
                            )
                            .to(Roles::Table, Roles::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(
                                RoleDefaultPermissions::Table,
                                RoleDefaultPermissions::PermissionId,
                            )
                            .to(Permissions::Table, Permissions::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // user_default_permissions
        manager
            .create_table(
                Table::create()
                    .table(UserDefaultPermissions::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(UserDefaultPermissions::UserId)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(UserDefaultPermissions::PermissionId)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(UserDefaultPermissions::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(UserDefaultPermissions::UserId)
                            .col(UserDefaultPermissions::PermissionId),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(
                                UserDefaultPermissions::Table,
                                UserDefaultPermissions::UserId,
                            )
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(
                                UserDefaultPermissions::Table,
                                UserDefaultPermissions::PermissionId,
                            )
                            .to(Permissions::Table, Permissions::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // tenant_default_permissions
        manager
            .create_table(
                Table::create()
                    .table(TenantDefaultPermissions::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(TenantDefaultPermissions::TenantId)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TenantDefaultPermissions::PermissionId)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TenantDefaultPermissions::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(TenantDefaultPermissions::TenantId)
                            .col(TenantDefaultPermissions::PermissionId),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(
                                TenantDefaultPermissions::Table,
                                TenantDefaultPermissions::TenantId,
                            )
                            .to(Tenants::Table, Tenants::Id),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(
                                TenantDefaultPermissions::Table,
                                TenantDefaultPermissions::PermissionId,
                            )
                            .to(Permissions::Table, Permissions::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // principal_configuration — per-user named JSON blobs
        // (routers/configuration.md §1).
        manager
            .create_table(
                Table::create()
                    .table(PrincipalConfiguration::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(PrincipalConfiguration::Id)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(PrincipalConfiguration::OwnerId)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(PrincipalConfiguration::Name)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(PrincipalConfiguration::Configuration)
                            .json()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(PrincipalConfiguration::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(PrincipalConfiguration::UpdatedAt)
                            .timestamp_with_time_zone(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(
                                PrincipalConfiguration::Table,
                                PrincipalConfiguration::OwnerId,
                            )
                            .to(Principals::Table, Principals::Id),
                    )
                    .to_owned(),
            )
            .await?;

        // ── Additive indexes (tenants.md §4) ─────────────────────────────────
        manager
            .create_index(
                Index::create()
                    .name("ix_acls_principal_dataset")
                    .table(Acls::Table)
                    .col(Acls::PrincipalId)
                    .col(Acls::DatasetId)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("ix_acls_dataset")
                    .table(Acls::Table)
                    .col(Acls::DatasetId)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("ix_user_roles_user")
                    .table(UserRoles::Table)
                    .col(UserRoles::UserId)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("ix_user_tenants_user")
                    .table(UserTenants::Table)
                    .col(UserTenants::UserId)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("ix_roles_tenant")
                    .table(Roles::Table)
                    .col(Roles::TenantId)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Per tenants.md §11.2: down is intentionally a no-op.
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Principals {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Permissions {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Tenants {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Roles {
    Table,
    Id,
    TenantId,
}

#[derive(DeriveIden)]
enum Acls {
    Table,
    PrincipalId,
    DatasetId,
}

#[derive(DeriveIden)]
enum UserRoles {
    Table,
    UserId,
}

#[derive(DeriveIden)]
enum UserTenants {
    Table,
    UserId,
}

#[derive(DeriveIden)]
enum RoleDefaultPermissions {
    Table,
    RoleId,
    PermissionId,
    CreatedAt,
}

#[derive(DeriveIden)]
enum UserDefaultPermissions {
    Table,
    UserId,
    PermissionId,
    CreatedAt,
}

#[derive(DeriveIden)]
enum TenantDefaultPermissions {
    Table,
    TenantId,
    PermissionId,
    CreatedAt,
}

#[derive(DeriveIden)]
enum PrincipalConfiguration {
    Table,
    Id,
    OwnerId,
    Name,
    Configuration,
    CreatedAt,
    UpdatedAt,
}
