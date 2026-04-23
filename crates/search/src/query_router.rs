//! Rule-based query type classifier for auto-routing search queries.
//!
//! Ports the Python weighted-scoring heuristic from `cognee/tasks/search/query_router.py`.
//! Each detection rule adds weight to a [`SearchType`]; the highest-scoring type wins.

use crate::types::SearchType;

/// Result of query routing.
#[derive(Debug, Clone)]
pub struct RouteResult {
    /// The recommended search type.
    pub search_type: SearchType,
    /// Confidence score (sum of matching rule weights).
    pub confidence: f32,
    /// Second-best search type.
    pub runner_up: SearchType,
    /// Runner-up confidence score.
    pub runner_up_score: f32,
}

/// Route a natural-language query to the most appropriate [`SearchType`].
///
/// Uses a rule-based weighted-scoring classifier (no LLM call). Each pattern
/// match adds weight to a candidate search type; the type with the highest
/// cumulative score wins.
///
/// Falls back to [`SearchType::GraphCompletion`] (base score 2.0) when no
/// strong pattern matches.
pub fn route_query(query: &str) -> RouteResult {
    let lower = query.to_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();

    let mut scores: Vec<(SearchType, f32)> = vec![
        // Default fallback score.
        (SearchType::GraphCompletion, 2.0),
    ];

    // -- Cypher syntax detection (highest priority) --
    let cypher_keywords = ["match", "return", "create", "merge", "where", "set"];
    let cypher_score: f32 = cypher_keywords
        .iter()
        .filter(|kw| words.contains(kw))
        .count() as f32
        * 3.5;
    if cypher_score > 0.0 {
        scores.push((SearchType::Cypher, cypher_score.min(10.0)));
    }

    // -- Quoted exact phrases -> lexical search --
    if query.contains('"') || query.contains('\'') {
        scores.push((SearchType::ChunksLexical, 8.0));
    }

    // -- Coding rules keywords --
    let coding_keywords = [
        "coding",
        "convention",
        "rule",
        "standard",
        "guideline",
        "best practice",
        "lint",
        "style guide",
    ];
    if coding_keywords.iter().any(|kw| lower.contains(kw)) {
        scores.push((SearchType::CodingRules, 5.0));
    }

    // -- Summary keywords --
    let summary_keywords = [
        "summary",
        "summarize",
        "summarise",
        "overview",
        "gist",
        "brief",
        "tldr",
        "tl;dr",
    ];
    if summary_keywords.iter().any(|kw| lower.contains(kw))
        && !has_negation_nearby(&lower, summary_keywords.as_slice())
    {
        scores.push((SearchType::GraphSummaryCompletion, 5.0));
    }

    // -- Reasoning / chain-of-thought keywords --
    let cot_keywords = [
        "reason",
        "reasoning",
        "explain",
        "why",
        "how does",
        "step by step",
        "think through",
        "analyze",
        "analyse",
    ];
    if cot_keywords.iter().any(|kw| lower.contains(kw))
        && !has_negation_nearby(&lower, cot_keywords.as_slice())
    {
        scores.push((SearchType::GraphCompletionCot, 4.0));
    }

    // -- Relationship / context-extension keywords --
    let relationship_keywords = [
        "relationship",
        "connected",
        "related",
        "between",
        "link",
        "association",
        "connection",
    ];
    if relationship_keywords.iter().any(|kw| lower.contains(kw))
        && !has_negation_nearby(&lower, relationship_keywords.as_slice())
    {
        scores.push((SearchType::GraphCompletionContextExtension, 5.0));
    }

    // -- Temporal keywords --
    let temporal_keywords = [
        "when",
        "before",
        "after",
        "during",
        "timeline",
        "chronolog",
        "date",
        "year",
        "month",
        "century",
    ];
    let temporal_count = temporal_keywords
        .iter()
        .filter(|kw| lower.contains(*kw))
        .count();
    if temporal_count > 0 {
        // Boost for years (4-digit numbers).
        let has_year = words.iter().any(|w| {
            w.len() == 4 && w.chars().all(|c| c.is_ascii_digit()) && w.starts_with(['1', '2'])
        });
        let temporal_score = (temporal_count as f32 * 3.0) + if has_year { 3.0 } else { 0.0 };
        scores.push((SearchType::Temporal, temporal_score.min(6.0)));
    }

    // -- Aggregate by SearchType (sum weights for each type) --
    let mut aggregated: Vec<(SearchType, f32)> = Vec::new();
    for (st, score) in &scores {
        if let Some(entry) = aggregated.iter_mut().find(|(s, _)| s == st) {
            entry.1 += score;
        } else {
            aggregated.push((*st, *score));
        }
    }

    // Sort descending by score.
    aggregated.sort_by(|a, b| b.1.total_cmp(&a.1));

    let (search_type, confidence) = aggregated
        .first()
        .copied()
        .unwrap_or((SearchType::GraphCompletion, 2.0));
    let (runner_up, runner_up_score) = aggregated
        .get(1)
        .copied()
        .unwrap_or((SearchType::GraphCompletion, 0.0));

    RouteResult {
        search_type,
        confidence,
        runner_up,
        runner_up_score,
    }
}

/// Check if a negation word appears within ~20 characters before any of the
/// given keywords in the text. This suppresses false-positive matches like
/// "I don't want a summary".
fn has_negation_nearby(text: &str, keywords: &[&str]) -> bool {
    let negation_words = ["not", "don't", "doesn't", "no", "never", "without", "isn't"];

    for kw in keywords {
        if let Some(kw_pos) = text.find(kw) {
            // Walk backwards to find a valid UTF-8 char boundary for the window.
            let mut window_start = kw_pos.saturating_sub(25);
            while window_start > 0 && !text.is_char_boundary(window_start) {
                window_start -= 1;
            }
            let window = &text[window_start..kw_pos];
            for neg in &negation_words {
                if window.contains(neg) {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_routes_to_graph_completion() {
        let result = route_query("tell me about quantum physics");
        assert_eq!(result.search_type, SearchType::GraphCompletion);
    }

    #[test]
    fn quoted_phrase_routes_to_lexical() {
        let result = route_query("find \"exact match\" in documents");
        assert_eq!(result.search_type, SearchType::ChunksLexical);
    }

    #[test]
    fn cypher_syntax_detected() {
        let result = route_query("MATCH (n) WHERE n.name = 'Alice' RETURN n");
        assert_eq!(result.search_type, SearchType::Cypher);
    }

    #[test]
    fn summary_keywords_route_correctly() {
        let result = route_query("give me a summary of the project");
        assert_eq!(result.search_type, SearchType::GraphSummaryCompletion);
    }

    #[test]
    fn negation_suppresses_summary() {
        let result = route_query("I don't want a summary, give me details");
        assert_ne!(
            result.search_type,
            SearchType::GraphSummaryCompletion,
            "negation should suppress summary routing"
        );
    }

    #[test]
    fn temporal_keywords_route_correctly() {
        let result = route_query("what happened in the year 2020 timeline");
        assert_eq!(result.search_type, SearchType::Temporal);
    }

    #[test]
    fn relationship_keywords_route_correctly() {
        let result = route_query("what is the relationship between Alice and Bob");
        assert_eq!(
            result.search_type,
            SearchType::GraphCompletionContextExtension
        );
    }

    #[test]
    fn reasoning_keywords_route_to_cot() {
        let result = route_query("explain step by step how photosynthesis works");
        assert_eq!(result.search_type, SearchType::GraphCompletionCot);
    }

    #[test]
    fn coding_keywords_route_correctly() {
        let result = route_query("what are the coding conventions for this project");
        assert_eq!(result.search_type, SearchType::CodingRules);
    }

    #[test]
    fn route_result_has_runner_up() {
        let result = route_query("summarize the timeline of events");
        // Should have both summary and temporal scores.
        assert!(result.confidence > 0.0);
        assert!(result.runner_up_score >= 0.0);
    }
}
