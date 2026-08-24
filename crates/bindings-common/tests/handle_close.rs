#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Regression test for issue #132: a warmed handle must release its SQLite
//! `-wal`/`-shm` sidecars when it is closed, not at process exit.
//!
//! Dropping the handle is not enough: sqlx's pool destructor only flags the pool
//! closed and lets its connections tear down concurrently, so whether SQLite ever
//! sees a "last connection" close — the precondition for unlinking the sidecars —
//! is a race. Every binding's teardown therefore goes through
//! [`HandleState::close`], and a caller that deletes the enclosing directory
//! right after closing (a JUnit `@TempDir`, a CLI scratch dir) must find nothing
//! left behind.

use std::path::{Path, PathBuf};

use cognee::config::Settings;
use cognee_bindings_common::HandleState;

/// A handle with every store rooted under `dir`, exactly as the binding test
/// suites configure one, plus its database path.
///
/// Built from `Settings::default()` rather than the environment so the test is
/// hermetic in both directions: it neither needs credentials (CI's no-secrets
/// lane) nor picks up a developer's `.env` provider. Warming still resolves the
/// LLM strictly, so it needs a non-empty key — a dummy suffices, since
/// `OpenAIAdapter::new` performs no network I/O — and the mock embedding provider
/// keeps warm off the network and away from any ONNX download.
fn handle_under(dir: &Path) -> (std::sync::Arc<HandleState>, PathBuf) {
    let db_path = dir.join("cognee.db");
    let settings = Settings {
        llm_api_key: "sk-test".to_owned(),
        embedding_provider: "mock".to_owned(),
        data_root_directory: dir.join("data").to_string_lossy().into_owned(),
        system_root_directory: dir.join("sys").to_string_lossy().into_owned(),
        relational_db_url: format!("sqlite://{}?mode=rwc", db_path.display()),
        ..Settings::default()
    };
    (
        std::sync::Arc::new(HandleState::from_settings(settings)),
        db_path,
    )
}

/// The `-wal` / `-shm` sidecar paths for a SQLite database file.
fn sidecars(db: &Path) -> (PathBuf, PathBuf) {
    let name = db
        .file_name()
        .expect("the database path has a file name")
        .to_string_lossy()
        .to_string();
    (
        db.with_file_name(format!("{name}-wal")),
        db.with_file_name(format!("{name}-shm")),
    )
}

/// The explicit teardown: releases the sidecars *and* closes the handle for good.
#[tokio::test(flavor = "multi_thread")]
async fn close_releases_sqlite_sidecars_and_marks_the_handle_closed() {
    let dir = tempfile::tempdir().unwrap();
    let (state, db_path) = handle_under(dir.path());
    let (wal, shm) = sidecars(&db_path);

    // Constructing a handle opens nothing, so there is nothing to release yet.
    assert!(
        !wal.exists() && !shm.exists(),
        "constructing a handle must not open the database",
    );
    assert!(!state.is_closed());

    state.services().await.expect("warm");
    assert!(
        wal.exists() && shm.exists(),
        "a warmed handle runs the relational database in WAL mode",
    );

    state.close().await;

    // No polling: the sidecars are gone before `close` resolves.
    assert!(
        !wal.exists(),
        "-wal must be released by close(), got a leak"
    );
    assert!(
        !shm.exists(),
        "-shm must be released by close(), got a leak"
    );

    // A closed handle stays closed: ops report it instead of reopening the DB.
    assert!(state.is_closed());
    // `CogneeServices` is not `Debug`, so match rather than `expect_err`.
    let err = match state.services().await {
        Ok(_) => panic!("an op after close() must fail"),
        Err(e) => e,
    };
    assert!(
        err.to_string().contains("closed"),
        "the error should name the cause, got: {err}"
    );
    assert!(
        !wal.exists(),
        "a failed op after close() must not reopen the database"
    );

    // Idempotent.
    state.close().await;
    state.close().await;
    assert!(!wal.exists() && !shm.exists(), "close must be idempotent");
}

/// The implicit teardown (what a GC finalizer calls): releases the sidecars but
/// leaves the handle usable, because the user never asked for a close — anything
/// still holding a clone of the state, like a Python `cognee.datasets` sub-handle
/// that outlived its parent, has to keep working.
#[tokio::test(flavor = "multi_thread")]
async fn release_frees_the_sidecars_without_closing_the_handle() {
    let dir = tempfile::tempdir().unwrap();
    let (state, db_path) = handle_under(dir.path());
    let (wal, shm) = sidecars(&db_path);

    state.services().await.expect("warm");
    assert!(wal.exists() && shm.exists());

    state.release().await;
    assert!(!wal.exists() && !shm.exists(), "release must free both");
    assert!(!state.is_closed(), "release must not close the handle");

    // Still usable: it just warms cold again against a fresh connection.
    state.services().await.expect("re-warm after release");
    assert!(wal.exists(), "re-warming reopens the database");

    state.release().await;
    state.release().await;
    assert!(!wal.exists() && !shm.exists(), "release must be idempotent");
}

/// Wait for an off-runtime teardown thread **without** blocking the runtime.
///
/// `JoinHandle::join` would block the calling task's worker thread, and on a
/// single-core machine (CI, or a `taskset`-pinned run) there is only one worker —
/// so joining inline wedges the runtime for the duration of the teardown. The
/// teardown needs it: releasing the last relational connection means polling the
/// task sqlx spawned from `PoolConnection::drop`, and only a worker can do that.
/// A real embedder does not have that problem, because the thread it closes from
/// (Node's main thread, a JVM `Cleaner`) is not one of the runtime's workers —
/// so awaiting a signal, which yields the worker, is what models it faithfully.
async fn join_off_runtime<F>(work: F)
where
    F: FnOnce() + Send + 'static,
{
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        work();
        let _ = tx.send(());
    });
    rx.await.expect("the off-runtime teardown thread panicked");
}

/// `close_blocking` is what the synchronous binding teardown hooks call, and both
/// of its deterministic branches must leave nothing behind by the time they
/// return: from a thread with no runtime of its own (a Java `close()` call, the
/// common case) and from inside the runtime itself (a `CompletableFuture`
/// continuation, which must not simply block a worker).
#[tokio::test(flavor = "multi_thread")]
async fn close_blocking_releases_the_sidecars_on_either_thread() {
    let handle = tokio::runtime::Handle::current();

    // (a) Off-runtime caller.
    let dir = tempfile::tempdir().unwrap();
    let (state, db_path) = handle_under(dir.path());
    let (wal, shm) = sidecars(&db_path);
    state.services().await.expect("warm");
    assert!(wal.exists() && shm.exists());
    {
        let state = std::sync::Arc::clone(&state);
        let handle = handle.clone();
        join_off_runtime(move || state.close_blocking(&handle)).await;
    }
    assert!(
        !wal.exists() && !shm.exists(),
        "close_blocking from an off-runtime thread must release the sidecars",
    );

    // (b) Caller already inside the runtime.
    let dir = tempfile::tempdir().unwrap();
    let (state, db_path) = handle_under(dir.path());
    let (wal, shm) = sidecars(&db_path);
    state.services().await.expect("warm");
    assert!(wal.exists() && shm.exists());
    state.close_blocking(&handle);
    assert!(
        !wal.exists() && !shm.exists(),
        "close_blocking from a runtime worker must release the sidecars",
    );

    // (c) `release_blocking` — the finalizer path — is blocking too, and leaves
    // the handle open.
    let dir = tempfile::tempdir().unwrap();
    let (state, db_path) = handle_under(dir.path());
    let (wal, shm) = sidecars(&db_path);
    state.services().await.expect("warm");
    assert!(wal.exists() && shm.exists());
    {
        let state = std::sync::Arc::clone(&state);
        let handle = handle.clone();
        join_off_runtime(move || state.release_blocking(&handle)).await;
    }
    assert!(
        !wal.exists() && !shm.exists(),
        "release_blocking must release the sidecars",
    );
    assert!(!state.is_closed());
}

/// The blocking close must hold its contract even when sqlx's own `Pool::close`
/// returns while a connection is still open — which is what it does whenever the
/// task sqlx spawns from `PoolConnection::drop` has not been polled yet, i.e.
/// routinely, on any loaded machine. Left unhandled this is what kept
/// `ts/__tests__/sdk_handle.test.ts` flaking after issue #132's first fix: it
/// failed twice in a row on CI on 2026-08-11, on a different test in the block
/// each time, and blocked an unrelated PR from merging.
///
/// `cognee_database::close` carries the mechanism and a deterministic test of it
/// (`crates/database/tests/connection_pool.rs`). This test is the same forced
/// ordering driven through the real binding entry point — a warmed handle, torn
/// down by `close_blocking` from a thread with no runtime of its own, exactly as
/// `cogneeClose` / `cg_sdk_close` / the JNI `destroy` do it.
#[test]
fn close_blocking_waits_for_a_straggling_connection() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    // One worker thread, so "the task sqlx spawned cannot run yet" is a fact we
    // control rather than a race we hope for. The teardown itself runs on a
    // thread of its own, so it still makes progress while the worker is busy.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let (state, db_path) = handle_under(dir.path());
    let (wal, shm) = sidecars(&db_path);

    let services = rt.block_on(state.services()).expect("warm");
    let pool = services.database.get_sqlite_connection_pool().clone();
    drop(services);
    assert!(wal.exists() && shm.exists());

    // Park idle connections: closing them is what pushes sqlx's semaphore over
    // capacity and lets its close barrier pass with a connection still out.
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
    assert!(
        pool.num_idle() >= 4,
        "the setup needs several genuinely idle connections, got {}",
        pool.num_idle(),
    );

    // Occupy the only worker, then strand a checked-out connection behind it.
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

    // Free the worker, ordered against the close rather than after a wall-clock
    // delay: the releaser waits until the close has been observed to *return* and
    // sampled, and only frees the worker if that has not happened within a grace
    // period — i.e. only when the close is still waiting. Without the drain the
    // close returns in microseconds, so the worker is never freed before the
    // sample and the assertions see the leak. (An earlier draft just slept 150ms,
    // which let the straggler be reaped inside the window on a loaded machine and
    // pass with the fix reverted.) The grace stays inside the drain's no-progress
    // ceiling so the drain does not rightly give up first.
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

    {
        let state = Arc::clone(&state);
        let handle = rt.handle().clone();
        std::thread::spawn(move || state.close_blocking(&handle))
            .join()
            .expect("closing thread");
    }

    // Sample the contract at the instant `close_blocking` returns. Looking any
    // later — even just long enough to join a thread — would let the straggler
    // finish on its own, and the test would pass with or without the fix.
    let (size, wal_left, shm_left) = (pool.size(), wal.exists(), shm.exists());
    sampled.store(true, Ordering::Release);

    // Unblock unconditionally so a failed assertion cannot hide behind a hang.
    release.store(true, Ordering::Release);
    releaser.join().unwrap();

    assert_eq!(
        size, 0,
        "close_blocking must not return while the pool still owns a connection",
    );
    assert!(
        !wal_left,
        "-wal must be gone when close_blocking returns, even when sqlx's own close returned early",
    );
    assert!(!shm_left, "-shm must be gone when close_blocking returns");
    assert!(state.is_closed());
}

/// Every `*.wal` under `root` — the embedded graph's sidecar (SQLite's is
/// `<db>-wal`, matched by [`sidecars`] instead).
fn graph_wal_files(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("wal") {
                found.push(path.to_string_lossy().into_owned());
            }
        }
    }
    found
}

/// The relational pool was never the only OS resource a warm handle holds: the
/// embedded graph keeps its own un-checkpointed `.wal` under the system root, and
/// a write lock on the graph file behind it.
///
/// Both were leaked by the #135 teardown, which closed the relational connection
/// and left every other slot in the manager's cache — i.e. never dropped, so
/// never released. This test is the binding-surface half of that fix: it fails
/// before it with `sys/graph.wal` still on disk after `close()` returned.
///
/// The re-open at the end is the other half: `close()` must not leave the graph
/// file locked, or the next handle on the same path (a binding test suite that
/// reuses its temp dir, a CLI invoked twice) fails to warm.
#[tokio::test(flavor = "multi_thread")]
async fn close_releases_the_embedded_graph_wal_and_unlocks_the_path() {
    let dir = tempfile::tempdir().unwrap();
    let (state, _db_path) = handle_under(dir.path());
    let sys = dir.path().join("sys");

    {
        let services = state.services().await.expect("warm");
        let nodes: Vec<_> = (0..500)
            .map(|i| {
                serde_json::json!({
                    "id": format!("n{i}"),
                    "name": format!("Node {i}"),
                    "type": "TestNode",
                    "properties": {"idx": i, "pad": "x".repeat(64)},
                })
            })
            .collect();
        services
            .graph_db
            .add_nodes_raw(nodes)
            .await
            .expect("write to the embedded graph");
        assert!(
            !graph_wal_files(&sys).is_empty(),
            "precondition: the writes must leave an un-checkpointed graph WAL under {}",
            sys.display(),
        );
    }

    state.close().await;

    let leftover = graph_wal_files(&sys);
    assert!(
        leftover.is_empty(),
        "close() must release the embedded graph's WAL too, found: {leftover:?}",
    );

    // A second handle on the same graph path must warm — the lock is gone.
    let (second, _) = handle_under(dir.path());
    let services = second.services().await.expect("re-warm on the same path");
    assert!(
        services
            .graph_db
            .has_node("n1")
            .await
            .expect("query the reopened graph"),
        "the checkpointed nodes must be readable by the second handle",
    );
    second.close().await;
    assert!(graph_wal_files(&sys).is_empty());
}

/// A warm that fails part-way still leaves resources open, and the finalizer's
/// probe has to say so.
///
/// `CogneeServices::build` warms slot by slot and resolves the LLM near the end,
/// so an empty `llm_api_key` fails **after** the SQLite pool (and the graph) are
/// cached. The services slot is then `None` — and a probe that only looked there
/// reported "nothing to release", so `Drop for PyCognee` / neon's `Finalize`
/// skipped the teardown and the open database survived until process exit.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_warm_still_reports_open_resources_and_releases_them() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("cognee.db");
    let (wal, shm) = sidecars(&db_path);

    // Everything valid except the LLM key, which is resolved strictly and last.
    let settings = Settings {
        llm_api_key: String::new(),
        embedding_provider: "mock".to_owned(),
        data_root_directory: dir.path().join("data").to_string_lossy().into_owned(),
        system_root_directory: dir.path().join("sys").to_string_lossy().into_owned(),
        relational_db_url: format!("sqlite://{}?mode=rwc", db_path.display()),
        ..Settings::default()
    };
    let state = std::sync::Arc::new(HandleState::from_settings(settings));

    let err = match state.services().await {
        Ok(_) => panic!("warm must fail without an LLM key"),
        Err(e) => e,
    };
    assert!(
        err.to_string().contains("llm_api_key"),
        "expected the LLM key to be what failed, got: {err}"
    );

    // The pool is open even though the bundle was never cached.
    assert!(
        wal.exists() && shm.exists(),
        "precondition: the relational pool was opened before the warm failed"
    );
    assert!(
        state.has_open_resources(),
        "a partly warmed handle has resources to release; reporting otherwise is \
         what made the finalizer skip the teardown"
    );

    // And the finalizer's teardown does release them.
    state.release().await;
    assert!(
        !wal.exists() && !shm.exists(),
        "release must free the sidecars"
    );
}

/// `release()` — the implicit tier — must not break a component that an operation
/// still in flight is holding.
///
/// A store's `close()` mutates state behind the shared `Arc`, so it is visible to
/// every clone. The explicit `close()` is entitled to that; a garbage collector is
/// not, and the two-tier split exists precisely so a finalizer cannot fail
/// somebody's in-flight query.
#[tokio::test(flavor = "multi_thread")]
async fn release_does_not_break_an_operation_still_holding_the_services() {
    let dir = tempfile::tempdir().unwrap();
    let (state, _db_path) = handle_under(dir.path());

    // Exactly what an in-flight op holds for its duration.
    let in_flight = state.services().await.expect("warm");

    state.release().await;

    in_flight
        .database
        .ping()
        .await
        .expect("release() must leave an in-flight op's connection usable");
    in_flight
        .graph_db
        .is_empty()
        .await
        .expect("release() must leave an in-flight op's graph usable");

    // The handle itself is still usable too, and re-warms cold.
    assert!(!state.is_closed());
    state.services().await.expect("re-warm after release");
}

/// The explicit `close()` is the opposite contract, and that difference is the
/// point: it closes the components even out from under an in-flight operation,
/// because the caller said they were done.
#[tokio::test(flavor = "multi_thread")]
async fn close_closes_even_what_an_operation_is_holding() {
    let dir = tempfile::tempdir().unwrap();
    let (state, db_path) = handle_under(dir.path());
    let (wal, _shm) = sidecars(&db_path);

    let in_flight = state.services().await.expect("warm");
    state.close().await;

    assert!(
        !wal.exists(),
        "close() must release the sidecars regardless"
    );
    assert!(
        in_flight.database.ping().await.is_err(),
        "close() closes the pool an in-flight op holds — that is the contract"
    );
}
