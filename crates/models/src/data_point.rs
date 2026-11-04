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

/// Default value for `importance_weight` (used by serde).
///
/// Mirrors Python's `DataPoint.importance_weight: float | None = 0.5`.
/// Using a named serde default (rather than bare `#[serde(default)]`)
/// ensures old payloads missing the key deserialize to `Some(0.5)` — the
/// neutral rank — rather than `None`.
fn default_importance_weight() -> Option<f64> {
    Some(0.5)
}

/// Default value for `version` (used by serde).
fn default_version() -> i32 {
    1
}

/// Default value for `topological_rank` (used by serde).
///
/// Python parity: `DataPoint.topological_rank: int | None = 0`
/// (`cognee/infrastructure/engine/models/DataPoint.py:65`) — `0` is the
/// "unset" sentinel, not a real rank. A named serde default (rather than
/// bare `#[serde(default)]`) means a stored `properties` blob missing the
/// key deserializes to `Some(0)` instead of failing / becoming `None`.
fn default_topological_rank() -> Option<i32> {
    Some(0)
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

    /// Topological rank for graph traversal optimization: the 1-based
    /// position of the pipeline task that emitted this DataPoint.
    ///
    /// Written by the pipeline executor (see
    /// `cognee_core::provenance::stamp_tree`) only when still unset —
    /// `None` or `Some(0)` — mirroring Python's
    /// `run_tasks_base.py::_stamp_provenance` guard
    /// (`if current_rank is None or current_rank == 0`).
    ///
    /// Emitted unconditionally (no `skip_serializing_if`): Python always
    /// writes the key, and the visualization JS reads it off every node.
    #[serde(default = "default_topological_rank")]
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

    /// Importance weight (default `Some(0.5)`, matching Python
    /// `importance_weight: float | None = 0.5`). Propagated from the source
    /// `Data` record through classify → chunk → summarize. Emitted
    /// unconditionally (no `skip_serializing_if`) since it is effectively
    /// always present.
    #[serde(default = "default_importance_weight")]
    pub importance_weight: Option<f64>,
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
            topological_rank: Some(0),
            metadata: HashMap::new(),
            data_type: data_type.into(),
            belongs_to_set: dataset_id.map(|id| vec![serde_json::json!(id.to_string())]),
            source_pipeline: None,
            source_task: None,
            source_node_set: None,
            source_user: None,
            source_content_hash: None,
            feedback_weight: 0.5,
            importance_weight: Some(0.5),
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
            topological_rank: Some(0),
            metadata,
            data_type: data_type.into(),
            belongs_to_set: dataset_id.map(|id| vec![serde_json::json!(id.to_string())]),
            source_pipeline: None,
            source_task: None,
            source_node_set: None,
            source_user: None,
            source_content_hash: None,
            feedback_weight: 0.5,
            importance_weight: Some(0.5),
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

    /// Serialize to a JSON string.
    ///
    /// Mirrors Python's `DataPoint.to_json() -> str` (`model_dump_json()`);
    /// [`DataPoint::to_json`] returns the same document as a `Value`.
    pub fn to_json_string(&self) -> String {
        self.to_json().to_string()
    }

    /// Deserialize from a JSON string, applying the serde defaults of
    /// fields the document omits.
    ///
    /// Mirrors Python's `DataPoint.from_json` (`model_validate_json`).
    pub fn from_json(json: &str) -> serde_json::Result<Self> {
        serde_json::from_str(json)
    }

    /// Convert to a field-name-keyed map (`data_type` under the key `"type"`).
    ///
    /// Mirrors Python's `DataPoint.to_dict()` (`model_dump()`).
    pub fn to_dict(&self) -> serde_json::Map<String, serde_json::Value> {
        match self.to_json() {
            serde_json::Value::Object(map) => map,
            _ => serde_json::Map::new(),
        }
    }

    /// Build a DataPoint from a field-name-keyed map, as produced by
    /// [`DataPoint::to_dict`].
    ///
    /// Mirrors Python's `DataPoint.from_dict` (`model_validate`).
    pub fn from_dict(data: serde_json::Map<String, serde_json::Value>) -> serde_json::Result<Self> {
        serde_json::from_value(serde_json::Value::Object(data))
    }

    /// Increment `version` and refresh `updated_at` to now.
    ///
    /// Mirrors Python's `DataPoint.update_version`.
    pub fn update_version(&mut self) {
        self.version += 1;
        self.touch();
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
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
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
        assert_eq!(dp.importance_weight, Some(0.5));
        assert!(dp.created_at > 0);
        assert!(dp.updated_at > 0);
    }

    #[test]
    fn with_metadata_initializes_importance_weight_default() {
        let dp = DataPoint::with_metadata("Entity", None, HashMap::new());
        assert_eq!(dp.importance_weight, Some(0.5));
    }

    #[test]
    fn test_data_point_importance_weight_default_on_missing_field() {
        // Simulate an old stored payload lacking the `importance_weight` key.
        let json = json!({
            "id": Uuid::new_v4().to_string(),
            "created_at": 1_i64,
            "updated_at": 1_i64,
            "ontology_valid": false,
            "metadata": {},
            "type": "Entity",
            "belongs_to_set": null,
        });
        let dp: DataPoint = serde_json::from_value(json).unwrap();
        assert_eq!(dp.importance_weight, Some(0.5));
    }

    #[test]
    fn importance_weight_round_trips_with_expected_key() {
        let mut dp = DataPoint::new("Entity", None);
        dp.importance_weight = Some(0.9);
        let json = serde_json::to_string(&dp).unwrap();
        assert!(json.contains(r#""importance_weight":0.9"#));

        let parsed: DataPoint = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.importance_weight, Some(0.9));
    }

    #[test]
    fn topological_rank_defaults_to_zero_sentinel_on_construction() {
        // Python parity: `DataPoint.topological_rank: int | None = 0`.
        assert_eq!(DataPoint::new("Entity", None).topological_rank, Some(0));
        assert_eq!(
            DataPoint::with_metadata("Entity", None, HashMap::new()).topological_rank,
            Some(0)
        );
    }

    #[test]
    fn topological_rank_default_on_missing_field() {
        // Simulate a persisted `properties` blob written before the field
        // existed: deserialization must succeed and yield the `0` sentinel
        // rather than erroring on a missing required field.
        let json = json!({
            "id": Uuid::new_v4().to_string(),
            "created_at": 1_i64,
            "updated_at": 1_i64,
            "ontology_valid": false,
            "metadata": {},
            "type": "Entity",
            "belongs_to_set": null,
        });
        let dp: DataPoint = serde_json::from_value(json).unwrap();
        assert_eq!(dp.topological_rank, Some(0));
    }

    #[test]
    fn topological_rank_serializes_as_zero_not_null() {
        let dp = DataPoint::new("Entity", None);
        let json = serde_json::to_string(&dp).unwrap();
        assert!(
            json.contains(r#""topological_rank":0"#),
            "fresh DataPoint must emit the `0` sentinel, not null: {json}"
        );

        let mut ranked = DataPoint::new("Entity", None);
        ranked.topological_rank = Some(3);
        let json = serde_json::to_string(&ranked).unwrap();
        assert!(json.contains(r#""topological_rank":3"#));
        let parsed: DataPoint = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.topological_rank, Some(3));
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
    fn test_touch_updates_timestamp() {
        let mut dp = DataPoint::new("Entity", None);
        let original_time = dp.updated_at;

        std::thread::sleep(std::time::Duration::from_millis(10));
        dp.touch();

        assert!(dp.updated_at > original_time);
    }

    fn populated_data_point() -> DataPoint {
        let mut dp = DataPoint::new("Entity", Some(Uuid::new_v4()));
        dp.set_metadata("index_fields", json!(["name"]));
        dp.source_pipeline = Some("cognify_pipeline".into());
        dp.source_content_hash = Some("md5:abcdef".into());
        dp.topological_rank = Some(2);
        dp.importance_weight = None;
        dp
    }

    #[test]
    fn update_version_bumps_version_and_refreshes_updated_at() {
        let mut dp = DataPoint::new("Entity", None);
        let original_time = dp.updated_at;

        std::thread::sleep(std::time::Duration::from_millis(10));
        dp.update_version();
        assert_eq!(dp.version, 2);
        assert!(dp.updated_at > original_time);

        dp.update_version();
        assert_eq!(dp.version, 3);
    }

    #[test]
    fn json_string_round_trips() {
        let dp = populated_data_point();
        let text = dp.to_json_string();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            dp.to_json()
        );
        assert_eq!(DataPoint::from_json(&text).unwrap(), dp);
    }

    #[test]
    fn dict_round_trips_with_python_field_names() {
        let dp = populated_data_point();
        let dict = dp.to_dict();
        assert_eq!(dict.get("type"), Some(&json!("Entity")));
        assert!(!dict.contains_key("data_type"));
        assert_eq!(dict.get("importance_weight"), Some(&json!(null)));
        assert!(
            !dict.contains_key("source_task"),
            "None source fields are skipped"
        );
        assert_eq!(DataPoint::from_dict(dict).unwrap(), dp);
    }

    #[test]
    fn from_dict_applies_defaults_for_omitted_fields() {
        let dict = json!({
            "id": Uuid::new_v4().to_string(),
            "created_at": 1_i64,
            "updated_at": 1_i64,
            "ontology_valid": false,
            "metadata": {},
            "type": "Entity",
            "belongs_to_set": null,
        });
        let serde_json::Value::Object(dict) = dict else {
            unreachable!("literal is an object")
        };
        let dp = DataPoint::from_dict(dict).unwrap();
        assert_eq!(dp.version, 1);
        assert_eq!(dp.topological_rank, Some(0));
        assert_eq!(dp.importance_weight, Some(0.5));
    }

    #[test]
    fn from_json_and_from_dict_reject_invalid_input() {
        assert!(DataPoint::from_json("not json").is_err());
        assert!(DataPoint::from_json(r#"{"type": "Entity"}"#).is_err());
        let mut dict = DataPoint::new("Entity", None).to_dict();
        dict.insert("version".into(), json!("one"));
        assert!(DataPoint::from_dict(dict).is_err());
    }
}
