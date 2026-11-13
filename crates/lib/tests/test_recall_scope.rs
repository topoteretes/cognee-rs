#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for `recall()` scope widening (LIB-07).
//!
//! Validates Python parity (`cognee/api/v1/recall/recall.py:574-594`,
//! `cognee/memory/entries.py:207-258`) for:
//!   * `auto` resolution per `(session_id, datasets, query_type)`,
//!   * the explicit `session_first` short-circuit,
//!   * `tools` / `code` refused before any source runs (SDK-1537),
//!   * scope-driven source fan-out across `graph` / `session` / `trace` /
//!     `session_context`
//!     (`graph_context` is a deprecated alias of `graph`),
//!   * graceful degradation when `session_id` is `None`,
//!   * an explicit empty scope list runs nothing (`normalize_scope([])` ->
//!     `[]`), while an absent scope (`None`) is auto.

use std::sync::Arc;

use async_trait::async_trait;
use cognee::api::ApiError;
use cognee::api::recall::{
    RecallItem, RecallOptions, RecallScope, RecallSource, ScopeInput, normalize_scope, recall,
};
use cognee_search::orchestration::SearchTypeRegistry;
use cognee_search::retrievers::SearchRetriever;
use cognee_search::types::{SearchContext, SearchError, SearchOutput, SearchParams};
use cognee_search::{SearchOrchestrator, SearchType};
use cognee_session::{
    CandidateContextUpdate, FsSessionStore, SessionContext, SessionManager, SessionStore,
    apply_candidate_updates,
};
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
        SearchType::HybridCompletion,
        SearchType::ChunksLexical,
        SearchType::CodingRules,
        SearchType::Temporal,
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
        None,
        10,
        false,
        Some(SESSION_ID),
        Some(USER_ID),
        &h.orchestrator,
        Some(&*h.store),
        Some(&h.manager),
        None, // scope = None => "auto"
        None, // options
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
        None,
        10,
        false,
        None, // no session_id
        Some(USER_ID),
        &h.orchestrator,
        Some(&*h.store),
        Some(&h.manager),
        None, // scope = None => "auto" => [Graph]
        None, // options
    )
    .await
    .expect("recall ok");

    assert!(!result.items.is_empty(), "graph stub should yield a result");
    assert!(
        result.items.iter().all(|i| i.source == RecallSource::Graph),
        "all items should be graph-tagged when session_id is None"
    );
}

/// Python `recall.py:576-585`: with a session id and a pinned query type,
/// `auto` is graph only (`elif session_id and query_type is None`), so the
/// seeded session entry does not answer.
#[tokio::test]
async fn test_scope_auto_with_session_id_and_query_type_uses_graph_only() {
    let h = build_harness().await;
    seed_qa(&*h.store, "what is rust", "a systems language").await;

    let result = recall(
        "rust language",
        Some(SearchType::HybridCompletion),
        None,
        None,
        10,
        false,
        Some(SESSION_ID),
        Some(USER_ID),
        &h.orchestrator,
        Some(&*h.store),
        Some(&h.manager),
        None,
        None,
    )
    .await
    .expect("recall ok");

    assert!(!result.items.is_empty(), "the graph should answer");
    assert!(
        result.items.iter().all(|i| i.source == RecallSource::Graph),
        "auto with a query type must not read the session; got {:?}",
        result.items.iter().map(|i| i.source).collect::<Vec<_>>()
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
        None,
        10,
        false,
        Some(SESSION_ID),
        Some(USER_ID),
        &h.orchestrator,
        Some(&*h.store),
        Some(&h.manager),
        Some(vec![RecallScope::Session]),
        None,
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
        None,
        10,
        false,
        Some(SESSION_ID),
        Some(USER_ID),
        &h.orchestrator,
        Some(&*h.store),
        Some(&h.manager),
        Some(vec![RecallScope::Trace]),
        None,
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

/// `graph_context` is a deprecated alias of `graph` (Python `3401da38a`
/// removed the graph-context lane): it normalizes to `Graph` and recalls graph
/// results only.
#[tokio::test]
async fn test_scope_graph_context_is_a_deprecated_alias_of_graph() {
    let h = build_harness().await;
    seed_qa(&*h.store, "session q rust", "session a rust").await;

    let scope = normalize_scope(Some(ScopeInput::from("graph_context"))).expect("normalize");
    assert_eq!(scope, vec![RecallScope::Graph]);

    let result = recall(
        "rust",
        None,
        None,
        None,
        10,
        false,
        Some(SESSION_ID),
        Some(USER_ID),
        &h.orchestrator,
        Some(&*h.store),
        Some(&h.manager),
        Some(scope),
        None,
    )
    .await
    .expect("recall ok");

    assert!(!result.items.is_empty());
    assert!(
        !result.items.is_empty(),
        "the alias must run the graph source, not an empty snapshot leg"
    );
    assert!(
        result.items.iter().all(|i| i.source == RecallSource::Graph),
        "the alias recalls graph results only"
    );
}

#[tokio::test]
async fn test_scope_all_merges_three_sources() {
    let h = build_harness().await;
    seed_qa(&*h.store, "session q rust", "session a rust").await;
    seed_trace(
        &h.manager,
        "trace.fn",
        "trace q about rust",
        "trace ctx rust",
    )
    .await;

    let result = recall(
        "rust",
        None,
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
        ]),
        None,
    )
    .await
    .expect("recall ok");

    let sources: std::collections::HashSet<RecallSource> =
        result.items.iter().map(|i| i.source).collect();
    assert!(sources.contains(&RecallSource::Graph));
    assert!(sources.contains(&RecallSource::Session));
    assert!(sources.contains(&RecallSource::Trace));

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
        None,
        10,
        false,
        None, // no session_id
        Some(USER_ID),
        &h.orchestrator,
        Some(&*h.store),
        Some(&h.manager),
        Some(vec![RecallScope::Session]),
        None,
    )
    .await
    .expect("recall ok");

    assert!(
        result.items.is_empty(),
        "session runner must short-circuit empty when session_id is None"
    );
}

/// Python: `normalize_scope([])` -> `[]`, so `recall(scope=[])` runs no lane
/// and returns `[]`, even with a session id and a graph that would answer.
#[tokio::test]
async fn test_scope_explicit_empty_list_runs_nothing() {
    let h = build_harness().await;
    seed_qa(&*h.store, "q1", "a1").await;

    let result = recall(
        "q1",
        None,
        None,
        None,
        10,
        false,
        Some("s1"),
        Some(USER_ID),
        &h.orchestrator,
        Some(&*h.store),
        Some(&h.manager),
        Some(vec![]),
        None,
    )
    .await
    .expect("recall ok");

    assert!(result.items.is_empty(), "an empty scope runs no source");
    assert!(result.search_type_used.is_none(), "the graph did not run");
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
        msg.contains(
            r#"['all', 'auto', 'code', 'graph', 'graph_context', 'session', 'session_context', 'session_first', 'tools', 'trace']"#
        ),
        "expected canonical sorted valid-values list; got: {msg}"
    );
}

// ---------------------------------------------------------------------------
// session_first (Python recall.py:586-594)
// ---------------------------------------------------------------------------

/// `scope=[session_first]` with a session hit answers from the session and
/// skips the graph, even with a pinned query type (which would make `auto`
/// run both).
#[tokio::test]
async fn test_scope_session_first_hit_skips_graph() {
    let h = build_harness().await;
    seed_qa(&*h.store, "what is rust", "a systems language").await;

    let result = recall(
        "rust language",
        Some(SearchType::HybridCompletion),
        None,
        None,
        10,
        false,
        Some(SESSION_ID),
        Some(USER_ID),
        &h.orchestrator,
        Some(&*h.store),
        Some(&h.manager),
        Some(vec![RecallScope::SessionFirst]),
        None,
    )
    .await
    .expect("recall ok");

    assert!(!result.items.is_empty(), "the session should answer");
    assert!(
        result
            .items
            .iter()
            .all(|i| i.source == RecallSource::Session),
        "a session hit must skip the graph; got {:?}",
        result.items.iter().map(|i| i.source).collect::<Vec<_>>()
    );
    assert!(result.search_response.is_none(), "the graph must not run");
}

/// `scope=[session_first]` with a session miss falls through to the graph.
#[tokio::test]
async fn test_scope_session_first_miss_runs_graph() {
    let h = build_harness().await;
    seed_qa(&*h.store, "what is rust", "a systems language").await;

    let result = recall(
        "unrelated words entirely",
        Some(SearchType::HybridCompletion),
        None,
        None,
        10,
        false,
        Some(SESSION_ID),
        Some(USER_ID),
        &h.orchestrator,
        Some(&*h.store),
        Some(&h.manager),
        Some(vec![RecallScope::SessionFirst]),
        None,
    )
    .await
    .expect("recall ok");

    assert!(!result.items.is_empty(), "the graph should answer");
    assert!(
        result.items.iter().all(|i| i.source == RecallSource::Graph),
        "a session miss must fall through to the graph; got {:?}",
        result.items.iter().map(|i| i.source).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// tools / code: accepted names, not supported yet (SDK-1040 D1)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_scope_tools_and_code_fail_before_any_source_runs() {
    for (scope, name) in [
        (vec![RecallScope::Session, RecallScope::Tools], "tools"),
        (vec![RecallScope::Code, RecallScope::Graph], "code"),
    ] {
        let h = build_harness().await;
        seed_qa(&*h.store, "what is rust", "a systems language").await;

        let err = recall(
            "rust language",
            None,
            None,
            None,
            10,
            false,
            Some(SESSION_ID),
            Some(USER_ID),
            &h.orchestrator,
            Some(&*h.store),
            Some(&h.manager),
            Some(scope),
            None,
        )
        .await
        .expect_err("unsupported scope must fail");

        match err {
            ApiError::InvalidArgument(message) => assert_eq!(
                message,
                format!(
                    "Recall scope '{name}' is not supported yet in the Rust SDK \
                     (tracked in SDK-1537)."
                )
            ),
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }
}

async fn seed_context(manager: &SessionManager, profile: &str, section: &str, content: &str) {
    let candidate = CandidateContextUpdate::new(profile, section, content, 0.9);
    let touched =
        apply_candidate_updates(manager, SESSION_ID, USER_ID, "src-1", &[candidate]).await;
    assert_eq!(touched.len(), 1, "seeded {profile}/{section}");
}

/// Recall with only `scope` and `options` varying.
async fn recall_scoped(
    h: &Harness,
    scope: Vec<RecallScope>,
    options: Option<RecallOptions>,
) -> Vec<RecallItem> {
    recall(
        "how should I answer",
        None,
        None,
        None,
        10,
        false,
        Some(SESSION_ID),
        Some(USER_ID),
        &h.orchestrator,
        Some(&*h.store),
        Some(&h.manager),
        Some(scope),
        options,
    )
    .await
    .expect("recall ok")
    .items
}

/// Python `_fetch_session_context` (`recall.py:384-424`): one entry carrying
/// the rendered block and its profile, `qa` by default.
#[tokio::test]
async fn test_scope_session_context_returns_one_qa_entry() {
    let h = build_harness().await;
    seed_context(&h.manager, "qa", "rules", "Always cite sources").await;
    seed_context(&h.manager, "agent", "tool_rules", "Run uv sync first").await;

    let items = recall_scoped(&h, vec![RecallScope::SessionContext], None).await;

    assert_eq!(items.len(), 1, "{items:?}");
    let item = &items[0];
    assert_eq!(item.source, RecallSource::SessionContext);
    assert_eq!(item.content["context_profile"], "qa");
    let block = item.content["content"].as_str().expect("block");
    assert!(block.starts_with("## Active session guidance\n"), "{block}");
    assert!(block.contains("### Rules\n"), "{block}");
    assert!(block.contains("Always cite sources"), "{block}");
    assert!(!block.contains("Run uv sync first"), "{block}");
    // Serialized as the bindings hand it out.
    let wire = serde_json::to_value(item).expect("serialize");
    assert_eq!(wire["source"], "session_context");
}

#[tokio::test]
async fn test_scope_session_context_agent_profile_uses_agent_headings() {
    let h = build_harness().await;
    seed_context(&h.manager, "qa", "rules", "Always cite sources").await;
    seed_context(&h.manager, "agent", "tool_rules", "Run uv sync first").await;

    let options = RecallOptions {
        context_profile: Some("agent".to_string()),
        ..Default::default()
    };
    let items = recall_scoped(&h, vec![RecallScope::SessionContext], Some(options)).await;

    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0].content["context_profile"], "agent");
    let block = items[0].content["content"].as_str().expect("block");
    assert!(block.contains("### Tool rules\n"), "{block}");
    assert!(block.contains("Run uv sync first"), "{block}");
    assert!(!block.contains("### Rules"), "{block}");
}

/// No stored guidance, or no session: the lane contributes nothing.
#[tokio::test]
async fn test_scope_session_context_without_entries_or_session_is_empty() {
    let h = build_harness().await;
    assert!(
        recall_scoped(&h, vec![RecallScope::SessionContext], None)
            .await
            .is_empty()
    );

    seed_context(&h.manager, "qa", "rules", "Always cite sources").await;
    let result = recall(
        "anything",
        None,
        None,
        None,
        10,
        false,
        None,
        Some(USER_ID),
        &h.orchestrator,
        Some(&*h.store),
        Some(&h.manager),
        Some(vec![RecallScope::SessionContext]),
        None,
    )
    .await
    .expect("recall ok");
    assert!(result.items.is_empty());
}

/// `all` expands to `[graph, session, trace, session_context]`
/// (`entries.py:242`), so the context entry comes last.
#[tokio::test]
async fn test_scope_all_includes_session_context_last() {
    let h = build_harness().await;
    seed_context(&h.manager, "qa", "rules", "Always cite sources").await;

    let scope = normalize_scope(Some(ScopeInput::from("all"))).expect("normalize");
    let items = recall_scoped(&h, scope, None).await;

    assert_eq!(
        items.first().map(|i| i.source),
        Some(RecallSource::Graph),
        "{items:?}"
    );
    assert_eq!(
        items.last().map(|i| i.source),
        Some(RecallSource::SessionContext),
        "{items:?}"
    );
}
