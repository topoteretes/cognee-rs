#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! `brute_force_triplet_search_batch` — Python's batch mode of
//! `brute_force_triplet_search(query_batch=...)`: one graph load, an unlimited
//! vector search per collection, one ranked list per query.
//!
//! Run with:
//!   cargo test --package cognee-search --test graph_triplet_search_batch -- --nocapture

use std::sync::Mutex;

use async_trait::async_trait;
use cognee_embedding::{EmbeddingEngine, EmbeddingResult};
use cognee_graph::{GraphDBTrait, MockGraphDB};
use cognee_search::graph_retrieval::{
    GraphRetrievalConfig, RankedGraphEdge, brute_force_triplet_search,
    brute_force_triplet_search_batch,
};
use cognee_search::types::SearchError;
use cognee_vector::{
    MockVectorDB, SearchResult, VectorDB, VectorDBError, VectorDBResult, VectorPoint,
};
use serde_json::json;
use uuid::Uuid;

/// `"x"` embeds to `[1, 0]`, anything else to `[0, 1]`.
struct KeyedEmbedding;

#[async_trait]
impl EmbeddingEngine for KeyedEmbedding {
    async fn embed(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|text| {
                if *text == "x" {
                    vec![1.0, 0.0]
                } else {
                    vec![0.0, 1.0]
                }
            })
            .collect())
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

/// Like [`KeyedEmbedding`], but only through `embed_queries`: the batch must
/// embed its queries in one call rather than one `embed_query` each.
struct BatchOnlyEmbedding;

#[async_trait]
impl EmbeddingEngine for BatchOnlyEmbedding {
    async fn embed(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
        KeyedEmbedding.embed(texts).await
    }
    async fn embed_query(&self, _query: &str) -> EmbeddingResult<Vec<Vec<f32>>> {
        panic!("batch search must embed through embed_queries");
    }
    async fn embed_queries(&self, queries: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
        KeyedEmbedding.embed(queries).await
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

/// How [`FaultyVectorDb`] misbehaves.
#[derive(Clone, Copy, PartialEq)]
enum Fault {
    None,
    /// `batch_search_similar` returns one result list fewer than queries.
    ShortBatch,
    /// `Entity` is dropped after `has_collection`: its size reads as missing.
    DroppedMidSearch,
}

/// A [`MockVectorDB`] that records which collections were searched and can
/// misbehave the way an external adapter or a concurrent prune might.
struct FaultyVectorDb {
    inner: MockVectorDB,
    fault: Fault,
    searched: Mutex<Vec<String>>,
}

#[async_trait]
impl VectorDB for FaultyVectorDb {
    async fn create_collection(&self, d: &str, f: &str, dim: usize) -> VectorDBResult<()> {
        self.inner.create_collection(d, f, dim).await
    }
    async fn has_collection(&self, d: &str, f: &str) -> VectorDBResult<bool> {
        self.inner.has_collection(d, f).await
    }
    async fn index_points(&self, d: &str, f: &str, points: &[VectorPoint]) -> VectorDBResult<()> {
        self.inner.index_points(d, f, points).await
    }
    async fn search_similar(
        &self,
        d: &str,
        f: &str,
        query_vector: &[f32],
        top_k: usize,
    ) -> VectorDBResult<Vec<SearchResult>> {
        self.inner.search_similar(d, f, query_vector, top_k).await
    }
    async fn batch_search_similar(
        &self,
        d: &str,
        f: &str,
        query_vectors: &[Vec<f32>],
        top_k: usize,
    ) -> VectorDBResult<Vec<Vec<SearchResult>>> {
        self.searched
            .lock()
            .unwrap() // lock poison is unrecoverable
            .push(format!("{d}_{f}"));
        let mut results = self
            .inner
            .batch_search_similar(d, f, query_vectors, top_k)
            .await?;
        if self.fault == Fault::ShortBatch {
            results.pop();
        }
        Ok(results)
    }
    async fn delete_collection(&self, d: &str, f: &str) -> VectorDBResult<()> {
        self.inner.delete_collection(d, f).await
    }
    async fn retrieve(&self, d: &str, f: &str, ids: &[Uuid]) -> VectorDBResult<Vec<SearchResult>> {
        self.inner.retrieve(d, f, ids).await
    }
    async fn collection_size(&self, d: &str, f: &str) -> VectorDBResult<usize> {
        if self.fault == Fault::DroppedMidSearch && d == "Entity" {
            return Err(VectorDBError::CollectionNotFound(format!("{d}_{f}")));
        }
        self.inner.collection_size(d, f).await
    }
}

/// The fixture's vectors plus a populated `Triplet_text` and `EdgeType` row,
/// wrapped to record and misbehave.
async fn faulty(fault: Fault) -> (FaultyVectorDb, MockGraphDB) {
    let (inner, graph) = fixture(true).await;
    for (data_type, field_name) in [("Triplet", "text"), ("EdgeType", "relationship_name")] {
        inner
            .create_collection(data_type, field_name, 2)
            .await
            .unwrap();
        inner
            .index_points(
                data_type,
                field_name,
                &[VectorPoint::new(Uuid::new_v4(), vec![1.0, 0.0])],
            )
            .await
            .unwrap();
    }
    let vector_db = FaultyVectorDb {
        inner,
        fault,
        searched: Mutex::new(Vec::new()),
    };
    (vector_db, graph)
}

/// The default 6.5 penalty scaled by `2 - 0.5` (the default importance).
const SCALED_PENALTY: f32 = 1.5 * 6.5;

const HUB: Uuid = Uuid::from_u128(0xF0);
/// Indexed at `[1, 0]`: the `"x"` query's exact match, distance 1 from `"y"`.
const A: Uuid = Uuid::from_u128(0x0A);
/// Indexed at `[0, 1]`: the `"y"` query's exact match, distance 1 from `"x"`.
const B: Uuid = Uuid::from_u128(0x0B);
/// An edge between two nodes no query can hit.
const E: Uuid = Uuid::from_u128(0x0E);
const F: Uuid = Uuid::from_u128(0x0F);

async fn fixture(with_far_edge: bool) -> (MockVectorDB, MockGraphDB) {
    let vector_db = MockVectorDB::new();
    vector_db
        .create_collection("Entity", "name", 2)
        .await
        .unwrap();
    vector_db
        .index_points(
            "Entity",
            "name",
            &[
                VectorPoint::new(A, vec![1.0, 0.0]).with_metadata("id", json!(A.to_string())),
                VectorPoint::new(B, vec![0.0, 1.0]).with_metadata("id", json!(B.to_string())),
            ],
        )
        .await
        .unwrap();

    let graph = MockGraphDB::new();
    for (id, name) in [(HUB, "H"), (A, "A"), (B, "B"), (E, "E"), (F, "F")] {
        graph
            .add_node_raw(json!({ "id": id.to_string(), "name": name }))
            .await
            .unwrap();
    }
    let mut edges = vec![(HUB, A, "to_a"), (HUB, B, "to_b")];
    if with_far_edge {
        edges.push((E, F, "e_f"));
    }
    for (source, target, rel) in edges {
        graph
            .add_edge(&source.to_string(), &target.to_string(), rel, None)
            .await
            .unwrap();
    }
    (vector_db, graph)
}

fn config() -> GraphRetrievalConfig {
    GraphRetrievalConfig {
        top_k: 10,
        // Batch mode ignores it: a limit of 1 would hide B from the "x" query.
        wide_search_top_k: 1,
        ..Default::default()
    }
}

fn ranking(edges: &[RankedGraphEdge]) -> Vec<(String, f32)> {
    edges
        .iter()
        .map(|edge| (edge.relationship_name.clone(), edge.score))
        .collect()
}

fn assert_ranking(edges: &[RankedGraphEdge], expected: &[(&str, f32)]) {
    let actual = ranking(edges);
    assert_eq!(actual.len(), expected.len(), "{actual:?}");
    for ((rel, score), (want_rel, want_score)) in actual.iter().zip(expected) {
        assert_eq!(rel, want_rel, "order: {actual:?}");
        assert!(
            (score - want_score).abs() < 1e-4,
            "{rel}: expected {want_score}, got {score} ({actual:?})"
        );
    }
}

/// N queries give N lists from one full-graph load. Every node has its real
/// distance to every query (no `wide_search_top_k` cut), and every loaded edge
/// is ranked — `e_f` touches no hit and still comes back, at the penalty.
#[tokio::test]
async fn one_graph_load_ranks_every_edge_per_query() {
    let (vector_db, graph) = fixture(true).await;
    let ranked = brute_force_triplet_search_batch(
        &["x", "y"],
        &vector_db,
        &KeyedEmbedding,
        &graph,
        &config(),
    )
    .await
    .unwrap();

    assert_eq!(ranked.len(), 2);
    let exact = 2.0 * SCALED_PENALTY;
    let one_off = 2.0 * SCALED_PENALTY + 1.5;
    let far = 3.0 * SCALED_PENALTY;
    assert_ranking(
        &ranked[0],
        &[("to_a", exact), ("to_b", one_off), ("e_f", far)],
    );
    assert_ranking(
        &ranked[1],
        &[("to_b", exact), ("to_a", one_off), ("e_f", far)],
    );

    let calls = graph.get_call_log();
    assert_eq!(
        calls
            .iter()
            .filter(|call| *call == "get_graph_data")
            .count(),
        1,
        "one projection for the whole batch; call log was {calls:?}"
    );
    assert!(
        !calls.iter().any(|call| call == "get_neighborhood"),
        "batch mode has no seed set to scope by; call log was {calls:?}"
    );
}

/// Where single-query search already sees every distance and every edge
/// touches a hit, each batch list equals that query's single-query result.
#[tokio::test]
async fn each_list_matches_the_single_query_result_on_the_same_graph() {
    let (vector_db, graph) = fixture(false).await;
    let wide = GraphRetrievalConfig {
        wide_search_top_k: 100,
        ..config()
    };
    let batch =
        brute_force_triplet_search_batch(&["x", "y"], &vector_db, &KeyedEmbedding, &graph, &wide)
            .await
            .unwrap();
    for (query, batch_ranking) in ["x", "y"].into_iter().zip(&batch) {
        let single = brute_force_triplet_search(query, &vector_db, &KeyedEmbedding, &graph, &wide)
            .await
            .unwrap();
        assert_eq!(ranking(batch_ranking), ranking(&single), "query {query}");
    }
}

#[tokio::test]
async fn an_empty_batch_or_a_blank_query_is_rejected() {
    let (vector_db, graph) = fixture(false).await;
    for (queries, expected) in [
        (&[][..], "query_batch must be a non-empty list of strings."),
        (
            &["x", "  "][..],
            "All items in query_batch must be non-empty strings.",
        ),
    ] {
        let error = brute_force_triplet_search_batch(
            queries,
            &vector_db,
            &KeyedEmbedding,
            &graph,
            &config(),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(&error, SearchError::InvalidInput(message) if message == expected),
            "got {error:?}"
        );
    }
}

/// Batch mode has no seed ids, so Python's neighborhood projection yields an
/// empty fragment: one empty list per query, and no graph load.
#[tokio::test]
async fn neighborhood_depth_yields_empty_lists() {
    let (vector_db, graph) = fixture(true).await;
    let cfg = GraphRetrievalConfig {
        neighborhood_depth: Some(2),
        ..config()
    };
    let ranked =
        brute_force_triplet_search_batch(&["x", "y"], &vector_db, &KeyedEmbedding, &graph, &cfg)
            .await
            .unwrap();
    assert_eq!(ranked.len(), 2);
    assert!(ranked.iter().all(Vec::is_empty), "{ranked:?}");
    assert!(
        graph.get_call_log().is_empty(),
        "{:?}",
        graph.get_call_log()
    );
}

/// No vector results for any query: empty lists, no graph load.
#[tokio::test]
async fn no_vector_results_yields_empty_lists() {
    let (_, graph) = fixture(true).await;
    let ranked = brute_force_triplet_search_batch(
        &["x", "y"],
        &MockVectorDB::new(),
        &KeyedEmbedding,
        &graph,
        &config(),
    )
    .await
    .unwrap();
    assert_eq!(ranked.len(), 2);
    assert!(ranked.iter().all(Vec::is_empty), "{ranked:?}");
    assert!(
        graph.get_call_log().is_empty(),
        "{:?}",
        graph.get_call_log()
    );
}

/// The batch embeds its queries in one `embed_queries` call, and never reads
/// `Triplet_text`: its ids are never graph nodes, so an unlimited read of it
/// could not change any ranking.
#[tokio::test]
async fn embeds_once_and_skips_the_triplet_collection() {
    let (vector_db, graph) = faulty(Fault::None).await;
    let ranked = brute_force_triplet_search_batch(
        &["x", "y"],
        &vector_db,
        &BatchOnlyEmbedding,
        &graph,
        &config(),
    )
    .await
    .unwrap();
    assert_eq!(ranked[0][0].relationship_name, "to_a");
    assert_eq!(ranked[1][0].relationship_name, "to_b");

    let searched = vector_db.searched.lock().unwrap().clone(); // lock poison is unrecoverable
    assert!(
        searched.contains(&"Entity_name".to_string()),
        "{searched:?}"
    );
    assert!(
        searched.contains(&"EdgeType_relationship_name".to_string()),
        "{searched:?}"
    );
    assert!(
        !searched.contains(&"Triplet_text".to_string()),
        "{searched:?}"
    );
}

/// One result list per query, or the distances would shift onto the wrong
/// query: a backend that returns fewer is an error, not a silent mismatch.
#[tokio::test]
async fn a_short_batch_result_is_an_error() {
    let (vector_db, graph) = faulty(Fault::ShortBatch).await;
    let error = brute_force_triplet_search_batch(
        &["x", "y"],
        &vector_db,
        &KeyedEmbedding,
        &graph,
        &config(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&error, SearchError::VectorError(message) if message.contains("1 result lists for 2 queries")),
        "got {error:?}"
    );
}

/// A collection dropped after `has_collection` counts as empty, as Python's
/// `_search_batch_collection` answers a `CollectionNotFoundError`: the batch
/// still ranks, every entity at the penalty.
#[tokio::test]
async fn a_collection_dropped_mid_search_counts_as_empty() {
    let (vector_db, graph) = faulty(Fault::DroppedMidSearch).await;
    let ranked = brute_force_triplet_search_batch(
        &["x", "y"],
        &vector_db,
        &KeyedEmbedding,
        &graph,
        &config(),
    )
    .await
    .unwrap();
    assert_eq!(ranked.len(), 2);
    assert!(
        ranked
            .iter()
            .flatten()
            .filter(|edge| edge.relationship_name != "e_f")
            .all(|edge| (edge.score - 3.0 * SCALED_PENALTY).abs() < 1e-4),
        "{ranked:?}"
    );
}
