//! DTOs for `/api/v1/recall`.
//!
//! Recall is a wire-level alias of `/api/v1/search` per Python's
//! [`get_recall_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/routers/get_recall_router.py).
//! `RecallPayloadDTO` is field-for-field identical to `SearchPayloadDTO`;
//! the history and result wire shapes are simple aliases.
//!
//! See `docs/http-server/routers/recall.md` §4 for the per-router spec.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::dto::search::{
    WireSearchType, default_query, default_search_type, default_system_prompt, default_top_k,
};

// Re-exports for OpenAPI clarity — recall and search share the wire shapes.
pub use crate::dto::search::{
    SearchHistoryItemDTO as RecallHistoryItemDTO, SearchResultDTO as RecallResultDTO,
};
// Re-export the recall error envelope from the error module — it lives there
// so the variant and its body type are co-located. See Step 2 of P4.
pub use crate::error::RecallErrorBody;

/// Mirrors Python `RecallPayloadDTO`
/// ([`get_recall_router.py:23-34`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/routers/get_recall_router.py#L23-L34)).
///
/// Field-for-field identical to `SearchPayloadDTO`. **Do NOT** add `session_id`
/// or `auto_route` here — Python's HTTP DTO doesn't expose them.
///
/// `RecallPayloadDTO` inherits `InDTO`, so the wire is camelCase per
/// Decision 10 with snake_case accepted as an inbound alias.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RecallPayloadDTO {
    #[serde(default = "default_search_type", alias = "search_type")]
    pub search_type: WireSearchType,

    #[serde(default)]
    pub datasets: Option<Vec<String>>,

    #[serde(default, alias = "dataset_ids")]
    pub dataset_ids: Option<Vec<Uuid>>,

    #[serde(default = "default_query")]
    pub query: String,

    #[serde(default = "default_system_prompt", alias = "system_prompt")]
    pub system_prompt: Option<String>,

    #[serde(default, alias = "node_name")]
    pub node_name: Option<Vec<String>>,

    #[serde(default = "default_top_k", alias = "top_k")]
    pub top_k: Option<i32>,

    #[serde(default, alias = "only_context")]
    pub only_context: bool,

    #[serde(default)]
    pub verbose: bool,
}

impl Default for RecallPayloadDTO {
    fn default() -> Self {
        Self {
            search_type: default_search_type(),
            datasets: None,
            dataset_ids: None,
            query: default_query(),
            system_prompt: default_system_prompt(),
            node_name: None,
            top_k: default_top_k(),
            only_context: false,
            verbose: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_empty_post_body_round_trips_with_defaults() {
        let payload: RecallPayloadDTO = serde_json::from_str("{}").expect("empty body");
        assert_eq!(payload.search_type, WireSearchType::GraphCompletion);
        assert_eq!(payload.query, "What is in the document?");
        assert_eq!(payload.top_k, Some(10));
        assert!(!payload.only_context);
        assert!(!payload.verbose);
    }

    #[test]
    fn test_recall_dto_does_not_accept_session_id() {
        // session_id and auto_route are deliberately not on the recall DTO
        // (Python parity). serde without `deny_unknown_fields` will silently
        // ignore them — assert that supplying them does not change the shape
        // of the deserialized struct.
        let json = r#"{"session_id": "ignored", "auto_route": true, "query": "x"}"#;
        let payload: RecallPayloadDTO = serde_json::from_str(json).expect("parses");
        assert_eq!(payload.query, "x");
    }

    #[test]
    fn recall_dto_accepts_camelcase_input() {
        let json = r#"{
            "searchType": "GRAPH_COMPLETION",
            "datasetIds": ["00000000-0000-0000-0000-000000000001"],
            "systemPrompt": "sys",
            "nodeName": ["n"],
            "topK": 5,
            "onlyContext": true,
            "query": "hi"
        }"#;
        let payload: RecallPayloadDTO = serde_json::from_str(json).expect("parse camelCase");
        assert_eq!(payload.search_type, WireSearchType::GraphCompletion);
        assert_eq!(payload.dataset_ids.as_ref().map(|v| v.len()), Some(1));
        assert_eq!(payload.system_prompt.as_deref(), Some("sys"));
        assert_eq!(payload.node_name.as_ref().map(|v| v.len()), Some(1));
        assert_eq!(payload.top_k, Some(5));
        assert!(payload.only_context);
        assert_eq!(payload.query, "hi");
    }

    #[test]
    fn recall_dto_accepts_snake_case_input_via_alias() {
        let json = r#"{
            "search_type": "GRAPH_COMPLETION",
            "dataset_ids": ["00000000-0000-0000-0000-000000000001"],
            "system_prompt": "sys",
            "node_name": ["n"],
            "top_k": 5,
            "only_context": true,
            "query": "hi"
        }"#;
        let payload: RecallPayloadDTO = serde_json::from_str(json).expect("parse snake_case");
        assert_eq!(payload.search_type, WireSearchType::GraphCompletion);
        assert_eq!(payload.dataset_ids.as_ref().map(|v| v.len()), Some(1));
        assert_eq!(payload.system_prompt.as_deref(), Some("sys"));
        assert_eq!(payload.node_name.as_ref().map(|v| v.len()), Some(1));
        assert_eq!(payload.top_k, Some(5));
        assert!(payload.only_context);
    }

    #[test]
    fn recall_dto_serializes_camelcase_only() {
        let dto = RecallPayloadDTO {
            search_type: WireSearchType::GraphCompletion,
            datasets: None,
            dataset_ids: Some(vec![uuid::Uuid::nil()]),
            query: "q".into(),
            system_prompt: Some("sys".into()),
            node_name: Some(vec!["n".into()]),
            top_k: Some(5),
            only_context: true,
            verbose: false,
        };
        let s = serde_json::to_string(&dto).expect("serialize");
        for k in [
            "\"searchType\"",
            "\"datasetIds\"",
            "\"systemPrompt\"",
            "\"nodeName\"",
            "\"topK\"",
            "\"onlyContext\"",
        ] {
            assert!(s.contains(k), "missing {k} in {s}");
        }
        for forbidden in [
            "\"search_type\"",
            "\"dataset_ids\"",
            "\"system_prompt\"",
            "\"node_name\"",
            "\"top_k\"",
            "\"only_context\"",
        ] {
            assert!(
                !s.contains(forbidden),
                "snake_case key {forbidden} leaked: {s}"
            );
        }
    }

    #[test]
    fn test_with_hint_envelope_serialization() {
        let body = RecallErrorBody::WithHint {
            error: "Recall prerequisites not met".to_string(),
            hint: "Run cognify".to_string(),
        };
        let value = serde_json::to_value(&body).unwrap();
        assert_eq!(
            value,
            json!({"error": "Recall prerequisites not met", "hint": "Run cognify"})
        );
    }

    #[test]
    fn test_just_error_envelope_serialization() {
        let body = RecallErrorBody::JustError {
            error: "An error occurred during recall.".to_string(),
        };
        let value = serde_json::to_value(&body).unwrap();
        assert_eq!(value, json!({"error": "An error occurred during recall."}));
        assert!(value.get("hint").is_none());
        assert!(value.get("detail").is_none());
    }
}
