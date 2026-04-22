use sea_orm_migration::prelude::*;

mod m20250402_000001_session_qa_entries;
mod m20250423_000002_session_qa_feedback_fields;

pub struct SessionMigrator;

#[async_trait::async_trait]
impl MigratorTrait for SessionMigrator {
    fn migration_table_name() -> DynIden {
        Alias::new("seaql_session_migrations").into_iden()
    }

    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20250402_000001_session_qa_entries::Migration),
            Box::new(m20250423_000002_session_qa_feedback_fields::Migration),
        ]
    }
}
