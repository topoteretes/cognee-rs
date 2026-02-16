//! EntityType - Storage-layer entity type model.
//!
//! Mirrors Python's `cognee/modules/engine/models/EntityType.py`
//! Represents a category/type of entities (e.g., "Organization", "Person", "Location").

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::DataPoint;

/// Storage-layer entity type model.
///
/// Represents a category of entities (e.g., "Organization", "Person", "Location").
/// Entity instances reference their EntityType via the `is_a` field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EntityType {
    /// Base data point fields (id, timestamps, metadata, etc.)
    #[serde(flatten)]
    pub base: DataPoint,

    /// Type name (e.g., "Organization", "Person", "Location")
    pub name: String,

    /// Type description
    pub description: String,
}

impl EntityType {
    /// Index fields to embed for vector search.
    pub const INDEX_FIELDS: &'static [&'static str] = &["name"];

    /// Create a new EntityType.
    ///
    /// # Arguments
    /// * `name` - Type name (e.g., "Organization")
    /// * `description` - Type description
    /// * `dataset_id` - Dataset UUID
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        dataset_id: Option<Uuid>,
    ) -> Self {
        let mut metadata = std::collections::HashMap::new();
        metadata.insert(
            "index_fields".to_string(),
            serde_json::json!(Self::INDEX_FIELDS),
        );

        let name_str = name.into();
        let description_str = description.into();

        Self {
            base: DataPoint::with_metadata("EntityType", dataset_id, metadata),
            name: name_str.clone(),
            description: if description_str.is_empty() {
                format!("Entity type: {}", name_str)
            } else {
                description_str
            },
        }
    }

    /// Create EntityType from LLM-extracted node type string.
    ///
    /// # Arguments
    /// * `type_name` - Node type from LLM (e.g., "Organization")
    /// * `dataset_id` - Dataset UUID
    pub fn from_node_type(type_name: impl Into<String>, dataset_id: Option<Uuid>) -> Self {
        let type_str = type_name.into();
        Self::new(
            type_str.clone(),
            format!("Entity type: {}", type_str),
            dataset_id,
        )
    }

    /// Get the type name (for embedding).
    pub fn get_embeddable_text(&self) -> String {
        self.name.clone()
    }

    /// Update type description.
    pub fn set_description(&mut self, description: impl Into<String>) {
        self.description = description.into();
        self.base.touch();
    }

    /// Check if this type has been validated against an ontology.
    pub fn is_ontology_valid(&self) -> bool {
        self.base.ontology_valid
    }

    /// Mark as ontology-validated with canonical name.
    ///
    /// # Arguments
    /// * `canonical_name` - Canonical name from ontology
    pub fn mark_ontology_valid(&mut self, canonical_name: Option<String>) {
        self.base.set_ontology_valid(true);

        if let Some(canonical) = canonical_name
            && canonical != self.name {
                // Store original name in metadata
                self.base
                    .set_metadata("original_name", serde_json::json!(self.name.clone()));
                // Update to canonical name
                self.name = canonical;
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_entity_type_creation() {
        let et = EntityType::new("Organization", "A company or institution", None);

        assert_eq!(et.name, "Organization");
        assert_eq!(et.description, "A company or institution");
        assert_eq!(et.base.data_type, "EntityType");
    }

    #[test]
    fn test_entity_type_empty_description() {
        let et = EntityType::new("Person", "", None);

        assert_eq!(et.name, "Person");
        assert_eq!(et.description, "Entity type: Person");
    }

    #[test]
    fn test_entity_type_from_node_type() {
        let et = EntityType::from_node_type("Location", None);

        assert_eq!(et.name, "Location");
        assert_eq!(et.description, "Entity type: Location");
    }

    #[test]
    fn test_entity_type_index_fields() {
        let et = EntityType::new("Organization", "A company", None);
        let index_fields = et.base.get_metadata("index_fields");

        assert_eq!(index_fields, Some(&serde_json::json!(["name"])));
    }

    #[test]
    fn test_entity_type_embeddable_text() {
        let et = EntityType::new("Organization", "A company", None);
        assert_eq!(et.get_embeddable_text(), "Organization");
    }

    #[test]
    fn test_entity_type_set_description() {
        let mut et = EntityType::new("Organization", "Old desc", None);
        et.set_description("New description");
        assert_eq!(et.description, "New description");
    }

    #[test]
    fn test_ontology_validation() {
        let mut et = EntityType::new("Mathematician", "", None);
        assert!(!et.is_ontology_valid());

        // Mark as valid with canonical name
        et.mark_ontology_valid(Some("Person".to_string()));

        assert!(et.is_ontology_valid());
        assert_eq!(et.name, "Person");
        assert_eq!(
            et.base.get_metadata("original_name"),
            Some(&serde_json::json!("Mathematician"))
        );
    }

    #[test]
    fn test_ontology_validation_same_name() {
        let mut et = EntityType::new("Person", "", None);
        et.mark_ontology_valid(Some("Person".to_string()));

        assert!(et.is_ontology_valid());
        assert_eq!(et.name, "Person");
        assert_eq!(et.base.get_metadata("original_name"), None);
    }

    #[test]
    fn test_ontology_validation_no_canonical() {
        let mut et = EntityType::new("Person", "", None);
        et.mark_ontology_valid(None);

        assert!(et.is_ontology_valid());
        assert_eq!(et.name, "Person");
    }
}
