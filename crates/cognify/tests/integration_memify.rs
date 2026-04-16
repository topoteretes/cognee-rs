#![cfg(feature = "testing")]

use cognee_cognify::memify::{MemifyConfig, memify};
use cognee_embedding::MockEmbeddingEngine;
use cognee_graph::{GraphDBTrait, MockGraphDB};
use cognee_vector::{MockVectorDB, VectorDB};
use serde_json::json;
use uuid::Uuid;

/// Helper: add a node to the mock graph with name and description.
async fn add_node(db: &MockGraphDB, id: Uuid, name: &str, description: &str) {
    let mut node_json = serde_json::Map::new();
    node_json.insert("id".to_string(), json!(id.to_string()));
    node_json.insert("name".to_string(), json!(name));
    if !description.is_empty() {
        node_json.insert("description".to_string(), json!(description));
    }
    db.add_node_raw(serde_json::Value::Object(node_json))
        .await
        .unwrap();
}

/// Helper: add an edge between two nodes.
async fn add_edge(db: &MockGraphDB, source: Uuid, target: Uuid, relationship: &str) {
    db.add_edge(&source.to_string(), &target.to_string(), relationship, None)
        .await
        .unwrap();
}

/// Seed a small graph with 3 nodes and 2 edges for reuse across tests.
async fn seed_graph(db: &MockGraphDB) -> (Uuid, Uuid, Uuid) {
    let id_a = Uuid::new_v4();
    let id_b = Uuid::new_v4();
    let id_c = Uuid::new_v4();

    add_node(db, id_a, "Alice", "Software engineer").await;
    add_node(db, id_b, "TechCorp", "Technology company").await;
    add_node(db, id_c, "Bob", "Product manager").await;
    add_edge(db, id_a, id_b, "works_at").await;
    add_edge(db, id_a, id_c, "knows").await;

    (id_a, id_b, id_c)
}

#[tokio::test]
async fn test_memify_end_to_end() {
    let graph_db = MockGraphDB::new();
    let vector_db = MockVectorDB::new();
    let engine = MockEmbeddingEngine::new(8);
    let config = MemifyConfig::default();

    let (_a, _b, _c) = seed_graph(&graph_db).await;

    let result = memify(
        &graph_db,
        &vector_db,
        &engine,
        Some(Uuid::new_v4()),
        Some(Uuid::new_v4()),
        None,
        &config,
    )
    .await
    .unwrap();

    assert_eq!(result.triplet_count, 2);
    assert_eq!(result.index_result.indexed_count, 2);
    assert!(result.index_result.batch_count >= 1);

    // Verify the vector collection was created and has 2 points
    assert!(vector_db.has_collection("Triplet", "text").await.unwrap());
    assert_eq!(
        vector_db.collection_size("Triplet", "text").await.unwrap(),
        2
    );
}

#[tokio::test]
async fn test_memify_idempotent() {
    let graph_db = MockGraphDB::new();
    let vector_db = MockVectorDB::new();
    let engine = MockEmbeddingEngine::new(8);
    let config = MemifyConfig::default();

    seed_graph(&graph_db).await;

    let r1 = memify(&graph_db, &vector_db, &engine, None, None, None, &config)
        .await
        .unwrap();

    let r2 = memify(&graph_db, &vector_db, &engine, None, None, None, &config)
        .await
        .unwrap();

    // Same number of triplets extracted both times
    assert_eq!(r1.triplet_count, r2.triplet_count);
    assert_eq!(r1.index_result.indexed_count, r2.index_result.indexed_count);

    // MockVectorDB does upsert, so collection size should still be 2 (not 4)
    assert_eq!(
        vector_db.collection_size("Triplet", "text").await.unwrap(),
        2,
        "idempotent upsert should not duplicate points"
    );
}

#[tokio::test]
async fn test_memify_empty_graph() {
    let graph_db = MockGraphDB::new();
    let vector_db = MockVectorDB::new();
    let engine = MockEmbeddingEngine::new(8);
    let config = MemifyConfig::default();

    let result = memify(&graph_db, &vector_db, &engine, None, None, None, &config)
        .await
        .unwrap();

    assert_eq!(result.triplet_count, 0);
    assert_eq!(result.index_result.indexed_count, 0);
    assert_eq!(result.index_result.batch_count, 0);

    // No collection should have been created
    assert!(
        !vector_db.has_collection("Triplet", "text").await.unwrap(),
        "no collection should be created for empty graph"
    );
}

#[tokio::test]
async fn test_memify_rejects_invalid_config() {
    let graph_db = MockGraphDB::new();
    let vector_db = MockVectorDB::new();
    let engine = MockEmbeddingEngine::new(8);
    let config = MemifyConfig::default().with_triplet_batch_size(0);

    let err = memify(&graph_db, &vector_db, &engine, None, None, None, &config)
        .await
        .unwrap_err();

    assert!(
        err.to_string().contains("triplet_batch_size"),
        "expected config validation error, got: {err}"
    );
}
