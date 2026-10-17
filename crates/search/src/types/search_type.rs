use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[derive(Default)]
pub enum SearchType {
    Summaries,
    Chunks,
    RagCompletion,
    TripletCompletion,
    GraphCompletion,
    GraphSummaryCompletion,
    Cypher,
    NaturalLanguage,
    GraphCompletionCot,
    GraphCompletionContextExtension,
    FeelingLucky,
    Feedback,
    Temporal,
    CodingRules,
    ChunksLexical,
    #[default]
    HybridCompletion,
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::SearchType;

    #[test]
    fn serializes_with_python_compatible_names() {
        let value = serde_json::to_value(SearchType::GraphCompletionCot).unwrap();
        assert_eq!(
            value,
            serde_json::Value::String("GRAPH_COMPLETION_COT".to_string())
        );
    }

    #[test]
    fn deserializes_python_compatible_names() {
        let parsed: SearchType = serde_json::from_str("\"CHUNKS_LEXICAL\"").unwrap();
        assert_eq!(parsed, SearchType::ChunksLexical);
    }

    #[test]
    fn serializes_hybrid_completion() {
        let value = serde_json::to_value(SearchType::HybridCompletion).unwrap();
        assert_eq!(
            value,
            serde_json::Value::String("HYBRID_COMPLETION".to_string())
        );
    }

    #[test]
    fn deserializes_hybrid_completion() {
        let parsed: SearchType = serde_json::from_str("\"HYBRID_COMPLETION\"").unwrap();
        assert_eq!(parsed, SearchType::HybridCompletion);
    }
}
