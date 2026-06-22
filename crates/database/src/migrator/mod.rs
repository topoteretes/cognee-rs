use sea_orm_migration::prelude::*;

mod m20260914_000001_baseline;

pub struct Migrator;

/// OSS core migrations, exposed so closed downstream crates (e.g. the
/// closed `cognee-access-control::Migrator`) can compose this list with
/// their own additional migrations and register the merged set.
///
/// The OSS [`Migrator`] simply delegates to this accessor so behaviour is
/// unchanged for OSS-only builds.
pub fn core_migrations() -> Vec<Box<dyn MigrationTrait>> {
    vec![Box::new(m20260914_000001_baseline::Migration)]
}

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        core_migrations()
    }
}
