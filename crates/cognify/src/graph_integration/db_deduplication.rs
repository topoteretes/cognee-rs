//! Database-backed graph deduplication utilities.
//!
//! Provides the existence check that keeps edges an earlier run already wrote
//! out of this run's graph write.

use std::collections::{HashMap, HashSet};

use cognee_graph::{EdgeData, GraphDBTrait};
use uuid::Uuid;

use crate::error::CognifyError;
use crate::graph_integration::types::GraphEdgePair;

/// An edge's stored identity: `(source_entity_id, target_entity_id,
/// relationship_name)`, exactly as [`GraphEdgePair::dedup_key`] returns it.
type EdgeIdentity = (Uuid, Uuid, String);

/// The distinct identities of `edges`, in first-seen order, as `has_edges`
/// candidates.
///
/// Chunk-level extraction routinely re-derives the same relation from several
/// chunks of one document, and on a backend without a set-based existence
/// check each repeat used to cost its own round-trip. De-duplicating here can
/// only remove redundant lookups: the answer is folded into a set of
/// identities, so a repeated candidate could never have changed the outcome.
fn collect_distinct_candidate_edges(edges: &[GraphEdgePair]) -> Vec<EdgeData> {
    let mut seen: HashSet<EdgeIdentity> = HashSet::new();
    let mut edges_to_check: Vec<EdgeData> = Vec::new();

    for edge in edges {
        // `insert` reports novelty, so this hashes the key once instead of
        // twice (`contains` then `insert`).
        if seen.insert(edge.dedup_key()) {
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

/// Split the edges an expansion pass produced into the ones to write and the
/// ones the graph database already holds.
///
/// Port of Python's `find_existing_edge_identities` +
/// `attach_new_edges_to_data_points` (`retrieve_existing_edges.py`,
/// `expand_with_nodes_and_edges.py:346-366`): the lookup is made on the
/// identity each edge **is stored under** — its resolved endpoint entity ids
/// and its `generate_edge_name`-normalized relationship, both decided by
/// [`expand_extracted_graphs`](super::expand_extracted_graphs) — not on a key
/// derived separately from the raw extraction. A separate derivation has to
/// repeat every transform expansion applies (name-derived entity ids, same-name
/// disambiguation, name-alias resolution, ontology canonicalization, timestamp
/// reversal, relationship normalization) and silently stops matching the first
/// time one is missed (#75, #76).
///
/// # Returns
/// `(new_edges, existing_edges)`, each in input order.
///
/// `existing_edges` must NOT be written to the graph again — that is what this
/// check is for — but they do need ownership rows: otherwise the only claim on
/// such an edge belongs to the run that first created it, and deleting *that*
/// run's data item finds the edge exclusively owned and deletes it from under
/// every later file that references it. A caller that drops this list
/// re-creates, across runs, the cross-file damage per-producer ownership rows
/// exist to prevent. The claim is dataset-scoped, like every other ownership
/// row: exclusivity is decided by `get_unique_edges_for_data` within one
/// dataset, so an edge shared between two *datasets* stays unprotected in
/// either direction.
///
/// `existing_edges` holds one edge per identity. An extracted edge and an
/// ontology edge can share one, and the claim list is turned into ownership
/// rows with no further de-duplication, so a repeat would put the same row id
/// in one upsert batch twice. `new_edges` is left as is: it goes through
/// [`deduplicate_nodes_and_edges`](super::deduplicate_nodes_and_edges) before
/// anything is written.
pub async fn partition_existing_edges(
    graph_db: &dyn GraphDBTrait,
    edges: Vec<GraphEdgePair>,
) -> Result<(Vec<GraphEdgePair>, Vec<GraphEdgePair>), CognifyError> {
    let edges_to_check = collect_distinct_candidate_edges(&edges);

    if edges_to_check.is_empty() {
        return Ok((edges, Vec::new()));
    }

    // Batch query graph database for existing edges
    let stored: HashSet<(String, String, String)> = graph_db
        .has_edges(&edges_to_check)
        .await
        .map_err(|e| CognifyError::GraphDatabaseError(e.to_string()))?
        .into_iter()
        .map(|(source_id, target_id, relationship_name, _)| {
            (source_id, target_id, relationship_name)
        })
        .collect();

    let mut new_edges = Vec::new();
    let mut existing_edges = Vec::new();
    let mut claimed: HashSet<EdgeIdentity> = HashSet::new();
    for edge in edges {
        let identity = (
            edge.source_entity_id.to_string(),
            edge.target_entity_id.to_string(),
            edge.relationship_name.clone(),
        );
        if !stored.contains(&identity) {
            new_edges.push(edge);
        } else if claimed.insert(edge.dedup_key()) {
            existing_edges.push(edge);
        }
    }

    Ok((new_edges, existing_edges))
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
    use crate::graph_integration::expand_extracted_graphs;
    use cognee_graph::MockGraphDB;
    use cognee_models::Entity;
    use cognee_ontology::traits::OntologySubgraph;
    use cognee_ontology::{
        AttachedOntologyNode, NoOpOntologyResolver, NodeCategory, OntologyResolver, OntologyResult,
    };

    fn node(id: &str, name: &str, node_type: &str) -> Node {
        Node {
            id: id.to_string(),
            name: name.to_string(),
            node_type: node_type.to_string(),
            description: format!("{name}."),
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

    /// `alice -relationship-> techcorp`, with caller-chosen node ids.
    fn works_at_graph(alice_id: &str, techcorp_id: &str, relationship: &str) -> KnowledgeGraph {
        KnowledgeGraph {
            nodes: vec![
                node(alice_id, "Alice", "Person"),
                node(techcorp_id, "TechCorp", "Organization"),
            ],
            edges: vec![edge(alice_id, techcorp_id, relationship)],
        }
    }

    fn pair(source: &str, target: &str, relationship: &str) -> GraphEdgePair {
        GraphEdgePair::new(Entity::id_for(source), Entity::id_for(target), relationship)
    }

    async fn store(graph_db: &MockGraphDB, edges: &[GraphEdgePair]) {
        let rows: Vec<EdgeData> = edges
            .iter()
            .map(|edge| {
                (
                    edge.source_entity_id.to_string(),
                    edge.target_entity_id.to_string(),
                    edge.relationship_name.clone(),
                    HashMap::new(),
                )
            })
            .collect();
        graph_db.add_edges(&rows).await.unwrap();
    }

    async fn expand(graph: KnowledgeGraph, resolver: &dyn OntologyResolver) -> Vec<GraphEdgePair> {
        expand_extracted_graphs(
            vec![(Uuid::new_v4(), graph)],
            Uuid::new_v4(),
            &HashMap::new(),
            &HashMap::new(),
            resolver,
            None,
            None,
        )
        .await
        .edges
    }

    /// Run 1 stores what it expanded; run 2 expands its own extraction and
    /// partitions it against the graph run 1 left. Returns run 2's
    /// `(new_edges, existing_edges)`.
    async fn second_run(
        first: KnowledgeGraph,
        second: KnowledgeGraph,
        resolver: &dyn OntologyResolver,
    ) -> (Vec<GraphEdgePair>, Vec<GraphEdgePair>) {
        let graph_db = MockGraphDB::new();
        let (first_new, _) = partition_existing_edges(&graph_db, expand(first, resolver).await)
            .await
            .unwrap();
        store(&graph_db, &first_new).await;
        partition_existing_edges(&graph_db, expand(second, resolver).await)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn partition_of_no_edges_is_empty() {
        let graph_db = MockGraphDB::new();
        let (new, existing) = partition_existing_edges(&graph_db, Vec::new())
            .await
            .unwrap();
        assert!(new.is_empty());
        assert!(existing.is_empty());
    }

    #[tokio::test]
    async fn partition_against_an_empty_graph_keeps_every_edge_new() {
        let graph_db = MockGraphDB::new();
        let edges = vec![
            pair("alice", "techcorp", "works_at"),
            pair("alice", "london", "lives_in"),
        ];

        let (new, existing) = partition_existing_edges(&graph_db, edges.clone())
            .await
            .unwrap();

        assert_eq!(new, edges);
        assert!(existing.is_empty());
    }

    #[tokio::test]
    async fn partition_routes_only_stored_identities_to_the_claim_list() {
        let graph_db = MockGraphDB::new();
        let stored = pair("alice", "techcorp", "works_at");
        store(&graph_db, std::slice::from_ref(&stored)).await;

        let fresh = pair("bob", "acmecorp", "works_at");
        let (new, existing) =
            partition_existing_edges(&graph_db, vec![stored.clone(), fresh.clone()])
                .await
                .unwrap();

        assert_eq!(new, vec![fresh]);
        assert_eq!(existing, vec![stored]);
    }

    /// The whole identity must match: a stored edge sharing only its endpoints
    /// or only its relationship with a candidate does not make it existing.
    #[tokio::test]
    async fn partition_matches_on_the_whole_identity() {
        let graph_db = MockGraphDB::new();
        store(&graph_db, &[pair("alice", "techcorp", "works_at")]).await;

        let candidates = vec![
            pair("alice", "techcorp", "founded"),
            pair("alice", "london", "works_at"),
        ];
        let (new, existing) = partition_existing_edges(&graph_db, candidates.clone())
            .await
            .unwrap();

        assert_eq!(new, candidates);
        assert!(existing.is_empty());
    }

    /// An extracted edge and an ontology edge can share an identity; the claim
    /// list turns into ownership rows with no further de-duplication, so it
    /// keeps one of them.
    #[tokio::test]
    async fn partition_claims_each_stored_identity_once() {
        let graph_db = MockGraphDB::new();
        let stored = pair("alice", "techcorp", "works_at");
        store(&graph_db, std::slice::from_ref(&stored)).await;

        let (new, existing) =
            partition_existing_edges(&graph_db, vec![stored.clone(), stored.clone()])
                .await
                .unwrap();

        assert!(new.is_empty());
        assert_eq!(existing.len(), 1);
    }

    /// #75: the second run's `Works At` is the stored `works_at`.
    #[tokio::test]
    async fn re_extraction_with_a_case_variant_relationship_finds_the_stored_edge() {
        let (new, existing) = second_run(
            works_at_graph("alice", "techcorp", "works_at"),
            works_at_graph("alice", "techcorp", "Works At"),
            &NoOpOntologyResolver::new(),
        )
        .await;

        assert!(new.is_empty(), "nothing new to write: {new:?}");
        assert_eq!(existing.len(), 1);
        assert_eq!(existing[0].relationship_name, "works_at");
    }

    /// #76, default path: entity ids derive from the node *name*, so node ids
    /// that differ from the names still resolve to the stored edge.
    #[tokio::test]
    async fn re_extraction_under_different_node_ids_finds_the_stored_edge() {
        let (new, existing) = second_run(
            works_at_graph("alice", "techcorp", "works_at"),
            works_at_graph("person_1", "org_7", "works_at"),
            &NoOpOntologyResolver::new(),
        )
        .await;

        assert!(new.is_empty(), "nothing new to write: {new:?}");
        assert_eq!(existing.len(), 1);
        assert_eq!(
            existing[0].source_entity_id,
            Entity::id_for("Alice"),
            "the endpoint is the name-derived entity id"
        );
    }

    /// #76, name-alias path: an edge that names its endpoint instead of using
    /// the node id resolves through the name fallback to the same entity.
    #[tokio::test]
    async fn re_extraction_with_a_name_referenced_endpoint_finds_the_stored_edge() {
        let mut second = works_at_graph("p1", "o1", "works_at");
        second.edges = vec![edge("Alice", "o1", "works_at")];

        let (new, existing) = second_run(
            works_at_graph("alice", "techcorp", "works_at"),
            second,
            &NoOpOntologyResolver::new(),
        )
        .await;

        assert!(new.is_empty(), "nothing new to write: {new:?}");
        assert_eq!(existing.len(), 1);
    }

    /// Canonicalizes the individual `Einstein` to `albert_einstein`, a name
    /// `Entity::id_for` does not equate with the extracted one.
    struct EinsteinOntology;

    impl OntologyResolver for EinsteinOntology {
        fn find_closest_match(&self, name: &str, category: &str) -> OntologyResult<Option<String>> {
            Ok((name == "Einstein" && category == "individuals")
                .then(|| "albert_einstein".to_string()))
        }

        fn get_subgraph(
            &self,
            node_name: &str,
            node_type: &str,
            _directed: bool,
        ) -> OntologyResult<OntologySubgraph> {
            let root = (node_name == "Einstein" && node_type == "individuals").then(|| {
                AttachedOntologyNode {
                    uri: "http://test.org#albert_einstein".to_string(),
                    name: "albert_einstein".to_string(),
                    category: NodeCategory::Individuals,
                }
            });
            Ok((vec![], vec![], root))
        }

        fn is_loaded(&self) -> bool {
            true
        }
    }

    /// #76, ontology path: the stored endpoint is the *canonical* entity id,
    /// which no hash of the extracted node id or name reproduces.
    #[tokio::test]
    async fn re_extraction_of_an_ontology_canonicalized_endpoint_finds_the_stored_edge() {
        let graph = || KnowledgeGraph {
            nodes: vec![
                node("einstein", "Einstein", "Person"),
                node("relativity", "Relativity", "Theory"),
            ],
            edges: vec![edge("einstein", "relativity", "developed")],
        };

        let (new, existing) = second_run(graph(), graph(), &EinsteinOntology).await;

        assert!(new.is_empty(), "nothing new to write: {new:?}");
        assert_eq!(existing.len(), 1);
        assert_eq!(
            existing[0].source_entity_id,
            Entity::id_for("albert_einstein")
        );
        assert_ne!(
            existing[0].source_entity_id,
            Entity::id_for("einstein"),
            "the case only means something if canonicalization changed the id"
        );
    }

    /// A relation re-extracted from several chunks is looked up once. The
    /// candidate list is what reaches `has_edges`, so this is what bounds the
    /// query count on backends without a set-based existence check.
    #[test]
    fn collect_distinct_candidate_edges_dedups_repeated_identities() {
        let edges = vec![
            pair("alice", "techcorp", "works_at"),
            pair("alice", "techcorp", "works_at"),
            pair("alice", "techcorp", "works_at"),
        ];
        let candidates = collect_distinct_candidate_edges(&edges);
        assert_eq!(candidates.len(), 1, "identical identities collapse to one");

        assert_eq!(candidates[0].0, Entity::id_for("alice").to_string());
        assert_eq!(candidates[0].1, Entity::id_for("techcorp").to_string());
        assert_eq!(candidates[0].2, "works_at");
    }

    /// Dedup keys on the whole identity, so edges sharing only an endpoint or
    /// only a relationship name are all kept.
    #[test]
    fn collect_distinct_candidate_edges_keeps_distinct_identities() {
        let edges = vec![
            pair("alice", "techcorp", "works_at"),
            // same endpoints, different relationship
            pair("alice", "techcorp", "founded"),
            // same relationship, different target
            pair("alice", "london", "works_at"),
            // exact repeat of the first — the only one that drops
            pair("alice", "techcorp", "works_at"),
        ];

        let candidates = collect_distinct_candidate_edges(&edges);
        assert_eq!(candidates.len(), 3);
        // First-seen order is preserved.
        assert_eq!(candidates[0].2, "works_at");
        assert_eq!(candidates[1].2, "founded");
        assert_eq!(candidates[2].2, "works_at");
        assert_ne!(candidates[0].1, candidates[2].1);
    }

    #[test]
    fn collect_distinct_candidate_edges_handles_no_edges() {
        assert!(collect_distinct_candidate_edges(&[]).is_empty());
    }
}
