//! Integration tests for `LadybugAdapter` using the shared GraphDBTrait test suite.
//!
//! These tests run against an embedded Ladybug database in a temporary directory
//! and require no external services.
#![cfg(feature = "ladybug")]

mod common;

use cognee_graph::{GraphDBTrait, LadybugAdapter};
use serial_test::serial;
use tempfile::TempDir;

/// Create a fresh Ladybug adapter backed by a temp directory.
async fn make_adapter() -> (LadybugAdapter, TempDir) {
    let dir = TempDir::new().expect("failed to create temp dir");
    let db_path = dir.path().join("test.db");
    let adapter = LadybugAdapter::new(db_path.to_str().unwrap())
        .await
        .expect("failed to create LadybugAdapter");
    adapter
        .initialize()
        .await
        .expect("failed to initialise Ladybug");
    (adapter, dir)
}

macro_rules! ladybug_test {
    ($name:ident) => {
        #[tokio::test]
        #[serial]
        async fn $name() {
            let (adapter, _dir) = make_adapter().await;
            common::$name(&adapter).await;
        }
    };
}

ladybug_test!(test_initialize_is_empty);
ladybug_test!(test_add_and_get_node);
ladybug_test!(test_add_nodes_batch);
ladybug_test!(test_has_node);
ladybug_test!(test_get_nodes_batch);
ladybug_test!(test_delete_node);
ladybug_test!(test_delete_nodes_batch);
ladybug_test!(test_add_and_has_edge);
ladybug_test!(test_add_edges_batch);
ladybug_test!(test_has_edges);
ladybug_test!(test_get_edges);
ladybug_test!(test_get_neighbors);
ladybug_test!(test_get_connections);
ladybug_test!(test_get_graph_data);
ladybug_test!(test_get_graph_metrics);
ladybug_test!(test_get_filtered_graph_data);
ladybug_test!(test_get_nodeset_subgraph_or);
ladybug_test!(test_get_nodeset_subgraph_and);
ladybug_test!(test_get_id_filtered_graph_data);
ladybug_test!(test_delete_graph);
ladybug_test!(test_node_delete_cascades_edges);
ladybug_test!(test_properties_json_round_trip);
