#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! The bounded graph-view reads against `MockGraphDB`, which inherits the
//! trait defaults: the shared contract cases then pin the default itself.
#![cfg(feature = "testing")]

#[allow(dead_code, reason = "only the bounded-read cases run against the mock")]
mod common;

use cognee_graph::MockGraphDB;

macro_rules! mock_test {
    ($name:ident) => {
        #[tokio::test]
        async fn $name() {
            let db = MockGraphDB::new();
            common::$name(&db).await;
        }
    };
}

mock_test!(test_get_top_degree_node_ids_matches_default);
mock_test!(test_get_bounded_neighborhood_matches_default);
mock_test!(test_get_entity_type_names_matches_default);
