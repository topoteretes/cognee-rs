//! DTOs for the `/api/v1/responses` router.
//!
//! Wire shape mirrors Python's `cognee.api.v1.responses.models`.
//! Stage A only uses `ResponseRequestDTO` for validation (→ 400 on bad
//! payloads before the 501 stub fires).  The response-side DTOs are shipped
//! now so the OpenAPI document is forward-compatible with Stage B.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use utoipa::ToSchema;

// ─── Request ─────────────────────────────────────────────────────────────────

/// Mirrors `cognee.api.v1.responses.models.CogneeModel`.
/// Single-variant today; kept as enum for non-breaking extensibility.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, ToSchema, PartialEq, Eq)]
pub enum CogneeModelDTO {
    #[default]
    #[serde(rename = "cognee-v1")]
    CogneeV1,
}

/// Mirrors `cognee.api.v1.responses.models.ResponseRequest`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ResponseRequestDTO {
    /// Model selector. Only `"cognee-v1"` accepted today.
    #[serde(default)]
    pub model: CogneeModelDTO,
    /// Natural-language input forwarded to the upstream model.
    pub input: String,
    /// Optional tools schema. `None` means "use server default tools".
    pub tools: Option<Vec<ToolFunctionDTO>>,
    /// Tool selection policy.  `"auto"` | `"none"` | `"required"` or a
    /// JSON object `{"type":"function","function":{"name":"..."}}`.
    /// Stored as `Value` to match Python's `Union[str, Dict[str, Any]]`.
    #[serde(default = "ResponseRequestDTO::default_tool_choice")]
    pub tool_choice: Value,
    /// Optional end-user identifier forwarded to OpenAI for abuse-tracking.
    pub user: Option<String>,
    /// Sampling temperature. Forwarded verbatim; range not validated.
    #[serde(default = "ResponseRequestDTO::default_temperature")]
    pub temperature: f32,
    /// Optional cap on completion tokens.
    pub max_completion_tokens: Option<u32>,
}

impl ResponseRequestDTO {
    fn default_tool_choice() -> Value {
        Value::String("auto".into())
    }
    fn default_temperature() -> f32 {
        1.0
    }
}

/// Mirrors `cognee.api.v1.responses.models.ToolFunction`.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct ToolFunctionDTO {
    /// Always `"function"` per OpenAI's schema.
    #[serde(default = "ToolFunctionDTO::default_kind", rename = "type")]
    pub kind: String,
    pub function: FunctionDTO,
}
impl ToolFunctionDTO {
    fn default_kind() -> String {
        "function".into()
    }
}

/// Mirrors `cognee.api.v1.responses.models.Function`.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct FunctionDTO {
    pub name: String,
    pub description: String,
    pub parameters: FunctionParametersDTO,
}

/// Mirrors `cognee.api.v1.responses.models.FunctionParameters`.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct FunctionParametersDTO {
    /// Always `"object"` per JSON Schema convention.
    #[serde(default = "FunctionParametersDTO::default_type", rename = "type")]
    pub kind: String,
    pub properties: HashMap<String, Value>,
    pub required: Option<Vec<String>>,
}
impl FunctionParametersDTO {
    fn default_type() -> String {
        "object".into()
    }
}

// ─── Response ────────────────────────────────────────────────────────────────

/// Mirrors `cognee.api.v1.responses.models.ResponseBody`.
/// Stage B returns this; Stage A never constructs it (returns 501 instead).
#[derive(Debug, Serialize, ToSchema)]
pub struct ResponseBodyDTO {
    /// Server-generated id; format `resp_<hex>`.
    pub id: String,
    /// Unix epoch seconds at response assembly time.
    pub created: i64,
    /// Echoes the request's `model` field.
    pub model: String,
    /// Always `"response"`.
    pub object: String,
    /// Always `"completed"` in Stage B.
    pub status: String,
    /// One entry per dispatched `function_call` from the upstream output.
    pub tool_calls: Vec<ResponseToolCallDTO>,
    /// Token usage from the upstream call.
    pub usage: Option<ChatUsageDTO>,
    /// Reserved metadata. Always `null` today.
    pub metadata: Option<HashMap<String, Value>>,
}

/// Mirrors `cognee.api.v1.responses.models.ResponseToolCall`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ResponseToolCallDTO {
    pub id: String,
    /// Always `"function"`.
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionCallDTO,
    pub output: Option<ToolCallOutputDTO>,
}

/// Mirrors `cognee.api.v1.responses.models.FunctionCall`.
#[derive(Debug, Serialize, ToSchema)]
pub struct FunctionCallDTO {
    pub name: String,
    /// JSON-encoded string — a *string of JSON*, not a JSON object.
    pub arguments: String,
}

/// Mirrors `cognee.api.v1.responses.models.ToolCallOutput`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ToolCallOutputDTO {
    /// `"success"` or `"error"`.
    pub status: String,
    pub data: Option<HashMap<String, Value>>,
}

/// Mirrors `cognee.api.v1.responses.models.ChatUsage`.
/// Note: Python renames `input_tokens`/`output_tokens` from OpenAI's wire
/// to `prompt_tokens`/`completion_tokens`.  We keep the rename for compat.
#[derive(Debug, Serialize, Deserialize, ToSchema, Default)]
pub struct ChatUsageDTO {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trip_response_request_dto() {
        let input = json!({
            "model": "cognee-v1",
            "input": "What is the meaning of life?",
            "tools": null,
            "tool_choice": "auto",
            "temperature": 1.0
        });

        let dto: ResponseRequestDTO =
            serde_json::from_value(input).expect("deserialize ResponseRequestDTO");
        assert_eq!(dto.input, "What is the meaning of life?");
        assert_eq!(dto.model, CogneeModelDTO::CogneeV1);
        assert_eq!(dto.temperature, 1.0);
    }

    #[test]
    fn tool_choice_accepts_object_variant() {
        let input = json!({
            "input": "hello",
            "tool_choice": {"type": "function", "function": {"name": "search"}}
        });
        let dto: ResponseRequestDTO =
            serde_json::from_value(input).expect("deserialize with object tool_choice");
        assert!(dto.tool_choice.is_object());
    }
}
