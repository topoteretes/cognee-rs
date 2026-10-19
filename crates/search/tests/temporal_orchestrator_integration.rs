#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! `SearchType::Temporal` driven through `SearchOrchestrator::search`, the
//! entry point every SDK binding calls.
//!
//! `temporal_retriever_integration.rs` exercises the
//! [`TemporalHybridRetriever`](cognee_search::TemporalHybridRetriever) on its
//! own; this file checks the orchestrator wires TEMPORAL to it and carries its
//! reranked context through the three request shapes that matter: a
//! completion, an `only_context` request (no answer generated), and a session
//! completion that records the graph elements it used. Python routes
//! `SearchType.TEMPORAL` to `TemporalHybridRetriever` in
//! `get_search_type_retriever_instance.py`, and its session path records
//! `used_graph_element_ids` through the inherited
//! `HybridRetriever.get_completion` (`hybrid_retriever.py`).
//!
//! The orchestrator comes from [`SearchBuilder`], so TEMPORAL is the standard
//! production retriever, not one registered by hand. The graph is a real
//! `LadybugAdapter` shaped like default cognify's output (chunks `contains`
//! entities and `Timestamp` nodes, entities reach timestamps through `*_at`
//! edges), the vector store is `MockVectorDB`, the embedding is fixed and the
//! LLM is scripted. No network, no environment.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cognee_database::{DatabaseError, SearchHistoryDb, SearchHistoryEntry};
use cognee_embedding::{EmbeddingEngine, EmbeddingResult};
use cognee_graph::{GraphDBTrait, GraphDBTraitExt, LadybugAdapter};
use cognee_llm::{GenerationOptions, GenerationResponse, Llm, LlmResult, Message};
use cognee_models::Timestamp;
use cognee_search::types::{SearchContext, SearchOutput, SearchRequest, SearchType};
use cognee_search::{SearchBuilder, SearchOrchestrator};
use cognee_session::{FsSessionStore, SessionManager, SessionStore};
use cognee_vector::{MockVectorDB, VectorDB, VectorPoint};
use serde_json::{Value, json};
use tempfile::TempDir;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

/// Answers the query-interval extraction (structured output) with a fixed
/// 18 March 1965 window and every completion with `"answer"`, counting both
/// and keeping the last completion prompt.
#[derive(Default)]
struct ScriptedLlm {
    completions: AtomicUsize,
    interval_extractions: AtomicUsize,
    last_prompt: Mutex<Vec<Message>>,
}

#[async_trait]
impl Llm for ScriptedLlm {
    async fn generate(
        &self,
        messages: Vec<Message>,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        self.completions.fetch_add(1, Ordering::SeqCst);
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
        self.interval_extractions.fetch_add(1, Ordering::SeqCst);
        Ok(json!({
            "starts_at": {"year": 1965, "month": 3, "day": 18},
            "ends_at": {"year": 1965, "month": 3, "day": 19},
        }))
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

/// `SearchBuilder` requires a history store; these tests do not read it.
struct NoOpHistoryDb;

#[async_trait]
impl SearchHistoryDb for NoOpHistoryDb {
    async fn log_query(
        &self,
        _query_text: &str,
        _query_type: &str,
        _user_id: Option<Uuid>,
    ) -> Result<Uuid, DatabaseError> {
        Ok(Uuid::new_v4())
    }

    async fn log_result(
        &self,
        _query_id: Uuid,
        _serialized_result: &str,
        _user_id: Option<Uuid>,
    ) -> Result<Uuid, DatabaseError> {
        Ok(Uuid::new_v4())
    }

    async fn get_history(
        &self,
        _user_id: Option<Uuid>,
        _limit: Option<usize>,
    ) -> Result<Vec<SearchHistoryEntry>, DatabaseError> {
        Ok(vec![])
    }
}

// ---------------------------------------------------------------------------
// Fixture: a default-cognified graph about the first spacewalk
// ---------------------------------------------------------------------------

/// The three chunks, most similar to the question first.
const CLOSE: &str = "The Soviet space program trained cosmonauts for long missions.";
const YEAR: &str = "1965 was a busy year for crewed spaceflight.";
const DAY: &str = "Voskhod 2 carried Alexei Leonov on the first spacewalk.";

const QUESTION: &str = "What happened on 18 March 1965?";

fn id(name: &str) -> Uuid {
    Uuid::new_v5(&Uuid::NAMESPACE_OID, name.as_bytes())
}

struct Fixture {
    graph: Arc<dyn GraphDBTrait>,
    vector: Arc<dyn VectorDB>,
    _dir: TempDir,
}

/// - `close` mentions Leonov and carries no date: the best similarity hit.
/// - `year` names "1965": the year `Timestamp`, which also overlaps the day.
/// - `day` mentions the Voskhod 2 entity, `launched_at` 1965-03-18 — dated
///   only through the entity, and the least similar of the three.
async fn spacewalk_graph() -> Fixture {
    let dir = TempDir::new().unwrap();
    let graph = LadybugAdapter::new(dir.path().join("graph").to_str().unwrap())
        .await
        .unwrap();
    graph.initialize().await.unwrap();

    // `Timestamp::from_text` fills time_at / time_until / timestamp_str /
    // precision exactly as default cognify does.
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
        (id("year").to_string(), year_id, "contains"),
        (
            id("day").to_string(),
            id("voskhod 2").to_string(),
            "contains",
        ),
        (id("voskhod 2").to_string(), day_id, "launched_at"),
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

/// The production orchestrator over the fixture, optionally with a session
/// manager.
fn orchestrator(
    fixture: &Fixture,
    llm: &Arc<ScriptedLlm>,
    session_manager: Option<Arc<SessionManager>>,
) -> SearchOrchestrator {
    let builder = SearchBuilder::new(
        Arc::clone(&fixture.vector),
        Arc::new(FixedQueryEmbedding),
        Arc::clone(&fixture.graph),
        Arc::clone(llm) as Arc<dyn Llm>,
        Arc::new(NoOpHistoryDb),
    );
    match session_manager {
        Some(session_manager) => builder.with_session_manager(session_manager),
        None => builder,
    }
    .build()
}

/// A TEMPORAL request for [`QUESTION`] with `top_k = 2`, so the undated best
/// similarity hit is cut once the two dated chunks are promoted.
fn temporal_request() -> SearchRequest {
    SearchRequest {
        query_text: QUESTION.to_string(),
        search_type: SearchType::Temporal,
        top_k: Some(2),
        datasets: None,
        dataset_ids: None,
        system_prompt: None,
        system_prompt_path: None,
        only_context: Some(false),
        use_combined_context: Some(false),
        session_id: None,
        node_type: None,
        node_name: None,
        node_name_filter_operator: None,
        wide_search_top_k: None,
        triplet_distance_penalty: None,
        save_interaction: Some(false),
        user_id: None,
        tenant_id: None,
        verbose: None,
        feedback_influence: None,
        retriever_specific_config: None,
        response_schema: None,
        custom_search_type: None,
        auto_feedback_detection: None,
        neighborhood_depth: None,
        neighborhood_seed_top_k: None,
        summarize_context: None,
    }
}

/// The chunk texts of a context, in order.
fn chunk_texts(context: &SearchContext) -> Vec<String> {
    context
        .iter()
        .filter(|item| item.payload.get("kind").and_then(Value::as_str) == Some("chunk"))
        .map(|item| item.payload["text"].as_str().unwrap().to_string())
        .collect()
}

/// The single (unscoped) context of a response.
fn only_response_context(
    context: Option<&std::collections::HashMap<String, SearchContext>>,
) -> &SearchContext {
    let context = context.expect("the response must carry its context");
    assert_eq!(context.len(), 1, "an unscoped search has one context");
    context.values().next().unwrap()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// A dated question answers over the reranked context: the chunk dated inside
/// the window (reached only through the Voskhod 2 entity's `launched_at`
/// timestamp) leads, the year-dated chunk follows, and the undated best
/// similarity hit is cut — both in the returned context and in the prompt the
/// answer was generated from.
#[tokio::test]
async fn temporal_completion_answers_over_the_window_reranked_context() {
    let fixture = spacewalk_graph().await;
    let llm = Arc::new(ScriptedLlm::default());
    let orchestrator = orchestrator(&fixture, &llm, None);

    let mut request = temporal_request();
    // Combined + verbose: the orchestrator fetches the context itself and
    // returns it next to the answer.
    request.use_combined_context = Some(true);
    request.verbose = Some(true);
    let response = orchestrator.search(&request).await.unwrap();

    assert_eq!(response.search_type, SearchType::Temporal);
    assert!(matches!(response.result, SearchOutput::Text(ref text) if text == "answer"));
    assert_eq!(
        chunk_texts(only_response_context(response.context.as_ref())),
        [DAY, YEAR]
    );

    assert_eq!(llm.interval_extractions.load(Ordering::SeqCst), 1);
    assert_eq!(llm.completions.load(Ordering::SeqCst), 1);
    let prompt = llm.last_prompt.lock().unwrap().clone();
    let user = &prompt.last().unwrap().content;
    let (day_at, year_at) = (user.find(DAY).unwrap(), user.find(YEAR).unwrap());
    assert!(day_at < year_at, "the day-dated passage leads the prompt");
    assert!(!user.contains(CLOSE), "the undated chunk was cut");
}

/// `only_context` returns the reranked context and the prompt the completion
/// would have sent, without generating an answer: the window is still
/// extracted (it drives the rerank) but no completion call is made.
#[tokio::test]
async fn temporal_only_context_skips_the_completion() {
    let fixture = spacewalk_graph().await;
    let llm = Arc::new(ScriptedLlm::default());
    let orchestrator = orchestrator(&fixture, &llm, None);

    let mut request = temporal_request();
    request.only_context = Some(true);
    let response = orchestrator.search(&request).await.unwrap();

    assert!(response.only_context);
    assert_eq!(
        chunk_texts(only_response_context(response.context.as_ref())),
        [DAY, YEAR]
    );
    assert_eq!(
        llm.completions.load(Ordering::SeqCst),
        0,
        "no answer generated"
    );
    assert_eq!(llm.interval_extractions.load(Ordering::SeqCst), 1);

    // The result is the user prompt hybrid's completion would send, day first.
    let user_prompt = response
        .user_prompt
        .as_deref()
        .expect("a non-empty context builds the prompt");
    assert!(matches!(response.result, SearchOutput::Text(ref text) if text == user_prompt));
    let (day_at, year_at) = (
        user_prompt.find(DAY).unwrap(),
        user_prompt.find(YEAR).unwrap(),
    );
    assert!(day_at < year_at);
    assert!(!user_prompt.contains(CLOSE));
}

/// A session completion on the default path (no combined context) makes the
/// orchestrator fetch the TEMPORAL context itself so the saved QA entry can
/// record the graph elements behind the answer: the reranked chunks and their
/// entities, and no edge ids (hybrid records node ids only, as Python's
/// `extract_context_object_ids`).
#[tokio::test]
async fn temporal_session_completion_records_used_graph_element_ids() {
    let fixture = spacewalk_graph().await;
    let llm = Arc::new(ScriptedLlm::default());
    let sessions = TempDir::new().unwrap();
    let store = Arc::new(FsSessionStore::new(sessions.path()));
    let session_manager = Arc::new(SessionManager::new(
        Arc::clone(&store) as Arc<dyn SessionStore>
    ));
    let orchestrator = orchestrator(&fixture, &llm, Some(session_manager));

    let session_id = "temporal-orchestrator-session";
    let mut request = temporal_request();
    request.session_id = Some(session_id.to_string());
    let response = orchestrator.search(&request).await.unwrap();
    assert!(matches!(response.result, SearchOutput::Text(ref text) if text == "answer"));
    // The context is fetched once and handed to the completion.
    assert_eq!(llm.interval_extractions.load(Ordering::SeqCst), 1);
    assert_eq!(llm.completions.load(Ordering::SeqCst), 1);

    let entries = store
        .get_latest_qa_entries(session_id, None, 10)
        .await
        .unwrap();
    let [entry] = entries.as_slice() else {
        panic!("one QA entry expected, got {entries:?}");
    };
    assert_eq!(entry.question, QUESTION);
    assert_eq!(entry.answer, "answer");
    let ids = entry
        .used_graph_element_ids
        .as_ref()
        .expect("a TEMPORAL session completion records used_graph_element_ids");
    for used in ["day", "year", "voskhod 2"] {
        assert!(
            ids.node_ids.contains(&id(used).to_string()),
            "node_ids should include {used:?}; got {:?}",
            ids.node_ids
        );
    }
    // The cut `close` chunk is not asserted absent: it is still recorded as
    // an endpoint of the Alexei Leonov entity's `contains` edge, since entity
    // edge endpoints count as used (Python's `extract_context_object_ids`).
    assert!(ids.edge_ids.is_empty(), "got {:?}", ids.edge_ids);
}
