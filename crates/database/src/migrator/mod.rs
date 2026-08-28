use std::sync::OnceLock;

use sea_orm_migration::prelude::*;

mod m20260914_000001_baseline;
mod m20260915_000001_pipeline_run_claims;

pub struct Migrator;

/// Supplier of migrations that run after [`core_migrations`].
pub type ExtraMigrations = fn() -> Vec<Box<dyn MigrationTrait>>;

static EXTRA_MIGRATIONS: OnceLock<ExtraMigrations> = OnceLock::new();

/// Register migrations to run after the core set.
///
/// Needed because sea-orm rejects a database whose `seaql_migrations` table
/// records a migration the running migrator does not know about
/// ("Migration file of version '…' is missing, this migration has been applied
/// but its file is missing"). A downstream crate that adds tables must therefore
/// extend the migrator that [`crate::initialize`] runs, rather than applying its
/// own migrator afterwards — the closed `cognee-access-control` auth tables are
/// the motivating case.
///
/// Call before the first connection is initialized; a second call is ignored and
/// reports `Err`. OSS-only builds never call it and are unaffected.
pub fn set_extra_migrations(supplier: ExtraMigrations) -> Result<(), ExtraMigrations> {
    EXTRA_MIGRATIONS.set(supplier)
}

/// OSS core migrations, exposed so closed downstream crates (e.g. the
/// closed `cognee-access-control::Migrator`) can compose this list with
/// their own additional migrations and register the merged set.
///
/// The OSS [`Migrator`] simply delegates to this accessor so behaviour is
/// unchanged for OSS-only builds.
pub fn core_migrations() -> Vec<Box<dyn MigrationTrait>> {
    vec![
        Box::new(m20260914_000001_baseline::Migration),
        Box::new(m20260915_000001_pipeline_run_claims::Migration),
    ]
}

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        let mut migrations = core_migrations();
        if let Some(supplier) = EXTRA_MIGRATIONS.get() {
            migrations.extend(supplier());
        }
        migrations
    }
}
