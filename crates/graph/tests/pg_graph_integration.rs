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
//! All tests run serially (shared DB state).
#![cfg(feature = "postgres")]

mod common;

use cognee_graph::{GraphDBTrait, PgGraphAdapter};
use serial_test::serial;

/// Read the connection URL or return `None` to skip.
fn test_url() -> Option<String> {
    std::env::var("PGGRAPH_TEST_URL").ok()
}

/// Create an adapter and wipe the graph for a clean slate.
async fn make_adapter() -> Option<PgGraphAdapter> {
    let url = test_url()?;
    let db = PgGraphAdapter::new(&url).await.ok()?;
    let _: () = db.delete_graph().await.ok()?;
    Some(db)
}

macro_rules! pggraph_test {
    ($name:ident) => {
        #[tokio::test]
        #[serial]
        async fn $name() {
            let Some(db) = make_adapter().await else {
                eprintln!("PGGRAPH_TEST_URL not set — skipping {}", stringify!($name));
                return;
            };
            common::$name(&db).await;
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
pggraph_test!(test_get_graph_metrics);
pggraph_test!(test_get_filtered_graph_data);
pggraph_test!(test_get_nodeset_subgraph_or);
pggraph_test!(test_get_nodeset_subgraph_and);
pggraph_test!(test_get_id_filtered_graph_data);
pggraph_test!(test_delete_graph);
pggraph_test!(test_node_delete_cascades_edges);
pggraph_test!(test_properties_json_round_trip);
