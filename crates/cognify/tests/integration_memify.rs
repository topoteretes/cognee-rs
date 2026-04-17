#![cfg(feature = "testing")]

use cognee_cognify::graph_integration::{GraphEdgePair, GraphNodePair};
use cognee_cognify::memify::extract_triplets::extract_triplets_from_graph_db;
use cognee_cognify::memify::{MemifyConfig, memify};
use cognee_cognify::triplet_creation::create_triplets_from_graph;
use cognee_embedding::MockEmbeddingEngine;
use cognee_graph::{GraphDBTrait, MockGraphDB};
use cognee_models::{Entity, EntityType};
use cognee_vector::{MockVectorDB, VectorDB};
use serde_json::json;
use std::collections::HashMap;
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

// ⚠️ CRITICAL: Breaking this test means memify will duplicate-insert
// instead of upsert vector points, corrupting production deployments.
// Both memify and cognify must derive Triplet.id identically (see
// Triplet::new in crates/models/src/triplet.rs).
#[tokio::test]
async fn test_memify_idempotent_ids_match_cognify() {
    // --- Seed the mock graph for memify's extract path ---
    let graph_db = MockGraphDB::new();

    let id_a = Uuid::new_v4();
    let id_b = Uuid::new_v4();
    let id_c = Uuid::new_v4();

    add_node(&graph_db, id_a, "Alice", "Software engineer").await;
    add_node(&graph_db, id_b, "TechCorp", "Technology company").await;
    add_node(&graph_db, id_c, "Bob", "Product manager").await;
    add_edge(&graph_db, id_a, id_b, "works_at").await;
    add_edge(&graph_db, id_a, id_c, "knows").await;
    add_edge(&graph_db, id_b, id_c, "employs").await;

    // --- Build parallel GraphNodePair / GraphEdgePair representations
    // for cognify's synchronous create_triplets_from_graph helper.
    // Same UUIDs + same (source, relationship, target) tuples guarantee
    // the deterministic Triplet::new() derivation is driven by identical
    // inputs on both sides.
    fn make_node(id: Uuid, name: &str, description: &str) -> GraphNodePair {
        let mut entity = Entity::new(name, None, description, None);
        entity.base.id = id;
        let entity_type = EntityType::new("Generic", "Generic type", None);
        GraphNodePair {
            entity,
            entity_type,
        }
    }

    let nodes = vec![
        make_node(id_a, "Alice", "Software engineer"),
        make_node(id_b, "TechCorp", "Technology company"),
        make_node(id_c, "Bob", "Product manager"),
    ];
    let edges = vec![
        GraphEdgePair::new(id_a, id_b, "works_at"),
        GraphEdgePair::new(id_a, id_c, "knows"),
        GraphEdgePair::new(id_b, id_c, "employs"),
    ];

    // --- Run both paths ---
    let memify_config = MemifyConfig::default();
    let memify_triplets = extract_triplets_from_graph_db(&graph_db, &memify_config)
        .await
        .expect("memify extract should succeed on seeded mock graph");

    let cognify_triplets = create_triplets_from_graph(&nodes, &edges);

    // --- Compare counts ---
    assert_eq!(
        memify_triplets.len(),
        cognify_triplets.len(),
        "memify and cognify should produce the same number of triplets for \
         the same logical graph state (memify={}, cognify={})",
        memify_triplets.len(),
        cognify_triplets.len(),
    );
    assert_eq!(
        memify_triplets.len(),
        3,
        "sanity: all three seeded edges should yield triplets"
    );

    // --- Compare (source, rel, target) -> id maps ---
    let memify_map: HashMap<(Uuid, String, Uuid), Uuid> = memify_triplets
        .iter()
        .map(|t| {
            (
                (
                    t.source_entity_id,
                    t.relationship_name.clone(),
                    t.target_entity_id,
                ),
                t.id,
            )
        })
        .collect();
    let cognify_map: HashMap<(Uuid, String, Uuid), Uuid> = cognify_triplets
        .iter()
        .map(|t| {
            (
                (
                    t.source_entity_id,
                    t.relationship_name.clone(),
                    t.target_entity_id,
                ),
                t.id,
            )
        })
        .collect();

    assert_eq!(
        memify_map.len(),
        memify_triplets.len(),
        "memify triplets must have unique (source, rel, target) tuples"
    );
    assert_eq!(
        cognify_map.len(),
        cognify_triplets.len(),
        "cognify triplets must have unique (source, rel, target) tuples"
    );

    // Key sets must be identical.
    let memify_keys: std::collections::HashSet<_> = memify_map.keys().collect();
    let cognify_keys: std::collections::HashSet<_> = cognify_map.keys().collect();
    assert_eq!(
        memify_keys, cognify_keys,
        "memify and cognify must cover the same (source, rel, target) tuples"
    );

    // For every shared key, the derived UUID5 id must match exactly.
    for (key, memify_id) in &memify_map {
        let cognify_id = cognify_map
            .get(key)
            .expect("key presence already asserted by set equality above");
        assert_eq!(
            memify_id, cognify_id,
            "Triplet.id diverges between memify and cognify for \
             (source={}, rel={}, target={}): memify={}, cognify={}. \
             This would cause duplicate vector points instead of upsert.",
            key.0, key.1, key.2, memify_id, cognify_id,
        );
    }
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

/// Helper: add a node with an explicit `type` property.
///
/// Used by the filter-path integration tests below to populate primary /
/// non-primary nodes exercised by `get_nodeset_subgraph`.
async fn add_typed_node(
    db: &MockGraphDB,
    id: Uuid,
    name: &str,
    node_type: &str,
    description: &str,
) {
    let mut node_json = serde_json::Map::new();
    node_json.insert("id".to_string(), json!(id.to_string()));
    node_json.insert("name".to_string(), json!(name));
    node_json.insert("type".to_string(), json!(node_type));
    if !description.is_empty() {
        node_json.insert("description".to_string(), json!(description));
    }
    db.add_node_raw(serde_json::Value::Object(node_json))
        .await
        .unwrap();
}

/// Seed a graph that exercises type + name filtering.
///
/// - 3 Entity nodes: Alice, Bob, Carol
/// - 1 Concept node: Idea1
/// - Edges:
///   Alice --knows--> Bob     (Entity↔Entity, Alice primary),
///   Bob   --knows--> Carol   (Entity↔Entity, Bob primary only),
///   Alice --likes--> Idea1   (Entity→Concept, Alice primary only)
///
/// With type=Entity, names=[Alice,Bob]:
/// OR  → included = {Alice,Bob,Carol,Idea1} → all 3 edges survive.
/// AND → included = {Alice,Bob}             → only Alice-knows-Bob survives.
async fn seed_filter_graph(db: &MockGraphDB) -> (Uuid, Uuid, Uuid, Uuid) {
    let alice = Uuid::new_v4();
    let bob = Uuid::new_v4();
    let carol = Uuid::new_v4();
    let idea1 = Uuid::new_v4();

    add_typed_node(db, alice, "Alice", "Entity", "Person A").await;
    add_typed_node(db, bob, "Bob", "Entity", "Person B").await;
    add_typed_node(db, carol, "Carol", "Entity", "Person C").await;
    add_typed_node(db, idea1, "Idea1", "Concept", "An idea").await;

    add_edge(db, alice, bob, "knows").await;
    add_edge(db, bob, carol, "knows").await;
    add_edge(db, alice, idea1, "likes").await;

    (alice, bob, carol, idea1)
}

#[tokio::test]
async fn test_memify_with_type_and_names_filter_or() {
    let graph_db = MockGraphDB::new();
    let vector_db = MockVectorDB::new();
    let engine = MockEmbeddingEngine::new(8);

    let (_alice, _bob, _carol, _idea1) = seed_filter_graph(&graph_db).await;

    let config = MemifyConfig::default()
        .with_node_type_filter("Entity".to_string())
        .with_node_name_filter(vec!["Alice".to_string(), "Bob".to_string()])
        .with_node_name_filter_operator("OR".to_string());

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

    // OR: primaries (Alice,Bob) ∪ all neighbors (Carol, Idea1) = {A,B,C,Idea1}.
    // All 3 edges have both endpoints in the included set, so all 3 survive.
    assert_eq!(
        result.triplet_count, 3,
        "OR filter should keep all 3 edges between the included primaries and their neighbors"
    );
    assert_eq!(result.index_result.indexed_count, 3);
    assert_eq!(
        vector_db.collection_size("Triplet", "text").await.unwrap(),
        3
    );
}

#[tokio::test]
async fn test_memify_with_type_and_names_filter_and() {
    let graph_db = MockGraphDB::new();
    let vector_db = MockVectorDB::new();
    let engine = MockEmbeddingEngine::new(8);

    let (_alice, _bob, _carol, _idea1) = seed_filter_graph(&graph_db).await;

    let config = MemifyConfig::default()
        .with_node_type_filter("Entity".to_string())
        .with_node_name_filter(vec!["Alice".to_string(), "Bob".to_string()])
        .with_node_name_filter_operator("AND".to_string());

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

    // AND: only neighbors connected to BOTH Alice and Bob qualify.
    // Neither Carol nor Idea1 are connected to both, so included = {Alice,Bob}.
    // The only edge with both endpoints included is Alice-knows-Bob.
    assert_eq!(
        result.triplet_count, 1,
        "AND filter should keep only the Alice-knows-Bob edge (both endpoints are primaries)"
    );
    assert_eq!(result.index_result.indexed_count, 1);
    assert_eq!(
        vector_db.collection_size("Triplet", "text").await.unwrap(),
        1
    );
}
