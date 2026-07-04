use std::time::Duration;

use sea_orm::{ConnectOptions, Database, DatabaseConnection};
use sea_orm_migration::MigratorTrait;

use crate::migrator::Migrator;
use crate::types::DatabaseError;

/// Connection-pool sizing policy.
///
/// This is tunable performance policy, owned by the layer that selects the
/// database URL: `lib` and `http-server` build it from the `DB_POOL_*`
/// environment variables (see `docs/configuration.md`) and pass it to
/// [`connect_with_pool`]; plain [`connect`] applies [`PoolConfig::default`].
/// In-memory SQLite URLs override parts of this sizing at connect time because
/// correctness requires it (see `connect_sqlite`).
#[derive(Clone, Copy, Debug)]
pub struct PoolConfig {
    pub max_connections: u32,
    pub min_connections: u32,
    pub acquire_timeout: Duration,
    pub idle_timeout: Duration,
}

impl Default for PoolConfig {
    /// The values deliberately pin sqlx's own pool defaults, except
    /// `min_connections = 1` (sqlx defaults to 0) so one warm connection
    /// survives idle periods. This pool serves only the relational database:
    /// the Postgres graph and vector adapters (`PgGraphAdapter`,
    /// `PgVectorAdapter`) open their own separate pools. `acquire_timeout`
    /// surfaces pool exhaustion as a prompt error instead of a silent hang.
    fn default() -> Self {
        Self {
            max_connections: 10,
            min_connections: 1,
            acquire_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(600),
        }
    }
}

/// How a SQLite URL behaves at connect time, derived from its path and query
/// parameters. Parameters are matched exactly after splitting the URL, never
/// by substring over the whole string: URLs are user-supplied, and a file
/// path that merely contains `mode=memory` must not be misclassified.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SqliteUrlKind {
    /// The `:memory:` path or the `mode=memory` query parameter.
    in_memory: bool,
    /// Explicit `cache=shared`. (sqlx 0.8 internally upgrades plain
    /// `:memory:` to a uniquely named shared-cache database as well; this
    /// flag only tracks what the URL asked for.)
    shared_cache: bool,
    /// `mode=ro` or `immutable=1|true`: the connection cannot write, so
    /// journal-mode pragmas must not be issued on it.
    read_only: bool,
}

fn classify_sqlite_url(url: &str) -> SqliteUrlKind {
    let rest = url
        .strip_prefix("sqlite://")
        .or_else(|| url.strip_prefix("sqlite:"))
        .unwrap_or(url);
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let path = path.strip_prefix("file:").unwrap_or(path);

    let mut kind = SqliteUrlKind {
        in_memory: path == ":memory:",
        ..SqliteUrlKind::default()
    };
    for param in query.split('&') {
        match param {
            "mode=memory" => kind.in_memory = true,
            "cache=shared" => kind.shared_cache = true,
            "mode=ro" | "immutable=1" | "immutable=true" => kind.read_only = true,
            _ => {}
        }
    }
    kind
}

/// True when a SQLite URL points at an in-memory database, in either spelling
/// (`sqlite::memory:` / `sqlite://:memory:` or `?mode=memory`).
///
/// Shared with `cognee-lib`, which must skip filesystem preparation (parent
/// directory creation) for such URLs; keeping one predicate here prevents the
/// layers from diverging on what counts as in-memory.
pub fn sqlite_url_is_in_memory(url: &str) -> bool {
    url.starts_with("sqlite") && classify_sqlite_url(url).in_memory
}

/// Open a connection to the relational database with the default pool policy.
pub async fn connect(url: &str) -> Result<DatabaseConnection, DatabaseError> {
    connect_with_pool(url, PoolConfig::default()).await
}

/// Open a connection applying an explicit [`PoolConfig`].
///
/// The pool sizing comes entirely from `pool`. SQLite additionally needs
/// connection-level tuning that sea-orm's `ConnectOptions` cannot express
/// (journal-mode pragmas, disabling the pool reapers), so the SQLite path is
/// built directly on the sqlx pool. Server backends go through sea-orm
/// unchanged.
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

/// Build the SQLite connection pool directly on sqlx so per-connection pragmas
/// and per-pool reaping can be controlled precisely.
///
/// - **File-backed, read-write:** WAL + `synchronous=NORMAL` gives real
///   reader/writer concurrency (writers no longer block readers), which is
///   what justifies a multi-connection pool for SQLite. Note the durability
///   trade: NORMAL under WAL can lose the last transactions on power loss
///   (not on process crash), and converting a database to WAL leaves recent
///   commits in the `-wal` sidecar until checkpoint.
/// - **Read-only (`mode=ro` / `immutable`):** no pragmas are issued —
///   `PRAGMA journal_mode=WAL` writes to the database file and would fail the
///   connection on a read-only open or filesystem.
/// - **In-memory (shared or not):** the database only lives as long as its
///   connections, so both pool reapers are disabled — sqlx's default
///   `idle_timeout`/`max_lifetime` would close an idle connection and
///   reconnect to a fresh, empty database — and at least one connection is
///   kept open. A non-shared in-memory URL is additionally pinned to exactly
///   one connection: defensive, since sqlx 0.8 internally rewrites `:memory:`
///   to a uniquely named shared-cache database, but the invariant that
///   matters (never drop to zero connections) does not depend on that
///   implementation detail.
#[cfg(feature = "sqlite")]
async fn connect_sqlite(url: &str, pool: PoolConfig) -> Result<DatabaseConnection, DatabaseError> {
    use std::str::FromStr;

    use sea_orm::SqlxSqliteConnector;
    use sea_orm::sqlx::ConnectOptions as _;
    use sea_orm::sqlx::sqlite::{
        SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous,
    };

    let kind = classify_sqlite_url(url);

    // Statement logging at INFO matches sea-orm's `ConnectOptions` default,
    // which the Postgres path still goes through; raw sqlx defaults to DEBUG.
    let mut conn_opts = SqliteConnectOptions::from_str(url)
        .map_err(|e| DatabaseError::ConnectionError(e.to_string()))?
        .log_statements(log::LevelFilter::Info);

    if !kind.in_memory && !kind.read_only {
        conn_opts = conn_opts
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal);
    }

    let mut pool_opts = SqlitePoolOptions::new()
        .max_connections(pool.max_connections)
        .min_connections(pool.min_connections)
        .acquire_timeout(pool.acquire_timeout)
        .idle_timeout(pool.idle_timeout);

    if kind.in_memory {
        pool_opts = pool_opts
            .min_connections(pool.min_connections.max(1))
            .idle_timeout(None)
            .max_lifetime(None);
        if !kind.shared_cache {
            pool_opts = pool_opts.max_connections(1).min_connections(1);
        }
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

#[cfg(test)]
mod tests {
    use super::{SqliteUrlKind, classify_sqlite_url, sqlite_url_is_in_memory};

    #[test]
    fn detects_in_memory_spellings() {
        for url in [
            "sqlite::memory:",
            "sqlite://:memory:",
            "sqlite:file:pinned?mode=memory",
            "sqlite::memory:?cache=shared",
        ] {
            assert!(classify_sqlite_url(url).in_memory, "{url}");
            assert!(sqlite_url_is_in_memory(url), "{url}");
        }
    }

    #[test]
    fn detects_shared_cache_only_when_explicit() {
        assert!(classify_sqlite_url("sqlite::memory:?cache=shared").shared_cache);
        assert!(classify_sqlite_url("sqlite:file:x?mode=memory&cache=shared").shared_cache);
        assert!(!classify_sqlite_url("sqlite::memory:").shared_cache);
        assert!(!classify_sqlite_url("sqlite:file:x?cache=private").shared_cache);
    }

    #[test]
    fn detects_read_only_opens() {
        assert!(classify_sqlite_url("sqlite://./a.db?mode=ro").read_only);
        assert!(classify_sqlite_url("sqlite:a.db?immutable=1").read_only);
        assert!(classify_sqlite_url("sqlite:a.db?immutable=true").read_only);
        assert!(!classify_sqlite_url("sqlite://./a.db?mode=rwc").read_only);
        assert!(!classify_sqlite_url("sqlite://./a.db?mode=rw").read_only);
    }

    #[test]
    fn file_paths_are_never_misclassified_by_substring() {
        // Query parameters are matched exactly, so path contents cannot leak
        // into the classification.
        let kind = classify_sqlite_url("sqlite:///tmp/mode=memory/app.db?mode=rwc");
        assert_eq!(kind, SqliteUrlKind::default());
        assert!(!sqlite_url_is_in_memory(
            "sqlite:///tmp/mode=memory/app.db?mode=rwc"
        ));
    }

    #[test]
    fn non_sqlite_urls_are_not_in_memory() {
        assert!(!sqlite_url_is_in_memory("postgres://user:pw@localhost/db"));
    }
}
