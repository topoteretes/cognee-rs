//! EdgeType - Storage-layer edge type model for indexing.
//!
//! Mirrors Python's `cognee/modules/engine/models/EdgeType.py`
//! Represents a type of relationship (e.g., "works_at", "located_in", "knows").

use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::DataPoint;
use crate::has_datapoint::HasDataPoint;

/// Storage-layer edge type model.
///
/// Represents a type of relationship between entities (e.g., "works_at",
/// "located_in", "knows"). Used for indexing and semantic search of
/// relationship types.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EdgeType {
    /// Base data point fields (id, timestamps, metadata, etc.)
    #[serde(flatten)]
    pub base: DataPoint,

    /// Relationship name (e.g., "works_at", "located_in")
    pub relationship_name: String,

    /// Number of edges of this type (for statistics)
    pub number_of_edges: i32,
}

impl EdgeType {
    /// Index fields to embed for vector search.
    pub const INDEX_FIELDS: &'static [&'static str] = &["relationship_name"];

    /// Compute a deterministic UUID for an EdgeType from its relationship name.
    ///
    /// Mirrors Python's `EdgeType.id_for(relationship_name)`
    /// (`identity_fields=["relationship_name"]`):
    /// `uuid5(NAMESPACE_OID, "EdgeType:<normalized relationship_name>")`.
    ///
    /// The `EdgeType:` class prefix was added upstream in cognee 1.2.0
    /// (`namespace_edge_type_point_ids.py`) — the previous bare-name scheme
    /// (`uuid5(OID, normalized)`) let a relationship and a same-named node
    /// collide on one point id.
    pub fn deterministic_id(relationship_name: &str) -> Uuid {
        cognee_utils::data_point_id_for("EdgeType", &[relationship_name])
    }

    /// Create a new EdgeType with a random UUID.
    ///
    /// # Arguments
    /// * `relationship_name` - Relationship name (e.g., "works_at")
    /// * `dataset_id` - Dataset UUID
    pub fn new(relationship_name: impl Into<String>, dataset_id: Option<Uuid>) -> Self {
        let mut metadata = std::collections::HashMap::new();
        metadata.insert(
            "index_fields".to_string(),
            serde_json::json!(Self::INDEX_FIELDS),
        );

        Self {
            base: DataPoint::with_metadata("EdgeType", dataset_id, metadata),
            relationship_name: relationship_name.into(),
            number_of_edges: 0,
        }
    }

    /// Create a new EdgeType with a deterministic UUID derived from the
    /// relationship name, matching Python's `generate_edge_id`.
    ///
    /// # Arguments
    /// * `relationship_name` - Relationship name (e.g., "works_at")
    /// * `dataset_id` - Dataset UUID
    pub fn new_deterministic(
        relationship_name: impl Into<String>,
        dataset_id: Option<Uuid>,
    ) -> Self {
        let name = relationship_name.into();
        let id = Self::deterministic_id(&name);
        let now = Utc::now().timestamp_millis();

        let mut metadata = std::collections::HashMap::new();
        metadata.insert(
            "index_fields".to_string(),
            serde_json::json!(Self::INDEX_FIELDS),
        );

        Self {
            base: DataPoint {
                id,
                created_at: now,
                updated_at: now,
                ontology_valid: false,
                version: 1,
                topological_rank: None,
                metadata,
                data_type: "EdgeType".to_string(),
                belongs_to_set: dataset_id.map(|ds_id| vec![serde_json::json!(ds_id.to_string())]),
                source_pipeline: None,
                source_task: None,
                source_node_set: None,
                source_user: None,
                source_content_hash: None,
                feedback_weight: 0.5,
            },
            relationship_name: name,
            number_of_edges: 0,
        }
    }

    /// Get the relationship name (for embedding).
    pub fn get_embeddable_text(&self) -> String {
        self.relationship_name.clone()
    }

    /// Increment the edge count.
    pub fn increment_count(&mut self) {
        self.number_of_edges += 1;
        self.base.touch();
    }

    /// Set the edge count.
    pub fn set_count(&mut self, count: i32) {
        self.number_of_edges = count;
        self.base.touch();
    }

    /// Get the edge count.
    pub fn count(&self) -> i32 {
        self.number_of_edges
    }
}

impl HasDataPoint for EdgeType {
    fn data_point(&self) -> &DataPoint {
        &self.base
    }
    fn data_point_mut(&mut self) -> &mut DataPoint {
        &mut self.base
    }
    // for_each_child_mut: default no-op — EdgeType has no nested
    // `HasDataPoint` children.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_edge_type_creation() {
        let et = EdgeType::new("works_at", None);

        assert_eq!(et.relationship_name, "works_at");
        assert_eq!(et.number_of_edges, 0);
        assert_eq!(et.base.data_type, "EdgeType");
    }

    #[test]
    fn test_edge_type_with_dataset() {
        let dataset_id = Uuid::new_v4();
        let et = EdgeType::new("works_at", Some(dataset_id));

        assert_eq!(
            et.base.belongs_to_set,
            Some(vec![serde_json::json!(dataset_id.to_string())])
        );
    }

    #[test]
    fn test_edge_type_index_fields() {
        let et = EdgeType::new("works_at", None);
        let index_fields = et.base.get_metadata("index_fields");

        assert_eq!(
            index_fields,
            Some(&serde_json::json!(["relationship_name"]))
        );
    }

    #[test]
    fn test_edge_type_embeddable_text() {
        let et = EdgeType::new("works_at", None);
        assert_eq!(et.get_embeddable_text(), "works_at");
    }

    #[test]
    fn test_edge_type_increment_count() {
        let mut et = EdgeType::new("works_at", None);
        assert_eq!(et.count(), 0);

        et.increment_count();
        assert_eq!(et.count(), 1);

        et.increment_count();
        assert_eq!(et.count(), 2);
    }

    #[test]
    fn test_edge_type_set_count() {
        let mut et = EdgeType::new("works_at", None);
        et.set_count(10);
        assert_eq!(et.count(), 10);
    }

    #[test]
    fn test_edge_type_increment_updates_timestamp() {
        let mut et = EdgeType::new("works_at", None);
        let old_time = et.base.updated_at;

        std::thread::sleep(std::time::Duration::from_millis(10));
        et.increment_count();

        // updated_at is i64 (millis since epoch); touch() should advance it
        assert!(et.base.updated_at >= old_time);
    }

    #[test]
    fn test_deterministic_id_basic() {
        let id1 = EdgeType::deterministic_id("works_at");
        let id2 = EdgeType::deterministic_id("works_at");
        assert_eq!(id1, id2, "same input must produce same UUID");
    }

    #[test]
    fn test_deterministic_id_normalization() {
        // Spaces become underscores, apostrophes removed, lowercased
        let id1 = EdgeType::deterministic_id("Works At");
        let id2 = EdgeType::deterministic_id("works_at");
        assert_eq!(
            id1, id2,
            "normalization should make 'Works At' equal 'works_at'"
        );

        let id3 = EdgeType::deterministic_id("it's_related");
        let id4 = EdgeType::deterministic_id("its_related");
        assert_eq!(id3, id4, "apostrophe removal should match");
    }

    #[test]
    fn test_deterministic_id_matches_python() {
        // Python: EdgeType.id_for("works_at") = uuid5(OID, "EdgeType:works_at")
        // (class-namespaced since cognee 1.2.0, namespace_edge_type_point_ids.py).
        let id = EdgeType::deterministic_id("works_at");
        assert_eq!(
            id,
            Uuid::new_v5(&Uuid::NAMESPACE_OID, b"EdgeType:works_at"),
            "deterministic_id('works_at') should equal uuid5(OID, 'EdgeType:works_at')"
        );
    }

    #[test]
    fn test_new_deterministic_constructor() {
        let et = EdgeType::new_deterministic("works_at", None);
        assert_eq!(et.relationship_name, "works_at");
        assert_eq!(et.base.data_type, "EdgeType");
        assert_eq!(et.base.id, EdgeType::deterministic_id("works_at"));
        assert_eq!(et.number_of_edges, 0);
    }

    #[test]
    fn test_new_deterministic_with_dataset() {
        let dataset_id = Uuid::new_v4();
        let et = EdgeType::new_deterministic("located_in", Some(dataset_id));
        assert_eq!(
            et.base.belongs_to_set,
            Some(vec![serde_json::json!(dataset_id.to_string())])
        );
        assert_eq!(et.base.id, EdgeType::deterministic_id("located_in"));
    }

    #[test]
    fn test_deterministic_id_different_names_differ() {
        let id1 = EdgeType::deterministic_id("works_at");
        let id2 = EdgeType::deterministic_id("located_in");
        assert_ne!(id1, id2, "different names must produce different UUIDs");
    }

    #[test]
    fn edge_type_implements_has_datapoint() {
        let et = EdgeType::new("rel", None);
        let dp_id = et.base.id;
        assert_eq!(et.data_point().id, dp_id);
        let mut et2 = et;
        assert_eq!(et2.data_point_mut().id, dp_id);
    }
}
