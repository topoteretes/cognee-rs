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

/// Closing the pool must remove the WAL sidecars, deterministically, and for a
/// pool that actually has several connections open.
///
/// Dropping the connection cannot be asserted instead: sqlx's pool destructor
/// only flags the pool closed and lets each connection tear down concurrently on
/// its own worker thread, so whether any of them observes itself as the last
/// connection — the precondition for SQLite unlinking `-wal`/`-shm` — is a race
/// (issue #132). `close` closes them one at a time, so it is not.
#[tokio::test(flavor = "multi_thread")]
async fn close_releases_wal_sidecars() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("closing.db");
    let wal = path.with_file_name("closing.db-wal");
    let shm = path.with_file_name("closing.db-shm");
    let url = format!("sqlite://{}?mode=rwc", path.display());

    let db = std::sync::Arc::new(connect(&url).await.expect("connect"));
    db.execute(Statement::from_string(
        DatabaseBackend::Sqlite,
        "CREATE TABLE t (x INTEGER)",
    ))
    .await
    .unwrap();

    // Force the pool past one connection: the single-connection drop path
    // happens to clean up, so a one-connection pool would not exercise the bug.
    let mut queries = Vec::new();
    for _ in 0..8 {
        let db = std::sync::Arc::clone(&db);
        queries.push(tokio::spawn(async move {
            db.query_one(Statement::from_string(DatabaseBackend::Sqlite, "SELECT 1"))
                .await
                .unwrap();
        }));
    }
    for q in queries {
        q.await.unwrap();
    }
    assert!(
        db.get_sqlite_connection_pool().size() > 1,
        "the concurrent queries should have opened more than one connection",
    );
    assert!(
        wal.exists() && shm.exists(),
        "WAL mode creates both sidecars"
    );

    cognee_database::close(&db).await.expect("close");

    // No polling: the files are gone before `close` resolves.
    assert!(!wal.exists(), "-wal must be removed by close");
    assert!(!shm.exists(), "-shm must be removed by close");
}

/// The same guarantee when sqlx's own `Pool::close` returns *early* — the case
/// that kept `-wal`/`-shm` orphaned after issue #132 was first fixed, and that
/// flaked `ts/__tests__/sdk_handle.test.ts` on any loaded machine.
///
/// The ordering is forced rather than raced, so this test does not depend on the
/// machine being slow (the bug itself is an instance of that mistake — it was
/// invisible on an idle host and reproduced on demand under CPU starvation):
///
/// 1. Four connections are parked as idle. Closing them is what pushes sqlx's
///    semaphore over capacity, which is what lets the close barrier pass while a
///    connection is still checked out — see `drain_sqlite_pool`.
/// 2. The runtime is given exactly one worker thread, and that thread is
///    occupied. Now the task sqlx spawns from `PoolConnection::drop` to return a
///    connection to the pool provably cannot run.
/// 3. A connection is checked out and dropped, so it is checked out — counted in
///    `size()`, absent from `idle_conns` — for the whole of the close.
/// 4. `close` is driven from a *different* thread, so it makes progress on its
///    own (the sqlite worker threads wake it directly, no tokio worker needed),
///    exactly as a binding's blocking `close()` does from the embedder's thread.
/// 5. The worker is freed 150ms after the close begins. A close that waits for
///    the pool to empty therefore finishes; a close that returns as soon as
///    sqlx's does cannot have — it has already returned with the straggler open
///    and both sidecars still on disk.
#[test]
fn close_waits_for_a_straggling_connection() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("straggler.db");
    let wal = path.with_file_name("straggler.db-wal");
    let shm = path.with_file_name("straggler.db-shm");
    let url = format!("sqlite://{}?mode=rwc", path.display());

    let db = rt.block_on(connect(&url)).expect("connect");
    rt.block_on(db.execute(Statement::from_string(
        DatabaseBackend::Sqlite,
        "CREATE TABLE t (x INTEGER)",
    )))
    .unwrap();
    let pool = db.get_sqlite_connection_pool().clone();

    // (1) Park four idle connections.
    rt.block_on(async {
        let mut held = Vec::new();
        for _ in 0..4 {
            held.push(pool.acquire().await.unwrap());
        }
        drop(held);
        for _ in 0..500 {
            if pool.num_idle() >= 4 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });
    assert_eq!(
        pool.num_idle(),
        4,
        "the setup needs four genuinely idle connections",
    );
    assert!(
        wal.exists() && shm.exists(),
        "WAL mode creates both sidecars"
    );

    // (2) Occupy the only worker thread.
    let release = Arc::new(AtomicBool::new(false));
    let occupied = Arc::new(AtomicBool::new(false));
    {
        let release = Arc::clone(&release);
        let occupied = Arc::clone(&occupied);
        rt.spawn(async move {
            occupied.store(true, Ordering::Release);
            while !release.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(1));
            }
        });
    }
    while !occupied.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(1));
    }

    // (3) Strand a checked-out connection: its return-to-pool task is queued
    // behind the blocker.
    rt.block_on(async {
        let conn = pool.acquire().await.unwrap();
        drop(conn);
    });
    assert_eq!(
        pool.num_idle(),
        3,
        "one of the idle connections should now be stranded mid-return",
    );

    // (5) Free the worker, but *ordered against the close* rather than after a
    // wall-clock delay. An earlier draft slept 150ms here, which made the whole
    // test a race it could lose in the safe direction: on the loaded machine this
    // bug lives on, the straggler could be reaped inside those 150ms and the test
    // passed with the drain reverted. Instead the releaser waits until the close
    // has been *observed to return*; only if it has not returned within a grace
    // period — which means it is waiting, i.e. the fix is present — does it free
    // the worker so the close can finish.
    //
    // Without the drain the close returns in microseconds, so `sampled` is set
    // long before the grace expires, the worker is never freed early, and the
    // assertions below see the leak. The grace only has to exceed the handful of
    // instructions between the close returning and the sample being taken — it is
    // a ~1000x margin over that, rather than the old 150ms racing the straggler's
    // reap. It must also stay comfortably *inside* the drain's no-progress
    // ceiling, or the drain would rightly give up before the worker is freed and
    // this test would be asserting the wrong contract (see
    // `close_returns_even_when_no_worker_can_drive_the_runtime_clock`, which
    // covers deliberately never freeing it).
    let sampled = Arc::new(AtomicBool::new(false));
    let releaser = {
        let release = Arc::clone(&release);
        let sampled = Arc::clone(&sampled);
        let pool = pool.clone();
        std::thread::spawn(move || {
            while !pool.is_closed() {
                std::thread::sleep(Duration::from_millis(1));
            }
            let grace = std::time::Instant::now() + Duration::from_millis(100);
            while !sampled.load(Ordering::Acquire) && std::time::Instant::now() < grace {
                std::thread::sleep(Duration::from_millis(1));
            }
            release.store(true, Ordering::Release);
        })
    };

    // (4) Close from a thread of its own, as a binding's blocking close does.
    rt.block_on(cognee_database::close(&db)).expect("close");

    // Sample the contract at the instant `close` returns, before anything else
    // gets a chance to run — the assertions below must judge that moment, not a
    // later one.
    let (size, wal_left, shm_left) = (pool.size(), wal.exists(), shm.exists());
    sampled.store(true, Ordering::Release);

    // Unblock unconditionally, so an assertion failure cannot leave the runtime
    // wedged and hide itself behind a hang.
    release.store(true, Ordering::Release);
    releaser.join().unwrap();

    assert_eq!(
        size, 0,
        "close must not return while the pool still owns a connection",
    );
    assert!(
        !wal_left,
        "-wal must be gone when close returns, even when sqlx's own close returned early",
    );
    assert!(!shm_left, "-shm must be gone when close returns");
}

/// `close` must always return, including when no worker thread is available to
/// drive the runtime's clock — which is exactly the state that makes the drain
/// necessary in the first place.
///
/// tokio's time driver on a multi-thread runtime is only polled by its workers.
/// `Handle::block_on` from an outside thread (`teardown_blocking`'s
/// no-current-runtime branch, what every binding's synchronous `close()` uses)
/// drives only its own future, so with every worker busy a `tokio::time::sleep`
/// inside the drain never fires and any deadline checked *after* awaiting one is
/// never reached. The first version of this fix did exactly that and would block
/// forever here: a wedged embedder thread, which is a worse outcome than the
/// orphaned sidecars it was preventing. The backstop therefore runs on a plain OS
/// thread that no scheduler can starve.
///
/// The worker is never freed, so nothing can reap the connection and the drain
/// *must* give up on its own. The close is driven on a separate thread and its
/// return is reported over a channel, so the pre-fix behaviour surfaces as a
/// bounded timeout rather than hanging the test process.
#[test]
fn close_returns_even_when_no_worker_can_drive_the_runtime_clock() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nodriver.db");
    let url = format!("sqlite://{}?mode=rwc", path.display());

    let db = Arc::new(rt.block_on(connect(&url)).expect("connect"));
    rt.block_on(db.execute(Statement::from_string(
        DatabaseBackend::Sqlite,
        "CREATE TABLE t (x INTEGER)",
    )))
    .unwrap();
    let pool = db.get_sqlite_connection_pool().clone();

    // Park idle connections, so closing them inflates sqlx's semaphore and its
    // own close returns with the straggler still out.
    rt.block_on(async {
        let mut held = Vec::new();
        for _ in 0..4 {
            held.push(pool.acquire().await.unwrap());
        }
        drop(held);
        for _ in 0..500 {
            if pool.num_idle() >= 4 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });

    // Occupy the only worker for the rest of the test. From here on the runtime
    // clock is dead: no worker will poll the time driver.
    let release = Arc::new(AtomicBool::new(false));
    let occupied = Arc::new(AtomicBool::new(false));
    {
        let release = Arc::clone(&release);
        let occupied = Arc::clone(&occupied);
        rt.spawn(async move {
            occupied.store(true, Ordering::Release);
            while !release.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(1));
            }
        });
    }
    while !occupied.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(1));
    }
    rt.block_on(async {
        let conn = pool.acquire().await.unwrap();
        drop(conn);
    });

    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let closer = {
        let handle = rt.handle().clone();
        let db = Arc::clone(&db);
        std::thread::spawn(move || {
            let started = std::time::Instant::now();
            handle.block_on(cognee_database::close(&db)).expect("close");
            let _ = done_tx.send(started.elapsed());
        })
    };

    // Generous margin over the 2s backstop; the point is bounded, not fast.
    let outcome = done_rx.recv_timeout(Duration::from_secs(15));

    release.store(true, Ordering::Release);
    closer.join().expect("closing thread");

    let elapsed = outcome.expect(
        "close() must give up and return with no worker available to drive the runtime clock",
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "close() should give up near its backstop, took {elapsed:?}",
    );
}

/// The other half of the same bug, and the worse half: a connection that is
/// pushed back into the idle queue *after* sqlx's close has drained it for the
/// last time is never closed at all. Not "closed a moment later" — never. The
/// pool is closed, so nothing acquires from it again; `Pool::close` has finished,
/// so nothing drains it again; the connection stays open and the sidecars stay on
/// disk until the pool itself is dropped, which for a cached
/// `ComponentManager` connection can be the rest of the process's life.
///
/// `Floating::return_to_pool` checks `is_closed()` once at its top and then calls
/// `release()` unconditionally at the bottom, with an `after_release` hook and a
/// `ping()` round-trip in between. A return task that passes that check just
/// before the pool is marked closed therefore strands its connection. This test
/// pins the ordering open with an `after_release` hook that parks in exactly that
/// window, so the strand is constructed rather than raced — a pool built here
/// rather than by [`connect`], because the hook is a test instrument and has no
/// business in production options.
#[test]
fn close_reaps_a_connection_stranded_after_the_pool_was_closed() {
    use std::str::FromStr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use sea_orm::SqlxSqliteConnector;
    use sea_orm::sqlx::sqlite::{
        SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous,
    };

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stranded.db");
    let wal = path.with_file_name("stranded.db-wal");
    let shm = path.with_file_name("stranded.db-shm");

    // `hold` parks a returning connection inside `after_release`; `parked` tells
    // the test it is actually in there.
    let hold = Arc::new(AtomicBool::new(false));
    let parked = Arc::new(AtomicBool::new(false));

    let db = {
        let hold = Arc::clone(&hold);
        let parked = Arc::clone(&parked);
        let conn_opts =
            SqliteConnectOptions::from_str(&format!("sqlite://{}?mode=rwc", path.display()))
                .unwrap()
                .journal_mode(SqliteJournalMode::Wal)
                .synchronous(SqliteSynchronous::Full);

        let pool = rt
            .block_on(
                SqlitePoolOptions::new()
                    .max_connections(5)
                    .min_connections(0)
                    .after_release(move |_conn, _meta| {
                        let hold = Arc::clone(&hold);
                        let parked = Arc::clone(&parked);
                        Box::pin(async move {
                            if hold.load(Ordering::Acquire) {
                                parked.store(true, Ordering::Release);
                                while hold.load(Ordering::Acquire) {
                                    tokio::time::sleep(Duration::from_millis(1)).await;
                                }
                            }
                            Ok(true)
                        })
                    })
                    .connect_with(conn_opts),
            )
            .expect("connect");
        SqlxSqliteConnector::from_sqlx_sqlite_pool(pool)
    };

    rt.block_on(db.execute(Statement::from_string(
        DatabaseBackend::Sqlite,
        "CREATE TABLE t (x INTEGER)",
    )))
    .unwrap();
    let pool = db.get_sqlite_connection_pool().clone();

    // Park two idle connections, while the hook still lets releases through.
    rt.block_on(async {
        let mut held = Vec::new();
        for _ in 0..2 {
            held.push(pool.acquire().await.unwrap());
        }
        drop(held);
        for _ in 0..500 {
            if pool.num_idle() >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });
    assert!(pool.num_idle() >= 2, "the setup needs idle connections");
    assert!(
        wal.exists() && shm.exists(),
        "WAL mode creates both sidecars"
    );

    // Now arm the hook and send a connection back through it. When `parked` is
    // set, that connection has passed `return_to_pool`'s `is_closed()` check and
    // is waiting to be released — the exact window the bug lives in.
    hold.store(true, Ordering::Release);
    rt.block_on(async {
        let conn = pool.acquire().await.unwrap();
        drop(conn);
    });
    for _ in 0..5_000 {
        if parked.load(Ordering::Acquire) {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        parked.load(Ordering::Acquire),
        "a returning connection should be parked inside after_release",
    );

    // Let it finish releasing once the pool is closed, so it lands in the idle
    // queue of a pool that sqlx has already stopped draining.
    let releaser = {
        let hold = Arc::clone(&hold);
        let pool = pool.clone();
        std::thread::spawn(move || {
            while !pool.is_closed() {
                std::thread::sleep(Duration::from_millis(1));
            }
            hold.store(false, Ordering::Release);
        })
    };

    rt.block_on(cognee_database::close(&db)).expect("close");
    let (size, wal_left, shm_left) = (pool.size(), wal.exists(), shm.exists());

    hold.store(false, Ordering::Release);
    releaser.join().unwrap();

    assert_eq!(
        size, 0,
        "a connection released into a closed pool must still be reaped by close",
    );
    assert!(!wal_left, "-wal must be gone when close returns");
    assert!(!shm_left, "-shm must be gone when close returns");
}

// NOTE: a `close_right_after_a_query_still_releases_the_sidecars` test lived here
// and was removed deliberately. It drove the real
// `initialize(&db)`-then-`close(&db)` shape and asserted the sidecars were gone,
// which reads well but *races*: whether the straggler's `return_to_pool` task
// lands inside the drain's stall window depends on machine load. Measured 3
// failures in 5 back-to-back runs on an otherwise idle laptop.
//
// The mechanism it meant to cover is already covered above, deterministically,
// by `close_waits_for_a_straggling_connection` and
// `close_reaps_a_connection_stranded_after_the_pool_was_closed` — both of which
// force the ordering instead of hoping for it. Racing this is the same mistake
// that kept the original bug invisible on an idle host, so a flaky duplicate is
// worse than no test.
