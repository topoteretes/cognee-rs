//! Truth-subspace build → hybrid re-ranking, end to end on a real Ladybug graph.
//!
//! `build_truth_subspace` (the `improve()` stage) projects every
//! `DocumentChunk` onto centroid slots built from the session's learnings and
//! commits the centroids last; the hybrid retriever then trusts a chunk's
//! coordinates only when its `truth_epoch` matches the live centroids. Two
//! chunks tie on vector similarity, and only one of them is aligned with the
//! learning: with `use_truth_weight` the aligned chunk must rank first, without
//! it the baseline order stands.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]

use std::sync::Arc;

use async_trait::async_trait;
use cognee_cognify::{DEFAULT_K, TruthBuildStatus, build_truth_subspace};
use cognee_embedding::{EmbeddingEngine, EmbeddingResult};
use cognee_graph::{GraphDBTrait, LadybugAdapter};
use cognee_llm::{GenerationOptions, GenerationResponse, Llm, LlmResult, Message};
use cognee_search::{HybridRetriever, SearchParams, retrievers::SearchRetriever};
use cognee_vector::{MockVectorDB, VectorDB, VectorPoint};
use serde_json::{Value, json};
use tempfile::TempDir;
use uuid::Uuid;

const SESSION: &str = "truth-session";
const LESSON: &str = "lesson: prefer the aligned source";
const ALIGNED: &str = "aligned chunk text";
const OTHER: &str = "other chunk text";

/// Two-dimensional embeddings with a fixed meaning: the lesson, the aligned
/// chunk and the query point along the first axis; the other chunk along the
/// second, so it is orthogonal to the learning.
struct AxisEmbedding;

#[async_trait]
impl EmbeddingEngine for AxisEmbedding {
    async fn embed(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|text| {
                if *text == OTHER {
                    vec![0.0, 1.0]
                } else {
                    vec![1.0, 0.0]
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

/// `get_context` never calls the LLM; the constructor only needs one.
struct NoLlm;

#[async_trait]
impl Llm for NoLlm {
    async fn generate(
        &self,
        _messages: Vec<Message>,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        unreachable!("get_context does not generate")
    }
    async fn create_structured_output_with_messages_raw(
        &self,
        _messages: Vec<Message>,
        _schema: &Value,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<Value> {
        unreachable!("get_context does not generate")
    }
    fn model(&self) -> &str {
        "none"
    }
}

fn retriever(
    vector_db: Arc<dyn VectorDB>,
    graph_db: Arc<dyn GraphDBTrait>,
    use_truth_weight: bool,
) -> HybridRetriever {
    HybridRetriever::new(
        vector_db,
        Arc::new(AxisEmbedding),
        graph_db,
        Arc::new(NoLlm),
        None,                   // chunks_top_k
        None,                   // entities_top_k
        None,                   // facts_top_k
        None,                   // max_edges_per_entity
        None,                   // text_summaries_top_k
        None,                   // use_importance_weight
        Some(use_truth_weight), // use_truth_weight
        None,                   // include_global_context_index
        None,                   // global_context_index_top_k
        None,                   // node_name
        None,                   // node_name_filter_operator
        None,                   // system_prompt
        None,                   // system_prompt_path
        None,                   // user_prompt_template
        None,                   // generation_options
    )
}

fn chunk_texts(context: &[cognee_search::types::SearchItem]) -> Vec<String> {
    context
        .iter()
        .filter(|item| item.payload.get("kind").and_then(Value::as_str) == Some("chunk"))
        .filter_map(|item| item.payload.get("text").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

async fn add_chunk(graph: &dyn GraphDBTrait, id: &str, text: &str) {
    graph
        .add_node_raw(json!({"id": id, "name": id, "type": "DocumentChunk", "text": text}))
        .await
        .unwrap();
}

#[tokio::test]
async fn built_truth_subspace_lifts_the_aligned_chunk_in_hybrid_search() {
    let temp_dir = TempDir::new().unwrap();
    let graph_path = temp_dir.path().join("graph").to_string_lossy().to_string();
    let ladybug: Arc<dyn GraphDBTrait> = Arc::new(LadybugAdapter::new(&graph_path).await.unwrap());
    ladybug.initialize().await.unwrap();
    assert!(ladybug.supports_truth_state().await.unwrap());

    // The session's learnings node set with one lesson chunk.
    let set_id = Uuid::new_v4().to_string();
    let lesson_id = Uuid::new_v4().to_string();
    ladybug
        .add_node_raw(json!({
            "id": set_id,
            "type": "NodeSet",
            "name": cognee_truth_subspace::truth_session_node_set(SESSION),
        }))
        .await
        .unwrap();
    add_chunk(ladybug.as_ref(), &lesson_id, LESSON).await;
    ladybug
        .add_edge(&set_id, &lesson_id, "contains", None)
        .await
        .unwrap();

    // Two corpus chunks, identical in the vector index; `other` is indexed
    // first, so it wins the baseline tie.
    let (aligned_id, other_id) = (Uuid::new_v4(), Uuid::new_v4());
    add_chunk(ladybug.as_ref(), &aligned_id.to_string(), ALIGNED).await;
    add_chunk(ladybug.as_ref(), &other_id.to_string(), OTHER).await;
    let vector = MockVectorDB::new();
    vector
        .create_collection("DocumentChunk", "text", 2)
        .await
        .unwrap();
    for (id, text) in [(other_id, OTHER), (aligned_id, ALIGNED)] {
        vector
            .index_points(
                "DocumentChunk",
                "text",
                &[VectorPoint::new(id, vec![1.0, 0.0])
                    .with_metadata("id", json!(id.to_string()))
                    .with_metadata("text", json!(text))],
            )
            .await
            .unwrap();
    }
    let vector_db: Arc<dyn VectorDB> = Arc::new(vector);

    let dataset_id = Uuid::new_v4();
    let built = build_truth_subspace(
        dataset_id,
        &[SESSION.to_string()],
        Arc::clone(&ladybug),
        Arc::clone(&vector_db),
        Arc::new(AxisEmbedding),
        DEFAULT_K,
    )
    .await;
    assert_eq!(built.status, TruthBuildStatus::Completed, "{built:?}");
    assert_eq!(built.truth_epoch, 1);
    assert_eq!(built.nodes_scored, 3, "lesson + both corpus chunks");
    let stored = ladybug
        .get_node_truth_state(&[aligned_id.to_string(), other_id.to_string()])
        .await
        .unwrap();
    assert!(stored.values().all(|state| state.truth_epoch == 1));

    let params = SearchParams {
        dataset_id: Some(dataset_id),
        ..SearchParams::default()
    };
    let baseline = retriever(Arc::clone(&vector_db), Arc::clone(&ladybug), false)
        .get_context("query", &params)
        .await
        .unwrap();
    assert_eq!(
        chunk_texts(&baseline),
        [OTHER, ALIGNED],
        "baseline tie order"
    );

    let reranked = retriever(vector_db, ladybug, true)
        .get_context("query", &params)
        .await
        .unwrap();
    assert_eq!(
        chunk_texts(&reranked),
        [ALIGNED, OTHER],
        "the chunk aligned with the learning must rank first"
    );
}
