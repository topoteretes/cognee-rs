//! EntityType - Storage-layer entity type model.
//!
//! Mirrors Python's `cognee/modules/engine/models/EntityType.py`
//! Represents a category/type of entities (e.g., "Organization", "Person", "Location").

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::DataPoint;
use crate::has_datapoint::HasDataPoint;

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

    /// Deterministic, class-namespaced id for an EntityType identity value.
    ///
    /// Mirrors Python's `EntityType.id_for(value)` (`identity_fields=["name"]`):
    /// `uuid5(NAMESPACE_OID, "EntityType:<normalized_value>")`. The distinct
    /// class prefix is what prevents an `Entity` and an `EntityType` with the
    /// same name from colliding on one id (topoteretes/cognee#2510/#2515).
    pub fn id_for(value: &str) -> Uuid {
        cognee_utils::data_point_id_for("EntityType", &[value])
    }

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

        // Deterministic, class-namespaced id derived from `name`, mirroring
        // Python's `identity_fields=["name"]` derivation.
        let mut base = DataPoint::with_metadata("EntityType", dataset_id, metadata);
        base.id = Self::id_for(&name_str);

        Self {
            base,
            name: name_str.clone(),
            description: if description_str.is_empty() {
                format!("Entity type: {name_str}")
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
            format!("Entity type: {type_str}"),
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
            && canonical != self.name
        {
            self.base
                .set_metadata("original_name", serde_json::json!(self.name.clone()));
            self.name = canonical;
        }
    }
}

impl HasDataPoint for EntityType {
    fn data_point(&self) -> &DataPoint {
        &self.base
    }
    fn data_point_mut(&mut self) -> &mut DataPoint {
        &mut self.base
    }
    // for_each_child_mut: default no-op — EntityType is a leaf in the
    // model graph (no owned `HasDataPoint` children).
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
    fn test_id_for_matches_python() {
        // Python: EntityType.id_for("Organization") = uuid5(OID, "EntityType:organization")
        assert_eq!(
            EntityType::id_for("Organization"),
            Uuid::new_v5(&Uuid::NAMESPACE_OID, b"EntityType:organization"),
        );
    }

    #[test]
    fn test_new_id_is_deterministic_from_name() {
        let a = EntityType::from_node_type("Organization", None);
        let b = EntityType::new(
            "Organization",
            "different description",
            Some(Uuid::new_v4()),
        );
        assert_eq!(a.base.id, b.base.id);
        assert_eq!(a.base.id, EntityType::id_for("Organization"));
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

    #[test]
    fn entity_type_implements_has_datapoint() {
        let et = EntityType::new("Org", "desc", None);
        let dp_id = et.base.id;
        assert_eq!(et.data_point().id, dp_id);
        let mut et2 = et;
        assert_eq!(et2.data_point_mut().id, dp_id);
    }
}
