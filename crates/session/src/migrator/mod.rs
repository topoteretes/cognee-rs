use sea_orm_migration::prelude::*;

mod m20250402_000001_session_qa_entries;

pub struct SessionMigrator;

#[async_trait::async_trait]
impl MigratorTrait for SessionMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(m20250402_000001_session_qa_entries::Migration)]
    }
}
