//! Smart search with session routing -- `recall()`.
//!
//! Wraps the standard search pipeline with two additional capabilities:
//! 1. Session-first routing: check session Q&A entries by keyword overlap
//!    before falling through to graph search.
//! 2. Auto query-type selection: use [`route_query()`] to pick the best
//!    [`SearchType`] based on query text patterns.
//!
//! Equivalent to Python's `cognee.api.v1.recall.recall()`.

use std::collections::HashSet;

use cognee_search::observability::{
    COGNEE_RECALL_SCOPE, COGNEE_RECALL_SOURCE, COGNEE_RESULT_COUNT, COGNEE_SEARCH_QUERY,
    COGNEE_SEARCH_TYPE, COGNEE_SESSION_ENTRY_COUNT, COGNEE_SESSION_ID,
};
use cognee_search::{
    SearchOrchestrator, SearchRequest, SearchResponse, SearchType, record_override, route_query,
};
use cognee_session::SessionStore;
use serde::{Deserialize, Serialize};
use tracing::{debug, field, info};

use super::error::ApiError;

/// Source tag for recall results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallSource {
    Session,
    Graph,
}

/// A single recall result item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallItem {
    /// The source of this result.
    pub source: RecallSource,
    /// Content (question, answer, or search result text).
    pub content: serde_json::Value,
    /// Relevance score (keyword overlap for session, similarity for graph).
    pub score: f64,
}

/// Full recall result.
#[derive(Debug, Clone)]
pub struct RecallResult {
    /// Source-tagged results.
    pub items: Vec<RecallItem>,
    /// The search type that was used (if graph search ran).
    pub search_type_used: Option<SearchType>,
    /// Whether auto-routing was applied.
    pub auto_routed: bool,
    /// The raw graph search response (if graph search ran).
    pub search_response: Option<SearchResponse>,
}

/// Smart search with optional session routing and auto query-type selection.
///
/// # Behavior
/// 1. If `session_id` is provided and no explicit `query_type` or `datasets`:
///    search session Q&A entries by keyword overlap first.
/// 2. If session search yields results, return them tagged as `source: session`.
/// 3. If session search is empty (or not applicable), fall through to graph search.
/// 4. If `auto_route=true` and no explicit `query_type`: use [`route_query()`]
///    to pick the best search type.
/// 5. If `auto_route=false` and no `query_type`: default to `GraphCompletion`.
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
) -> Result<RecallResult, ApiError> {
    // -- Compute scope for observability (parity with Python recall.py:164) --
    let scope: &'static str = match (session_id, datasets.as_deref(), query_type) {
        (Some(_), None, None) => "session",
        (Some(_), Some(_), _) => "auto",
        _ => "graph",
    };

    // Truncate query preview to 500 chars for PII control, mirroring Python's
    // `span.set_attribute(COGNEE_SEARCH_QUERY, query_text[:500])`.
    let query_preview: &str = {
        // Compute the byte index of the 500th char boundary.
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

    // Open a `cognee.api.recall` span with Python-parity semantic
    // attributes. Fields that are not known yet are initialised as
    // `field::Empty` and recorded at each return site.
    let span = tracing::info_span!(
        "cognee.api.recall",
        { COGNEE_SEARCH_QUERY } = query_preview,
        { COGNEE_RECALL_SCOPE } = scope,
        { COGNEE_SESSION_ID } = session_id.unwrap_or(""),
        "cognee.recall.top_k" = top_k,
        { COGNEE_SEARCH_TYPE } = field::Empty,
        { COGNEE_RECALL_SOURCE } = field::Empty,
        { COGNEE_RESULT_COUNT } = field::Empty,
        { COGNEE_SESSION_ENTRY_COUNT } = field::Empty,
    );
    let _enter = span.enter();

    // -- Session-first routing --
    if let (Some(sid), Some(store)) = (session_id, session_store) {
        // Only try session search when caller has not explicitly set datasets or query_type.
        if query_type.is_none() && datasets.is_none() {
            let session_results =
                session_keyword_search(query_text, sid, user_id, top_k, store).await?;
            if !session_results.is_empty() {
                info!(
                    session_id = sid,
                    results = session_results.len(),
                    "recall: returning session-matched results"
                );
                span.record(COGNEE_RECALL_SOURCE, "session");
                span.record(COGNEE_RESULT_COUNT, session_results.len());
                span.record(COGNEE_SESSION_ENTRY_COUNT, session_results.len());
                return Ok(RecallResult {
                    items: session_results,
                    search_type_used: None,
                    auto_routed: false,
                    search_response: None,
                });
            }
            debug!(
                session_id = sid,
                "recall: no session matches; falling through to graph search"
            );
        }
    }

    // -- Determine search type (Python parity: still run the router on
    //    explicit query_type + auto_route=true so we can record the
    //    override; see recall.py:225-238) --
    let (search_type, auto_routed) = match (query_type, auto_route) {
        (Some(qt), true) => {
            let routed = route_query(query_text);
            record_override(routed.search_type, qt);
            (qt, false)
        }
        (Some(qt), false) => (qt, false),
        (None, true) => {
            let routed = route_query(query_text);
            info!(
                search_type = ?routed.search_type,
                confidence = routed.confidence,
                "recall: auto-routed query"
            );
            (routed.search_type, true)
        }
        (None, false) => (SearchType::GraphCompletion, false),
    };

    span.record(COGNEE_SEARCH_TYPE, format!("{search_type:?}").as_str());

    // -- Build and execute search request --
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

    // Convert search output to RecallItems tagged with source: graph.
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

    span.record(COGNEE_RECALL_SOURCE, "graph");
    span.record(COGNEE_RESULT_COUNT, items.len());

    Ok(RecallResult {
        items,
        search_type_used: Some(search_type),
        auto_routed,
        search_response: Some(response),
    })
}

/// Search session Q&A entries by keyword overlap.
///
/// Tokenizes the query into word-boundary tokens (min length 2, lowercased),
/// then scores each session entry by the number of overlapping tokens.
async fn session_keyword_search(
    query_text: &str,
    session_id: &str,
    user_id: Option<&str>,
    top_k: usize,
    store: &dyn SessionStore,
) -> Result<Vec<RecallItem>, ApiError> {
    let query_tokens = tokenize(query_text);
    if query_tokens.is_empty() {
        return Ok(vec![]);
    }

    let entries = store.get_all_qa_entries(session_id, user_id).await?;
    if entries.is_empty() {
        return Ok(vec![]);
    }

    let mut scored: Vec<(usize, usize)> = entries
        .iter()
        .enumerate()
        .map(|(idx, entry)| {
            let entry_text = format!(
                "{} {} {}",
                entry.question,
                entry.answer,
                entry.context.as_deref().unwrap_or("")
            );
            let entry_tokens = tokenize(&entry_text);
            let overlap = query_tokens.intersection(&entry_tokens).count();
            (idx, overlap)
        })
        .filter(|(_, overlap)| *overlap > 0)
        .collect();

    // Sort by overlap descending.
    scored.sort_by_key(|s| std::cmp::Reverse(s.1));
    scored.truncate(top_k);

    let items = scored
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
        .collect();

    Ok(items)
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
}
