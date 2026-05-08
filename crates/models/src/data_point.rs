//! DataPoint - Base model for all storage-layer entities.
//!
//! Mirrors Python's `cognee/infrastructure/engine/models/DataPoint.py`
//! Provides common fields for UUID, timestamps, versioning, and metadata.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// Default value for `feedback_weight` (used by serde).
fn default_feedback_weight() -> f64 {
    0.5
}

/// Default value for `version` (used by serde).
fn default_version() -> i32 {
    1
}

/// Base model for all storage-layer entities.
///
/// Provides:
/// - Unique identifier (UUID)
/// - Timestamps (created_at, updated_at) as milliseconds since epoch
/// - Ontology validation flag
/// - Version tracking (integer)
/// - Topological rank for graph traversal
/// - Flexible metadata storage
/// - Type discriminator
/// - Dataset membership
/// - Pipeline provenance fields
/// - Feedback weight
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DataPoint {
    /// Unique identifier
    pub id: Uuid,

    /// Creation timestamp (milliseconds since epoch, matching Python)
    pub created_at: i64,

    /// Last update timestamp (milliseconds since epoch, matching Python)
    pub updated_at: i64,

    /// Whether this entity has been validated against an ontology
    pub ontology_valid: bool,

    /// Version number (default 1, matching Python)
    #[serde(default = "default_version")]
    pub version: i32,

    /// Topological rank for graph traversal optimization
    pub topological_rank: Option<i32>,

    /// Flexible metadata storage (e.g., index_fields, custom attributes)
    pub metadata: HashMap<String, serde_json::Value>,

    /// Type discriminator (e.g., "Entity", "EntityType", "EdgeType")
    #[serde(rename = "type")]
    pub data_type: String,

    /// Dataset this data point belongs to (list of JSON values, matching Python)
    pub belongs_to_set: Option<Vec<serde_json::Value>>,

    /// Pipeline that created this data point
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_pipeline: Option<String>,

    /// Task that created this data point
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_task: Option<String>,

    /// Node set source
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_node_set: Option<String>,

    /// User that triggered creation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_user: Option<String>,

    /// Content hash of the raw `Data` artefact that produced this DataPoint.
    /// Propagates from upstream `Data.content_hash` through every task in
    /// the cognify pipeline, enabling content-addressed lineage queries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_content_hash: Option<String>,

    /// Feedback weight (default 0.5, matching Python)
    #[serde(default = "default_feedback_weight")]
    pub feedback_weight: f64,
}

impl DataPoint {
    /// Create a new DataPoint with default values.
    ///
    /// # Arguments
    /// * `data_type` - Type discriminator (e.g., "Entity", "EntityType")
    /// * `dataset_id` - Optional dataset UUID
    pub fn new(data_type: impl Into<String>, dataset_id: Option<Uuid>) -> Self {
        let now = Utc::now().timestamp_millis();
        Self {
            id: Uuid::new_v4(),
            created_at: now,
            updated_at: now,
            ontology_valid: false,
            version: 1,
            topological_rank: None,
            metadata: HashMap::new(),
            data_type: data_type.into(),
            belongs_to_set: dataset_id.map(|id| vec![serde_json::json!(id.to_string())]),
            source_pipeline: None,
            source_task: None,
            source_node_set: None,
            source_user: None,
            source_content_hash: None,
            feedback_weight: 0.5,
        }
    }

    /// Create a DataPoint with specific metadata.
    pub fn with_metadata(
        data_type: impl Into<String>,
        dataset_id: Option<Uuid>,
        metadata: HashMap<String, serde_json::Value>,
    ) -> Self {
        let now = Utc::now().timestamp_millis();
        Self {
            id: Uuid::new_v4(),
            created_at: now,
            updated_at: now,
            ontology_valid: false,
            version: 1,
            topological_rank: None,
            metadata,
            data_type: data_type.into(),
            belongs_to_set: dataset_id.map(|id| vec![serde_json::json!(id.to_string())]),
            source_pipeline: None,
            source_task: None,
            source_node_set: None,
            source_user: None,
            source_content_hash: None,
            feedback_weight: 0.5,
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

    /// Canonical vector-store payload keys for this DataPoint.
    ///
    /// Mirrors Python's `DataPoint.model_dump()` payload shape: every
    /// pydantic-equivalent field flows into the metadata map. Keys with
    /// `None` values are omitted (consistent with the
    /// `skip_serializing_if = "Option::is_none"` annotations on the
    /// struct).
    ///
    /// Used by the cognify and memify pipelines when constructing
    /// `VectorPoint` payloads to keep the Rust shape byte-comparable to
    /// Python's for the cross-SDK parity tests. Note: the `data_type`
    /// field carries `#[serde(rename = "type")]`, so the resulting map
    /// uses the JSON key `"type"` (matching Python).
    pub fn vector_metadata(&self) -> HashMap<String, serde_json::Value> {
        match serde_json::to_value(self) {
            Ok(serde_json::Value::Object(map)) => map.into_iter().collect(),
            _ => HashMap::new(),
        }
    }

    /// Update the timestamp to current time.
    pub fn touch(&mut self) {
        self.updated_at = Utc::now().timestamp_millis();
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
    use serde_json::json;

    #[test]
    fn test_data_point_creation() {
        let dp = DataPoint::new("TestType", None);
        assert_eq!(dp.data_type, "TestType");
        assert_eq!(dp.version, 1);
        assert!(!dp.ontology_valid);
        assert!(dp.metadata.is_empty());
        assert!(dp.belongs_to_set.is_none());
        assert!(dp.source_pipeline.is_none());
        assert!(dp.source_task.is_none());
        assert!(dp.source_node_set.is_none());
        assert!(dp.source_user.is_none());
        assert!(dp.source_content_hash.is_none());
        assert!((dp.feedback_weight - 0.5).abs() < f64::EPSILON);
        assert!(dp.created_at > 0);
        assert!(dp.updated_at > 0);
    }

    #[test]
    fn test_data_point_with_dataset() {
        let dataset_id = Uuid::new_v4();
        let dp = DataPoint::new("Entity", Some(dataset_id));
        assert_eq!(
            dp.belongs_to_set,
            Some(vec![serde_json::json!(dataset_id.to_string())])
        );
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
    fn source_content_hash_round_trips_when_set_and_omitted_when_none() {
        let mut dp = DataPoint::new("Entity", None);
        assert!(
            !serde_json::to_string(&dp)
                .unwrap()
                .contains("source_content_hash"),
            "absent field must be skipped by serde"
        );

        dp.source_content_hash = Some("md5:abcdef".to_string());
        let json = serde_json::to_string(&dp).unwrap();
        assert!(json.contains(r#""source_content_hash":"md5:abcdef""#));

        let parsed: DataPoint = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.source_content_hash.as_deref(), Some("md5:abcdef"));
    }

    #[test]
    fn vector_metadata_includes_all_set_source_fields() {
        let mut dp = DataPoint::new("Entity", None);
        dp.source_pipeline = Some("cognify_pipeline".into());
        dp.source_task = Some("classify_documents".into());
        dp.source_user = Some("alice@example.com".into());
        dp.source_node_set = Some("entity_nodes".into());
        dp.source_content_hash = Some("md5:abcdef".into());

        let m = dp.vector_metadata();
        assert_eq!(
            m.get("source_pipeline").unwrap(),
            &json!("cognify_pipeline")
        );
        assert_eq!(m.get("source_task").unwrap(), &json!("classify_documents"));
        assert_eq!(m.get("source_user").unwrap(), &json!("alice@example.com"));
        assert_eq!(m.get("source_node_set").unwrap(), &json!("entity_nodes"));
        assert_eq!(m.get("source_content_hash").unwrap(), &json!("md5:abcdef"));
        // `data_type` round-trips as the JSON key `"type"` because of
        // `#[serde(rename = "type")]` on the struct field.
        assert_eq!(m.get("type").unwrap(), &json!("Entity"));
        assert_eq!(m.get("version").unwrap(), &json!(1));
        assert!(m.contains_key("created_at"));
        assert!(m.contains_key("updated_at"));
    }

    #[test]
    fn vector_metadata_omits_none_source_fields() {
        let dp = DataPoint::new("Entity", None);
        let m = dp.vector_metadata();
        assert!(!m.contains_key("source_pipeline"));
        assert!(!m.contains_key("source_task"));
        assert!(!m.contains_key("source_user"));
        assert!(!m.contains_key("source_node_set"));
        assert!(!m.contains_key("source_content_hash"));
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
