use std::time::Duration;

use sea_orm::{ConnectOptions, Database, DatabaseConnection};
use sea_orm_migration::MigratorTrait;

use crate::migrator::Migrator;
use crate::types::DatabaseError;

/// Pool sizing for server-backed databases (Postgres). The relational crate and
/// the Postgres graph adapter share this pool concurrently during ingestion, so
/// allow enough connections for both while bounding contention. `acquire_timeout`
/// surfaces pool exhaustion as a prompt error instead of a silent hang.
const POOL_MAX_CONNECTIONS: u32 = 10;
const POOL_MIN_CONNECTIONS: u32 = 1;
const POOL_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(30);
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(600);

/// Open a connection to the relational database with an explicit pool config.
pub async fn connect(url: &str) -> Result<DatabaseConnection, DatabaseError> {
    let mut opt = ConnectOptions::new(url.to_owned());
    opt.acquire_timeout(POOL_ACQUIRE_TIMEOUT);

    // SQLite (the default/embedded backend, including `sqlite::memory:`) must use
    // a single connection: writes serialise on one file lock anyway, and every
    // extra pooled connection to `:memory:` would get its own separate empty
    // database. A server backend (Postgres) gets a real pool shared with the
    // graph adapter.
    if url.starts_with("sqlite") {
        opt.max_connections(1).min_connections(1);
    } else {
        opt.max_connections(POOL_MAX_CONNECTIONS)
            .min_connections(POOL_MIN_CONNECTIONS)
            .idle_timeout(POOL_IDLE_TIMEOUT);
    }

    Database::connect(opt)
        .await
        .map_err(|e| DatabaseError::ConnectionError(e.to_string()))
}

/// Run all pending migrations on an existing connection.
pub async fn initialize(db: &DatabaseConnection) -> Result<(), DatabaseError> {
    Migrator::up(db, None)
        .await
        .map_err(|e| DatabaseError::QueryError(e.to_string()))
}
