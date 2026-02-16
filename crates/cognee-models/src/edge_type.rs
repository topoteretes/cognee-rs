//! EdgeType - Storage-layer edge type model for indexing.
//!
//! Mirrors Python's `cognee/modules/engine/models/EdgeType.py`
//! Represents a type of relationship (e.g., "works_at", "located_in", "knows").

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::DataPoint;

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

    /// Create a new EdgeType.
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

        assert_eq!(et.base.belongs_to_set, Some(dataset_id));
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

        assert!(et.base.updated_at > old_time);
    }
}
