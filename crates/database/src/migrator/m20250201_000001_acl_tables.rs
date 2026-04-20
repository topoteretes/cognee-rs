use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // principals
        manager
            .create_table(
                Table::create()
                    .table(Principals::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Principals::Id)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Principals::PrincipalType).text().not_null())
                    .col(
                        ColumnDef::new(Principals::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Principals::UpdatedAt).timestamp_with_time_zone())
                    .to_owned(),
            )
            .await?;

        // permissions
        manager
            .create_table(
                Table::create()
                    .table(Permissions::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Permissions::Id)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Permissions::Name)
                            .text()
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(Permissions::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Permissions::UpdatedAt).timestamp_with_time_zone())
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_permissions_name")
                    .table(Permissions::Table)
                    .col(Permissions::Name)
                    .to_owned(),
            )
            .await?;

        // acls
        manager
            .create_table(
                Table::create()
                    .table(Acls::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Acls::Id).text().not_null().primary_key())
                    .col(ColumnDef::new(Acls::PrincipalId).text().not_null())
                    .col(ColumnDef::new(Acls::PermissionId).text().not_null())
                    .col(ColumnDef::new(Acls::DatasetId).text().not_null())
                    .col(
                        ColumnDef::new(Acls::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Acls::UpdatedAt).timestamp_with_time_zone())
                    .foreign_key(
                        ForeignKey::create()
                            .from(Acls::Table, Acls::PrincipalId)
                            .to(Principals::Table, Principals::Id),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Acls::Table, Acls::PermissionId)
                            .to(Permissions::Table, Permissions::Id),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Acls::Table, Acls::DatasetId)
                            .to(Datasets::Table, Datasets::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_acls_unique_grant")
                    .table(Acls::Table)
                    .col(Acls::PrincipalId)
                    .col(Acls::PermissionId)
                    .col(Acls::DatasetId)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // Seed the four permission rows (using 32-char hex IDs to match
        // the UUID format convention used throughout the Rust codebase).
        let db = manager.get_connection();
        db.execute_unprepared(
            "INSERT INTO permissions (id, name, created_at) VALUES
                ('00000000000000000000000000000001', 'read',   datetime('now')),
                ('00000000000000000000000000000002', 'write',  datetime('now')),
                ('00000000000000000000000000000003', 'delete', datetime('now')),
                ('00000000000000000000000000000004', 'share',  datetime('now'))
            ON CONFLICT (name) DO NOTHING",
        )
        .await?;

        // Retroactively grant all four permissions to each dataset's owner.
        // This ensures that existing datasets are not left in an inaccessible state.
        db.execute_unprepared(
            "INSERT INTO principals (id, type, created_at)
             SELECT DISTINCT owner_id, 'user', datetime('now')
             FROM datasets
             WHERE owner_id NOT IN (SELECT id FROM principals)
            ",
        )
        .await?;

        db.execute_unprepared(
            "INSERT INTO acls (id, principal_id, permission_id, dataset_id, created_at)
             SELECT
                 -- deterministic ID from (principal_id, permission_id, dataset_id)
                 lower(hex(randomblob(16))),
                 d.owner_id,
                 p.id,
                 d.id,
                 datetime('now')
             FROM datasets d
             CROSS JOIN permissions p
             WHERE NOT EXISTS (
                 SELECT 1 FROM acls a
                 WHERE a.principal_id = d.owner_id
                   AND a.permission_id = p.id
                   AND a.dataset_id = d.id
             )",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Acls::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Permissions::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Principals::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Principals {
    Table,
    Id,
    #[sea_orm(iden = "type")]
    PrincipalType,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Permissions {
    Table,
    Id,
    Name,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Acls {
    Table,
    Id,
    PrincipalId,
    PermissionId,
    DatasetId,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Datasets {
    Table,
    Id,
}
