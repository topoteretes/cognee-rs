#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for session→search wiring (Part B of task 20).
//!
//! These tests verify:
//! 1. `search_prepends_graph_context` — stored graph snapshot is prepended to
//!    the session history with the exact Python prefix.
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
        save_interaction: Some(true),
        user_id: None,
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

fn graph_request_with_feedback_detection(query: &str, session_id: Option<&str>) -> SearchRequest {
    let mut r = graph_request(query, session_id);
    r.auto_feedback_detection = Some(true);
    r
}

// ---------------------------------------------------------------------------
// Test 1: search_prepends_graph_context
//
// Store a graph context snapshot via `set_graph_context`, run a session search,
// assert the SessionContext passed to the retriever has `formatted_history`
// starting with the exact Python prefix
// "Background knowledge from the knowledge graph:\n".
// ---------------------------------------------------------------------------

#[tokio::test]
async fn search_prepends_graph_context() {
    let dir = tempfile::tempdir().unwrap();
    let session_store = Arc::new(FsSessionStore::new(dir.path().join("sessions")));
    let session_manager = Arc::new(SessionManager::new(session_store.clone()));

    let session_id = "gc-session-1";
    let snapshot = "Rust is a systems programming language.";

    // Store the graph snapshot via the session manager.
    session_manager
        .set_graph_context(Some(session_id), None, snapshot)
        .await
        .expect("set_graph_context must succeed");

    // Seed one QA exchange so there is existing formatted_history.
    session_store
        .create_qa_entry(session_id, None, "prior question", "prior answer", None)
        .await
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

    // The Python prefix must be the very first thing in formatted_history.
    assert!(
        fh.starts_with("Background knowledge from the knowledge graph:\n"),
        "formatted_history must start with the Python graph-context prefix; got: {fh:?}"
    );
    // The snapshot text must appear right after the prefix.
    assert!(
        fh.contains(snapshot),
        "formatted_history must contain the graph snapshot; got: {fh:?}"
    );
    // Prior conversation history must also appear.
    assert!(
        fh.contains("prior question"),
        "formatted_history must contain prior history; got: {fh:?}"
    );
    // The graph_context field on SessionContext must be populated.
    assert_eq!(
        sessions[0].graph_context.as_deref(),
        Some(snapshot),
        "SessionContext.graph_context must hold the raw snapshot"
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

    // Manually create the first QA entry (the prior entry that feedback targets).
    session_store
        .create_qa_entry(session_id, None, "first question", "first answer", None)
        .await
        .unwrap();

    // The MockLlm queue: first response is for feedback detection
    // (feedback_detected=true, score=5, not a follow-up question → pure feedback
    // so the search returns early without hitting the retriever).
    let feedback_json = serde_json::to_string(&serde_json::json!({
        "feedback_detected": true,
        "feedback_text": "Great answer!",
        "feedback_score": 5.0,
        "response_to_user": "Thank you for the feedback!",
        "contains_followup_question": false
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
        .with_llm(llm);

    // Issue the feedback turn.
    let response = orchestrator
        .search(&graph_request_with_feedback_detection(
            "That was a great answer!",
            Some(session_id),
        ))
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

    // Read all entries — should still be just the original one (no new QA entry
    // for a pure-feedback turn, matching Python semantics).
    let entries = session_store
        .get_all_qa_entries(session_id, None)
        .await
        .expect("reading session entries must succeed");

    // The original entry must now carry the feedback.
    let prior = entries
        .iter()
        .find(|e| e.question == "first question")
        .expect("original QA entry must still exist");

    assert_eq!(
        prior.feedback_text.as_deref(),
        Some("Great answer!"),
        "feedback_text must be persisted to the prior entry"
    );
    assert_eq!(
        prior.feedback_score,
        Some(5),
        "feedback_score must be persisted to the prior entry"
    );

    // feedback_weights_applied must be false (reset by add_feedback).
    let memify = prior
        .memify_metadata
        .as_ref()
        .expect("memify_metadata must be set after add_feedback");
    assert_eq!(
        memify.get("feedback_weights_applied"),
        Some(&false),
        "feedback_weights_applied must be false after feedback is stored"
    );
}
