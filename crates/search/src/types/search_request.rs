use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::types::SearchType;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRequest {
    pub query_text: String,
    #[serde(default)]
    pub search_type: SearchType,
    pub top_k: Option<usize>,
    pub datasets: Option<Vec<String>>,
    pub dataset_ids: Option<Vec<Uuid>>,
    pub system_prompt: Option<String>,
    pub system_prompt_path: Option<String>,
    pub only_context: Option<bool>,
    pub use_combined_context: Option<bool>,
    pub session_id: Option<String>,
    pub node_type: Option<String>,
    pub node_name: Option<Vec<String>>,
    /// Controls how multiple `node_name` filters combine:
    /// `"OR"` = match any (default), `"AND"` = match all.
    pub node_name_filter_operator: Option<String>,
    pub wide_search_top_k: Option<usize>,
    pub triplet_distance_penalty: Option<f32>,
    /// Whether to persist this query and its result to the search history database.
    /// Defaults to `true` when omitted, matching the Python SDK behavior where every
    /// search is logged unconditionally. Set to `Some(false)` to opt out of logging.
    pub save_interaction: Option<bool>,
    #[serde(default)]
    pub user_id: Option<Uuid>,
    pub verbose: Option<bool>,
    pub feedback_influence: Option<f32>,
    /// Arbitrary retriever-specific configuration passed through from the caller.
    /// Keys and values are retriever-defined; unknown keys are silently ignored.
    pub retriever_specific_config: Option<HashMap<String, serde_json::Value>>,
    /// Optional JSON schema for structured LLM output.
    /// When present, completion-generating retrievers return structured JSON
    /// matching this schema instead of plain text.
    pub response_schema: Option<serde_json::Value>,
    /// When set, routes to a community/custom retriever registered by this name,
    /// ignoring `search_type`.
    pub custom_search_type: Option<String>,
    /// When `true` and a session is active, automatically detect if the query
    /// contains feedback about the previous response before executing the search.
    pub auto_feedback_detection: Option<bool>,
    /// Number of hops from query result nodes to include in the graph context.
    /// Controls context breadth.
    pub neighborhood_depth: Option<usize>,
    /// Number of initial seed nodes for neighborhood expansion.
    /// Controls starting point density.
    pub neighborhood_seed_top_k: Option<usize>,
    /// When `true`, store a JSON summary of the retrieved context alongside the Q&A entry.
    /// When `false` (default), store an empty string — matching the Python SDK behavior
    /// where `context_to_store = ""` unless `summarize_context=True`.
    pub summarize_context: Option<bool>,
}

impl SearchRequest {
    pub fn only_context(&self) -> bool {
        self.only_context.unwrap_or(false)
    }

    pub fn use_combined_context(&self) -> bool {
        self.use_combined_context.unwrap_or(false)
    }

    pub fn top_k_or_default(&self, default_value: usize) -> usize {
        self.top_k.unwrap_or(default_value)
    }

    pub fn verbose(&self) -> bool {
        self.verbose.unwrap_or(false)
    }

    pub fn feedback_influence_or_default(&self) -> f32 {
        self.feedback_influence.unwrap_or(0.0)
    }

    /// Return the retriever-specific config map.
    pub fn retriever_config(&self) -> Option<&HashMap<String, serde_json::Value>> {
        self.retriever_specific_config.as_ref()
    }

    /// Get a string value from `retriever_specific_config`, with a fallback.
    pub fn retriever_config_str<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.retriever_specific_config
            .as_ref()
            .and_then(|m| m.get(key))
            .and_then(|v| v.as_str())
            .unwrap_or(default)
    }

    /// Get a `usize` value from `retriever_specific_config`, with a fallback.
    pub fn retriever_config_usize(&self, key: &str, default: usize) -> usize {
        self.retriever_specific_config
            .as_ref()
            .and_then(|m| m.get(key))
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(default)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_name_deserializes_as_vec() {
        let json = r#"{"query_text": "test", "node_name": ["Alice", "Bob"]}"#;
        let request: SearchRequest = serde_json::from_str(json).unwrap();
        assert_eq!(
            request.node_name,
            Some(vec!["Alice".to_string(), "Bob".to_string()])
        );
    }

    #[test]
    fn node_name_none_when_absent() {
        let json = r#"{"query_text": "test"}"#;
        let request: SearchRequest = serde_json::from_str(json).unwrap();
        assert_eq!(request.node_name, None);
    }

    #[test]
    fn retriever_config_usize_reads_value() {
        let json = r#"{
            "query_text": "test",
            "retriever_specific_config": {"max_iter": 8, "missing": null}
        }"#;
        let req: SearchRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.retriever_config_usize("max_iter", 4), 8);
        assert_eq!(req.retriever_config_usize("unknown", 4), 4);
    }

    #[test]
    fn retriever_config_str_reads_value() {
        let json = r#"{
            "query_text": "test",
            "retriever_specific_config": {"prompt_path": "/tmp/prompt.txt"}
        }"#;
        let req: SearchRequest = serde_json::from_str(json).unwrap();
        assert_eq!(
            req.retriever_config_str("prompt_path", "default.txt"),
            "/tmp/prompt.txt"
        );
        assert_eq!(req.retriever_config_str("missing", "fallback"), "fallback");
    }

    #[test]
    fn search_params_from_request_extracts_max_iter() {
        use crate::types::SearchParams;
        let json = r#"{
            "query_text": "test",
            "retriever_specific_config": {"max_iter": 6}
        }"#;
        let req: SearchRequest = serde_json::from_str(json).unwrap();
        let params = SearchParams::from(&req);
        assert_eq!(params.max_iter, Some(6));
    }

    #[test]
    fn node_name_filter_operator_deserializes() {
        let json = r#"{"query_text": "test", "node_name_filter_operator": "AND"}"#;
        let req: SearchRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.node_name_filter_operator, Some("AND".to_string()));
    }

    #[test]
    fn node_name_filter_operator_none_when_absent() {
        let json = r#"{"query_text": "test"}"#;
        let req: SearchRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.node_name_filter_operator, None);
    }

    #[test]
    fn node_name_filter_operator_wired_to_search_params() {
        use crate::types::SearchParams;
        let json = r#"{"query_text": "test", "node_name_filter_operator": "AND"}"#;
        let req: SearchRequest = serde_json::from_str(json).unwrap();
        let params = SearchParams::from(&req);
        assert_eq!(params.node_name_filter_operator, Some("AND".to_string()));
    }

    #[test]
    fn neighborhood_params_deserialize() {
        let json =
            r#"{"query_text": "test", "neighborhood_depth": 3, "neighborhood_seed_top_k": 5}"#;
        let req: SearchRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.neighborhood_depth, Some(3));
        assert_eq!(req.neighborhood_seed_top_k, Some(5));
    }

    #[test]
    fn neighborhood_params_wired_to_search_params() {
        use crate::types::SearchParams;
        let json =
            r#"{"query_text": "test", "neighborhood_depth": 2, "neighborhood_seed_top_k": 10}"#;
        let req: SearchRequest = serde_json::from_str(json).unwrap();
        let params = SearchParams::from(&req);
        assert_eq!(params.neighborhood_depth, Some(2));
        assert_eq!(params.neighborhood_seed_top_k, Some(10));
    }
}
