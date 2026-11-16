//! Graph integration types.
//!
//! Defines the pair types used for converting LLM-layer knowledge graphs
//! to storage-layer entities.

use cognee_models::{Entity, EntityType, Timestamp};
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

    /// The entity type (e.g., "Organization"), or `None` when the extracted
    /// type has no name — an empty or punctuation-only type from the LLM.
    /// Python's `_get_or_create_entity_type` returns `None` there rather than
    /// storing an EntityType named "", so the entity has no `is_a` edge.
    pub entity_type: Option<EntityType>,
}

/// A `Timestamp` node built from an extracted node the LLM typed "Timestamp".
///
/// Unlike an entity it has no `EntityType` and is not embedded; its id derives
/// from the normalized time string, so every chunk that names the same time
/// lands on one node. `description` is the extraction's description of the
/// first declaration — the `Timestamp` datapoint itself has no such field, but
/// the chunk's `contains` edge text quotes it, as Python's
/// `_link_chunk_to_entity` does.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TimestampNode {
    pub timestamp: Timestamp,
    pub description: String,
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

    /// [`GraphEdgePair::dedup_key`] → chunk ids that *extracted* the edge, in
    /// first-seen order.
    edges: HashMap<(Uuid, Uuid, String), Vec<Uuid>>,

    /// [`GraphEdgePair::dedup_key`] of an ontology edge → chunk ids whose
    /// entity or type pulled the ontology subgraph holding it in, in
    /// first-seen order.
    ///
    /// Kept apart from `edges` because the two feed different rows. Both make
    /// the chunk's document an owner of the edge, but only an extraction is
    /// evidence for it: Python appends to `_provenance_edges` (what edge
    /// evidence reads) only in `_add_extracted_edges`
    /// (`expand_with_nodes_and_edges.py:290-302`), while `_add_ontology_edges`
    /// only `setdefault`s the edge
    /// (`construct_data_points_and_edges_with_ontology.py:420-433`). A chunk
    /// that names "Einstein" is not evidence that `albert_einstein is_a
    /// scientist`.
    ontology_edges: HashMap<(Uuid, Uuid, String), Vec<Uuid>>,
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

    /// Record `chunk_id` as having *extracted* the edge identified by `key`.
    ///
    /// The chunk becomes an owner of the edge and evidence for it. Use
    /// [`record_ontology_edge`](Self::record_ontology_edge) for an edge the
    /// chunk only pulled in from the ontology.
    pub fn record_edge(&mut self, key: (Uuid, Uuid, String), chunk_id: Uuid) {
        push_unique(self.edges.entry(key).or_default(), chunk_id);
    }

    /// Record `chunk_id` as the chunk whose entity or type pulled in the
    /// ontology edge identified by `key`.
    ///
    /// The chunk's document owns the edge (see
    /// [`edge_owner_chunks`](Self::edge_owner_chunks)), but the chunk is not
    /// evidence for it: [`edges`](Self::edges) and
    /// [`edge_chunks`](Self::edge_chunks) do not report it.
    pub fn record_ontology_edge(&mut self, key: (Uuid, Uuid, String), chunk_id: Uuid) {
        push_unique(self.ontology_edges.entry(key).or_default(), chunk_id);
    }

    /// Chunks that produced `entity_id`, or an empty slice when unknown
    /// (ontology-derived entities have no producing chunk).
    pub fn entity_chunks(&self, entity_id: Uuid) -> &[Uuid] {
        self.entities.get(&entity_id).map_or(&[], Vec::as_slice)
    }

    /// Chunks that extracted the edge identified by `key`, or an empty slice
    /// when unknown. Ontology pullers are not included; see
    /// [`edge_owner_chunks`](Self::edge_owner_chunks).
    pub fn edge_chunks(&self, key: &(Uuid, Uuid, String)) -> &[Uuid] {
        self.edges.get(key).map_or(&[], Vec::as_slice)
    }

    /// Chunks whose documents own the edge identified by `key`: the chunks
    /// that extracted it, then the chunks that pulled it in from the ontology,
    /// deduplicated in that first-seen order. Empty when unknown.
    pub fn edge_owner_chunks(&self, key: &(Uuid, Uuid, String)) -> Vec<Uuid> {
        let mut owners = self.edge_chunks(key).to_vec();
        for &chunk_id in self.ontology_edges.get(key).map_or(&[][..], Vec::as_slice) {
            push_unique(&mut owners, chunk_id);
        }
        owners
    }

    /// Every extracted edge with the chunks that extracted it, in no
    /// particular order. Includes the edges an earlier run already wrote,
    /// which this run only claims (see
    /// [`partition_existing_edges`](super::db_deduplication::partition_existing_edges)).
    /// Edges only pulled in from the ontology are not included: this is what
    /// edge evidence reads, and an ontology edge has none.
    pub fn edges(&self) -> impl Iterator<Item = (&(Uuid, Uuid, String), &[Uuid])> {
        self.edges
            .iter()
            .map(|(key, chunks)| (key, chunks.as_slice()))
    }
}

/// Append `chunk_id` unless it is already listed, keeping first-seen order.
fn push_unique(chunks: &mut Vec<Uuid>, chunk_id: Uuid) {
    if !chunks.contains(&chunk_id) {
        chunks.push(chunk_id);
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
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
    fn edge_owner_chunks_unions_extractors_and_ontology_pullers() {
        let edge = GraphEdgePair::new(Uuid::new_v4(), Uuid::new_v4(), "is_a");
        let (extractor, puller, both) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());

        let mut producers = ArtifactProducers::default();
        producers.record_edge(edge.dedup_key(), extractor);
        producers.record_edge(edge.dedup_key(), both);
        producers.record_ontology_edge(edge.dedup_key(), puller);
        producers.record_ontology_edge(edge.dedup_key(), both);

        // Owners: extractors first, then pullers, each chunk once.
        assert_eq!(
            producers.edge_owner_chunks(&edge.dedup_key()),
            [extractor, both, puller]
        );
        // Evidence-side readers see the extractors only.
        assert_eq!(producers.edge_chunks(&edge.dedup_key()), [extractor, both]);
        let (_, chunks) = producers
            .edges()
            .next()
            .expect("the extracted edge is listed");
        assert_eq!(chunks, [extractor, both]);
    }

    #[test]
    fn an_edge_only_pulled_from_the_ontology_is_owned_but_not_listed_as_extracted() {
        let edge = GraphEdgePair::new(Uuid::new_v4(), Uuid::new_v4(), "is_a");
        let puller = Uuid::new_v4();

        let mut producers = ArtifactProducers::default();
        producers.record_ontology_edge(edge.dedup_key(), puller);

        assert_eq!(producers.edge_owner_chunks(&edge.dedup_key()), [puller]);
        assert!(producers.edge_chunks(&edge.dedup_key()).is_empty());
        assert_eq!(producers.edges().count(), 0);
    }

    #[test]
    fn test_graph_node_pair_structure() {
        let entity = Entity::new("TechCorp", None, "A technology company", None);
        let entity_type = EntityType::new("Organization", "", None);

        let node_pair = GraphNodePair {
            entity,
            entity_type: Some(entity_type),
        };

        assert_eq!(node_pair.entity.name, "TechCorp");
        assert_eq!(node_pair.entity_type.as_ref().unwrap().name, "Organization");
    }
}
