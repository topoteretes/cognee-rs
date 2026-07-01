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
/// sea-orm/sqlx give every pooled connection to a non-shared in-memory database
/// its own empty DB, so in-memory SQLite must be a single connection. `?cache=shared`
/// genuinely shares one DB across connections, so it is excluded. This is a backend
/// invariant, not tuning, which is why it is keyed on *in-memory* and not on the
/// `sqlite` scheme (file-backed SQLite can and should use a pool).
///
/// Both in-memory spellings are matched: the `:memory:` shorthand and the URI form
/// `file:name?mode=memory` — the latter is exclusive too unless `cache=shared` is set.
#[cfg(feature = "sqlite")]
fn is_exclusive_in_memory_sqlite(url: &str) -> bool {
    url.starts_with("sqlite")
        && (url.contains(":memory:") || url.contains("mode=memory"))
        && !url.contains("cache=shared")
}

/// Open a connection to the relational database with the default pool policy.
pub async fn connect(url: &str) -> Result<DatabaseConnection, DatabaseError> {
    connect_with_pool(url, PoolConfig::default()).await
}

/// Open a connection applying an explicit [`PoolConfig`].
///
/// The pool sizing comes entirely from `pool`. SQLite additionally needs
/// connection-level tuning that sea-orm's `ConnectOptions` cannot express — WAL for
/// file-backed databases (so a >1 pool actually buys reader/writer concurrency) and
/// disabled connection reaping for the single exclusive-in-memory connection (whose
/// reaping would silently discard the whole database) — so the SQLite path is built
/// directly on the sqlx pool. Server backends go through sea-orm unchanged.
pub async fn connect_with_pool(
    url: &str,
    pool: PoolConfig,
) -> Result<DatabaseConnection, DatabaseError> {
    #[cfg(feature = "sqlite")]
    if url.starts_with("sqlite") {
        return connect_sqlite(url, pool).await;
    }

    let mut opt = ConnectOptions::new(url.to_owned());
    opt.max_connections(pool.max_connections)
        .min_connections(pool.min_connections)
        .acquire_timeout(pool.acquire_timeout)
        .idle_timeout(pool.idle_timeout);

    Database::connect(opt)
        .await
        .map_err(|e| DatabaseError::ConnectionError(e.to_string()))
}

/// Build the SQLite connection pool directly on sqlx so per-connection pragmas and
/// per-pool reaping can be controlled precisely.
///
/// - **File-backed:** WAL + `synchronous=NORMAL` gives real reader/writer
///   concurrency (writers no longer block readers), which is the only thing that
///   justifies a multi-connection pool for SQLite; `busy_timeout` serializes the
///   inevitable writer-vs-writer contention instead of failing with `SQLITE_BUSY`.
/// - **Exclusive in-memory:** the entire database lives inside one connection, so it
///   is pinned to a single connection with *both* reapers disabled — sqlx's default
///   `idle_timeout`/`max_lifetime` would otherwise close it and reconnect to a fresh,
///   empty `:memory:` DB, silently dropping all data.
#[cfg(feature = "sqlite")]
async fn connect_sqlite(url: &str, pool: PoolConfig) -> Result<DatabaseConnection, DatabaseError> {
    use std::str::FromStr;

    use sea_orm::SqlxSqliteConnector;
    use sea_orm::sqlx::sqlite::{
        SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous,
    };

    let exclusive_in_memory = is_exclusive_in_memory_sqlite(url);

    let mut conn_opts = SqliteConnectOptions::from_str(url)
        .map_err(|e| DatabaseError::ConnectionError(e.to_string()))?
        .busy_timeout(Duration::from_secs(5));

    // WAL is silently ignored for :memory:, so only enable it for file-backed DBs.
    if !exclusive_in_memory {
        conn_opts = conn_opts
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .pragma("wal_autocheckpoint", "1000");
    }

    let mut pool_opts = SqlitePoolOptions::new()
        .max_connections(pool.max_connections)
        .min_connections(pool.min_connections)
        .acquire_timeout(pool.acquire_timeout)
        .idle_timeout(pool.idle_timeout);

    if exclusive_in_memory {
        pool_opts = pool_opts
            .max_connections(1)
            .min_connections(1)
            .idle_timeout(None)
            .max_lifetime(None);
    }

    let sqlx_pool = pool_opts
        .connect_with(conn_opts)
        .await
        .map_err(|e| DatabaseError::ConnectionError(e.to_string()))?;

    Ok(SqlxSqliteConnector::from_sqlx_sqlite_pool(sqlx_pool))
}

/// Run all pending migrations on an existing connection.
pub async fn initialize(db: &DatabaseConnection) -> Result<(), DatabaseError> {
    Migrator::up(db, None)
        .await
        .map_err(|e| DatabaseError::QueryError(e.to_string()))
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::is_exclusive_in_memory_sqlite as is_exclusive;

    #[test]
    fn detects_both_in_memory_spellings() {
        assert!(is_exclusive("sqlite::memory:"));
        assert!(is_exclusive("sqlite:file:pinned?mode=memory"));
    }

    #[test]
    fn shared_cache_in_memory_is_not_exclusive() {
        // `cache=shared` genuinely shares one DB across connections, so pooling is fine.
        assert!(!is_exclusive("sqlite::memory:?cache=shared"));
        assert!(!is_exclusive("sqlite:file:pinned?mode=memory&cache=shared"));
    }

    #[test]
    fn file_and_server_urls_are_not_exclusive() {
        assert!(!is_exclusive("sqlite://./cognee.db?mode=rwc"));
        assert!(!is_exclusive("postgres://user:pw@localhost/db"));
    }
}
