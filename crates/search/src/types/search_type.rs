use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[derive(Default)]
pub enum SearchType {
    Summaries,
    Chunks,
    RagCompletion,
    GraphCompletion,
    GraphCompletionDecomposition,
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
    GraphReport,
    #[default]
    HybridCompletion,
    /// Deterministic, model-free queries over a dataset's code graph
    /// (Python's `SearchType.CODE`): the request's `code_query` names one
    /// operation and its arguments.
    Code,
}

impl SearchType {
    /// The wire name, Python's `SearchType.value`: `GRAPH_COMPLETION`.
    #[allow(clippy::expect_used, reason = "invariant is upheld by construction")]
    pub fn wire_name(self) -> String {
        serde_json::to_value(self)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .expect(
                "SearchType should serialize to a string, as all its variants are unit variants",
            )
    }
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
    fn decomposition_and_report_use_python_names() {
        for (search_type, name) in [
            (
                SearchType::GraphCompletionDecomposition,
                "GRAPH_COMPLETION_DECOMPOSITION",
            ),
            (SearchType::GraphReport, "GRAPH_REPORT"),
        ] {
            let value = serde_json::to_value(search_type).unwrap();
            assert_eq!(value, serde_json::Value::String(name.to_string()));
            let parsed: SearchType = serde_json::from_value(value).unwrap();
            assert_eq!(parsed, search_type);
        }
    }

    #[test]
    fn code_uses_the_python_name() {
        let value = serde_json::to_value(SearchType::Code).unwrap();
        assert_eq!(value, serde_json::Value::String("CODE".to_string()));
        let parsed: SearchType = serde_json::from_str("\"CODE\"").unwrap();
        assert_eq!(parsed, SearchType::Code);
        assert_eq!(SearchType::Code.wire_name(), "CODE");
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
