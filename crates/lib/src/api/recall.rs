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

use cognee_search::{SearchOrchestrator, SearchRequest, SearchResponse, SearchType, route_query};
use cognee_session::SessionStore;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

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

    // -- Determine search type --
    let (search_type, auto_routed) = if let Some(qt) = query_type {
        (qt, false)
    } else if auto_route {
        let route_result = route_query(query_text);
        info!(
            search_type = ?route_result.search_type,
            confidence = route_result.confidence,
            "recall: auto-routed query"
        );
        (route_result.search_type, true)
    } else {
        (SearchType::GraphCompletion, false)
    };

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
    scored.sort_by(|a, b| b.1.cmp(&a.1));
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
