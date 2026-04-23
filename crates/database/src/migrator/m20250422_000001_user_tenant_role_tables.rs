use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // tenants (must be created before users, which references tenants)
        manager
            .create_table(
                Table::create()
                    .table(Tenants::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Tenants::Id).text().not_null().primary_key())
                    .col(ColumnDef::new(Tenants::Name).text().not_null().unique_key())
                    .col(ColumnDef::new(Tenants::OwnerId).text().not_null())
                    .col(
                        ColumnDef::new(Tenants::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Tenants::UpdatedAt).timestamp_with_time_zone())
                    .foreign_key(
                        ForeignKey::create()
                            .from(Tenants::Table, Tenants::Id)
                            .to(Principals::Table, Principals::Id),
                    )
                    .to_owned(),
            )
            .await?;

        // users
        manager
            .create_table(
                Table::create()
                    .table(Users::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Users::Id).text().not_null().primary_key())
                    .col(ColumnDef::new(Users::Email).text().not_null().unique_key())
                    .col(
                        ColumnDef::new(Users::IsActive)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(Users::IsSuperuser)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(ColumnDef::new(Users::TenantId).text())
                    .col(
                        ColumnDef::new(Users::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Users::UpdatedAt).timestamp_with_time_zone())
                    .foreign_key(
                        ForeignKey::create()
                            .from(Users::Table, Users::Id)
                            .to(Principals::Table, Principals::Id),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Users::Table, Users::TenantId)
                            .to(Tenants::Table, Tenants::Id),
                    )
                    .to_owned(),
            )
            .await?;

        // roles
        manager
            .create_table(
                Table::create()
                    .table(Roles::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Roles::Id).text().not_null().primary_key())
                    .col(ColumnDef::new(Roles::Name).text().not_null())
                    .col(ColumnDef::new(Roles::TenantId).text().not_null())
                    .col(
                        ColumnDef::new(Roles::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Roles::UpdatedAt).timestamp_with_time_zone())
                    .foreign_key(
                        ForeignKey::create()
                            .from(Roles::Table, Roles::Id)
                            .to(Principals::Table, Principals::Id),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Roles::Table, Roles::TenantId)
                            .to(Tenants::Table, Tenants::Id),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_roles_tenant_name")
                    .table(Roles::Table)
                    .col(Roles::TenantId)
                    .col(Roles::Name)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // user_tenants junction
        manager
            .create_table(
                Table::create()
                    .table(UserTenants::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(UserTenants::UserId).text().not_null())
                    .col(ColumnDef::new(UserTenants::TenantId).text().not_null())
                    .col(
                        ColumnDef::new(UserTenants::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(UserTenants::UserId)
                            .col(UserTenants::TenantId),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(UserTenants::Table, UserTenants::UserId)
                            .to(Users::Table, Users::Id),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(UserTenants::Table, UserTenants::TenantId)
                            .to(Tenants::Table, Tenants::Id),
                    )
                    .to_owned(),
            )
            .await?;

        // user_roles junction
        manager
            .create_table(
                Table::create()
                    .table(UserRoles::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(UserRoles::UserId).text().not_null())
                    .col(ColumnDef::new(UserRoles::RoleId).text().not_null())
                    .col(
                        ColumnDef::new(UserRoles::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(UserRoles::UserId)
                            .col(UserRoles::RoleId),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(UserRoles::Table, UserRoles::UserId)
                            .to(Users::Table, Users::Id),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(UserRoles::Table, UserRoles::RoleId)
                            .to(Roles::Table, Roles::Id),
                    )
                    .to_owned(),
            )
            .await?;

        // Seed the default user (matches `default_user_id = "00000000-…-000000000000"`).
        let db = manager.get_connection();
        db.execute_unprepared(
            "INSERT INTO principals (id, type, created_at)
             VALUES ('00000000000000000000000000000000', 'user', datetime('now'))
             ON CONFLICT (id) DO NOTHING",
        )
        .await?;

        db.execute_unprepared(
            "INSERT INTO users (id, email, is_active, is_superuser, tenant_id, created_at)
             VALUES ('00000000000000000000000000000000', 'default_user@example.com', 1, 1, NULL, datetime('now'))
             ON CONFLICT (id) DO NOTHING",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(UserRoles::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(UserTenants::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Roles::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Users::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Tenants::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Principals {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Tenants {
    Table,
    Id,
    Name,
    OwnerId,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
    Email,
    IsActive,
    IsSuperuser,
    TenantId,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Roles {
    Table,
    Id,
    Name,
    TenantId,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum UserTenants {
    Table,
    UserId,
    TenantId,
    CreatedAt,
}

#[derive(DeriveIden)]
enum UserRoles {
    Table,
    UserId,
    RoleId,
    CreatedAt,
}
