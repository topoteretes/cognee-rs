#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! TEMPORAL search end to end on a graph shaped like default cognify's output.
//!
//! Default cognify writes every date it extracts as a shared `Timestamp` node:
//! chunks `contains` the timestamps they name and the entities they mention,
//! and entities reach timestamps through `*_at` edges. The
//! [`TemporalHybridRetriever`] runs plain hybrid retrieval, asks the graph which
//! candidates are attached to a `Timestamp` inside the question's window, and
//! reranks — so the chunk dated inside the window comes first even when
//! similarity ranks it last. Python's counterpart is
//! `tests/integration/retrieval/test_temporal_retriever.py`.
//!
//! The graph is a real `LadybugAdapter` (so its native `get_timestamps_in_range`
//! / `get_temporal_anchors` run), the vector store is `MockVectorDB`, and the
//! LLM is scripted: it answers the window extraction with a fixed interval and
//! the completion with a fixed string. No network, no environment.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cognee_embedding::{EmbeddingEngine, EmbeddingResult};
use cognee_graph::{GraphDBTrait, GraphDBTraitExt, LadybugAdapter};
use cognee_llm::{GenerationOptions, GenerationResponse, Llm, LlmResult, Message};
use cognee_models::Timestamp;
use cognee_search::{
    HybridRetriever, SearchParams, SessionContext, TemporalFallbackReason, TemporalHybridRetriever,
    retrievers::SearchRetriever,
    types::{SearchContext, SearchOutput},
};
use cognee_vector::{MockVectorDB, VectorDB, VectorPoint};
use serde_json::{Value, json};
use tempfile::TempDir;
use uuid::Uuid;

/// Answers the window extraction with `interval`, the completion with
/// `"answer"`, and keeps the completion prompt.
struct ScriptedLlm {
    interval: Value,
    last_prompt: Mutex<Vec<Message>>,
}

#[async_trait]
impl Llm for ScriptedLlm {
    async fn generate(
        &self,
        messages: Vec<Message>,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        *self.last_prompt.lock().unwrap() = messages;
        Ok(GenerationResponse {
            content: "answer".to_string(),
            model: "scripted".to_string(),
            usage: None,
            finish_reason: Some("stop".to_string()),
        })
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        _messages: Vec<Message>,
        _json_schema: &Value,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<Value> {
        Ok(self.interval.clone())
    }

    fn model(&self) -> &str {
        "scripted"
    }
}

/// Every query embeds to `[1, 0]`, so a row's similarity is set by its own
/// vector.
struct FixedQueryEmbedding;

#[async_trait]
impl EmbeddingEngine for FixedQueryEmbedding {
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

/// The three chunks, most similar to the question first.
const CLOSE: &str = "The Soviet space program trained cosmonauts for long missions.";
const YEAR: &str = "1965 was a busy year for crewed spaceflight.";
const DAY: &str = "Voskhod 2 carried Alexei Leonov on the first spacewalk.";

fn id(name: &str) -> Uuid {
    Uuid::new_v5(&Uuid::NAMESPACE_OID, name.as_bytes())
}

struct Fixture {
    graph: Arc<dyn GraphDBTrait>,
    vector: Arc<dyn VectorDB>,
    _dir: TempDir,
}

/// A default-cognified graph about the first spacewalk.
///
/// - `close` mentions Leonov and carries no date: the best similarity hit.
/// - `year` names "1965": the year node, which also overlaps the day.
/// - `day` mentions the Voskhod 2 entity, launched on 1965-03-18 — dated only
///   through the entity, and the least similar of the three.
async fn spacewalk_graph() -> Fixture {
    let dir = TempDir::new().unwrap();
    let graph = LadybugAdapter::new(dir.path().join("graph").to_str().unwrap())
        .await
        .unwrap();
    graph.initialize().await.unwrap();

    let year = Timestamp::from_text("1965").unwrap();
    let day = Timestamp::from_text("18 March 1965").unwrap();
    graph.add_nodes(&[&year, &day]).await.unwrap();
    for (name, node_type) in [
        ("close", "DocumentChunk"),
        ("year", "DocumentChunk"),
        ("day", "DocumentChunk"),
        ("voskhod 2", "Entity"),
        ("alexei leonov", "Entity"),
    ] {
        graph
            .add_node_raw(json!({"id": id(name).to_string(), "name": name, "type": node_type}))
            .await
            .unwrap();
    }
    let (year_id, day_id) = (year.base.id.to_string(), day.base.id.to_string());
    for (source, target, relationship) in [
        (id("year").to_string(), year_id.clone(), "contains"),
        (
            id("day").to_string(),
            id("voskhod 2").to_string(),
            "contains",
        ),
        (id("voskhod 2").to_string(), day_id.clone(), "launched_at"),
        (
            id("day").to_string(),
            id("alexei leonov").to_string(),
            "contains",
        ),
        (
            id("close").to_string(),
            id("alexei leonov").to_string(),
            "contains",
        ),
    ] {
        graph
            .add_edge(&source, &target, relationship, None)
            .await
            .unwrap();
    }

    let vector = MockVectorDB::new();
    vector
        .create_collection("DocumentChunk", "text", 2)
        .await
        .unwrap();
    for (name, text, embedding) in [
        ("close", CLOSE, vec![1.0, 0.0]),
        ("year", YEAR, vec![0.8, 0.6]),
        ("day", DAY, vec![0.6, 0.8]),
    ] {
        vector
            .index_points(
                "DocumentChunk",
                "text",
                &[VectorPoint::new(id(name), embedding)
                    .with_metadata("id", json!(id(name).to_string()))
                    .with_metadata("text", json!(text))],
            )
            .await
            .unwrap();
    }
    vector.create_collection("Entity", "name", 2).await.unwrap();
    for (name, embedding) in [
        ("alexei leonov", vec![0.9, 0.1]),
        ("voskhod 2", vec![0.5, 0.5]),
    ] {
        vector
            .index_points(
                "Entity",
                "name",
                &[VectorPoint::new(id(name), embedding)
                    .with_metadata("id", json!(id(name).to_string()))
                    .with_metadata("name", json!(name))],
            )
            .await
            .unwrap();
    }

    Fixture {
        graph: Arc::new(graph),
        vector: Arc::new(vector),
        _dir: dir,
    }
}

fn temporal(fixture: &Fixture, llm: Arc<ScriptedLlm>) -> TemporalHybridRetriever {
    TemporalHybridRetriever::new(
        Arc::clone(&fixture.vector),
        Arc::new(FixedQueryEmbedding),
        Arc::clone(&fixture.graph),
        llm,
        None,
        None,
        None,
        None,
        None,
        None,
    )
}

fn scripted(interval: Value) -> Arc<ScriptedLlm> {
    Arc::new(ScriptedLlm {
        interval,
        last_prompt: Mutex::new(vec![]),
    })
}

/// The chunk texts of a context, in order.
fn chunk_texts(context: &SearchContext) -> Vec<String> {
    context
        .iter()
        .filter(|item| item.payload.get("kind").and_then(Value::as_str) == Some("chunk"))
        .map(|item| item.payload["text"].as_str().unwrap().to_string())
        .collect()
}

fn top_2() -> SearchParams {
    SearchParams {
        top_k: Some(2),
        ..SearchParams::default()
    }
}

#[tokio::test]
async fn the_chunk_dated_inside_the_window_ranks_first() {
    let fixture = spacewalk_graph().await;
    let llm = scripted(json!({
        "starts_at": {"year": 1965, "month": 3, "day": 18},
        "ends_at": {"year": 1965, "month": 3, "day": 19},
    }));
    let retriever = temporal(&fixture, Arc::clone(&llm));

    let result = retriever
        .retrieve("What happened on 18 March 1965?", &top_2())
        .await
        .unwrap();

    assert_eq!(result.diagnostics.reason, None);
    // The day-dated chunk first (reached through the Voskhod 2 entity), then
    // the year-dated one; the undated best similarity hit is cut.
    assert_eq!(chunk_texts(&result.context), [DAY, YEAR]);
    assert_eq!(chunk_texts(&result.diagnostics.baseline), [CLOSE, YEAR]);
    assert!(
        result
            .diagnostics
            .anchors
            .entity_ids
            .contains(&id("voskhod 2").to_string())
    );

    // The answer is generated over the reranked context, day first.
    let output = retriever
        .get_completion(
            "What happened on 18 March 1965?",
            Some(result.context),
            &SessionContext::default(),
            &top_2(),
        )
        .await
        .unwrap();
    assert!(matches!(output, SearchOutput::Text(ref text) if text == "answer"));
    let prompt = llm.last_prompt.lock().unwrap().clone();
    let user = &prompt.last().unwrap().content;
    let (day_at, year_at) = (user.find(DAY).unwrap(), user.find(YEAR).unwrap());
    assert!(day_at < year_at, "the day-dated passage leads the prompt");
    assert!(!user.contains(CLOSE));
}

#[tokio::test]
async fn a_question_without_a_time_window_answers_with_plain_hybrid() {
    let fixture = spacewalk_graph().await;
    let retriever = temporal(
        &fixture,
        scripted(json!({"starts_at": null, "ends_at": null})),
    );

    let result = retriever
        .retrieve("Who walked in space first?", &top_2())
        .await
        .unwrap();
    assert_eq!(
        result.diagnostics.reason,
        Some(TemporalFallbackReason::NoTimeConstraint)
    );
    assert_eq!(chunk_texts(&result.context), [CLOSE, YEAR]);

    // The same chunks plain HYBRID_COMPLETION returns for the question.
    let hybrid = HybridRetriever::new(
        Arc::clone(&fixture.vector),
        Arc::new(FixedQueryEmbedding),
        Arc::clone(&fixture.graph),
        scripted(Value::Null),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    );
    let plain = hybrid
        .get_context("Who walked in space first?", &top_2())
        .await
        .unwrap();
    assert_eq!(chunk_texts(&plain), chunk_texts(&result.context));
}

#[tokio::test]
async fn a_window_with_no_dated_data_falls_back_with_a_reason() {
    let fixture = spacewalk_graph().await;
    let retriever = temporal(
        &fixture,
        scripted(json!({
            "starts_at": {"year": 1980},
            "ends_at": {"year": 1981},
        })),
    );
    let result = retriever
        .retrieve("What happened in 1980?", &top_2())
        .await
        .unwrap();
    assert_eq!(
        result.diagnostics.reason,
        Some(TemporalFallbackReason::NoTemporalMatch)
    );
    assert_eq!(chunk_texts(&result.context), [CLOSE, YEAR]);
}
