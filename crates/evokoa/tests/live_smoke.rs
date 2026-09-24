#![allow(clippy::expect_used, reason = "integration-test assertions")]

use cognee_evokoa::EvokoaHybridAdapter;
use cognee_graph::{GraphDBTrait, GraphDBTraitExt};
use cognee_vector::{VectorDB, VectorPoint};
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires PostgreSQL 17+ with pgGraph and pgContext installed"]
async fn graph_vector_and_combined_query_round_trip() {
    let url = std::env::var("EVOKOA_TEST_DATABASE_URL").expect("EVOKOA_TEST_DATABASE_URL");
    let adapter = EvokoaHybridAdapter::new(&url).await.expect("connect");
    adapter.initialize().await.expect("initialize");

    let alice = Uuid::new_v4();
    let bob = Uuid::new_v4();
    adapter
        .graph()
        .add_node(&json!({"id": alice, "name": "Alice", "type": "Person"}))
        .await
        .expect("alice");
    adapter
        .graph()
        .add_node(&json!({"id": bob, "name": "Bob", "type": "Person"}))
        .await
        .expect("bob");
    adapter
        .graph()
        .add_edge(&alice.to_string(), &bob.to_string(), "knows", None)
        .await
        .expect("edge");

    adapter
        .vector()
        .create_collection("Entity", "name", 3)
        .await
        .expect("collection");
    adapter
        .vector()
        .index_points(
            "Entity",
            "name",
            &[VectorPoint::new(alice, vec![1.0, 0.0, 0.0])
                .with_metadata("dataset_ids", json!(["test"]))],
        )
        .await
        .expect("point");
    let vector_hits = adapter
        .vector()
        .search_similar("Entity", "name", &[1.0, 0.0, 0.0], 1)
        .await
        .expect("vector search");
    assert_eq!(vector_hits[0].id, alice);
    assert!((vector_hits[0].score - 1.0).abs() < 1e-5);
    let filtered = adapter
        .vector()
        .search_similar_filtered(
            "Entity",
            "name",
            &[1.0, 0.0, 0.0],
            1,
            Some(&["test".to_string()]),
            "AND",
        )
        .await
        .expect("filtered vector search");
    assert_eq!(filtered[0].id, alice);

    let hybrid = adapter
        .search_graph_with_vectors("Entity", "name", &[1.0, 0.0, 0.0], 1, 10)
        .await
        .expect("combined search");
    let bob_id = bob.to_string();
    assert!(
        hybrid
            .iter()
            .any(|hit| hit.neighbor_id.as_deref() == Some(bob_id.as_str()))
    );
}
