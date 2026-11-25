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

    /// Reference to EntityType UUID (e.g., UUID of "Organization" type).
    /// The primary type: the first one declared for this entity.
    pub is_a: Option<Uuid>,

    /// Further EntityType UUIDs, in the order they were first declared, never
    /// repeating [`Self::is_a`]. Python keeps the first type per document and
    /// combines the types of every document that mentions an entity, so one
    /// entity can carry several `is_a` edges; this holds every type after the
    /// first, for [`Self::type_ids`] and the `is_a` edges it becomes.
    ///
    /// Never serialised (SDK-1694 O6), so it is in no graph node, ledger row,
    /// vector payload or COGX export: Python's `Entity` has no such field, and
    /// a stored copy went stale as soon as a later run added a type or a
    /// delete removed one, while the `is_a` edges stay right. A payload
    /// written while it was stored still deserialises, the field included.
    #[serde(default, skip_serializing)]
    pub extra_types: Vec<Uuid>,

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
            extra_types: Vec::new(),
            description: description.into(),
        }
    }

    /// Create Entity from LLM-extracted Node, with an id derived from the
    /// LLM's **node id**.
    ///
    /// The cognify pipeline no longer uses this: it derives entity ids from the
    /// node *name*, as current Python does (see the note in the body).
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

        // Node-id-derived id: what older Python (`_create_entity_node`) did.
        // Current Python derives the id from the *name* instead, with ordinal
        // ids for same-name nodes in one graph, and so does the cognify
        // expansion (`entity_ids_by_node_id` in cognee-cognify); it no longer
        // calls this constructor.
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
        self.extra_types.retain(|id| *id != entity_type_id);
        self.base.touch();
    }

    /// Every type of this entity: [`Self::is_a`] first, then
    /// [`Self::extra_types`].
    pub fn type_ids(&self) -> impl Iterator<Item = Uuid> + '_ {
        self.is_a
            .into_iter()
            .chain(self.extra_types.iter().copied())
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
#[allow(
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
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
    fn extra_types_are_left_out_of_the_serialised_form_when_empty() {
        let entity = Entity::new("TechCorp", Some(Uuid::new_v4()), "A company", None);
        let value = serde_json::to_value(&entity).expect("an entity serialises");
        assert!(value.get("extra_types").is_none(), "{value}");

        // A payload written before the field existed still deserialises.
        let back: Entity = serde_json::from_value(value).expect("and deserialises");
        assert!(back.extra_types.is_empty());
    }

    /// SDK-1694 O6: the extra types are never stored — the `is_a` edges are
    /// the entity's type set — while a payload that stored them still reads.
    #[test]
    fn extra_types_are_never_serialised_but_still_read() {
        let (company, fruit) = (Uuid::new_v4(), Uuid::new_v4());
        let mut entity = Entity::new("Apple", Some(company), "", None);
        entity.extra_types.push(fruit);
        assert_eq!(entity.type_ids().collect::<Vec<_>>(), [company, fruit]);

        let mut value = serde_json::to_value(&entity).expect("an entity serialises");
        assert!(value.get("extra_types").is_none(), "{value}");
        assert_eq!(value["is_a"], serde_json::json!(company));

        value["extra_types"] = serde_json::json!([fruit]);
        let back: Entity = serde_json::from_value(value).expect("an older payload deserialises");
        assert_eq!(back.type_ids().collect::<Vec<_>>(), [company, fruit]);
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
