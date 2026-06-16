//! DTOs for `/api/v1/llm`.
//!
//! Mirrors Python's
//! [`get_llm_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/llm/routers/get_llm_router.py)
//! — `_ALLOWED_LLM_PARAMS` is the wire filter; anything else is silently dropped.
//!
//! See `docs/http-server/routers/llm.md` §4 for the per-router spec.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use utoipa::ToSchema;

/// LLM kwargs the wire is allowed to forward into the underlying adapter.
/// Mirrors Python's `_ALLOWED_LLM_PARAMS` constant in
/// [`get_llm_router.py:20`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/llm/routers/get_llm_router.py#L20).
pub const ALLOWED_LLM_PARAMS: &[&str] = &["temperature", "max_tokens", "top_p", "seed"];

/// Filter `parameters` against the `ALLOWED_LLM_PARAMS` whitelist.
///
/// Drops any key not in the allow-list silently (no error). Non-object inputs
/// produce an empty object. Matches Python's `_safe_params()` semantics.
pub fn safe_params(input: &Value) -> Value {
    let mut out = Map::new();
    if let Some(obj) = input.as_object() {
        for (k, v) in obj {
            if ALLOWED_LLM_PARAMS.contains(&k.as_str()) {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    Value::Object(out)
}

// ─── /custom-prompt ───────────────────────────────────────────────────────────

/// Mirrors Python `CustomPromptGenerationPayloadDTO`
/// ([`get_llm_router.py:27-32`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/llm/routers/get_llm_router.py#L27-L32)).
///
/// Inherits `InDTO` in Python, so the wire is camelCase per Decision 10
/// (`graphModel` is the canonical key; `graph_model` is accepted as an alias).
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CustomPromptGenerationPayloadDTO {
    /// Free-form JSON object describing the desired graph model.
    #[serde(alias = "graph_model")]
    pub graph_model: Value,

    /// Kwargs forwarded to the LLM adapter (filtered via `safe_params`).
    #[serde(default)]
    pub parameters: Value,
}

/// Inherits `OutDTO` in Python — wire is camelCase (`customPrompt`).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CustomPromptGenerationResponseDTO {
    pub custom_prompt: String,
}

// ─── /infer-schema ────────────────────────────────────────────────────────────

/// JSON-body adapter for `POST /api/v1/llm/infer-schema`.
///
/// Note: Python's endpoint takes multipart `Form(...)` fields rather than a
/// JSON body — the Rust port uses a JSON body (acknowledged divergence). All
/// fields here are single-word, so camelCase has no wire effect, but the
/// attribute is added for forward consistency.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct InferSchemaPayloadDTO {
    /// Sample text to analyze for entity types and relationships.
    pub text: String,

    /// Same `safe_params` filter rules as `CustomPromptGenerationPayloadDTO`.
    #[serde(default)]
    pub parameters: Value,
}

/// Inherits `OutDTO` in Python — wire is camelCase (`graphSchema`).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct InferSchemaResponseDTO {
    /// Parsed-and-validated JSON object the LLM produced.
    pub graph_schema: Value,
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_safe_params_keeps_all_allowed_keys() {
        let input = json!({
            "temperature": 0.7,
            "max_tokens": 256,
            "top_p": 0.9,
            "seed": 42,
        });
        let out = safe_params(&input);
        assert_eq!(out["temperature"], json!(0.7));
        assert_eq!(out["max_tokens"], json!(256));
        assert_eq!(out["top_p"], json!(0.9));
        assert_eq!(out["seed"], json!(42));
    }

    #[test]
    fn test_safe_params_drops_unknown_keys() {
        let input = json!({
            "temperature": 0.5,
            "junk_key": "x",
            "model": "gpt-4o",
            "stream": true,
        });
        let out = safe_params(&input);
        assert_eq!(out["temperature"], json!(0.5));
        assert!(out.get("junk_key").is_none());
        assert!(out.get("model").is_none());
        assert!(out.get("stream").is_none());
    }

    #[test]
    fn test_safe_params_non_object_returns_empty_object() {
        assert_eq!(safe_params(&json!(null)), json!({}));
        assert_eq!(safe_params(&json!([1, 2, 3])), json!({}));
        assert_eq!(safe_params(&json!("string")), json!({}));
    }

    #[test]
    fn test_safe_params_empty_object_round_trips() {
        assert_eq!(safe_params(&json!({})), json!({}));
    }

    #[test]
    fn test_custom_prompt_dto_deserializes_snake_case_via_alias() {
        let json = r#"{
            "graph_model": {"entity_types": []},
            "parameters": {"temperature": 0.0}
        }"#;
        let payload: CustomPromptGenerationPayloadDTO = serde_json::from_str(json).unwrap();
        assert!(payload.graph_model.is_object());
        assert_eq!(payload.parameters["temperature"], json!(0.0));
    }

    #[test]
    fn test_custom_prompt_dto_deserializes_camelcase() {
        let json = r#"{
            "graphModel": {"entity_types": []},
            "parameters": {"temperature": 0.0}
        }"#;
        let payload: CustomPromptGenerationPayloadDTO = serde_json::from_str(json).unwrap();
        assert!(payload.graph_model.is_object());
    }

    #[test]
    fn custom_prompt_response_dto_serializes_camelcase_only() {
        let dto = CustomPromptGenerationResponseDTO {
            custom_prompt: "p".into(),
        };
        let s = serde_json::to_string(&dto).expect("serialize");
        assert!(s.contains("\"customPrompt\""), "missing customPrompt: {s}");
        assert!(
            !s.contains("\"custom_prompt\""),
            "snake_case custom_prompt leaked: {s}"
        );
    }

    #[test]
    fn infer_schema_response_dto_serializes_camelcase_only() {
        let dto = InferSchemaResponseDTO {
            graph_schema: serde_json::json!({"x": 1}),
        };
        let s = serde_json::to_string(&dto).expect("serialize");
        assert!(s.contains("\"graphSchema\""), "missing graphSchema: {s}");
        assert!(
            !s.contains("\"graph_schema\""),
            "snake_case graph_schema leaked: {s}"
        );
    }

    #[test]
    fn test_infer_schema_dto_deserializes_with_default_parameters() {
        let json = r#"{"text": "Alice met Bob."}"#;
        let payload: InferSchemaPayloadDTO = serde_json::from_str(json).unwrap();
        assert_eq!(payload.text, "Alice met Bob.");
        assert!(payload.parameters.is_null());
    }
}
