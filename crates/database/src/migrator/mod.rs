use sea_orm_migration::prelude::*;

mod m20250101_000001_initial_schema;
mod m20250201_000001_acl_tables;
mod m20250301_000001_add_importance_weight;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20250101_000001_initial_schema::Migration),
            Box::new(m20250201_000001_acl_tables::Migration),
            Box::new(m20250301_000001_add_importance_weight::Migration),
        ]
    }
}
