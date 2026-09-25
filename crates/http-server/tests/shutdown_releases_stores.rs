#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Regression test for `on_shutdown`: a graceful shutdown must release the
//! knowledge stores' OS resources, not just the relational pool.
//!
//! The relational half landed in #135. The stores were left out, and for the HTTP
//! server that gap is not closable by a `Drop`: `lib.graph_db` is an `Arc` clone
//! held by handlers and pipeline builders, so there is no owner to drop — the
//! store has to be closed through `&self` or not at all.
//!
//! What that costs, measured on the real binary and stated precisely because the
//! easy overstatement is wrong: on a **normal** exit the graph WAL does get
//! released either way, because process teardown eventually drops the last `Arc`.
//! The difference shows up whenever the process does not get to finish exiting —
//! a container whose grace period ends when the shutdown hook does, an in-process
//! embedder that rebuilds the router, or any Postgres store, whose pool a retained
//! `Arc` keeps open for the life of the process. SIGTERM followed by SIGKILL as
//! soon as `on_shutdown` finished its work: **1 orphan (`sys/graph.wal`, 1.1 KB)
//! before the fix, 0 after**, with `graph database closed` logged.
#![cfg(feature = "ladybug")]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cognee_graph::{GraphDBTrait, LadybugAdapter};
use cognee_http_server::lifecycle::on_shutdown;

mod support;

/// Every `*.wal` under `root` — the embedded graph's sidecar.
fn wal_files(root: &Path) -> Vec<String> {
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

/// Warm an embedded graph under `dir` and write enough to leave a real WAL.
async fn graph_with_writes(dir: &Path) -> (Arc<dyn GraphDBTrait>, PathBuf) {
    let db_path = dir.join("graph.db");
    let adapter = LadybugAdapter::new(db_path.to_str().expect("utf-8 temp path"))
        .await
        .expect("open the embedded graph");
    adapter.initialize().await.expect("initialize");

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
    adapter.add_nodes_raw(nodes).await.expect("add_nodes_raw");

    (Arc::new(adapter) as Arc<dyn GraphDBTrait>, db_path)
}

/// `on_shutdown` must leave no graph WAL behind.
///
/// Fails before the fix — `on_shutdown` closed only the relational pool, so
/// `graph.db.wal` (~660 KB for this workload) survived the shutdown and, with the
/// process being killed straight afterwards, survived the exit too.
#[tokio::test]
async fn shutdown_releases_the_embedded_graph_wal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (graph, _db_path) = graph_with_writes(dir.path()).await;

    // A second holder, standing in for the handler/pipeline clones that make a
    // drop-based teardown impossible here.
    let still_held = Arc::clone(&graph);

    let state = support::build_p4_state(None, None, Some(graph)).await;

    assert!(
        !wal_files(dir.path()).is_empty(),
        "precondition: the writes must leave an un-checkpointed WAL under {}",
        dir.path().display(),
    );

    on_shutdown(&state).await;

    let leftover = wal_files(dir.path());
    assert!(
        leftover.is_empty(),
        "on_shutdown must release the graph store's WAL, found: {leftover:?}"
    );

    // The surviving clone observes the closed store rather than reopening it.
    assert!(
        still_held.get_node("n1").await.is_err(),
        "a query after shutdown must fail rather than silently reopen the store"
    );
}

/// `on_shutdown` stays a no-op-safe path for a state with no stores wired, and
/// for one whose store owns nothing closable — both are normal configurations
/// (`lib: None` in most tests, an in-memory vector store in production).
#[tokio::test]
async fn shutdown_is_safe_without_stores() {
    let state = support::build_p4_state(None, None, None).await;
    on_shutdown(&state).await;
    // Twice, because a shutdown signal can arrive while one is already running.
    on_shutdown(&state).await;
}

/// A request in flight when the shutdown fires must still complete successfully.
///
/// This is what `with_graceful_shutdown` is for: axum stops accepting and then
/// **drains**, and draining only begins once the future it was given completes. So
/// running the teardown inside that future closes the stores while handlers are
/// still executing — the in-flight request then hits a closed store and returns
/// 500 for a shutdown that was supposed to be graceful. Ordering the two
/// (`serve(...).await`, *then* `on_shutdown`) is what makes the drain mean
/// anything.
///
/// Deterministic by construction: the handler announces that it has started, the
/// test fires the shutdown only after seeing that, and the handler touches the
/// database *after* the shutdown has been requested.
#[tokio::test]
async fn a_request_in_flight_survives_the_shutdown() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let state = support::build_p4_state(None, None, None).await;
    let db = state
        .lib
        .as_ref()
        .expect("the p4 state wires a relational connection")
        .database
        .clone();

    let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let hit_db_after_shutdown = Arc::new(AtomicBool::new(false));

    let handler_db = Arc::clone(&db);
    let flag = Arc::clone(&hit_db_after_shutdown);
    let started = Arc::new(tokio::sync::Mutex::new(Some(started_tx)));
    let app = axum::Router::new().route(
        "/slow",
        axum::routing::get(move || {
            let db = Arc::clone(&handler_db);
            let flag = Arc::clone(&flag);
            let started = Arc::clone(&started);
            async move {
                // Tell the test we are in flight, then give it room to fire the
                // shutdown before we touch the store.
                if let Some(tx) = started.lock().await.take() {
                    let _ = tx.send(());
                }
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                // The assertion: a handler still running during shutdown must find
                // its resources intact.
                let ok = db.ping().await.is_ok();
                flag.store(ok, Ordering::Release);
                if ok { "ok" } else { "closed" }
            }
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let server = tokio::spawn(cognee_http_server::serve_with_shutdown(
        listener,
        app,
        async move {
            let _ = shutdown_rx.await;
        },
        state,
    ));

    let request = tokio::spawn(async move {
        reqwest::get(format!("http://{addr}/slow"))
            .await
            .expect("request")
            .text()
            .await
            .expect("body")
    });

    // Only fire the shutdown once the handler is provably running.
    started_rx.await.expect("handler started");
    shutdown_tx.send(()).expect("trigger shutdown");

    let body = request.await.expect("join the request");
    server.await.expect("join the server").expect("serve");

    assert_eq!(
        body, "ok",
        "the in-flight request must complete against live resources, not a store \
         closed underneath it"
    );
    assert!(
        hit_db_after_shutdown.load(Ordering::Acquire),
        "the handler reached the database after the shutdown was requested and it \
         must still have been open"
    );

    // And the teardown did run — after the drain.
    assert!(
        db.ping().await.is_err(),
        "once serve() returned, the teardown must have closed the pool"
    );
}

/// One store that will not close must not hold shutdown open forever.
///
/// A pool close waits for its checked-out connections to come back, so a task
/// parked in a driver read (or a handler outside the pipeline registry) can make it
/// wait indefinitely. Unbounded, that turns a SIGTERM into a SIGKILL — which skips
/// every *remaining* close as well, so an unbounded wait costs more resources than
/// it saves.
#[tokio::test]
async fn a_store_that_never_closes_does_not_hang_the_shutdown() {
    let hanging: Arc<dyn GraphDBTrait> = Arc::new(cognee_graph::MockGraphDB::hanging_on_close());
    let state = support::build_p4_state(None, None, Some(hanging)).await;

    // The whole shutdown must finish well inside the per-store ceiling (10 s), and
    // the ceiling itself must be finite. 30 s is the outer bound of the assertion,
    // not of the code.
    let finished = tokio::time::timeout(std::time::Duration::from_secs(30), on_shutdown(&state))
        .await
        .is_ok();

    assert!(
        finished,
        "on_shutdown must bound each store close; it waited on one that never \
         returns and would have been SIGKILLed"
    );
}
