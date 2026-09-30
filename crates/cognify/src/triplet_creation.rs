//! Triplet creation from graph nodes and edges.
//!
//! Mirrors Python's _create_triplets_from_graph() in add_data_points.py
//! Creates triplet embeddings from knowledge graph structure.

use cognee_models::Triplet;
use std::collections::HashMap;
use tracing::warn;
use uuid::Uuid;

use crate::graph_integration::{GraphEdgePair, GraphNodePair};

/// Fold a list of freshly built triplets down to one per **point id**, picking
/// the survivor deterministically (SDK-708, the `Triplet_text` half).
///
/// `Triplet::new` hashes `source_id + relationship_name + target_id` through
/// the same lower-case / spaces→underscores / strip-apostrophes normalization
/// the other content-addressed ids use, but the embedded `text` keeps the
/// relation's **raw** spelling. Nothing upstream normalizes it either: the edge
/// dedup key (`GraphEdgePair::dedup_key`) is the raw triple, so
/// `(A, B, "works at")` and `(A, B, "Works At")` both survive as edges and yield
/// two triplets carrying one identical id — with *different* embedding vectors,
/// `text` and `relationship` metadata.
///
/// Emitting both into one `index_points` batch is the id-collision hazard:
/// pgvector rejects a multi-row `INSERT … ON CONFLICT (id) DO UPDATE` that
/// touches a row twice, and since the adapter-side fold landed (#256) one of the
/// two is instead dropped silently, with batch order deciding which. Folding
/// here makes the choice the producer's, and makes it reproducible.
///
/// Survivor rule: lexicographically smallest `text`, tie-broken on
/// `relationship_name` — the same "smallest spelling wins" rule
/// `build_edge_types` applies to `EdgeType`, so the two collections agree about
/// which spelling of a relation is canonical. The sort also removes the
/// non-determinism inherited from the input order, which reaches here from a
/// `HashMap::into_values()` upstream.
pub(crate) fn fold_triplets_by_id(mut triplets: Vec<Triplet>) -> Vec<Triplet> {
    triplets.sort_by(|a, b| {
        a.id.cmp(&b.id)
            .then_with(|| a.text.cmp(&b.text))
            .then_with(|| a.relationship_name.cmp(&b.relationship_name))
    });
    // Equal ids are now adjacent; `dedup_by` keeps the first of each run, which
    // the sort above made the smallest `(text, relationship_name)`.
    triplets.dedup_by(|a, b| a.id == b.id);
    triplets
}

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

    // One row per point id before the caller batches these into
    // `index_points("Triplet", "text", …)` — see [`fold_triplets_by_id`].
    fold_triplets_by_id(triplets)
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

    /// SDK-708 (`Triplet_text` half): edges whose `relationship_name` differs
    /// only in case, spacing or apostrophes collapse onto one triplet point id,
    /// so exactly one triplet may leave the producer — and which one must not
    /// depend on the order the edges happen to arrive in.
    ///
    /// Before the fold, all three edges below produced a triplet carrying the
    /// id `uuid5(OID, "{src}works_at{tgt}")`, and all three went into one
    /// `index_points("Triplet", "text", …)` batch with *different* embedding
    /// text. pgvector rejected the batch outright; every other backend kept
    /// whichever the batch order left last.
    #[test]
    fn triplets_fold_on_derived_id_deterministically() {
        let source = create_test_entity("Alice", "A person");
        let target = create_test_entity("Wonderland Ltd", "A company");
        let nodes = [source.clone(), target.clone()];

        let spellings = ["works at", "Works At", "works_at"];
        let make_edges = |rotation: usize| -> Vec<GraphEdgePair> {
            (0..spellings.len())
                .map(|i| GraphEdgePair {
                    source_entity_id: source.entity.base.id,
                    target_entity_id: target.entity.base.id,
                    relationship_name: spellings[(i + rotation) % spellings.len()].to_string(),
                    properties: HashMap::new(),
                })
                .collect()
        };

        let mut survivors = std::collections::HashSet::new();
        for rotation in 0..spellings.len() {
            let triplets = create_triplets_from_graph(&nodes, &make_edges(rotation));

            assert_eq!(
                triplets.len(),
                1,
                "three spellings of one relation must yield one triplet, got {:?}",
                triplets
                    .iter()
                    .map(|t| (t.id, t.text.as_str()))
                    .collect::<Vec<_>>()
            );
            survivors.insert(triplets[0].text.clone());
        }

        assert_eq!(
            survivors.len(),
            1,
            "edge arrival order changed the embedded triplet text: {survivors:?}"
        );
        // Lexicographically smallest text wins, matching `build_edge_types`.
        assert_eq!(
            survivors.iter().next().map(String::as_str),
            Some("Alice: A person-\u{203a}Works At-\u{203a}Wonderland Ltd: A company")
        );
    }

    /// Relations that normalize *differently* must keep their own triplets —
    /// the fold collapses collisions, it does not merge distinct relations.
    #[test]
    fn triplets_with_distinct_relations_are_not_folded() {
        let source = create_test_entity("Alice", "A person");
        let target = create_test_entity("Wonderland Ltd", "A company");
        let edges: Vec<GraphEdgePair> = ["works at", "founded"]
            .iter()
            .map(|rel| GraphEdgePair {
                source_entity_id: source.entity.base.id,
                target_entity_id: target.entity.base.id,
                relationship_name: (*rel).to_string(),
                properties: HashMap::new(),
            })
            .collect();

        let triplets = create_triplets_from_graph(&[source, target], &edges);
        assert_eq!(triplets.len(), 2);
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

        let source_id = entity1.entity.base.id;
        let target_id = entity2.entity.base.id;
        let triplets = create_triplets_from_graph(&[entity1, entity2], &[edge]);

        assert_eq!(triplets.len(), 1);
        // Should use "works at" from edge_text, not "employed_by"
        assert!(triplets[0].text.contains("works at"));
        assert!(!triplets[0].text.contains("employed_by"));

        // The description must stay out of the *id*: `Triplet::new` hashes
        // `source + relationship_name + target` and nothing else. The delete
        // path relies on this — `cognee-delete`'s `triplet_vector_id`
        // recomputes the point id from the ledger row's `relationship_name`,
        // which is `relationship_name` (not `edge_text`) for every
        // non-`contains` edge — so a described edge and an undescribed one
        // between the same endpoints must key identically. Pinned here, on the
        // writing side, so the two crates cannot drift apart silently.
        assert_eq!(
            triplets[0].id,
            Triplet::new(
                source_id,
                target_id,
                "employed_by".to_string(),
                String::new()
            )
            .id,
            "edge_text must not enter the Triplet id the delete path recomputes"
        );
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
