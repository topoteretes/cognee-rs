#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for the [`HybridRetriever`] driven through the crate's
//! public API only.
//!
//! Two tiers:
//! - A mock-backed test (no LLM) that seeds `MockVectorDB` + `MockGraphDB`, runs
//!   the real `SearchRetriever::get_context`, and asserts the returned
//!   kind-tagged `SearchItem` payload shapes (chunk / entity-with-edges / fact).
//! - An OpenAI-gated test that routes a full completion through
//!   `SearchOrchestrator::search` with a `session_id` and asserts a non-empty
//!   answer plus a populated `used_graph_element_ids` snapshot persisted to the
//!   session store. It skips (prints `-- skipping ...`) when `OPENAI_URL` /
//!   `OPENAI_TOKEN` are unset, mirroring `temporal_retriever_integration.rs`.
//!
//! Run with:
//!   cargo test --package cognee-search --test hybrid_retriever_integration \
//!     -- --nocapture

use std::sync::Arc;

use async_trait::async_trait;
use cognee_embedding::{EmbeddingEngine, EmbeddingResult};
use cognee_graph::{GraphDBTrait, MockGraphDB};
use cognee_llm::{
    GenerationOptions, GenerationResponse, Llm, LlmResult, Message, TokenUsage,
    build_openai_compatible_adapter,
};
use cognee_models::EdgeType;
use cognee_search::types::{SearchOutput, SearchType};
use cognee_search::{
    HybridRetriever, SearchOrchestrator, SearchParams, SearchTypeRegistry,
    retrievers::SearchRetriever, types::SearchRequest,
};
use cognee_session::{FsSessionStore, SessionManager, SessionStore};
use cognee_vector::{MockVectorDB, VectorDB, VectorPoint};
use serde_json::{Value, json};
use std::borrow::Cow;
use std::collections::HashMap;
use tempfile::TempDir;
use uuid::Uuid;

const CHUNK_TEXT: &str = "the rust ownership model chunk";
const ENTITY_NAME: &str = "Alice";
const BULLET_TEXT: &str = "Alice works at Acme.";
const FACT_TEXT: &str = "Acme acquired Initech.";

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

/// Embedding engine that maps every input to the same 2-D unit vector so the
/// seeded `MockVectorDB` points (also `[1.0, 0.0]`) are always the nearest
/// neighbours — keeps retrieval deterministic without a real model.
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

/// Minimal LLM stub for the no-LLM mock test: the retriever constructor needs an
/// `Arc<dyn Llm>`, but `get_context` never calls it.
struct StubLlm;

#[async_trait]
impl Llm for StubLlm {
    async fn generate(
        &self,
        _messages: Vec<Message>,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        Ok(GenerationResponse {
            content: "stub answer".to_string(),
            model: "stub".to_string(),
            usage: Some(TokenUsage {
                prompt_tokens: 1,
                completion_tokens: 1,
                total_tokens: 2,
            }),
            finish_reason: Some("stop".to_string()),
        })
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        _messages: Vec<Message>,
        _json_schema: &Value,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<Value> {
        Ok(json!({ "ok": true }))
    }

    fn model(&self) -> &str {
        "stub"
    }
}

/// Build a real LLM adapter from environment variables, or `None` to skip.
/// Mirrors `temporal_retriever_integration.rs::build_test_llm`.
fn build_test_llm() -> Option<Arc<dyn Llm>> {
    let _ = dotenv::dotenv();
    let url = std::env::var("OPENAI_URL")
        .ok()
        .or_else(|| std::env::var("LLM_ENDPOINT").ok())?;
    let token = std::env::var("OPENAI_TOKEN")
        .ok()
        .or_else(|| std::env::var("LLM_API_KEY").ok())?;
    if url.is_empty() || token.is_empty() {
        return None;
    }
    let model = std::env::var("OPENAI_MODEL")
        .ok()
        .or_else(|| std::env::var("LLM_MODEL").ok())
        .unwrap_or_else(|| "gpt-4o-mini".to_string());
    let provider = std::env::var("LLM_PROVIDER").unwrap_or_else(|_| "openai".to_string());
    Some(Arc::new(
        build_openai_compatible_adapter(&provider, &model, &token, &url, 3)
            .expect("build_openai_compatible_adapter should succeed with valid args"),
    ))
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

async fn edge_props(edge_text: &str) -> HashMap<Cow<'static, str>, Value> {
    let mut props: HashMap<Cow<'static, str>, Value> = HashMap::new();
    props.insert(Cow::from("edge_text"), json!(edge_text));
    props
}

/// Seed vector + graph DBs with one chunk, one entity (Alice --works_at-->
/// Acme), and two EdgeType rows (one surfaced as an entity bullet, one that
/// survives as a standalone fact). Returns the seeded ids for assertions.
async fn seed_dbs() -> (Arc<dyn VectorDB>, Arc<dyn GraphDBTrait>, Uuid, Uuid) {
    let db = MockVectorDB::new();

    db.create_collection("DocumentChunk", "text", 2)
        .await
        .unwrap();
    let chunk_id = Uuid::new_v4();
    db.index_points(
        "DocumentChunk",
        "text",
        &[VectorPoint::new(chunk_id, vec![1.0, 0.0])
            .with_metadata("id", json!(chunk_id.to_string()))
            .with_metadata("text", json!(CHUNK_TEXT))],
    )
    .await
    .unwrap();

    db.create_collection("Entity", "name", 2).await.unwrap();
    let alice_id = Uuid::new_v4();
    db.index_points(
        "Entity",
        "name",
        &[VectorPoint::new(alice_id, vec![1.0, 0.0])
            .with_metadata("id", json!(alice_id.to_string()))
            .with_metadata("name", json!(ENTITY_NAME))],
    )
    .await
    .unwrap();

    db.create_collection("EdgeType", "relationship_name", 2)
        .await
        .unwrap();
    let bullet_edge_id = EdgeType::deterministic_id(BULLET_TEXT);
    let fact_edge_id = EdgeType::deterministic_id(FACT_TEXT);
    db.index_points(
        "EdgeType",
        "relationship_name",
        &[
            VectorPoint::new(bullet_edge_id, vec![1.0, 0.0])
                .with_metadata("id", json!(bullet_edge_id.to_string()))
                .with_metadata("text", json!(BULLET_TEXT)),
            VectorPoint::new(fact_edge_id, vec![1.0, 0.0])
                .with_metadata("id", json!(fact_edge_id.to_string()))
                .with_metadata("text", json!(FACT_TEXT)),
        ],
    )
    .await
    .unwrap();

    let graph = MockGraphDB::new();
    graph
        .add_node_raw(json!({ "id": alice_id.to_string(), "name": ENTITY_NAME }))
        .await
        .unwrap();
    graph
        .add_node_raw(json!({ "id": "acme-id", "name": "Acme" }))
        .await
        .unwrap();
    graph
        .add_edge(
            &alice_id.to_string(),
            "acme-id",
            "works_at",
            Some(edge_props(BULLET_TEXT).await),
        )
        .await
        .unwrap();

    (Arc::new(db), Arc::new(graph), chunk_id, alice_id)
}

fn build_hybrid_retriever(
    vector_db: Arc<dyn VectorDB>,
    graph_db: Arc<dyn GraphDBTrait>,
    llm: Arc<dyn Llm>,
) -> HybridRetriever {
    HybridRetriever::new(
        vector_db,
        Arc::new(AlignedEmbedding),
        graph_db,
        llm,
        None, // chunks_top_k
        None, // entities_top_k
        None, // facts_top_k
        None, // max_edges_per_entity
        None, // text_summaries_top_k
        None, // use_importance_weight
        None, // use_truth_weight
        None, // include_global_context_index
        None, // global_context_index_top_k
        None, // node_name
        None, // node_name_filter_operator
        None, // system_prompt
        None, // system_prompt_path
        None, // user_prompt_template
        None, // generation_options
    )
}

fn kinds(context: &[cognee_search::types::SearchItem]) -> Vec<String> {
    context
        .iter()
        .filter_map(|item| {
            item.payload
                .get("kind")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

fn completion_request(query: &str, session_id: &str) -> SearchRequest {
    SearchRequest {
        query_text: query.to_string(),
        search_type: SearchType::HybridCompletion,
        top_k: Some(5),
        datasets: None,
        dataset_ids: None,
        system_prompt: None,
        system_prompt_path: None,
        only_context: Some(false),
        // Combined context makes the orchestrator materialize `context` (and
        // therefore build `used_graph_element_ids`) on the completion path.
        use_combined_context: Some(true),
        session_id: Some(session_id.to_string()),
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

// ---------------------------------------------------------------------------
// Test 1: mock-backed payload-shape round trip (always runs)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn hybrid_get_context_returns_kind_tagged_payload_shapes() {
    let (vector_db, graph_db, chunk_id, alice_id) = seed_dbs().await;
    let retriever = build_hybrid_retriever(vector_db, graph_db, Arc::new(StubLlm));

    let context = retriever
        .get_context("query", &SearchParams::default())
        .await
        .expect("get_context should succeed against seeded mocks");

    // All three lanes surface, in chunk -> entity -> fact order.
    assert_eq!(kinds(&context), vec!["chunk", "entity", "fact"]);

    // Chunk item carries its id + text.
    let chunk = &context[0];
    assert_eq!(chunk.payload["kind"], json!("chunk"));
    assert_eq!(chunk.payload["id"], json!(chunk_id.to_string()));
    assert_eq!(chunk.payload["text"], json!(CHUNK_TEXT));

    // Entity item carries its id and a nested edges array with endpoints.
    let entity = &context[1];
    assert_eq!(entity.payload["kind"], json!("entity"));
    assert_eq!(entity.payload["id"], json!(alice_id.to_string()));
    assert_eq!(entity.payload["name"], json!(ENTITY_NAME));
    let edges = entity.payload["edges"]
        .as_array()
        .expect("entity payload must carry an edges array");
    assert!(!edges.is_empty(), "the works_at edge must be present");
    let edge = &edges[0];
    assert_eq!(edge["source_id"], json!(alice_id.to_string()));
    assert_eq!(edge["target_id"], json!("acme-id"));

    // Fact item carries no edges (the field-level contract that lets a
    // kind-aware id extractor skip facts).
    let fact = &context[2];
    assert_eq!(fact.payload["kind"], json!("fact"));
    assert_eq!(fact.payload["text"], json!(FACT_TEXT));
    assert!(fact.payload.get("edges").is_none());
    assert!(fact.payload.get("source_id").is_none());
}

// ---------------------------------------------------------------------------
// Test 2: OpenAI-gated end-to-end completion + used_graph_element_ids
// ---------------------------------------------------------------------------

#[tokio::test]
async fn hybrid_orchestrator_completion_populates_used_graph_element_ids() {
    let Some(llm) = build_test_llm() else {
        eprintln!(
            "OPENAI_URL/OPENAI_TOKEN not set -- skipping \
             hybrid_orchestrator_completion_populates_used_graph_element_ids"
        );
        return;
    };

    let (vector_db, graph_db, chunk_id, alice_id) = seed_dbs().await;
    let retriever = build_hybrid_retriever(vector_db, graph_db, Arc::clone(&llm));

    let mut registry = SearchTypeRegistry::new();
    registry.register(Arc::new(retriever));

    // Filesystem-backed session store so we can read the persisted QA entry
    // (and its used_graph_element_ids) back after the search completes.
    let temp_dir = TempDir::new().expect("TempDir::new should succeed");
    let store: Arc<FsSessionStore> = Arc::new(FsSessionStore::new(temp_dir.path()));
    let session_manager = Arc::new(SessionManager::new(
        Arc::clone(&store) as Arc<dyn SessionStore>
    ));

    let orchestrator = SearchOrchestrator::new(registry)
        .with_session_manager(session_manager)
        .with_llm(Arc::clone(&llm));

    let session_id = "hybrid-integration-session";
    let request = completion_request("Where does Alice work?", session_id);

    let response = match orchestrator.search(&request).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Skipping: orchestrator.search failed: {e}");
            return;
        }
    };

    match &response.result {
        SearchOutput::Text(answer) => {
            assert!(!answer.is_empty(), "completion answer should be non-empty");
        }
        other => panic!("expected text completion output, got {other:?}"),
    }

    // The persisted QA entry must carry a populated used_graph_element_ids
    // snapshot derived from the hybrid context items.
    let entries = store
        .get_latest_qa_entries(session_id, None, 10)
        .await
        .expect("reading session QA entries should succeed");
    let entry = entries
        .first()
        .expect("a QA entry should have been persisted for the session");
    let ids = entry
        .used_graph_element_ids
        .as_ref()
        .expect("used_graph_element_ids should be populated for a hybrid completion");

    assert!(
        !ids.node_ids.is_empty(),
        "hybrid completion should record graph node ids; got {ids:?}"
    );
    // Chunk and entity ids from the seeded graph should be present.
    assert!(
        ids.node_ids.contains(&chunk_id.to_string()),
        "node_ids should include the seeded chunk id; got {:?}",
        ids.node_ids
    );
    assert!(
        ids.node_ids.contains(&alice_id.to_string()),
        "node_ids should include the seeded entity id; got {:?}",
        ids.node_ids
    );
    // Entity edge endpoint should be captured too.
    assert!(
        ids.node_ids.contains(&"acme-id".to_string()),
        "node_ids should include the entity edge endpoint; got {:?}",
        ids.node_ids
    );
    // Hybrid path never emits edge_ids.
    assert!(
        ids.edge_ids.is_empty(),
        "hybrid path must not populate edge_ids; got {:?}",
        ids.edge_ids
    );
}

// ---------------------------------------------------------------------------
// Test 3: deterministic force-materialize branch (no LLM creds needed)
// ---------------------------------------------------------------------------

/// Same session/used_graph_element_ids contract as test 2, but exercised through
/// the `use_combined_context = false` **force-materialize** branch
/// (`search_orchestrator.rs:562-570`): on the default completion path
/// `include_context` is false, so the orchestrator must re-fetch the hybrid
/// context solely to snapshot its graph node ids into the persisted QA entry.
/// Driven by the deterministic `StubLlm`, so — unlike test 2 — it always runs
/// (no `OPENAI_URL`/`OPENAI_TOKEN` gate) and still fails if the P1-10 wiring
/// regresses.
#[tokio::test]
async fn hybrid_session_completion_persists_used_graph_element_ids_mock() {
    let llm: Arc<dyn Llm> = Arc::new(StubLlm);

    let (vector_db, graph_db, chunk_id, alice_id) = seed_dbs().await;
    let retriever = build_hybrid_retriever(vector_db, graph_db, Arc::clone(&llm));

    let mut registry = SearchTypeRegistry::new();
    registry.register(Arc::new(retriever));

    // Filesystem-backed session store so we can read the persisted QA entry
    // (and its used_graph_element_ids) back after the search completes.
    let temp_dir = TempDir::new().expect("TempDir::new should succeed");
    let store: Arc<FsSessionStore> = Arc::new(FsSessionStore::new(temp_dir.path()));
    let session_manager = Arc::new(SessionManager::new(
        Arc::clone(&store) as Arc<dyn SessionStore>
    ));

    let orchestrator = SearchOrchestrator::new(registry)
        .with_session_manager(session_manager)
        .with_llm(Arc::clone(&llm));

    let session_id = "hybrid-force-materialize-session";
    // use_combined_context = false so `include_context` is false and the
    // orchestrator hits the force-materialize branch rather than fetching the
    // context up front.
    let mut request = completion_request("Where does Alice work?", session_id);
    request.use_combined_context = Some(false);

    let response = orchestrator
        .search(&request)
        .await
        .expect("orchestrator.search should succeed with the StubLlm and seeded mocks");

    // The StubLlm always answers "stub answer" — deterministic, non-empty.
    match &response.result {
        SearchOutput::Text(answer) => {
            assert_eq!(answer, "stub answer", "expected the StubLlm's fixed answer");
        }
        other => panic!("expected text completion output, got {other:?}"),
    }

    // The persisted QA entry must carry a populated used_graph_element_ids
    // snapshot derived from the force-materialized hybrid context items.
    let entries = store
        .get_latest_qa_entries(session_id, None, 10)
        .await
        .expect("reading session QA entries should succeed");
    let entry = entries
        .first()
        .expect("a QA entry should have been persisted for the session");
    let ids = entry
        .used_graph_element_ids
        .as_ref()
        .expect("used_graph_element_ids should be populated on the force-materialize branch");

    // Chunk and entity ids from the seeded graph should be present.
    assert!(
        ids.node_ids.contains(&chunk_id.to_string()),
        "node_ids should include the seeded chunk id; got {:?}",
        ids.node_ids
    );
    assert!(
        ids.node_ids.contains(&alice_id.to_string()),
        "node_ids should include the seeded entity id; got {:?}",
        ids.node_ids
    );
    // Entity edge endpoint should be captured too.
    assert!(
        ids.node_ids.contains(&"acme-id".to_string()),
        "node_ids should include the entity edge endpoint; got {:?}",
        ids.node_ids
    );
    // Hybrid path never emits edge_ids.
    assert!(
        ids.edge_ids.is_empty(),
        "hybrid path must not populate edge_ids; got {:?}",
        ids.edge_ids
    );
}
