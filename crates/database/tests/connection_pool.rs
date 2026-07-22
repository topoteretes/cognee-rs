#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::permissions_set_readonly_false,
    reason = "test code — panics are acceptable failures; readonly is cleared only to let tempdir remove a deliberately read-only fixture file"
)]
//! Regression tests for relational connection-pool sizing and SQLite
//! journaling.
//!
//! In-memory SQLite (shared-cache or not) must never lose its last pool
//! connection — the database lives only as long as its connections — so both
//! sqlx reapers must be disabled. File-backed SQLite must NOT be pinned to a
//! single connection (that would needlessly serialize concurrent reads) and
//! runs in WAL mode, the only journal mode where a multi-connection pool
//! actually buys reader/writer concurrency. Read-only opens must not receive
//! journal-mode pragmas at all. sea-orm 1.1 exposes the underlying sqlx pool,
//! so the configured options are directly assertable.
#![cfg(feature = "sqlite")]

use cognee_database::connect;
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

async fn journal_mode(db: &cognee_database::DatabaseConnection) -> String {
    let mode: String = db
        .query_one(Statement::from_string(
            DatabaseBackend::Sqlite,
            "PRAGMA journal_mode;",
        ))
        .await
        .unwrap()
        .expect("PRAGMA journal_mode returns a row")
        .try_get_by_index(0)
        .unwrap();
    mode.to_lowercase()
}

#[tokio::test]
async fn in_memory_sqlite_is_single_connection() {
    let db = connect("sqlite::memory:").await.expect("connect");
    let opts = db.get_sqlite_connection_pool().options();
    assert_eq!(
        opts.get_max_connections(),
        1,
        "non-shared in-memory SQLite must be pinned to one connection",
    );
    // The database only lives as long as its connections, so reaping the last
    // one would silently swap in a fresh empty DB. Both reapers off.
    assert_eq!(
        opts.get_idle_timeout(),
        None,
        "in-memory connection must not be idle-reaped",
    );
    assert_eq!(
        opts.get_max_lifetime(),
        None,
        "in-memory connection must not be expired by max-lifetime",
    );
}

#[tokio::test]
async fn shared_cache_in_memory_disables_reapers() {
    let db = connect("sqlite:file:pool_shared_reaper_test?mode=memory&cache=shared")
        .await
        .expect("connect");
    let opts = db.get_sqlite_connection_pool().options();
    // Shared-cache in-memory may pool (the DB is genuinely shared across
    // connections), but the reapers must still be off: sqlx closes an expiring
    // connection before opening its replacement, so `min_connections >= 1`
    // alone cannot prevent the count from touching zero, at which point SQLite
    // frees the shared in-memory database.
    assert!(
        opts.get_max_connections() > 1,
        "shared-cache in-memory SQLite should not be pinned to one connection",
    );
    assert!(
        opts.get_min_connections() >= 1,
        "shared-cache in-memory SQLite must keep at least one connection",
    );
    assert_eq!(
        opts.get_idle_timeout(),
        None,
        "shared-cache in-memory connections must not be idle-reaped",
    );
    assert_eq!(
        opts.get_max_lifetime(),
        None,
        "shared-cache in-memory connections must not be expired by max-lifetime",
    );
}

#[tokio::test]
async fn file_sqlite_allows_a_pool_in_wal_mode() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("t.db").display());
    let db = connect(&url).await.expect("connect");

    assert!(
        db.get_sqlite_connection_pool()
            .options()
            .get_max_connections()
            > 1,
        "file-backed SQLite should not be pinned to a single connection",
    );

    // A multi-connection pool only pays off with WAL's reader/writer
    // concurrency; in rollback-journal mode the extra connections just contend
    // for one lock.
    assert_eq!(
        journal_mode(&db).await,
        "wal",
        "file-backed read-write SQLite should run in WAL mode",
    );
}

#[tokio::test]
async fn read_only_file_sqlite_connects_and_keeps_journal_mode() {
    use sea_orm::sqlx::ConnectOptions as _;
    use sea_orm::sqlx::sqlite::SqliteConnectOptions;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ro.db");

    // Seed the file with raw sqlx so it stays in the default DELETE journal.
    // (Seeding through `connect` would already convert it to WAL, and
    // `PRAGMA journal_mode=WAL` on an already-WAL database succeeds even on a
    // read-only connection — the regression would go undetected.)
    {
        use sea_orm::sqlx::Connection;
        let mut conn = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .connect()
            .await
            .expect("seed connect");
        sea_orm::sqlx::query("CREATE TABLE t (x INTEGER)")
            .execute(&mut conn)
            .await
            .expect("seed schema");
        sea_orm::sqlx::query("INSERT INTO t VALUES (1)")
            .execute(&mut conn)
            .await
            .expect("seed row");
        conn.close().await.expect("close seed connection");
    }

    // Pre-fix, this connect failed: the unconditional `PRAGMA journal_mode=WAL`
    // attempts to write to a read-only database.
    let url = format!("sqlite://{}?mode=ro", path.display());
    let db = connect(&url).await.expect("read-only connect must succeed");

    let row = db
        .query_one(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT COUNT(*) FROM t;",
        ))
        .await
        .expect("read query")
        .expect("count row");
    let count: i64 = row.try_get_by_index(0).unwrap();
    assert_eq!(count, 1, "read-only connection must be able to read");

    assert_eq!(
        journal_mode(&db).await,
        "delete",
        "read-only open must not switch the journal mode",
    );
}

/// The read-only *mount / file-permission* case (no `mode=ro` in the URL):
/// WAL is gated on real filesystem writability, so a plain URL on a read-only
/// file opens read-only and serves reads instead of failing at connect.
#[tokio::test]
async fn read_only_file_permission_connects_without_wal() {
    use sea_orm::sqlx::ConnectOptions as _;
    use sea_orm::sqlx::sqlite::SqliteConnectOptions;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ro_perm.db");

    // Seed with raw sqlx so the file stays in the default DELETE journal.
    {
        use sea_orm::sqlx::Connection;
        let mut conn = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .connect()
            .await
            .expect("seed connect");
        sea_orm::sqlx::query("CREATE TABLE t (x INTEGER)")
            .execute(&mut conn)
            .await
            .expect("seed schema");
        conn.close().await.expect("close seed connection");
    }

    // Make the file itself read-only, without `mode=ro` in the URL. Pre-fix,
    // `connect` issued `PRAGMA journal_mode=WAL` unconditionally, which writes
    // to the file and fails with "attempt to write a readonly database".
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&path, perms).unwrap();

    // Where DAC can't make the file unwritable (e.g. running as root on Unix),
    // the regression can't be exercised — restore and skip rather than fail
    // spuriously.
    if std::fs::OpenOptions::new().write(true).open(&path).is_ok() {
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_readonly(false);
        std::fs::set_permissions(&path, perms).unwrap();
        return;
    }

    let url = format!("sqlite://{}", path.display());
    let db = connect(&url)
        .await
        .expect("plain URL on a read-only file must still connect");

    let row = db
        .query_one(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT COUNT(*) FROM t;",
        ))
        .await
        .expect("read query")
        .expect("count row");
    let count: i64 = row.try_get_by_index(0).unwrap();
    assert_eq!(count, 0, "read-only connection must be able to read");

    assert_eq!(
        journal_mode(&db).await,
        "delete",
        "read-only file must not be switched to WAL",
    );

    // Restore writability so tempdir cleanup can remove the file (Windows keeps
    // a read-only attribute that blocks deletion otherwise).
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_readonly(false);
    std::fs::set_permissions(&path, perms).unwrap();
}

/// The writability probe must use the driver's *decoded* filename. sqlx
/// percent-decodes the path while parsing, so a URL-escaped path pointing at a
/// read-only file must still be seen as unwritable; probing the raw URL would
/// test a non-existent literal path (`my%20app.db`), report it writable, and
/// issue `PRAGMA journal_mode=WAL` on a read-only file.
#[tokio::test]
async fn read_only_percent_encoded_path_connects_without_wal() {
    use sea_orm::sqlx::ConnectOptions as _;
    use sea_orm::sqlx::sqlite::SqliteConnectOptions;

    let dir = tempfile::tempdir().unwrap();
    // A real filename containing a space, addressed as `%20` in the URL.
    let path = dir.path().join("my app.db");

    {
        use sea_orm::sqlx::Connection;
        let mut conn = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .connect()
            .await
            .expect("seed connect");
        sea_orm::sqlx::query("CREATE TABLE t (x INTEGER)")
            .execute(&mut conn)
            .await
            .expect("seed schema");
        conn.close().await.expect("close seed connection");
    }

    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&path, perms).unwrap();

    if std::fs::OpenOptions::new().write(true).open(&path).is_ok() {
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_readonly(false);
        std::fs::set_permissions(&path, perms).unwrap();
        return;
    }

    let url = format!(
        "sqlite://{}",
        path.display().to_string().replace(' ', "%20")
    );
    let db = connect(&url)
        .await
        .expect("percent-encoded read-only path must still connect");

    assert_eq!(
        journal_mode(&db).await,
        "delete",
        "escaped read-only path must not be switched to WAL",
    );

    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_readonly(false);
    std::fs::set_permissions(&path, perms).unwrap();
}

/// An existing, writable DB file inside a read-only *directory* must still
/// connect: WAL creates `-wal`/`-shm` sidecars in that directory, so forcing
/// WAL would fail the connect where the file previously opened read-only. The
/// probe checks parent-directory writability, not just the file.
#[cfg(unix)]
#[tokio::test]
async fn writable_file_in_read_only_dir_connects_without_wal() {
    use std::os::unix::fs::PermissionsExt;

    use sea_orm::sqlx::ConnectOptions as _;
    use sea_orm::sqlx::sqlite::SqliteConnectOptions;

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("ro_dir");
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("app.db");

    // Seed with raw sqlx so the file stays in the default DELETE journal.
    {
        use sea_orm::sqlx::Connection;
        let mut conn = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .connect()
            .await
            .expect("seed connect");
        sea_orm::sqlx::query("CREATE TABLE t (x INTEGER)")
            .execute(&mut conn)
            .await
            .expect("seed schema");
        conn.close().await.expect("close seed connection");
    }

    // Directory read-only (r-x), file itself still writable.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();

    // If the environment can't enforce the read-only directory (e.g. running as
    // root), the regression can't be exercised — restore and skip.
    let can_create = std::fs::File::create(dir.join(".probe")).is_ok();
    if can_create {
        let _ = std::fs::remove_file(dir.join(".probe"));
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }

    let url = format!("sqlite://{}", path.display());
    let db = connect(&url)
        .await
        .expect("writable file in a read-only dir must still connect");

    assert_eq!(
        journal_mode(&db).await,
        "delete",
        "read-only dir must not force WAL (sidecars can't be created there)",
    );

    // Restore so tempdir cleanup can remove the directory.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
}
