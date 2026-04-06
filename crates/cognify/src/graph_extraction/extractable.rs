//! GraphExtractable trait and get_graph_from_model function.
//!
//! Mirrors Python's `get_graph_from_model()` which uses runtime reflection
//! to discover DataPoint fields that reference other DataPoints. In Rust we
//! use a trait that each type explicitly implements.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use chrono::Utc;
use cognee_graph::EdgeData;
use cognee_models::{DocumentChunk, Entity, EntityType};
use serde_json::json;
use uuid::Uuid;

use crate::summarization::TextSummary;

// ---------------------------------------------------------------------------
// Trait and Relationship
// ---------------------------------------------------------------------------

/// A directed relationship from a DataPoint to another DataPoint.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Relationship {
    /// The field name that declares this relationship (e.g. "is_part_of", "contains").
    pub field_name: String,
    /// The UUID of the target DataPoint.
    pub target_id: Uuid,
}

/// Declares how a DataPoint type participates in the knowledge graph.
///
/// Each concrete DataPoint struct (DocumentChunk, Entity, TextSummary, etc.)
/// implements this trait to declare its outgoing structural relationships.
///
/// `belongs_to_set` is intentionally excluded — it is a metadata property,
/// not a graph edge.
pub trait GraphExtractable: Send + Sync {
    /// The DataPoint ID of this instance.
    fn data_point_id(&self) -> Uuid;

    /// The DataPoint type name (e.g., "DocumentChunk", "Entity").
    fn data_point_type(&self) -> &str;

    /// Outgoing structural relationships from this instance.
    fn relationships(&self) -> Vec<Relationship>;
}

// ---------------------------------------------------------------------------
// Implementations for built-in types
// ---------------------------------------------------------------------------

impl GraphExtractable for DocumentChunk {
    fn data_point_id(&self) -> Uuid {
        self.base.id
    }

    fn data_point_type(&self) -> &str {
        &self.base.data_type
    }

    fn relationships(&self) -> Vec<Relationship> {
        let mut rels = Vec::new();

        // is_part_of: DocumentChunk → Document
        if let Some(doc_id) = self.is_part_of {
            rels.push(Relationship {
                field_name: "is_part_of".to_string(),
                target_id: doc_id,
            });
        }

        // contains: DocumentChunk → Entity (from chunk.contains populated in graph extraction)
        for entity_ref in &self.contains {
            if let Some(id_str) = entity_ref.as_str()
                && let Ok(id) = Uuid::parse_str(id_str)
            {
                rels.push(Relationship {
                    field_name: "contains".to_string(),
                    target_id: id,
                });
            }
        }

        rels
    }
}

impl GraphExtractable for Entity {
    fn data_point_id(&self) -> Uuid {
        self.base.id
    }

    fn data_point_type(&self) -> &str {
        &self.base.data_type
    }

    fn relationships(&self) -> Vec<Relationship> {
        let mut rels = Vec::new();

        // is_a: Entity → EntityType
        if let Some(type_id) = self.is_a {
            rels.push(Relationship {
                field_name: "is_a".to_string(),
                target_id: type_id,
            });
        }

        rels
    }
}

impl GraphExtractable for EntityType {
    fn data_point_id(&self) -> Uuid {
        self.base.id
    }

    fn data_point_type(&self) -> &str {
        &self.base.data_type
    }

    fn relationships(&self) -> Vec<Relationship> {
        // EntityType has no outgoing DataPoint relationships
        Vec::new()
    }
}

impl GraphExtractable for TextSummary {
    fn data_point_id(&self) -> Uuid {
        self.base.id
    }

    fn data_point_type(&self) -> &str {
        &self.base.data_type
    }

    fn relationships(&self) -> Vec<Relationship> {
        let mut rels = Vec::new();

        // made_from: TextSummary → DocumentChunk
        if let Some(chunk_id) = self.made_from {
            rels.push(Relationship {
                field_name: "made_from".to_string(),
                target_id: chunk_id,
            });
        }

        rels
    }
}

// ---------------------------------------------------------------------------
// get_graph_from_model
// ---------------------------------------------------------------------------

/// Discover all structural edges from a set of graph-extractable items.
///
/// Returns a deduplicated list of [`EdgeData`] tuples, each with an
/// `updated_at` property matching Python's format.
///
/// Port of Python's `get_graph_from_model()` — simplified because our
/// current types don't have nested DataPoint fields that require recursive
/// DFS traversal.
pub fn get_graph_from_model(items: &[&dyn GraphExtractable]) -> Vec<EdgeData> {
    let mut edges: Vec<EdgeData> = Vec::new();
    let mut seen: HashSet<(String, String, String)> = HashSet::new();
    let now = Utc::now().to_rfc3339();

    for item in items {
        for rel in item.relationships() {
            let source = item.data_point_id().to_string();
            let target = rel.target_id.to_string();
            let key = (source.clone(), target.clone(), rel.field_name.clone());

            if seen.insert(key) {
                edges.push((
                    source,
                    target,
                    rel.field_name,
                    HashMap::from([(Cow::from("updated_at"), json!(now.clone()))]),
                ));
            }
        }
    }

    edges
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_document_chunk_relationships() {
        let doc_id = Uuid::new_v4();
        let chunk = DocumentChunk::new(
            Uuid::new_v4(),
            "test text".to_string(),
            2,
            0,
            "paragraph_end".to_string(),
            doc_id,
        );

        let rels = chunk.relationships();
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].field_name, "is_part_of");
        assert_eq!(rels[0].target_id, doc_id);
    }

    #[test]
    fn test_document_chunk_with_contains() {
        let doc_id = Uuid::new_v4();
        let entity_id = Uuid::new_v4();
        let mut chunk = DocumentChunk::new(
            Uuid::new_v4(),
            "test text".to_string(),
            2,
            0,
            "paragraph_end".to_string(),
            doc_id,
        );
        chunk.contains = vec![json!(entity_id.to_string())];

        let rels = chunk.relationships();
        assert_eq!(rels.len(), 2);
        assert_eq!(rels[0].field_name, "is_part_of");
        assert_eq!(rels[1].field_name, "contains");
        assert_eq!(rels[1].target_id, entity_id);
    }

    #[test]
    fn test_entity_relationships() {
        let type_id = Uuid::new_v4();
        let entity = Entity::new("TechCorp", Some(type_id), "A company", None);

        let rels = entity.relationships();
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].field_name, "is_a");
        assert_eq!(rels[0].target_id, type_id);
    }

    #[test]
    fn test_entity_no_type_no_relationships() {
        let entity = Entity::new("TechCorp", None, "A company", None);

        let rels = entity.relationships();
        assert!(rels.is_empty());
    }

    #[test]
    fn test_entity_type_no_relationships() {
        let et = EntityType::new("Organization", "A company type", None);

        let rels = et.relationships();
        assert!(rels.is_empty());
    }

    #[test]
    fn test_text_summary_relationships() {
        let chunk_id = Uuid::new_v4();
        let summary = TextSummary::new(
            chunk_id,
            "Summary text".to_string(),
            None,
            "gpt-4".to_string(),
        );

        let rels = summary.relationships();
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].field_name, "made_from");
        assert_eq!(rels[0].target_id, chunk_id);
    }

    #[test]
    fn test_get_graph_from_model_basic() {
        let doc_id = Uuid::new_v4();
        let chunk = DocumentChunk::new(
            Uuid::new_v4(),
            "test".to_string(),
            1,
            0,
            "paragraph_end".to_string(),
            doc_id,
        );

        let items: Vec<&dyn GraphExtractable> = vec![&chunk];
        let edges = get_graph_from_model(&items);

        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].0, chunk.base.id.to_string());
        assert_eq!(edges[0].1, doc_id.to_string());
        assert_eq!(edges[0].2, "is_part_of");
        assert!(edges[0].3.contains_key(&Cow::from("updated_at")));
    }

    #[test]
    fn test_get_graph_from_model_deduplication() {
        let doc_id = Uuid::new_v4();
        let chunk_id = Uuid::new_v4();
        let chunk = DocumentChunk::new(
            chunk_id,
            "test".to_string(),
            1,
            0,
            "paragraph_end".to_string(),
            doc_id,
        );

        // Pass the same item twice — edges should be deduplicated
        let items: Vec<&dyn GraphExtractable> = vec![&chunk, &chunk];
        let edges = get_graph_from_model(&items);

        assert_eq!(edges.len(), 1);
    }

    #[test]
    fn test_get_graph_from_model_multiple_types() {
        let doc_id = Uuid::new_v4();
        let type_id = Uuid::new_v4();
        let chunk_id = Uuid::new_v4();

        let chunk = DocumentChunk::new(
            chunk_id,
            "test".to_string(),
            1,
            0,
            "paragraph_end".to_string(),
            doc_id,
        );

        let entity = Entity::new("TechCorp", Some(type_id), "A company", None);
        let entity_type = EntityType::new("Organization", "A type", None);

        let summary = TextSummary::new(chunk_id, "Summary".to_string(), None, "gpt-4".to_string());

        let items: Vec<&dyn GraphExtractable> = vec![&chunk, &entity, &entity_type, &summary];
        let edges = get_graph_from_model(&items);

        // chunk: is_part_of → doc_id (1)
        // entity: is_a → type_id (1)
        // entity_type: (0)
        // summary: made_from → chunk_id (1)
        assert_eq!(edges.len(), 3);

        let edge_names: Vec<&str> = edges.iter().map(|e| e.2.as_str()).collect();
        assert!(edge_names.contains(&"is_part_of"));
        assert!(edge_names.contains(&"is_a"));
        assert!(edge_names.contains(&"made_from"));
    }

    #[test]
    fn test_get_graph_from_model_empty() {
        let items: Vec<&dyn GraphExtractable> = vec![];
        let edges = get_graph_from_model(&items);
        assert!(edges.is_empty());
    }

    #[test]
    fn test_get_graph_from_model_contains_edges() {
        let doc_id = Uuid::new_v4();
        let entity_id_1 = Uuid::new_v4();
        let entity_id_2 = Uuid::new_v4();

        let mut chunk = DocumentChunk::new(
            Uuid::new_v4(),
            "test".to_string(),
            1,
            0,
            "paragraph_end".to_string(),
            doc_id,
        );
        chunk.contains = vec![
            json!(entity_id_1.to_string()),
            json!(entity_id_2.to_string()),
        ];

        let items: Vec<&dyn GraphExtractable> = vec![&chunk];
        let edges = get_graph_from_model(&items);

        // is_part_of + 2 contains
        assert_eq!(edges.len(), 3);

        let contains_edges: Vec<_> = edges.iter().filter(|e| e.2 == "contains").collect();
        assert_eq!(contains_edges.len(), 2);
    }

    #[test]
    fn test_relationship_equality() {
        let id = Uuid::new_v4();
        let r1 = Relationship {
            field_name: "is_a".to_string(),
            target_id: id,
        };
        let r2 = Relationship {
            field_name: "is_a".to_string(),
            target_id: id,
        };
        assert_eq!(r1, r2);
    }

    #[test]
    fn test_data_point_type_names() {
        let chunk = DocumentChunk::new(
            Uuid::new_v4(),
            "t".to_string(),
            1,
            0,
            "word".to_string(),
            Uuid::new_v4(),
        );
        assert_eq!(chunk.data_point_type(), "DocumentChunk");

        let entity = Entity::new("Test", None, "desc", None);
        assert_eq!(entity.data_point_type(), "Entity");

        let et = EntityType::new("Type", "desc", None);
        assert_eq!(et.data_point_type(), "EntityType");

        let summary = TextSummary::new(Uuid::new_v4(), "s".to_string(), None, "model".to_string());
        assert_eq!(summary.data_point_type(), "TextSummary");
    }

    #[test]
    fn test_invalid_uuid_in_contains_is_skipped() {
        let doc_id = Uuid::new_v4();
        let mut chunk = DocumentChunk::new(
            Uuid::new_v4(),
            "test".to_string(),
            1,
            0,
            "paragraph_end".to_string(),
            doc_id,
        );
        // Add an invalid UUID string — should be silently skipped
        chunk.contains = vec![json!("not-a-valid-uuid")];

        let rels = chunk.relationships();
        // Only is_part_of, the invalid contains entry is skipped
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].field_name, "is_part_of");
    }

    #[test]
    fn test_non_string_in_contains_is_skipped() {
        let doc_id = Uuid::new_v4();
        let mut chunk = DocumentChunk::new(
            Uuid::new_v4(),
            "test".to_string(),
            1,
            0,
            "paragraph_end".to_string(),
            doc_id,
        );
        // Add a non-string JSON value — should be silently skipped
        chunk.contains = vec![json!(42)];

        let rels = chunk.relationships();
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].field_name, "is_part_of");
    }
}
