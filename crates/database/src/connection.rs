use std::time::Duration;

use sea_orm::{ConnectOptions, Database, DatabaseConnection};
use sea_orm_migration::MigratorTrait;

use crate::migrator::Migrator;
use crate::types::DatabaseError;

/// Relational connection-pool sizing, applied by [`connect`].
///
/// `POOL_MAX_CONNECTIONS` and `POOL_MIN_CONNECTIONS` are sqlx's own pool
/// defaults, kept deliberately rather than invented. No benchmark motivates a
/// different ceiling, and an embedded SQLite database has one writer regardless,
/// so the pool only ever buys concurrent readers under WAL; a larger ceiling
/// would add contention, not throughput. `min = 0` lets a file database fall
/// back to zero connections when idle and release its `-wal`/`-shm` sidecars.
/// Only an in-memory database must never drop to zero connections, and that
/// branch sets `min` explicitly (see [`connect_sqlite`]).
///
/// The pool serves only the relational database: the Postgres graph and vector
/// adapters (`PgGraphAdapter`, `PgVectorAdapter`) open their own separate pools.
/// `POOL_ACQUIRE_TIMEOUT` surfaces pool exhaustion as a prompt error instead of
/// a silent hang.
const POOL_MAX_CONNECTIONS: u32 = 10;
const POOL_MIN_CONNECTIONS: u32 = 0;
const POOL_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(30);
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(600);

/// SQLite lock-wait ceiling, matching Python's `SqlAlchemyAdapter`
/// (`busy_timeout=120000`, added for the "database is locked" fix in
/// topoteretes/cognee#2717). sqlx defaults to 5s, which `upsert_provenance_graph`
/// can exceed on a slow device: it holds the single writer lock across the whole
/// node+edge batch group, so a second writer waiting on that lock needs a
/// ceiling above the group's commit time or the wait surfaces as `SQLITE_BUSY`.
#[cfg(feature = "sqlite")]
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(120);

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

/// Open a connection to the relational database.
///
/// SQLite needs connection-level tuning that sea-orm's `ConnectOptions` cannot
/// express (journal-mode pragmas, `busy_timeout`, disabling the pool reapers),
/// so the SQLite path is built directly on the sqlx pool. Server backends go
/// through sea-orm unchanged.
pub async fn connect(url: &str) -> Result<DatabaseConnection, DatabaseError> {
    #[cfg(feature = "sqlite")]
    if url.starts_with("sqlite") {
        return connect_sqlite(url).await;
    }

    let mut opt = ConnectOptions::new(url.to_owned());
    opt.max_connections(POOL_MAX_CONNECTIONS)
        .min_connections(POOL_MIN_CONNECTIONS)
        .acquire_timeout(POOL_ACQUIRE_TIMEOUT)
        .idle_timeout(POOL_IDLE_TIMEOUT);

    Database::connect(opt)
        .await
        .map_err(|e| DatabaseError::ConnectionError(e.to_string()))
}

/// Build the SQLite connection pool directly on sqlx so per-connection pragmas
/// and per-pool reaping can be controlled precisely.
///
/// - **File-backed, writable:** WAL + `synchronous=NORMAL` gives real
///   reader/writer concurrency (writers no longer block readers), which is
///   what justifies a multi-connection pool for SQLite. `busy_timeout` makes
///   the inevitable writer-vs-writer contention wait for the lock rather than
///   failing immediately with `SQLITE_BUSY`. Note the durability trade: NORMAL
///   under WAL can lose the last transactions on power loss (not on process
///   crash), and converting a database to WAL leaves recent commits in the
///   `-wal` sidecar until checkpoint.
/// - **Read-only (`mode=ro` / `immutable`, or a file that is not writable):**
///   the connection is opened read-only and no journal-mode pragma is issued.
///   `PRAGMA journal_mode=WAL` writes to the database file and would fail the
///   connect on a read-only open, a read-only mount, or a read-only file —
///   cases that served reads before this crate configured WAL. Writability is
///   probed on the filesystem, not inferred from the URL alone (see
///   [`sqlite_path_is_writable`]).
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
async fn connect_sqlite(url: &str) -> Result<DatabaseConnection, DatabaseError> {
    use std::str::FromStr;

    use sea_orm::SqlxSqliteConnector;
    use sea_orm::sqlx::ConnectOptions as _;
    use sea_orm::sqlx::sqlite::{
        SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous,
    };

    let kind = classify_sqlite_url(url);

    // Statement logging at INFO matches sea-orm's `ConnectOptions` default,
    // which the Postgres path still goes through; raw sqlx defaults to DEBUG.
    // `busy_timeout` lets a writer wait for the lock (WAL still serializes
    // writers) instead of erroring out immediately with `SQLITE_BUSY`.
    let mut conn_opts = SqliteConnectOptions::from_str(url)
        .map_err(|e| DatabaseError::ConnectionError(e.to_string()))?
        .log_statements(log::LevelFilter::Info)
        .busy_timeout(SQLITE_BUSY_TIMEOUT);

    // In-memory has no file to journal, and sqlx's default WAL is a no-op there.
    if !kind.in_memory {
        // Probe the driver's own filename rather than the raw URL: sqlx
        // percent-decodes the path while parsing, so re-deriving it here would
        // test a path that does not exist (`my%20app.db`) and wrongly report a
        // read-only file as writable.
        let writable = sqlite_path_is_writable(conn_opts.get_filename());
        if kind.read_only || !writable {
            // A read-only URL, or a file we cannot write (read-only mount or
            // permissions): open read-only so sqlx issues no journal-mode
            // pragma. `PRAGMA journal_mode=WAL` writes to the file and would
            // otherwise fail the connect, where before it served reads.
            conn_opts = conn_opts.read_only(true);
        } else {
            conn_opts = conn_opts
                .journal_mode(SqliteJournalMode::Wal)
                .synchronous(SqliteSynchronous::Normal);
        }
    }

    let mut pool_opts = SqlitePoolOptions::new()
        .max_connections(POOL_MAX_CONNECTIONS)
        .min_connections(POOL_MIN_CONNECTIONS)
        .acquire_timeout(POOL_ACQUIRE_TIMEOUT)
        .idle_timeout(POOL_IDLE_TIMEOUT);

    if kind.in_memory {
        // The database lives only as long as its connections, so keep one alive
        // and disable both reapers.
        pool_opts = pool_opts
            .min_connections(1)
            .idle_timeout(None)
            .max_lifetime(None);
        if !kind.shared_cache {
            pool_opts = pool_opts.max_connections(1);
        }
    }

    let sqlx_pool = pool_opts
        .connect_with(conn_opts)
        .await
        .map_err(|e| DatabaseError::ConnectionError(e.to_string()))?;

    Ok(SqlxSqliteConnector::from_sqlx_sqlite_pool(sqlx_pool))
}

/// Whether WAL can safely be enabled, based on real filesystem writability
/// rather than the URL alone.
///
/// Takes the driver's already-decoded filename
/// (`SqliteConnectOptions::get_filename`) so an escaped path is probed exactly
/// as sqlx will open it. Only an *existing* file that cannot be opened for
/// writing forces read-only: a missing file is created by the driver
/// (`mode=rwc`), and if its parent is unwritable the connect fails the same way
/// with or without WAL. The write probe does not truncate or create.
#[cfg(feature = "sqlite")]
fn sqlite_path_is_writable(path: &std::path::Path) -> bool {
    if path.exists() {
        std::fs::OpenOptions::new().write(true).open(path).is_ok()
    } else {
        true
    }
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
