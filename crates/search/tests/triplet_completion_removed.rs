#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! `TRIPLET_COMPLETION` is gone (SDK-1039 P1, Python SDK-1115 post-removal).
//!
//! Python deprecated and then removed the triplet retriever; Rust goes straight
//! to the removed state. An unknown search type is rejected at parse time, and a
//! database that still holds a legacy `Triplet_text` vector collection (written
//! by an older cognify / memify run) keeps serving every other search type.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use cognee_embedding::EmbeddingResult;
use cognee_embedding::engine::EmbeddingEngine;
use cognee_graph::{GraphDBTrait, MockGraphDB};
use cognee_search::graph_retrieval::{GraphRetrievalConfig, brute_force_triplet_search};
use cognee_vector::{BruteForceVectorDB, MockVectorDB, VectorDB, VectorPoint};
use serde_json::json;
use uuid::Uuid;

use cognee_search::retrievers::ChunksRetriever;
use cognee_search::types::{SearchOutput, SearchRequest, SearchType};
use cognee_search::{SearchOrchestrator, SearchTypeRegistry};

struct FixedEmbedding;

#[async_trait]
impl EmbeddingEngine for FixedEmbedding {
    async fn embed(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
    }

    fn dimension(&self) -> usize {
        2
    }

    fn batch_size(&self) -> usize {
        16
    }

    fn max_sequence_length(&self) -> usize {
        512
    }
}

fn point(text: &str) -> VectorPoint {
    VectorPoint {
        id: Uuid::new_v4(),
        vector: vec![1.0, 0.0],
        metadata: HashMap::from([("text".to_string(), json!(text))]),
    }
}

#[test]
fn triplet_completion_is_not_a_search_type() {
    let parsed = serde_json::from_value::<SearchType>(json!("TRIPLET_COMPLETION"));
    assert!(
        parsed.is_err(),
        "TRIPLET_COMPLETION must be an unknown type"
    );

    let request = serde_json::from_value::<SearchRequest>(json!({
        "query_text": "q",
        "search_type": "TRIPLET_COMPLETION",
    }));
    assert!(request.is_err(), "a request naming it must not deserialize");
}

#[tokio::test]
async fn search_ignores_a_legacy_triplet_text_collection() {
    let vector_db = BruteForceVectorDB::new();
    vector_db
        .create_collection("DocumentChunk", "text", 2)
        .await
        .unwrap();
    vector_db
        .index_points("DocumentChunk", "text", &[point("Alice works at Acme.")])
        .await
        .unwrap();
    // Legacy collection from an older run.
    vector_db
        .create_collection("Triplet", "text", 2)
        .await
        .unwrap();
    vector_db
        .index_points("Triplet", "text", &[point("Alice-›works_at-›Acme")])
        .await
        .unwrap();
    let vector_db: Arc<dyn VectorDB> = Arc::new(vector_db);

    let mut registry = SearchTypeRegistry::new();
    registry.register(Arc::new(ChunksRetriever::new(
        vector_db,
        Arc::new(FixedEmbedding),
        Some(5),
    )));
    let orchestrator = SearchOrchestrator::new(registry);

    let mut request: SearchRequest =
        serde_json::from_value(json!({ "query_text": "Who works at Acme?" })).unwrap();
    request.search_type = SearchType::Chunks;
    let response = orchestrator.search(&request).await.unwrap();
    match response.result {
        SearchOutput::Items(items) => {
            assert_eq!(items.len(), 1, "only the chunk is returned: {items:?}");
            assert!(
                items[0]
                    .payload
                    .to_string()
                    .contains("Alice works at Acme.")
            );
        }
        other => panic!("expected items, got {other:?}"),
    }
}

/// The graph search is the one reader of `Triplet_text` (it is in
/// `SEARCH_COLLECTIONS`, as in Python's `brute_force_triplet_search`), so this
/// is the search that must keep working on a database that still holds the
/// legacy collection: it succeeds and ranks the same edges as without it.
#[tokio::test]
async fn graph_search_succeeds_on_a_legacy_triplet_text_collection() {
    let a = Uuid::from_u128(0x0A);
    let b = Uuid::from_u128(0x0B);

    async fn seed(with_legacy: bool, a: Uuid, b: Uuid) -> (MockVectorDB, MockGraphDB) {
        let vector_db = MockVectorDB::new();
        vector_db
            .create_collection("Entity", "name", 2)
            .await
            .unwrap();
        vector_db
            .index_points(
                "Entity",
                "name",
                &[VectorPoint {
                    id: a,
                    vector: vec![1.0, 0.0],
                    metadata: HashMap::from([("id".to_string(), json!(a.to_string()))]),
                }],
            )
            .await
            .unwrap();
        if with_legacy {
            vector_db
                .create_collection("Triplet", "text", 2)
                .await
                .unwrap();
            vector_db
                .index_points("Triplet", "text", &[point("Alice-›works_at-›Acme")])
                .await
                .unwrap();
            assert!(vector_db.has_collection("Triplet", "text").await.unwrap());
        }
        let graph = MockGraphDB::new();
        for (id, name) in [(a, "Alice"), (b, "Acme")] {
            graph
                .add_node_raw(json!({ "id": id.to_string(), "name": name }))
                .await
                .unwrap();
        }
        graph
            .add_edge(&a.to_string(), &b.to_string(), "works_at", None)
            .await
            .unwrap();
        (vector_db, graph)
    }

    let config = GraphRetrievalConfig::default();
    let (plain_vectors, plain_graph) = seed(false, a, b).await;
    let expected = brute_force_triplet_search(
        "Who works at Acme?",
        &plain_vectors,
        &FixedEmbedding,
        &plain_graph,
        &config,
    )
    .await
    .unwrap();
    let (legacy_vectors, legacy_graph) = seed(true, a, b).await;
    let actual = brute_force_triplet_search(
        "Who works at Acme?",
        &legacy_vectors,
        &FixedEmbedding,
        &legacy_graph,
        &config,
    )
    .await
    .unwrap();

    assert_eq!(expected.len(), 1, "{expected:?}");
    assert_eq!(actual.len(), expected.len(), "{actual:?}");
    assert_eq!(actual[0].relationship_name, "works_at");
    assert!((actual[0].score - expected[0].score).abs() < 1e-6);
}

/// The legacy collection is actually *queried*, not merely tolerated: here it
/// is the only collection holding a hit (no `Entity`, `DocumentChunk`, ... at
/// all), so the graph search finds its seed node through `Triplet_text` alone.
/// Dropping `("Triplet", "text")` from the searched collections leaves no
/// candidate nodes and this test fails with an empty result. Python includes
/// `Triplet_text` in the same enumeration (graph_completion_retriever.py:88-99).
#[tokio::test]
async fn graph_search_reads_the_legacy_triplet_text_collection() {
    let a = Uuid::from_u128(0x0A);
    let b = Uuid::from_u128(0x0B);

    let vector_db = MockVectorDB::new();
    vector_db
        .create_collection("Triplet", "text", 2)
        .await
        .unwrap();
    vector_db
        .index_points(
            "Triplet",
            "text",
            &[VectorPoint {
                id: a,
                vector: vec![1.0, 0.0],
                metadata: HashMap::from([("text".to_string(), json!("Alice-›works_at-›Acme"))]),
            }],
        )
        .await
        .unwrap();
    let graph = MockGraphDB::new();
    for (id, name) in [(a, "Alice"), (b, "Acme")] {
        graph
            .add_node_raw(json!({ "id": id.to_string(), "name": name }))
            .await
            .unwrap();
    }
    graph
        .add_edge(&a.to_string(), &b.to_string(), "works_at", None)
        .await
        .unwrap();

    let ranked = brute_force_triplet_search(
        "Who works at Acme?",
        &vector_db,
        &FixedEmbedding,
        &graph,
        &GraphRetrievalConfig::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        ranked.len(),
        1,
        "the legacy Triplet_text hit must seed the graph search: {ranked:?}"
    );
    assert_eq!(ranked[0].relationship_name, "works_at");
}
