//! Graph integration types.
//!
//! Defines the pair types used for converting LLM-layer knowledge graphs
//! to storage-layer entities.

use cognee_models::{Entity, EntityType, Timestamp};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
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

    /// Every further type, in first-seen order, never repeating
    /// [`Self::entity_type`]. An entity that several documents (or blocks of
    /// one document) typed differently keeps each of their types, as Python
    /// does; see [`Self::merge_types_from`]. Kept in step with
    /// `entity.extra_types`, which is what becomes the extra `is_a` edges.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_entity_types: Vec<EntityType>,
}

impl GraphNodePair {
    /// A node with at most one type.
    pub fn new(entity: Entity, entity_type: Option<EntityType>) -> Self {
        Self {
            entity,
            entity_type,
            extra_entity_types: Vec::new(),
        }
    }

    /// Every type of this node: the primary one first, then the extra ones.
    pub fn entity_types(&self) -> impl Iterator<Item = &EntityType> {
        self.entity_type
            .iter()
            .chain(self.extra_entity_types.iter())
    }

    /// Every type of this node, mutably, in [`Self::entity_types`] order.
    pub fn entity_types_mut(&mut self) -> impl Iterator<Item = &mut EntityType> {
        self.entity_type
            .iter_mut()
            .chain(self.extra_entity_types.iter_mut())
    }

    /// Add the types of `other` to this node — the one rule that decides
    /// which types an entity declared in several units keeps.
    ///
    /// Python's rule: within one unit (a data item, or a block of
    /// `chunks_per_batch` chunks of it) the first type declared wins, and
    /// the types of every unit are combined. Callers have already applied
    /// the first half; this is the second: a type this node does not have yet
    /// is appended, in first-seen order, and this node's first type stays
    /// primary. An untyped node takes `other`'s first type as its primary.
    ///
    /// The [`ONTOLOGY_INDIVIDUAL_TYPE`] placeholder is not a type anybody
    /// declared — Rust gives it to an ontology individual no extraction typed
    /// (Python leaves such an entity untyped), and it never becomes an `is_a`
    /// edge. It never joins a node that has a real type, and a real type
    /// replaces it as primary.
    ///
    /// This function only combines units. Switching to "every declared type,
    /// with counts" is not local to it: inside a unit `expand_unit` keeps
    /// only the first declaration's type (a later chunk's type for the same
    /// entity never reaches a [`GraphNodePair`]), so the switch would need
    /// `expand_unit` to keep every declared type of an entity with a count,
    /// a count per type on [`GraphNodePair`], and this function to sum the
    /// counts and pick the most frequent type as primary.
    pub fn merge_types_from(&mut self, other: &GraphNodePair) {
        for entity_type in other.entity_types() {
            let id = entity_type.base.id;
            if self.entity_types().any(|known| known.base.id == id) {
                continue;
            }
            let placeholder = is_ontology_individual_placeholder(entity_type);
            if placeholder && self.entity_types().next().is_some() {
                continue;
            }
            let replaces_placeholder = self
                .entity_type
                .as_ref()
                .is_some_and(is_ontology_individual_placeholder);
            if self.entity_type.is_none() || replaces_placeholder {
                self.entity_type = Some(entity_type.clone());
                // The placeholder is never an `is_a` edge: an ontology
                // individual's entity has no `is_a` of its own.
                self.entity.is_a = (!placeholder).then_some(id);
            } else {
                self.extra_entity_types.push(entity_type.clone());
                self.entity.extra_types.push(id);
            }
        }
    }
}

/// Name of the placeholder EntityType an ontology individual gets when no
/// extraction typed it. Rust-only: Python's `Entity.is_a` is optional and
/// such an entity has none.
pub const ONTOLOGY_INDIVIDUAL_TYPE: &str = "OntologyIndividual";

/// Whether `entity_type` is the [`ONTOLOGY_INDIVIDUAL_TYPE`] placeholder.
pub fn is_ontology_individual_placeholder(entity_type: &EntityType) -> bool {
    entity_type.base.id == EntityType::id_for(ONTOLOGY_INDIVIDUAL_TYPE)
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
/// Entity and edge merging spans the run: within a unit (a data item, or a
/// `chunks_per_batch` block of one) the first chunk to yield an artifact
/// creates it and every later chunk reuses it, and the units are then merged,
/// so the artifact itself remembers only its first producer. Ownership rows are keyed per
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
///
/// # Types and ontology output (SDK-1694 O5a)
///
/// Besides the extracted entities and edges, it records:
///
/// - **types**: `(entity, type) → chunks` and `type → chunks`. Within a unit
///   an entity has the type its first declaration gave it, so every chunk
///   that owns the entity in that unit owns that `(entity, type)` pair too
///   (Python's `collect_chunk_ownership` hands a chunk "the entities it
///   `contains` and their types"). After the merge an entity can carry the
///   types of several documents; these maps say which chunk gave which.
/// - **ontology output**: the entities and edges an ontology lookup brought
///   in, against the chunk whose declaration triggered the lookup. Kept apart
///   from [`Self::entity_chunks`] / [`Self::edge_chunks`], which also decide a
///   chunk's `contains` links: Python links a chunk only to what it extracted.
///   An ontology entity is recorded only when its unit did not also extract
///   it. A lookup runs once per unit for a class, so these are exact per
///   document (what the ledger keys on), not per chunk.
/// - **ontology-first entities**: the entities whose first appearance in the
///   run was through the ontology alone. Set by the merge, which sees the
///   run's units in order.
#[derive(Debug, Clone, Default)]
pub struct ArtifactProducers {
    /// Final entity id → producing chunk ids, in first-seen order.
    entities: HashMap<Uuid, Vec<Uuid>>,

    /// [`GraphEdgePair::dedup_key`] → producing chunk ids, in first-seen order.
    edges: HashMap<(Uuid, Uuid, String), Vec<Uuid>>,

    /// `(entity id, EntityType id)` → chunks that declared the entity with it.
    entity_types: HashMap<(Uuid, Uuid), Vec<Uuid>>,

    /// EntityType id → chunks that declared some entity with it.
    types: HashMap<Uuid, Vec<Uuid>>,

    /// Entity id → chunks whose ontology lookup brought it in, where the
    /// chunk's unit did not extract it.
    ontology_entities: HashMap<Uuid, Vec<Uuid>>,

    /// Ontology edge key → chunks whose ontology lookup brought it in.
    ontology_edges: HashMap<(Uuid, Uuid, String), Vec<Uuid>>,

    /// Entities first seen in the run through the ontology alone.
    ontology_first: HashSet<Uuid>,

    /// Whether graph building recorded this set (`expand_unit`), as opposed to
    /// a caller handing over a `SummarizedData` it built itself. Only a
    /// recorded set says that a type it does not name was declared by some
    /// other document (SDK-1694 O6 review).
    recorded: bool,
}

/// Append `chunk_id` to `chunks` unless it is already there.
fn push_producer(chunks: &mut Vec<Uuid>, chunk_id: Uuid) {
    if !chunks.contains(&chunk_id) {
        chunks.push(chunk_id);
    }
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

    /// Record `chunk_id` as a producer of `entity_id` declared with
    /// `type_id`, and so of the `is_a` edge between them and of the type.
    pub fn record_entity_type(&mut self, entity_id: Uuid, type_id: Uuid, chunk_id: Uuid) {
        push_producer(
            self.entity_types.entry((entity_id, type_id)).or_default(),
            chunk_id,
        );
        push_producer(self.types.entry(type_id).or_default(), chunk_id);
    }

    /// Record `chunk_id` as the chunk whose ontology lookup brought in
    /// `entity_id`, which its unit did not extract.
    pub fn record_ontology_entity(&mut self, entity_id: Uuid, chunk_id: Uuid) {
        push_producer(
            self.ontology_entities.entry(entity_id).or_default(),
            chunk_id,
        );
    }

    /// Record `chunk_id` as the chunk whose ontology lookup brought in the
    /// ontology edge identified by `key`.
    pub fn record_ontology_edge(&mut self, key: (Uuid, Uuid, String), chunk_id: Uuid) {
        push_producer(self.ontology_edges.entry(key).or_default(), chunk_id);
    }

    /// Mark `entity_id` as first seen in the run through the ontology alone.
    pub fn mark_ontology_first(&mut self, entity_id: Uuid) {
        self.ontology_first.insert(entity_id);
    }

    /// Add every producer `other` recorded, after the ones already here.
    pub fn merge(&mut self, other: ArtifactProducers) {
        for (entity_id, chunks) in other.entities {
            for chunk_id in chunks {
                self.record_entity(entity_id, chunk_id);
            }
        }
        for (key, chunks) in other.edges {
            for chunk_id in chunks {
                self.record_edge(key.clone(), chunk_id);
            }
        }
        for (key, chunks) in other.entity_types {
            let entry = self.entity_types.entry(key).or_default();
            for chunk_id in chunks {
                push_producer(entry, chunk_id);
            }
        }
        for (type_id, chunks) in other.types {
            let entry = self.types.entry(type_id).or_default();
            for chunk_id in chunks {
                push_producer(entry, chunk_id);
            }
        }
        for (entity_id, chunks) in other.ontology_entities {
            for chunk_id in chunks {
                self.record_ontology_entity(entity_id, chunk_id);
            }
        }
        for (key, chunks) in other.ontology_edges {
            for chunk_id in chunks {
                self.record_ontology_edge(key.clone(), chunk_id);
            }
        }
        self.ontology_first.extend(other.ontology_first);
        self.recorded |= other.recorded;
    }

    /// Chunks that declared `entity_id` with `type_id`, or an empty slice.
    pub fn entity_type_chunks(&self, entity_id: Uuid, type_id: Uuid) -> &[Uuid] {
        self.entity_types
            .get(&(entity_id, type_id))
            .map_or(&[], Vec::as_slice)
    }

    /// Mark the set as recorded by graph building (`expand_unit`).
    pub fn mark_recorded(&mut self) {
        self.recorded = true;
    }

    /// Whether graph building recorded this set ([`Self::mark_recorded`],
    /// carried through [`Self::merge`]). A recorded set names every type its
    /// units' entities were declared with, so a type it does not name came
    /// from another document. A hand-built set (`ArtifactProducers::default`)
    /// is not recorded, whatever it holds.
    pub fn is_recorded(&self) -> bool {
        self.recorded
    }

    /// Chunks that declared some entity with `type_id`, or an empty slice.
    pub fn type_chunks(&self, type_id: Uuid) -> &[Uuid] {
        self.types.get(&type_id).map_or(&[], Vec::as_slice)
    }

    /// Chunks whose ontology lookup brought in `entity_id` (which their unit
    /// did not extract), or an empty slice.
    pub fn ontology_entity_chunks(&self, entity_id: Uuid) -> &[Uuid] {
        self.ontology_entities
            .get(&entity_id)
            .map_or(&[], Vec::as_slice)
    }

    /// Chunks whose ontology lookup brought in the edge `key`, or an empty
    /// slice.
    pub fn ontology_edge_chunks(&self, key: &(Uuid, Uuid, String)) -> &[Uuid] {
        self.ontology_edges.get(key).map_or(&[], Vec::as_slice)
    }

    /// Whether some chunk extracted the edge `key` or brought it in through
    /// the ontology.
    pub fn produces_edge(&self, key: &(Uuid, Uuid, String)) -> bool {
        !self.edge_chunks(key).is_empty() || !self.ontology_edge_chunks(key).is_empty()
    }

    /// Whether `entity_id` was first seen in the run through the ontology
    /// alone.
    pub fn is_ontology_first(&self, entity_id: Uuid) -> bool {
        self.ontology_first.contains(&entity_id)
    }

    /// Chunks that extracted `entity_id`, or an empty slice when unknown. An
    /// entity only an ontology lookup brought in has none here; see
    /// [`Self::ontology_entity_chunks`].
    pub fn entity_chunks(&self, entity_id: Uuid) -> &[Uuid] {
        self.entities.get(&entity_id).map_or(&[], Vec::as_slice)
    }

    /// Chunks that produced the edge identified by `key`, or an empty slice
    /// when unknown.
    pub fn edge_chunks(&self, key: &(Uuid, Uuid, String)) -> &[Uuid] {
        self.edges.get(key).map_or(&[], Vec::as_slice)
    }

    /// Every produced edge with its producing chunks, in no particular order.
    /// Includes the edges an earlier run already wrote, which this run only
    /// claims (see `claimed_existing_edges`).
    pub fn edges(&self) -> impl Iterator<Item = (&(Uuid, Uuid, String), &[Uuid])> {
        self.edges
            .iter()
            .map(|(key, chunks)| (key, chunks.as_slice()))
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
    fn test_graph_node_pair_structure() {
        let entity = Entity::new("TechCorp", None, "A technology company", None);
        let entity_type = EntityType::new("Organization", "", None);

        let node_pair = GraphNodePair::new(entity, Some(entity_type));

        assert_eq!(node_pair.entity.name, "TechCorp");
        assert_eq!(node_pair.entity_type.as_ref().unwrap().name, "Organization");
    }
}
