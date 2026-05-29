//! Integration tests for `recall()` scope widening (LIB-07).
//!
//! Validates Python parity (`cognee/api/v1/recall/recall.py:317-531`,
//! `cognee/memory/entries.py:81-115`) for:
//!   * `auto` resolution per `(session_id, datasets, query_type)`,
//!   * scope-driven source fan-out across `graph` / `session` / `trace` /
//!     `graph_context`,
//!   * graceful degradation when `session_id` is `None`,
//!   * Rust-only ergonomics: empty `Vec<RecallScope>` collapses to `[Auto]`.

use std::sync::Arc;

use async_trait::async_trait;
use cognee_lib::api::recall::{RecallScope, RecallSource, ScopeInput, normalize_scope, recall};
use cognee_search::orchestration::SearchTypeRegistry;
use cognee_search::retrievers::SearchRetriever;
use cognee_search::types::{SearchContext, SearchError, SearchOutput, SearchParams};
use cognee_search::{SearchOrchestrator, SearchType};
use cognee_session::{FsSessionStore, SessionContext, SessionManager, SessionStore};
use tempfile::TempDir;

const USER_ID: &str = "user-1";
const SESSION_ID: &str = "sess-1";

/// Minimal retriever that returns a fixed text completion for any registered
/// search type. Used to keep the graph-source path runnable without real
/// backends.
struct StubRetriever(SearchType);

#[async_trait]
impl SearchRetriever for StubRetriever {
    fn search_type(&self) -> SearchType {
        self.0
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
        _session: &SessionContext,
        _params: &SearchParams,
    ) -> Result<SearchOutput, SearchError> {
        Ok(SearchOutput::Text("graph-stub".to_string()))
    }
}

fn build_orchestrator() -> SearchOrchestrator {
    let mut registry = SearchTypeRegistry::new();
    // Register every search type that recall() may route to so that any
    // auto-routed test still finds a retriever.
    for st in [
        SearchType::GraphCompletion,
        SearchType::GraphSummaryCompletion,
        SearchType::Temporal,
        SearchType::RagCompletion,
        SearchType::Chunks,
        SearchType::Summaries,
    ] {
        registry.register(Arc::new(StubRetriever(st)));
    }
    SearchOrchestrator::new(registry)
}

struct Harness {
    _sess_dir: TempDir,
    orchestrator: SearchOrchestrator,
    store: Arc<dyn SessionStore>,
    manager: SessionManager,
}

async fn build_harness() -> Harness {
    let sess_dir = TempDir::new().expect("tempdir");
    let store: Arc<dyn SessionStore> = Arc::new(FsSessionStore::new(sess_dir.path()));
    let manager = SessionManager::new(Arc::clone(&store));
    Harness {
        _sess_dir: sess_dir,
        orchestrator: build_orchestrator(),
        store,
        manager,
    }
}

async fn seed_qa(store: &dyn SessionStore, q: &str, a: &str) {
    store
        .create_qa_entry(SESSION_ID, Some(USER_ID), q, a, None)
        .await
        .expect("create qa");
}

async fn seed_trace(manager: &SessionManager, origin: &str, query: &str, ctx: &str) {
    manager
        .add_agent_trace_step(
            USER_ID,
            Some(SESSION_ID),
            origin,
            "success",
            query,
            ctx,
            serde_json::json!({}),
            None,
            "",
            false,
        )
        .await
        .expect("add trace step");
}

// ---------------------------------------------------------------------------
// auto resolution
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_scope_auto_with_session_id_uses_session_path() {
    let h = build_harness().await;
    seed_qa(&*h.store, "what is rust", "a systems language").await;

    let result = recall(
        "rust language",
        None,
        None,
        10,
        false,
        Some(SESSION_ID),
        Some(USER_ID),
        &h.orchestrator,
        Some(&*h.store),
        Some(&h.manager),
        None, // scope = None => "auto"
    )
    .await
    .expect("recall ok");

    assert!(
        result
            .items
            .iter()
            .any(|i| i.source == RecallSource::Session),
        "expected at least one session item; got {:?}",
        result.items.iter().map(|i| i.source).collect::<Vec<_>>()
    );
    // auto_fallthrough short-circuits the graph runner once session has hits.
    assert!(
        result.items.iter().all(|i| i.source != RecallSource::Graph),
        "graph runner should be short-circuited when session matched"
    );
}

#[tokio::test]
async fn test_scope_auto_without_session_id_uses_graph_path() {
    let h = build_harness().await;

    let result = recall(
        "anything",
        None,
        None,
        10,
        false,
        None, // no session_id
        Some(USER_ID),
        &h.orchestrator,
        Some(&*h.store),
        Some(&h.manager),
        None, // scope = None => "auto" => [Graph]
    )
    .await
    .expect("recall ok");

    assert!(!result.items.is_empty(), "graph stub should yield a result");
    assert!(
        result.items.iter().all(|i| i.source == RecallSource::Graph),
        "all items should be graph-tagged when session_id is None"
    );
}

// ---------------------------------------------------------------------------
// explicit scope per source
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_scope_session_returns_qa_pairs() {
    let h = build_harness().await;
    seed_qa(
        &*h.store,
        "rust ownership rules",
        "borrow checker enforces them",
    )
    .await;
    seed_qa(&*h.store, "what is python", "an interpreted language").await;

    let result = recall(
        "rust ownership",
        None,
        None,
        10,
        false,
        Some(SESSION_ID),
        Some(USER_ID),
        &h.orchestrator,
        Some(&*h.store),
        Some(&h.manager),
        Some(vec![RecallScope::Session]),
    )
    .await
    .expect("recall ok");

    assert!(!result.items.is_empty(), "expected session matches");
    assert!(
        result
            .items
            .iter()
            .all(|i| i.source == RecallSource::Session)
    );
    assert!(
        result.search_response.is_none(),
        "session-only scope must not invoke graph"
    );
}

#[tokio::test]
async fn test_scope_trace_returns_trace_entries() {
    let h = build_harness().await;
    seed_trace(
        &h.manager,
        "search.recall",
        "find rust facts",
        "ownership and borrowing",
    )
    .await;
    seed_trace(
        &h.manager,
        "ingest.add",
        "store a doc",
        "doc about python coroutines",
    )
    .await;

    let result = recall(
        "rust ownership",
        None,
        None,
        10,
        false,
        Some(SESSION_ID),
        Some(USER_ID),
        &h.orchestrator,
        Some(&*h.store),
        Some(&h.manager),
        Some(vec![RecallScope::Trace]),
    )
    .await
    .expect("recall ok");

    assert!(
        !result.items.is_empty(),
        "expected trace match for 'rust ownership'"
    );
    assert!(
        result.items.iter().all(|i| i.source == RecallSource::Trace),
        "trace-only scope should yield only trace items"
    );
}

#[tokio::test]
async fn test_scope_graph_context_returns_subgraph() {
    let h = build_harness().await;
    let snapshot = "graph-knowledge: rust borrow checker; entity:Rust; rel:has_feature.";
    h.manager
        .set_graph_context(Some(SESSION_ID), Some(USER_ID), snapshot)
        .await
        .expect("set graph context");

    let result = recall(
        "doesn't matter -- not query-matched",
        None,
        None,
        10,
        false,
        Some(SESSION_ID),
        Some(USER_ID),
        &h.orchestrator,
        Some(&*h.store),
        Some(&h.manager),
        Some(vec![RecallScope::GraphContext]),
    )
    .await
    .expect("recall ok");

    assert_eq!(result.items.len(), 1);
    assert_eq!(result.items[0].source, RecallSource::GraphContext);
    assert_eq!(
        result.items[0].content,
        serde_json::Value::String(snapshot.to_string())
    );
}

#[tokio::test]
async fn test_scope_all_merges_four_sources() {
    let h = build_harness().await;
    seed_qa(&*h.store, "session q rust", "session a rust").await;
    seed_trace(
        &h.manager,
        "trace.fn",
        "trace q about rust",
        "trace ctx rust",
    )
    .await;
    h.manager
        .set_graph_context(Some(SESSION_ID), Some(USER_ID), "graph-context rust note")
        .await
        .expect("set graph context");

    let result = recall(
        "rust",
        None,
        None,
        10,
        false,
        Some(SESSION_ID),
        Some(USER_ID),
        &h.orchestrator,
        Some(&*h.store),
        Some(&h.manager),
        Some(vec![
            RecallScope::Graph,
            RecallScope::Session,
            RecallScope::Trace,
            RecallScope::GraphContext,
        ]),
    )
    .await
    .expect("recall ok");

    let sources: std::collections::HashSet<RecallSource> =
        result.items.iter().map(|i| i.source).collect();
    assert!(sources.contains(&RecallSource::Graph));
    assert!(sources.contains(&RecallSource::Session));
    assert!(sources.contains(&RecallSource::Trace));
    assert!(sources.contains(&RecallSource::GraphContext));

    // Order: caller asked Graph first, so the first item should be Graph.
    assert_eq!(
        result.items.first().map(|i| i.source),
        Some(RecallSource::Graph)
    );
}

#[tokio::test]
async fn test_scope_session_without_session_id_returns_empty() {
    let h = build_harness().await;
    seed_qa(&*h.store, "q1", "a1").await;

    let result = recall(
        "q1",
        None,
        None,
        10,
        false,
        None, // no session_id
        Some(USER_ID),
        &h.orchestrator,
        Some(&*h.store),
        Some(&h.manager),
        Some(vec![RecallScope::Session]),
    )
    .await
    .expect("recall ok");

    assert!(
        result.items.is_empty(),
        "session runner must short-circuit empty when session_id is None"
    );
}

#[tokio::test]
async fn test_scope_unknown_value_returns_error() {
    let err = normalize_scope(Some(ScopeInput::from("bogus_scope"))).expect_err("should error");
    let msg = err.to_string();
    assert!(
        msg.contains("Unknown recall scope(s)"),
        "expected Python-parity error message; got: {msg}"
    );
    assert!(
        msg.contains("bogus_scope"),
        "expected unknown value to appear in error; got: {msg}"
    );
    assert!(
        msg.contains(r#"["all", "auto", "graph", "graph_context", "session", "trace"]"#),
        "expected canonical sorted valid-values list; got: {msg}"
    );
}
