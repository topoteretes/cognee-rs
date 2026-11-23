//! Database-backed graph deduplication utilities.
//!
//! Port of Python's `find_existing_edge_identities`
//! (`cognee/modules/graph/utils/retrieve_existing_edges.py`): which of the
//! edges an expansion built the graph database already holds, looked up on
//! the edges' final ids and normalised labels.

use std::collections::{HashMap, HashSet};

use cognee_graph::{EdgeData, GraphDBTrait};
use tracing::warn;
use uuid::Uuid;

use crate::error::CognifyError;
use crate::graph_integration::types::GraphEdgePair;

/// The distinct (source, target, label) keys of `edges`, in first-seen order,
/// as `has_edges` takes them.
///
/// An expansion's edge list can hold one key twice (an extracted edge and an
/// ontology edge between the same nodes), and on a backend without a
/// set-based existence check each repeat would cost its own round-trip.
fn collect_distinct_candidate_edges<'a>(
    edges: impl IntoIterator<Item = &'a GraphEdgePair>,
) -> Vec<EdgeData> {
    let mut seen: HashSet<(Uuid, Uuid, &str)> = HashSet::new();
    let mut edges_to_check: Vec<EdgeData> = Vec::new();
    for edge in edges {
        if seen.insert((
            edge.source_entity_id,
            edge.target_entity_id,
            edge.relationship_name.as_str(),
        )) {
            edges_to_check.push((
                edge.source_entity_id.to_string(),
                edge.target_entity_id.to_string(),
                edge.relationship_name.clone(),
                HashMap::new(), // Properties not needed for existence check
            ));
        }
    }
    edges_to_check
}

/// Which of `edges` the graph database already holds.
///
/// Python's `find_existing_edge_identities`: the candidates are the edges
/// expansion built, so they carry the **resolved** ids — the name-based
/// entity id, a Timestamp's id, an ontology-canonicalised entity's id, a
/// same-name node's chunk-scoped id — and the normalised label, exactly as
/// the graph stores them. Pass the result to
/// [`crate::graph_integration::Expansion::split_existing_edges`].
///
/// Returns the [`GraphEdgePair::dedup_key`] of every existing edge.
///
/// # Example
/// ```ignore
/// let mut expansion = expand_extracted_graphs_by_unit(/* … */).await?;
/// let existing = retrieve_existing_edges(graph_db.as_ref(), &expansion.edges).await?;
/// expansion.split_existing_edges(&existing);
/// ```
pub async fn retrieve_existing_edges<'a>(
    graph_db: &dyn GraphDBTrait,
    edges: impl IntoIterator<Item = &'a GraphEdgePair>,
) -> Result<HashSet<(Uuid, Uuid, String)>, CognifyError> {
    let edges_to_check = collect_distinct_candidate_edges(edges);
    if edges_to_check.is_empty() {
        return Ok(HashSet::new());
    }

    let existing_edges = graph_db
        .has_edges(&edges_to_check)
        .await
        .map_err(|e| CognifyError::GraphDatabaseError(e.to_string()))?;

    let (existing_edge_keys, unparsable) = edge_keys_of(existing_edges);
    if unparsable > 0 {
        // Every candidate id is a UUID, so a backend that answers in another
        // form makes every such edge look new: it is written and embedded
        // again on every run. Once per call, not per edge.
        warn!(
            unparsable,
            "has_edges returned {unparsable} edge(s) whose endpoint ids are not UUIDs; \
             they are treated as not in the graph and will be written again"
        );
    }
    Ok(existing_edge_keys)
}

/// The [`GraphEdgePair::dedup_key`] of every edge `has_edges` returned, and
/// how many it skipped because an endpoint id does not parse as a UUID.
fn edge_keys_of(edges: Vec<EdgeData>) -> (HashSet<(Uuid, Uuid, String)>, usize) {
    let mut keys = HashSet::with_capacity(edges.len());
    let mut unparsable = 0;
    for (source_id, target_id, relationship_name, _) in edges {
        match (Uuid::parse_str(&source_id), Uuid::parse_str(&target_id)) {
            (Ok(source), Ok(target)) => {
                keys.insert((source, target, relationship_name));
            }
            _ => unparsable += 1,
        }
    }
    (keys, unparsable)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use crate::fact_extraction::{Edge, KnowledgeGraph, Node};
    use crate::graph_integration::expand_with_nodes_and_edges;
    use cognee_graph::MockGraphDB;
    use cognee_models::Entity;
    use cognee_ontology::NoOpOntologyResolver;

    fn node(id: &str, name: &str) -> Node {
        Node {
            id: id.to_string(),
            name: name.to_string(),
            node_type: "Thing".to_string(),
            description: String::new(),
        }
    }

    fn edge(source: &str, target: &str, relationship: &str) -> Edge {
        Edge {
            source_node_id: source.to_string(),
            target_node_id: target.to_string(),
            relationship_name: relationship.to_string(),
            description: None,
        }
    }

    /// The edges expansion builds from `graph`, on their resolved ids.
    async fn built_edges(graph: KnowledgeGraph) -> Vec<GraphEdgePair> {
        let (_nodes, edges, _claimed, _producers) = expand_with_nodes_and_edges(
            vec![(Uuid::new_v4(), graph)],
            Uuid::new_v4(),
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            &NoOpOntologyResolver::new(),
            None,
            None,
        )
        .await;
        edges
    }

    /// `alice` named "Alice Smith" works at `techcorp`: the node id is not the
    /// snake-case form of the name, so the entity id (from the name) and an
    /// id derived from the node id differ.
    fn alice_works_at_techcorp() -> KnowledgeGraph {
        KnowledgeGraph {
            nodes: vec![node("alice", "Alice Smith"), node("techcorp", "TechCorp")],
            edges: vec![edge("alice", "techcorp", "works_at")],
        }
    }

    async fn seed(graph_db: &MockGraphDB, source: Uuid, target: Uuid, relationship: &str) {
        graph_db
            .add_edge(&source.to_string(), &target.to_string(), relationship, None)
            .await
            .unwrap();
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
        let edges = built_edges(alice_works_at_techcorp()).await;

        let result = retrieve_existing_edges(&graph_db, &edges).await.unwrap();

        assert!(result.is_empty());
    }

    /// The database holds the edge under the entity ids cognify stores, which
    /// come from the node *names*. Seeding it under ids derived from the node
    /// ids, as this test once did, could not catch a check that built its
    /// keys from the node ids (SDK-1694 finding 5).
    #[tokio::test]
    async fn test_retrieve_existing_edges_with_existing() {
        let graph_db = MockGraphDB::new();
        let alice = Entity::id_for("Alice Smith");
        let techcorp = Entity::id_for("TechCorp");
        seed(&graph_db, alice, techcorp, "works_at").await;
        // An edge under the node-id-derived id is a different edge.
        seed(&graph_db, Entity::id_for("alice"), techcorp, "employs").await;

        let edges = built_edges(alice_works_at_techcorp()).await;
        let result = retrieve_existing_edges(&graph_db, &edges).await.unwrap();

        assert_eq!(
            result,
            HashSet::from([(alice, techcorp, "works_at".to_string())])
        );
    }

    #[tokio::test]
    async fn test_retrieve_existing_edges_partial_match() {
        let graph_db = MockGraphDB::new();
        let graph = KnowledgeGraph {
            nodes: vec![
                node("alice", "Alice Smith"),
                node("techcorp", "TechCorp"),
                node("bob", "Bob"),
                node("acmecorp", "AcmeCorp"),
            ],
            edges: vec![
                edge("alice", "techcorp", "works_at"),
                edge("bob", "acmecorp", "works_at"),
            ],
        };
        let alice = Entity::id_for("Alice Smith");
        let techcorp = Entity::id_for("TechCorp");
        seed(&graph_db, alice, techcorp, "works_at").await;

        let edges = built_edges(graph).await;
        let result = retrieve_existing_edges(&graph_db, &edges).await.unwrap();

        assert_eq!(
            result,
            HashSet::from([(alice, techcorp, "works_at".to_string())])
        );
    }

    /// A key on the list twice is looked up once. The candidate list is what
    /// reaches `has_edges`, so this bounds the query count on backends without
    /// a set-based existence check.
    #[test]
    fn collect_distinct_candidate_edges_dedups_repeated_keys() {
        let (alice, techcorp) = (Uuid::new_v4(), Uuid::new_v4());
        let edges = vec![
            GraphEdgePair::new(alice, techcorp, "works_at"),
            GraphEdgePair::new(alice, techcorp, "works_at"),
        ];
        let candidates = collect_distinct_candidate_edges(&edges);
        assert_eq!(candidates.len(), 1, "identical keys collapse to one");
        assert_eq!(candidates[0].0, alice.to_string());
        assert_eq!(candidates[0].1, techcorp.to_string());
        assert_eq!(candidates[0].2, "works_at");
    }

    /// Dedup keys on the whole triple, so edges sharing only an endpoint or
    /// only a label are all kept, in first-seen order.
    #[test]
    fn collect_distinct_candidate_edges_keeps_distinct_keys() {
        let (alice, techcorp, london) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let edges = vec![
            GraphEdgePair::new(alice, techcorp, "works_at"),
            GraphEdgePair::new(alice, techcorp, "founded"),
            GraphEdgePair::new(alice, london, "works_at"),
            GraphEdgePair::new(alice, techcorp, "works_at"),
        ];
        let candidates = collect_distinct_candidate_edges(&edges);
        let labels: Vec<&str> = candidates.iter().map(|c| c.2.as_str()).collect();
        assert_eq!(labels, ["works_at", "founded", "works_at"]);
        assert_eq!(candidates[2].1, london.to_string());
    }

    #[test]
    fn collect_distinct_candidate_edges_handles_no_edges() {
        assert!(collect_distinct_candidate_edges(&[]).is_empty());
    }

    /// An answer whose endpoint ids are not UUIDs is counted for the warning
    /// and left out, never a panic; the parsable edges are still keys, in
    /// any UUID spelling.
    #[test]
    fn edge_keys_of_counts_endpoints_that_are_not_uuids() {
        let (source, target) = (Uuid::new_v4(), Uuid::new_v4());
        let answer: Vec<EdgeData> = vec![
            (
                source.to_string().to_uppercase(),
                target.simple().to_string(),
                "works_at".to_string(),
                HashMap::new(),
            ),
            (
                "alice".to_string(),
                target.to_string(),
                "works_at".to_string(),
                HashMap::new(),
            ),
            (
                source.to_string(),
                String::new(),
                "works_at".to_string(),
                HashMap::new(),
            ),
        ];

        let (keys, unparsable) = edge_keys_of(answer);

        assert_eq!(
            keys,
            HashSet::from([(source, target, "works_at".to_string())])
        );
        assert_eq!(unparsable, 2);
    }
}
