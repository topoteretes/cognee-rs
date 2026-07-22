//! Graph deduplication utilities.
//!
//! Mirrors Python's `cognee/modules/graph/utils/deduplicate_nodes_and_edges.py`
//! Provides in-memory deduplication of nodes and edges using HashMaps.

use std::collections::HashMap;

use crate::graph_integration::types::{GraphEdgePair, GraphNodePair};

/// Result of deduplication operation.
#[derive(Debug, Clone)]
pub struct DeduplicationResult {
    /// Unique nodes after deduplication
    pub unique_nodes: Vec<GraphNodePair>,

    /// Unique edges after deduplication
    pub unique_edges: Vec<GraphEdgePair>,
}

/// Deduplicate nodes and edges in-memory.
///
/// **Node deduplication key**: `str(entity.id)` (entity UUID as string)
/// **Edge deduplication key**: `"{source_id}_{target_id}_{relationship_name}"`
///
/// # Arguments
/// * `nodes` - Vector of GraphNodePair to deduplicate
/// * `edges` - Vector of GraphEdgePair to deduplicate
///
/// # Returns
/// DeduplicationResult with unique nodes and edges.
pub fn deduplicate_nodes_and_edges(
    nodes: Vec<GraphNodePair>,
    edges: Vec<GraphEdgePair>,
) -> DeduplicationResult {
    // Deduplicate nodes by entity ID
    let mut node_map = HashMap::new();
    for node in nodes {
        let key = node.entity.base.id;
        // Later entries with same ID will overwrite earlier ones
        node_map.insert(key, node);
    }

    // Deduplicate edges by (source, target, relationship) tuple
    let mut edge_map = HashMap::new();
    for edge in edges {
        let key = edge.dedup_key();
        // Later entries with same key will overwrite earlier ones
        edge_map.insert(key, edge);
    }

    DeduplicationResult {
        unique_nodes: node_map.into_values().collect(),
        unique_edges: edge_map.into_values().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cognee_models::{Entity, EntityType};
    use uuid::Uuid;

    fn create_test_node(name: &str, type_name: &str) -> GraphNodePair {
        let entity = Entity::new(name, None, format!("{name} description"), None);
        let entity_type = EntityType::from_node_type(type_name, None);
        GraphNodePair {
            entity,
            entity_type,
        }
    }

    fn create_test_edge(source_id: Uuid, target_id: Uuid, relationship: &str) -> GraphEdgePair {
        GraphEdgePair::new(source_id, target_id, relationship)
    }

    #[test]
    fn test_deduplicate_nodes_removes_duplicates() {
        // Two entities with the same name now share a deterministic id
        // (Entity::id_for), so they dedup without any manual id-forcing. This is
        // the unit-level regression for issue #57: random v4 ids used to make the
        // same entity duplicate silently across runs.
        let node1 = create_test_node("TechCorp", "Organization");
        let node2 = create_test_node("TechCorp", "Organization");
        assert_eq!(
            node1.entity.base.id, node2.entity.base.id,
            "same-name entities must derive the same id"
        );

        let result = deduplicate_nodes_and_edges(vec![node1, node2], vec![]);

        assert_eq!(result.unique_nodes.len(), 1);
        assert_eq!(result.unique_nodes[0].entity.name, "TechCorp");
    }

    #[test]
    fn test_deduplicate_keys_purely_on_entity_id() {
        // Two entities with DIFFERENT names but the same forced id must collapse
        // to one — proving the dedup key is `entity.base.id` alone (not name or
        // other fields). Guards against a future change that keys on more than
        // the id, which would let issue-#57-style silent duplication reappear.
        let node1 = create_test_node("Alpha", "Organization");
        let mut node2 = create_test_node("Beta", "Organization");
        node2.entity.base.id = node1.entity.base.id;

        let result = deduplicate_nodes_and_edges(vec![node1, node2], vec![]);

        assert_eq!(result.unique_nodes.len(), 1);
    }

    #[test]
    fn test_deduplicate_edges_removes_duplicates() {
        let source_id = Uuid::new_v4();
        let target_id = Uuid::new_v4();

        let edge1 = create_test_edge(source_id, target_id, "works_at");
        let edge2 = create_test_edge(source_id, target_id, "works_at");

        let result = deduplicate_nodes_and_edges(vec![], vec![edge1, edge2]);

        assert_eq!(result.unique_edges.len(), 1);
        assert_eq!(result.unique_edges[0].relationship_name, "works_at");
    }

    #[test]
    fn test_deduplicate_preserves_unique_nodes() {
        let node1 = create_test_node("TechCorp", "Organization");
        let node2 = create_test_node("Alice", "Person");
        let node3 = create_test_node("London", "Location");

        let result = deduplicate_nodes_and_edges(vec![node1, node2, node3], vec![]);

        assert_eq!(result.unique_nodes.len(), 3);

        // Verify all unique nodes are present (order might differ due to HashMap)
        let names: Vec<String> = result
            .unique_nodes
            .iter()
            .map(|n| n.entity.name.clone())
            .collect();
        assert!(names.contains(&"TechCorp".to_string()));
        assert!(names.contains(&"Alice".to_string()));
        assert!(names.contains(&"London".to_string()));
    }

    #[test]
    fn test_deduplicate_preserves_unique_edges() {
        let source_id = Uuid::new_v4();
        let target_id1 = Uuid::new_v4();
        let target_id2 = Uuid::new_v4();

        let edge1 = create_test_edge(source_id, target_id1, "works_at");
        let edge2 = create_test_edge(source_id, target_id2, "located_in");

        let result = deduplicate_nodes_and_edges(vec![], vec![edge1, edge2]);

        assert_eq!(result.unique_edges.len(), 2);
    }

    #[test]
    fn test_deduplicate_different_relationships_same_entities() {
        let source_id = Uuid::new_v4();
        let target_id = Uuid::new_v4();

        // Same source and target, different relationships
        let edge1 = create_test_edge(source_id, target_id, "works_at");
        let edge2 = create_test_edge(source_id, target_id, "founded");

        let result = deduplicate_nodes_and_edges(vec![], vec![edge1, edge2]);

        // Should keep both edges (different relationships)
        assert_eq!(result.unique_edges.len(), 2);
    }

    #[test]
    fn test_deduplicate_empty_input() {
        let result = deduplicate_nodes_and_edges(vec![], vec![]);

        assert_eq!(result.unique_nodes.len(), 0);
        assert_eq!(result.unique_edges.len(), 0);
    }

    #[test]
    fn test_deduplicate_later_entry_overwrites() {
        let source_id = Uuid::new_v4();
        let target_id = Uuid::new_v4();

        // Create two edges with same key but different properties
        let mut edge1 = create_test_edge(source_id, target_id, "works_at");
        edge1.add_property("since", "2020");

        let mut edge2 = create_test_edge(source_id, target_id, "works_at");
        edge2.add_property("since", "2021");

        let result = deduplicate_nodes_and_edges(vec![], vec![edge1, edge2]);

        assert_eq!(result.unique_edges.len(), 1);
        // Later entry (edge2) should win
        assert_eq!(
            result.unique_edges[0].properties.get("since"),
            Some(&"2021".to_string())
        );
    }

    #[test]
    fn test_deduplicate_mixed_unique_and_duplicate() {
        let node1 = create_test_node("TechCorp", "Organization");
        let node2 = create_test_node("Alice", "Person");
        // Same name → same deterministic id, no manual forcing needed.
        let node3 = create_test_node("Alice", "Person");

        let result = deduplicate_nodes_and_edges(vec![node1, node2, node3], vec![]);

        // Should have 2 unique nodes (TechCorp and Alice)
        assert_eq!(result.unique_nodes.len(), 2);
    }
}
