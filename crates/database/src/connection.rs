use std::time::Duration;

use sea_orm::{ConnectOptions, Database, DatabaseConnection};
use sea_orm_migration::MigratorTrait;

use crate::migrator::Migrator;
use crate::types::DatabaseError;

/// Connection-pool sizing policy.
///
/// This is tunable performance policy, owned by the layer that selects the
/// database URL (`lib`/`http-server` config), not by the generic `connect`
/// plumbing. `connect` applies [`PoolConfig::default`]; callers that want to
/// size the pool themselves use [`connect_with_pool`]. The in-memory SQLite
/// single-connection invariant (see [`connect_with_pool`]) is enforced on top
/// regardless of these values, because it is a backend correctness requirement
/// rather than tuning.
#[derive(Clone, Copy, Debug)]
pub struct PoolConfig {
    pub max_connections: u32,
    pub min_connections: u32,
    pub acquire_timeout: Duration,
    pub idle_timeout: Duration,
}

impl Default for PoolConfig {
    /// Sized for the relational crate and the Postgres graph adapter sharing one
    /// pool during ingestion. `acquire_timeout` surfaces pool exhaustion as a
    /// prompt error instead of a silent hang.
    fn default() -> Self {
        Self {
            max_connections: 10,
            min_connections: 1,
            acquire_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(600),
        }
    }
}

/// True only for an *exclusive* in-memory SQLite database.
///
/// sea-orm/sqlx give every pooled connection to a non-shared `:memory:` database
/// its own empty DB, so in-memory SQLite must be a single connection. `?cache=shared`
/// genuinely shares one DB across connections, so it is excluded. This is a backend
/// invariant, not tuning, which is why it is keyed on *in-memory* and not on the
/// `sqlite` scheme (file-backed SQLite can and should use a pool).
fn is_exclusive_in_memory_sqlite(url: &str) -> bool {
    url.starts_with("sqlite") && url.contains(":memory:") && !url.contains("cache=shared")
}

/// Open a connection to the relational database with the default pool policy.
pub async fn connect(url: &str) -> Result<DatabaseConnection, DatabaseError> {
    connect_with_pool(url, PoolConfig::default()).await
}

/// Open a connection applying an explicit [`PoolConfig`].
///
/// The pool sizing comes entirely from `pool`; the only thing decided here is the
/// exclusive-in-memory-SQLite invariant, which forces a single connection on top
/// of whatever sizing was requested.
pub async fn connect_with_pool(
    url: &str,
    pool: PoolConfig,
) -> Result<DatabaseConnection, DatabaseError> {
    let mut opt = ConnectOptions::new(url.to_owned());
    opt.max_connections(pool.max_connections)
        .min_connections(pool.min_connections)
        .acquire_timeout(pool.acquire_timeout)
        .idle_timeout(pool.idle_timeout);

    if is_exclusive_in_memory_sqlite(url) {
        opt.max_connections(1).min_connections(1);
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
