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

use std::collections::HashSet;

use cognee_search::observability::{
    COGNEE_RECALL_SCOPE, COGNEE_RECALL_SOURCE, COGNEE_RESULT_COUNT, COGNEE_SEARCH_QUERY,
    COGNEE_SEARCH_TYPE, COGNEE_SESSION_ENTRY_COUNT, COGNEE_SESSION_ID,
};
use cognee_search::{
    SearchOrchestrator, SearchRequest, SearchResponse, SearchType, record_override, route_query,
};
use cognee_session::{SessionManager, SessionStore};
use serde::{Deserialize, Serialize};
use tracing::{debug, field, info};

use super::error::ApiError;

/// Source tag for recall results. Mirrors the discriminator strings emitted
/// by Python's `_search_session`, `_search_trace`, `_fetch_graph_context`,
/// and `_run_graph` helpers in `cognee/api/v1/recall/recall.py`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallSource {
    Session,
    Graph,
    Trace,
    GraphContext,
}

impl RecallSource {
    /// Lowercase wire name (matches Python's `_source` literal).
    fn as_str(&self) -> &'static str {
        match self {
            RecallSource::Session => "session",
            RecallSource::Graph => "graph",
            RecallSource::Trace => "trace",
            RecallSource::GraphContext => "graph_context",
        }
    }
}

/// Scope selector. Mirrors Python's
/// `RecallScope = Literal["auto", "graph", "session", "trace", "graph_context", "all"]`
/// (`cognee/memory/entries.py:75`). `Auto` is the sentinel returned by
/// `normalize_scope(None)` and is resolved into concrete sources inside
/// `recall()`. `All` never appears in a normalized list — it expands to the
/// four concrete sources during normalization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallScope {
    Auto,
    Graph,
    Session,
    Trace,
    GraphContext,
}

impl RecallScope {
    /// The four concrete sources, in the canonical order Python uses when
    /// expanding `"all"` (`entries.py:106`).
    pub const ALL: &'static [Self] = &[Self::Graph, Self::Session, Self::Trace, Self::GraphContext];

    #[cfg_attr(not(test), allow(dead_code))]
    fn as_wire(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Graph => "graph",
            Self::Session => "session",
            Self::Trace => "trace",
            Self::GraphContext => "graph_context",
        }
    }

    fn from_wire(s: &str) -> Option<Self> {
        match s {
            "auto" => Some(Self::Auto),
            "graph" => Some(Self::Graph),
            "session" => Some(Self::Session),
            "trace" => Some(Self::Trace),
            "graph_context" => Some(Self::GraphContext),
            _ => None,
        }
    }

    /// Map an already-resolved (non-Auto) `RecallScope` to its `RecallSource`.
    fn as_source(&self) -> Option<RecallSource> {
        match self {
            RecallScope::Auto => None,
            RecallScope::Graph => Some(RecallSource::Graph),
            RecallScope::Session => Some(RecallSource::Session),
            RecallScope::Trace => Some(RecallSource::Trace),
            RecallScope::GraphContext => Some(RecallSource::GraphContext),
        }
    }
}

/// Untyped input accepted by [`normalize_scope`] -- mirrors Python's
/// `Optional[Union[str, list[str]]]` (`entries.py:81`). HTTP/CLI layers can
/// build a `ScopeInput` directly from JSON; callers with a strongly-typed
/// `Vec<RecallScope>` can pass it via `recall()`'s `scope` parameter without
/// going through this helper.
#[derive(Debug, Clone)]
pub enum ScopeInput {
    Single(String),
    Many(Vec<String>),
}

impl From<&str> for ScopeInput {
    fn from(s: &str) -> Self {
        ScopeInput::Single(s.to_string())
    }
}

impl From<String> for ScopeInput {
    fn from(s: String) -> Self {
        ScopeInput::Single(s)
    }
}

impl From<Vec<String>> for ScopeInput {
    fn from(v: Vec<String>) -> Self {
        ScopeInput::Many(v)
    }
}

/// Normalize the recall ``scope`` parameter to a concrete source list,
/// mirroring Python's `normalize_scope` at `cognee/memory/entries.py:81-115`.
///
/// - `None` -> `[Auto]` (Python: `["auto"]`).
/// - `"all"` -> `[Graph, Session, Trace, GraphContext]` (`entries.py:105-106`).
/// - Single string -> singleton list.
/// - List of strings -> order-preserving dedup (`entries.py:108-115`).
/// - Unknown values -> `Err(ApiError::InvalidArgument(...))` with the
///   Python-parity error message (`entries.py:99-103`).
pub fn normalize_scope(input: Option<ScopeInput>) -> Result<Vec<RecallScope>, ApiError> {
    let raw: Vec<String> = match input {
        None => return Ok(vec![RecallScope::Auto]),
        Some(ScopeInput::Single(s)) => vec![s],
        Some(ScopeInput::Many(v)) => v,
    };

    if raw.is_empty() {
        // Python passes `[]` through to the unknown-check (which finds none)
        // and the dedup loop (which yields `[]`). An empty vector is a valid
        // (if useless) result. Match that exactly: empty input -> empty list.
        return Ok(vec![]);
    }

    // Collect unknowns *in encounter order* — matches Python's
    // `[s for s in scopes if s not in _VALID_SCOPES]` (`entries.py:99`).
    // `_VALID_SCOPES` includes `"all"` even though it doesn't map to a
    // `RecallScope` variant — it's the expansion sentinel.
    fn is_valid_wire(s: &str) -> bool {
        s == "all" || RecallScope::from_wire(s).is_some()
    }
    let unknown: Vec<&str> = raw
        .iter()
        .filter(|s| !is_valid_wire(s))
        .map(String::as_str)
        .collect();
    if !unknown.is_empty() {
        // Python sorts `_VALID_SCOPES` for the error message
        // (`entries.py:102` -- `sorted(_VALID_SCOPES)`).
        let valid_sorted = ["all", "auto", "graph", "graph_context", "session", "trace"];
        // Match Python's `f"Unknown recall scope(s): {unknown}. Valid values: {sorted(_VALID_SCOPES)}"`
        // formatting: Rust's debug-format for a `Vec<&str>` produces the same
        // bracketed quoted-string list Python's `repr(list)` does.
        return Err(ApiError::InvalidArgument(format!(
            "Unknown recall scope(s): {unknown:?}. Valid values: {valid_sorted:?}"
        )));
    }

    // `"all"` short-circuits to the canonical four-source list, in fixed order
    // (`entries.py:105-106`).
    if raw.iter().any(|s| s == "all") {
        return Ok(RecallScope::ALL.to_vec());
    }

    // Order-preserving dedup (`entries.py:108-115`).
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out: Vec<RecallScope> = Vec::with_capacity(raw.len());
    for s in &raw {
        if seen.insert(s.as_str())
            && let Some(scope) = RecallScope::from_wire(s)
        {
            out.push(scope);
        }
    }
    Ok(out)
}

/// A single recall result item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallItem {
    /// The source of this result.
    pub source: RecallSource,
    /// Content (question, answer, search result text, trace fields, or graph_context snapshot).
    pub content: serde_json::Value,
    /// Relevance score (keyword overlap for session/trace, similarity for graph,
    /// constant `1.0` for graph_context which is not query-matched).
    pub score: f64,
}

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
        { COGNEE_SEARCH_TYPE } = field::Empty,
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
                search_session(query_text, session_id, user_id, top_k, session_store).await?
            }
            RecallScope::Trace => {
                search_trace(query_text, session_id, user_id, top_k, session_manager).await?
            }
            RecallScope::GraphContext => {
                fetch_graph_context(session_id, user_id, session_manager).await?
            }
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
                .await?;
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

    Ok(RecallResult {
        items: merged,
        search_type_used: graph_search_type,
        auto_routed: graph_auto_routed,
        search_response: graph_response,
    })
}

/// Mirrors Python `_search_session` (`recall.py:146-208`).
///
/// Returns empty when `session_id` is missing or the backend isn't wired in
/// (Python `recall.py:170-171`).
async fn search_session(
    query_text: &str,
    session_id: Option<&str>,
    user_id: Option<&str>,
    top_k: usize,
    store: Option<&dyn SessionStore>,
) -> Result<Vec<RecallItem>, ApiError> {
    let (Some(sid), Some(store)) = (session_id, store) else {
        // Python `_search_session`: missing session_id => caller resolves
        // empty per `_run_session` (`recall.py:431-432`); missing backend
        // => `is_available` short-circuit (`recall.py:170-171`).
        return Ok(vec![]);
    };

    let query_tokens = tokenize(query_text);
    if query_tokens.is_empty() {
        return Ok(vec![]);
    }

    let entries = store.get_all_qa_entries(sid, user_id).await?;
    if entries.is_empty() {
        return Ok(vec![]);
    }

    let mut scored: Vec<(usize, usize)> = entries
        .iter()
        .enumerate()
        .map(|(idx, entry)| {
            // Python recall.py:191-194 — concat question + context + answer.
            let entry_text = format!(
                "{} {} {}",
                entry.question,
                entry.context.as_deref().unwrap_or(""),
                entry.answer,
            );
            let entry_tokens = tokenize(&entry_text);
            let overlap = query_tokens.intersection(&entry_tokens).count();
            (idx, overlap)
        })
        .filter(|(_, overlap)| *overlap > 0)
        .collect();

    scored.sort_by_key(|s| std::cmp::Reverse(s.1));
    scored.truncate(top_k);

    Ok(scored
        .into_iter()
        .map(|(idx, overlap)| {
            let entry = &entries[idx];
            RecallItem {
                source: RecallSource::Session,
                content: serde_json::json!({
                    "question": entry.question,
                    "answer": entry.answer,
                    "context": entry.context,
                    "session_id": entry.session_id,
                    "created_at": entry.created_at.to_rfc3339(),
                }),
                score: overlap as f64,
            }
        })
        .collect())
}

/// Mirrors Python `_search_trace` (`recall.py:211-286`).
///
/// Tokenizes `origin_function`, `status`, `memory_query`, `memory_context`,
/// `session_feedback`, `error_message` plus JSON-serialized `method_params`
/// and `method_return_value`. Ranks by token-set intersection.
async fn search_trace(
    query_text: &str,
    session_id: Option<&str>,
    user_id: Option<&str>,
    top_k: usize,
    sm: Option<&SessionManager>,
) -> Result<Vec<RecallItem>, ApiError> {
    let (Some(sid), Some(sm)) = (session_id, sm) else {
        return Ok(vec![]);
    };
    // Python recall.py:227-228: caller_user_id falsy -> empty. Rust requires
    // a user_id string for `get_agent_trace_session`, so empty/None -> empty.
    let Some(uid) = user_id else {
        return Ok(vec![]);
    };
    if uid.is_empty() {
        return Ok(vec![]);
    }

    let query_tokens = tokenize(query_text);
    if query_tokens.is_empty() {
        return Ok(vec![]);
    }

    let entries = sm.get_agent_trace_session(uid, Some(sid), None).await?;
    if entries.is_empty() {
        return Ok(vec![]);
    }

    let mut scored: Vec<(usize, usize)> = entries
        .iter()
        .enumerate()
        .map(|(idx, e)| {
            // Python recall.py:252-271 -- six string fields plus two
            // JSON-serialized fields.
            let mut parts: Vec<String> = vec![
                e.origin_function.clone(),
                e.status.clone(),
                e.memory_query.clone(),
                e.memory_context.clone(),
                e.session_feedback.clone(),
                e.error_message.clone(),
            ];
            // method_params is non-Option in Rust (default {}). Python
            // skips when `is None`; we always include the JSON serialization
            // since `{}` -> "{}" which contributes no tokens >=2 chars.
            match serde_json::to_string(&e.method_params) {
                Ok(s) => parts.push(s),
                Err(_) => parts.push(format!("{:?}", e.method_params)),
            }
            if let Some(ref mrv) = e.method_return_value {
                match serde_json::to_string(mrv) {
                    Ok(s) => parts.push(s),
                    Err(_) => parts.push(format!("{:?}", mrv)),
                }
            }

            let joined = parts.join(" ");
            let entry_tokens = tokenize(&joined);
            let overlap = query_tokens.intersection(&entry_tokens).count();
            (idx, overlap)
        })
        .filter(|(_, overlap)| *overlap > 0)
        .collect();

    scored.sort_by_key(|s| std::cmp::Reverse(s.1));
    scored.truncate(top_k);

    Ok(scored
        .into_iter()
        .map(|(idx, overlap)| {
            let e = &entries[idx];
            RecallItem {
                source: RecallSource::Trace,
                content: serde_json::json!({
                    "trace_id": e.trace_id,
                    "origin_function": e.origin_function,
                    "status": e.status,
                    "memory_query": e.memory_query,
                    "memory_context": e.memory_context,
                    "method_params": e.method_params,
                    "method_return_value": e.method_return_value,
                    "error_message": e.error_message,
                    "session_feedback": e.session_feedback,
                }),
                score: overlap as f64,
            }
        })
        .collect())
}

/// Mirrors Python `_fetch_graph_context` (`recall.py:289-314`). Reads the
/// pre-computed snapshot via `SessionManager::get_graph_context` -- not a
/// graph-DB walk.
async fn fetch_graph_context(
    session_id: Option<&str>,
    user_id: Option<&str>,
    sm: Option<&SessionManager>,
) -> Result<Vec<RecallItem>, ApiError> {
    let (Some(_sid), Some(sm)) = (session_id, sm) else {
        return Ok(vec![]);
    };
    let snapshot_opt = sm.get_graph_context(session_id, user_id).await?;
    match snapshot_opt {
        Some(snapshot) if !snapshot.is_empty() => Ok(vec![RecallItem {
            source: RecallSource::GraphContext,
            content: serde_json::Value::String(snapshot),
            score: 1.0,
        }]),
        _ => Ok(vec![]),
    }
}

/// Lifted from the original recall body — runs the graph search via the
/// orchestrator. Mirrors Python's inline `_run_graph` closure
/// (`recall.py:455-493`). Returns `(items, search_type_used, auto_routed,
/// raw_response)`.
#[allow(clippy::too_many_arguments)]
async fn run_graph(
    query_text: &str,
    query_type: Option<SearchType>,
    datasets: Option<Vec<String>>,
    top_k: usize,
    auto_route: bool,
    session_id: Option<&str>,
    search_orchestrator: &SearchOrchestrator,
    span: &tracing::Span,
) -> Result<(Vec<RecallItem>, SearchType, bool, SearchResponse), ApiError> {
    // Python recall.py:458-472: still run the router on explicit query_type
    // + auto_route=true so the override gets recorded.
    let (search_type, auto_routed) = match (query_type, auto_route) {
        (Some(qt), true) => {
            let routed = route_query(query_text);
            record_override(routed.search_type, qt);
            (qt, false)
        }
        (Some(qt), false) => (qt, false),
        (None, true) => {
            let routed = route_query(query_text);
            debug!(
                search_type = ?routed.search_type,
                confidence = routed.confidence,
                "recall: auto-routed query"
            );
            (routed.search_type, true)
        }
        (None, false) => (SearchType::GraphCompletion, false),
    };

    span.record(COGNEE_SEARCH_TYPE, format!("{search_type:?}").as_str());

    let request = SearchRequest {
        query_text: query_text.to_string(),
        search_type,
        top_k: Some(top_k),
        datasets,
        dataset_ids: None,
        system_prompt: None,
        system_prompt_path: None,
        only_context: None,
        use_combined_context: None,
        session_id: session_id.map(|s| s.to_string()),
        node_type: None,
        node_name: None,
        wide_search_top_k: None,
        triplet_distance_penalty: None,
        save_interaction: None,
        user_id: None,
        verbose: None,
        feedback_influence: None,
        retriever_specific_config: None,
        response_schema: None,
        custom_search_type: None,
        auto_feedback_detection: None,
        node_name_filter_operator: None,
        neighborhood_depth: None,
        neighborhood_seed_top_k: None,
    };

    let response = search_orchestrator
        .search(&request)
        .await
        .map_err(|e| ApiError::Search(e.to_string()))?;

    let items: Vec<RecallItem> = match &response.result {
        cognee_search::SearchOutput::Items(search_items) => search_items
            .iter()
            .enumerate()
            .map(|(i, item)| RecallItem {
                source: RecallSource::Graph,
                content: serde_json::to_value(item)
                    .unwrap_or_else(|_| serde_json::Value::String(format!("{:?}", item))),
                score: 1.0 - (i as f64 * 0.01),
            })
            .collect(),
        cognee_search::SearchOutput::Text(text) => vec![RecallItem {
            source: RecallSource::Graph,
            content: serde_json::Value::String(text.clone()),
            score: 1.0,
        }],
        cognee_search::SearchOutput::Texts(texts) => texts
            .iter()
            .enumerate()
            .map(|(i, t)| RecallItem {
                source: RecallSource::Graph,
                content: serde_json::Value::String(t.clone()),
                score: 1.0 - (i as f64 * 0.01),
            })
            .collect(),
        other => vec![RecallItem {
            source: RecallSource::Graph,
            content: serde_json::to_value(other)
                .unwrap_or_else(|_| serde_json::Value::String(format!("{:?}", other))),
            score: 1.0,
        }],
    };

    Ok((items, search_type, auto_routed, response))
}

/// Tokenize text into lowercase words of length >= 2.
fn tokenize(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 2)
        .map(|w| w.to_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_splits_and_lowercases() {
        let tokens = tokenize("Hello, World! How are you?");
        assert!(tokens.contains("hello"));
        assert!(tokens.contains("world"));
        assert!(tokens.contains("how"));
        assert!(tokens.contains("are"));
        assert!(tokens.contains("you"));
        // Single-char tokens should be excluded.
        assert!(!tokens.contains("a"));
    }

    #[test]
    fn tokenize_empty_string() {
        let tokens = tokenize("");
        assert!(tokens.is_empty());
    }

    #[test]
    fn recall_source_serializes_correctly() {
        let s = serde_json::to_string(&RecallSource::Session).expect("serialize");
        assert_eq!(s, "\"session\"");
        let g = serde_json::to_string(&RecallSource::Graph).expect("serialize");
        assert_eq!(g, "\"graph\"");
    }

    #[test]
    fn recall_source_trace_serializes_correctly() {
        let t = serde_json::to_string(&RecallSource::Trace).expect("serialize");
        assert_eq!(t, "\"trace\"");
    }

    #[test]
    fn recall_source_graph_context_serializes_correctly() {
        let g = serde_json::to_string(&RecallSource::GraphContext).expect("serialize");
        assert_eq!(g, "\"graph_context\"");
    }

    #[test]
    fn test_normalize_scope_none_returns_auto() {
        let out = normalize_scope(None).expect("normalize");
        assert_eq!(out, vec![RecallScope::Auto]);
    }

    #[test]
    fn test_normalize_scope_string_passes_through() {
        for (s, expected) in [
            ("graph", RecallScope::Graph),
            ("session", RecallScope::Session),
            ("trace", RecallScope::Trace),
            ("graph_context", RecallScope::GraphContext),
            ("auto", RecallScope::Auto),
        ] {
            let out = normalize_scope(Some(ScopeInput::from(s))).expect("normalize");
            assert_eq!(out, vec![expected], "scope={s}");
        }
    }

    #[test]
    fn test_normalize_scope_list_dedupes() {
        let out = normalize_scope(Some(ScopeInput::Many(vec![
            "session".to_string(),
            "graph".to_string(),
            "session".to_string(),
            "trace".to_string(),
            "graph".to_string(),
        ])))
        .expect("normalize");
        // Order preserved, duplicates dropped.
        assert_eq!(
            out,
            vec![RecallScope::Session, RecallScope::Graph, RecallScope::Trace,]
        );
    }

    #[test]
    fn test_normalize_scope_all_expands() {
        let out = normalize_scope(Some(ScopeInput::from("all"))).expect("normalize");
        assert_eq!(
            out,
            vec![
                RecallScope::Graph,
                RecallScope::Session,
                RecallScope::Trace,
                RecallScope::GraphContext,
            ]
        );
        // `"all"` mixed in with other values still expands to canonical four.
        let out2 = normalize_scope(Some(ScopeInput::Many(vec![
            "session".to_string(),
            "all".to_string(),
        ])))
        .expect("normalize");
        assert_eq!(
            out2,
            vec![
                RecallScope::Graph,
                RecallScope::Session,
                RecallScope::Trace,
                RecallScope::GraphContext,
            ]
        );
    }

    #[test]
    fn test_normalize_scope_unknown_returns_error() {
        let err = normalize_scope(Some(ScopeInput::from("nonsense"))).expect_err("should error");
        match err {
            ApiError::InvalidArgument(_) => {}
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[test]
    fn test_normalize_scope_error_message_matches_python() {
        let err = normalize_scope(Some(ScopeInput::from("foo"))).expect_err("should error");
        let msg = match err {
            ApiError::InvalidArgument(m) => m,
            other => panic!("expected InvalidArgument, got {other:?}"),
        };
        // Python: f'Unknown recall scope(s): {unknown}. Valid values: {sorted(_VALID_SCOPES)}'
        // -> `Unknown recall scope(s): ['foo']. Valid values: ['all', 'auto', 'graph', 'graph_context', 'session', 'trace']`
        // Rust's debug format uses double quotes:
        let expected = "Unknown recall scope(s): [\"foo\"]. Valid values: [\"all\", \"auto\", \"graph\", \"graph_context\", \"session\", \"trace\"]";
        assert_eq!(msg, expected);
    }

    #[test]
    fn recall_scope_all_constant_matches_canonical_order() {
        assert_eq!(
            RecallScope::ALL,
            &[
                RecallScope::Graph,
                RecallScope::Session,
                RecallScope::Trace,
                RecallScope::GraphContext,
            ]
        );
    }

    #[test]
    fn recall_scope_serde_round_trip() {
        for (s, expected) in [
            ("\"auto\"", RecallScope::Auto),
            ("\"graph\"", RecallScope::Graph),
            ("\"session\"", RecallScope::Session),
            ("\"trace\"", RecallScope::Trace),
            ("\"graph_context\"", RecallScope::GraphContext),
        ] {
            let parsed: RecallScope = serde_json::from_str(s).expect("deserialize");
            assert_eq!(parsed, expected);
            assert_eq!(serde_json::to_string(&expected).expect("serialize"), s);
        }
    }

    #[test]
    fn recall_scope_as_wire_matches_serde() {
        assert_eq!(RecallScope::Auto.as_wire(), "auto");
        assert_eq!(RecallScope::Graph.as_wire(), "graph");
        assert_eq!(RecallScope::Session.as_wire(), "session");
        assert_eq!(RecallScope::Trace.as_wire(), "trace");
        assert_eq!(RecallScope::GraphContext.as_wire(), "graph_context");
    }
}
