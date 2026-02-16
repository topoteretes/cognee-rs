//! DataPoint - Base model for all storage-layer entities.
//!
//! Mirrors Python's `cognee/infrastructure/engine/models/DataPoint.py`
//! Provides common fields for UUID, timestamps, versioning, and metadata.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// Base model for all storage-layer entities.
///
/// Provides:
/// - Unique identifier (UUID)
/// - Timestamps (created_at, updated_at)
/// - Ontology validation flag
/// - Version tracking
/// - Topological rank for graph traversal
/// - Flexible metadata storage
/// - Type discriminator
/// - Dataset membership
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DataPoint {
    /// Unique identifier
    pub id: Uuid,

    /// Creation timestamp
    pub created_at: DateTime<Utc>,

    /// Last update timestamp
    pub updated_at: DateTime<Utc>,

    /// Whether this entity has been validated against an ontology
    pub ontology_valid: bool,

    /// Version string (e.g., "1.0")
    pub version: String,

    /// Topological rank for graph traversal optimization
    pub topological_rank: Option<i32>,

    /// Flexible metadata storage (e.g., index_fields, custom attributes)
    pub metadata: HashMap<String, serde_json::Value>,

    /// Type discriminator (e.g., "Entity", "EntityType", "EdgeType")
    #[serde(rename = "type")]
    pub data_type: String,

    /// Dataset this data point belongs to
    pub belongs_to_set: Option<Uuid>,
}

impl DataPoint {
    /// Create a new DataPoint with default values.
    ///
    /// # Arguments
    /// * `data_type` - Type discriminator (e.g., "Entity", "EntityType")
    /// * `dataset_id` - Optional dataset UUID
    pub fn new(data_type: impl Into<String>, dataset_id: Option<Uuid>) -> Self {
        Self {
            id: Uuid::new_v4(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            ontology_valid: false,
            version: "1.0".to_string(),
            topological_rank: None,
            metadata: HashMap::new(),
            data_type: data_type.into(),
            belongs_to_set: dataset_id,
        }
    }

    /// Create a DataPoint with specific metadata.
    pub fn with_metadata(
        data_type: impl Into<String>,
        dataset_id: Option<Uuid>,
        metadata: HashMap<String, serde_json::Value>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            ontology_valid: false,
            version: "1.0".to_string(),
            topological_rank: None,
            metadata,
            data_type: data_type.into(),
            belongs_to_set: dataset_id,
        }
    }

    /// Get embeddable data as JSON string for vector indexing.
    ///
    /// Returns a JSON representation of this DataPoint.
    pub fn get_embeddable_data(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// Convert to JSON value.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }

    /// Update the timestamp to current time.
    pub fn touch(&mut self) {
        self.updated_at = Utc::now();
    }

    /// Set ontology validation status.
    pub fn set_ontology_valid(&mut self, valid: bool) {
        self.ontology_valid = valid;
        self.touch();
    }

    /// Add or update metadata field.
    pub fn set_metadata(&mut self, key: impl Into<String>, value: serde_json::Value) {
        self.metadata.insert(key.into(), value);
        self.touch();
    }

    /// Get metadata field.
    pub fn get_metadata(&self, key: &str) -> Option<&serde_json::Value> {
        self.metadata.get(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_data_point_creation() {
        let dp = DataPoint::new("TestType", None);
        assert_eq!(dp.data_type, "TestType");
        assert_eq!(dp.version, "1.0");
        assert!(!dp.ontology_valid);
        assert!(dp.metadata.is_empty());
    }

    #[test]
    fn test_data_point_with_dataset() {
        let dataset_id = Uuid::new_v4();
        let dp = DataPoint::new("Entity", Some(dataset_id));
        assert_eq!(dp.belongs_to_set, Some(dataset_id));
    }

    #[test]
    fn test_metadata_operations() {
        let mut dp = DataPoint::new("Entity", None);
        dp.set_metadata("index_fields", serde_json::json!(["name"]));

        assert_eq!(
            dp.get_metadata("index_fields"),
            Some(&serde_json::json!(["name"]))
        );
    }

    #[test]
    fn test_ontology_validation() {
        let mut dp = DataPoint::new("Entity", None);
        assert!(!dp.ontology_valid);

        dp.set_ontology_valid(true);
        assert!(dp.ontology_valid);
    }

    #[test]
    fn test_get_embeddable_data() {
        let dp = DataPoint::new("Entity", None);
        let json_str = dp.get_embeddable_data();
        assert!(json_str.contains("\"type\":\"Entity\""));
    }

    #[test]
    fn test_touch_updates_timestamp() {
        let mut dp = DataPoint::new("Entity", None);
        let original_time = dp.updated_at;

        std::thread::sleep(std::time::Duration::from_millis(10));
        dp.touch();

        assert!(dp.updated_at > original_time);
    }
}
