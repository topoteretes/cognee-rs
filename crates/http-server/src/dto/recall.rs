//! DTOs for `/api/v1/recall`.
//!
//! Recall is a wire-level alias of `/api/v1/search` per Python's
//! [`get_recall_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/routers/get_recall_router.py).
//! `RecallPayloadDTO` is field-for-field identical to `SearchPayloadDTO`;
//! the history and result wire shapes are simple aliases.
//!
//! See `docs/http-server/routers/recall.md` §4 for the per-router spec.

use cognee_search::recall_scope::{RecallScope, ScopeInput, normalize_scope};
use serde::{Deserialize, Deserializer, Serialize};
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
/// ([`get_recall_router.py:23-48`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/routers/get_recall_router.py#L23-L48)).
///
/// `RecallPayloadDTO` inherits `InDTO`, so the wire is camelCase per
/// Decision 10 with snake_case accepted as an inbound alias.
///
/// E-04 added `session_id` and `scope` (per Decisions 17 + 18). The DTO
/// delegates `scope` normalization to
/// [`cognee_search::recall_scope::normalize_scope`].
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

    /// Optional session id — when set, `_search_session` and (under `auto`)
    /// the session-first short-circuit run. Wire is camelCase
    /// (`sessionId`) per Decision 10; snake_case accepted as alias.
    #[serde(default, alias = "session_id")]
    pub session_id: Option<String>,

    /// Optional source scope — `null | string | list<string>`. Normalized
    /// via [`cognee_search::recall_scope::normalize_scope`]; unknown values
    /// surface as a `serde::de::Error::custom` whose message is
    /// byte-identical to Python (`entries.py:99-103`).
    ///
    /// `#[schema(value_type = ...)]` would mis-document the wire shape
    /// (string OR list of strings), so the DTO field stays untyped at the
    /// OpenAPI layer — full schema documentation is deferred.
    #[schema(value_type = Option<Vec<String>>)]
    #[serde(default, deserialize_with = "deserialize_scope")]
    pub scope: Option<Vec<RecallScope>>,
}

/// Custom deserializer for the `scope` field.
///
/// Accepts:
/// - `null` -> `Some(vec![RecallScope::Auto])` (Python: `["auto"]`).
/// - `"graph"` -> `Some(vec![RecallScope::Graph])`.
/// - `["graph", "session"]` -> `Some(vec![Graph, Session])`.
/// - `"all"` -> the canonical four-source list.
///
/// Builds a [`ScopeInput`] from the raw JSON, dispatches into
/// [`normalize_scope`], and surfaces unknowns through
/// [`serde::de::Error::custom`] so the validation envelope ends up at
/// `400 {"detail":[{"loc":["body"],...}]}` via the `ValidatedJson`
/// extractor.
fn deserialize_scope<'de, D>(de: D) -> Result<Option<Vec<RecallScope>>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error as _;
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Single(String),
        Many(Vec<String>),
        Null,
    }

    // `Option<Raw>` gives us `null` -> `None`; otherwise we forward to
    // `normalize_scope`. Note that Python treats `null` and a missing field
    // identically (-> `["auto"]`), so we map both to `Some(vec![Auto])`.
    let raw: Option<Raw> = Option::<Raw>::deserialize(de)?;
    let scope_input: Option<ScopeInput> = match raw {
        None | Some(Raw::Null) => None,
        Some(Raw::Single(s)) => Some(ScopeInput::Single(s)),
        Some(Raw::Many(v)) => Some(ScopeInput::Many(v)),
    };

    match normalize_scope(scope_input) {
        Ok(v) => Ok(Some(v)),
        Err(err) => Err(D::Error::custom(err.to_string())),
    }
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
            session_id: None,
            scope: None,
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
    fn recall_dto_accepts_session_id() {
        // camelCase wire form (Decision 10).
        let json = r#"{"sessionId": "s1", "query": "x"}"#;
        let payload: RecallPayloadDTO = serde_json::from_str(json).expect("parse camelCase");
        assert_eq!(payload.session_id.as_deref(), Some("s1"));
        assert_eq!(payload.query, "x");

        // snake_case alias.
        let json2 = r#"{"session_id": "s2", "query": "y"}"#;
        let payload2: RecallPayloadDTO = serde_json::from_str(json2).expect("parse snake_case");
        assert_eq!(payload2.session_id.as_deref(), Some("s2"));
    }

    #[test]
    fn recall_dto_accepts_scope_as_string() {
        let json = r#"{"query": "x", "scope": "graph"}"#;
        let payload: RecallPayloadDTO = serde_json::from_str(json).expect("parse scope=graph");
        assert_eq!(payload.scope, Some(vec![RecallScope::Graph]));
    }

    #[test]
    fn recall_dto_accepts_scope_as_list() {
        let json = r#"{"query": "x", "scope": ["graph", "session"]}"#;
        let payload: RecallPayloadDTO = serde_json::from_str(json).expect("parse scope list");
        assert_eq!(
            payload.scope,
            Some(vec![RecallScope::Graph, RecallScope::Session])
        );
    }

    #[test]
    fn recall_dto_scope_all_expands_to_four_sources() {
        let json = r#"{"query": "x", "scope": "all"}"#;
        let payload: RecallPayloadDTO = serde_json::from_str(json).expect("parse scope=all");
        assert_eq!(
            payload.scope,
            Some(vec![
                RecallScope::Graph,
                RecallScope::Session,
                RecallScope::Trace,
                RecallScope::GraphContext,
            ])
        );
    }

    #[test]
    fn recall_dto_scope_null_normalizes_to_auto() {
        let json = r#"{"query": "x", "scope": null}"#;
        let payload: RecallPayloadDTO = serde_json::from_str(json).expect("parse scope=null");
        assert_eq!(payload.scope, Some(vec![RecallScope::Auto]));
    }

    #[test]
    fn recall_dto_scope_unknown_returns_serde_error() {
        let json = r#"{"query": "x", "scope": "foo"}"#;
        let err = serde_json::from_str::<RecallPayloadDTO>(json).expect_err("should error");
        let msg = err.to_string();
        assert!(
            msg.contains("Unknown recall scope(s)"),
            "msg should contain 'Unknown recall scope(s)': {msg}"
        );
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
            session_id: None,
            scope: None,
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
