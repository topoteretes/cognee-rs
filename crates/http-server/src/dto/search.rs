//! DTOs for `/api/v1/search` (and shared with `/api/v1/recall`).
//!
//! Wire-shape mirrors Python's `SearchPayloadDTO`, `SearchHistoryItem`,
//! `SearchResult`, and `ErrorResponse` from
//! [`cognee/api/v1/search/routers/get_search_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/search/routers/get_search_router.py).
//!
//! See `docs/http-server/routers/search.md` §4 for the field-by-field reference.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

// ─── Wire-facing SearchType ───────────────────────────────────────────────────

/// Wire-facing search-type enum mirroring Python's `SearchType` byte-for-byte.
///
/// Note: the Rust core enum (`cognee_search::types::SearchType`) carries an
/// extra `Feedback` variant that has no Python counterpart. Per the audit
/// outcome documented in `docs/http-server/routers/search.md` §6 Q1, the wire
/// DTO drops `Feedback` entirely. Library callers can still reach the internal
/// variant via `cognee_search::types::SearchType` directly; HTTP requests that
/// supply `"FEEDBACK"` deserialize as a validation error instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WireSearchType {
    Summaries,
    Chunks,
    RagCompletion,
    TripletCompletion,
    #[default]
    GraphCompletion,
    GraphSummaryCompletion,
    Cypher,
    NaturalLanguage,
    GraphCompletionCot,
    GraphCompletionContextExtension,
    FeelingLucky,
    Temporal,
    CodingRules,
    ChunksLexical,
}

impl From<WireSearchType> for cognee_search::types::SearchType {
    fn from(value: WireSearchType) -> Self {
        use cognee_search::types::SearchType as Core;
        match value {
            WireSearchType::Summaries => Core::Summaries,
            WireSearchType::Chunks => Core::Chunks,
            WireSearchType::RagCompletion => Core::RagCompletion,
            WireSearchType::TripletCompletion => Core::TripletCompletion,
            WireSearchType::GraphCompletion => Core::GraphCompletion,
            WireSearchType::GraphSummaryCompletion => Core::GraphSummaryCompletion,
            WireSearchType::Cypher => Core::Cypher,
            WireSearchType::NaturalLanguage => Core::NaturalLanguage,
            WireSearchType::GraphCompletionCot => Core::GraphCompletionCot,
            WireSearchType::GraphCompletionContextExtension => {
                Core::GraphCompletionContextExtension
            }
            WireSearchType::FeelingLucky => Core::FeelingLucky,
            WireSearchType::Temporal => Core::Temporal,
            WireSearchType::CodingRules => Core::CodingRules,
            WireSearchType::ChunksLexical => Core::ChunksLexical,
        }
    }
}

// ─── SearchPayloadDTO ─────────────────────────────────────────────────────────

/// Mirrors Python `SearchPayloadDTO` in
/// [`get_search_router.py:25-36`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/search/routers/get_search_router.py#L25-L36).
///
/// `SearchPayloadDTO` inherits `InDTO`, so the wire is camelCase per Decision
/// 10 with snake_case accepted as an inbound alias.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchPayloadDTO {
    /// Python: `search_type: SearchType = SearchType.GRAPH_COMPLETION`
    #[serde(default = "default_search_type", alias = "search_type")]
    pub search_type: WireSearchType,

    /// Python: `datasets: Optional[list[str]] = None`
    #[serde(default)]
    pub datasets: Option<Vec<String>>,

    /// Python: `dataset_ids: Optional[list[UUID]] = None`
    #[serde(default, alias = "dataset_ids")]
    pub dataset_ids: Option<Vec<Uuid>>,

    /// Python: `query: str = "What is in the document?"`
    #[serde(default = "default_query")]
    pub query: String,

    /// Python: `system_prompt: Optional[str] = "Answer the question..."`.
    #[serde(default = "default_system_prompt", alias = "system_prompt")]
    pub system_prompt: Option<String>,

    /// Python: `node_name: Optional[list[str]] = None`
    #[serde(default, alias = "node_name")]
    pub node_name: Option<Vec<String>>,

    /// Python: `top_k: Optional[int] = 10`
    #[serde(default = "default_top_k", alias = "top_k")]
    pub top_k: Option<i32>,

    /// Python: `only_context: bool = False`
    #[serde(default, alias = "only_context")]
    pub only_context: bool,

    /// Python: `verbose: bool = False`
    #[serde(default)]
    pub verbose: bool,
}

pub(crate) fn default_search_type() -> WireSearchType {
    WireSearchType::GraphCompletion
}

pub(crate) fn default_query() -> String {
    "What is in the document?".to_string()
}

pub(crate) fn default_system_prompt() -> Option<String> {
    Some("Answer the question using the provided context. Be as brief as possible.".to_string())
}

pub(crate) fn default_top_k() -> Option<i32> {
    Some(10)
}

impl Default for SearchPayloadDTO {
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

// ─── SearchHistoryItemDTO ─────────────────────────────────────────────────────

/// Mirrors Python's inline `SearchHistoryItem` (`get_search_router.py:42-46`).
///
/// Carries only the four fields the frontend relies on; the underlying
/// `SearchHistoryEntry` row has `query_id`/`entry_type`/`query_type` columns
/// that are intentionally not exposed for Python parity.
///
/// `SearchHistoryItem` inherits `OutDTO` in Python, so the wire is camelCase
/// (e.g. `createdAt`) per Decision 10.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchHistoryItemDTO {
    pub id: Uuid,
    pub text: String,
    /// `"user"` for query rows, `"system"` for result rows.
    pub user: String,
    /// Wire format: RFC 3339 with explicit `+00:00` offset and microsecond
    /// precision (Python parity per Decision 6). See
    /// [`crate::dto::util::iso8601_offset`].
    #[serde(with = "crate::dto::util::iso8601_offset")]
    pub created_at: DateTime<Utc>,
}

impl SearchHistoryItemDTO {
    /// Project a `cognee_database::SearchHistoryEntry` onto the wire shape.
    pub fn from_entry(entry: cognee_database::SearchHistoryEntry) -> Self {
        let user = match entry.entry_type {
            cognee_database::SearchHistoryEntryType::Query => "user",
            cognee_database::SearchHistoryEntryType::Result => "system",
        };
        Self {
            id: entry.entry_id,
            text: entry.content,
            user: user.to_string(),
            created_at: entry.created_at,
        }
    }
}

// ─── SearchResultDTO ──────────────────────────────────────────────────────────

/// Mirrors Python `SearchResult` (`cognee/modules/search/types/SearchResult.py`).
///
/// `search_result` is polymorphic — see `flatten_search_response` for the
/// per-`SearchOutput` variant mapping.
///
/// `SearchResult` inherits `OutDTO` in Python, so the wire is camelCase
/// (`searchResult`, `datasetId`, `datasetName`) per Decision 10.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchResultDTO {
    pub search_result: Value,
    pub dataset_id: Option<Uuid>,
    pub dataset_name: Option<String>,
}

// ─── ErrorResponseDTO ─────────────────────────────────────────────────────────

/// Mirrors Python's `ErrorResponse {error, detail}` from
/// [`cognee/api/DTO.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/DTO.py).
///
/// Used by `/api/v1/search`. The recall router uses a different envelope —
/// see `crate::error::RecallErrorBody`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ErrorResponseDTO {
    pub error: String,
    pub detail: Option<String>,
}

// ─── flatten_search_response ──────────────────────────────────────────────────

/// Flatten a `SearchResponse` into the Python-shaped wire `Vec<SearchResultDTO>`.
///
/// The `SearchOutput` enum's variant determines the JSON shape of the
/// `search_result` field:
///
/// - `Text(s)`              → `search_result: <string>`
/// - `Items(items)`         → `search_result: <array of items>`
/// - `Texts(strings)`       → `search_result: <array of strings>`
/// - `GraphQueryRows(rows)` → `search_result: <array of arrays>`
/// - `Rules(rules)`         → `search_result: <array of {node_set, text}>`
/// - `Structured(value)`    → `search_result: <value>`
/// - `Ack { message }`      → `search_result: {"message": "..."}`
///
/// See `docs/http-server/routers/search.md` §4 ("Wire shape of `search_result`").
pub fn flatten_search_response(
    response: cognee_search::types::SearchResponse,
) -> Vec<SearchResultDTO> {
    use cognee_search::types::SearchOutput;

    let dataset_id = response.datasets.as_ref().and_then(|d| d.first().copied());

    let search_result = match response.result {
        SearchOutput::Text(s) => Value::String(s),
        SearchOutput::Items(items) => {
            serde_json::to_value(items).unwrap_or(Value::Array(Vec::new()))
        }
        SearchOutput::Texts(texts) => {
            serde_json::to_value(texts).unwrap_or(Value::Array(Vec::new()))
        }
        SearchOutput::GraphQueryRows(rows) => {
            serde_json::to_value(rows).unwrap_or(Value::Array(Vec::new()))
        }
        SearchOutput::Rules(rules) => {
            serde_json::to_value(rules).unwrap_or(Value::Array(Vec::new()))
        }
        SearchOutput::Structured(v) => v,
        SearchOutput::Ack { message } => serde_json::json!({"message": message}),
    };

    vec![SearchResultDTO {
        search_result,
        dataset_id,
        dataset_name: None,
    }]
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_post_body_round_trips_with_defaults() {
        let payload: SearchPayloadDTO = serde_json::from_str("{}").expect("parse empty body");
        assert_eq!(payload.search_type, WireSearchType::GraphCompletion);
        assert_eq!(payload.query, "What is in the document?");
        assert_eq!(
            payload.system_prompt.as_deref(),
            Some("Answer the question using the provided context. Be as brief as possible.")
        );
        assert_eq!(payload.top_k, Some(10));
        assert!(!payload.only_context);
        assert!(!payload.verbose);
        assert!(payload.datasets.is_none());
        assert!(payload.dataset_ids.is_none());
        assert!(payload.node_name.is_none());
    }

    #[test]
    fn test_every_wire_search_type_deserializes() {
        let cases = [
            ("SUMMARIES", WireSearchType::Summaries),
            ("CHUNKS", WireSearchType::Chunks),
            ("RAG_COMPLETION", WireSearchType::RagCompletion),
            ("TRIPLET_COMPLETION", WireSearchType::TripletCompletion),
            ("GRAPH_COMPLETION", WireSearchType::GraphCompletion),
            (
                "GRAPH_SUMMARY_COMPLETION",
                WireSearchType::GraphSummaryCompletion,
            ),
            ("CYPHER", WireSearchType::Cypher),
            ("NATURAL_LANGUAGE", WireSearchType::NaturalLanguage),
            ("GRAPH_COMPLETION_COT", WireSearchType::GraphCompletionCot),
            (
                "GRAPH_COMPLETION_CONTEXT_EXTENSION",
                WireSearchType::GraphCompletionContextExtension,
            ),
            ("FEELING_LUCKY", WireSearchType::FeelingLucky),
            ("TEMPORAL", WireSearchType::Temporal),
            ("CODING_RULES", WireSearchType::CodingRules),
            ("CHUNKS_LEXICAL", WireSearchType::ChunksLexical),
        ];

        for (wire, expected) in cases {
            let json = format!("{{\"search_type\": \"{}\"}}", wire);
            let payload: SearchPayloadDTO =
                serde_json::from_str(&json).unwrap_or_else(|e| panic!("{wire}: {e}"));
            assert_eq!(payload.search_type, expected, "wire {wire}");
        }
    }

    #[test]
    fn test_feedback_variant_is_dropped_from_wire() {
        // Audit decision: `FEEDBACK` is not in the Python enum, so the wire
        // refuses it. Library callers reach `SearchType::Feedback` via the
        // core enum, never through this DTO.
        let json = r#"{"search_type": "FEEDBACK"}"#;
        let res: Result<SearchPayloadDTO, _> = serde_json::from_str(json);
        assert!(
            res.is_err(),
            "FEEDBACK must NOT deserialize on the wire-facing DTO"
        );
    }

    #[test]
    fn test_flatten_text_output() {
        use cognee_search::types::{SearchOutput, SearchResponse, SearchType};

        let response = SearchResponse::from_output(
            SearchType::GraphCompletion,
            SearchOutput::Text("hello".to_string()),
        );
        let dto_list = flatten_search_response(response);
        assert_eq!(dto_list.len(), 1);
        assert_eq!(
            dto_list[0].search_result,
            Value::String("hello".to_string())
        );
    }

    #[test]
    fn test_flatten_items_output() {
        use cognee_search::types::{SearchItem, SearchOutput, SearchResponse, SearchType};

        let items = vec![SearchItem {
            id: None,
            score: Some(0.5),
            payload: serde_json::json!({"text": "chunk"}),
        }];
        let response = SearchResponse::from_output(SearchType::Chunks, SearchOutput::Items(items));
        let dto_list = flatten_search_response(response);
        assert_eq!(dto_list.len(), 1);
        assert!(dto_list[0].search_result.is_array());
    }

    #[test]
    fn test_flatten_graph_query_rows() {
        use cognee_search::types::{SearchOutput, SearchResponse, SearchType};

        let rows = vec![vec![
            Value::String("a".to_string()),
            Value::Number(1.into()),
        ]];
        let response =
            SearchResponse::from_output(SearchType::Cypher, SearchOutput::GraphQueryRows(rows));
        let dto_list = flatten_search_response(response);
        let arr = dto_list[0].search_result.as_array().expect("array");
        assert_eq!(arr.len(), 1);
    }

    #[test]
    fn test_flatten_rules_output() {
        use cognee_search::types::{Rule, SearchOutput, SearchResponse, SearchType};

        let rules = vec![Rule {
            node_set: "ns".into(),
            text: "always do X".into(),
        }];
        let response =
            SearchResponse::from_output(SearchType::CodingRules, SearchOutput::Rules(rules));
        let dto_list = flatten_search_response(response);
        let arr = dto_list[0].search_result.as_array().expect("array");
        assert_eq!(arr[0]["node_set"], "ns");
        assert_eq!(arr[0]["text"], "always do X");
    }

    #[test]
    fn test_flatten_structured_output() {
        use cognee_search::types::{SearchOutput, SearchResponse, SearchType};

        let value = serde_json::json!({"key": "val"});
        let response = SearchResponse::from_output(
            SearchType::GraphCompletion,
            SearchOutput::Structured(value.clone()),
        );
        let dto_list = flatten_search_response(response);
        assert_eq!(dto_list[0].search_result, value);
    }

    #[test]
    fn test_flatten_ack_output() {
        use cognee_search::types::{SearchOutput, SearchResponse, SearchType};

        let response = SearchResponse::from_output(
            SearchType::GraphCompletion,
            SearchOutput::Ack {
                message: "ok".into(),
            },
        );
        let dto_list = flatten_search_response(response);
        assert_eq!(dto_list[0].search_result["message"], "ok");
    }

    #[test]
    fn search_dto_accepts_camelcase_input() {
        let json = r#"{
            "searchType": "GRAPH_COMPLETION",
            "datasetIds": ["00000000-0000-0000-0000-000000000001"],
            "systemPrompt": "sys",
            "nodeName": ["n"],
            "topK": 7,
            "onlyContext": true
        }"#;
        let payload: SearchPayloadDTO = serde_json::from_str(json).expect("camelCase parse");
        assert_eq!(payload.search_type, WireSearchType::GraphCompletion);
        assert_eq!(payload.dataset_ids.as_ref().map(|v| v.len()), Some(1));
        assert_eq!(payload.system_prompt.as_deref(), Some("sys"));
        assert_eq!(payload.top_k, Some(7));
        assert!(payload.only_context);
    }

    #[test]
    fn search_dto_accepts_snake_case_input_via_alias() {
        let json = r#"{
            "search_type": "GRAPH_COMPLETION",
            "dataset_ids": ["00000000-0000-0000-0000-000000000001"],
            "system_prompt": "sys",
            "node_name": ["n"],
            "top_k": 7,
            "only_context": true
        }"#;
        let payload: SearchPayloadDTO = serde_json::from_str(json).expect("snake_case parse");
        assert_eq!(payload.search_type, WireSearchType::GraphCompletion);
        assert_eq!(payload.dataset_ids.as_ref().map(|v| v.len()), Some(1));
        assert_eq!(payload.system_prompt.as_deref(), Some("sys"));
        assert_eq!(payload.top_k, Some(7));
        assert!(payload.only_context);
    }

    #[test]
    fn search_dto_serializes_camelcase_only() {
        let dto = SearchPayloadDTO::default();
        let s = serde_json::to_string(&dto).expect("serialize");
        for k in [
            "\"searchType\"",
            "\"systemPrompt\"",
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
    fn search_history_item_dto_serializes_camelcase_only() {
        let dto = SearchHistoryItemDTO {
            id: Uuid::nil(),
            text: "hi".into(),
            user: "user".into(),
            created_at: chrono::Utc::now(),
        };
        let s = serde_json::to_string(&dto).expect("serialize");
        assert!(s.contains("\"createdAt\""), "missing createdAt: {s}");
        assert!(
            !s.contains("\"created_at\""),
            "snake_case created_at leaked: {s}"
        );
    }

    #[test]
    fn search_result_dto_serializes_camelcase_only() {
        let dto = SearchResultDTO {
            search_result: Value::String("x".into()),
            dataset_id: Some(Uuid::nil()),
            dataset_name: Some("ds".into()),
        };
        let s = serde_json::to_string(&dto).expect("serialize");
        for k in ["\"searchResult\"", "\"datasetId\"", "\"datasetName\""] {
            assert!(s.contains(k), "missing {k} in {s}");
        }
        for forbidden in ["\"search_result\"", "\"dataset_id\"", "\"dataset_name\""] {
            assert!(
                !s.contains(forbidden),
                "snake_case key {forbidden} leaked: {s}"
            );
        }
    }

    #[test]
    fn test_history_item_user_field() {
        use chrono::Utc;
        use cognee_database::{SearchHistoryEntry, SearchHistoryEntryType};

        let q = SearchHistoryEntry {
            entry_id: Uuid::nil(),
            query_id: Uuid::nil(),
            entry_type: SearchHistoryEntryType::Query,
            content: "hi".into(),
            query_type: Some("GRAPH_COMPLETION".into()),
            user_id: None,
            created_at: Utc::now(),
        };
        assert_eq!(SearchHistoryItemDTO::from_entry(q).user, "user");

        let r = SearchHistoryEntry {
            entry_id: Uuid::nil(),
            query_id: Uuid::nil(),
            entry_type: SearchHistoryEntryType::Result,
            content: "hi".into(),
            query_type: None,
            user_id: None,
            created_at: Utc::now(),
        };
        assert_eq!(SearchHistoryItemDTO::from_entry(r).user, "system");
    }
}
