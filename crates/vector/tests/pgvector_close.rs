#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Regression tests for `PgVectorAdapter::close` — the vector half of the
//! Postgres pool leak in topoteretes/cognee-rs#132.
//!
//! `PgVectorAdapter` opens a pool of its own, separate from both the relational
//! pool and the graph adapter's, so a warm `ComponentManager` on Postgres holds
//! three and #135 closed one. See `cognee-graph`'s `pg_graph_close.rs` for the
//! full measurement table; the two things `close()` buys are:
//!
//! - a **retained `Arc`** has no drop to fall back on (measured on the graph
//!   adapter: 10 backends still open at 5 s while idle-but-held, 0 in ~2 ms after
//!   `close()` on that same `Arc`), and
//! - under contention a drop pins the whole pool behind the slowest query, where
//!   `close` reclaims the idle connections immediately.
//!
//! Requires a running PostgreSQL with the `vector` extension; set
//! `PGVECTOR_TEST_URL` (or the `DB_*` set that `cognee_test_utils::pg_test_url`
//! reads), e.g.
//!
//!   PGVECTOR_TEST_URL="postgres://user:pass@localhost:5432/cognee_test_vectors"
//!
//! Tests skip automatically when it is absent. **These tests must be named
//! explicitly in `ci.yml`'s `pgvector-spans` lane and in the `community.yml`
//! mirror**: those lanes invoke individual `--test` targets, so a new file runs
//! nowhere unless it is added there.
#![cfg(feature = "pgvector")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use cognee_vector::{PgVectorAdapter, VectorDB};
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use serial_test::serial;

const CONCURRENCY: usize = 10;
const DIMENSION: usize = 4;

/// The Postgres URL, or `None` to skip.
///
/// Accepts both conventions in the tree because the two CI lanes that could run
/// this file disagree: `pgvector_integration.rs` reads `PGVECTOR_TEST_URL`, while
/// the `pgvector-spans` lane (the one with a pgvector service container) provides
/// the `DB_*` set that `cognee_test_utils::pg_test_url` assembles. Reading only
/// one of them is how a suite ends up green without ever reaching a server.
fn test_url() -> Option<String> {
    std::env::var("PGVECTOR_TEST_URL")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(cognee_test_utils::pg_test_url)
}

/// Tag a URL with a unique `application_name` so the observer counts *this*
/// adapter's backends and nothing else.
fn tagged(url: &str, tag: &str) -> String {
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}application_name={tag}")
}

/// A second, independent connection: it must not come from the pool under test,
/// which is about to be closed.
async fn observer(url: &str) -> DatabaseConnection {
    Database::connect(tagged(url, "cognee_vec_close_observer"))
        .await
        .expect("PGVECTOR_TEST_URL is set, so the observer must connect")
}

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

/// Grow the pool past one connection with overlapping real vector operations, and
/// return the number of backends observed.
async fn saturate(adapter: &Arc<PgVectorAdapter>, obs: &DatabaseConnection, tag: &str) -> i64 {
    let mut tasks = Vec::new();
    for _ in 0..CONCURRENCY {
        let adapter = Arc::clone(adapter);
        tasks.push(tokio::spawn(async move {
            let deadline = Instant::now() + Duration::from_millis(400);
            while Instant::now() < deadline {
                adapter
                    .has_collection("CloseProbe", "text")
                    .await
                    .expect("has_collection");
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

/// The case with no `Drop` to fall back on: the `Arc` is still held (the HTTP
/// server's `AppState` shape), so `close()` is the only available teardown.
#[tokio::test]
#[serial]
async fn close_releases_the_pool_while_the_adapter_is_still_held() {
    let Some(url) = test_url() else {
        eprintln!("PGVECTOR_TEST_URL not set — skipping");
        return;
    };
    let obs = observer(&url).await;
    let tag = format!("cognee_vector_retained_{}", std::process::id());

    let adapter = Arc::new(
        PgVectorAdapter::new(&tagged(&url, &tag), DIMENSION)
            .await
            .expect("connect"),
    );
    let peak = saturate(&adapter, &obs, &tag).await;

    // Time alone does not help.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        backend_count(&obs, &tag).await,
        peak,
        "an idle but retained pool must still hold all {peak} backends"
    );

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
        "the pool must report itself closed to every holder"
    );

    // Idempotent, and a surviving clone fails rather than reconnecting.
    adapter.close().await.expect("second close is a no-op");
    assert!(
        second_holder
            .has_collection("CloseProbe", "text")
            .await
            .is_err(),
        "an operation after close must fail rather than reconnect"
    );
}

/// The outage-prevention test: [`PgVectorAdapter::from_connection`] wraps a
/// connection the caller owns — in the shared-Postgres layout, the relational pool
/// — and `close()` must leave it alone.
#[tokio::test]
#[serial]
async fn close_does_not_touch_a_caller_owned_connection() {
    let Some(url) = test_url() else {
        eprintln!("PGVECTOR_TEST_URL not set — skipping");
        return;
    };
    let tag = format!("cognee_vector_borrowed_{}", std::process::id());
    let caller_owned = Database::connect(tagged(&url, &tag))
        .await
        .expect("caller connection");

    let adapter = PgVectorAdapter::from_connection(caller_owned.clone(), DIMENSION)
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
    // And the adapter itself still works, because nothing was closed.
    adapter
        .has_collection("CloseProbe", "text")
        .await
        .expect("a borrowed-connection adapter stays usable after its no-op close");
}
