//! Triplet data model for relationship-based embeddings.
//!
//! Mirrors Python's `cognee/modules/engine/models/Triplet.py`
//! Triplets represent semantic relationships between entities in a format suitable for embedding.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A triplet representing a semantic relationship between two entities.
///
/// Triplets are embedded as text in the format:
/// "source_text -› relationship_text -› target_text"
///
/// Example: "Steve Jobs: Co-founder of Apple -› founded -› Apple Inc.: Technology company"
///
/// Python reference: cognee/modules/engine/models/Triplet.py
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Triplet {
    /// Unique identifier for this triplet.
    /// Generated as uuid5 from edge key (deterministic).
    pub id: Uuid,

    /// Source entity ID.
    pub source_entity_id: Uuid,

    /// Target entity ID.
    pub target_entity_id: Uuid,

    /// Relationship name (edge type).
    pub relationship_name: String,

    /// Embeddable text representation.
    /// Format: "{source_text} -› {relationship_text} -› {target_text}"
    /// This is the text that gets embedded for semantic search.
    pub embeddable_text: String,

    /// Optional: Source entity name for display/debugging.
    pub source_name: Option<String>,

    /// Optional: Target entity name for display/debugging.
    pub target_name: Option<String>,
}

impl Triplet {
    /// Create a new triplet with deterministic ID.
    ///
    /// The ID is generated using UUID v5 from the edge key, matching Python's behavior.
    ///
    /// # Arguments
    /// * `source_entity_id` - Source entity UUID
    /// * `target_entity_id` - Target entity UUID
    /// * `relationship_name` - Relationship/edge type name
    /// * `embeddable_text` - Formatted text for embedding
    ///
    /// # Example
    /// ```
    /// use cognee_models::Triplet;
    /// use uuid::Uuid;
    ///
    /// let source_id = Uuid::new_v4();
    /// let target_id = Uuid::new_v4();
    /// let triplet = Triplet::new(
    ///     source_id,
    ///     target_id,
    ///     "founded".to_string(),
    ///     "Steve Jobs -› founded -› Apple Inc.".to_string(),
    /// );
    /// ```
    pub fn new(
        source_entity_id: Uuid,
        target_entity_id: Uuid,
        relationship_name: String,
        embeddable_text: String,
    ) -> Self {
        // Generate deterministic ID from edge key (matches Python behavior)
        let edge_key = format!(
            "{}_{}_{}",
            source_entity_id, target_entity_id, relationship_name
        );
        let id = Uuid::new_v5(&Uuid::NAMESPACE_OID, edge_key.as_bytes());

        Self {
            id,
            source_entity_id,
            target_entity_id,
            relationship_name,
            embeddable_text,
            source_name: None,
            target_name: None,
        }
    }

    /// Set source and target names for display purposes.
    ///
    /// # Arguments
    /// * `source_name` - Source entity name
    /// * `target_name` - Target entity name
    pub fn with_names(mut self, source_name: String, target_name: String) -> Self {
        self.source_name = Some(source_name);
        self.target_name = Some(target_name);
        self
    }

    /// Get embeddable text (for consistency with other models).
    pub fn get_embeddable_text(&self) -> &str {
        &self.embeddable_text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_triplet_creation() {
        let source_id = Uuid::new_v4();
        let target_id = Uuid::new_v4();
        let triplet = Triplet::new(
            source_id,
            target_id,
            "founded".to_string(),
            "Steve Jobs -› founded -› Apple Inc.".to_string(),
        );

        assert_eq!(triplet.source_entity_id, source_id);
        assert_eq!(triplet.target_entity_id, target_id);
        assert_eq!(triplet.relationship_name, "founded");
        assert!(triplet.embeddable_text.contains("-›"));
        assert_eq!(triplet.source_name, None);
        assert_eq!(triplet.target_name, None);
    }

    #[test]
    fn test_triplet_with_names() {
        let source_id = Uuid::new_v4();
        let target_id = Uuid::new_v4();
        let triplet = Triplet::new(
            source_id,
            target_id,
            "works_at".to_string(),
            "Alice -› works at -› TechCorp".to_string(),
        )
        .with_names("Alice".to_string(), "TechCorp".to_string());

        assert_eq!(triplet.source_name, Some("Alice".to_string()));
        assert_eq!(triplet.target_name, Some("TechCorp".to_string()));
    }

    #[test]
    fn test_triplet_deterministic_id() {
        // Same inputs should produce same ID (UUID v5)
        let source_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let target_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440001").unwrap();

        let triplet1 = Triplet::new(
            source_id,
            target_id,
            "relates".to_string(),
            "A -› relates -› B".to_string(),
        );

        let triplet2 = Triplet::new(
            source_id,
            target_id,
            "relates".to_string(),
            "A -› relates -› B".to_string(),
        );

        assert_eq!(triplet1.id, triplet2.id, "IDs should be deterministic");
    }

    #[test]
    fn test_triplet_get_embeddable_text() {
        let triplet = Triplet::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            "test".to_string(),
            "test text".to_string(),
        );

        assert_eq!(triplet.get_embeddable_text(), "test text");
    }
}
