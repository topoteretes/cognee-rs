//! Graph expansion logic.
//!
//! Mirrors Python's `cognee/modules/graph/utils/expand_with_nodes_and_edges.py`
//! Converts LLM-layer KnowledgeGraph objects to storage-layer Entity/EntityType pairs.

use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use cognee_models::{Entity, EntityType};
use cognee_ontology::{AttachedOntologyNode, NodeCategory, OntologyResolver};
use cognee_ontology::traits::OntologyEdge;
use tracing::warn;

use crate::fact_extraction::{KnowledgeGraph, Node};
use crate::graph_integration::types::{GraphEdgePair, GraphNodePair};

/// Core graph integration function. Converts LLM-layer KnowledgeGraph objects
/// to storage-layer Entity/EntityType pairs.
///
/// This mirrors the Python `expand_with_nodes_and_edges()` function from
/// `cognee/modules/graph/utils/expand_with_nodes_and_edges.py`.
///
/// # Process
/// 1. Create EntityType for each unique node type
/// 2. Create Entity for each node
/// 3. Create Edge for each relationship
/// 4. Deduplicate in-memory using HashMaps
///
/// # Deduplication Keys
/// - **Node**: `{node_id}_{category}` where category = "entity" or "type"
/// - **Edge**: `{source_entity_id}_{target_entity_id}_{relationship_name}`
///
/// # Arguments
/// * `graphs` - Vector of (chunk_id, KnowledgeGraph) pairs. Each graph is
///   paired with the UUID of the chunk it was extracted from, so entities
///   are tagged with the correct source chunk.
/// * `dataset_id` - UUID of the dataset
/// * `existing_edges_set` - Set of edges that already exist in the database
/// * `ontology_resolver` - Ontology resolver for entity validation and enrichment.
///   When loaded, validates entity types against "classes" and entities against
///   "individuals". A [`NoOpOntologyResolver`] leaves everything unvalidated.
///
/// # Returns
/// Tuple of (graph_nodes, graph_edges) for storage.
pub async fn expand_with_nodes_and_edges(
    graphs: Vec<(Uuid, KnowledgeGraph)>,
    dataset_id: Uuid,
    existing_edges_set: &HashSet<String>,
    ontology_resolver: &dyn OntologyResolver,
) -> (Vec<GraphNodePair>, Vec<GraphEdgePair>) {
    // Maps for deduplication
    let mut node_map = HashMap::new();
    let mut edge_map = HashMap::new();
    let mut type_map = HashMap::new();

    // Map from node_id to entity_id for edge resolution
    let mut node_id_to_entity_id: HashMap<String, Uuid> = HashMap::new();

    // Ontology-specific collections (populated by get_subgraph expansion)
    let mut key_mapping: HashMap<String, String> = HashMap::new();
    let mut ontology_types_map: HashMap<String, EntityType> = HashMap::new();
    let mut ontology_entities_map: HashMap<String, GraphNodePair> = HashMap::new();
    let mut ontology_edge_keys: HashSet<String> = HashSet::new();
    let mut ontology_edges_out: Vec<GraphEdgePair> = Vec::new();

    // Process all graphs — each graph carries its source chunk_id
    for (chunk_id, graph) in graphs {
        for node in graph.nodes {
            // Step 1: Create or get EntityType (with ontology subgraph expansion)
            let type_key = format!("{}_type", node.node_type);

            // Check if this key was already remapped to a canonical form
            let effective_key = key_mapping
                .get(&type_key)
                .cloned()
                .unwrap_or_else(|| type_key.clone());

            if !type_map.contains_key(&effective_key) {
                let mut et = EntityType::from_node_type(&node.node_type, Some(dataset_id));

                if ontology_resolver.is_loaded() {
                    match ontology_resolver.get_subgraph(&node.node_type, "classes", true) {
                        Ok((onto_nodes, onto_edges, Some(root_node))) => {
                            let canonical_name = root_node.name.clone();

                            // Canonicalize: rename + regenerate deterministic ID
                            et.mark_ontology_valid(Some(canonical_name.clone()));
                            et.base.id = ontology_name_to_uuid(&canonical_name);

                            // Record key mapping if canonical differs
                            let new_type_key = format!("{}_type", canonical_name);
                            if new_type_key != type_key {
                                key_mapping.insert(type_key.clone(), new_type_key.clone());
                            }

                            // Process ontology subgraph nodes and edges
                            process_ontology_nodes(
                                &onto_nodes,
                                dataset_id,
                                &node_map,
                                &type_map,
                                &mut ontology_types_map,
                                &mut ontology_entities_map,
                            );
                            process_ontology_edges(
                                &onto_edges,
                                existing_edges_set,
                                &mut ontology_edge_keys,
                                &mut ontology_edges_out,
                            );

                            // Insert under canonical key
                            type_map.insert(
                                if new_type_key != type_key {
                                    new_type_key
                                } else {
                                    effective_key.clone()
                                },
                                et,
                            );
                        }
                        Ok((_, _, None)) => {
                            // No match in ontology
                            type_map.insert(effective_key.clone(), et);
                        }
                        Err(e) => {
                            warn!(
                                "Ontology subgraph extraction failed for '{}': {}",
                                node.node_type, e
                            );
                            type_map.insert(effective_key.clone(), et);
                        }
                    }
                } else {
                    type_map.insert(effective_key.clone(), et);
                }
            }

            // Re-resolve the effective key (may have been remapped above)
            let resolved_key = key_mapping
                .get(&type_key)
                .cloned()
                .unwrap_or_else(|| type_key.clone());
            let entity_type = type_map
                .get(&resolved_key)
                .expect("entity type was just inserted or already existed");

            // Step 2: Create Entity
            let entity_key = format!("{}_entity", node.id);

            // Validate entity against ontology "individuals" with subgraph expansion.
            // Collect subgraph data for deferred processing (after insert releases borrow).
            let mut deferred_individual_data = None;

            if let std::collections::hash_map::Entry::Vacant(e) = node_map.entry(entity_key) {
                let mut entity_pair = create_entity_node(
                    &node,
                    entity_type.clone(), // Pass the shared entity_type
                    dataset_id,
                    chunk_id,
                );

                if ontology_resolver.is_loaded() {
                    match ontology_resolver.get_subgraph(&node.name, "individuals", true) {
                        Ok((ont_nodes, ont_edges, Some(root_individual))) => {
                            let canonical_name = root_individual.name.clone();

                            // Store original name in metadata
                            entity_pair.entity.base.set_metadata(
                                "original_name",
                                serde_json::json!(entity_pair.entity.name.clone()),
                            );

                            // Replace name and ID with canonical form
                            entity_pair.entity.name = canonical_name.clone();
                            entity_pair.entity.base.id =
                                ontology_name_to_uuid(&canonical_name);
                            entity_pair.entity.base.set_ontology_valid(true);

                            // Defer subgraph processing until after insert
                            deferred_individual_data = Some((ont_nodes, ont_edges));
                        }
                        Ok((_, _, None)) => {}
                        Err(err) => {
                            warn!(
                                "Ontology individual lookup failed for '{}': {}",
                                node.name, err
                            );
                        }
                    }
                }

                // Track node_id -> entity_id mapping for edge resolution
                node_id_to_entity_id.insert(node.id.clone(), entity_pair.entity.base.id);

                e.insert(entity_pair);
            }

            // Process deferred ontology individual subgraph (outside the Vacant borrow)
            if let Some((ont_nodes, ont_edges)) = deferred_individual_data {
                process_ontology_nodes(
                    &ont_nodes,
                    dataset_id,
                    &node_map,
                    &type_map,
                    &mut ontology_types_map,
                    &mut ontology_entities_map,
                );
                process_ontology_edges(
                    &ont_edges,
                    existing_edges_set,
                    &mut ontology_edge_keys,
                    &mut ontology_edges_out,
                );
            }
        }

        // Step 3: Create Edges (skip if already in database)
        for edge in graph.edges {
            // Look up entity IDs from node IDs; skip edges the LLM produced with
            // node IDs that don't match any extracted node (common with local models).
            let Some(source_entity_id) = node_id_to_entity_id.get(&edge.source_node_id) else {
                warn!(
                    "Skipping edge: source node '{}' not found in extracted nodes",
                    edge.source_node_id
                );
                continue;
            };

            let Some(target_entity_id) = node_id_to_entity_id.get(&edge.target_node_id) else {
                warn!(
                    "Skipping edge: target node '{}' not found in extracted nodes",
                    edge.target_node_id
                );
                continue;
            };

            // Check if edge already exists in database
            let edge_db_key = format!(
                "{}_{}_{}",
                source_entity_id, target_entity_id, edge.relationship_name
            );
            if existing_edges_set.contains(&edge_db_key) {
                // Edge already exists in database, skip it
                continue;
            }

            let edge_key = (
                *source_entity_id,
                *target_entity_id,
                edge.relationship_name.clone(),
            );

            if let std::collections::hash_map::Entry::Vacant(e) = edge_map.entry(edge_key) {
                e.insert(GraphEdgePair::new(
                    *source_entity_id,
                    *target_entity_id,
                    edge.relationship_name,
                ));
            }
        }
    }

    // Merge LLM-extracted nodes with ontology-derived nodes
    let mut graph_nodes: Vec<GraphNodePair> = node_map.into_values().collect();

    // Convert ontology-derived class types into GraphNodePairs (as "type nodes")
    for et in ontology_types_map.into_values() {
        let entity = Entity::from_node(
            &et.name,
            &et.name,
            format!("Ontology-derived type: {}", et.name),
            et.base.id,
            Some(dataset_id),
        );
        graph_nodes.push(GraphNodePair {
            entity,
            entity_type: et,
        });
    }

    // Add ontology-derived individual nodes
    graph_nodes.extend(ontology_entities_map.into_values());

    // Merge LLM-extracted edges with ontology-derived edges
    let mut graph_edges: Vec<GraphEdgePair> = edge_map.into_values().collect();
    graph_edges.extend(ontology_edges_out);

    (graph_nodes, graph_edges)
}

/// Helper: Create Entity from Node.
///
/// Mirrors Python's `_create_entity_node()` function.
fn create_entity_node(
    node: &Node,
    entity_type: EntityType,
    dataset_id: Uuid,
    chunk_id: Uuid,
) -> GraphNodePair {
    let entity = Entity::from_node(
        &node.id,
        &node.name,
        &node.description,
        entity_type.base.id,
        Some(dataset_id),
    );

    // Store chunk_id reference in metadata
    let mut entity_with_chunk = entity;
    entity_with_chunk
        .base
        .set_metadata("chunk_id", serde_json::json!(chunk_id.to_string()));

    GraphNodePair {
        entity: entity_with_chunk,
        entity_type,
    }
}

/// Compute a deterministic UUID5 from a normalized name.
///
/// Follows Python's `generate_node_id()` pattern: lowercase, replace spaces
/// with underscores, strip apostrophes, then hash with UUID5 NAMESPACE_OID.
fn ontology_name_to_uuid(name: &str) -> Uuid {
    let normalized = name.to_lowercase().replace(' ', "_").replace('\'', "");
    Uuid::new_v5(&Uuid::NAMESPACE_OID, normalized.as_bytes())
}

/// Normalize an edge/relationship name for deduplication and storage.
///
/// Lowercases, replaces spaces with underscores, and strips apostrophes.
fn normalize_edge_name(name: &str) -> String {
    name.to_lowercase().replace(' ', "_").replace('\'', "")
}

/// Convert ontology subgraph nodes into graph integration types.
///
/// For each [`AttachedOntologyNode`]:
/// - **Classes** become [`EntityType`] entries in `ontology_types_map`
/// - **Individuals** become [`GraphNodePair`] entries in `ontology_entities_map`
///
/// All produced items receive deterministic UUID5 IDs and `ontology_valid = true`.
/// Duplicates are skipped when a matching key already exists in the LLM-produced
/// maps (`node_map`, `type_map`) or in the ontology output maps.
fn process_ontology_nodes(
    ontology_nodes: &[AttachedOntologyNode],
    dataset_id: Uuid,
    node_map: &HashMap<String, GraphNodePair>,
    type_map: &HashMap<String, EntityType>,
    ontology_types_map: &mut HashMap<String, EntityType>,
    ontology_entities_map: &mut HashMap<String, GraphNodePair>,
) {
    for node in ontology_nodes {
        let node_id = ontology_name_to_uuid(&node.name);

        match node.category {
            NodeCategory::Classes => {
                let dedup_key = format!("{}_type", node_id);
                // Skip if the LLM already extracted this type (check by name-based key)
                let llm_type_key = format!("{}_type", node.name);
                if type_map.contains_key(&llm_type_key) || ontology_types_map.contains_key(&dedup_key) {
                    continue;
                }
                // Also skip if there is already a node_map entry for this node id
                let node_entity_key = format!("{}_entity", node_id);
                if node_map.contains_key(&node_entity_key) {
                    continue;
                }

                let mut et = EntityType::new(&node.name, &node.name, Some(dataset_id));
                et.base.id = node_id;
                et.base.set_ontology_valid(true);
                ontology_types_map.insert(dedup_key, et);
            }
            NodeCategory::Individuals => {
                let dedup_key = format!("{}_entity", node_id);
                // Skip if already present in either map
                if node_map.contains_key(&dedup_key) || ontology_entities_map.contains_key(&dedup_key) {
                    continue;
                }

                let mut entity = Entity::new(&node.name, None, &node.name, Some(dataset_id));
                entity.base.id = node_id;
                entity.base.set_ontology_valid(true);

                // Placeholder EntityType for the GraphNodePair
                let mut placeholder_et = EntityType::new("OntologyIndividual", "", Some(dataset_id));
                placeholder_et.base.id = ontology_name_to_uuid("ontologyindividual");

                let pair = GraphNodePair {
                    entity,
                    entity_type: placeholder_et,
                };
                ontology_entities_map.insert(dedup_key, pair);
            }
        }
    }
}

/// Convert ontology edge tuples into [`GraphEdgePair`] objects.
///
/// Each `(source, relation, target)` tuple is mapped to a [`GraphEdgePair`] with
/// deterministic UUID5 source/target IDs and normalized relationship names. Edges
/// that already exist (in `existing_edge_keys` or `ontology_edge_keys`) are skipped.
fn process_ontology_edges(
    ontology_edges: &[OntologyEdge],
    existing_edge_keys: &HashSet<String>,
    ontology_edge_keys: &mut HashSet<String>,
    ontology_edges_out: &mut Vec<GraphEdgePair>,
) {
    for (source, relation, target) in ontology_edges {
        let source_id = ontology_name_to_uuid(source);
        let target_id = ontology_name_to_uuid(target);
        let rel_name = normalize_edge_name(relation);
        let edge_key = format!("{}_{}_{}", source_id, target_id, rel_name);

        if existing_edge_keys.contains(&edge_key) || ontology_edge_keys.contains(&edge_key) {
            continue;
        }

        let mut edge = GraphEdgePair::new(source_id, target_id, &rel_name);
        edge.add_property("ontology_valid", "true");
        edge.add_property("relationship_name", &rel_name);
        edge.add_property("source_node_id", source_id.to_string());
        edge.add_property("target_node_id", target_id.to_string());

        ontology_edge_keys.insert(edge_key);
        ontology_edges_out.push(edge);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fact_extraction::Edge;
    use cognee_ontology::{NoOpOntologyResolver, OntologyResult, traits::OntologySubgraph};

    /// Helper to get the default no-op resolver used by most tests.
    fn noop() -> NoOpOntologyResolver {
        NoOpOntologyResolver::new()
    }

    fn create_test_graph() -> KnowledgeGraph {
        KnowledgeGraph {
            nodes: vec![
                Node {
                    id: "techcorp_1".to_string(),
                    name: "TechCorp".to_string(),
                    node_type: "Organization".to_string(),
                    description: "A technology company".to_string(),
                },
                Node {
                    id: "alice_1".to_string(),
                    name: "Alice".to_string(),
                    node_type: "Person".to_string(),
                    description: "A software engineer".to_string(),
                },
            ],
            edges: vec![Edge {
                source_node_id: "alice_1".to_string(),
                target_node_id: "techcorp_1".to_string(),
                relationship_name: "works_at".to_string(),
            }],
        }
    }

    #[tokio::test]
    async fn test_expand_single_graph() {
        let graph = create_test_graph();
        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();

        let (nodes, edges) = expand_with_nodes_and_edges(
            vec![(chunk_id, graph)],
            dataset_id,
            &HashSet::new(),
            &noop(),
        )
        .await;

        // Should have 2 nodes (TechCorp, Alice)
        assert_eq!(nodes.len(), 2);

        // Should have 1 edge (Alice works_at TechCorp)
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].relationship_name, "works_at");

        // Verify node names
        let names: Vec<String> = nodes.iter().map(|n| n.entity.name.clone()).collect();
        assert!(names.contains(&"TechCorp".to_string()));
        assert!(names.contains(&"Alice".to_string()));
    }

    #[tokio::test]
    async fn test_expand_deduplicates_nodes() {
        let graph1 = create_test_graph();
        let graph2 = create_test_graph();

        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();

        let (nodes, edges) = expand_with_nodes_and_edges(
            vec![(chunk_id, graph1), (chunk_id, graph2)],
            dataset_id,
            &HashSet::new(),
            &noop(),
        )
        .await;

        // Should have 2 unique nodes (deduplication by node_id)
        assert_eq!(nodes.len(), 2);

        // Should have 1 unique edge (deduplication by source+target+relationship)
        assert_eq!(edges.len(), 1);
    }

    #[tokio::test]
    async fn test_expand_creates_entity_types() {
        let graph = create_test_graph();
        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();

        let (nodes, _) = expand_with_nodes_and_edges(
            vec![(chunk_id, graph)],
            dataset_id,
            &HashSet::new(),
            &noop(),
        )
        .await;

        // Check that entity types are created
        for node_pair in &nodes {
            assert!(!node_pair.entity_type.name.is_empty());
            assert_eq!(node_pair.entity_type.base.data_type, "EntityType");
        }

        // Verify types
        let types: Vec<String> = nodes.iter().map(|n| n.entity_type.name.clone()).collect();
        assert!(types.contains(&"Organization".to_string()));
        assert!(types.contains(&"Person".to_string()));
    }

    #[tokio::test]
    async fn test_expand_links_entities_to_types() {
        let graph = create_test_graph();
        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();

        let (nodes, _) = expand_with_nodes_and_edges(
            vec![(chunk_id, graph)],
            dataset_id,
            &HashSet::new(),
            &noop(),
        )
        .await;

        // Check that entities reference their types
        for node_pair in &nodes {
            assert_eq!(node_pair.entity.is_a, Some(node_pair.entity_type.base.id));
        }
    }

    #[tokio::test]
    async fn test_expand_stores_chunk_reference() {
        let graph = create_test_graph();
        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();

        let (nodes, _) = expand_with_nodes_and_edges(
            vec![(chunk_id, graph)],
            dataset_id,
            &HashSet::new(),
            &noop(),
        )
        .await;

        // Verify chunk_id is stored in metadata
        for node_pair in &nodes {
            let chunk_ref = node_pair.entity.base.get_metadata("chunk_id");
            assert!(chunk_ref.is_some());
        }
    }

    #[tokio::test]
    async fn test_expand_missing_target_node_is_skipped() {
        let graph = KnowledgeGraph {
            nodes: vec![Node {
                id: "alice_1".to_string(),
                name: "Alice".to_string(),
                node_type: "Person".to_string(),
                description: "A person".to_string(),
            }],
            edges: vec![Edge {
                source_node_id: "alice_1".to_string(),
                target_node_id: "missing_node".to_string(), // LLM inconsistency
                relationship_name: "knows".to_string(),
            }],
        };

        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();

        let (nodes, edges) = expand_with_nodes_and_edges(
            vec![(chunk_id, graph)],
            dataset_id,
            &HashSet::new(),
            &noop(),
        )
        .await;

        // Node is kept; the unresolvable edge is silently skipped
        assert_eq!(nodes.len(), 1);
        assert_eq!(edges.len(), 0);
    }

    #[tokio::test]
    async fn test_expand_empty_graphs() {
        let dataset_id = Uuid::new_v4();

        let (nodes, edges) =
            expand_with_nodes_and_edges(vec![], dataset_id, &HashSet::new(), &noop()).await;

        assert_eq!(nodes.len(), 0);
        assert_eq!(edges.len(), 0);
    }

    #[tokio::test]
    async fn test_expand_multiple_edges_same_entities() {
        let graph = KnowledgeGraph {
            nodes: vec![
                Node {
                    id: "alice_1".to_string(),
                    name: "Alice".to_string(),
                    node_type: "Person".to_string(),
                    description: "A person".to_string(),
                },
                Node {
                    id: "techcorp_1".to_string(),
                    name: "TechCorp".to_string(),
                    node_type: "Organization".to_string(),
                    description: "A company".to_string(),
                },
            ],
            edges: vec![
                Edge {
                    source_node_id: "alice_1".to_string(),
                    target_node_id: "techcorp_1".to_string(),
                    relationship_name: "works_at".to_string(),
                },
                Edge {
                    source_node_id: "alice_1".to_string(),
                    target_node_id: "techcorp_1".to_string(),
                    relationship_name: "founded".to_string(),
                },
            ],
        };

        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();

        let (nodes, edges) = expand_with_nodes_and_edges(
            vec![(chunk_id, graph)],
            dataset_id,
            &HashSet::new(),
            &noop(),
        )
        .await;

        assert_eq!(nodes.len(), 2);
        // Should have 2 edges (different relationships)
        assert_eq!(edges.len(), 2);

        let relationships: Vec<String> =
            edges.iter().map(|e| e.relationship_name.clone()).collect();
        assert!(relationships.contains(&"works_at".to_string()));
        assert!(relationships.contains(&"founded".to_string()));
    }

    #[tokio::test]
    async fn test_expand_multiple_chunks_different_ids() {
        // Create two graphs from different chunks — each entity should get
        // the chunk_id of the chunk it was extracted from.
        let chunk_id_a = Uuid::new_v4();
        let chunk_id_b = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();

        let graph_a = KnowledgeGraph {
            nodes: vec![Node {
                id: "alice_1".to_string(),
                name: "Alice".to_string(),
                node_type: "Person".to_string(),
                description: "A software engineer".to_string(),
            }],
            edges: vec![],
        };

        let graph_b = KnowledgeGraph {
            nodes: vec![Node {
                id: "bob_1".to_string(),
                name: "Bob".to_string(),
                node_type: "Person".to_string(),
                description: "A data scientist".to_string(),
            }],
            edges: vec![],
        };

        let (nodes, _edges) = expand_with_nodes_and_edges(
            vec![(chunk_id_a, graph_a), (chunk_id_b, graph_b)],
            dataset_id,
            &HashSet::new(),
            &noop(),
        )
        .await;

        assert_eq!(nodes.len(), 2);

        // Find each node and verify its chunk_id metadata
        for node_pair in &nodes {
            let chunk_ref = node_pair
                .entity
                .base
                .get_metadata("chunk_id")
                .expect("chunk_id metadata should be present");

            if node_pair.entity.name == "Alice" {
                assert_eq!(
                    chunk_ref.as_str().unwrap(),
                    chunk_id_a.to_string(),
                    "Alice should be tagged with chunk_id_a"
                );
            } else if node_pair.entity.name == "Bob" {
                assert_eq!(
                    chunk_ref.as_str().unwrap(),
                    chunk_id_b.to_string(),
                    "Bob should be tagged with chunk_id_b"
                );
            } else {
                panic!("Unexpected entity name: {}", node_pair.entity.name);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Mock ontology resolver for testing ontology validation
    // -----------------------------------------------------------------------

    /// Mock resolver that returns canonical names and realistic subgraphs.
    ///
    /// **`get_subgraph` behavior:**
    /// - `("Organization", "classes")` → root "organisation" with ancestor "legalentity", is_a edge
    /// - `("Person", "classes")` → root "person", no ancestors
    /// - Everything else → empty
    ///
    /// **`find_closest_match` behavior:**
    /// - `("Alice", "individuals")` → `Some("Alice_Canonical")`
    /// - Everything else → `None` (classes are handled via get_subgraph)
    struct MockOntologyResolver;

    impl OntologyResolver for MockOntologyResolver {
        fn find_closest_match(&self, name: &str, category: &str) -> OntologyResult<Option<String>> {
            match (name, category) {
                ("Alice", "individuals") => Ok(Some("Alice_Canonical".to_string())),
                _ => Ok(None),
            }
        }

        fn get_subgraph(
            &self,
            node_name: &str,
            node_type: &str,
            _directed: bool,
        ) -> OntologyResult<OntologySubgraph> {
            match (node_name, node_type) {
                ("Organization", "classes") => {
                    let root = AttachedOntologyNode {
                        uri: "http://test.org#Organisation".to_string(),
                        name: "organisation".to_string(),
                        category: NodeCategory::Classes,
                    };
                    let ancestor = AttachedOntologyNode {
                        uri: "http://test.org#LegalEntity".to_string(),
                        name: "legalentity".to_string(),
                        category: NodeCategory::Classes,
                    };
                    Ok((
                        vec![ancestor],
                        vec![(
                            "organisation".to_string(),
                            "is_a".to_string(),
                            "legalentity".to_string(),
                        )],
                        Some(root),
                    ))
                }
                ("Person", "classes") => {
                    let root = AttachedOntologyNode {
                        uri: "http://test.org#Person".to_string(),
                        name: "person".to_string(),
                        category: NodeCategory::Classes,
                    };
                    Ok((vec![], vec![], Some(root)))
                }
                ("Alice", "individuals") => {
                    let root = AttachedOntologyNode {
                        uri: "http://test.org#alice_canonical".to_string(),
                        name: "alice_canonical".to_string(),
                        category: NodeCategory::Individuals,
                    };
                    Ok((vec![], vec![], Some(root)))
                }
                _ => Ok((vec![], vec![], None)),
            }
        }

        fn is_loaded(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn test_expand_with_ontology_validates_entity_types() {
        let graph = create_test_graph();
        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();
        let resolver = MockOntologyResolver;

        let (nodes, _edges) = expand_with_nodes_and_edges(
            vec![(chunk_id, graph)],
            dataset_id,
            &HashSet::new(),
            &resolver,
        )
        .await;

        // 2 LLM nodes + 1 ontology ancestor (legalentity) = 3
        assert!(nodes.len() >= 2, "Expected at least 2 nodes, got {}", nodes.len());

        // Find LLM-extracted nodes (not ontology-derived)
        // Note: Alice's name is canonicalized to "alice_canonical" by individual matching
        let llm_nodes: Vec<_> = nodes
            .iter()
            .filter(|n| {
                n.entity.name == "TechCorp"
                    || n.entity.name == "Alice"
                    || n.entity.name == "alice_canonical"
            })
            .collect();
        assert_eq!(llm_nodes.len(), 2);

        for node_pair in &llm_nodes {
            // All entity types should be ontology-valid (both "Organization"
            // and "Person" are matched by MockOntologyResolver via get_subgraph)
            assert!(
                node_pair.entity_type.is_ontology_valid(),
                "EntityType '{}' should be ontology-valid",
                node_pair.entity_type.name
            );

            if node_pair.entity.name == "TechCorp" {
                // "Organization" → canonical "organisation" (lowercase from uri_to_key)
                assert_eq!(node_pair.entity_type.name, "organisation");
            } else if node_pair.entity.name == "alice_canonical" {
                // "Person" → canonical "person" (lowercase from uri_to_key)
                assert_eq!(node_pair.entity_type.name, "person");
                // Alice is matched as individual and canonicalized
                assert!(
                    node_pair.entity.base.ontology_valid,
                    "Entity 'alice_canonical' should be ontology-valid"
                );
                // Original name stored in metadata
                assert_eq!(
                    node_pair.entity.base.get_metadata("original_name"),
                    Some(&serde_json::json!("Alice")),
                );
            }
        }
    }

    #[tokio::test]
    async fn test_expand_noop_resolver_leaves_entities_unvalidated() {
        let graph = create_test_graph();
        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();

        let (nodes, _edges) = expand_with_nodes_and_edges(
            vec![(chunk_id, graph)],
            dataset_id,
            &HashSet::new(),
            &noop(),
        )
        .await;

        // With NoOp resolver nothing should be ontology-validated
        for node_pair in &nodes {
            assert!(
                !node_pair.entity_type.is_ontology_valid(),
                "EntityType '{}' should NOT be ontology-valid with NoOp resolver",
                node_pair.entity_type.name
            );
            assert!(
                !node_pair.entity.base.ontology_valid,
                "Entity '{}' should NOT be ontology-valid with NoOp resolver",
                node_pair.entity.name
            );
        }
    }

    // -----------------------------------------------------------------------
    // Tests for ontology helper functions
    // -----------------------------------------------------------------------

    #[test]
    fn test_ontology_name_to_uuid_deterministic() {
        // "Car" and "car" should produce the same UUID (both normalize to "car")
        let uuid_upper = ontology_name_to_uuid("Car");
        let uuid_lower = ontology_name_to_uuid("car");
        assert_eq!(uuid_upper, uuid_lower);

        // Should match the canonical UUID5 derivation for "car"
        let expected = Uuid::new_v5(&Uuid::NAMESPACE_OID, b"car");
        assert_eq!(uuid_upper, expected);
    }

    #[test]
    fn test_normalize_edge_name() {
        assert_eq!(normalize_edge_name("is a"), "is_a");
        assert_eq!(normalize_edge_name("Is A"), "is_a");
        assert_eq!(normalize_edge_name("don't know"), "dont_know");
    }

    #[test]
    fn test_process_ontology_nodes_creates_entity_types_for_classes() {
        let dataset_id = Uuid::new_v4();
        let nodes = vec![
            AttachedOntologyNode {
                uri: "http://example.org#Vehicle".to_string(),
                name: "Vehicle".to_string(),
                category: NodeCategory::Classes,
            },
            AttachedOntologyNode {
                uri: "http://example.org#Car".to_string(),
                name: "Car".to_string(),
                category: NodeCategory::Classes,
            },
        ];

        let node_map = HashMap::new();
        let type_map = HashMap::new();
        let mut ontology_types_map = HashMap::new();
        let mut ontology_entities_map = HashMap::new();

        process_ontology_nodes(
            &nodes,
            dataset_id,
            &node_map,
            &type_map,
            &mut ontology_types_map,
            &mut ontology_entities_map,
        );

        assert_eq!(ontology_types_map.len(), 2);
        assert!(ontology_entities_map.is_empty());

        // Verify each EntityType has ontology_valid=true and deterministic IDs
        for et in ontology_types_map.values() {
            assert!(et.base.ontology_valid);
        }

        // Check deterministic IDs
        let vehicle_key = format!("{}_type", ontology_name_to_uuid("Vehicle"));
        let car_key = format!("{}_type", ontology_name_to_uuid("Car"));
        assert!(ontology_types_map.contains_key(&vehicle_key));
        assert!(ontology_types_map.contains_key(&car_key));

        let vehicle_et = &ontology_types_map[&vehicle_key];
        assert_eq!(vehicle_et.base.id, ontology_name_to_uuid("Vehicle"));
        assert_eq!(vehicle_et.name, "Vehicle");
    }

    #[test]
    fn test_process_ontology_nodes_skips_duplicates() {
        let dataset_id = Uuid::new_v4();
        let nodes = vec![AttachedOntologyNode {
            uri: "http://example.org#Organization".to_string(),
            name: "Organization".to_string(),
            category: NodeCategory::Classes,
        }];

        let node_map = HashMap::new();
        // Pre-populate type_map with an "Organization" entry (as if LLM already extracted it)
        let mut type_map = HashMap::new();
        type_map.insert(
            "Organization_type".to_string(),
            EntityType::new("Organization", "A type", Some(dataset_id)),
        );

        let mut ontology_types_map = HashMap::new();
        let mut ontology_entities_map = HashMap::new();

        process_ontology_nodes(
            &nodes,
            dataset_id,
            &node_map,
            &type_map,
            &mut ontology_types_map,
            &mut ontology_entities_map,
        );

        // Should be skipped because it already exists in type_map
        assert!(ontology_types_map.is_empty());
    }

    #[test]
    fn test_process_ontology_nodes_creates_entities_for_individuals() {
        let dataset_id = Uuid::new_v4();
        let nodes = vec![AttachedOntologyNode {
            uri: "http://example.org#MyCar".to_string(),
            name: "MyCar".to_string(),
            category: NodeCategory::Individuals,
        }];

        let node_map = HashMap::new();
        let type_map = HashMap::new();
        let mut ontology_types_map = HashMap::new();
        let mut ontology_entities_map = HashMap::new();

        process_ontology_nodes(
            &nodes,
            dataset_id,
            &node_map,
            &type_map,
            &mut ontology_types_map,
            &mut ontology_entities_map,
        );

        assert_eq!(ontology_entities_map.len(), 1);
        assert!(ontology_types_map.is_empty());

        let dedup_key = format!("{}_entity", ontology_name_to_uuid("MyCar"));
        let pair = &ontology_entities_map[&dedup_key];
        assert!(pair.entity.base.ontology_valid);
        assert_eq!(pair.entity.base.id, ontology_name_to_uuid("MyCar"));
        assert_eq!(pair.entity.name, "MyCar");
        // Placeholder type
        assert_eq!(pair.entity_type.name, "OntologyIndividual");
        assert_eq!(
            pair.entity_type.base.id,
            ontology_name_to_uuid("ontologyindividual")
        );
    }

    #[test]
    fn test_process_ontology_edges_creates_edges() {
        let edges: Vec<OntologyEdge> = vec![
            (
                "Car".to_string(),
                "is a".to_string(),
                "Vehicle".to_string(),
            ),
            (
                "Vehicle".to_string(),
                "has part".to_string(),
                "Engine".to_string(),
            ),
        ];

        let existing_edge_keys = HashSet::new();
        let mut ontology_edge_keys = HashSet::new();
        let mut ontology_edges_out = Vec::new();

        process_ontology_edges(
            &edges,
            &existing_edge_keys,
            &mut ontology_edge_keys,
            &mut ontology_edges_out,
        );

        assert_eq!(ontology_edges_out.len(), 2);
        assert_eq!(ontology_edge_keys.len(), 2);

        // Verify first edge: Car -> Vehicle via "is_a"
        let car_id = ontology_name_to_uuid("Car");
        let vehicle_id = ontology_name_to_uuid("Vehicle");
        let edge0 = &ontology_edges_out[0];
        assert_eq!(edge0.source_entity_id, car_id);
        assert_eq!(edge0.target_entity_id, vehicle_id);
        assert_eq!(edge0.relationship_name, "is_a");
        assert_eq!(edge0.properties.get("ontology_valid"), Some(&"true".to_string()));
        assert_eq!(
            edge0.properties.get("source_node_id"),
            Some(&car_id.to_string())
        );
        assert_eq!(
            edge0.properties.get("target_node_id"),
            Some(&vehicle_id.to_string())
        );

        // Verify second edge: Vehicle -> Engine via "has_part"
        let engine_id = ontology_name_to_uuid("Engine");
        let edge1 = &ontology_edges_out[1];
        assert_eq!(edge1.source_entity_id, vehicle_id);
        assert_eq!(edge1.target_entity_id, engine_id);
        assert_eq!(edge1.relationship_name, "has_part");
    }

    #[test]
    fn test_process_ontology_edges_skips_existing() {
        let car_id = ontology_name_to_uuid("Car");
        let vehicle_id = ontology_name_to_uuid("Vehicle");
        let existing_key = format!("{}_{}_{}", car_id, vehicle_id, "is_a");

        let mut existing_edge_keys = HashSet::new();
        existing_edge_keys.insert(existing_key);

        let edges: Vec<OntologyEdge> = vec![
            (
                "Car".to_string(),
                "is a".to_string(),
                "Vehicle".to_string(),
            ),
            (
                "Vehicle".to_string(),
                "has part".to_string(),
                "Engine".to_string(),
            ),
        ];

        let mut ontology_edge_keys = HashSet::new();
        let mut ontology_edges_out = Vec::new();

        process_ontology_edges(
            &edges,
            &existing_edge_keys,
            &mut ontology_edge_keys,
            &mut ontology_edges_out,
        );

        // Only the second edge should be present; the first is in existing_edge_keys
        assert_eq!(ontology_edges_out.len(), 1);
        assert_eq!(ontology_edges_out[0].relationship_name, "has_part");
    }

    // -----------------------------------------------------------------------
    // Tests for entity type subgraph expansion (Step 3)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_expand_ontology_adds_ancestor_type_nodes() {
        // "Organization" matches ontology class "organisation" which has ancestor "legalentity"
        let graph = KnowledgeGraph {
            nodes: vec![Node {
                id: "techcorp_1".to_string(),
                name: "TechCorp".to_string(),
                node_type: "Organization".to_string(),
                description: "A technology company".to_string(),
            }],
            edges: vec![],
        };

        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();
        let resolver = MockOntologyResolver;

        let (nodes, _edges) = expand_with_nodes_and_edges(
            vec![(chunk_id, graph)],
            dataset_id,
            &HashSet::new(),
            &resolver,
        )
        .await;

        // LLM node (TechCorp) + ontology-derived ancestor (legalentity)
        assert!(
            nodes.len() >= 2,
            "Expected at least 2 nodes (LLM + ontology ancestor), got {}",
            nodes.len()
        );

        // The ancestor "legalentity" should be present as an ontology-derived node
        let legalentity_node = nodes
            .iter()
            .find(|n| n.entity.name == "legalentity" || n.entity_type.name == "legalentity");
        assert!(
            legalentity_node.is_some(),
            "Expected ontology-derived 'legalentity' node in output"
        );

        // The ancestor should be ontology-valid
        if let Some(le) = legalentity_node {
            assert!(le.entity_type.base.ontology_valid || le.entity.base.ontology_valid);
        }
    }

    #[tokio::test]
    async fn test_expand_ontology_adds_is_a_edges() {
        // "Organization" matches ontology class "organisation" → is_a → "legalentity"
        let graph = KnowledgeGraph {
            nodes: vec![Node {
                id: "techcorp_1".to_string(),
                name: "TechCorp".to_string(),
                node_type: "Organization".to_string(),
                description: "A technology company".to_string(),
            }],
            edges: vec![],
        };

        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();
        let resolver = MockOntologyResolver;

        let (_nodes, edges) = expand_with_nodes_and_edges(
            vec![(chunk_id, graph)],
            dataset_id,
            &HashSet::new(),
            &resolver,
        )
        .await;

        // There should be an ontology-derived "is_a" edge
        let is_a_edges: Vec<_> = edges
            .iter()
            .filter(|e| e.relationship_name == "is_a")
            .collect();
        assert_eq!(
            is_a_edges.len(),
            1,
            "Expected exactly 1 is_a edge from ontology"
        );

        let is_a = &is_a_edges[0];

        // Source = organisation, target = legalentity (deterministic UUIDs)
        assert_eq!(is_a.source_entity_id, ontology_name_to_uuid("organisation"));
        assert_eq!(
            is_a.target_entity_id,
            ontology_name_to_uuid("legalentity")
        );

        // Should be marked as ontology-derived
        assert_eq!(
            is_a.properties.get("ontology_valid"),
            Some(&"true".to_string())
        );
    }
}
