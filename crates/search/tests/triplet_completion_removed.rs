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
use cognee_vector::{BruteForceVectorDB, VectorDB, VectorPoint};
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
