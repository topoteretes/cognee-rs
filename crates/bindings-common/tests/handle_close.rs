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
    let off_thread = {
        let state = std::sync::Arc::clone(&state);
        let handle = handle.clone();
        std::thread::spawn(move || state.close_blocking(&handle))
    };
    off_thread.join().expect("closing thread");
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
    let off_thread = {
        let state = std::sync::Arc::clone(&state);
        let handle = handle.clone();
        std::thread::spawn(move || state.release_blocking(&handle))
    };
    off_thread.join().expect("releasing thread");
    assert!(
        !wal.exists() && !shm.exists(),
        "release_blocking must release the sidecars",
    );
    assert!(!state.is_closed());
}
