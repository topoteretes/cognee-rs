//! Graph expansion logic.
//!
//! Mirrors Python's `cognee/modules/graph/utils/expand_with_nodes_and_edges.py`
//! Converts LLM-layer KnowledgeGraph objects to storage-layer Entity/EntityType pairs.

use std::collections::HashMap;
use uuid::Uuid;

use cognee_models::{Entity, EntityType};

use crate::fact_extraction::{KnowledgeGraph, Node};
use crate::graph_integration::types::{GraphEdgePair, GraphNodePair};

/// Graph integration error.
#[derive(Debug, thiserror::Error)]
pub enum GraphIntegrationError {
    #[error("Missing node reference: {0}")]
    MissingNodeReference(String),

    #[error("Invalid edge: source or target node not found")]
    InvalidEdge,
}

/// Core graph integration function. Converts LLM-layer KnowledgeGraph objects
/// to storage-layer Entity/EntityType pairs.
///
/// This mirrors the Python `expand_with_nodes_and_edges()` function from
/// `cognee/modules/graph/utils/expand_with_nodes_and_edges.py`.
///
/// # Process
/// 1. Create EntityType for each unique node type
/// 2. Create Entity for each node
/// 3. Create Edge for each relationship
/// 4. Deduplicate in-memory using HashMaps
///
/// # Deduplication Keys
/// - **Node**: `{node_id}_{category}` where category = "entity" or "type"
/// - **Edge**: `{source_entity_id}_{target_entity_id}_{relationship_name}`
///
/// # Arguments
/// * `graphs` - Vector of KnowledgeGraph objects from LLM extraction
/// * `chunk_id` - UUID of the chunk these graphs were extracted from
/// * `dataset_id` - UUID of the dataset
///
/// # Returns
/// Tuple of (graph_nodes, graph_edges) for storage.
pub async fn expand_with_nodes_and_edges(
    graphs: Vec<KnowledgeGraph>,
    chunk_id: Uuid,
    dataset_id: Uuid,
) -> Result<(Vec<GraphNodePair>, Vec<GraphEdgePair>), GraphIntegrationError> {
    // Maps for deduplication
    let mut node_map = HashMap::new();
    let mut edge_map = HashMap::new();
    let mut type_map = HashMap::new();

    // Map from node_id to entity_id for edge resolution
    let mut node_id_to_entity_id: HashMap<String, Uuid> = HashMap::new();

    // Process all graphs
    for graph in graphs {
        for node in graph.nodes {
            // Step 1: Create or get EntityType
            let type_key = format!("{}_type", node.node_type);
            let entity_type = type_map.entry(type_key.clone()).or_insert_with(|| {
                EntityType::from_node_type(&node.node_type, Some(dataset_id))
            });

            // Step 2: Create Entity
            let entity_key = format!("{}_entity", node.id);

            if let std::collections::hash_map::Entry::Vacant(e) = node_map.entry(entity_key) {
                let entity_pair = create_entity_node(
                    &node,
                    entity_type.clone(), // Pass the shared entity_type
                    dataset_id,
                    chunk_id,
                );

                // Track node_id -> entity_id mapping for edge resolution
                node_id_to_entity_id.insert(node.id.clone(), entity_pair.entity.base.id);

                e.insert(entity_pair);
            }
        }

        // Step 3: Create Edges
        for edge in graph.edges {
            // Look up entity IDs from node IDs
            let source_entity_id =
                node_id_to_entity_id
                    .get(&edge.source_node_id)
                    .ok_or_else(|| {
                        GraphIntegrationError::MissingNodeReference(edge.source_node_id.clone())
                    })?;

            let target_entity_id =
                node_id_to_entity_id
                    .get(&edge.target_node_id)
                    .ok_or_else(|| {
                        GraphIntegrationError::MissingNodeReference(edge.target_node_id.clone())
                    })?;

            let edge_key = (
                *source_entity_id, *target_entity_id, edge.relationship_name.clone()
            );

            if let std::collections::hash_map::Entry::Vacant(e) = edge_map.entry(edge_key) {
                e.insert(GraphEdgePair::new(
                        *source_entity_id,
                        *target_entity_id,
                        edge.relationship_name,
                    ));
            }
        }
    }

    // Convert maps to vectors
    let graph_nodes: Vec<GraphNodePair> = node_map.into_values().collect();
    let graph_edges: Vec<GraphEdgePair> = edge_map.into_values().collect();

    Ok((graph_nodes, graph_edges))
}

/// Helper: Create Entity from Node.
///
/// Mirrors Python's `_create_entity_node()` function.
fn create_entity_node(
    node: &Node,
    entity_type: EntityType,
    dataset_id: Uuid,
    chunk_id: Uuid,
) -> GraphNodePair {
    let entity = Entity::from_node(
        &node.id,
        &node.name,
        &node.description,
        entity_type.base.id,
        Some(dataset_id),
    );

    // Store chunk_id reference in metadata
    let mut entity_with_chunk = entity;
    entity_with_chunk
        .base
        .set_metadata("chunk_id", serde_json::json!(chunk_id.to_string()));

    GraphNodePair {
        entity: entity_with_chunk,
        entity_type,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fact_extraction::Edge;

    fn create_test_graph() -> KnowledgeGraph {
        KnowledgeGraph {
            nodes: vec![
                Node {
                    id: "techcorp_1".to_string(),
                    name: "TechCorp".to_string(),
                    node_type: "Organization".to_string(),
                    description: "A technology company".to_string(),
                },
                Node {
                    id: "alice_1".to_string(),
                    name: "Alice".to_string(),
                    node_type: "Person".to_string(),
                    description: "A software engineer".to_string(),
                },
            ],
            edges: vec![Edge {
                source_node_id: "alice_1".to_string(),
                target_node_id: "techcorp_1".to_string(),
                relationship_name: "works_at".to_string(),
            }],
        }
    }

    #[tokio::test]
    async fn test_expand_single_graph() {
        let graph = create_test_graph();
        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();

        let (nodes, edges) = expand_with_nodes_and_edges(vec![graph], chunk_id, dataset_id)
            .await
            .unwrap();

        // Should have 2 nodes (TechCorp, Alice)
        assert_eq!(nodes.len(), 2);

        // Should have 1 edge (Alice works_at TechCorp)
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].relationship_name, "works_at");

        // Verify node names
        let names: Vec<String> = nodes.iter().map(|n| n.entity.name.clone()).collect();
        assert!(names.contains(&"TechCorp".to_string()));
        assert!(names.contains(&"Alice".to_string()));
    }

    #[tokio::test]
    async fn test_expand_deduplicates_nodes() {
        let graph1 = create_test_graph();
        let graph2 = create_test_graph();

        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();

        let (nodes, edges) =
            expand_with_nodes_and_edges(vec![graph1, graph2], chunk_id, dataset_id)
                .await
                .unwrap();

        // Should have 2 unique nodes (deduplication by node_id)
        assert_eq!(nodes.len(), 2);

        // Should have 1 unique edge (deduplication by source+target+relationship)
        assert_eq!(edges.len(), 1);
    }

    #[tokio::test]
    async fn test_expand_creates_entity_types() {
        let graph = create_test_graph();
        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();

        let (nodes, _) = expand_with_nodes_and_edges(vec![graph], chunk_id, dataset_id)
            .await
            .unwrap();

        // Check that entity types are created
        for node_pair in &nodes {
            assert!(!node_pair.entity_type.name.is_empty());
            assert_eq!(node_pair.entity_type.base.data_type, "EntityType");
        }

        // Verify types
        let types: Vec<String> = nodes.iter().map(|n| n.entity_type.name.clone()).collect();
        assert!(types.contains(&"Organization".to_string()));
        assert!(types.contains(&"Person".to_string()));
    }

    #[tokio::test]
    async fn test_expand_links_entities_to_types() {
        let graph = create_test_graph();
        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();

        let (nodes, _) = expand_with_nodes_and_edges(vec![graph], chunk_id, dataset_id)
            .await
            .unwrap();

        // Check that entities reference their types
        for node_pair in &nodes {
            assert_eq!(node_pair.entity.is_a, Some(node_pair.entity_type.base.id));
        }
    }

    #[tokio::test]
    async fn test_expand_stores_chunk_reference() {
        let graph = create_test_graph();
        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();

        let (nodes, _) = expand_with_nodes_and_edges(vec![graph], chunk_id, dataset_id)
            .await
            .unwrap();

        // Verify chunk_id is stored in metadata
        for node_pair in &nodes {
            let chunk_ref = node_pair.entity.base.get_metadata("chunk_id");
            assert!(chunk_ref.is_some());
        }
    }

    #[tokio::test]
    async fn test_expand_missing_source_node() {
        let graph = KnowledgeGraph {
            nodes: vec![Node {
                id: "alice_1".to_string(),
                name: "Alice".to_string(),
                node_type: "Person".to_string(),
                description: "A person".to_string(),
            }],
            edges: vec![Edge {
                source_node_id: "alice_1".to_string(),
                target_node_id: "missing_node".to_string(), // Missing!
                relationship_name: "knows".to_string(),
            }],
        };

        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();

        let result = expand_with_nodes_and_edges(vec![graph], chunk_id, dataset_id).await;

        // Should fail with MissingNodeReference error
        assert!(result.is_err());
        match result {
            Err(GraphIntegrationError::MissingNodeReference(node_id)) => {
                assert_eq!(node_id, "missing_node");
            }
            _ => panic!("Expected MissingNodeReference error"),
        }
    }

    #[tokio::test]
    async fn test_expand_empty_graphs() {
        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();

        let (nodes, edges) = expand_with_nodes_and_edges(vec![], chunk_id, dataset_id)
            .await
            .unwrap();

        assert_eq!(nodes.len(), 0);
        assert_eq!(edges.len(), 0);
    }

    #[tokio::test]
    async fn test_expand_multiple_edges_same_entities() {
        let graph = KnowledgeGraph {
            nodes: vec![
                Node {
                    id: "alice_1".to_string(),
                    name: "Alice".to_string(),
                    node_type: "Person".to_string(),
                    description: "A person".to_string(),
                },
                Node {
                    id: "techcorp_1".to_string(),
                    name: "TechCorp".to_string(),
                    node_type: "Organization".to_string(),
                    description: "A company".to_string(),
                },
            ],
            edges: vec![
                Edge {
                    source_node_id: "alice_1".to_string(),
                    target_node_id: "techcorp_1".to_string(),
                    relationship_name: "works_at".to_string(),
                },
                Edge {
                    source_node_id: "alice_1".to_string(),
                    target_node_id: "techcorp_1".to_string(),
                    relationship_name: "founded".to_string(),
                },
            ],
        };

        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();

        let (nodes, edges) = expand_with_nodes_and_edges(vec![graph], chunk_id, dataset_id)
            .await
            .unwrap();

        assert_eq!(nodes.len(), 2);
        // Should have 2 edges (different relationships)
        assert_eq!(edges.len(), 2);

        let relationships: Vec<String> =
            edges.iter().map(|e| e.relationship_name.clone()).collect();
        assert!(relationships.contains(&"works_at".to_string()));
        assert!(relationships.contains(&"founded".to_string()));
    }
}
