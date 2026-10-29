#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! TEMPORAL search over the graph a real default `cognify()` writes.
//!
//! Rust used to have a separate event-graph `temporal_cognify` pipeline, and a
//! TEMPORAL answer depended on running it. Python deprecated and ignores that
//! flag (`cognee/api/v1/cognify/cognify.py`, "since 1.6.3 the flag has no
//! effect"): default cognify extracts dates as `Timestamp` nodes and TEMPORAL
//! reranks by them. Rust removed the pipeline outright (SDK-1039), so this test
//! pins the replacement contract end to end: default cognify → TEMPORAL search
//! finds the dated chunk through the `Timestamp` node cognify wrote.
//!
//! The other temporal tests in this crate build the graph by hand; this one
//! runs the real pipeline, so a change to how cognify writes or links
//! `Timestamp` nodes that TEMPORAL can no longer read fails here.
//!
//! Offline: a real `LadybugAdapter` (its native temporal queries run),
//! `MockVectorDB`, in-memory SQLite and a scripted LLM.

use std::sync::Arc;

use async_trait::async_trait;
use cognee_cognify::{CognifyConfig, cognify};
use cognee_database::{DatabaseConnection, connect, initialize};
use cognee_embedding::{EmbeddingEngine, EmbeddingResult};
use cognee_graph::{GraphDBTrait, LadybugAdapter};
use cognee_llm::{GenerationOptions, GenerationResponse, Llm, LlmResult, Message, MessageRole};
use cognee_models::{Data, Timestamp};
use cognee_ontology::NoOpOntologyResolver;
use cognee_search::{SearchParams, TemporalHybridRetriever, types::SearchContext};
use cognee_storage::{MockStorage, StorageTrait};
use cognee_vector::{MockVectorDB, VectorDB};
use serde_json::{Value, json};
use tempfile::TempDir;
use uuid::Uuid;

const DATED: &str = "Yuri Gagarin orbited the Earth in 1961.";
const UNDATED: &str = "Cosmonaut training was long and demanding.";

/// Answers each structured call by the prompt that asks it: graph extraction
/// (a dated graph for the dated text, an undated one otherwise), summaries,
/// and the TEMPORAL window extraction (the year 1961).
struct ScriptedLlm;

#[async_trait]
impl Llm for ScriptedLlm {
    async fn generate(
        &self,
        _messages: Vec<Message>,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        Ok(GenerationResponse {
            content: "answer".to_string(),
            model: "scripted".to_string(),
            usage: None,
            finish_reason: Some("stop".to_string()),
        })
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        messages: Vec<Message>,
        _json_schema: &Value,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<Value> {
        let system = messages
            .iter()
            .find(|m| matches!(m.role, MessageRole::System))
            .map(|m| m.content.clone())
            .unwrap_or_default();
        let user = messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, MessageRole::User))
            .map(|m| m.content.clone())
            .unwrap_or_default();

        if system.starts_with("Extract one time window from the question.") {
            return Ok(json!({
                "starts_at": {"year": 1961},
                "ends_at": {"year": 1962},
            }));
        }
        if !system.contains("# 2. Timestamps") {
            return Ok(json!({"summary": "s", "description": "d"}));
        }
        if user.contains("Gagarin") {
            Ok(json!({
                "nodes": [
                    {"id": "Yuri Gagarin", "name": "Yuri Gagarin", "type": "Person", "description": "A cosmonaut"},
                    {"id": "1961", "name": "1961", "type": "Timestamp", "description": "A year"}
                ],
                "edges": [
                    {"source_node_id": "Yuri Gagarin", "target_node_id": "1961", "relationship_name": "orbited_at"}
                ]
            }))
        } else {
            Ok(json!({
                "nodes": [
                    {"id": "Cosmonaut training", "name": "Cosmonaut training", "type": "Concept", "description": "Preparation"}
                ],
                "edges": []
            }))
        }
    }

    fn model(&self) -> &str {
        "scripted"
    }
}

/// The question and everything about training embed to `[1, 0]`, everything
/// else to `[0.6, 0.8]`: plain similarity ranks the undated chunk first, so
/// only the temporal rerank can put the dated one ahead of it.
struct BiasedEmbedding;

#[async_trait]
impl EmbeddingEngine for BiasedEmbedding {
    async fn embed(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|text| {
                let lower = text.to_lowercase();
                if lower.contains("training") || lower.contains("what happened") {
                    vec![1.0, 0.0]
                } else {
                    vec![0.6, 0.8]
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

async fn make_db(dataset_id: Uuid, owner_id: Uuid) -> Arc<DatabaseConnection> {
    let conn = connect("sqlite::memory:").await.expect("connect");
    initialize(&conn).await.expect("initialize");
    cognee_database::ops::datasets::create_dataset(
        &conn,
        cognee_models::Dataset::new("space".into(), owner_id, None, dataset_id),
    )
    .await
    .expect("seed dataset");
    Arc::new(conn)
}

async fn ingest(text: &str, name: &str, storage: &Arc<MockStorage>, owner_id: Uuid) -> Data {
    let id = Uuid::new_v4();
    let location = storage
        .store(text.as_bytes(), &format!("{name}-{id}.txt"))
        .await
        .expect("store");
    Data::builder(
        id,
        name,
        location,
        name,
        "txt",
        "text/plain",
        format!("hash-{name}"),
        owner_id,
    )
    .build()
}

/// The chunk texts of a context, in order.
fn chunk_texts(context: &SearchContext) -> Vec<String> {
    context
        .iter()
        .filter(|item| item.payload.get("kind").and_then(Value::as_str) == Some("chunk"))
        .filter_map(|item| item.payload["text"].as_str().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn temporal_search_reranks_by_the_timestamps_default_cognify_writes() {
    let dir = TempDir::new().unwrap();
    let graph: Arc<dyn GraphDBTrait> = Arc::new(
        LadybugAdapter::new(dir.path().join("graph").to_str().unwrap())
            .await
            .unwrap(),
    );
    graph.initialize().await.unwrap();
    let vector: Arc<dyn VectorDB> = Arc::new(MockVectorDB::new());
    let embedding: Arc<dyn EmbeddingEngine> = Arc::new(BiasedEmbedding);
    let llm: Arc<dyn Llm> = Arc::new(ScriptedLlm);

    let owner_id = Uuid::nil();
    let dataset_id = Uuid::new_v4();
    let db = make_db(dataset_id, owner_id).await;
    let storage = Arc::new(MockStorage::new());
    let undated = ingest(UNDATED, "undated.txt", &storage, owner_id).await;
    let dated = ingest(DATED, "dated.txt", &storage, owner_id).await;

    // The default pipeline: there is no temporal flag left to set.
    cognify(
        vec![undated, dated],
        dataset_id,
        None,
        None,
        None,
        Arc::clone(&llm),
        storage,
        Arc::clone(&graph),
        Arc::clone(&vector),
        Arc::clone(&embedding),
        Arc::clone(&db) as Arc<dyn cognee_database::RelationalDb>,
        Arc::new(cognee_database::NoopPipelineRunRepository::new())
            as Arc<dyn cognee_database::PipelineRunRepository>,
        Arc::new(cognee_core::RayonThreadPool::with_default_threads().expect("pool")),
        Arc::new(NoOpOntologyResolver::new()),
        &CognifyConfig::default(),
    )
    .await
    .expect("default cognify must complete against the fixture");

    // Default cognify wrote the date as a Timestamp node, and no Event node.
    let (nodes, _) = graph.get_graph_data().await.unwrap();
    let types: Vec<String> = nodes
        .iter()
        .filter_map(|(_, props)| props.get("type").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    assert!(
        types.iter().any(|t| t == "Timestamp"),
        "default cognify writes Timestamp nodes: {types:?}"
    );
    assert!(
        !types.iter().any(|t| t == "Event"),
        "no event graph is built: {types:?}"
    );
    let year = Timestamp::from_text("1961").unwrap();

    let retriever = TemporalHybridRetriever::new(
        Arc::clone(&vector),
        Arc::clone(&embedding),
        Arc::clone(&graph),
        Arc::clone(&llm),
        None,
        None,
        None,
        None,
        None,
        None,
    );
    let params = SearchParams {
        top_k: Some(1),
        ..SearchParams::default()
    };
    let result = retriever
        .retrieve("What happened in 1961?", &params)
        .await
        .unwrap();

    assert_eq!(
        result.diagnostics.reason, None,
        "the window matched a Timestamp default cognify wrote, so the rerank ran"
    );
    assert!(
        result
            .diagnostics
            .anchors
            .timestamp_ids
            .contains(&year.base.id.to_string()),
        "the anchor is cognify's 1961 node: {:?}",
        result.diagnostics.anchors
    );
    assert_eq!(
        chunk_texts(&result.diagnostics.baseline),
        [UNDATED],
        "plain hybrid ranks the undated chunk first"
    );
    assert_eq!(
        chunk_texts(&result.context),
        [DATED],
        "the rerank puts the chunk dated through cognify's Timestamp first"
    );
}
