//! Entity - Storage-layer entity model.
//!
//! Mirrors Python's `cognee/modules/engine/models/Entity.py`
//! Represents an entity extracted from text and stored in the graph database.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::DataPoint;
use crate::has_datapoint::HasDataPoint;

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

    /// Deterministic, class-namespaced id for an Entity identity value.
    ///
    /// Mirrors Python's `Entity.id_for(value)` (`identity_fields=["name"]`):
    /// `uuid5(NAMESPACE_OID, "Entity:<normalized_value>")`. This is the single
    /// source of truth for "what id does the entity with this identity have",
    /// used both when constructing entities and when looking them up from a raw
    /// string before an instance exists.
    pub fn id_for(value: &str) -> Uuid {
        cognee_utils::data_point_id_for("Entity", &[value])
    }

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
        let name = name.into();
        let mut metadata = std::collections::HashMap::new();
        metadata.insert(
            "index_fields".to_string(),
            serde_json::json!(Self::INDEX_FIELDS),
        );

        // Deterministic, class-namespaced id derived from the identity value
        // (`name`), mirroring Python's `identity_fields=["name"]` derivation in
        // `DataPoint.__init__`. Prevents the random-uuid4 footgun that made the
        // same entity duplicate across cognify runs.
        let mut base = DataPoint::with_metadata("Entity", dataset_id, metadata);
        base.id = Self::id_for(&name);

        Self {
            base,
            name,
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
        let node_id = node_id.into();
        let mut entity = Self::new(
            node_name,
            Some(entity_type_id),
            node_description,
            dataset_id,
        );

        // Python `_create_entity_node` hashes the LLM-supplied node id (not the
        // display name) into the id — `Entity(id=Entity.id_for(node_id), …)`
        // (expand_with_nodes_and_edges.py:183,209). Override the name-derived id
        // from `new` with the node-id-derived one for faithful parity.
        entity.base.id = Self::id_for(&node_id);

        entity
            .base
            .set_metadata("original_node_id", serde_json::json!(node_id));

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

impl HasDataPoint for Entity {
    fn data_point(&self) -> &DataPoint {
        &self.base
    }
    fn data_point_mut(&mut self) -> &mut DataPoint {
        &mut self.base
    }
    // for_each_child_mut: default no-op — Entity references its EntityType
    // by UUID (`is_a: Option<Uuid>`), not by ownership. If a future variant
    // owns an `entity_type: Box<EntityType>` field, override here to recurse.
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
    fn test_id_for_matches_python() {
        // Python: Entity.id_for("Alice") = uuid5(OID, "Entity:alice")
        assert_eq!(
            Entity::id_for("Alice"),
            Uuid::new_v5(&Uuid::NAMESPACE_OID, b"Entity:alice"),
        );
    }

    #[test]
    fn test_new_id_is_deterministic_from_name() {
        // Two entities with the same name resolve to the same id — this is what
        // lets the same entity merge across cognify runs (regresses issue #57's
        // inverse: random ids caused silent duplication).
        let a = Entity::new("Acme Corp", None, "desc a", None);
        let b = Entity::new(
            "Acme Corp",
            Some(Uuid::new_v4()),
            "desc b",
            Some(Uuid::new_v4()),
        );
        assert_eq!(a.base.id, b.base.id);
        assert_eq!(a.base.id, Entity::id_for("Acme Corp"));
    }

    #[test]
    fn test_from_node_hashes_node_id_not_name() {
        // Python hashes the LLM node id, not the display name.
        let e = Entity::from_node("node-42", "Alice", "desc", Uuid::new_v4(), None);
        assert_eq!(e.base.id, Entity::id_for("node-42"));
        assert_ne!(e.base.id, Entity::id_for("Alice"));
    }

    #[test]
    fn test_entity_and_entity_type_ids_do_not_collide() {
        // The class prefix keeps a Person "institution" and a type "institution"
        // on distinct ids (topoteretes/cognee#2510/#2515).
        assert_ne!(
            Entity::id_for("institution"),
            crate::EntityType::id_for("institution"),
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
        // updated_at is i64 (millis since epoch); touch() should advance it
        assert!(entity.base.updated_at >= old_time);
    }

    #[test]
    fn test_entity_set_type() {
        let mut entity = Entity::new("TechCorp", None, "A company", None);
        let type_id = Uuid::new_v4();

        entity.set_entity_type(type_id);
        assert_eq!(entity.is_a, Some(type_id));
    }

    #[test]
    fn entity_implements_has_datapoint() {
        let e = Entity::new("Foo", None, "desc", None);
        let dp_id = e.base.id;
        assert_eq!(e.data_point().id, dp_id);
        let mut e2 = e;
        assert_eq!(e2.data_point_mut().id, dp_id);
    }
}
