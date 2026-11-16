//! Retriever-flow tests for [`TemporalHybridRetriever`]: ports of the
//! retriever cases in Python's
//! `tests/unit/modules/retrieval/test_temporal_hybrid_retriever.py`, run on
//! `MockGraphDB` / `MockVectorDB` and a scripted LLM instead of monkeypatched
//! engines.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use chrono::TimeZone;
use cognee_embedding::EmbeddingResult;
use cognee_graph::{GraphDBTraitExt, MockGraphDB};
use cognee_llm::{GenerationResponse, LlmError, LlmResult};
use cognee_models::Timestamp;
use cognee_vector::{MockVectorDB, VectorPoint};
use serde_json::{Value, json};
use uuid::Uuid;

use super::*;
use crate::retrievers::hybrid::{EdgeBullet, EntityResult, FactCandidates};
use crate::types::SearchItem;

// --- fakes -------------------------------------------------------------------

/// Answers the window extraction with `interval` (or fails with `error`) and
/// the completion with `"answer"`, counting both.
struct ScriptedLlm {
    interval: Value,
    error: Option<String>,
    answer: String,
    structured_calls: AtomicUsize,
    generate_calls: AtomicUsize,
    last_prompt: Mutex<Vec<Message>>,
}

impl ScriptedLlm {
    fn new(interval: Value) -> Self {
        Self {
            interval,
            error: None,
            answer: "answer".to_string(),
            structured_calls: AtomicUsize::new(0),
            generate_calls: AtomicUsize::new(0),
            last_prompt: Mutex::new(vec![]),
        }
    }

    fn answering(answer: &str) -> Self {
        Self {
            answer: answer.to_string(),
            ..Self::new(json!({}))
        }
    }

    fn failing(message: &str) -> Self {
        Self {
            error: Some(message.to_string()),
            ..Self::new(Value::Null)
        }
    }
}

#[async_trait]
impl Llm for ScriptedLlm {
    async fn generate(
        &self,
        messages: Vec<Message>,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        self.generate_calls.fetch_add(1, Ordering::SeqCst);
        *self.last_prompt.lock().unwrap() = messages;
        Ok(GenerationResponse {
            content: self.answer.clone(),
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
        self.structured_calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            messages[0].content,
            matching::QUERY_INTERVAL_PROMPT,
            "the window extraction sends the vendored prompt"
        );
        match &self.error {
            Some(message) => Err(LlmError::ApiError(message.clone())),
            None => Ok(self.interval.clone()),
        }
    }

    fn model(&self) -> &str {
        "scripted"
    }
}

/// Embeds every query to `[1, 0]`, counting calls.
#[derive(Default)]
struct CountingEmbedding {
    calls: AtomicUsize,
}

#[async_trait]
impl EmbeddingEngine for CountingEmbedding {
    async fn embed(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
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

// --- fixtures ----------------------------------------------------------------

/// A deterministic UUID per readable name: vector rows are keyed by UUID.
fn id(name: &str) -> String {
    Uuid::new_v5(&Uuid::NAMESPACE_OID, name.as_bytes()).to_string()
}

fn utc(year: i32, month: u32, day: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(year, month, day, 0, 0, 0).unwrap()
}

fn window(start: DateTime<Utc>, end: DateTime<Utc>) -> ExtractedWindow {
    ExtractedWindow::Window {
        start: Some(start),
        end: Some(end),
    }
}

fn chunk(name: &str) -> SearchItem {
    SearchItem {
        id: None,
        score: None,
        payload: json!({"id": id(name), "text": format!("text of {name}")}),
    }
}

fn entity(name: &str, edge_type_ids: &[&str]) -> EntityResult {
    EntityResult {
        id: id(name),
        name: name.to_string(),
        description: Some(format!("about {name}")),
        entity_type: None,
        edges: edge_type_ids
            .iter()
            .map(|edge_type_id| EdgeBullet {
                text: format!("bullet {edge_type_id}"),
                source: None,
                target: None,
                source_id: None,
                relationship: None,
                target_id: None,
                edge_type_id: Some((*edge_type_id).to_string()),
            })
            .collect(),
        covered_edge_type_ids: vec![],
        dataset_ids: vec![],
    }
}

fn fact_hit(id: &str, text: &str) -> SearchItem {
    SearchItem {
        id: None,
        score: None,
        payload: json!({"id": id, "text": text}),
    }
}

fn hits() -> Vec<SearchItem> {
    vec![
        fact_hit("f_atlas", "Atlas was founded in 1950"),
        fact_hit("f_helios", "Helios launched in 1898"),
        fact_hit("f_other", "Something else entirely happened"),
    ]
}

/// Python's `_candidates()`: chunks c1..c3, entities e1, e2, three fact hits.
fn candidates() -> HybridCandidates {
    HybridCandidates {
        chunks: vec![chunk("c1"), chunk("c2"), chunk("c3")],
        chunk_summaries: HashMap::new(),
        entities: vec![entity("e1", &[]), entity("e2", &[])],
        fact_candidates: FactCandidates {
            edge_hits: hits(),
            facts_top_k: 2,
            ..FactCandidates::default()
        },
    }
}

async fn add_timestamp(graph: &MockGraphDB, text: &str) -> String {
    let timestamp = Timestamp::from_text(text).unwrap();
    graph.add_node(&timestamp).await.unwrap();
    timestamp.base.id.to_string()
}

async fn add_node(graph: &MockGraphDB, name: &str, node_type: &str) {
    graph
        .add_node_raw(json!({"id": id(name), "name": name, "type": node_type}))
        .await
        .unwrap();
}

async fn add_edge(graph: &MockGraphDB, source: &str, target: &str, relationship: &str) {
    graph
        .add_edge(source, target, relationship, None)
        .await
        .unwrap();
}

struct Harness {
    retriever: TemporalHybridRetriever,
    graph: Arc<MockGraphDB>,
    vector: Arc<MockVectorDB>,
    llm: Arc<ScriptedLlm>,
    embedding: Arc<CountingEmbedding>,
}

fn harness_with(llm: ScriptedLlm, top_k: Option<usize>, candidate_top_k: Option<usize>) -> Harness {
    let graph = Arc::new(MockGraphDB::new());
    let vector = Arc::new(MockVectorDB::new());
    let llm = Arc::new(llm);
    let embedding = Arc::new(CountingEmbedding::default());
    let retriever = TemporalHybridRetriever::new(
        Arc::clone(&vector) as Arc<dyn VectorDB>,
        Arc::clone(&embedding) as Arc<dyn EmbeddingEngine>,
        Arc::clone(&graph) as Arc<dyn GraphDBTrait>,
        Arc::clone(&llm) as Arc<dyn Llm>,
        top_k,
        candidate_top_k,
        None,
        None,
        None,
        None,
    );
    Harness {
        retriever,
        graph,
        vector,
        llm,
        embedding,
    }
}

/// `top_k = 2`, `candidate_top_k = 20`, as Python's `_retriever` fixture.
fn harness() -> Harness {
    harness_with(ScriptedLlm::new(json!({})), Some(2), Some(20))
}

impl Harness {
    fn limits(&self) -> Limits {
        self.retriever.limits(&SearchParams::default()).unwrap()
    }

    fn lanes(&self) -> HybridLanes {
        self.retriever
            .lanes(&SearchParams::default(), self.limits())
    }

    async fn rerank(
        &self,
        candidates: HybridCandidates,
        window: ExtractedWindow,
    ) -> TemporalRetrieval {
        self.retriever
            .rerank(candidates, window, self.limits(), &self.lanes(), false)
            .await
            .unwrap()
    }

    /// Index a `DocumentChunk_text` row so the window expansion can resolve it.
    async fn index_chunk(&self, name: &str, sets: &[&str]) {
        let uuid = Uuid::parse_str(&id(name)).unwrap();
        let mut point = VectorPoint::new(uuid, vec![0.0, 1.0])
            .with_metadata("id", json!(id(name)))
            .with_metadata("text", json!(format!("text of {name}")))
            .with_metadata(
                "external_metadata",
                json!(r#"{"allowed":"visible","secret":"hidden"}"#),
            );
        if !sets.is_empty() {
            point = point.with_metadata("belongs_to_set", json!(sets));
        }
        if !self
            .vector
            .has_collection("DocumentChunk", "text")
            .await
            .unwrap()
        {
            self.vector
                .create_collection("DocumentChunk", "text", 2)
                .await
                .unwrap();
        }
        self.vector
            .index_points("DocumentChunk", "text", &[point])
            .await
            .unwrap();
    }
}

/// The ids of the `"kind": "chunk"` / `"entity"` / `"fact"` items, in order.
fn kind_ids(context: &SearchContext, kind: &str) -> Vec<String> {
    context
        .iter()
        .filter(|item| item.payload.get("kind").and_then(Value::as_str) == Some(kind))
        .filter_map(|item| item.payload.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

fn names(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|name| id(name)).collect()
}

/// A graph where c2 and c3 contain the 1950 timestamp and e2 has an edge into
/// it. Returns the timestamp id.
async fn graph_1950(graph: &MockGraphDB) -> String {
    let ts = add_timestamp(graph, "1950").await;
    for name in ["c1", "c2", "c3"] {
        add_node(graph, name, "DocumentChunk").await;
    }
    for name in ["e1", "e2"] {
        add_node(graph, name, "Entity").await;
    }
    add_edge(graph, &id("c2"), &ts, "contains").await;
    add_edge(graph, &id("c3"), &ts, "contains").await;
    add_edge(graph, &id("e2"), &ts, "founded_at").await;
    ts
}

// --- limits --------------------------------------------------------------------

#[test]
fn limits_reject_bad_values_and_default_candidates_to_four_times_top_k() {
    let bad = harness_with(ScriptedLlm::new(json!({})), Some(2), Some(1));
    assert!(matches!(
        bad.retriever.limits(&SearchParams::default()),
        Err(SearchError::InvalidInput(_))
    ));
    let zero = harness_with(ScriptedLlm::new(json!({})), Some(0), Some(5));
    assert!(zero.retriever.limits(&SearchParams::default()).is_err());

    let three = harness_with(ScriptedLlm::new(json!({})), Some(3), None);
    assert_eq!(
        three.retriever.limits(&SearchParams::default()).unwrap(),
        Limits {
            top_k: 3,
            candidate_top_k: 12
        }
    );
    // No top_k anywhere resolves to 5 before the arithmetic.
    let unset = harness_with(ScriptedLlm::new(json!({})), None, None);
    assert_eq!(
        unset.retriever.limits(&SearchParams::default()).unwrap(),
        Limits {
            top_k: 5,
            candidate_top_k: 20
        }
    );
    // The request overrides both.
    let params = SearchParams {
        top_k: Some(4),
        candidate_top_k: Some(9),
        ..SearchParams::default()
    };
    assert_eq!(
        unset.retriever.limits(&params).unwrap(),
        Limits {
            top_k: 4,
            candidate_top_k: 9
        }
    );
    let lanes = unset
        .retriever
        .lanes(&params, unset.retriever.limits(&params).unwrap());
    assert_eq!(
        (lanes.chunks_top_k, lanes.entities_top_k, lanes.facts_top_k),
        (9, 9, 4)
    );
}

#[test]
fn fallback_reasons_have_pythons_names() {
    use TemporalFallbackReason::*;
    let names: Vec<&str> = [
        EmptyGraph,
        NoTimeConstraint,
        InvalidInterval,
        NoTemporalMatch,
        NoCandidateOverlap,
    ]
    .iter()
    .map(|reason| reason.as_str())
    .collect();
    assert_eq!(
        names,
        [
            "empty_graph",
            "no_time_constraint",
            "invalid_interval",
            "no_temporal_match",
            "no_candidate_overlap"
        ]
    );
    assert_eq!(NoTimeConstraint.to_string(), "no_time_constraint");
}

// --- the retrieve() entry point -------------------------------------------------

#[tokio::test]
async fn empty_graph_makes_no_llm_or_embedding_call() {
    let h = harness();
    let blank = h.retriever.retrieve("   ", &SearchParams::default()).await;
    assert!(matches!(blank, Err(SearchError::InvalidInput(message)) if message.contains("blank")));

    let result = h
        .retriever
        .retrieve("in 1950", &SearchParams::default())
        .await
        .unwrap();
    assert_eq!(
        result.diagnostics.reason,
        Some(TemporalFallbackReason::EmptyGraph)
    );
    assert!(result.context.is_empty());
    assert_eq!(h.llm.structured_calls.load(Ordering::SeqCst), 0);
    assert_eq!(h.embedding.calls.load(Ordering::SeqCst), 0);
    assert!(
        !h.graph
            .get_call_log()
            .iter()
            .any(|call| call == "get_temporal_anchors")
    );

    // And the completion over it skips the LLM.
    let output = h
        .retriever
        .get_completion(
            "in 1950",
            None,
            &SessionContext::default(),
            &SearchParams::default(),
        )
        .await
        .unwrap();
    assert!(matches!(output, SearchOutput::Texts(ref texts) if texts.is_empty()));
    assert_eq!(h.llm.generate_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn llm_errors_propagate_instead_of_falling_back() {
    let h = harness_with(ScriptedLlm::failing("provider down"), Some(2), None);
    add_node(&h.graph, "c1", "DocumentChunk").await;
    let error = h
        .retriever
        .retrieve("in 1950", &SearchParams::default())
        .await
        .unwrap_err();
    assert!(
        matches!(&error, SearchError::LlmError(message) if message.contains("provider down")),
        "{error:?}"
    );
}

#[tokio::test]
async fn batches_are_rejected() {
    let h = harness();
    let queries = vec!["in 1950".to_string()];
    assert!(matches!(
        h.retriever
            .get_context_batch(&queries, &SearchParams::default())
            .await,
        Err(SearchError::InvalidInput(_))
    ));
}

// --- rerank ---------------------------------------------------------------------

#[tokio::test]
async fn reranks_by_the_candidates_anchors() {
    let h = harness();
    let ts = graph_1950(&h.graph).await;
    let result = h
        .rerank(candidates(), window(utc(1950, 1, 1), utc(1951, 1, 1)))
        .await;

    let diagnostics = &result.diagnostics;
    assert_eq!(diagnostics.reason, None);
    assert_eq!(
        diagnostics.interval,
        (Some(utc(1950, 1, 1)), Some(utc(1951, 1, 1)))
    );
    assert_eq!(diagnostics.anchors.timestamp_ids, HashSet::from([ts]));
    assert_eq!(
        diagnostics.anchors.chunk_ids,
        names(&["c2", "c3"]).into_iter().collect()
    );
    assert_eq!(
        diagnostics.anchors.entity_ids,
        names(&["e2"]).into_iter().collect()
    );
    // Nothing outside the candidates hangs off the window: no expansion.
    assert!(diagnostics.expansion.is_empty());
    assert_eq!(kind_ids(&result.context, "chunk"), names(&["c2", "c3"]));
    assert_eq!(kind_ids(&result.context, "entity"), names(&["e2", "e1"]));
    assert_eq!(
        kind_ids(&diagnostics.baseline, "chunk"),
        names(&["c1", "c2"])
    );
}

/// Voskhod 2: the chunk whose timestamp is 1965-03-18 ranks ahead of chunks
/// that only reach the year node "1965", which also overlaps the day.
#[tokio::test]
async fn the_chunk_dated_to_the_day_comes_first() {
    let h = harness();
    let year = add_timestamp(&h.graph, "1965").await;
    let day = add_timestamp(&h.graph, "1965-03-18").await;
    for name in ["c1", "c2", "c3"] {
        add_node(&h.graph, name, "DocumentChunk").await;
    }
    add_edge(&h.graph, &id("c1"), &year, "contains").await;
    add_edge(&h.graph, &id("c2"), &year, "contains").await;
    add_edge(&h.graph, &id("c3"), &year, "contains").await;
    add_edge(&h.graph, &id("c3"), &day, "contains").await;

    let result = h
        .rerank(candidates(), window(utc(1965, 3, 18), utc(1965, 3, 19)))
        .await;
    assert_eq!(kind_ids(&result.context, "chunk"), names(&["c3", "c1"]));
    assert_eq!(
        result.diagnostics.anchors.chunk_timestamps[&id("c3")],
        HashSet::from([day, year])
    );
}

/// Anchors reached through an entity the chunk `contains` count like a
/// chunk's own date — the default-cognify shape (`chunk -contains-> entity
/// -*_at-> Timestamp`).
#[tokio::test]
async fn a_date_reached_through_a_contained_entity_anchors_the_chunk() {
    let h = harness();
    let day = add_timestamp(&h.graph, "1965-03-18").await;
    for name in ["c1", "c2", "c3"] {
        add_node(&h.graph, name, "DocumentChunk").await;
    }
    add_node(&h.graph, "voskhod", "Entity").await;
    add_edge(&h.graph, &id("c3"), &id("voskhod"), "contains").await;
    add_edge(&h.graph, &id("voskhod"), &day, "launched_at").await;

    let result = h
        .rerank(candidates(), window(utc(1965, 3, 18), utc(1965, 3, 19)))
        .await;
    assert_eq!(result.diagnostics.reason, None);
    assert_eq!(kind_ids(&result.context, "chunk"), names(&["c3", "c1"]));
    assert!(
        result
            .diagnostics
            .anchors
            .entity_ids
            .contains(&id("voskhod"))
    );
}

#[tokio::test]
async fn facts_are_selected_against_the_entities_shown() {
    let h = harness_with(ScriptedLlm::new(json!({})), Some(1), Some(20));
    let ts = add_timestamp(&h.graph, "1950").await;
    add_node(&h.graph, "c1", "DocumentChunk").await;
    add_node(&h.graph, "atlas", "Entity").await;
    add_edge(&h.graph, &id("c1"), &ts, "contains").await;
    add_edge(&h.graph, &id("atlas"), &ts, "founded_at").await;
    let candidates = HybridCandidates {
        chunks: vec![chunk("c1")],
        entities: vec![
            entity("atlas", &["f_atlas"]),
            entity("helios", &["f_helios"]),
        ],
        fact_candidates: FactCandidates {
            edge_hits: hits(),
            facts_top_k: 2,
            ..FactCandidates::default()
        },
        ..HybridCandidates::default()
    };

    let result = h
        .rerank(candidates, window(utc(1950, 1, 1), utc(1951, 1, 1)))
        .await;
    // Helios is cut, so its fact stands alone; Atlas' is under Atlas.
    assert_eq!(kind_ids(&result.context, "entity"), names(&["atlas"]));
    assert_eq!(kind_ids(&result.context, "fact"), ["f_helios", "f_other"]);
    assert_eq!(
        kind_ids(&result.diagnostics.baseline, "fact"),
        ["f_helios", "f_other"]
    );
}

#[tokio::test]
async fn the_no_entity_fact_budget_follows_top_k_not_the_candidate_fetch() {
    let h = harness();
    let ts = add_timestamp(&h.graph, "1950").await;
    add_node(&h.graph, "c1", "DocumentChunk").await;
    add_edge(&h.graph, &id("c1"), &ts, "contains").await;
    let candidates = HybridCandidates {
        chunks: vec![chunk("c1")],
        fact_candidates: FactCandidates {
            edge_hits: (0..100)
                .map(|i| fact_hit(&format!("f{i}"), &format!("fact number {i} happened")))
                .collect(),
            facts_top_k: 2,
            // What the fetch sets: candidate_top_k (20) x max_edges_per_entity.
            entity_edge_budget: 200,
            ..FactCandidates::default()
        },
        ..HybridCandidates::default()
    };
    let result = h
        .rerank(candidates, window(utc(1950, 1, 1), utc(1951, 1, 1)))
        .await;
    let budget = 2 * h.lanes().max_edges_per_entity;
    assert_eq!(kind_ids(&result.context, "fact").len(), budget);
    assert_eq!(kind_ids(&result.diagnostics.baseline, "fact").len(), budget);
}

#[tokio::test]
async fn no_window_falls_back_to_the_baseline_with_its_reason() {
    for reason in [
        TemporalFallbackReason::NoTimeConstraint,
        TemporalFallbackReason::InvalidInterval,
    ] {
        let h = harness();
        graph_1950(&h.graph).await;
        let result = h
            .rerank(candidates(), ExtractedWindow::NoWindow(reason))
            .await;
        assert_eq!(result.diagnostics.reason, Some(reason));
        assert_eq!(kind_ids(&result.context, "chunk"), names(&["c1", "c2"]));
        assert_eq!(
            kind_ids(&result.context, "chunk"),
            kind_ids(&result.diagnostics.baseline, "chunk")
        );
        assert_eq!(result.diagnostics.interval, (None, None));
        // The graph's dates were never consulted.
        let calls = h.graph.get_call_log();
        assert!(!calls.iter().any(|call| call == "get_timestamps_in_range"));
        assert!(!calls.iter().any(|call| call == "get_temporal_anchors"));
    }
}

#[tokio::test]
async fn a_window_the_graph_has_no_time_in_is_no_temporal_match() {
    let h = harness();
    graph_1950(&h.graph).await;
    let result = h
        .rerank(candidates(), window(utc(1800, 1, 1), utc(1801, 1, 1)))
        .await;
    assert_eq!(
        result.diagnostics.reason,
        Some(TemporalFallbackReason::NoTemporalMatch)
    );
    assert_eq!(kind_ids(&result.context, "chunk"), names(&["c1", "c2"]));
}

#[tokio::test]
async fn window_times_outside_the_candidates_are_no_candidate_overlap() {
    let h = harness();
    let ts = add_timestamp(&h.graph, "1950").await;
    // Something dated in the window, but no chunk row to retrieve for it.
    add_node(&h.graph, "tbl", "Table").await;
    add_edge(&h.graph, &id("tbl"), &ts, "whatever").await;
    let result = h
        .rerank(candidates(), window(utc(1950, 1, 1), utc(1951, 1, 1)))
        .await;
    assert_eq!(
        result.diagnostics.reason,
        Some(TemporalFallbackReason::NoCandidateOverlap)
    );
    assert!(result.diagnostics.expansion.is_empty());
    assert_eq!(kind_ids(&result.context, "chunk"), names(&["c1", "c2"]));
}

#[tokio::test]
async fn an_anchored_candidate_already_first_changes_nothing() {
    let h = harness();
    let ts = add_timestamp(&h.graph, "1950").await;
    add_node(&h.graph, "c1", "DocumentChunk").await;
    add_edge(&h.graph, &id("c1"), &ts, "contains").await;
    let result = h
        .rerank(candidates(), window(utc(1950, 1, 1), utc(1951, 1, 1)))
        .await;
    // Informational: the rerank ran but moved nothing.
    assert_eq!(
        result.diagnostics.reason,
        Some(TemporalFallbackReason::NoCandidateOverlap)
    );
    assert!(!result.diagnostics.anchors.is_empty());
    assert_eq!(kind_ids(&result.context, "chunk"), names(&["c1", "c2"]));
}

// --- window expansion --------------------------------------------------------------

#[tokio::test]
async fn the_window_adds_the_chunks_it_points_at_but_not_entities() {
    let h = harness_with(ScriptedLlm::new(json!({})), Some(4), Some(20));
    let year = add_timestamp(&h.graph, "1950").await;
    let month = add_timestamp(&h.graph, "1950-06").await;
    for name in ["c1", "r1", "r2", "r3"] {
        add_node(&h.graph, name, "DocumentChunk").await;
        h.index_chunk(name, &[]).await;
    }
    add_node(&h.graph, "atlas", "Entity").await;
    add_node(&h.graph, "tbl", "Table").await;
    add_edge(&h.graph, &id("r2"), &month, "contains").await;
    add_edge(&h.graph, &id("r1"), &year, "contains").await;
    add_edge(&h.graph, &id("r3"), &year, "contains").await;
    add_edge(&h.graph, &id("c1"), &year, "contains").await;
    add_edge(&h.graph, &id("atlas"), &year, "founded_at").await;
    add_edge(&h.graph, &id("tbl"), &year, "whatever").await;

    let result = h
        .rerank(candidates(), window(utc(1950, 1, 1), utc(1951, 1, 1)))
        .await;
    assert_eq!(result.diagnostics.reason, None);
    assert_eq!(
        result.diagnostics.expansion,
        names(&["r1", "r2", "r3"]).into_iter().collect()
    );
    // Entities attached to the window stay out of the entity list.
    assert_eq!(kind_ids(&result.context, "entity"), names(&["e1", "e2"]));
    // Every date fits a year question fully, so the anchored chunks tie and
    // keep candidate order: hybrid's own anchored candidate first, then the
    // window's rows as added — tightest timestamp (the month) first.
    assert_eq!(
        kind_ids(&result.context, "chunk"),
        names(&["c1", "r2", "r1", "r3"])
    );
    assert!(result.context.iter().all(|item| {
        !item
            .payload
            .as_object()
            .is_some_and(|payload| payload.contains_key("external_metadata"))
    }));
    assert_eq!(
        kind_ids(&result.diagnostics.baseline, "chunk"),
        names(&["c1", "c2", "c3"])
    );
}

/// A one-day window also overlaps the bare-year node. 30 chunks hang off the
/// year and one off the day; with a budget of 20 the day's chunk must still
/// get in.
#[tokio::test]
async fn the_expansion_budget_goes_to_the_tightest_timestamp_first() {
    let h = harness();
    let year = add_timestamp(&h.graph, "1965").await;
    let day = add_timestamp(&h.graph, "1965-03-18").await;
    for i in 0..30 {
        let name = format!("y{i:02}");
        add_node(&h.graph, &name, "DocumentChunk").await;
        h.index_chunk(&name, &[]).await;
        add_edge(&h.graph, &id(&name), &year, "contains").await;
    }
    add_node(&h.graph, "voskhod", "DocumentChunk").await;
    h.index_chunk("voskhod", &[]).await;
    add_edge(&h.graph, &id("voskhod"), &day, "contains").await;

    let result = h
        .rerank(candidates(), window(utc(1965, 3, 18), utc(1965, 3, 19)))
        .await;
    assert!(result.diagnostics.expansion.contains(&id("voskhod")));
    assert_eq!(result.diagnostics.expansion.len(), 20);
    assert_eq!(kind_ids(&result.context, "chunk")[0], id("voskhod"));
}

#[tokio::test]
async fn the_expansion_respects_the_node_set_filter() {
    let h = harness();
    let ts = add_timestamp(&h.graph, "1950").await;
    for (name, sets) in [("r1", vec![]), ("r3", vec!["other"])] {
        add_node(&h.graph, name, "DocumentChunk").await;
        h.index_chunk(name, &sets).await;
        add_edge(&h.graph, &id(name), &ts, "contains").await;
    }
    let lanes = HybridLanes {
        node_name: Some(vec!["other".to_string()]),
        ..h.lanes()
    };
    let result = h
        .retriever
        .rerank(
            candidates(),
            window(utc(1950, 1, 1), utc(1951, 1, 1)),
            h.limits(),
            &lanes,
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        result.diagnostics.expansion,
        names(&["r3"]).into_iter().collect()
    );
}

// --- include_references -----------------------------------------------------------

const CITED_ANSWER: &str = "Atlas was founded during the expedition.";

/// A TEMPORAL retriever over a non-empty graph whose chunk lane holds one
/// citable chunk (document name and index) that grounds [`CITED_ANSWER`].
async fn citable_temporal() -> (Arc<TemporalHybridRetriever>, Uuid) {
    let h = harness_with(ScriptedLlm::answering(CITED_ANSWER), Some(2), None);
    add_node(&h.graph, "c1", "DocumentChunk").await;
    let chunk_id = Uuid::new_v4();
    h.vector
        .create_collection("DocumentChunk", "text", 2)
        .await
        .unwrap();
    h.vector
        .index_points(
            "DocumentChunk",
            "text",
            &[VectorPoint::new(chunk_id, vec![1.0, 0.0])
                .with_metadata("id", json!(chunk_id.to_string()))
                .with_metadata(
                    "text",
                    json!("Atlas was founded in 1950 by the expedition."),
                )
                .with_metadata("document_name", json!("atlas.md"))
                .with_metadata("document_id", json!("data-1"))
                .with_metadata("chunk_index", json!(0))],
        )
        .await
        .unwrap();
    (Arc::new(h.retriever), chunk_id)
}

fn temporal_orchestrator(
    retriever: Arc<TemporalHybridRetriever>,
) -> crate::orchestration::SearchOrchestrator {
    let mut registry = crate::orchestration::SearchTypeRegistry::new();
    registry.register(retriever as Arc<dyn SearchRetriever>);
    crate::orchestration::SearchOrchestrator::new(registry)
}

fn answer_text(output: &SearchOutput) -> String {
    match output {
        SearchOutput::Text(text) => text.clone(),
        SearchOutput::Texts(texts) if texts.len() == 1 => texts[0].clone(),
        other => panic!("expected one text answer, got {other:?}"),
    }
}

fn assert_cites_the_chunk(text: &str, chunk_id: Uuid) {
    assert!(
        text.starts_with(&format!("{CITED_ANSWER}\n\nEvidence:\n")),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "- chunk 1 of document atlas.md (data_id: data-1, chunk_id: {chunk_id})"
        )),
        "{text}"
    );
}

/// Python's `TemporalHybridRetriever` inherits `HybridRetriever`'s
/// `append_references`; an explicit TEMPORAL search cites its chunk lane.
#[tokio::test]
async fn explicit_temporal_cites_its_chunk_lane_under_include_references() {
    let (retriever, chunk_id) = citable_temporal().await;
    let orchestrator = temporal_orchestrator(retriever);
    let mut request: crate::types::SearchRequest =
        serde_json::from_value(json!({"query_text": "What happened in 1950?"})).unwrap();
    request.search_type = SearchType::Temporal;

    let plain = orchestrator.search(&request).await.unwrap();
    assert_eq!(answer_text(&plain.result), CITED_ANSWER);

    request.include_references = true;
    let cited = orchestrator.search(&request).await.unwrap();
    assert_cites_the_chunk(&answer_text(&cited.result), chunk_id);

    // A response schema (Python `response_model is not str`) cites nothing.
    let params = SearchParams {
        include_references: true,
        response_schema: Some(json!({"type": "object"})),
        ..SearchParams::default()
    };
    let context = vec![SearchItem {
        id: None,
        score: None,
        payload: json!({"kind": "chunk", "id": "x", "text": CITED_ANSWER,
                        "document_name": "atlas.md", "chunk_index": 0}),
    }];
    let untouched = append_with_a_fresh_temporal(&context, &params);
    assert_eq!(untouched, CITED_ANSWER);
}

fn append_with_a_fresh_temporal(context: &SearchContext, params: &SearchParams) -> String {
    let h = harness();
    answer_text(&h.retriever.append_references(
        SearchOutput::Text(CITED_ANSWER.to_string()),
        context,
        params,
    ))
}

/// A recall the router sends to TEMPORAL (no explicit type) carries the
/// same citations, as Python's factory passes `include_references` to it.
#[tokio::test]
async fn auto_routed_temporal_cites_its_chunk_lane_under_include_references() {
    let (retriever, chunk_id) = citable_temporal().await;
    let orchestrator = temporal_orchestrator(retriever);
    let span = tracing::Span::none();
    let query = "What happened in 1950?";
    let options = crate::recall_scope::RecallOptions {
        include_references: true,
        ..Default::default()
    };
    let plan = crate::recall_scope::plan_graph(
        query,
        None,
        true,
        None,
        None,
        None,
        &orchestrator,
        &span,
        Some(&options),
    )
    .unwrap();
    let (_items, used, auto_routed, response) = crate::recall_scope::run_graph(
        &plan,
        query,
        None,
        None,
        2,
        None,
        &orchestrator,
        &span,
        Some(&options),
    )
    .await
    .unwrap();
    assert_eq!(used, SearchType::Temporal);
    assert!(auto_routed);
    assert_cites_the_chunk(&answer_text(&response.result), chunk_id);
}

// --- session turns (SDK-1461) --------------------------------------------------

/// One session lane: `per_kind` chunk, entity and fact items, ids prefixed by
/// the lane so the merge cannot dedupe across lanes.
fn session_lane(prefix: &str, per_kind: usize) -> SearchContext {
    ["chunk", "entity", "fact"]
        .iter()
        .flat_map(|kind| {
            (0..per_kind).map(move |n| SearchItem {
                id: None,
                score: None,
                payload: json!({"kind": kind, "id": format!("{prefix}-{kind}-{n}")}),
            })
        })
        .collect()
}

/// `(chunks, entities, facts)` in a merged context.
fn kind_counts(context: &SearchContext) -> (usize, usize, usize) {
    (
        kind_ids(context, "chunk").len(),
        kind_ids(context, "entity").len(),
        kind_ids(context, "fact").len(),
    )
}

/// Python's `TemporalHybridRetriever` inherits `HybridRetriever.merge_retrieved_objects`,
/// so a concurrent session turn fuses the raw and conversational lanes (the
/// trait default would keep only the raw one), with chunks and entities capped
/// at `candidate_top_k` and facts at `top_k` — the sizes Python's constructor
/// hands to `HybridRetriever`.
#[test]
fn session_merge_fuses_both_lanes_at_temporal_limits() {
    // Constructor limits: top_k = 2, candidate_top_k = 20.
    let h = harness();
    let merged = h.retriever.merge_session_contexts(
        Some(session_lane("r", 4)),
        Some(session_lane("c", 4)),
        &SearchParams::default(),
    );
    assert_eq!(kind_counts(&merged), (8, 8, 2));
    let chunks = kind_ids(&merged, "chunk");
    assert!(chunks.iter().any(|id| id.starts_with("r-")), "{chunks:?}");
    assert!(chunks.iter().any(|id| id.starts_with("c-")), "{chunks:?}");

    // The inner hybrid's own merge sizes every kind from `top_k`: forwarding
    // to it would cut chunks and entities to 2, narrower than Python.
    let forwarded = h.retriever.hybrid.merge_session_contexts(
        Some(session_lane("r", 4)),
        Some(session_lane("c", 4)),
        &SearchParams {
            top_k: Some(2),
            ..SearchParams::default()
        },
    );
    assert_eq!(kind_counts(&forwarded), (2, 2, 2));
}

/// Without constructor or request limits, `top_k` is 5 and `candidate_top_k`
/// 20; a request's `top_k` / `candidate_top_k` override them, as for the
/// fetch.
#[test]
fn session_merge_caps_follow_default_and_request_limits() {
    let h = harness_with(ScriptedLlm::new(json!({})), None, None);
    let merged = h.retriever.merge_session_contexts(
        Some(session_lane("r", 12)),
        Some(session_lane("c", 12)),
        &SearchParams::default(),
    );
    assert_eq!(kind_counts(&merged), (20, 20, 5));

    // A request `top_k` alone moves `candidate_top_k` to four times it.
    let merged = h.retriever.merge_session_contexts(
        Some(session_lane("r", 12)),
        Some(session_lane("c", 12)),
        &SearchParams {
            top_k: Some(1),
            ..SearchParams::default()
        },
    );
    assert_eq!(kind_counts(&merged), (4, 4, 1));

    // Both request knobs override the constructor's.
    let h = harness();
    let merged = h.retriever.merge_session_contexts(
        Some(session_lane("r", 12)),
        Some(session_lane("c", 12)),
        &SearchParams {
            top_k: Some(3),
            candidate_top_k: Some(5),
            ..SearchParams::default()
        },
    );
    assert_eq!(kind_counts(&merged), (5, 5, 3));
}

/// A lane that failed open (`None`) leaves the other lane, still capped; with
/// limits no fetch would accept, the merge keeps a lane instead of panicking.
#[test]
fn session_merge_keeps_the_surviving_lane() {
    let h = harness();
    let params = SearchParams::default();
    let lane_prefixes = |context: &SearchContext| -> HashSet<String> {
        kind_ids(context, "chunk")
            .iter()
            .chain(&kind_ids(context, "entity"))
            .chain(&kind_ids(context, "fact"))
            .map(|id| id[..1].to_string())
            .collect()
    };

    let raw_only = h
        .retriever
        .merge_session_contexts(Some(session_lane("r", 4)), None, &params);
    assert_eq!(kind_counts(&raw_only), (4, 4, 2));
    assert_eq!(lane_prefixes(&raw_only), HashSet::from(["r".to_string()]));

    let conversational_only =
        h.retriever
            .merge_session_contexts(None, Some(session_lane("c", 4)), &params);
    assert_eq!(kind_counts(&conversational_only), (4, 4, 2));
    assert_eq!(
        lane_prefixes(&conversational_only),
        HashSet::from(["c".to_string()])
    );

    assert!(
        h.retriever
            .merge_session_contexts(None, None, &params)
            .is_empty()
    );

    let invalid = SearchParams {
        top_k: Some(0),
        ..SearchParams::default()
    };
    let kept = h.retriever.merge_session_contexts(
        Some(session_lane("r", 1)),
        Some(session_lane("c", 1)),
        &invalid,
    );
    assert_eq!(lane_prefixes(&kept), HashSet::from(["r".to_string()]));
}

/// Embeds the conversational rewrite (`"Prior user: …"`) to `[0, 1]` and
/// every other query to `[1, 0]`, so the two session lanes would rank
/// different chunks first, and records every text it embeds.
struct LaneEmbedding(Arc<Mutex<Vec<String>>>);

#[async_trait]
impl EmbeddingEngine for LaneEmbedding {
    async fn embed(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
        self.0
            .lock()
            .unwrap()
            .extend(texts.iter().map(|text| (*text).to_string()));
        Ok(texts
            .iter()
            .map(|text| {
                if text.starts_with("Prior user:") {
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

/// End to end through the orchestrator: a TEMPORAL session turn retrieves
/// the raw question only, even in concurrent mode with history to rewrite
/// from. Python's concurrent turn opts retrievers in by exact class
/// (`session_aware_completion.py` `_eligible_retriever_types`), so its
/// `TemporalHybridRetriever` subclass takes the sequential runner and its
/// inherited `merge_retrieved_objects` is never reached; Rust's
/// `concurrent_session::eligible` mirrors that. The merge hook above stays
/// correct for the day TEMPORAL is opted in, and this pins that it is not
/// yet.
#[tokio::test]
async fn temporal_session_turn_retrieves_the_raw_lane_only() {
    let graph = Arc::new(MockGraphDB::new());
    add_node(&graph, "c1", "DocumentChunk").await;
    let vector = Arc::new(MockVectorDB::new());
    vector
        .create_collection("DocumentChunk", "text", 2)
        .await
        .unwrap();
    let point = |name: &str, embedding: Vec<f32>| {
        VectorPoint::new(Uuid::parse_str(&id(name)).unwrap(), embedding)
            .with_metadata("id", json!(id(name)))
            .with_metadata("text", json!(format!("text of {name}")))
    };
    vector
        .index_points(
            "DocumentChunk",
            "text",
            &[point("raw", vec![1.0, 0.0]), point("conv", vec![0.0, 1.0])],
        )
        .await
        .unwrap();
    let llm = Arc::new(ScriptedLlm::answering("answer"));
    let embedded = Arc::new(Mutex::new(Vec::new()));
    let retriever = Arc::new(TemporalHybridRetriever::new(
        vector as Arc<dyn VectorDB>,
        Arc::new(LaneEmbedding(Arc::clone(&embedded))) as Arc<dyn EmbeddingEngine>,
        graph as Arc<dyn GraphDBTrait>,
        Arc::clone(&llm) as Arc<dyn Llm>,
        None,
        None,
        None,
        None,
        None,
        None,
    ));

    let user = Uuid::new_v4();
    let dir = tempfile::tempdir().unwrap();
    let manager = Arc::new(cognee_session::SessionManager::new(Arc::new(
        cognee_session::FsSessionStore::new(dir.path()),
    )));
    manager
        .save_qa(
            Some("s"),
            Some(&user.to_string()),
            "Which expedition founded Atlas?",
            "The northern one.",
            None,
            None,
            None,
        )
        .await
        .unwrap();
    // An orchestrator LLM, so concurrent mode would run if TEMPORAL were
    // eligible; it is never called with auto-feedback off.
    let orchestrator = temporal_orchestrator(retriever)
        .with_session_manager(manager)
        .with_llm(Arc::new(ScriptedLlm::failing("turn analysis must not run")))
        .with_auto_feedback_default(false)
        .with_session_search_mode(crate::SessionSearchMode::Concurrent);
    let mut request: crate::types::SearchRequest = serde_json::from_value(json!({
        "query_text": "When was it founded?",
        "top_k": 1,
        "session_id": "s",
    }))
    .unwrap();
    request.search_type = SearchType::Temporal;
    request.user_id = Some(user);
    request.use_combined_context = Some(false);

    let result = orchestrator.search(&request).await.unwrap();
    assert_eq!(answer_text(&result.result), "answer");
    let prompt: String = llm
        .last_prompt
        .lock()
        .unwrap()
        .iter()
        .map(|message| message.content.clone())
        .collect();
    assert!(prompt.contains("text of raw"), "{prompt}");
    assert!(!prompt.contains("text of conv"), "{prompt}");
    assert!(
        embedded
            .lock()
            .unwrap()
            .iter()
            .all(|text| !text.starts_with("Prior user:")),
        "no conversational lane ran"
    );
}
