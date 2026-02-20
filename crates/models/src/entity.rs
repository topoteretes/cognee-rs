//! Entity - Storage-layer entity model.
//!
//! Mirrors Python's `cognee/modules/engine/models/Entity.py`
//! Represents an entity extracted from text and stored in the graph database.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::DataPoint;

/// Storage-layer entity model.
///
/// Represents an entity (e.g., "TechCorp", "Alice", "London") extracted
/// from text. Each entity has a name, description, and a reference to its
/// EntityType (e.g., "Organization", "Person", "Location").
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entity {
    /// Base data point fields (id, timestamps, metadata, etc.)
    #[serde(flatten)]
    pub base: DataPoint,

    /// Entity name (e.g., "TechCorp")
    pub name: String,

    /// Reference to EntityType UUID (e.g., UUID of "Organization" type)
    pub is_a: Option<Uuid>,

    /// Entity description from LLM extraction
    pub description: String,
}

impl Entity {
    /// Index fields to embed for vector search.
    pub const INDEX_FIELDS: &'static [&'static str] = &["name"];

    /// Create a new Entity.
    ///
    /// # Arguments
    /// * `name` - Entity name
    /// * `entity_type_id` - Optional reference to EntityType
    /// * `description` - Entity description
    /// * `dataset_id` - Dataset UUID
    pub fn new(
        name: impl Into<String>,
        entity_type_id: Option<Uuid>,
        description: impl Into<String>,
        dataset_id: Option<Uuid>,
    ) -> Self {
        let mut metadata = std::collections::HashMap::new();
        metadata.insert(
            "index_fields".to_string(),
            serde_json::json!(Self::INDEX_FIELDS),
        );

        Self {
            base: DataPoint::with_metadata("Entity", dataset_id, metadata),
            name: name.into(),
            is_a: entity_type_id,
            description: description.into(),
        }
    }

    /// Create Entity from LLM-extracted Node.
    ///
    /// # Arguments
    /// * `node_id` - Original node ID from LLM extraction
    /// * `node_name` - Node name
    /// * `node_description` - Node description
    /// * `entity_type_id` - EntityType UUID
    /// * `dataset_id` - Dataset UUID
    pub fn from_node(
        node_id: impl Into<String>,
        node_name: impl Into<String>,
        node_description: impl Into<String>,
        entity_type_id: Uuid,
        dataset_id: Option<Uuid>,
    ) -> Self {
        let mut entity = Self::new(
            node_name,
            Some(entity_type_id),
            node_description,
            dataset_id,
        );

        // Store original node ID in metadata for reference
        entity
            .base
            .set_metadata("original_node_id", serde_json::json!(node_id.into()));

        entity
    }

    /// Get the entity name (for embedding).
    pub fn get_embeddable_text(&self) -> String {
        self.name.clone()
    }

    /// Update entity description.
    pub fn set_description(&mut self, description: impl Into<String>) {
        self.description = description.into();
        self.base.touch();
    }

    /// Update entity type reference.
    pub fn set_entity_type(&mut self, entity_type_id: Uuid) {
        self.is_a = Some(entity_type_id);
        self.base.touch();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_entity_creation() {
        let entity = Entity::new("TechCorp", None, "A technology company", None);

        assert_eq!(entity.name, "TechCorp");
        assert_eq!(entity.description, "A technology company");
        assert_eq!(entity.base.data_type, "Entity");
        assert!(entity.is_a.is_none());
    }

    #[test]
    fn test_entity_with_type() {
        let type_id = Uuid::new_v4();
        let entity = Entity::new("TechCorp", Some(type_id), "A technology company", None);

        assert_eq!(entity.is_a, Some(type_id));
    }

    #[test]
    fn test_entity_from_node() {
        let type_id = Uuid::new_v4();
        let entity = Entity::from_node(
            "techcorp_1",
            "TechCorp",
            "A technology company",
            type_id,
            None,
        );

        assert_eq!(entity.name, "TechCorp");
        assert_eq!(entity.is_a, Some(type_id));
        assert_eq!(
            entity.base.get_metadata("original_node_id"),
            Some(&serde_json::json!("techcorp_1"))
        );
    }

    #[test]
    fn test_entity_index_fields() {
        let entity = Entity::new("TechCorp", None, "A company", None);
        let index_fields = entity.base.get_metadata("index_fields");

        assert_eq!(index_fields, Some(&serde_json::json!(["name"])));
    }

    #[test]
    fn test_entity_embeddable_text() {
        let entity = Entity::new("TechCorp", None, "A company", None);
        assert_eq!(entity.get_embeddable_text(), "TechCorp");
    }

    #[test]
    fn test_entity_set_description() {
        let mut entity = Entity::new("TechCorp", None, "Old desc", None);
        let old_time = entity.base.updated_at;

        std::thread::sleep(std::time::Duration::from_millis(10));
        entity.set_description("New description");

        assert_eq!(entity.description, "New description");
        assert!(entity.base.updated_at > old_time);
    }

    #[test]
    fn test_entity_set_type() {
        let mut entity = Entity::new("TechCorp", None, "A company", None);
        let type_id = Uuid::new_v4();

        entity.set_entity_type(type_id);
        assert_eq!(entity.is_a, Some(type_id));
    }
}
