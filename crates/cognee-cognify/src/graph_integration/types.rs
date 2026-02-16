//! Graph integration types.
//!
//! Defines the pair types used for converting LLM-layer knowledge graphs
//! to storage-layer entities.

use cognee_models::{Entity, EntityType};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// A pair of (Entity, EntityType) representing a node in the storage layer.
///
/// When processing a KnowledgeGraph, each Node is converted to an Entity
/// with a corresponding EntityType. This struct holds both for storage.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphNodePair {
    /// The entity instance (e.g., "TechCorp")
    pub entity: Entity,

    /// The entity type (e.g., "Organization")
    pub entity_type: EntityType,
}

/// An edge in the storage layer with source/target entities.
///
/// Represents a relationship between two entities. Unlike the LLM-layer Edge
/// which uses node IDs, this uses entity UUIDs for database storage.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphEdgePair {
    /// Source entity UUID
    pub source_entity_id: Uuid,

    /// Target entity UUID
    pub target_entity_id: Uuid,

    /// Relationship name (e.g., "works_at", "located_in")
    pub relationship_name: String,

    /// Additional edge properties (flexible key-value storage)
    pub properties: HashMap<String, String>,
}

impl GraphEdgePair {
    /// Create a new GraphEdgePair.
    pub fn new(
        source_entity_id: Uuid,
        target_entity_id: Uuid,
        relationship_name: impl Into<String>,
    ) -> Self {
        Self {
            source_entity_id,
            target_entity_id,
            relationship_name: relationship_name.into(),
            properties: HashMap::new(),
        }
    }

    /// Create an edge with properties.
    pub fn with_properties(
        source_entity_id: Uuid,
        target_entity_id: Uuid,
        relationship_name: impl Into<String>,
        properties: HashMap<String, String>,
    ) -> Self {
        Self {
            source_entity_id,
            target_entity_id,
            relationship_name: relationship_name.into(),
            properties,
        }
    }

    /// Get the deduplication key for this edge.
    ///
    /// Format: "{source_id}_{target_id}_{relationship_name}"
    /// This matches the Python implementation.
    pub fn dedup_key(&self) -> (Uuid, Uuid, String) {
        (
            self.source_entity_id,
            self.target_entity_id,
            self.relationship_name.clone(),
        )
    }

    /// Add a property to the edge.
    pub fn add_property(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.properties.insert(key.into(), value.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cognee_models::{Entity, EntityType};

    #[test]
    fn test_graph_edge_pair_creation() {
        let source_id = Uuid::new_v4();
        let target_id = Uuid::new_v4();

        let edge = GraphEdgePair::new(source_id, target_id, "works_at");

        assert_eq!(edge.source_entity_id, source_id);
        assert_eq!(edge.target_entity_id, target_id);
        assert_eq!(edge.relationship_name, "works_at");
        assert!(edge.properties.is_empty());
    }

    #[test]
    fn test_graph_edge_pair_with_properties() {
        let source_id = Uuid::new_v4();
        let target_id = Uuid::new_v4();
        let mut props = HashMap::new();
        props.insert("since".to_string(), "2020".to_string());

        let edge = GraphEdgePair::with_properties(source_id, target_id, "works_at", props);

        assert_eq!(edge.properties.get("since"), Some(&"2020".to_string()));
    }

    #[test]
    fn test_edge_add_property() {
        let source_id = Uuid::new_v4();
        let target_id = Uuid::new_v4();
        let mut edge = GraphEdgePair::new(source_id, target_id, "works_at");

        edge.add_property("since", "2020");
        assert_eq!(edge.properties.get("since"), Some(&"2020".to_string()));
    }

    #[test]
    fn test_graph_node_pair_structure() {
        let entity = Entity::new("TechCorp", None, "A technology company", None);
        let entity_type = EntityType::new("Organization", "", None);

        let node_pair = GraphNodePair {
            entity,
            entity_type,
        };

        assert_eq!(node_pair.entity.name, "TechCorp");
        assert_eq!(node_pair.entity_type.name, "Organization");
    }
}
