use sea_orm_migration::prelude::*;

mod m20260914_000001_baseline;
mod m20260914_000002_external_event_id;

pub struct SessionMigrator;

#[async_trait::async_trait]
impl MigratorTrait for SessionMigrator {
    fn migration_table_name() -> DynIden {
        Alias::new("seaql_session_migrations").into_iden()
    }

    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260914_000001_baseline::Migration),
            Box::new(m20260914_000002_external_event_id::Migration),
        ]
    }
}
