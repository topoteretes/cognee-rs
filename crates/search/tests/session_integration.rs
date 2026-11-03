#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for session→search wiring (Part B of task 20).
//!
//! These tests verify:
//! 1. `session_history_has_no_graph_knowledge_prefix` — the session history
//!    reaches the retriever without the removed graph-knowledge prefix.
//! 2. `save_qa_populates_used_graph_element_ids` — QA entries saved after a
//!    graph-retrieval search carry non-empty `used_graph_element_ids`.
//! 3. `conversational_feedback_persists_to_prior_entry` — when
//!    `auto_feedback_detection` fires, feedback is written to the PRIOR entry and
//!    the new entry is saved normally.
//!
//! Uses `FsSessionStore`, `MockLlm` (via `cognee_test_utils`), and a custom
//! `FakeGraphRetriever` that captures the session context it receives.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cognee_database::{DatabaseError, SearchHistoryDb, SearchHistoryEntry};
use cognee_session::{FsSessionStore, SessionContext, SessionManager, SessionStore};
use cognee_test_utils::MockLlm;
use serde_json::json;
use uuid::Uuid;

use cognee_search::retrievers::SearchRetriever;
use cognee_search::types::{
    SearchContext, SearchError, SearchItem, SearchOutput, SearchParams, SearchRequest, SearchType,
};
use cognee_search::{SearchOrchestrator, SearchTypeRegistry};

// ---------------------------------------------------------------------------
// FakeGraphRetriever — captures SessionContext; returns items with source_id
// ---------------------------------------------------------------------------

struct FakeGraphRetriever {
    captured_sessions: Arc<Mutex<Vec<SessionContext>>>,
    /// When `Some`, return graph-completion text; when `None`, return items.
    fixed_text: Option<String>,
}

impl FakeGraphRetriever {
    fn capturing(captured_sessions: Arc<Mutex<Vec<SessionContext>>>) -> Self {
        Self {
            captured_sessions,
            fixed_text: Some("graph answer".to_string()),
        }
    }

    fn with_graph_items(captured_sessions: Arc<Mutex<Vec<SessionContext>>>) -> Self {
        Self {
            captured_sessions,
            fixed_text: None,
        }
    }
}

#[async_trait]
impl SearchRetriever for FakeGraphRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::GraphCompletion
    }

    async fn get_context(
        &self,
        _query: &str,
        _params: &SearchParams,
    ) -> Result<SearchContext, SearchError> {
        // Return a single item that has source_id / target_id so that
        // `build_used_graph_element_ids` can extract them.
        Ok(vec![SearchItem {
            id: Some(Uuid::new_v4()),
            score: Some(0.9),
            payload: json!({"source_id": "node-src", "target_id": "node-tgt", "text": "edge text"}),
        }])
    }

    async fn get_completion(
        &self,
        _query: &str,
        _context: Option<SearchContext>,
        session: &SessionContext,
        _params: &SearchParams,
    ) -> Result<SearchOutput, SearchError> {
        self.captured_sessions
            .lock()
            .unwrap() // lock poison is unrecoverable
            .push(session.clone());
        if let Some(ref text) = self.fixed_text {
            Ok(SearchOutput::Text(text.clone()))
        } else {
            Ok(SearchOutput::Text("graph items answer".to_string()))
        }
    }
}

// ---------------------------------------------------------------------------
// NoOpHistoryDb
// ---------------------------------------------------------------------------

struct NoOpHistoryDb;

#[async_trait]
impl SearchHistoryDb for NoOpHistoryDb {
    async fn log_query(
        &self,
        _query_text: &str,
        _query_type: &str,
        _user_id: Option<Uuid>,
        _dataset_id: Option<Uuid>,
    ) -> Result<Uuid, DatabaseError> {
        Ok(Uuid::new_v4())
    }

    async fn log_result(
        &self,
        _query_id: Uuid,
        _serialized_result: &str,
        _user_id: Option<Uuid>,
        _dataset_id: Option<Uuid>,
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
// Helper: build a GraphCompletion SearchRequest
// ---------------------------------------------------------------------------

fn graph_request(query: &str, session_id: Option<&str>) -> SearchRequest {
    SearchRequest {
        query_text: query.to_string(),
        search_type: SearchType::GraphCompletion,
        top_k: None,
        datasets: None,
        dataset_ids: None,
        system_prompt: None,
        system_prompt_path: None,
        only_context: Some(false),
        use_combined_context: Some(false),
        session_id: session_id.map(String::from),
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
    }
}

fn graph_request_with_feedback_detection(query: &str, session_id: Option<&str>) -> SearchRequest {
    let mut r = graph_request(query, session_id);
    r.auto_feedback_detection = Some(true);
    r
}

// ---------------------------------------------------------------------------
// Test 1: session_history_has_no_graph_knowledge_prefix
//
// The graph-to-session sync and its "Background knowledge from the knowledge
// graph" prefix are gone (Python `3401da38a`): the retriever gets the plain
// conversation history.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn session_history_has_no_graph_knowledge_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let session_store = Arc::new(FsSessionStore::new(dir.path().join("sessions")));
    let session_manager = Arc::new(SessionManager::new(session_store.clone()));

    let session_id = "gc-session-1";
    session_store
        .create_qa_entry(session_id, None, "prior question", "prior answer", None)
        .await
        .unwrap();
    // A graph snapshot the removed sync stage left on disk (the fs store kept
    // it at `{user}/_graph_context_{session}.json`) must not be read back.
    std::fs::write(
        dir.path()
            .join("sessions")
            .join("default")
            .join(format!("_graph_context_{session_id}.json")),
        "\"Alice -> knows -> Bob\"",
    )
    .unwrap();

    let captured: Arc<Mutex<Vec<SessionContext>>> = Arc::new(Mutex::new(vec![]));
    let retriever = Arc::new(FakeGraphRetriever::capturing(Arc::clone(&captured)));

    let mut registry = SearchTypeRegistry::new();
    registry.register(retriever);

    let orchestrator = SearchOrchestrator::new(registry)
        .with_database(Arc::new(NoOpHistoryDb))
        .with_session_manager(session_manager.clone());

    orchestrator
        .search(&graph_request("What is Rust?", Some(session_id)))
        .await
        .expect("search must succeed");

    let sessions = captured.lock().unwrap(); // lock poison is unrecoverable
    assert_eq!(sessions.len(), 1, "retriever should be called once");
    let fh = &sessions[0].formatted_history;
    assert!(
        fh.contains("prior question"),
        "formatted_history must contain prior history; got: {fh:?}"
    );
    assert!(
        !fh.contains("Background knowledge from the knowledge graph"),
        "no graph-knowledge prefix; got: {fh:?}"
    );
    assert!(
        !fh.contains("Alice -> knows -> Bob"),
        "a legacy graph snapshot must not reach the completion; got: {fh:?}"
    );
}

// ---------------------------------------------------------------------------
// Test 2: save_qa_populates_used_graph_element_ids
//
// Run a graph-completion search in a session where the retriever returns
// items with source_id/target_id. The saved QA entry must carry non-empty
// used_graph_element_ids.node_ids.
//
// `use_combined_context: true` is set so the orchestrator calls `get_context`
// and passes the returned items as `context` to `get_completion`, which is the
// path that feeds `build_used_graph_element_ids`.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn save_qa_populates_used_graph_element_ids() {
    let dir = tempfile::tempdir().unwrap();
    let session_store = Arc::new(FsSessionStore::new(dir.path().join("sessions")));
    let session_manager = Arc::new(SessionManager::new(session_store.clone()));

    let session_id = "graph-ids-session";

    let captured: Arc<Mutex<Vec<SessionContext>>> = Arc::new(Mutex::new(vec![]));
    let retriever = Arc::new(FakeGraphRetriever::with_graph_items(Arc::clone(&captured)));

    let mut registry = SearchTypeRegistry::new();
    registry.register(retriever);

    let orchestrator = SearchOrchestrator::new(registry)
        .with_database(Arc::new(NoOpHistoryDb))
        .with_session_manager(session_manager.clone());

    // use_combined_context=true causes the orchestrator to call get_context and
    // pass the result to get_completion, then on to build_used_graph_element_ids.
    let mut req = graph_request("find entities", Some(session_id));
    req.use_combined_context = Some(true);
    orchestrator
        .search(&req)
        .await
        .expect("search must succeed");

    // Load the saved QA entry and inspect used_graph_element_ids.
    let entries = session_store
        .get_all_qa_entries(session_id, None)
        .await
        .expect("reading session entries must succeed");

    assert_eq!(entries.len(), 1, "one QA entry must be saved");
    let ids = entries[0]
        .used_graph_element_ids
        .as_ref()
        .expect("used_graph_element_ids must be Some after a graph search");

    assert!(
        !ids.node_ids.is_empty(),
        "node_ids must be non-empty; got: {ids:?}"
    );
    assert!(
        ids.node_ids.contains(&"node-src".to_string()),
        "source_id 'node-src' must appear in node_ids; got: {:?}",
        ids.node_ids
    );
    assert!(
        ids.node_ids.contains(&"node-tgt".to_string()),
        "target_id 'node-tgt' must appear in node_ids; got: {:?}",
        ids.node_ids
    );
}

// ---------------------------------------------------------------------------
// FakeHybridRetriever — reports SearchType::HybridCompletion and returns
// hybrid `kind`-tagged context items so `build_used_graph_element_ids` (via
// `extract_used_ids`) can derive node ids from the hybrid lane.
// ---------------------------------------------------------------------------

struct FakeHybridRetriever;

#[async_trait]
impl SearchRetriever for FakeHybridRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::HybridCompletion
    }

    async fn get_context(
        &self,
        _query: &str,
        _params: &SearchParams,
    ) -> Result<SearchContext, SearchError> {
        // A chunk item and an entity item, both kind-tagged like the real
        // hybrid retriever emits. `extract_used_ids` folds the chunk id and the
        // entity id + edge endpoints into node_ids.
        Ok(vec![
            SearchItem {
                id: None,
                score: Some(0.9),
                payload: json!({ "kind": "chunk", "id": "hybrid-chunk-1", "text": "t" }),
            },
            SearchItem {
                id: None,
                score: Some(0.8),
                payload: json!({
                    "kind": "entity",
                    "id": "hybrid-entity-1",
                    "name": "Alice",
                    "edges": [ { "source_id": "hybrid-entity-1", "target_id": "acme-id" } ]
                }),
            },
        ])
    }

    async fn get_completion(
        &self,
        query: &str,
        context: Option<SearchContext>,
        _session: &SessionContext,
        params: &SearchParams,
    ) -> Result<SearchOutput, SearchError> {
        // Standard retriever contract: use the passed context if present,
        // otherwise fetch privately. The orchestrator fix must supply the
        // context on the unscoped path so used ids are captured.
        let _ctx = match context {
            Some(existing) => existing,
            None => self.get_context(query, params).await?,
        };
        Ok(SearchOutput::Text("hybrid answer".to_string()))
    }
}

fn hybrid_request(query: &str, session_id: Option<&str>) -> SearchRequest {
    SearchRequest {
        search_type: SearchType::HybridCompletion,
        // Unscoped: no dataset scope, not only_context, not combined_context.
        only_context: Some(false),
        use_combined_context: Some(false),
        ..graph_request(query, session_id)
    }
}

// ---------------------------------------------------------------------------
// Test 2b: unscoped_hybrid_completion_populates_used_graph_element_ids
//
// Regression test for the P1-10 default-path gap: a HYBRID_COMPLETION search
// with a session_id but WITHOUT only_context / use_combined_context / dataset
// scope must still record used_graph_element_ids on the saved QA entry. Before
// the fix, `context` was None on this path (include_context false), so
// `build_used_graph_element_ids(None)` returned None and the feature was inert.
// Python's HybridRetriever computes extract_context_object_ids on every path.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unscoped_hybrid_completion_populates_used_graph_element_ids() {
    let dir = tempfile::tempdir().unwrap();
    let session_store = Arc::new(FsSessionStore::new(dir.path().join("sessions")));
    let session_manager = Arc::new(SessionManager::new(session_store.clone()));

    let session_id = "hybrid-unscoped-session";

    let mut registry = SearchTypeRegistry::new();
    registry.register(Arc::new(FakeHybridRetriever));

    let orchestrator = SearchOrchestrator::new(registry)
        .with_database(Arc::new(NoOpHistoryDb))
        .with_session_manager(session_manager.clone());

    // Deliberately unscoped: only_context=false, use_combined_context=false,
    // no dataset_ids — the default path where the bug lived.
    orchestrator
        .search(&hybrid_request("what happened?", Some(session_id)))
        .await
        .expect("search must succeed");

    let entries = session_store
        .get_all_qa_entries(session_id, None)
        .await
        .expect("reading session entries must succeed");

    assert_eq!(entries.len(), 1, "one QA entry must be saved");
    let ids = entries[0]
        .used_graph_element_ids
        .as_ref()
        .expect("used_graph_element_ids must be Some on the unscoped hybrid path");

    assert!(
        ids.node_ids.contains(&"hybrid-chunk-1".to_string()),
        "chunk id must be captured; got: {:?}",
        ids.node_ids
    );
    assert!(
        ids.node_ids.contains(&"hybrid-entity-1".to_string()),
        "entity id must be captured; got: {:?}",
        ids.node_ids
    );
    assert!(
        ids.node_ids.contains(&"acme-id".to_string()),
        "entity edge endpoint must be captured; got: {:?}",
        ids.node_ids
    );
    assert!(
        ids.edge_ids.is_empty(),
        "hybrid path never emits edge_ids; got: {:?}",
        ids.edge_ids
    );
}

// ---------------------------------------------------------------------------
// Test 3: conversational_feedback_persists_to_prior_entry
//
// 1. Save a first QA entry (the "prior" entry).
// 2. Issue a second search with auto_feedback_detection=true and a MockLlm
//    that returns feedback_detected=true with feedback_score=5.
// 3. Assert the PRIOR entry has feedback_text/feedback_score set and
//    memify_metadata["feedback_weights_applied"] == false.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn conversational_feedback_persists_to_prior_entry() {
    let dir = tempfile::tempdir().unwrap();
    let session_store = Arc::new(FsSessionStore::new(dir.path().join("sessions")));
    let session_manager = Arc::new(SessionManager::new(session_store.clone()));

    let session_id = "feedback-session";
    let user = Uuid::new_v4();
    let user_id = user.to_string();

    // Manually create the first QA entry (the prior entry that feedback targets).
    session_store
        .create_qa_entry(
            session_id,
            Some(&user_id),
            "first question",
            "first answer",
            None,
        )
        .await
        .unwrap();

    // Session-turn analysis identifies a rating-only feedback turn.
    let feedback_json = serde_json::to_string(&serde_json::json!({
        "response_to_user": "Thank you for the feedback!",
        "query_to_answer": null,
        "served_context_ratings": [],
        "candidate_context_updates": [{
            "section": "preferences",
            "content": "Values concise answers",
            "confidence": 0.9
        }],
        "previous_answer_rating": 5
    }))
    .unwrap();

    let llm: Arc<dyn cognee_llm::Llm> = Arc::new(MockLlm::new(vec![feedback_json]));

    let captured: Arc<Mutex<Vec<SessionContext>>> = Arc::new(Mutex::new(vec![]));
    let retriever = Arc::new(FakeGraphRetriever::capturing(Arc::clone(&captured)));

    let mut registry = SearchTypeRegistry::new();
    registry.register(retriever);

    let orchestrator = SearchOrchestrator::new(registry)
        .with_database(Arc::new(NoOpHistoryDb))
        .with_session_manager(session_manager.clone())
        .with_llm(llm)
        .with_session_search_mode(cognee_search::SessionSearchMode::Sequential);

    // Issue the feedback turn.
    let mut request =
        graph_request_with_feedback_detection("That was a great answer!", Some(session_id));
    request.user_id = Some(user);
    let response = orchestrator
        .search(&request)
        .await
        .expect("search must succeed");

    // The response should be an acknowledgment (pure feedback, no follow-up).
    match response.result {
        SearchOutput::Text(ref t) => {
            assert!(
                t.contains("feedback") || t.contains("Thank"),
                "expected feedback acknowledgment; got: {t:?}"
            );
        }
        other => panic!("expected Text ack output, got: {other:?}"),
    }

    // The retriever should NOT have been called (pure feedback → early return).
    {
        let sessions = captured.lock().unwrap(); // lock poison is unrecoverable
        assert!(
            sessions.is_empty(),
            "retriever must NOT be called on a pure feedback turn"
        );
    }

    // Latest Python stores the acknowledgement as this turn's QA row while
    // attaching its rating to the prior answer.
    let entries = session_store
        .get_all_qa_entries(session_id, Some(&user_id))
        .await
        .expect("reading session entries must succeed");

    // Inferred feedback must not overwrite explicit QA feedback fields.
    let prior = entries
        .iter()
        .find(|e| e.question == "first question")
        .expect("original QA entry must still exist");

    assert!(prior.feedback_text.is_none());
    assert!(prior.feedback_score.is_none());
    assert_eq!(entries.len(), 2);
    let context = session_manager
        .get_session_context_entries(Some(session_id), Some(&user_id))
        .await
        .unwrap();
    assert!(context.iter().any(|row| {
        row.get("kind").and_then(serde_json::Value::as_str) == Some("feedback")
            && row
                .get("referenced_qa_rating")
                .and_then(serde_json::Value::as_i64)
                == Some(5)
            && row.get("raw_text").and_then(serde_json::Value::as_str)
                == Some("That was a great answer!")
    }));
}

// ---------------------------------------------------------------------------
// A session search under per-dataset storage runs the retriever once per
// dataset but records ONE turn: the session side effects run around the
// fan-out, not inside each dataset's run.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_per_dataset_session_search_saves_one_turn() {
    let dir = tempfile::tempdir().unwrap();
    let session_store = Arc::new(FsSessionStore::new(dir.path().join("sessions")));
    let session_manager = Arc::new(SessionManager::new(session_store.clone()));
    let captured: Arc<Mutex<Vec<SessionContext>>> = Arc::new(Mutex::new(vec![]));
    let mut registry = SearchTypeRegistry::new();
    registry.register(Arc::new(FakeGraphRetriever::capturing(Arc::clone(
        &captured,
    ))));
    let orchestrator = SearchOrchestrator::new(registry)
        .with_database(Arc::new(NoOpHistoryDb))
        .with_session_manager(session_manager)
        .with_per_dataset_storage(true);

    let session_id = "per-dataset-session";
    let mut request = graph_request("What is Rust?", Some(session_id));
    request.dataset_ids = Some(vec![Uuid::new_v4(), Uuid::new_v4()]);
    orchestrator.search(&request).await.expect("search");

    // lock poison is unrecoverable
    assert_eq!(captured.lock().unwrap().len(), 2, "one run per dataset");
    let entries = session_store
        .get_all_qa_entries(session_id, None)
        .await
        .unwrap();
    assert_eq!(
        entries.len(),
        1,
        "one turn for the whole search: {entries:?}"
    );
}

// ---------------------------------------------------------------------------
// Session-turn commit correctness (SDK-1045 P1): declined turns are recorded in
// the search history, an empty answer commits nothing, and batch searches skip
// turn preparation.
// ---------------------------------------------------------------------------

/// Records every query/result pair written to the search history.
#[derive(Default)]
struct RecordingHistoryDb {
    queries: Mutex<Vec<(Uuid, String)>>,
    results: Mutex<Vec<(Uuid, String)>>,
}

impl RecordingHistoryDb {
    /// `(query text, result text)` per recorded row, in write order.
    fn rows(&self) -> Vec<(String, String)> {
        let queries = self.queries.lock().unwrap(); // lock poison is unrecoverable
        let results = self.results.lock().unwrap(); // lock poison is unrecoverable
        results
            .iter()
            .map(|(id, text)| {
                let query = queries.iter().find(|(qid, _)| qid == id).unwrap();
                (query.1.clone(), text.clone())
            })
            .collect()
    }
}

#[async_trait]
impl SearchHistoryDb for RecordingHistoryDb {
    async fn log_query(
        &self,
        query_text: &str,
        _query_type: &str,
        _user_id: Option<Uuid>,
        _dataset_id: Option<Uuid>,
    ) -> Result<Uuid, DatabaseError> {
        let id = Uuid::new_v4();
        self.queries
            .lock()
            .unwrap() // lock poison is unrecoverable
            .push((id, query_text.to_string()));
        Ok(id)
    }

    async fn log_result(
        &self,
        query_id: Uuid,
        serialized_result: &str,
        _user_id: Option<Uuid>,
        _dataset_id: Option<Uuid>,
    ) -> Result<Uuid, DatabaseError> {
        self.results
            .lock()
            .unwrap() // lock poison is unrecoverable
            .push((query_id, serialized_result.to_string()));
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

/// Answers session-turn analysis from fixed JSON and counts the calls: every
/// structured-output call a search makes is an analysis call.
struct RoutingLlm {
    analysis: serde_json::Value,
    analysis_calls: Mutex<usize>,
}

#[async_trait]
impl cognee_llm::Llm for RoutingLlm {
    async fn generate(
        &self,
        _messages: Vec<cognee_llm::Message>,
        _options: Option<cognee_llm::GenerationOptions>,
    ) -> cognee_llm::LlmResult<cognee_llm::GenerationResponse> {
        Err(cognee_llm::LlmError::ConfigError("not used".into()))
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        _messages: Vec<cognee_llm::Message>,
        _json_schema: &serde_json::Value,
        _options: Option<cognee_llm::GenerationOptions>,
    ) -> cognee_llm::LlmResult<serde_json::Value> {
        *self.analysis_calls.lock().unwrap() += 1; // lock poison is unrecoverable
        Ok(self.analysis.clone())
    }

    fn model(&self) -> &str {
        "routing-test"
    }
}

/// A graph-completion retriever with a scripted answer that records the query
/// it was asked in `get_context` and `get_completion`.
struct ScriptedRetriever {
    search_type: SearchType,
    prepares_turns: bool,
    answer: SearchOutput,
    context_queries: Mutex<Vec<String>>,
    completion_queries: Mutex<Vec<String>>,
}

#[async_trait]
impl SearchRetriever for ScriptedRetriever {
    fn search_type(&self) -> SearchType {
        self.search_type
    }

    fn supports_session_turn_preparation(&self) -> bool {
        self.prepares_turns
    }

    async fn get_context(
        &self,
        query: &str,
        _params: &SearchParams,
    ) -> Result<SearchContext, SearchError> {
        self.context_queries.lock().unwrap().push(query.into()); // lock poison is unrecoverable
        Ok(vec![SearchItem {
            id: None,
            score: Some(0.9),
            payload: json!({"text": "edge text"}),
        }])
    }

    async fn get_completion(
        &self,
        query: &str,
        _context: Option<SearchContext>,
        _session: &SessionContext,
        _params: &SearchParams,
    ) -> Result<SearchOutput, SearchError> {
        self.completion_queries
            .lock()
            .unwrap() // lock poison is unrecoverable
            .push(query.into());
        Ok(self.answer.clone())
    }
}

/// What the scripted retriever is and how its session is stored.
#[derive(Clone, Copy)]
struct HarnessKind {
    search_type: SearchType,
    prepares_turns: bool,
    /// Session memory switched off (`session.caching = false`).
    disabled_store: bool,
}

impl HarnessKind {
    /// A generative graph-completion retriever on a working session store.
    const GRAPH: Self = Self {
        search_type: SearchType::GraphCompletion,
        prepares_turns: true,
        disabled_store: false,
    };
}

struct TurnHarness {
    search_type: SearchType,
    orchestrator: SearchOrchestrator,
    manager: Arc<SessionManager>,
    history: Arc<RecordingHistoryDb>,
    llm: Arc<RoutingLlm>,
    retriever: Arc<ScriptedRetriever>,
    user: Uuid,
    _dir: tempfile::TempDir,
}

const SESSION: &str = "turn-session";

impl TurnHarness {
    /// A session holding one prior QA turn, so analysis may decline the next.
    async fn new(
        mode: cognee_search::SessionSearchMode,
        per_dataset: bool,
        answer: SearchOutput,
        analysis: serde_json::Value,
    ) -> Self {
        Self::of_kind(mode, per_dataset, answer, analysis, HarnessKind::GRAPH).await
    }

    async fn of_kind(
        mode: cognee_search::SessionSearchMode,
        per_dataset: bool,
        answer: SearchOutput,
        analysis: serde_json::Value,
        kind: HarnessKind,
    ) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn cognee_session::SessionStore> = if kind.disabled_store {
            Arc::new(cognee_session::DisabledSessionStore)
        } else {
            Arc::new(FsSessionStore::new(dir.path().join("sessions")))
        };
        let manager = Arc::new(SessionManager::new(store));
        let user = Uuid::new_v4();
        // A disabled store refuses writes; the session then simply starts empty.
        let _ = manager
            .save_qa(
                Some(SESSION),
                Some(&user.to_string()),
                "old question",
                "old answer",
                None,
                None,
            )
            .await;
        let retriever = Arc::new(ScriptedRetriever {
            search_type: kind.search_type,
            prepares_turns: kind.prepares_turns,
            answer,
            context_queries: Mutex::new(vec![]),
            completion_queries: Mutex::new(vec![]),
        });
        let mut registry = SearchTypeRegistry::new();
        registry.register(retriever.clone());
        let history = Arc::new(RecordingHistoryDb::default());
        let llm = Arc::new(RoutingLlm {
            analysis,
            analysis_calls: Mutex::new(0),
        });
        let orchestrator = SearchOrchestrator::new(registry)
            .with_database(history.clone())
            .with_session_manager(manager.clone())
            .with_llm(llm.clone())
            .with_auto_feedback_default(true)
            // Makes the orchestrator fetch the context itself, so the
            // retriever's `get_context` query is observable.
            .with_access_tracking()
            .with_session_search_mode(mode)
            .with_per_dataset_storage(per_dataset);
        Self {
            search_type: kind.search_type,
            orchestrator,
            manager,
            history,
            llm,
            retriever,
            user,
            _dir: dir,
        }
    }

    fn request(&self, query: &str) -> SearchRequest {
        let mut request = graph_request(query, Some(SESSION));
        request.search_type = self.search_type;
        request.user_id = Some(self.user);
        request.auto_feedback_detection = Some(true);
        request
    }

    async fn qa_count(&self) -> usize {
        self.manager
            .get_session(Some(SESSION), Some(&self.user.to_string()))
            .await
            .unwrap()
            .len()
    }

    async fn context_row_count(&self) -> usize {
        self.manager
            .get_session_context_entries(Some(SESSION), Some(&self.user.to_string()))
            .await
            .unwrap()
            .len()
    }
}

fn declining_analysis() -> serde_json::Value {
    json!({
        "response_to_user": "Noted, thanks.",
        "query_to_answer": null,
        "served_context_ratings": [],
        "candidate_context_updates": [
            {"section": "lessons_learned", "content": "Use X.", "confidence": 0.9}
        ],
        "previous_answer_rating": 2
    })
}

fn answering_analysis() -> serde_json::Value {
    json!({
        "response_to_user": null,
        "query_to_answer": "effective question",
        "served_context_ratings": [],
        "candidate_context_updates": [
            {"section": "preferences", "content": "Be terse.", "confidence": 0.9}
        ],
        "previous_answer_rating": 4
    })
}

fn answer(text: &str) -> SearchOutput {
    SearchOutput::Text(text.to_string())
}

async fn assert_declined_turn_logged(harness: &TurnHarness, request: SearchRequest) {
    let response = harness.orchestrator.search(&request).await.unwrap();

    assert!(matches!(&response.result, SearchOutput::Text(t) if t == "Noted, thanks."));
    assert_eq!(
        harness.history.rows(),
        vec![("No, use X.".to_string(), "Noted, thanks.".to_string())],
        "the acknowledgement is one history row under the raw query"
    );
    assert_eq!(harness.qa_count().await, 2, "ack is saved as the turn's QA");
}

#[tokio::test]
async fn sequential_declined_turn_is_logged_to_search_history() {
    let harness = TurnHarness::new(
        cognee_search::SessionSearchMode::Sequential,
        false,
        answer("unused"),
        declining_analysis(),
    )
    .await;
    assert_declined_turn_logged(&harness, harness.request("No, use X.")).await;
}

#[tokio::test]
async fn sequential_declined_turn_acknowledges_even_with_a_response_schema() {
    let harness = TurnHarness::new(
        cognee_search::SessionSearchMode::Sequential,
        false,
        answer("unused"),
        declining_analysis(),
    )
    .await;
    let mut request = harness.request("No, use X.");
    request.response_schema = Some(json!({"type": "object"}));

    assert_declined_turn_logged(&harness, request).await;
    assert!(
        harness
            .retriever
            .completion_queries
            .lock()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn concurrent_declined_turn_is_logged_to_search_history() {
    let harness = TurnHarness::new(
        cognee_search::SessionSearchMode::Concurrent,
        false,
        answer("discarded answer"),
        declining_analysis(),
    )
    .await;
    assert_declined_turn_logged(&harness, harness.request("No, use X.")).await;
}

#[tokio::test]
async fn fanout_declined_turn_is_logged_to_search_history() {
    let harness = TurnHarness::new(
        cognee_search::SessionSearchMode::Concurrent,
        true,
        answer("discarded answer"),
        declining_analysis(),
    )
    .await;
    let mut request = harness.request("No, use X.");
    request.dataset_ids = Some(vec![Uuid::new_v4(), Uuid::new_v4()]);

    assert_declined_turn_logged(&harness, request).await;
}

#[tokio::test]
async fn concurrent_empty_context_commits_nothing_for_a_declining_analysis() {
    let harness = TurnHarness::new(
        cognee_search::SessionSearchMode::Concurrent,
        false,
        SearchOutput::Texts(vec![]),
        declining_analysis(),
    )
    .await;

    let response = harness
        .orchestrator
        .search(&harness.request("No, use X."))
        .await
        .unwrap();

    assert!(matches!(&response.result, SearchOutput::Texts(t) if t.is_empty()));
    assert_eq!(harness.qa_count().await, 1, "no acknowledgement QA");
    assert_eq!(harness.context_row_count().await, 0, "no analysis commit");
}

#[tokio::test]
async fn concurrent_empty_context_commits_nothing_for_an_answering_analysis() {
    let harness = TurnHarness::new(
        cognee_search::SessionSearchMode::Concurrent,
        false,
        SearchOutput::Texts(vec![]),
        answering_analysis(),
    )
    .await;

    harness
        .orchestrator
        .search(&harness.request("Be terse. What now?"))
        .await
        .unwrap();

    assert_eq!(harness.qa_count().await, 1, "no QA for an empty answer");
    assert_eq!(harness.context_row_count().await, 0, "no analysis commit");
}

#[tokio::test]
async fn fanout_empty_context_commits_nothing() {
    for analysis in [declining_analysis(), answering_analysis()] {
        let harness = TurnHarness::new(
            cognee_search::SessionSearchMode::Concurrent,
            true,
            SearchOutput::Texts(vec![]),
            analysis,
        )
        .await;
        let mut request = harness.request("No, use X.");
        request.dataset_ids = Some(vec![Uuid::new_v4(), Uuid::new_v4()]);

        harness.orchestrator.search(&request).await.unwrap();

        assert_eq!(harness.qa_count().await, 1, "no QA for an empty answer");
        assert_eq!(harness.context_row_count().await, 0, "no analysis commit");
    }
}

#[tokio::test]
async fn fanout_answering_analysis_commits_once_an_answer_exists() {
    let harness = TurnHarness::new(
        cognee_search::SessionSearchMode::Concurrent,
        true,
        answer("fan-out answer"),
        answering_analysis(),
    )
    .await;
    let mut request = harness.request("Be terse. What now?");
    request.dataset_ids = Some(vec![Uuid::new_v4(), Uuid::new_v4()]);

    harness.orchestrator.search(&request).await.unwrap();

    assert_eq!(harness.qa_count().await, 2);
    assert!(harness.context_row_count().await > 0, "analysis committed");
}

#[tokio::test]
async fn batch_search_skips_session_turn_preparation() {
    let harness = TurnHarness::new(
        cognee_search::SessionSearchMode::Sequential,
        false,
        answer("unused"),
        declining_analysis(),
    )
    .await;

    let responses = harness
        .orchestrator
        .search_batch(&[harness.request("Great answer!")])
        .await
        .unwrap();

    assert_eq!(*harness.llm.analysis_calls.lock().unwrap(), 0); // lock poison is unrecoverable
    // No turn is prepared and no legacy detector runs: the message is searched.
    assert!(matches!(&responses[0].result, SearchOutput::Text(t) if t == "unused"));
}

#[tokio::test]
async fn empty_readable_scope_records_an_empty_result_text() {
    let harness = TurnHarness::new(
        cognee_search::SessionSearchMode::Sequential,
        true,
        answer("unused"),
        declining_analysis(),
    )
    .await;

    // Per-dataset storage with no dataset named and none readable.
    let results = harness
        .orchestrator
        .search_datasets(&harness.request("anything"))
        .await
        .unwrap();

    assert!(results.is_empty());
    assert_eq!(
        harness.history.rows(),
        vec![("anything".to_string(), String::new())]
    );
}

#[tokio::test]
async fn sequential_turn_retrieves_and_answers_with_the_effective_query_alone() {
    let harness = TurnHarness::new(
        cognee_search::SessionSearchMode::Sequential,
        false,
        answer("final answer"),
        answering_analysis(),
    )
    .await;

    harness
        .orchestrator
        .search(&harness.request("Be terse. What now?"))
        .await
        .unwrap();

    let queries = |recorded: &Mutex<Vec<String>>| recorded.lock().unwrap().clone(); // lock poison is unrecoverable
    assert_eq!(
        queries(&harness.retriever.context_queries),
        vec!["effective question".to_string()]
    );
    assert_eq!(
        queries(&harness.retriever.completion_queries),
        vec!["effective question".to_string()]
    );
    let session = harness
        .manager
        .get_session(Some(SESSION), Some(&harness.user.to_string()))
        .await
        .unwrap();
    assert_eq!(
        session.last().map(|e| e.question.as_str()),
        Some("Be terse. What now?"),
        "storage keeps the raw message"
    );
    assert_eq!(
        harness.history.rows(),
        vec![(
            "Be terse. What now?".to_string(),
            "final answer".to_string()
        )]
    );
}

#[tokio::test]
async fn an_empty_completion_text_is_still_an_answer_and_commits() {
    for per_dataset in [false, true] {
        let harness = TurnHarness::new(
            cognee_search::SessionSearchMode::Concurrent,
            per_dataset,
            answer(""),
            answering_analysis(),
        )
        .await;
        let mut request = harness.request("Be terse. What now?");
        if per_dataset {
            request.dataset_ids = Some(vec![Uuid::new_v4()]);
        }

        harness.orchestrator.search(&request).await.unwrap();

        // Python skips the commit only when no completion was generated, so a
        // generated "" is saved and committed together.
        assert_eq!(harness.qa_count().await, 2, "per_dataset={per_dataset}");
        assert!(
            harness.context_row_count().await > 0,
            "per_dataset={per_dataset}"
        );
    }
}

/// The latest QA row of the harness session as `(answer, context)`.
async fn last_qa(harness: &TurnHarness) -> (String, Option<String>) {
    let entries = harness
        .manager
        .get_session(Some(SESSION), Some(&harness.user.to_string()))
        .await
        .unwrap();
    let last = entries.last().expect("a QA row");
    (last.answer.clone(), last.context.clone())
}

/// Python's `supports_session_turn_preparation = False` retrievers make no
/// analysis call and write no feedback (Python's `detect_feedback` has no
/// callers): the message is just searched, and no turn is recorded (Python
/// records one only on its session-aware completion paths).
#[tokio::test]
async fn an_opted_out_retriever_makes_no_analysis_call_and_writes_no_feedback() {
    for mode in [
        cognee_search::SessionSearchMode::Sequential,
        cognee_search::SessionSearchMode::Concurrent,
    ] {
        let harness = TurnHarness::of_kind(
            mode,
            false,
            answer("chunk text"),
            declining_analysis(),
            HarnessKind {
                search_type: SearchType::Chunks,
                prepares_turns: false,
                disabled_store: false,
            },
        )
        .await;

        let response = harness
            .orchestrator
            .search(&harness.request("Great answer!"))
            .await
            .unwrap();

        assert!(matches!(&response.result, SearchOutput::Text(t) if t == "chunk text"));
        assert_eq!(*harness.llm.analysis_calls.lock().unwrap(), 0); // lock poison is unrecoverable
        assert_eq!(harness.context_row_count().await, 0, "no feedback written");
        assert_eq!(
            harness.qa_count().await,
            1,
            "only the seeded row: no turn saved"
        );
        let entries = harness
            .manager
            .get_session(Some(SESSION), Some(&harness.user.to_string()))
            .await
            .unwrap();
        assert!(
            entries.iter().all(|e| e.feedback_text.is_none()),
            "no feedback on any QA row"
        );
    }
}

/// CodingRules keeps the trait default and prepares turns.
#[tokio::test]
async fn coding_rules_prepares_turns() {
    let harness = TurnHarness::of_kind(
        cognee_search::SessionSearchMode::Sequential,
        false,
        answer("unused"),
        declining_analysis(),
        HarnessKind {
            search_type: SearchType::CodingRules,
            prepares_turns: true,
            disabled_store: false,
        },
    )
    .await;

    let response = harness
        .orchestrator
        .search(&harness.request("No, use X."))
        .await
        .unwrap();

    assert!(matches!(&response.result, SearchOutput::Text(t) if t == "Noted, thanks."));
    assert_eq!(*harness.llm.analysis_calls.lock().unwrap(), 1); // lock poison is unrecoverable
}

/// A retriever that leaves `supports_session_turn_preparation` at its default.
struct DefaultCapabilityRetriever {
    completion_queries: Mutex<Vec<String>>,
}

#[async_trait]
impl SearchRetriever for DefaultCapabilityRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::GraphCompletion
    }

    async fn get_context(
        &self,
        _query: &str,
        _params: &SearchParams,
    ) -> Result<SearchContext, SearchError> {
        Ok(vec![])
    }

    async fn get_completion(
        &self,
        query: &str,
        _context: Option<SearchContext>,
        _session: &SessionContext,
        _params: &SearchParams,
    ) -> Result<SearchOutput, SearchError> {
        self.completion_queries
            .lock()
            .unwrap() // lock poison is unrecoverable
            .push(query.into());
        Ok(answer("custom answer"))
    }
}

/// A session over a working store holding one prior QA turn, and the user.
async fn seeded_session(dir: &tempfile::TempDir) -> (Arc<SessionManager>, Uuid) {
    let manager = Arc::new(SessionManager::new(Arc::new(FsSessionStore::new(
        dir.path().join("sessions"),
    ))));
    let user = Uuid::new_v4();
    manager
        .save_qa(
            Some(SESSION),
            Some(&user.to_string()),
            "old question",
            "old answer",
            None,
            None,
        )
        .await
        .unwrap();
    (manager, user)
}

/// A custom (community) retriever keeps the trait default and prepares turns.
#[tokio::test]
async fn a_custom_retriever_prepares_turns_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let (manager, user) = seeded_session(&dir).await;
    let llm = Arc::new(RoutingLlm {
        analysis: declining_analysis(),
        analysis_calls: Mutex::new(0),
    });
    let orchestrator = SearchOrchestrator::new(SearchTypeRegistry::new())
        .with_community_retriever(
            "my_custom",
            Arc::new(DefaultCapabilityRetriever {
                completion_queries: Mutex::new(vec![]),
            }),
        )
        .with_session_manager(manager)
        .with_llm(llm.clone())
        .with_auto_feedback_default(true);
    let mut request = graph_request("No, use X.", Some(SESSION));
    request.user_id = Some(user);
    request.custom_search_type = Some("my_custom".into());

    let response = orchestrator.search(&request).await.unwrap();

    assert!(matches!(&response.result, SearchOutput::Text(t) if t == "Noted, thanks."));
    assert_eq!(*llm.analysis_calls.lock().unwrap(), 1); // lock poison is unrecoverable
}

/// FEELING_LUCKY resolves to the selected retriever before the turn is
/// prepared (Python's effective search type): a generative pick prepares the
/// turn, a raw-retrieval one does not.
#[tokio::test]
async fn feeling_lucky_prepares_the_turn_by_its_selected_retriever() {
    for (selected, prepares) in [
        (SearchType::GraphCompletion, true),
        (SearchType::Chunks, false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (manager, user) = seeded_session(&dir).await;
        let llm = Arc::new(RoutingLlm {
            analysis: answering_analysis(),
            analysis_calls: Mutex::new(0),
        });
        let delegate = Arc::new(ScriptedRetriever {
            search_type: selected,
            prepares_turns: prepares,
            answer: answer("picked answer"),
            context_queries: Mutex::new(vec![]),
            completion_queries: Mutex::new(vec![]),
        });
        // The selector's own LLM call fails here, so it falls back to `selected`.
        let lucky = cognee_search::FeelingLuckyRetriever::new(
            llm.clone(),
            std::collections::HashMap::from([(selected, delegate.clone() as _)]),
            Some(selected),
            None,
        );
        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(lucky));
        let orchestrator = SearchOrchestrator::new(registry)
            .with_session_manager(manager.clone())
            .with_llm(llm.clone())
            .with_auto_feedback_default(true);
        let mut request = graph_request("Be terse. What now?", Some(SESSION));
        request.search_type = SearchType::FeelingLucky;
        request.user_id = Some(user);

        orchestrator.search(&request).await.unwrap();

        assert_eq!(
            *llm.analysis_calls.lock().unwrap(), // lock poison is unrecoverable
            usize::from(prepares),
            "{selected:?}"
        );
        let asked = delegate.completion_queries.lock().unwrap().clone(); // lock poison is unrecoverable
        let expected = if prepares {
            "effective question"
        } else {
            "Be terse. What now?"
        };
        assert_eq!(asked, vec![expected.to_string()], "{selected:?}");
    }
}

/// A structured answer is stored as compact JSON in both modes (Python's
/// concurrent `model_dump_json`; its sequential path stores `str(model)`, a
/// deliberate divergence), and the context column is always empty.
#[tokio::test]
async fn a_structured_answer_is_persisted_as_compact_json() {
    for mode in [
        cognee_search::SessionSearchMode::Sequential,
        cognee_search::SessionSearchMode::Concurrent,
    ] {
        let harness = TurnHarness::new(
            mode,
            false,
            SearchOutput::Structured(json!({"city": "Paris", "n": 2})),
            answering_analysis(),
        )
        .await;
        let mut request = harness.request("Be terse. What now?");
        request.response_schema = Some(json!({"type": "object"}));

        harness.orchestrator.search(&request).await.unwrap();

        // The concurrent runner answers the raw message; the sequential turn
        // answers the analysis' effective query, so this proves which ran.
        let expected = if mode == cognee_search::SessionSearchMode::Concurrent {
            "Be terse. What now?"
        } else {
            "effective question"
        };
        let asked = harness.retriever.completion_queries.lock().unwrap().clone(); // lock poison is unrecoverable
        assert_eq!(asked, vec![expected.to_string()], "{mode:?}");
        assert_eq!(harness.qa_count().await, 2, "{mode:?} records the turn");
        let (stored, context) = last_qa(&harness).await;
        assert_eq!(stored, r#"{"city":"Paris","n":2}"#, "{mode:?}");
        assert!(context.unwrap_or_default().is_empty(), "{mode:?}");
    }
}

/// Several answer texts are stored joined by a blank line, in the single
/// store and in the per-dataset fan-out alike.
#[tokio::test]
async fn several_answer_texts_are_stored_with_one_join_rule() {
    for per_dataset in [false, true] {
        let harness = TurnHarness::new(
            cognee_search::SessionSearchMode::Sequential,
            per_dataset,
            SearchOutput::Texts(vec!["first".into(), "second".into()]),
            answering_analysis(),
        )
        .await;
        let mut request = harness.request("Be terse. What now?");
        if per_dataset {
            request.dataset_ids = Some(vec![Uuid::new_v4()]);
        }

        harness.orchestrator.search(&request).await.unwrap();

        let (stored, _) = last_qa(&harness).await;
        assert_eq!(stored, "first\n\nsecond", "per_dataset={per_dataset}");
    }
}

/// Python's `is_session_available_for_completion` needs a working cache: with
/// session memory off, concurrent mode falls back to the sequential turn.
#[tokio::test]
async fn concurrent_mode_needs_an_available_session_cache() {
    let harness = TurnHarness::of_kind(
        cognee_search::SessionSearchMode::Concurrent,
        false,
        answer("final answer"),
        answering_analysis(),
        HarnessKind {
            disabled_store: true,
            ..HarnessKind::GRAPH
        },
    )
    .await;

    harness
        .orchestrator
        .search(&harness.request("Be terse. What now?"))
        .await
        .unwrap();

    // Python makes no analysis call without an available session cache (even
    // with an explicit `auto_feedback_detection`), so neither the sequential
    // nor the concurrent runner analysed: the raw message is answered and no
    // QA row exists.
    assert_eq!(*harness.llm.analysis_calls.lock().unwrap(), 0); // lock poison is unrecoverable
    let completion_queries = harness.retriever.completion_queries.lock().unwrap().clone(); // lock poison is unrecoverable
    assert_eq!(completion_queries, vec!["Be terse. What now?".to_string()]);
    assert_eq!(harness.qa_count().await, 0);
}
