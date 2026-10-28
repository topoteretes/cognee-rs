#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! GRAPH_COMPLETION_DECOMPOSITION and GRAPH_REPORT end to end (SDK-1062):
//! the standard retrievers from [`SearchBuilder`], through the orchestrator,
//! over a fixture graph in a real embedded Ladybug database.
//!
//! The LLM is scripted and records every call, so the tests can check what
//! each retriever sends as well as what it returns. The report is checked
//! byte for byte against the one Python's own `GraphReportRetriever` builds
//! for the same graph (see [`PYTHON_REPORT_TOP_3`]).
//!
//! Run with:
//!   cargo test --package cognee-search --test decomposition_graph_report_integration

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cognee_database::{SearchHistoryDb, connect, initialize};
use cognee_embedding::{EmbeddingEngine, EmbeddingResult};
use cognee_graph::{GraphDBTrait, LadybugAdapter};
use cognee_llm::{
    GenerationOptions, GenerationResponse, Llm, LlmError, LlmResult, Message, MessageRole,
};
use cognee_search::types::{SearchError, SearchOutput, SearchRequest, SearchType};
use cognee_search::{SearchBuilder, SearchOrchestrator};
use cognee_vector::{MockVectorDB, VectorDB, VectorPoint};
use serde_json::{Value, json};
use tempfile::TempDir;
use uuid::Uuid;

// ── Fixture graph ────────────────────────────────────────────────────────────

const GEOGRAPHY: Uuid = Uuid::from_u128(0xA1);
const POLITICS: Uuid = Uuid::from_u128(0xA2);
const GERMANY: Uuid = Uuid::from_u128(0xE1);
const NETHERLANDS: Uuid = Uuid::from_u128(0xE2);
const FRANCE: Uuid = Uuid::from_u128(0xE3);
const BERLIN: Uuid = Uuid::from_u128(0xE4);
const AMSTERDAM: Uuid = Uuid::from_u128(0xE5);
const COUNTRY: Uuid = Uuid::from_u128(0xF1);
const CITY: Uuid = Uuid::from_u128(0xF2);
const CHUNK: Uuid = Uuid::from_u128(0xC1);
const DOCUMENT: Uuid = Uuid::from_u128(0xD1);

/// `(id, type, name)` of every node.
const NODES: [(Uuid, &str, &str); 11] = [
    (GEOGRAPHY, "NodeSet", "geography"),
    (POLITICS, "NodeSet", "politics"),
    (GERMANY, "Entity", "Germany"),
    (NETHERLANDS, "Entity", "Netherlands"),
    (FRANCE, "Entity", "France"),
    (BERLIN, "Entity", "Berlin"),
    (AMSTERDAM, "Entity", "Amsterdam"),
    (COUNTRY, "EntityType", "Country"),
    (CITY, "EntityType", "City"),
    (CHUNK, "DocumentChunk", "chunk-0"),
    (DOCUMENT, "TextDocument", "europe.txt"),
];

/// Node-set membership, entity-to-entity facts, types and chunk structure.
/// No two edges share an ordered node pair and no two hubs tie, so neither
/// report nor ranking depends on the order the database returns rows in.
const EDGES: [(Uuid, Uuid, &str); 17] = [
    (GERMANY, GEOGRAPHY, "belongs_to_set"),
    (NETHERLANDS, GEOGRAPHY, "belongs_to_set"),
    (BERLIN, GEOGRAPHY, "belongs_to_set"),
    (FRANCE, POLITICS, "belongs_to_set"),
    (AMSTERDAM, POLITICS, "belongs_to_set"),
    (GERMANY, NETHERLANDS, "borders"),
    (GERMANY, FRANCE, "borders"),
    (BERLIN, GERMANY, "capital_of"),
    (AMSTERDAM, NETHERLANDS, "capital_of"),
    (GERMANY, COUNTRY, "is_a"),
    (NETHERLANDS, COUNTRY, "is_a"),
    (FRANCE, COUNTRY, "is_a"),
    (BERLIN, CITY, "is_a"),
    (AMSTERDAM, CITY, "is_a"),
    (CHUNK, GERMANY, "contains"),
    (CHUNK, BERLIN, "contains"),
    (CHUNK, DOCUMENT, "is_part_of"),
];

/// Where each entity sits in the 2-d embedding space: the border question
/// points at `[1, 0]`, the capital question at `[0, 1]`.
const ENTITY_VECTORS: [(Uuid, [f32; 2]); 5] = [
    (GERMANY, [1.0, 0.0]),
    (FRANCE, [0.95, 0.31]),
    (BERLIN, [0.71, 0.71]),
    (NETHERLANDS, [0.31, 0.95]),
    (AMSTERDAM, [0.0, 1.0]),
];

/// Python's report for the fixture graph with `top_n = 3`, from
/// `GraphReportRetriever.get_context_from_objects` at cognee `origin/dev`
/// (bcab7e8) with networkx 3.6.1's (scipy-free) PageRank. It is the same for
/// any order of the node and edge lists (checked over 20 shuffles).
const PYTHON_REPORT_TOP_3: &str = "# Graph Insight Report\n\n**Nodes:** 9 | **Edges:** 12\n\n\n## 🏆 Hub Nodes\n\nTop nodes by degree + PageRank (most connected / influential):\n\n1. **Country** (set: `—`, degree: 3, PageRank: 0.2738)\n2. **Germany** (set: `geography`, degree: 5, PageRank: 0.1123)\n3. **Netherlands** (set: `geography`, degree: 3, PageRank: 0.1193)\n\n\n## 🔗 Surprising Cross-Set Connections\n\nEntity pairs linked across different node sets:\n\n- **Germany** (`geography`) —[borders]→ **France** (`politics`)\n- **Amsterdam** (`politics`) —[capital_of]→ **Netherlands** (`geography`)\n\n\n## 🏷️ Edge Provenance\n\nHow edges entered the graph:\n\n- `EXTRACTED (entity-to-entity, LLM-derived)`: 4 edges (33.3%)\n- `DERIVED (structural / chunk-linked)`: 8 edges (66.7%)\n";

/// The same with `top_n = 10`: every knowledge node.
const PYTHON_REPORT_TOP_10_HUBS: &str = "1. **Country** (set: `—`, degree: 3, PageRank: 0.2738)\n2. **Germany** (set: `geography`, degree: 5, PageRank: 0.1123)\n3. **Netherlands** (set: `geography`, degree: 3, PageRank: 0.1193)\n4. **Berlin** (set: `geography`, degree: 3, PageRank: 0.0788)\n5. **City** (set: `—`, degree: 2, PageRank: 0.1210)\n6. **France** (set: `politics`, degree: 2, PageRank: 0.0932)\n7. **Amsterdam** (set: `politics`, degree: 2, PageRank: 0.0614)\n";

const BORDER_QUESTION: &str = "Which countries border Germany?";
const CAPITAL_QUESTION: &str = "What is the capital of the Netherlands?";

// ── Engines ──────────────────────────────────────────────────────────────────

/// Border questions embed to `[1, 0]`, capital questions to `[0, 1]`,
/// anything else halfway.
struct KeyedEmbedding;

#[async_trait]
impl EmbeddingEngine for KeyedEmbedding {
    async fn embed(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|text| {
                if text.contains("border") {
                    vec![1.0, 0.0]
                } else if text.contains("capital") {
                    vec![0.0, 1.0]
                } else {
                    vec![0.71, 0.71]
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

/// One LLM call as the scripted LLM saw it.
#[derive(Debug, Clone)]
enum Call {
    /// A structured call, by the top-level properties of its schema.
    Structured {
        properties: Vec<String>,
        user: String,
    },
    Generate {
        system: String,
        user: String,
    },
}

/// Splits a question into [`BORDER_QUESTION`] and [`CAPITAL_QUESTION`],
/// suggests two questions for a report, and answers a generation with
/// `answer #n`. Records every call.
#[derive(Default)]
struct ScriptedLlm {
    calls: Mutex<Vec<Call>>,
}

impl ScriptedLlm {
    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    fn generations(&self) -> Vec<(String, String)> {
        self.calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Generate { system, user } => Some((system, user)),
                Call::Structured { .. } => None,
            })
            .collect()
    }
}

fn content_of(messages: &[Message], role: MessageRole) -> String {
    messages
        .iter()
        .filter(|message| message.role == role)
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[async_trait]
impl Llm for ScriptedLlm {
    async fn generate(
        &self,
        messages: Vec<Message>,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        let mut calls = self.calls.lock().unwrap();
        calls.push(Call::Generate {
            system: content_of(&messages, MessageRole::System),
            user: content_of(&messages, MessageRole::User),
        });
        let n = calls
            .iter()
            .filter(|call| matches!(call, Call::Generate { .. }))
            .count();
        Ok(GenerationResponse {
            content: format!("answer #{n}"),
            model: "scripted".to_string(),
            usage: None,
            finish_reason: Some("stop".to_string()),
        })
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        messages: Vec<Message>,
        json_schema: &Value,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<Value> {
        let properties: Vec<String> = json_schema["properties"]
            .as_object()
            .map(|properties| properties.keys().cloned().collect())
            .unwrap_or_default();
        self.calls.lock().unwrap().push(Call::Structured {
            properties: properties.clone(),
            user: content_of(&messages, MessageRole::User),
        });
        if properties.iter().any(|p| p == "subqueries") {
            Ok(json!({ "subqueries": [BORDER_QUESTION, CAPITAL_QUESTION] }))
        } else if properties.iter().any(|p| p == "questions") {
            Ok(json!({ "questions": ["Which country is the biggest hub?", "Where is Berlin?"] }))
        } else {
            Err(LlmError::ConfigError(format!(
                "unexpected schema: {json_schema}"
            )))
        }
    }

    fn model(&self) -> &str {
        "scripted"
    }
}

// ── Harness ──────────────────────────────────────────────────────────────────

struct Harness {
    orchestrator: SearchOrchestrator,
    llm: Arc<ScriptedLlm>,
    _dir: TempDir,
}

/// The standard search stack over a Ladybug graph holding the fixture (or
/// nothing), the fixture's entity vectors and a SQLite search history.
async fn harness(with_graph: bool) -> Harness {
    let dir = tempfile::tempdir().unwrap();

    let db_path = dir.path().join("cognee.db");
    std::fs::File::create(&db_path).unwrap();
    let database = connect(&format!("sqlite://{}", db_path.display()))
        .await
        .unwrap();
    initialize(&database).await.unwrap();

    let graph_path = dir.path().join("graph").to_string_lossy().to_string();
    let graph_db = LadybugAdapter::new(&graph_path).await.unwrap();
    graph_db.initialize().await.unwrap();
    if with_graph {
        for (id, node_type, name) in NODES {
            graph_db
                .add_node_raw(json!({ "id": id.to_string(), "type": node_type, "name": name }))
                .await
                .unwrap();
        }
        for (source, target, relationship) in EDGES {
            graph_db
                .add_edge(&source.to_string(), &target.to_string(), relationship, None)
                .await
                .unwrap();
        }
    }

    let vector_db = MockVectorDB::new();
    vector_db
        .create_collection("Entity", "name", 2)
        .await
        .unwrap();
    let points: Vec<VectorPoint> = ENTITY_VECTORS
        .iter()
        .map(|(id, vector)| {
            VectorPoint::new(*id, vector.to_vec()).with_metadata("id", json!(id.to_string()))
        })
        .collect();
    vector_db
        .index_points("Entity", "name", &points)
        .await
        .unwrap();

    let llm = Arc::new(ScriptedLlm::default());
    let orchestrator = SearchBuilder::new(
        Arc::new(vector_db) as Arc<dyn VectorDB>,
        Arc::new(KeyedEmbedding) as Arc<dyn EmbeddingEngine>,
        Arc::new(graph_db) as Arc<dyn GraphDBTrait>,
        Arc::clone(&llm) as Arc<dyn Llm>,
        Arc::new(database) as Arc<dyn SearchHistoryDb>,
    )
    .build();

    Harness {
        orchestrator,
        llm,
        _dir: dir,
    }
}

fn request(search_type: SearchType, query: &str) -> SearchRequest {
    SearchRequest {
        query_text: query.to_string(),
        search_type,
        top_k: None,
        datasets: None,
        dataset_ids: None,
        system_prompt: None,
        system_prompt_path: None,
        only_context: None,
        use_combined_context: None,
        session_id: None,
        node_type: None,
        node_name: None,
        node_name_filter_operator: None,
        wide_search_top_k: None,
        triplet_distance_penalty: None,
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

fn text(output: &SearchOutput) -> &str {
    match output {
        SearchOutput::Text(text) => text,
        other => panic!("expected a text answer, got {other:?}"),
    }
}

/// The `Connections:` lines of a rendered triplet context.
fn connections(context: &str) -> Vec<&str> {
    context
        .split("Connections:\n")
        .nth(1)
        .unwrap_or_default()
        .lines()
        .take_while(|line| line.contains(" --["))
        .collect()
}

// ── GRAPH_COMPLETION_DECOMPOSITION ───────────────────────────────────────────

/// The default mode: one decomposition call, one answer per sub-question over
/// that sub-question's own triplets, then the question answered over the
/// sub-answers.
#[tokio::test]
async fn decomposition_answers_each_subquery_then_the_question() {
    let h = harness(true).await;
    let question = "Tell me about Germany and the Netherlands.";
    let mut req = request(SearchType::GraphCompletionDecomposition, question);
    req.top_k = Some(2);

    let response = h.orchestrator.search(&req).await.unwrap();
    assert_eq!(
        response.search_type,
        SearchType::GraphCompletionDecomposition
    );
    assert_eq!(text(&response.result), "answer #3");

    let calls = h.llm.calls();
    let Call::Structured { properties, user } = &calls[0] else {
        panic!("the first call decomposes the question: {calls:?}");
    };
    assert_eq!(properties, &["subqueries"]);
    assert_eq!(user, question);

    let generations = h.llm.generations();
    assert_eq!(generations.len(), 3, "{generations:?}");

    // Sub-answers run concurrently, so match them by question.
    let sub_prompt = |subquery: &str| {
        generations[..2]
            .iter()
            .find(|(_, user)| user.contains(subquery))
            .unwrap_or_else(|| panic!("no sub-answer prompt for {subquery:?}: {generations:?}"))
            .1
            .clone()
    };
    let border = sub_prompt(BORDER_QUESTION);
    let capital = sub_prompt(CAPITAL_QUESTION);
    assert!(
        connections(&border).contains(&"Germany --[borders]--> France"),
        "{border}"
    );
    assert!(
        connections(&capital).contains(&"Amsterdam --[capital_of]--> Netherlands"),
        "{capital}"
    );
    assert_eq!(
        connections(&border).len(),
        2,
        "top_k triplets each: {border}"
    );
    assert_eq!(
        connections(&capital).len(),
        2,
        "top_k triplets each: {capital}"
    );

    // The final answer reads the sub-answers, in sub-question order, not
    // the triplets. Which sub-answer got which number is up to scheduling.
    let (_, final_prompt) = &generations[2];
    let answer_to = |subquery: &str| {
        let position = generations[..2]
            .iter()
            .position(|(_, user)| user.contains(subquery))
            .unwrap();
        format!("answer #{}", position + 1)
    };
    let expected = format!(
        "Question decomposition results:\n\n\
         Subquery 1: {BORDER_QUESTION}\nSubquery 1 Answer:\n{}\n\n\
         Subquery 2: {CAPITAL_QUESTION}\nSubquery 2 Answer:\n{}",
        answer_to(BORDER_QUESTION),
        answer_to(CAPITAL_QUESTION),
    );
    assert!(final_prompt.contains(&expected), "{final_prompt}");
    assert!(final_prompt.contains(question), "{final_prompt}");
    assert!(!final_prompt.contains(" --["), "{final_prompt}");
}

/// `combined_triplets_context`: one answer over every sub-question's
/// triplets, each triplet once.
#[tokio::test]
async fn decomposition_combined_mode_answers_once_over_merged_triplets() {
    let h = harness(true).await;
    let mut req = request(
        SearchType::GraphCompletionDecomposition,
        "Tell me about Germany and the Netherlands.",
    );
    req.top_k = Some(2);
    req.retriever_specific_config = Some(
        [(
            "decomposition_mode".to_string(),
            json!("combined_triplets_context"),
        )]
        .into(),
    );

    let response = h.orchestrator.search(&req).await.unwrap();
    assert_eq!(text(&response.result), "answer #1");

    let generations = h.llm.generations();
    assert_eq!(generations.len(), 1, "{generations:?}");
    let lines = connections(&generations[0].1);
    assert!(
        lines.contains(&"Germany --[borders]--> France"),
        "{lines:?}"
    );
    assert!(
        lines.contains(&"Amsterdam --[capital_of]--> Netherlands"),
        "{lines:?}"
    );
    let mut unique = lines.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        unique.len(),
        lines.len(),
        "a triplet is merged once: {lines:?}"
    );
    assert!(
        lines.len() <= 4,
        "at most top_k per sub-question: {lines:?}"
    );
}

/// An unknown mode is rejected before any LLM call.
#[tokio::test]
async fn decomposition_rejects_an_unknown_mode() {
    let h = harness(true).await;
    let mut req = request(SearchType::GraphCompletionDecomposition, BORDER_QUESTION);
    req.retriever_specific_config =
        Some([("decomposition_mode".to_string(), json!("fan_out"))].into());

    let error = h.orchestrator.search(&req).await.unwrap_err();
    assert!(matches!(error, SearchError::InvalidInput(_)), "{error:?}");
    assert!(h.llm.calls().is_empty(), "{:?}", h.llm.calls());
}

/// An empty graph is `NotFound`, as for GRAPH_COMPLETION, before any LLM call.
#[tokio::test]
async fn decomposition_over_an_empty_graph_is_not_found() {
    let h = harness(false).await;
    let req = request(SearchType::GraphCompletionDecomposition, BORDER_QUESTION);

    let error = h.orchestrator.search(&req).await.unwrap_err();
    assert!(matches!(error, SearchError::NotFound(_)), "{error:?}");
    assert!(h.llm.calls().is_empty(), "{:?}", h.llm.calls());
}

// ── GRAPH_REPORT ─────────────────────────────────────────────────────────────

/// The report Python builds for the same graph, then the suggested questions
/// from one structured call over it. `top_k` is the hub count.
#[tokio::test]
async fn graph_report_matches_python_and_appends_suggestions() {
    let h = harness(true).await;
    let mut req = request(SearchType::GraphReport, "ignored");
    req.top_k = Some(3);

    let response = h.orchestrator.search(&req).await.unwrap();
    assert_eq!(response.search_type, SearchType::GraphReport);
    let expected = format!(
        "{PYTHON_REPORT_TOP_3}\n\n## 💡 Suggested Questions\n\n\
         Questions ready to pipe into `cognee.search()`:\n\n\
         - Which country is the biggest hub?\n- Where is Berlin?"
    );
    assert_eq!(text(&response.result), expected);

    let calls = h.llm.calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    let Call::Structured { properties, user } = &calls[0] else {
        panic!("one structured call: {calls:?}");
    };
    assert_eq!(properties, &["questions"]);
    assert!(user.contains(PYTHON_REPORT_TOP_3), "{user}");
}

/// `only_context` answers the bare report text, as Python's context for this
/// type is the Markdown string, and makes no LLM call.
#[tokio::test]
async fn graph_report_only_context_is_the_bare_report() {
    let h = harness(true).await;
    let mut req = request(SearchType::GraphReport, "ignored");
    req.top_k = Some(3);
    req.only_context = Some(true);

    let response = h.orchestrator.search(&req).await.unwrap();
    assert!(response.only_context);
    assert_eq!(text(&response.result), PYTHON_REPORT_TOP_3);
    assert!(response.user_prompt.is_none());
    assert!(h.llm.calls().is_empty(), "{:?}", h.llm.calls());
}

/// Without `top_k` the retriever's default of 10 applies: every knowledge
/// node is a hub here.
#[tokio::test]
async fn graph_report_defaults_to_ten_hubs() {
    let h = harness(true).await;
    let response = h
        .orchestrator
        .search(&request(SearchType::GraphReport, "ignored"))
        .await
        .unwrap();
    assert!(
        text(&response.result).contains(PYTHON_REPORT_TOP_10_HUBS),
        "{}",
        text(&response.result)
    );
}

/// An empty graph is Python's fixed report, not an error; the suggestions
/// call still runs.
#[tokio::test]
async fn graph_report_over_an_empty_graph_is_the_fixed_report() {
    let h = harness(false).await;
    let response = h
        .orchestrator
        .search(&request(SearchType::GraphReport, "ignored"))
        .await
        .unwrap();
    assert!(
        text(&response.result)
            .starts_with("# Graph Insight Report\n\n_Graph is empty — run `cognify` first._\n"),
        "{}",
        text(&response.result)
    );
    assert_eq!(h.llm.calls().len(), 1, "{:?}", h.llm.calls());
}
