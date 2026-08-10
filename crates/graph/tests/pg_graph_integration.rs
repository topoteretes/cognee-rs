#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for `PgGraphAdapter` using the shared GraphDBTrait test suite.
//!
//! These tests require a running PostgreSQL instance. Set `PGGRAPH_TEST_URL` to a
//! Postgres connection string, e.g.:
//!
//!   PGGRAPH_TEST_URL="postgres://user:pass@localhost:5432/cognee_test_graph"
//!
//! Tests are skipped automatically when the variable is absent.
//!
//! Each case provisions its **own throwaway database** on that server and drops
//! it again afterwards, so nothing is shared and nothing is wiped. The URL's own
//! database is only ever used to reach the server — pointing `PGGRAPH_TEST_URL`
//! at a database you actually keep data in is safe. The role it names needs
//! `CREATEDB`; see [`cognee_test_utils::create_temp_postgres_db`] for the full
//! rationale.
#![cfg(feature = "postgres")]

mod common;

use cognee_graph::PgGraphAdapter;
use cognee_test_utils::{TempPostgresDb, create_temp_postgres_db};

/// Read the connection URL or return `None` to skip.
fn test_url() -> Option<String> {
    std::env::var("PGGRAPH_TEST_URL").ok()
}

/// Provision a throwaway database for one case.
///
/// `None` means one thing only: no URL was configured, so the caller should
/// skip. Once a URL *is* set, a failure to provision is a real defect and panics
/// with the underlying error; the same goes for the connect/migrate step in the
/// macro below. These used to be `.ok()?`-ed into the same `None`, so an adapter
/// regression (a migrator collision, say) surfaced as "PGGRAPH_TEST_URL not set"
/// and sent whoever read the CI log hunting for a broken service container
/// instead.
///
/// This deliberately stops at the database and does **not** build the adapter:
/// everything that can panic after `CREATE DATABASE` has to run inside the
/// macro's cleanup guard, or that panic strands the database. `CREATE DATABASE`
/// is the last fallible step inside the helper, so an `Err` here means no
/// database exists yet and there is nothing to drop.
///
/// The database is empty by construction, so the old `delete_graph()` "clean
/// slate" wipe is gone: it is no longer needed, and it is what made the shared
/// database unsafe to point at anything real in the first place.
async fn temp_db() -> Option<TempPostgresDb> {
    let base_url = test_url()?;
    Some(
        create_temp_postgres_db(&base_url)
            .await
            .expect("PGGRAPH_TEST_URL is set, so CREATE DATABASE must succeed on that server"),
    )
}

/// Register a shared-suite case as a test with a database of its own.
///
/// No `#[serial]`: the cases share no state any more, which is the point — an
/// in-process serial guard does nothing under `cargo nextest`, where every test
/// gets its own process.
///
/// **Everything fallible runs inside the spawned task**, adapter construction
/// included, so a panic arrives as a `JoinError` instead of unwinding straight
/// out of the test function. `TempPostgresDb::cleanup` is `async` and so cannot
/// live in a `Drop` impl; without catching the panic here a red case would
/// strand its database on the server, and across 32 cases that turns one failing
/// run into a manual cleanup chore. Building the adapter *outside* the guard
/// would have left exactly that hole on the connect/migrate path — the one
/// failure this suite most exists to catch. The panic is re-raised unchanged
/// afterwards, so libtest/nextest still report the original failure and message.
///
/// The one leak this cannot cover is a test hard-killed rather than unwound
/// (nextest's `slow-timeout` terminate-after, or a `SIGKILL`), which no async
/// cleanup can survive; the databases are uniquely named, so the fallback is
/// dropping stragglers by hand.
macro_rules! pggraph_test {
    ($name:ident) => {
        #[tokio::test]
        async fn $name() {
            let Some(tmp) = temp_db().await else {
                eprintln!("PGGRAPH_TEST_URL not set — skipping {}", stringify!($name));
                return;
            };
            let url = tmp.url().to_string();
            let outcome = tokio::spawn(async move {
                let db = PgGraphAdapter::new(&url)
                    .await
                    .expect("PGGRAPH_TEST_URL is set, so the adapter must connect and migrate");
                common::$name(&db).await;
                // Hand the pooled connections back before the database is
                // dropped, so cleanup does not have to lean on `WITH (FORCE)`.
                drop(db);
            })
            .await;
            tmp.cleanup().await;
            if let Err(join_err) = outcome {
                std::panic::resume_unwind(join_err.into_panic());
            }
        }
    };
}

pggraph_test!(test_initialize_is_empty);
pggraph_test!(test_add_and_get_node);
pggraph_test!(test_add_nodes_batch);
pggraph_test!(test_has_node);
pggraph_test!(test_get_nodes_batch);
pggraph_test!(test_delete_node);
pggraph_test!(test_delete_nodes_batch);
pggraph_test!(test_node_upsert_same_id);
pggraph_test!(test_add_and_has_edge);
pggraph_test!(test_add_edges_batch);
pggraph_test!(test_edge_upsert_same_key);
pggraph_test!(test_has_edges);
pggraph_test!(test_has_edges_batch_equivalence);
pggraph_test!(test_get_edges);
pggraph_test!(test_get_neighbors);
pggraph_test!(test_get_connections);
pggraph_test!(test_get_graph_data);
pggraph_test!(test_get_graph_data_surfaces_created_at);
pggraph_test!(test_get_graph_metrics);
pggraph_test!(test_get_filtered_graph_data);
pggraph_test!(test_get_nodeset_subgraph_or);
pggraph_test!(test_get_nodeset_subgraph_and);
pggraph_test!(test_get_id_filtered_graph_data);
pggraph_test!(test_delete_graph);
pggraph_test!(test_node_delete_cascades_edges);
pggraph_test!(test_properties_json_round_trip);
pggraph_test!(test_get_neighborhood_depth1);
pggraph_test!(test_get_neighborhood_multiple_seeds);
pggraph_test!(test_get_neighborhood_empty_seeds);
pggraph_test!(test_node_truth_state_round_trip);
pggraph_test!(test_node_truth_state_missing_and_invalid);
pggraph_test!(test_node_truth_state_preserves_other_properties);
