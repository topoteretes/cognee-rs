#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "shared adapter contract tests"
)]

#[path = "../../graph/tests/common/mod.rs"]
mod common;

use cognee_evokoa::EvokoaGraphAdapter;
use cognee_graph::GraphDBTrait;
use cognee_test_utils::create_temp_postgres_db;
use sea_orm::Database;

fn test_url() -> Option<String> {
    std::env::var("EVOKOA_TEST_DATABASE_URL")
        .ok()
        .filter(|value| !value.is_empty())
}

macro_rules! graph_contract_test {
    ($name:ident) => {
        #[tokio::test]
        async fn $name() {
            let Some(base_url) = test_url() else {
                eprintln!(
                    "EVOKOA_TEST_DATABASE_URL not set — skipping {}",
                    stringify!($name)
                );
                return;
            };
            let tmp = create_temp_postgres_db(&base_url)
                .await
                .expect("temporary database creation");
            let url = tmp.url().to_string();
            let outcome = tokio::spawn(async move {
                let db = Database::connect(&url).await.expect("SeaORM connection");
                let adapter = EvokoaGraphAdapter::from_connection(db)
                    .await
                    .expect("pgGraph adapter");
                adapter.initialize().await.expect("pgGraph initialization");
                common::$name(&adapter).await;
            })
            .await;
            tmp.cleanup().await;
            if let Err(error) = outcome {
                assert!(error.is_panic(), "contract task was cancelled: {error}");
                std::panic::resume_unwind(error.into_panic());
            }
        }
    };
}

graph_contract_test!(test_initialize_is_empty);
graph_contract_test!(test_add_and_get_node);
graph_contract_test!(test_add_nodes_batch);
graph_contract_test!(test_has_node);
graph_contract_test!(test_get_nodes_batch);
graph_contract_test!(test_delete_node);
graph_contract_test!(test_delete_nodes_batch);
graph_contract_test!(test_node_upsert_same_id);
graph_contract_test!(test_add_and_has_edge);
graph_contract_test!(test_add_edges_batch);
graph_contract_test!(test_edge_upsert_same_key);
graph_contract_test!(test_has_edges);
graph_contract_test!(test_has_edges_batch_equivalence);
graph_contract_test!(test_get_edges);
graph_contract_test!(test_get_neighbors);
graph_contract_test!(test_get_connections);
graph_contract_test!(test_get_graph_data);
graph_contract_test!(test_get_graph_data_surfaces_created_at);
graph_contract_test!(test_get_graph_metrics);
graph_contract_test!(test_get_filtered_graph_data);
graph_contract_test!(test_get_candidate_nodes_by_label);
graph_contract_test!(test_get_nodeset_subgraph_or);
graph_contract_test!(test_get_nodeset_subgraph_and);
graph_contract_test!(test_get_id_filtered_graph_data);
graph_contract_test!(test_delete_graph);
graph_contract_test!(test_node_delete_cascades_edges);
graph_contract_test!(test_properties_json_round_trip);
graph_contract_test!(test_get_neighborhood_depth1);
graph_contract_test!(test_get_neighborhood_multiple_seeds);
graph_contract_test!(test_get_neighborhood_empty_seeds);
graph_contract_test!(test_node_truth_state_round_trip);
graph_contract_test!(test_node_truth_state_missing_and_invalid);
graph_contract_test!(test_node_truth_state_preserves_other_properties);
graph_contract_test!(test_update_node_property_preserves_edges_and_siblings);
graph_contract_test!(test_node_feedback_weight_round_trip);
graph_contract_test!(test_node_feedback_weight_preserves_edges_and_siblings);
graph_contract_test!(test_edge_feedback_weight_round_trip);
graph_contract_test!(test_property_writes_tolerate_nul_in_value);
graph_contract_test!(test_edge_feedback_weight_rejects_non_finite);
graph_contract_test!(test_nul_bytes_in_text_are_persistable);
