#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! `neighborhood_depth` / `neighborhood_seed_top_k` on graph triplet search —
//! Python's neighborhood projection (`get_memory_fragment` +
//! `_get_top_triplet_importances` in `brute_force_triplet_search.py`).
//!
//! Run with:
//!   cargo test --package cognee-search --test graph_neighborhood_projection -- --nocapture

use std::collections::HashMap;

use async_trait::async_trait;
use cognee_embedding::{EmbeddingEngine, EmbeddingResult};
use cognee_graph::{GraphDBTrait, MockGraphDB};
use cognee_search::graph_retrieval::{
    DEFAULT_TRIPLET_DISTANCE_PENALTY, GraphRetrievalConfig, RankedGraphEdge,
    brute_force_triplet_search,
};
use cognee_search::types::SearchError;
use cognee_vector::{MockVectorDB, SearchResult, VectorDB, VectorDBResult, VectorPoint};
use serde_json::json;
use uuid::Uuid;

const PENALTY: f32 = DEFAULT_TRIPLET_DISTANCE_PENALTY;

/// Every element distance is scaled by `2 - importance_weight`; no fixture
/// node carries a weight, so it is Python's default 0.5.
const SCALE: f32 = 1.5;

/// Every query embeds to `[1, 0]`.
struct AlignedEmbedding;

#[async_trait]
impl EmbeddingEngine for AlignedEmbedding {
    async fn embed(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
    }
    fn dimension(&self) -> usize {
        2
    }
    fn batch_size(&self) -> usize {
        8
    }
    fn max_sequence_length(&self) -> usize {
        128
    }
}

/// Fails the test if the search embeds the query at all.
struct UnusedEmbedding;

#[async_trait]
impl EmbeddingEngine for UnusedEmbedding {
    async fn embed(&self, _texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
        panic!("the query must not be embedded");
    }
    fn dimension(&self) -> usize {
        2
    }
    fn batch_size(&self) -> usize {
        8
    }
    fn max_sequence_length(&self) -> usize {
        128
    }
}

/// A [`MockVectorDB`] that does not implement `score_by_ids`, like an
/// external adapter written before it existed.
struct NoScoreById(MockVectorDB);

#[async_trait]
impl VectorDB for NoScoreById {
    async fn create_collection(&self, d: &str, f: &str, dim: usize) -> VectorDBResult<()> {
        self.0.create_collection(d, f, dim).await
    }
    async fn has_collection(&self, d: &str, f: &str) -> VectorDBResult<bool> {
        self.0.has_collection(d, f).await
    }
    async fn index_points(&self, d: &str, f: &str, points: &[VectorPoint]) -> VectorDBResult<()> {
        self.0.index_points(d, f, points).await
    }
    async fn search_similar(
        &self,
        d: &str,
        f: &str,
        query_vector: &[f32],
        top_k: usize,
    ) -> VectorDBResult<Vec<SearchResult>> {
        self.0.search_similar(d, f, query_vector, top_k).await
    }
    async fn delete_collection(&self, d: &str, f: &str) -> VectorDBResult<()> {
        self.0.delete_collection(d, f).await
    }
    async fn retrieve(&self, d: &str, f: &str, ids: &[Uuid]) -> VectorDBResult<Vec<SearchResult>> {
        self.0.retrieve(d, f, ids).await
    }
    async fn collection_size(&self, d: &str, f: &str) -> VectorDBResult<usize> {
        self.0.collection_size(d, f).await
    }
}

async fn entity_vectors(points: &[(Uuid, [f32; 2])]) -> MockVectorDB {
    let vector_db = MockVectorDB::new();
    vector_db
        .create_collection("Entity", "name", 2)
        .await
        .unwrap();
    let points: Vec<VectorPoint> = points
        .iter()
        .map(|(id, vector)| {
            VectorPoint::new(*id, vector.to_vec()).with_metadata("id", json!(id.to_string()))
        })
        .collect();
    vector_db
        .index_points("Entity", "name", &points)
        .await
        .unwrap();
    vector_db
}

async fn graph(nodes: &[Uuid], edges: &[(Uuid, Uuid, &str)]) -> MockGraphDB {
    let graph = MockGraphDB::new();
    for id in nodes {
        graph
            .add_node_raw(json!({ "id": id.to_string(), "name": id.to_string() }))
            .await
            .unwrap();
    }
    for (src, tgt, rel) in edges {
        graph
            .add_edge(&src.to_string(), &tgt.to_string(), rel, None)
            .await
            .unwrap();
    }
    graph
}

/// The chain `s → x1 → x2 → x3`. Only `s` is a vector-search hit
/// (`wide_search_top_k = 1`); `x2` is also indexed, at cosine distance 0.4,
/// so only the expansion re-scoring can give it a real distance.
struct Chain {
    s: Uuid,
    x1: Uuid,
    x2: Uuid,
    x3: Uuid,
}

async fn chain() -> (MockVectorDB, MockGraphDB, Chain) {
    let c = Chain {
        s: Uuid::new_v4(),
        x1: Uuid::new_v4(),
        x2: Uuid::new_v4(),
        x3: Uuid::new_v4(),
    };
    let vector_db = entity_vectors(&[(c.s, [1.0, 0.0]), (c.x2, [0.6, 0.8])]).await;
    let graph = graph(
        &[c.s, c.x1, c.x2, c.x3],
        &[
            (c.s, c.x1, "s_x1"),
            (c.x1, c.x2, "x1_x2"),
            (c.x2, c.x3, "x2_x3"),
        ],
    )
    .await;
    (vector_db, graph, c)
}

fn config(depth: Option<usize>) -> GraphRetrievalConfig {
    GraphRetrievalConfig {
        top_k: 100,
        wide_search_top_k: 1,
        neighborhood_depth: depth,
        ..Default::default()
    }
}

async fn search(
    vector_db: &dyn VectorDB,
    graph: &MockGraphDB,
    cfg: &GraphRetrievalConfig,
) -> Result<Vec<RankedGraphEdge>, SearchError> {
    brute_force_triplet_search("q", vector_db, &AlignedEmbedding, graph, cfg).await
}

fn scores_by_relationship(ranked: &[RankedGraphEdge]) -> HashMap<String, f32> {
    ranked
        .iter()
        .map(|edge| (edge.relationship_name.clone(), edge.score))
        .collect()
}

fn assert_close(actual: f32, expected: f32, what: &str) {
    assert!(
        (actual - expected).abs() < 1e-4,
        "{what}: expected {expected}, got {actual}"
    );
}

/// Without a depth the one-hop, seed-touching load is unchanged.
#[tokio::test]
async fn without_depth_only_seed_edges_rank() {
    let (vector_db, graph, _) = chain().await;
    let ranked = search(&vector_db, &graph, &config(None)).await.unwrap();
    let rels: Vec<&str> = ranked
        .iter()
        .map(|edge| edge.relationship_name.as_str())
        .collect();
    assert_eq!(rels, vec!["s_x1"]);
}

/// Depth 2 surfaces `x1 → x2`, an edge with no seed endpoint that the default
/// path can never return, and gives the expanded node `x2` its real distance.
/// `x2 → x3` is three hops out and stays unloaded.
#[tokio::test]
async fn depth_two_ranks_every_projected_edge_with_rescored_expansion() {
    let (vector_db, graph, _) = chain().await;
    let ranked = search(&vector_db, &graph, &config(Some(2))).await.unwrap();
    let scores = scores_by_relationship(&ranked);

    assert_eq!(scores.len(), 2, "got {scores:?}");
    // s is an exact hit (0); x1 is in no collection and the edge type has no
    // vector, so both take the penalty.
    assert_close(scores["s_x1"], SCALE * 2.0 * PENALTY, "s_x1");
    // x2 was not a top-1 hit, so only `score_by_ids` gives it 1 - 0.6 = 0.4.
    assert_close(scores["x1_x2"], SCALE * (0.4 + 2.0 * PENALTY), "x1_x2");
    assert_eq!(
        ranked[0].relationship_name, "s_x1",
        "the seed edge stays first"
    );
}

/// An adapter without `score_by_ids` still projects the neighborhood; the
/// expanded nodes keep the penalty, as Python does on `NotImplementedError`.
#[tokio::test]
async fn adapter_without_score_by_ids_leaves_expansion_at_the_penalty() {
    let (vector_db, graph, _) = chain().await;
    let ranked = search(&NoScoreById(vector_db), &graph, &config(Some(2)))
        .await
        .unwrap();
    let scores = scores_by_relationship(&ranked);
    assert_close(scores["x1_x2"], SCALE * 3.0 * PENALTY, "x1_x2");
}

/// `seed_top_k` keeps the *nearest* seeds, not the first by id: `far` sorts
/// first by id but is the farther hit, so only `near`'s neighborhood loads.
#[tokio::test]
async fn seed_top_k_keeps_the_nearest_seeds() {
    let far = Uuid::from_u128(1);
    let near = Uuid::from_u128(2);
    let far_n = Uuid::new_v4();
    let near_n = Uuid::new_v4();
    let vector_db = entity_vectors(&[(near, [1.0, 0.0]), (far, [0.8, 0.6])]).await;
    let graph = graph(
        &[far, near, far_n, near_n],
        &[(near, near_n, "near_edge"), (far, far_n, "far_edge")],
    )
    .await;

    let all = GraphRetrievalConfig {
        top_k: 100,
        neighborhood_depth: Some(1),
        ..Default::default()
    };
    let ranked = search(&vector_db, &graph, &all).await.unwrap();
    let mut rels: Vec<&str> = ranked
        .iter()
        .map(|edge| edge.relationship_name.as_str())
        .collect();
    rels.sort_unstable();
    assert_eq!(rels, vec!["far_edge", "near_edge"], "unset = every seed");

    let one = GraphRetrievalConfig {
        neighborhood_seed_top_k: Some(1),
        ..all
    };
    let ranked = search(&vector_db, &graph, &one).await.unwrap();
    let rels: Vec<&str> = ranked
        .iter()
        .map(|edge| edge.relationship_name.as_str())
        .collect();
    assert_eq!(rels, vec!["near_edge"]);
}

#[tokio::test]
async fn non_positive_depth_or_seed_count_is_rejected() {
    let (vector_db, graph, _) = chain().await;
    for (cfg, expected) in [
        (
            config(Some(0)),
            "neighborhood_depth must be a positive integer.",
        ),
        (
            GraphRetrievalConfig {
                neighborhood_seed_top_k: Some(0),
                ..config(Some(1))
            },
            "neighborhood_seed_top_k must be a positive integer.",
        ),
    ] {
        let error = search(&vector_db, &graph, &cfg).await.unwrap_err();
        assert!(
            matches!(&error, SearchError::InvalidInput(message) if message == expected),
            "got {error:?}"
        );
    }
}

/// `node_name` disables Python's seed search, so its neighborhood projection
/// has no seeds and the search comes back empty rather than failing. That is
/// known from the config alone, so neither the embedding nor a graph load runs.
#[tokio::test]
async fn depth_with_node_name_returns_empty() {
    let (vector_db, graph, _) = chain().await;
    let cfg = GraphRetrievalConfig {
        node_type: Some("Entity".to_string()),
        node_name: Some(vec!["anything".to_string()]),
        ..config(Some(2))
    };
    let ranked = brute_force_triplet_search("q", &vector_db, &UnusedEmbedding, &graph, &cfg)
        .await
        .unwrap();
    assert!(ranked.is_empty(), "got {ranked:?}");
    assert!(
        !graph
            .get_call_log()
            .iter()
            .any(|call| call == "get_nodeset_subgraph" || call == "get_neighborhood"),
        "no graph load should run; call log was {:?}",
        graph.get_call_log()
    );
}
