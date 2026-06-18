//! Triplet creation from graph nodes and edges.
//!
//! Mirrors Python's _create_triplets_from_graph() in add_data_points.py
//! Creates triplet embeddings from knowledge graph structure.

use cognee_models::Triplet;
use std::collections::HashMap;
use tracing::warn;
use uuid::Uuid;

use crate::graph_integration::{GraphEdgePair, GraphNodePair};

/// Create triplets from graph nodes and edges.
///
/// Each triplet combines:
/// - Source entity (name + description)
/// - Relationship name (or edge_text property)
/// - Target entity (name + description)
///
/// Into embeddable text format:
/// "source_text-›relationship_text-›target_text"
///
/// # Arguments
/// * `nodes` - List of graph nodes (entities with descriptions)
/// * `edges` - List of graph edges (relationships between entities)
///
/// # Returns
/// List of Triplet objects with embeddable text ready for vector indexing.
///
/// # Example
/// ```ignore
/// use cognee_cognify::triplet_creation::create_triplets_from_graph;
///
/// let triplets = create_triplets_from_graph(&entities, &edges);
/// println!("Created {} triplets", triplets.len());
/// ```
pub fn create_triplets_from_graph(
    nodes: &[GraphNodePair],
    edges: &[GraphEdgePair],
) -> Vec<Triplet> {
    // Build node lookup map (id -> node) for O(1) access
    let node_map: HashMap<Uuid, &GraphNodePair> = nodes
        .iter()
        .map(|node| (node.entity.base.id, node))
        .collect();

    let mut triplets = Vec::new();
    let mut skipped_count = 0;

    for edge in edges {
        let source_node = node_map.get(&edge.source_entity_id);
        let target_node = node_map.get(&edge.target_entity_id);

        // Skip if either node is missing (orphaned edge)
        if source_node.is_none() || target_node.is_none() {
            skipped_count += 1;
            continue;
        }

        #[allow(clippy::expect_used, reason = "invariant is upheld by construction")]
        let source_node = source_node
            .expect("source_node is Some; None case was handled by the is_none() check above");
        #[allow(clippy::expect_used, reason = "invariant is upheld by construction")]
        let target_node = target_node
            .expect("target_node is Some; None case was handled by the is_none() check above");

        // Extract embeddable text from source node (name: description)
        let source_text = if !source_node.entity.description.is_empty() {
            format!(
                "{}: {}",
                source_node.entity.name, source_node.entity.description
            )
        } else {
            source_node.entity.name.clone()
        }
        .trim()
        .to_string();

        // Extract embeddable text from target node
        let target_text = if !target_node.entity.description.is_empty() {
            format!(
                "{}: {}",
                target_node.entity.name, target_node.entity.description
            )
        } else {
            target_node.entity.name.clone()
        }
        .trim()
        .to_string();

        // Get relationship text: prefer the nonblank `edge_text` property,
        // falling back to the relationship name. Mirrors Python's
        // `_extract_relationship_text` (get_triplet_datapoints.py:87-96),
        // which treats a blank `edge_text` as absent. The `edge_text` property
        // is always present on LLM-extracted edges now (empty when the edge
        // carried no description), so the blank filter — not just `unwrap_or`
        // — is required to keep the fallback to `relationship_name`.
        let relationship_text = edge
            .properties
            .get("edge_text")
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .unwrap_or(&edge.relationship_name)
            .to_string();

        // Skip if we have no meaningful text to embed
        if source_text.is_empty() && relationship_text.is_empty() && target_text.is_empty() {
            skipped_count += 1;
            continue;
        }

        // Create embeddable text: "source-›relationship-›target"
        // Format matches Python memify get_triplet_datapoints.py:157:
        //   f"{start_node_text}-›{relationship_text}-›{end_node_text}"
        // Kept aligned with memify (no spaces around arrows) since both cognify
        // and memify write into the same "Triplet"/"text" vector collection.
        let text = format!("{source_text}-\u{203a}{relationship_text}-\u{203a}{target_text}");

        let triplet = Triplet::new(
            edge.source_entity_id,
            edge.target_entity_id,
            edge.relationship_name.clone(),
            text,
        )
        .with_names(
            source_node.entity.name.clone(),
            target_node.entity.name.clone(),
        );

        triplets.push(triplet);
    }

    if skipped_count > 0 {
        warn!(
            "⚠  Skipped {} triplets (missing nodes or empty text)",
            skipped_count
        );
    }

    triplets
}

#[cfg(test)]
mod tests {
    use super::*;
    use cognee_models::{DataPoint, Entity, EntityType};

    fn create_test_entity(name: &str, description: &str) -> GraphNodePair {
        let id = Uuid::new_v4();
        let entity = Entity {
            base: DataPoint::new("Entity", None),
            name: name.to_string(),
            is_a: None,
            description: description.to_string(),
        };

        // Override ID
        let mut entity = entity;
        entity.base.id = id;

        let entity_type = EntityType {
            base: DataPoint::new("EntityType", None),
            name: "Generic".to_string(),
            description: "Generic type".to_string(),
        };

        GraphNodePair {
            entity,
            entity_type,
        }
    }

    #[test]
    fn test_triplet_creation_basic() {
        let entity1 = create_test_entity("Steve Jobs", "Co-founder of Apple");
        let entity2 = create_test_entity("Apple Inc.", "Technology company");

        let edge = GraphEdgePair {
            source_entity_id: entity1.entity.base.id,
            target_entity_id: entity2.entity.base.id,
            relationship_name: "founded".to_string(),
            properties: HashMap::new(),
        };

        let triplets = create_triplets_from_graph(&[entity1.clone(), entity2.clone()], &[edge]);

        assert_eq!(triplets.len(), 1);
        let triplet = &triplets[0];
        assert_eq!(triplet.source_entity_id, entity1.entity.base.id);
        assert_eq!(triplet.target_entity_id, entity2.entity.base.id);
        assert_eq!(triplet.relationship_name, "founded");
        assert!(triplet.text.contains("Steve Jobs"));
        assert!(triplet.text.contains("Co-founder of Apple"));
        assert!(triplet.text.contains("founded"));
        assert!(triplet.text.contains("Apple Inc."));
        assert!(triplet.text.contains("Technology company"));
        assert!(triplet.text.contains("-›"));
    }

    #[test]
    fn test_triplet_with_edge_text_property() {
        let entity1 = create_test_entity("Alice", "Software engineer");
        let entity2 = create_test_entity("TechCorp", "Tech company");

        let mut properties = HashMap::new();
        properties.insert("edge_text".to_string(), "works at".to_string());

        let edge = GraphEdgePair {
            source_entity_id: entity1.entity.base.id,
            target_entity_id: entity2.entity.base.id,
            relationship_name: "employed_by".to_string(),
            properties,
        };

        let triplets = create_triplets_from_graph(&[entity1, entity2], &[edge]);

        assert_eq!(triplets.len(), 1);
        // Should use "works at" from edge_text, not "employed_by"
        assert!(triplets[0].text.contains("works at"));
        assert!(!triplets[0].text.contains("employed_by"));
    }

    #[test]
    fn test_triplet_blank_edge_text_falls_back_to_relationship_name() {
        // A blank `edge_text` property (present but empty/whitespace) must fall
        // back to `relationship_name`, mirroring Python's
        // `_extract_relationship_text`. LLM-extracted edges always carry an
        // `edge_text` property now (empty when no description was emitted), so
        // the fallback must survive an empty value.
        let entity1 = create_test_entity("Alice", "Software engineer");
        let entity2 = create_test_entity("TechCorp", "Tech company");

        let mut properties = HashMap::new();
        properties.insert("edge_text".to_string(), "   ".to_string());

        let edge = GraphEdgePair {
            source_entity_id: entity1.entity.base.id,
            target_entity_id: entity2.entity.base.id,
            relationship_name: "employed_by".to_string(),
            properties,
        };

        let triplets = create_triplets_from_graph(&[entity1, entity2], &[edge]);
        assert_eq!(triplets.len(), 1);
        // Relationship segment falls back to relationship_name, not blank.
        assert!(triplets[0].text.contains("-\u{203a}employed_by-\u{203a}"));
        assert!(triplets[0].text.starts_with("Alice"));
    }

    #[test]
    fn test_triplet_skips_missing_source() {
        let entity = create_test_entity("Target", "Description");
        let missing_id = Uuid::new_v4();

        let edge = GraphEdgePair {
            source_entity_id: missing_id, // Not in nodes list
            target_entity_id: entity.entity.base.id,
            relationship_name: "relates".to_string(),
            properties: HashMap::new(),
        };

        let triplets = create_triplets_from_graph(&[entity], &[edge]);
        assert_eq!(triplets.len(), 0, "Should skip edge with missing source");
    }

    #[test]
    fn test_triplet_skips_missing_target() {
        let entity = create_test_entity("Source", "Description");
        let missing_id = Uuid::new_v4();

        let edge = GraphEdgePair {
            source_entity_id: entity.entity.base.id,
            target_entity_id: missing_id, // Not in nodes list
            relationship_name: "relates".to_string(),
            properties: HashMap::new(),
        };

        let triplets = create_triplets_from_graph(&[entity], &[edge]);
        assert_eq!(triplets.len(), 0, "Should skip edge with missing target");
    }

    #[test]
    fn test_triplet_without_descriptions() {
        // Entities with no descriptions should still work (name only)
        let entity1 = create_test_entity("Alice", "");
        let entity2 = create_test_entity("Bob", "");

        let edge = GraphEdgePair {
            source_entity_id: entity1.entity.base.id,
            target_entity_id: entity2.entity.base.id,
            relationship_name: "knows".to_string(),
            properties: HashMap::new(),
        };

        let triplets = create_triplets_from_graph(&[entity1, entity2], &[edge]);

        assert_eq!(triplets.len(), 1);
        let text = &triplets[0].text;
        assert!(text.contains("Alice"));
        assert!(text.contains("knows"));
        assert!(text.contains("Bob"));
        // Should not have ": " since descriptions are empty
        assert!(!text.contains(": "));
    }

    #[test]
    fn test_triplet_format_matches_python() {
        // Python memify format (get_triplet_datapoints.py:157):
        //   f"{start_node_text}-›{relationship_text}-›{end_node_text}"
        // No spaces around arrows. Cognify's add_data_points stage writes into
        // the same "Triplet"/"text" collection, so we use the memify format
        // for consistency across both pipelines.
        let entity1 = create_test_entity("Alice", "");
        let entity2 = create_test_entity("Bob", "");

        let edge = GraphEdgePair {
            source_entity_id: entity1.entity.base.id,
            target_entity_id: entity2.entity.base.id,
            relationship_name: "knows".to_string(),
            properties: HashMap::new(),
        };

        let triplets = create_triplets_from_graph(&[entity1, entity2], &[edge]);
        assert_eq!(triplets.len(), 1);

        // Exact format: "Alice-›knows-›Bob"
        assert_eq!(triplets[0].text, "Alice-\u{203a}knows-\u{203a}Bob");
    }

    #[test]
    fn test_multiple_triplets() {
        let e1 = create_test_entity("A", "Entity A");
        let e2 = create_test_entity("B", "Entity B");
        let e3 = create_test_entity("C", "Entity C");

        let edges = vec![
            GraphEdgePair {
                source_entity_id: e1.entity.base.id,
                target_entity_id: e2.entity.base.id,
                relationship_name: "r1".to_string(),
                properties: HashMap::new(),
            },
            GraphEdgePair {
                source_entity_id: e2.entity.base.id,
                target_entity_id: e3.entity.base.id,
                relationship_name: "r2".to_string(),
                properties: HashMap::new(),
            },
        ];

        let triplets = create_triplets_from_graph(&[e1, e2, e3], &edges);
        assert_eq!(triplets.len(), 2);
    }
}
