//! EdgeMetadata - Metadata for relationships between DataPoints.
//!
//! Mirrors Python's `cognee/infrastructure/engine/models/Edge.py`.
//! Represents edge properties like weight, relationship type, and edge text
//! for relationships between DataPoints.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Edge metadata for relationships between DataPoints.
///
/// Matches the Python `Edge(BaseModel)` class. Supports single weight,
/// multiple named weights, arbitrary properties, and an `edge_text` field
/// that is auto-populated from `relationship_type` when not explicitly set
/// (mirroring Python's `field_validator("edge_text")` behavior).
///
/// # Examples
///
/// ```
/// use cognee_models::EdgeMetadata;
///
/// // Auto-populates edge_text from relationship_type
/// let edge = EdgeMetadata::new(Some("contains".into()), None, None);
/// assert_eq!(edge.edge_text.as_deref(), Some("contains"));
///
/// // Explicit edge_text takes priority
/// let edge = EdgeMetadata::new(
///     Some("contains".into()),
///     Some(0.5),
///     Some("relationship_name: contains; entity: Alice".into()),
/// );
/// assert_eq!(edge.edge_text.as_deref(), Some("relationship_name: contains; entity: Alice"));
/// ```
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct EdgeMetadata {
    /// Relationship type name (e.g., "works_at", "contains").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relationship_type: Option<String>,

    /// Single weight value (backward compatible with Python's `weight` field).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight: Option<f64>,

    /// Multiple named weights (e.g., `{"strength": 0.8, "confidence": 0.9}`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weights: Option<HashMap<String, f64>>,

    /// Arbitrary edge properties (flexible key-value storage).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<HashMap<String, serde_json::Value>>,

    /// Text representation for embedding. Auto-populated from `relationship_type`
    /// if not explicitly set (matches Python's `field_validator` behavior).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge_text: Option<String>,
}

impl EdgeMetadata {
    /// Create a new EdgeMetadata, auto-populating `edge_text` from `relationship_type`
    /// if `edge_text` is not provided (matches Python's `field_validator` behavior).
    pub fn new(
        relationship_type: Option<String>,
        weight: Option<f64>,
        edge_text: Option<String>,
    ) -> Self {
        let effective_edge_text = edge_text.or_else(|| relationship_type.clone());
        Self {
            relationship_type,
            weight,
            weights: None,
            properties: None,
            edge_text: effective_edge_text,
        }
    }

    /// Create an EdgeMetadata with all fields specified.
    pub fn with_all(
        relationship_type: Option<String>,
        weight: Option<f64>,
        weights: Option<HashMap<String, f64>>,
        properties: Option<HashMap<String, serde_json::Value>>,
        edge_text: Option<String>,
    ) -> Self {
        let effective_edge_text = edge_text.or_else(|| relationship_type.clone());
        Self {
            relationship_type,
            weight,
            weights,
            properties,
            edge_text: effective_edge_text,
        }
    }

    /// Auto-populate `edge_text` from `relationship_type` if `edge_text` is `None`.
    ///
    /// This mirrors Python's `field_validator("edge_text")` which sets
    /// `edge_text = relationship_type` when `edge_text` is not provided.
    pub fn ensure_edge_text(&mut self) {
        if self.edge_text.is_none() {
            self.edge_text.clone_from(&self.relationship_type);
        }
    }
}

/// Custom `Deserialize` implementation that auto-populates `edge_text` from
/// `relationship_type` after deserialization, matching Python's `field_validator`
/// behavior. This ensures that JSON like `{"relationship_type": "contains"}`
/// will produce `edge_text == Some("contains")`.
impl<'de> Deserialize<'de> for EdgeMetadata {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        /// Helper struct with standard derive for deserialization.
        #[derive(Deserialize)]
        struct EdgeMetadataRaw {
            relationship_type: Option<String>,
            weight: Option<f64>,
            weights: Option<HashMap<String, f64>>,
            properties: Option<HashMap<String, serde_json::Value>>,
            edge_text: Option<String>,
        }

        let raw = EdgeMetadataRaw::deserialize(deserializer)?;

        // Auto-populate edge_text from relationship_type (Python field_validator)
        let effective_edge_text = raw.edge_text.or_else(|| raw.relationship_type.clone());

        Ok(EdgeMetadata {
            relationship_type: raw.relationship_type,
            weight: raw.weight,
            weights: raw.weights,
            properties: raw.properties,
            edge_text: effective_edge_text,
        })
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
    fn test_new_auto_populates_edge_text() {
        let edge = EdgeMetadata::new(Some("contains".into()), None, None);
        assert_eq!(edge.relationship_type.as_deref(), Some("contains"));
        assert_eq!(edge.edge_text.as_deref(), Some("contains"));
    }

    #[test]
    fn test_new_explicit_edge_text_preserved() {
        let edge = EdgeMetadata::new(
            Some("contains".into()),
            Some(0.5),
            Some("custom text".into()),
        );
        assert_eq!(edge.relationship_type.as_deref(), Some("contains"));
        assert_eq!(edge.weight, Some(0.5));
        assert_eq!(edge.edge_text.as_deref(), Some("custom text"));
    }

    #[test]
    fn test_new_no_relationship_type_no_edge_text() {
        let edge = EdgeMetadata::new(None, Some(1.0), None);
        assert_eq!(edge.relationship_type, None);
        assert_eq!(edge.edge_text, None);
        assert_eq!(edge.weight, Some(1.0));
    }

    #[test]
    fn test_ensure_edge_text_populates_when_none() {
        let mut edge = EdgeMetadata {
            relationship_type: Some("works_at".into()),
            edge_text: None,
            ..Default::default()
        };
        edge.ensure_edge_text();
        assert_eq!(edge.edge_text.as_deref(), Some("works_at"));
    }

    #[test]
    fn test_ensure_edge_text_preserves_existing() {
        let mut edge = EdgeMetadata {
            relationship_type: Some("works_at".into()),
            edge_text: Some("already set".into()),
            ..Default::default()
        };
        edge.ensure_edge_text();
        assert_eq!(edge.edge_text.as_deref(), Some("already set"));
    }

    #[test]
    fn test_ensure_edge_text_both_none() {
        let mut edge = EdgeMetadata::default();
        edge.ensure_edge_text();
        assert_eq!(edge.edge_text, None);
    }

    #[test]
    fn test_default_all_none() {
        let edge = EdgeMetadata::default();
        assert_eq!(edge.relationship_type, None);
        assert_eq!(edge.weight, None);
        assert_eq!(edge.weights, None);
        assert_eq!(edge.properties, None);
        assert_eq!(edge.edge_text, None);
    }

    #[test]
    fn test_with_all_fields() {
        let mut weights = HashMap::new();
        weights.insert("strength".into(), 0.8);
        weights.insert("confidence".into(), 0.9);

        let mut properties = HashMap::new();
        properties.insert("source".into(), json!("manual"));

        let edge = EdgeMetadata::with_all(
            Some("contains".into()),
            Some(0.5),
            Some(weights.clone()),
            Some(properties.clone()),
            Some("custom text".into()),
        );

        assert_eq!(edge.relationship_type.as_deref(), Some("contains"));
        assert_eq!(edge.weight, Some(0.5));
        assert_eq!(edge.weights.as_ref().unwrap().get("strength"), Some(&0.8));
        assert_eq!(edge.weights.as_ref().unwrap().get("confidence"), Some(&0.9));
        assert_eq!(
            edge.properties.as_ref().unwrap().get("source"),
            Some(&json!("manual"))
        );
        assert_eq!(edge.edge_text.as_deref(), Some("custom text"));
    }

    #[test]
    fn test_with_all_auto_populates_edge_text() {
        let edge = EdgeMetadata::with_all(
            Some("located_in".into()),
            None,
            None,
            None,
            None, // edge_text not provided
        );
        assert_eq!(edge.edge_text.as_deref(), Some("located_in"));
    }

    #[test]
    fn test_serialization_roundtrip() {
        let edge = EdgeMetadata::new(Some("works_at".into()), Some(0.75), None);
        let json = serde_json::to_string(&edge).unwrap();
        let deserialized: EdgeMetadata = serde_json::from_str(&json).unwrap();

        assert_eq!(edge, deserialized);
    }

    #[test]
    fn test_serialization_skips_none_fields() {
        let edge = EdgeMetadata::new(Some("works_at".into()), None, None);
        let json_value: serde_json::Value = serde_json::to_value(&edge).unwrap();
        let obj = json_value.as_object().unwrap();

        assert!(obj.contains_key("relationship_type"));
        assert!(obj.contains_key("edge_text"));
        assert!(!obj.contains_key("weight"));
        assert!(!obj.contains_key("weights"));
        assert!(!obj.contains_key("properties"));
    }

    #[test]
    fn test_deserialize_auto_populates_edge_text() {
        // Deserializing JSON without edge_text should auto-populate from relationship_type
        let json = r#"{"relationship_type": "contains"}"#;
        let edge: EdgeMetadata = serde_json::from_str(json).unwrap();

        assert_eq!(edge.relationship_type.as_deref(), Some("contains"));
        assert_eq!(edge.edge_text.as_deref(), Some("contains"));
    }

    #[test]
    fn test_deserialize_explicit_edge_text_preserved() {
        let json = r#"{"relationship_type": "contains", "edge_text": "custom"}"#;
        let edge: EdgeMetadata = serde_json::from_str(json).unwrap();

        assert_eq!(edge.relationship_type.as_deref(), Some("contains"));
        assert_eq!(edge.edge_text.as_deref(), Some("custom"));
    }

    #[test]
    fn test_deserialize_empty_json() {
        let json = r#"{}"#;
        let edge: EdgeMetadata = serde_json::from_str(json).unwrap();

        assert_eq!(edge, EdgeMetadata::default());
    }

    #[test]
    fn test_deserialize_with_weights() {
        let json = r#"{
            "relationship_type": "contains",
            "weight": 0.5,
            "weights": {"strength": 0.8, "confidence": 0.9}
        }"#;
        let edge: EdgeMetadata = serde_json::from_str(json).unwrap();

        assert_eq!(edge.weight, Some(0.5));
        let weights = edge.weights.as_ref().unwrap();
        assert_eq!(weights.get("strength"), Some(&0.8));
        assert_eq!(weights.get("confidence"), Some(&0.9));
        // edge_text auto-populated from relationship_type
        assert_eq!(edge.edge_text.as_deref(), Some("contains"));
    }

    #[test]
    fn test_deserialize_with_properties() {
        let json = r#"{
            "relationship_type": "works_at",
            "properties": {"since": "2020", "role": "engineer", "active": true}
        }"#;
        let edge: EdgeMetadata = serde_json::from_str(json).unwrap();

        let props = edge.properties.as_ref().unwrap();
        assert_eq!(props.get("since"), Some(&json!("2020")));
        assert_eq!(props.get("role"), Some(&json!("engineer")));
        assert_eq!(props.get("active"), Some(&json!(true)));
    }

    #[test]
    fn test_clone() {
        let edge = EdgeMetadata::new(Some("contains".into()), Some(0.5), None);
        let cloned = edge.clone();
        assert_eq!(edge, cloned);
    }

    #[test]
    fn test_debug_format() {
        let edge = EdgeMetadata::new(Some("contains".into()), None, None);
        let debug = format!("{edge:?}");
        assert!(debug.contains("EdgeMetadata"));
        assert!(debug.contains("contains"));
    }
}
