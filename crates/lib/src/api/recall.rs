//! Smart search with session routing -- `recall()`.
//!
//! Wraps the standard search pipeline with two additional capabilities:
//! 1. Session-first routing: check session Q&A entries by keyword overlap
//!    before falling through to graph search.
//! 2. Auto query-type selection: use [`route_query()`] to pick the best
//!    [`SearchType`] based on query text patterns.
//!
//! Equivalent to Python's `cognee.api.v1.recall.recall()` and
//! `cognee.memory.entries.normalize_scope()`.
//!
//! As of LIB-08 the scope-routing primitives (`RecallScope`, `RecallSource`,
//! `ScopeInput`, `RecallItem`, `normalize_scope`) and the four source helpers
//! (`search_session`, `search_trace`, `fetch_graph_context`, `run_graph`) live
//! in `cognee_search::recall_scope`. They are re-exported here so existing
//! call sites (`api::mod.rs`, `lib.rs::prelude`, integration tests) compile
//! unchanged.

use cognee_search::observability::{
    COGNEE_RECALL_SCOPE, COGNEE_RECALL_SOURCE, COGNEE_RESULT_COUNT, COGNEE_SEARCH_QUERY,
    COGNEE_SESSION_ENTRY_COUNT, COGNEE_SESSION_ID,
};
use cognee_search::recall_scope::{fetch_graph_context, run_graph, search_session, search_trace};
use cognee_search::{SearchOrchestrator, SearchResponse, SearchType};
use cognee_session::{SessionManager, SessionStore};
use tracing::{field, info};

use super::error::ApiError;

pub use cognee_search::recall_scope::{
    RecallItem, RecallScope, RecallSource, ScopeInput, normalize_scope,
};

/// Full recall result.
#[derive(Debug, Clone)]
pub struct RecallResult {
    /// Source-tagged results -- order matches the iteration order of the
    /// resolved `sources` list (Python `recall.py:503-513`).
    pub items: Vec<RecallItem>,
    /// The search type that was used (if graph search ran).
    pub search_type_used: Option<SearchType>,
    /// Whether auto-routing was applied (only relevant when graph ran).
    pub auto_routed: bool,
    /// The raw graph search response (if graph search ran).
    pub search_response: Option<SearchResponse>,
}

/// Smart search with optional session routing, auto query-type selection, and
/// configurable source fan-out. Byte-for-byte parity with Python
/// `cognee.api.v1.recall.recall()` (`cognee/api/v1/recall/recall.py:317-531`).
///
/// # Behavior
/// 1. Normalize `scope` via [`normalize_scope`] (`recall.py:373`).
/// 2. Resolve `[Auto]` per `(session_id, datasets, query_type)` triple
///    (`recall.py:374-386`):
///    - `session_id` set + no datasets + no query_type -> `[Session, Graph]`
///      with `auto_fallthrough=true` (graph short-circuited on session hit).
///    - `session_id` set otherwise -> `[Session, Graph]`, both contribute.
///    - No `session_id` -> `[Graph]`.
/// 3. Iterate sources in caller-supplied order (`recall.py:503`); each source
///    appends to a flat merged list of [`RecallItem`]s.
/// 4. Telemetry attrs: `COGNEE_RECALL_SCOPE`, `COGNEE_RECALL_SOURCE`,
///    `COGNEE_RESULT_COUNT`, `COGNEE_SESSION_ENTRY_COUNT` (`recall.py:515-522`).
#[allow(clippy::too_many_arguments)]
pub async fn recall(
    query_text: &str,
    query_type: Option<SearchType>,
    datasets: Option<Vec<String>>,
    top_k: usize,
    auto_route: bool,
    session_id: Option<&str>,
    user_id: Option<&str>,
    search_orchestrator: &SearchOrchestrator,
    session_store: Option<&dyn SessionStore>,
    session_manager: Option<&SessionManager>,
    scope: Option<Vec<RecallScope>>,
) -> Result<RecallResult, ApiError> {
    // -- Resolve scope to a concrete source list (Python recall.py:373-386). --
    let normalized: Vec<RecallScope> = match scope {
        // None means caller did not supply a scope at all -> "auto".
        None => vec![RecallScope::Auto],
        // Caller-supplied. We do NOT re-validate via normalize_scope here
        // because the type system already constrained values to RecallScope;
        // unknown strings are rejected at the HTTP/CLI boundary before they
        // reach this function.
        Some(v) if v.is_empty() => vec![RecallScope::Auto],
        Some(v) => v,
    };

    let auto_mode = normalized.as_slice() == [RecallScope::Auto];
    let (sources, auto_fallthrough): (Vec<RecallScope>, bool) = if auto_mode {
        // Python recall.py:374-386.
        match (session_id, datasets.as_ref(), query_type) {
            (Some(_), None, None) => (vec![RecallScope::Session, RecallScope::Graph], true),
            (Some(_), _, _) => (vec![RecallScope::Session, RecallScope::Graph], false),
            (None, _, _) => (vec![RecallScope::Graph], false),
        }
    } else {
        (normalized, false)
    };

    // Comma-joined source names, mirroring Python's `span_scope` (`recall.py:388`).
    let span_scope: String = sources
        .iter()
        .filter_map(|s| s.as_source().map(|src| src.as_str()))
        .collect::<Vec<_>>()
        .join(",");

    // Truncate query preview to 500 chars for PII control, mirroring Python's
    // `span.set_attribute(COGNEE_SEARCH_QUERY, query_text[:500])`.
    let query_preview: &str = {
        let mut end = query_text.len();
        if query_text.chars().count() > 500 {
            let mut idx = 0usize;
            for (count, (byte_idx, _)) in query_text.char_indices().enumerate() {
                if count == 500 {
                    idx = byte_idx;
                    break;
                }
            }
            if idx > 0 {
                end = idx;
            }
        }
        &query_text[..end]
    };

    let span = tracing::info_span!(
        "cognee.api.recall",
        { COGNEE_SEARCH_QUERY } = query_preview,
        { COGNEE_RECALL_SCOPE } = span_scope.as_str(),
        { COGNEE_SESSION_ID } = session_id.unwrap_or(""),
        "cognee.recall.top_k" = top_k,
        { cognee_search::observability::COGNEE_SEARCH_TYPE } = field::Empty,
        { COGNEE_RECALL_SOURCE } = field::Empty,
        { COGNEE_RESULT_COUNT } = field::Empty,
        { COGNEE_SESSION_ENTRY_COUNT } = field::Empty,
    );
    let _enter = span.enter();

    // Track the captured graph response (if any) so the result struct can
    // surface `search_type_used` / `auto_routed` / `search_response` to
    // callers that still rely on those fields.
    let mut merged: Vec<RecallItem> = Vec::new();
    let mut graph_search_type: Option<SearchType> = None;
    let mut graph_auto_routed = false;
    let mut graph_response: Option<SearchResponse> = None;
    let mut session_result_count: usize = 0;

    // -- Iterate sources in caller-supplied order (Python recall.py:503-513). --
    for src in &sources {
        // Auto-mode short-circuit: a session hit skips the graph runner.
        // (Python recall.py:508-509)
        if auto_fallthrough && *src == RecallScope::Graph && !merged.is_empty() {
            break;
        }

        let part: Vec<RecallItem> = match src {
            RecallScope::Auto => continue, // sentinel -- already resolved above
            RecallScope::Session => {
                search_session(query_text, session_id, user_id, top_k, session_store)
                    .await
                    .map_err(|e| ApiError::Search(e.to_string()))?
            }
            RecallScope::Trace => {
                search_trace(query_text, session_id, user_id, top_k, session_manager)
                    .await
                    .map_err(|e| ApiError::Search(e.to_string()))?
            }
            RecallScope::GraphContext => fetch_graph_context(session_id, user_id, session_manager)
                .await
                .map_err(|e| ApiError::Search(e.to_string()))?,
            RecallScope::Graph => {
                let (items, used_type, was_auto, response) = run_graph(
                    query_text,
                    query_type,
                    datasets.clone(),
                    top_k,
                    auto_route,
                    session_id,
                    search_orchestrator,
                    &span,
                )
                .await
                .map_err(|e| ApiError::Search(e.to_string()))?;
                graph_search_type = Some(used_type);
                graph_auto_routed = was_auto;
                graph_response = Some(response);
                items
            }
        };

        if *src == RecallScope::Session {
            session_result_count = part.len();
        }
        merged.extend(part);
    }

    // -- Telemetry (Python recall.py:515-522). --
    let source_label: &str = if sources.iter().filter(|s| s.as_source().is_some()).count() == 1 {
        sources
            .iter()
            .find_map(|s| s.as_source())
            .map(|s| s.as_str())
            .unwrap_or("graph")
    } else {
        "multi"
    };
    span.record(COGNEE_RECALL_SOURCE, source_label);
    span.record(COGNEE_RESULT_COUNT, merged.len());
    if session_result_count > 0 {
        span.record(COGNEE_SESSION_ENTRY_COUNT, session_result_count);
    }

    info!(
        results = merged.len(),
        sources = ?sources,
        session_id = session_id.unwrap_or("-"),
        "recall: completed"
    );

    // Mirrors Python `send_telemetry("cognee.recall", ...)` from
    // cognee/api/v1/recall/recall.py:402.
    #[cfg(feature = "telemetry")]
    {
        let search_type_label = graph_search_type
            .or(query_type)
            .map(|t| format!("{t:?}"))
            .unwrap_or_default();
        cognee_telemetry::send_telemetry(
            "cognee.recall",
            user_id.unwrap_or("sdk"),
            Some(serde_json::json!({
                "query_length": query_text.len(),
                "scope": span_scope,
                "auto_route": auto_route,
                "top_k": top_k,
                "search_type": search_type_label,
                "session_id": session_id,
                "datasets": datasets,
                "dataset_ids": serde_json::Value::Null,
            })),
        );
    }

    Ok(RecallResult {
        items: merged,
        search_type_used: graph_search_type,
        auto_routed: graph_auto_routed,
        search_response: graph_response,
    })
}
