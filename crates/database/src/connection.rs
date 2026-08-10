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
/// Shared with `cognee-components` (`builtins::database`), which must skip
/// filesystem preparation (parent directory creation) for such URLs; keeping
/// one predicate here prevents the layers from diverging on what counts as
/// in-memory.
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
/// - **File-backed, writable:** WAL + `synchronous=FULL` gives real
///   reader/writer concurrency (writers no longer block readers), which is
///   what justifies a multi-connection pool for SQLite, while `FULL` keeps
///   every committed transaction durable across an OS crash or power loss —
///   `NORMAL` under WAL trades that away, silently losing the last commits on
///   power loss, which is the wrong default for a memory pipeline. If enabling
///   WAL fails — typically a network/FUSE filesystem with no shared-memory
///   support, where `PRAGMA journal_mode=WAL` cannot create the `-shm` sidecar
///   — the connect retries once with an explicit rollback journal rather than
///   failing outright, so those deployments still open (they served reads
///   before this crate configured WAL). If that retry also fails, WAL was not
///   the cause and the original error is surfaced. `busy_timeout` makes the
///   inevitable
///   writer-vs-writer contention wait for the lock rather than failing
///   immediately with `SQLITE_BUSY`.
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
    let base_opts = SqliteConnectOptions::from_str(url)
        .map_err(|e| DatabaseError::ConnectionError(e.to_string()))?
        .log_statements(log::LevelFilter::Info)
        .busy_timeout(SQLITE_BUSY_TIMEOUT);

    // In-memory has no file to journal, and sqlx's default WAL is a no-op there.
    let mut want_wal = false;
    let mut conn_opts = base_opts.clone();
    if !kind.in_memory {
        // Probe the driver's own filename rather than the raw URL: sqlx
        // percent-decodes the path while parsing, so re-deriving it here would
        // test a path that does not exist (`my%20app.db`) and wrongly report a
        // read-only file as writable. The probe touches the filesystem
        // (open/create/unlink), which can block on a slow or hung mount, so run
        // it off the async runtime thread.
        let probe_path = conn_opts.get_filename().to_path_buf();
        let writable = tokio::task::spawn_blocking(move || sqlite_path_is_writable(&probe_path))
            .await
            .map_err(|e| DatabaseError::ConnectionError(e.to_string()))?;
        if kind.read_only {
            // The URL explicitly asked for a read-only open (`mode=ro` /
            // `immutable`): honour it and issue no journal-mode pragma.
            conn_opts = conn_opts.read_only(true);
        } else if !writable {
            // The URL wanted write access but the file/parent is not writable
            // (read-only mount, permissions, or on Windows a transient
            // share-lock from another process). Fall back to a read-only open
            // so `PRAGMA journal_mode=WAL` does not fail the connect — but warn,
            // because a genuinely write-intended database opened read-only will
            // fail the first `add`/`cognify` with an opaque "attempt to write a
            // readonly database" far from here.
            tracing::warn!(
                path = %conn_opts.get_filename().display(),
                "SQLite database is not writable; opening read-only. Writes will fail. \
                 Check file and parent-directory permissions (and, on Windows, other \
                 processes holding the file) if this database is meant to be written."
            );
            conn_opts = conn_opts.read_only(true);
        } else {
            // synchronous=FULL keeps every committed transaction durable across
            // an OS crash or power loss; WAL still gives reader/writer
            // concurrency. `want_wal` records that WAL is best-effort: if the
            // connect fails because the filesystem cannot back WAL (no
            // shared-memory support, e.g. NFS/FUSE), we retry without it below.
            want_wal = true;
            conn_opts = conn_opts
                .journal_mode(SqliteJournalMode::Wal)
                .synchronous(SqliteSynchronous::Full);
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

    let sqlx_pool = match pool_opts.clone().connect_with(conn_opts).await {
        Ok(pool) => pool,
        // WAL needs a shared-memory `-shm` file, which some filesystems (NFS and
        // other network/FUSE mounts) cannot provide, so enabling it can fail the
        // connect. We cannot tell such a WAL/shm failure apart from an unrelated
        // one (disk full, I/O error, a corrupt header) at this layer, so retry
        // once with an *explicit* rollback journal — `Delete`, not the
        // driver's implicit default, so a database persisted in WAL mode is
        // actively downgraded rather than reopened in WAL. Only warn if that
        // retry actually succeeds; if it fails too, WAL was almost certainly not
        // the cause, so surface the ORIGINAL error, which is the real one.
        Err(original) if want_wal => {
            let fallback_opts = base_opts
                .journal_mode(SqliteJournalMode::Delete)
                .synchronous(SqliteSynchronous::Full);
            match pool_opts.connect_with(fallback_opts).await {
                Ok(pool) => {
                    tracing::warn!(
                        error = %original,
                        "Enabling SQLite WAL failed; opened with a rollback journal instead \
                         (this happens on a network/FUSE filesystem without shared-memory \
                         support). Reader/writer concurrency is reduced for this database."
                    );
                    pool
                }
                Err(_) => return Err(DatabaseError::ConnectionError(original.to_string())),
            }
        }
        Err(e) => return Err(DatabaseError::ConnectionError(e.to_string())),
    };

    Ok(SqlxSqliteConnector::from_sqlx_sqlite_pool(sqlx_pool))
}

/// Whether WAL can safely be enabled, based on real filesystem writability
/// rather than the URL alone.
///
/// Takes the driver's already-decoded filename
/// (`SqliteConnectOptions::get_filename`) so an escaped path is probed exactly
/// as sqlx will open it. WAL writes the database file's header *and* creates
/// `-wal`/`-shm` sidecars next to it, so both the file and its parent directory
/// must be writable:
///
/// - The file is writable when it can be opened for writing, or when it does
///   not exist yet (the driver creates it via `mode=rwc`).
/// - The parent directory is writable when a temporary file can be created in
///   it. This catches an existing, writable file inside a read-only directory
///   (`chmod 555`), where the file opens fine but the sidecars cannot be
///   created and `PRAGMA journal_mode=WAL` fails the connect.
///
/// Neither probe truncates or modifies the database.
#[cfg(feature = "sqlite")]
fn sqlite_path_is_writable(path: &std::path::Path) -> bool {
    let file_writable = if path.exists() {
        std::fs::OpenOptions::new().write(true).open(path).is_ok()
    } else {
        true
    };
    file_writable && sqlite_parent_dir_is_writable(path)
}

/// Whether a file can be created next to `path`, probed by actually creating a
/// uniquely named temporary file (permission bits do not reliably reflect
/// effective writability across platforms, mounts, and ACLs). `AlreadyExists`
/// means the directory accepted the create attempt, so it counts as writable.
#[cfg(feature = "sqlite")]
fn sqlite_parent_dir_is_writable(path: &std::path::Path) -> bool {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => std::path::Path::new("."),
    };
    let probe = parent.join(format!(".cognee-wal-probe-{}.tmp", std::process::id()));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => true,
        Err(_) => false,
    }
}

/// Close the relational connection pool, releasing SQLite's `-wal`/`-shm`
/// sidecars deterministically.
///
/// **Dropping a [`DatabaseConnection`] is not a close.** sqlx's pool destructor
/// only flags the pool closed and then lets every pooled connection tear itself
/// down independently, each on its own sqlite worker thread, concurrently and in
/// no defined order. SQLite removes the `-wal`/`-shm` sidecars only when the
/// *last* connection to the file closes, and only if that close can take an
/// exclusive lock on the shared-memory index: when two connections close at the
/// same instant neither can take it, so neither runs the final
/// checkpoint-and-unlink and the sidecars are orphaned on disk — observable as
/// `<db>-wal`/`<db>-shm` files that outlive the last handle by minutes
/// (topoteretes/cognee-rs#132). With a single pooled connection the drop path
/// happens to clean up; with more than one it is a race, which is why relying on
/// `Drop` is not enough.
///
/// [`sea_orm::DatabaseConnection::close_by_ref`] runs sqlx's real close: pooled
/// connections are closed one at a time and checked-out ones are awaited, so
/// exactly one connection observes itself as the last and the sidecars are gone
/// before this future resolves.
///
/// Idempotent, and safe to call while other `Arc` clones of the connection are
/// still alive: they observe a closed pool and fail their next query rather than
/// silently reconnecting. Callers that may be reused (e.g.
/// `ComponentManager::close`) drop their cached connection so the next access
/// builds a fresh one.
pub async fn close(db: &DatabaseConnection) -> Result<(), DatabaseError> {
    db.close_by_ref()
        .await
        .map_err(|e| DatabaseError::ConnectionError(e.to_string()))
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
