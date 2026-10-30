use sea_orm_migration::prelude::*;

mod m20260914_000001_baseline;
mod m20261028_000001_session_context_entries;
mod m20261029_000001_drop_session_graph_context;
mod m20261030_000001_session_context_lineage;

pub struct SessionMigrator;

#[async_trait::async_trait]
impl MigratorTrait for SessionMigrator {
    fn migration_table_name() -> DynIden {
        Alias::new("seaql_session_migrations").into_iden()
    }

    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260914_000001_baseline::Migration),
            Box::new(m20261028_000001_session_context_entries::Migration),
            Box::new(m20261029_000001_drop_session_graph_context::Migration),
            Box::new(m20261030_000001_session_context_lineage::Migration),
        ]
    }
}
