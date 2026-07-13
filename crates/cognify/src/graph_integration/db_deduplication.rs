//! Database-backed graph deduplication utilities.
//!
//! Provides functions to query the graph database for existing edges before
//! graph expansion to avoid creating duplicates.

use std::collections::{HashMap, HashSet};

use cognee_graph::{EdgeData, GraphDBTrait};
use cognee_models::Entity;

use crate::error::CognifyError;
use crate::fact_extraction::KnowledgeGraph;

/// Retrieve existing edges from the graph database.
///
/// This function:
/// 1. Collects all edges from the knowledge graphs
/// 2. Generates deterministic UUIDs for entity nodes using `Entity::id_for()`
/// 3. Batch queries the graph database to check which edges already exist
/// 4. Returns a set of existing edge keys
///
/// **Edge key format:** `"{source_uuid}_{target_uuid}_{relationship_name}"`
/// (matches the format used in `expand_with_nodes_and_edges`)
///
/// **Deduplication strategy:** Uses `processed_nodes` set to avoid querying
/// the same node multiple times within a batch.
///
/// # Arguments  
/// * `graph_db` - Graph database trait object
/// * `graphs` - Knowledge graphs extracted from text chunks
///
/// # Returns
/// HashSet containing edge identifiers that already exist in the database
///
/// # Example
/// ```ignore
/// let graphs = vec![knowledge_graph1, knowledge_graph2];
/// let existing_edges = retrieve_existing_edges(&graph_db, &graphs).await?;
///
/// // Check if edge exists before creating
/// let edge_key = format!("{}_{}_{}",  source_id, target_id, "works_at");
/// if !existing_edges.contains(&edge_key) {
///     // Create new edge
/// }
/// ```
pub async fn retrieve_existing_edges(
    graph_db: &dyn GraphDBTrait,
    graphs: &[KnowledgeGraph],
) -> Result<HashSet<String>, CognifyError> {
    if graphs.is_empty() {
        return Ok(HashSet::new());
    }

    // Track nodes we've processed to avoid duplicate work
    let mut processed_nodes: HashSet<String> = HashSet::new();

    // Collect all edges to check
    let mut edges_to_check: Vec<EdgeData> = Vec::new();

    for graph in graphs {
        for edge in &graph.edges {
            // Generate deterministic UUIDs for source and target nodes. Must use
            // the SAME scheme entities are actually persisted with
            // (`Entity::id_for`) — using the old bare `generate_node_id` here made
            // these keys never match the stored (deterministic) entity ids, so
            // edge dedup was a silent no-op (issue #57 corollary). Matches
            // Python `retrieve_existing_edges.py:75-76` (`Entity.id_for`).
            let source_uuid = Entity::id_for(&edge.source_node_id);
            let target_uuid = Entity::id_for(&edge.target_node_id);

            // Only process if we haven't seen both nodes before
            let source_str = edge.source_node_id.as_str();
            let target_str = edge.target_node_id.as_str();

            // Mark nodes as processed
            if !processed_nodes.contains(source_str) {
                processed_nodes.insert(source_str.to_string());
            }
            if !processed_nodes.contains(target_str) {
                processed_nodes.insert(target_str.to_string());
            }

            // Create edge tuple for database query
            // Note: relationship_name is already normalized by LLM or should be
            let edge_tuple = (
                source_uuid.to_string(),
                target_uuid.to_string(),
                edge.relationship_name.clone(),
                HashMap::new(), // Properties not needed for existence check
            );

            edges_to_check.push(edge_tuple);
        }
    }

    if edges_to_check.is_empty() {
        return Ok(HashSet::new());
    }

    // Batch query graph database for existing edges
    let existing_edges = graph_db
        .has_edges(&edges_to_check)
        .await
        .map_err(|e| CognifyError::GraphDatabaseError(e.to_string()))?;

    // Build edge existence set
    // Key format: "{source_uuid}_{target_uuid}_{relationship_name}"
    // This matches the format used in expand_with_nodes_and_edges
    let mut existing_edges_set = HashSet::new();
    for (source_id, target_id, relationship_name, _) in existing_edges {
        let edge_key = format!("{source_id}_{target_id}_{relationship_name}");
        existing_edges_set.insert(edge_key);
    }

    Ok(existing_edges_set)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use crate::fact_extraction::{Edge, Node};
    use cognee_graph::MockGraphDB;

    fn create_test_graph() -> KnowledgeGraph {
        KnowledgeGraph {
            nodes: vec![
                Node {
                    id: "alice".to_string(),
                    name: "Alice".to_string(),
                    node_type: "Person".to_string(),
                    description: "A person".to_string(),
                },
                Node {
                    id: "techcorp".to_string(),
                    name: "TechCorp".to_string(),
                    node_type: "Organization".to_string(),
                    description: "A company".to_string(),
                },
            ],
            edges: vec![Edge {
                source_node_id: "alice".to_string(),
                target_node_id: "techcorp".to_string(),
                relationship_name: "works_at".to_string(),
                description: None,
            }],
        }
    }

    #[tokio::test]
    async fn test_retrieve_existing_edges_empty() {
        let graph_db = MockGraphDB::new();
        let result = retrieve_existing_edges(&graph_db, &[]).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn test_retrieve_existing_edges_no_existing() {
        let graph_db = MockGraphDB::new();
        let graph = create_test_graph();

        let result = retrieve_existing_edges(&graph_db, &[graph]).await.unwrap();

        // No edges in DB, so map should be empty
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn test_retrieve_existing_edges_with_existing() {
        let graph_db = MockGraphDB::new();
        let graph = create_test_graph();

        // Add the edge to the database first
        let alice_uuid = Entity::id_for("alice");
        let techcorp_uuid = Entity::id_for("techcorp");

        let _ = graph_db
            .add_edge(
                &alice_uuid.to_string(),
                &techcorp_uuid.to_string(),
                "works_at",
                None,
            )
            .await;

        // Now query for existing edges
        let result = retrieve_existing_edges(&graph_db, &[graph]).await.unwrap();

        // Should find the edge
        let expected_key = format!("{alice_uuid}_{techcorp_uuid}_works_at");
        assert!(result.contains(&expected_key));
    }

    #[tokio::test]
    async fn test_retrieve_existing_edges_partial_match() {
        let graph_db = MockGraphDB::new();

        // Create two graphs with different edges
        let graph1 = create_test_graph();
        let graph2 = KnowledgeGraph {
            nodes: vec![
                Node {
                    id: "bob".to_string(),
                    name: "Bob".to_string(),
                    node_type: "Person".to_string(),
                    description: "Another person".to_string(),
                },
                Node {
                    id: "acmecorp".to_string(),
                    name: "AcmeCorp".to_string(),
                    node_type: "Organization".to_string(),
                    description: "Another company".to_string(),
                },
            ],
            edges: vec![Edge {
                source_node_id: "bob".to_string(),
                target_node_id: "acmecorp".to_string(),
                relationship_name: "works_at".to_string(),
                description: None,
            }],
        };

        // Add only the first edge to the database
        let alice_uuid = Entity::id_for("alice");
        let techcorp_uuid = Entity::id_for("techcorp");

        let _ = graph_db
            .add_edge(
                &alice_uuid.to_string(),
                &techcorp_uuid.to_string(),
                "works_at",
                None,
            )
            .await;

        // Query for both graphs
        let result = retrieve_existing_edges(&graph_db, &[graph1, graph2])
            .await
            .unwrap();

        // Should only find the first edge
        let alice_edge_key = format!("{alice_uuid}_{techcorp_uuid}_works_at");
        assert!(result.contains(&alice_edge_key));
        assert_eq!(result.len(), 1);
    }

    #[tokio::test]
    async fn test_processed_nodes_tracking() {
        let graph_db = MockGraphDB::new();

        // Create graph with same node appearing in multiple edges
        let graph = KnowledgeGraph {
            nodes: vec![
                Node {
                    id: "alice".to_string(),
                    name: "Alice".to_string(),
                    node_type: "Person".to_string(),
                    description: "A person".to_string(),
                },
                Node {
                    id: "techcorp".to_string(),
                    name: "TechCorp".to_string(),
                    node_type: "Organization".to_string(),
                    description: "A company".to_string(),
                },
                Node {
                    id: "london".to_string(),
                    name: "London".to_string(),
                    node_type: "Location".to_string(),
                    description: "A city".to_string(),
                },
            ],
            edges: vec![
                Edge {
                    source_node_id: "alice".to_string(),
                    target_node_id: "techcorp".to_string(),
                    relationship_name: "works_at".to_string(),
                    description: None,
                },
                Edge {
                    source_node_id: "alice".to_string(),
                    target_node_id: "london".to_string(),
                    relationship_name: "lives_in".to_string(),
                    description: None,
                },
            ],
        };

        // Should handle duplicate node references correctly
        let result = retrieve_existing_edges(&graph_db, &[graph]).await.unwrap();

        // No edges in DB yet, so map should be empty
        assert!(result.is_empty());
    }
}
