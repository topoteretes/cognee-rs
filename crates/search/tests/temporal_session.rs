//! Phase 10: Temporal search session integration tests.
//!
//! Verifies that temporal search results are stored in session history and that
//! the `SearchOrchestrator` correctly passes session context through to the
//! retriever.  Uses `FsSessionStore` for lightweight, file-based session
//! persistence and a fake `Temporal` retriever that captures the `SessionContext`
//! it receives so assertions can inspect the history propagation.
//!
//! Run with: cargo test --package cognee-search --test temporal_session -- --nocapture

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cognee_database::{DatabaseError, SearchHistoryDb, SearchHistoryEntry};
use cognee_session::{FsSessionStore, SessionContext, SessionManager, SessionStore};
use uuid::Uuid;

use cognee_search::retrievers::SearchRetriever;
use cognee_search::types::{
    SearchContext, SearchError, SearchOutput, SearchParams, SearchRequest, SearchType,
};
use cognee_search::{SearchOrchestrator, SearchTypeRegistry};

// ---------------------------------------------------------------------------
// Fake retriever that captures the SessionContext it receives
// ---------------------------------------------------------------------------

struct FakeTemporalRetriever {
    /// Captures every `SessionContext` received across all `get_completion` calls.
    captured_sessions: Arc<Mutex<Vec<SessionContext>>>,
    /// Counter for generating distinct answers.
    call_count: Arc<Mutex<u32>>,
}

impl FakeTemporalRetriever {
    fn new(
        captured_sessions: Arc<Mutex<Vec<SessionContext>>>,
        call_count: Arc<Mutex<u32>>,
    ) -> Self {
        Self {
            captured_sessions,
            call_count,
        }
    }
}

#[async_trait]
impl SearchRetriever for FakeTemporalRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::Temporal
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
        _query: &str,
        _context: Option<SearchContext>,
        session: &SessionContext,
        _params: &SearchParams,
    ) -> Result<SearchOutput, SearchError> {
        // Record the session context snapshot.
        self.captured_sessions
            .lock()
            .unwrap() // lock poison is unrecoverable
            .push(session.clone());

        let mut count = self.call_count.lock().unwrap(); // lock poison is unrecoverable
        *count += 1;
        Ok(SearchOutput::Text(format!("temporal answer #{}", *count)))
    }
}

// ---------------------------------------------------------------------------
// Fake SearchHistoryDb (no-op; we only care about session store here)
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
// Helper: build a SearchRequest targeting Temporal with the given session id
// ---------------------------------------------------------------------------

fn temporal_request(query: &str, session_id: Option<&str>) -> SearchRequest {
    SearchRequest {
        query_text: query.to_string(),
        search_type: SearchType::Temporal,
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
    }
}

// ---------------------------------------------------------------------------
// Test 1: temporal search passes session context to the retriever
// ---------------------------------------------------------------------------

/// Build a search using `SearchOrchestrator`, provide a session_id, run a
/// temporal search, and verify:
/// 1. The search completes successfully.
/// 2. The retriever received a `SessionContext` with the correct session_id.
/// 3. The QA pair is persisted to the `FsSessionStore`.
#[tokio::test]
async fn temporal_search_passes_session_context() {
    let temp_dir = tempfile::tempdir().expect("tempdir creation must succeed");
    let session_store = Arc::new(FsSessionStore::new(temp_dir.path().join("sessions")));
    let session_manager = Arc::new(SessionManager::new(session_store.clone()));

    let captured_sessions: Arc<Mutex<Vec<SessionContext>>> = Arc::new(Mutex::new(Vec::new()));
    let call_count: Arc<Mutex<u32>> = Arc::new(Mutex::new(0));

    let retriever = Arc::new(FakeTemporalRetriever::new(
        Arc::clone(&captured_sessions),
        Arc::clone(&call_count),
    ));

    let mut registry = SearchTypeRegistry::new();
    registry.register(retriever);

    let orchestrator = SearchOrchestrator::new(registry)
        .with_database(Arc::new(NoOpHistoryDb))
        .with_session_manager(session_manager.clone());

    let session_id = "test-temporal-session-1";

    // Execute a temporal search with a session_id.
    let response = orchestrator
        .search(&temporal_request(
            "What events happened in 2024?",
            Some(session_id),
        ))
        .await
        .expect("temporal search with session should succeed");

    // 1. Verify search returned a text result.
    match &response.result {
        SearchOutput::Text(text) => {
            assert!(
                text.contains("temporal answer"),
                "expected temporal answer, got: {text}"
            );
        }
        other => panic!("expected Text output, got: {other:?}"),
    }

    // 2. Verify the retriever received the correct session context.
    {
        let sessions = captured_sessions.lock().unwrap(); // lock poison is unrecoverable
        assert_eq!(sessions.len(), 1, "retriever should be called exactly once");
        assert_eq!(
            sessions[0].session_id.as_deref(),
            Some(session_id),
            "session_id should be forwarded to the retriever"
        );
        // First call: history should be empty since there are no prior exchanges.
        assert!(
            sessions[0].history.is_empty(),
            "first search in a session should have empty history"
        );
        assert!(
            sessions[0].formatted_history.is_empty(),
            "first search in a session should have empty formatted history"
        );
    }

    // 3. Verify the QA pair was persisted to the session store.
    let entries = session_store
        .get_all_qa_entries(session_id, None)
        .await
        .expect("reading session entries should succeed");

    assert_eq!(
        entries.len(),
        1,
        "one QA entry should be stored after a single search"
    );
    assert_eq!(entries[0].question, "What events happened in 2024?");
    assert!(entries[0].answer.contains("temporal answer #1"));
}

// ---------------------------------------------------------------------------
// Test 2: multiple temporal queries in the same session accumulate history
// ---------------------------------------------------------------------------

/// Run two temporal queries with the same session_id and verify:
/// 1. Both queries complete successfully.
/// 2. The second query's retriever invocation receives session history from
///    the first query.
/// 3. The session store contains two QA entries after both queries.
#[tokio::test]
async fn temporal_search_multiple_queries_in_session() {
    let temp_dir = tempfile::tempdir().expect("tempdir creation must succeed");
    let session_store = Arc::new(FsSessionStore::new(temp_dir.path().join("sessions")));
    let session_manager = Arc::new(SessionManager::new(session_store.clone()));

    let captured_sessions: Arc<Mutex<Vec<SessionContext>>> = Arc::new(Mutex::new(Vec::new()));
    let call_count: Arc<Mutex<u32>> = Arc::new(Mutex::new(0));

    let retriever = Arc::new(FakeTemporalRetriever::new(
        Arc::clone(&captured_sessions),
        Arc::clone(&call_count),
    ));

    let mut registry = SearchTypeRegistry::new();
    registry.register(retriever);

    let orchestrator = SearchOrchestrator::new(registry)
        .with_database(Arc::new(NoOpHistoryDb))
        .with_session_manager(session_manager.clone());

    let session_id = "test-temporal-session-multi";

    // --- First query ---
    let response1 = orchestrator
        .search(&temporal_request(
            "What happened in World War II?",
            Some(session_id),
        ))
        .await
        .expect("first temporal search should succeed");

    match &response1.result {
        SearchOutput::Text(text) => assert!(
            text.contains("temporal answer #1"),
            "first response: {text}"
        ),
        other => panic!("expected Text output for first query, got: {other:?}"),
    }

    // --- Second query (same session) ---
    let response2 = orchestrator
        .search(&temporal_request(
            "What happened after the war ended?",
            Some(session_id),
        ))
        .await
        .expect("second temporal search should succeed");

    match &response2.result {
        SearchOutput::Text(text) => assert!(
            text.contains("temporal answer #2"),
            "second response: {text}"
        ),
        other => panic!("expected Text output for second query, got: {other:?}"),
    }

    // Verify the retriever was called twice.
    {
        let sessions = captured_sessions.lock().unwrap(); // lock poison is unrecoverable
        assert_eq!(sessions.len(), 2, "retriever should be called twice");

        // First call: empty history (no prior conversation).
        assert!(
            sessions[0].history.is_empty(),
            "first call should have empty history"
        );

        // Second call: should include the first QA pair in the history.
        assert!(
            !sessions[1].history.is_empty(),
            "second call should have non-empty history from the first exchange"
        );
        assert_eq!(
            sessions[1].history.len(),
            2,
            "second call should have 2 history messages (user + assistant from first exchange)"
        );
        assert_eq!(
            sessions[1].history[0].content, "What happened in World War II?",
            "first history message should be the first question"
        );
        assert!(
            sessions[1].history[1]
                .content
                .contains("temporal answer #1"),
            "second history message should be the first answer"
        );

        // Verify formatted_history is non-empty on the second call.
        assert!(
            !sessions[1].formatted_history.is_empty(),
            "second call should have non-empty formatted_history"
        );
        assert!(
            sessions[1]
                .formatted_history
                .contains("What happened in World War II?"),
            "formatted_history should contain the first question"
        );
    }

    // Verify the session store has both QA entries persisted.
    let entries = session_store
        .get_all_qa_entries(session_id, None)
        .await
        .expect("reading session entries should succeed");

    assert_eq!(
        entries.len(),
        2,
        "two QA entries should be stored after two searches"
    );
    assert_eq!(entries[0].question, "What happened in World War II?");
    assert_eq!(entries[1].question, "What happened after the war ended?");
}
