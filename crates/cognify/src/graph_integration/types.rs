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

/// Which source chunks produced each merged artifact.
///
/// Entity and edge merging is run-wide: the first chunk to yield an artifact
/// creates it and every later chunk reuses it, so the artifact itself
/// remembers only its first producer. Ownership rows are keyed per
/// (artifact, data item) — Python's `upsert_nodes.py:41` puts `data_id` in the
/// row id, and Rust's edge rows fold it in too — so the *set* of producing
/// chunks is what the provenance upsert needs to write one row per producing
/// file. Python gets this for free on the node side by running each data item
/// through its own task chain, and does not get it for edges at all.
///
/// Chunk ids, not data ids: expansion knows nothing about documents. The
/// chunk → data mapping is applied by the provenance upsert, which already
/// builds it.
///
/// Deliberately neither `Serialize` nor a field on [`GraphNodePair`] /
/// [`GraphEdgePair`]: those are serialized into graph node properties, vector
/// payloads and the ledger's `attributes` column, all of which Python reads.
/// A producer list has no Python counterpart and must not leak into any of
/// them.
#[derive(Debug, Clone, Default)]
pub struct ArtifactProducers {
    /// Final entity id → producing chunk ids, in first-seen order.
    entities: HashMap<Uuid, Vec<Uuid>>,

    /// [`GraphEdgePair::dedup_key`] → producing chunk ids, in first-seen order.
    edges: HashMap<(Uuid, Uuid, String), Vec<Uuid>>,
}

impl ArtifactProducers {
    /// Record `chunk_id` as a producer of `entity_id`.
    ///
    /// Insertion-ordered and deduplicated. Order is kept for determinism —
    /// re-cognifying the same input emits the ownership rows in the same
    /// sequence, so batches and diffs are comparable across runs. No rule
    /// depends on *which* producer comes first; every one of them gets a row.
    pub fn record_entity(&mut self, entity_id: Uuid, chunk_id: Uuid) {
        let producers = self.entities.entry(entity_id).or_default();
        if !producers.contains(&chunk_id) {
            producers.push(chunk_id);
        }
    }

    /// Record `chunk_id` as a producer of the edge identified by `key`.
    pub fn record_edge(&mut self, key: (Uuid, Uuid, String), chunk_id: Uuid) {
        let producers = self.edges.entry(key).or_default();
        if !producers.contains(&chunk_id) {
            producers.push(chunk_id);
        }
    }

    /// Chunks that produced `entity_id`, or an empty slice when unknown
    /// (ontology-derived entities have no producing chunk).
    pub fn entity_chunks(&self, entity_id: Uuid) -> &[Uuid] {
        self.entities.get(&entity_id).map_or(&[], Vec::as_slice)
    }

    /// Chunks that produced the edge identified by `key`, or an empty slice
    /// when unknown.
    pub fn edge_chunks(&self, key: &(Uuid, Uuid, String)) -> &[Uuid] {
        self.edges.get(key).map_or(&[], Vec::as_slice)
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
    fn artifact_producers_keep_every_producing_chunk_in_order() {
        let entity_id = Uuid::new_v4();
        let (c1, c2) = (Uuid::new_v4(), Uuid::new_v4());

        let mut producers = ArtifactProducers::default();
        producers.record_entity(entity_id, c1);
        producers.record_entity(entity_id, c2);
        // A repeat producer is not recorded twice.
        producers.record_entity(entity_id, c1);

        assert_eq!(producers.entity_chunks(entity_id), [c1, c2]);
        assert!(producers.entity_chunks(Uuid::new_v4()).is_empty());
    }

    #[test]
    fn artifact_producers_key_edges_on_the_dedup_key() {
        let source = Uuid::new_v4();
        let target = Uuid::new_v4();
        let (c1, c2) = (Uuid::new_v4(), Uuid::new_v4());
        let edge = GraphEdgePair::new(source, target, "knows");

        let mut producers = ArtifactProducers::default();
        producers.record_edge(edge.dedup_key(), c1);
        producers.record_edge(edge.dedup_key(), c2);

        assert_eq!(producers.edge_chunks(&edge.dedup_key()), [c1, c2]);
        assert!(
            producers
                .edge_chunks(&(source, target, "unrelated".to_string()))
                .is_empty()
        );
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
