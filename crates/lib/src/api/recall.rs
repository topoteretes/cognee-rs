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
//! `ScopeInput`, `RecallItem`, `normalize_scope`) and the source helpers
//! (`search_session`, `search_trace`, `search_session_context`, `run_graph`)
//! live
//! in `cognee_search::recall_scope`. They are re-exported here so existing
//! call sites (`api::mod.rs`, `lib.rs::prelude`, integration tests) compile
//! unchanged.

use cognee_search::observability::{
    COGNEE_RECALL_SCOPE, COGNEE_RECALL_SOURCE, COGNEE_RESULT_COUNT, COGNEE_SEARCH_QUERY,
    COGNEE_SESSION_ENTRY_COUNT, COGNEE_SESSION_ID,
};
use cognee_search::recall_scope::{
    ResolvedSources, has_dataset_filter, plan_graph, resolve_sources, run_graph, search_session,
    search_session_context, search_trace,
};
use cognee_search::{SearchOrchestrator, SearchResponse, SearchType};
use cognee_session::{SessionManager, SessionStore};
use tracing::{field, info};

use super::error::ApiError;

pub use cognee_search::recall_scope::{
    RecallItem, RecallOptions, RecallScope, RecallSource, ScopeInput, ToolsTrigger,
    normalize_scope, resolve_session_cache_user_id,
};

/// Full recall result.
#[derive(Debug, Clone)]
pub struct RecallResult {
    /// Source-tagged results -- order matches the iteration order of the
    /// resolved `sources` list (Python `recall.py:1243-1257`).
    pub items: Vec<RecallItem>,
    /// The search type that was used (if graph search ran), or would have
    /// been when the warm-up guard skipped it.
    pub search_type_used: Option<SearchType>,
    /// Whether auto-routing was applied (only relevant when graph ran).
    pub auto_routed: bool,
    /// The raw graph search response (if graph search ran); empty when the
    /// warm-up guard skipped it.
    pub search_response: Option<SearchResponse>,
}

/// Smart search with optional session routing, auto query-type selection, and
/// configurable source fan-out. Byte-for-byte parity with Python
/// `cognee.api.v1.recall.recall()` (`cognee/api/v1/recall/recall.py:434`).
///
/// # Behavior
/// 1. `scope` arrives normalized (the callers run [`normalize_scope`] on the
///    wire form, `recall.py:574`).
/// 2. [`resolve_sources`] resolves it into the sources to run
///    (`recall.py:575-594`):
///    - `[Auto]` with `session_id` set + no datasets + no query_type ->
///      `[Session, Graph]` with `auto_fallthrough=true` (graph
///      short-circuited on session hit).
///    - `[Auto]` with `session_id` set, a dataset filter and no query_type
///      -> `[Session, Graph]`, both contribute.
///    - `[Auto]` otherwise (no `session_id`, or a query_type) -> `[Graph]`.
///    - An explicit `session_first` -> `[Session, Graph, ..rest]` with
///      `auto_fallthrough=true`.
///
///    A `tools` or `code` scope fails here with [`ApiError::InvalidArgument`]
///    before any source runs: Rust has no lane for them yet (SDK-1537).
/// 3. Iterate sources in resolved order (`recall.py:1243-1257`); each source
///    appends to a flat merged list of [`RecallItem`]s. The session, trace
///    and session_context sources read the session cache under
///    [`RecallOptions::session_cache_user_id`] when set (the session's owner,
///    from [`resolve_session_cache_user_id`]), else under `user_id`.
/// 4. Telemetry attrs: `COGNEE_RECALL_SCOPE`, `COGNEE_RECALL_SOURCE`,
///    `COGNEE_RESULT_COUNT`, `COGNEE_SESSION_ENTRY_COUNT` (`recall.py:515-522`).
#[allow(clippy::too_many_arguments)]
pub async fn recall(
    query_text: &str,
    query_type: Option<SearchType>,
    datasets: Option<Vec<String>>,
    dataset_ids: Option<Vec<uuid::Uuid>>,
    top_k: usize,
    auto_route: bool,
    session_id: Option<&str>,
    user_id: Option<&str>,
    search_orchestrator: &SearchOrchestrator,
    session_store: Option<&dyn SessionStore>,
    session_manager: Option<&SessionManager>,
    scope: Option<Vec<RecallScope>>,
    options: Option<RecallOptions>,
) -> Result<RecallResult, ApiError> {
    // -- Resolve scope to a concrete source list (Python recall.py:574-594). --
    // `None` means "auto"; an explicit empty list runs nothing and returns
    // `[]`, as Python's `normalize_scope([])` does. The type system already
    // constrained the values to `RecallScope`; unknown strings are rejected
    // by `normalize_scope` at the HTTP/CLI/binding boundary. Python's
    // `has_dataset_scope = bool(dataset_ids) or bool(datasets)`: either
    // filter counts, and an empty list does not.
    let ResolvedSources {
        sources,
        auto_fallthrough,
    } = resolve_sources(
        scope.unwrap_or_else(|| vec![RecallScope::Auto]),
        session_id,
        has_dataset_filter(datasets.as_deref(), dataset_ids.as_deref()),
        query_type,
    )
    .map_err(|e| ApiError::InvalidArgument(e.to_string()))?;

    // Comma-joined source names, `auto` included, mirroring Python's
    // `span_scope` (`recall.py:388`).
    let span_scope: String = sources
        .iter()
        .map(RecallScope::as_wire)
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
        { cognee_search::observability::COGNEE_RECALL_ROUTE_RULE } = field::Empty,
        { cognee_search::observability::COGNEE_RECALL_WARMUP_SHORTCIRCUIT } = field::Empty,
        { cognee_search::observability::COGNEE_RECALL_WARMUP_STATE } = field::Empty,
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
    // The session-cache lanes read under the session's owner when the caller
    // resolved one (Python `recall.py:272`, `:330`, `:401`); graph runs as
    // the caller.
    let session_user_id = RecallOptions::session_cache_user_id_or(options.as_ref(), user_id);

    // -- Iterate sources in resolved order (Python recall.py:1243-1257). --
    for src in &sources {
        // Session-first short-circuit: a session hit skips the graph runner,
        // and every source after it (Python recall.py:1248-1249).
        if auto_fallthrough && *src == RecallScope::Graph && !merged.is_empty() {
            break;
        }

        let part: Vec<RecallItem> = match src {
            // `Auto` next to real sources is skipped (Python has no runner
            // for it). `resolve_sources` has already turned `SessionFirst`
            // into `[Session, Graph]` and refused `Tools` / `Code`.
            RecallScope::Auto
            | RecallScope::SessionFirst
            | RecallScope::Tools
            | RecallScope::Code => continue,
            RecallScope::SessionContext => search_session_context(
                query_text,
                session_id,
                session_user_id,
                RecallOptions::context_profile_or_default(options.as_ref()),
                session_manager,
            )
            .await
            .map_err(|e| ApiError::Search(e.to_string()))?,
            RecallScope::Session => search_session(
                query_text,
                session_id,
                session_user_id,
                top_k,
                session_store,
            )
            .await
            .map_err(|e| ApiError::Search(e.to_string()))?,
            RecallScope::Trace => search_trace(
                query_text,
                session_id,
                session_user_id,
                top_k,
                session_manager,
            )
            .await
            .map_err(|e| ApiError::Search(e.to_string()))?,
            RecallScope::Graph => {
                let search_err =
                    |e: cognee_search::types::SearchError| ApiError::Search(e.to_string());
                let plan = plan_graph(
                    query_text,
                    query_type,
                    auto_route,
                    datasets.as_deref(),
                    dataset_ids.as_deref(),
                    user_id,
                    search_orchestrator,
                    &span,
                )
                .map_err(search_err)?;
                let note = plan
                    .warmup_note(
                        search_orchestrator,
                        datasets.as_deref(),
                        dataset_ids.as_deref(),
                        &span,
                        options.as_ref(),
                    )
                    .await
                    .map_err(search_err)?;
                // A graph that is not built is not searched: it answers the
                // note as the one source, and nothing next to others.
                let (items, used_type, was_auto, response) = match note {
                    Some(note) => (
                        if sources.as_slice() == [RecallScope::Graph] {
                            vec![note]
                        } else {
                            Vec::new()
                        },
                        plan.search_type(),
                        plan.auto_routed(),
                        plan.empty_response(),
                    ),
                    None => run_graph(
                        &plan,
                        query_text,
                        datasets.clone(),
                        dataset_ids.clone(),
                        top_k,
                        session_id,
                        search_orchestrator,
                        &span,
                        options.as_ref(),
                    )
                    .await
                    .map_err(search_err)?,
                };
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
    span.record(COGNEE_RECALL_SOURCE, source_label(&sources));
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
                "dataset_ids": dataset_ids,
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

/// The recall's `cognee.recall.source`: a single source names itself; any
/// other scope, `[auto, graph]` included, is "multi".
fn source_label(sources: &[RecallScope]) -> &'static str {
    match sources {
        [only] => only.as_wire(),
        _ => "multi",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_label_names_a_single_source_only() {
        use RecallScope::{Auto, Graph, Session};
        assert_eq!(source_label(&[Graph]), "graph");
        assert_eq!(source_label(&[Auto, Graph]), "multi");
        assert_eq!(source_label(&[Session, Graph]), "multi");
    }

    #[test]
    fn recall_options_fields_are_set_correctly() {
        let opts = RecallOptions {
            triplet_distance_penalty: Some(6.5),
            node_name: Some(vec!["Alice".to_string()]),
            ..Default::default()
        };
        assert_eq!(opts.triplet_distance_penalty, Some(6.5));
        assert_eq!(
            opts.node_name.as_deref(),
            Some(["Alice".to_string()].as_slice())
        );
        // Un-set fields default to None.
        assert!(opts.system_prompt.is_none());
        assert!(opts.wide_search_top_k.is_none());
        assert!(opts.feedback_influence.is_none());
        assert!(opts.neighborhood_depth.is_none());
        assert!(opts.neighborhood_seed_top_k.is_none());
    }
}
