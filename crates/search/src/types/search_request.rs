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
    pub wide_search_top_k: Option<usize>,
    pub triplet_distance_penalty: Option<f32>,
    /// Whether to persist this query and its result to the search history database.
    /// Defaults to `true` when omitted, matching the Python SDK behavior where every
    /// search is logged unconditionally. Set to `Some(false)` to opt out of logging.
    pub save_interaction: Option<bool>,
    #[serde(default)]
    pub user_id: Option<Uuid>,
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
}
