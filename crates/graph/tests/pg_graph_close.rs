#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Regression tests for `PgGraphAdapter::close` — the Postgres half of
//! topoteretes/cognee-rs#132.
//!
//! `PgGraphAdapter` opens a pool that is **entirely separate** from the relational
//! one (see the pool-sizing note in `cognee_database::connection`), so #135's
//! relational close did nothing for it: a warm `ComponentManager` on Postgres
//! holds three pools of ten, and nothing closed two of them.
//!
//! What was actually measured here, against a live `pgvector/pg16` with
//! `max_connections = 100`, matters for how these tests are written — because one
//! popular framing of the bug turns out to be **wrong**:
//!
//! | teardown | backends afterwards |
//! |---|---|
//! | `drop`, pool idle | drained in **4 ms** — a drop *is* enough here |
//! | `close`, pool idle | drained in **1 ms** (call returned in 67 µs) |
//! | `drop`, one `pg_sleep` in flight | **all 10 pinned** for the query's full duration |
//! | `close`, one `pg_sleep` in flight | **9 of 10 reclaimed** within 500 ms; the query completes normally |
//! | `Arc` retained, never closed | **10 still open at 5 s**; `close()` on the retained `Arc` drains them in 2 ms |
//!
//! So the two things `close()` buys are the last two rows, and those are what is
//! asserted below:
//!
//! 1. **A retained `Arc` has no drop to wait for.** The HTTP server keeps
//!    `lib.graph_db` as an `Arc` clone in `AppState`, and an in-flight pipeline
//!    holds its own; closing through `&self` is the only teardown available.
//! 2. **Under contention a drop is much worse than an idle drop.** A checked-out
//!    `PoolConnection` holds an `Arc<PoolInner>` (sqlx-core `pool/connection.rs`),
//!    so one slow query keeps the entire pool alive; `close` releases the idle
//!    connections immediately instead.
//!
//! Requires a running PostgreSQL instance; set `PGGRAPH_TEST_URL`, e.g.
//!
//!   PGGRAPH_TEST_URL="postgres://user:pass@localhost:5432/cognee_test_graph"
//!
//! Tests skip automatically when it is absent, following the convention in
//! `pg_graph_integration.rs`. **These tests must be named explicitly in `ci.yml`'s
//! `pggraph-postgres` lane and in the `community.yml` mirror**: those lanes invoke
//! individual `--test` targets, so a new file runs nowhere unless it is added
//! there.
#![cfg(feature = "postgres")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use cognee_graph::{GraphDBTrait, PgGraphAdapter};
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use serial_test::serial;

/// Concurrency used to grow the pool past a single connection. Matches sqlx's
/// default ceiling, which the adapter does not override; the assertions compare
/// against the count actually observed, not against this.
const CONCURRENCY: usize = 10;

fn test_url() -> Option<String> {
    std::env::var("PGGRAPH_TEST_URL").ok()
}

/// Tag a URL with a unique `application_name` so the observer counts *this*
/// adapter's backends and nothing else — no `psql`, no superuser, and no
/// interference from a parallel test or a developer's own session.
fn tagged(url: &str, tag: &str) -> String {
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}application_name={tag}")
}

/// A second, independent connection used purely to observe the server's view.
///
/// It must not come from the pool under test: closing that pool would close the
/// observer with it, and the whole point is to see what the *server* still holds.
async fn observer(url: &str) -> DatabaseConnection {
    Database::connect(tagged(url, "cognee_close_observer"))
        .await
        .expect("PGGRAPH_TEST_URL is set, so the observer must connect")
}

/// Backends the server still has open for `tag`.
async fn backend_count(obs: &DatabaseConnection, tag: &str) -> i64 {
    let stmt = Statement::from_sql_and_values(
        obs.get_database_backend(),
        "SELECT count(*)::bigint FROM pg_stat_activity WHERE application_name = $1",
        [tag.into()],
    );
    obs.query_one(stmt)
        .await
        .expect("pg_stat_activity query")
        .expect("count() always returns a row")
        .try_get_by_index::<i64>(0)
        .expect("count is a bigint")
}

/// Poll until the count is at or below `target`, returning the last value seen.
async fn wait_for(obs: &DatabaseConnection, tag: &str, target: i64, window: Duration) -> i64 {
    let deadline = Instant::now() + window;
    loop {
        let n = backend_count(obs, tag).await;
        if n <= target || Instant::now() >= deadline {
            return n;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Grow the pool past one connection with overlapping real operations, and return
/// how many backends the server ended up with.
///
/// `GraphDBTrait::query` cannot be used for this — the Postgres backend rejects raw
/// Cypher by design and never touches the pool — so this uses `has_node`. The peak
/// is returned rather than asserted equal to `CONCURRENCY`, so the before/after
/// comparisons below are against a number actually observed on this machine.
async fn saturate(adapter: &Arc<PgGraphAdapter>, obs: &DatabaseConnection, tag: &str) -> i64 {
    let mut tasks = Vec::new();
    for _ in 0..CONCURRENCY {
        let adapter = Arc::clone(adapter);
        tasks.push(tokio::spawn(async move {
            let deadline = Instant::now() + Duration::from_millis(400);
            while Instant::now() < deadline {
                adapter
                    .has_node("saturation-probe")
                    .await
                    .expect("has_node");
            }
        }));
    }
    for t in tasks {
        t.await.expect("saturation task");
    }
    let n = backend_count(obs, tag).await;
    assert!(
        n > 1,
        "precondition: overlapping operations must open more than one pooled \
         connection (got {n}), or this test cannot tell a pool close apart from a \
         single connection dropping"
    );
    n
}

/// The case with no `Drop` to fall back on: the `Arc` is still held, so `close()`
/// is the only thing that can release the pool.
///
/// This is the shape of every long-lived holder in the tree — the HTTP server's
/// `AppState`, a pipeline task that captured the store — and it is where the leak
/// is unbounded rather than merely untidy: measured 10 backends still open at 5 s
/// on an idle but retained adapter, versus 0 in ~2 ms once `close()` is called on
/// that very same `Arc`.
#[tokio::test]
#[serial]
async fn close_releases_the_pool_while_the_adapter_is_still_held() {
    let Some(url) = test_url() else {
        eprintln!("PGGRAPH_TEST_URL not set — skipping");
        return;
    };
    let obs = observer(&url).await;
    let tag = format!("cognee_graph_retained_{}", std::process::id());

    let adapter = Arc::new(
        PgGraphAdapter::new(&tagged(&url, &tag))
            .await
            .expect("connect"),
    );
    let peak = saturate(&adapter, &obs, &tag).await;

    // Time alone does not help: the pool is idle, but every backend stays.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        backend_count(&obs, &tag).await,
        peak,
        "an idle but retained pool must still hold all {peak} backends — if this \
         ever drains on its own, sqlx's idle timeout changed and these numbers \
         need re-deriving"
    );

    // A second holder, exactly like the HTTP server's AppState clone: the close
    // has to work through `&self`, with no drop involved anywhere.
    let second_holder = Arc::clone(&adapter);

    adapter.close().await.expect("close");
    let after_close = wait_for(&obs, &tag, 0, Duration::from_secs(5)).await;
    assert_eq!(
        after_close, 0,
        "close() must reclaim all {peak} backends while the adapter is still \
         held; {after_close} left"
    );
    assert!(
        second_holder
            .connection()
            .get_postgres_connection_pool()
            .is_closed(),
        "the pool must report itself closed to every holder, not merely emptied"
    );

    // Idempotent, and the surviving clone fails its next query rather than
    // silently reconnecting.
    adapter.close().await.expect("second close is a no-op");
    assert!(
        second_holder.is_empty().await.is_err(),
        "a query after close must fail rather than reconnect"
    );
}

/// Under contention `close()` is strictly better than a drop — both halves
/// asserted in one run, over the same window, so a slow machine cannot make a
/// leak look like a fix.
///
/// A checked-out `PoolConnection` holds an `Arc<PoolInner>`, so the **drop** path
/// pins the whole pool for as long as the slowest query runs (measured: 10 of 10
/// backends still open at 0.5, 1, 1.5, 2 and 2.5 s with one `pg_sleep(3)` in
/// flight). `close_by_ref` reclaims the idle connections immediately (measured: 9
/// of 10 gone within 500 ms, the call itself returning in ~500 µs) and lets the
/// running query finish normally.
#[tokio::test]
#[serial]
async fn close_reclaims_idle_connections_that_a_drop_would_pin() {
    let Some(url) = test_url() else {
        eprintln!("PGGRAPH_TEST_URL not set — skipping");
        return;
    };
    let obs = observer(&url).await;

    // -- control: drop, with one query in flight ---------------------------
    let drop_tag = format!("cognee_graph_dropslow_{}", std::process::id());
    let (drop_peak, drop_pinned) = {
        let adapter = Arc::new(
            PgGraphAdapter::new(&tagged(&url, &drop_tag))
                .await
                .expect("connect"),
        );
        let peak = saturate(&adapter, &obs, &drop_tag).await;
        let slow = {
            let adapter = Arc::clone(&adapter);
            tokio::spawn(async move {
                adapter
                    .connection()
                    .execute_unprepared("SELECT pg_sleep(2)")
                    .await
                    .map(|_| ())
            })
        };
        tokio::time::sleep(Duration::from_millis(300)).await;
        drop(adapter);
        // Sampled while the slow query is still running.
        tokio::time::sleep(Duration::from_millis(700)).await;
        let pinned = backend_count(&obs, &drop_tag).await;
        slow.await.expect("task").expect("query");
        (peak, pinned)
    };

    // -- close, same shape -------------------------------------------------
    let close_tag = format!("cognee_graph_closeslow_{}", std::process::id());
    let adapter = Arc::new(
        PgGraphAdapter::new(&tagged(&url, &close_tag))
            .await
            .expect("connect"),
    );
    let close_peak = saturate(&adapter, &obs, &close_tag).await;
    let slow = {
        let adapter = Arc::clone(&adapter);
        tokio::spawn(async move {
            adapter
                .connection()
                .execute_unprepared("SELECT pg_sleep(2)")
                .await
                .map(|_| ())
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;

    let started = Instant::now();
    adapter.close().await.expect("close");
    let close_took = started.elapsed();
    assert!(
        close_took < Duration::from_secs(1),
        "close() must not wait out the in-flight query, took {close_took:?}"
    );

    let while_running = wait_for(&obs, &close_tag, 1, Duration::from_millis(700)).await;

    assert_eq!(
        drop_pinned, drop_peak,
        "control: with a query in flight, dropping the adapter pins the whole \
         pool ({drop_peak} backends) — if this stops being true, sqlx's pool \
         semantics changed and this fix should be re-derived, not deleted"
    );
    assert!(
        while_running <= 1,
        "close() must reclaim the idle connections while the slow query runs: \
         {while_running} of {close_peak} left, against {drop_pinned} for the \
         dropped control"
    );

    // The in-flight query is neither cancelled nor broken by the close.
    slow.await
        .expect("the in-flight task must not be cancelled")
        .expect("the in-flight query must still complete against a closed pool");
    let after = wait_for(&obs, &close_tag, 0, Duration::from_secs(5)).await;
    assert_eq!(
        after, 0,
        "every backend must be gone once the query finishes"
    );
}

/// The outage-prevention test: an adapter built with
/// [`PgGraphAdapter::from_connection`] must **not** close the caller's connection.
///
/// In the single-shared-Postgres layout the caller's connection is the relational
/// pool. Closing it from a graph teardown would take out every relational query in
/// the process — a leak fix turned into an outage. This is why `owns_pool` exists
/// and is load-bearing rather than defensive.
#[tokio::test]
#[serial]
async fn close_does_not_touch_a_caller_owned_connection() {
    let Some(url) = test_url() else {
        eprintln!("PGGRAPH_TEST_URL not set — skipping");
        return;
    };
    let tag = format!("cognee_graph_borrowed_{}", std::process::id());
    let caller_owned = Database::connect(tagged(&url, &tag))
        .await
        .expect("caller connection");

    let adapter = PgGraphAdapter::from_connection(caller_owned.clone())
        .await
        .expect("from_connection");
    adapter.close().await.expect("close must succeed");

    let stmt = Statement::from_string(caller_owned.get_database_backend(), "SELECT 1");
    caller_owned
        .query_one(stmt)
        .await
        .expect("the caller's connection must survive the adapter's close");
    assert!(
        !caller_owned.get_postgres_connection_pool().is_closed(),
        "close() must not close a caller-owned pool"
    );
}
