//! JSON Schema generation utilities for structured output.
//!
//! This module provides helpers for generating JSON schemas from Rust types
//! using the `schemars` crate. The schemas are used to guide LLMs in producing
//! correctly structured output.

use schemars::{JsonSchema, schema_for};
use serde_json::{Value, json};

/// Generate a JSON schema for a given type.
///
/// This is a convenience wrapper around `schemars::schema_for!` that:
/// - Generates the schema at runtime
/// - Serializes it to a `serde_json::Value`
/// - Can be easily included in LLM prompts or function call definitions
///
/// # Type Parameters
/// * `T` - The type to generate a schema for (must implement `JsonSchema`)
///
/// # Returns
/// A JSON schema as a `serde_json::Value`
///
/// # Example
/// ```
/// use cognee_llm::schema::generate_json_schema;
/// use schemars::JsonSchema;
/// use serde::{Deserialize, Serialize};
///
/// #[derive(Serialize, Deserialize, JsonSchema)]
/// struct Person {
///     name: String,
///     age: u32,
/// }
///
/// let schema = generate_json_schema::<Person>();
/// println!("{}", serde_json::to_string_pretty(&schema).unwrap());
/// ```
#[allow(
    clippy::expect_used,
    reason = "schemars-generated schema always serializes to valid JSON"
)]
pub fn generate_json_schema<T: JsonSchema>() -> Value {
    let schema = schema_for!(T);
    serde_json::to_value(schema).expect("Failed to serialize schema")
}

/// Generate a JSON schema string for a given type.
///
/// Same as `generate_json_schema` but returns a formatted JSON string
/// that can be directly embedded in prompts.
///
/// # Type Parameters
/// * `T` - The type to generate a schema for
///
/// # Arguments
/// * `pretty` - If true, formats the JSON with indentation
///
/// # Returns
/// A JSON schema as a String
///
/// # Example
/// ```
/// use cognee_llm::schema::generate_json_schema_string;
/// use schemars::JsonSchema;
/// use serde::{Deserialize, Serialize};
///
/// #[derive(Serialize, Deserialize, JsonSchema)]
/// struct Task {
///     title: String,
///     completed: bool,
/// }
///
/// let schema_str = generate_json_schema_string::<Task>(true);
/// println!("Schema:\n{}", schema_str);
/// ```
#[allow(
    clippy::expect_used,
    reason = "schemars-generated schema always serializes to valid JSON"
)]
pub fn generate_json_schema_string<T: JsonSchema>(pretty: bool) -> String {
    let schema = generate_json_schema::<T>();
    if pretty {
        serde_json::to_string_pretty(&schema).expect("Failed to serialize schema")
    } else {
        serde_json::to_string(&schema).expect("Failed to serialize schema")
    }
}

/// Convert a `GraphModel` (or any `JsonSchema` type) to its JSON schema representation.
///
/// This is a convenience alias for [`generate_json_schema`] with a domain-specific name,
/// mirroring Python's `graph_model_to_graph_schema()` from `cognee/shared/graph_model_utils.py`.
///
/// # Type Parameters
/// * `T` - The graph model type (must implement `JsonSchema`)
///
/// # Returns
/// A JSON schema as a `serde_json::Value`
///
/// # Example
/// ```
/// use cognee_llm::schema::graph_model_to_schema;
/// use schemars::JsonSchema;
/// use serde::{Deserialize, Serialize};
///
/// #[derive(Serialize, Deserialize, JsonSchema)]
/// struct MyGraphModel {
///     nodes: Vec<String>,
///     edges: Vec<(String, String)>,
/// }
///
/// let schema = graph_model_to_schema::<MyGraphModel>();
/// assert!(schema["properties"].is_object());
/// ```
pub fn graph_model_to_schema<T: JsonSchema>() -> Value {
    generate_json_schema::<T>()
}

/// Convert a `GraphModel` (or any `JsonSchema` type) to its JSON schema as a string.
///
/// Convenience wrapper around [`generate_json_schema_string`] with domain-specific naming.
///
/// # Type Parameters
/// * `T` - The graph model type (must implement `JsonSchema`)
///
/// # Arguments
/// * `pretty` - If true, formats the JSON with indentation
///
/// # Returns
/// A JSON schema as a String
pub fn graph_model_to_schema_string<T: JsonSchema>(pretty: bool) -> String {
    generate_json_schema_string::<T>(pretty)
}

/// Build a system prompt that includes a JSON schema.
///
/// This helper constructs a system prompt following the Python cognee pattern:
/// 1. Includes the user's instructions
/// 2. Specifies that output must be valid JSON only (no markdown, no extra text)
/// 3. Includes the JSON schema specification for reference
///
/// Note: When using OpenAI function calling, the schema is also sent via the API.
/// When using JSON mode (Ollama, etc.), this prompt-embedded schema guides the model.
///
/// # Type Parameters
/// * `T` - The expected response type (must implement JsonSchema)
///
/// # Arguments
/// * `instructions` - Task instructions for the LLM (what to extract/generate)
///
/// # Returns
/// A complete system prompt with embedded schema and strict formatting instructions
///
/// # Example
/// ```
/// use cognee_llm::schema::build_schema_prompt;
/// use schemars::JsonSchema;
/// use serde::{Deserialize, Serialize};
///
/// #[derive(Serialize, Deserialize, JsonSchema)]
/// struct Answer {
///     result: String,
///     confidence: f32,
/// }
///
/// let prompt = build_schema_prompt::<Answer>(
///     "Extract the answer and confidence score from the user's input."
/// );
/// ```
pub fn build_schema_prompt<T: JsonSchema>(instructions: &str) -> String {
    let schema = generate_json_schema_string::<T>(true);
    format!(
        r#"{instructions}

Your response MUST be a valid JSON object that conforms to the schema below. Do not include any explanatory text, markdown formatting, or code blocks outside of the JSON.

Schema:
{schema}

IMPORTANT: Return ONLY the JSON object. No additional text before or after."#
    )
}

/// Build a schema-aware validator for the type-erased raw structured-output path
/// (which has no Rust type to deserialize into).
///
/// Enforces that every property named in the schema's top-level `required` array
/// is present and non-null, so a tool/function input omitting a required field
/// drives a corrective retry instead of being returned as `Ok`. Deliberately
/// shallow (top-level only), matching the non-strict tool-calling path both
/// adapters use — it does not recurse into `$defs` or set `additionalProperties`.
///
/// Shared by the OpenAI and Anthropic adapters so the raw path validates
/// identically across providers (previously a verbatim copy in each adapter).
pub fn schema_required_validator(schema: &Value) -> impl Fn(&Value) -> Result<(), String> + '_ {
    move |value: &Value| {
        let Some(required) = schema.get("required").and_then(Value::as_array) else {
            return Ok(());
        };
        let Some(obj) = value.as_object() else {
            return Err("expected a JSON object".to_string());
        };
        for field in required {
            if let Some(name) = field.as_str() {
                match obj.get(name) {
                    None => return Err(format!("missing required field `{name}`")),
                    Some(Value::Null) => {
                        return Err(format!("required field `{name}` is null"));
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
}

/// The corrective-retry instruction text, without touching any request body.
///
/// Split out of [`append_corrective_instruction`] so providers whose message
/// content is **not** a plain string can reuse the exact wording while applying
/// their own append semantics — Bedrock Converse carries
/// `content: [{"text": …}]` blocks, so the string-content append below would
/// corrupt its body (plan §6.7: the Anthropic repair loop is a pattern, and only
/// this helper and [`schema_required_validator`] are genuinely reusable).
///
/// `tool_name` / `tool_kind` name the forced extractor as the provider addresses
/// it — e.g. `"extract_structured_data"` + `"tool"` for Anthropic,
/// `"json_tool_call"` + `"tool"` for Bedrock Converse, or `+ "function"` for
/// OpenAI.
pub fn corrective_instruction(reason: Option<&str>, tool_name: &str, tool_kind: &str) -> String {
    let detail = corrective_reason_detail(reason);
    format!(
        "{detail}Call the `{tool_name}` {tool_kind} again and return ONE complete object that \
         fills in every required field, strictly matching the schema. No extra text."
    )
}

/// The "why the previous attempt failed" preamble shared by every corrective
/// re-ask, ending in a trailing space so a directive can be appended to it.
///
/// Exists so a provider whose structured output does **not** travel through a
/// tool can state its own directive without re-inventing (or drifting from) this
/// wording: Bedrock Converse's native `outputConfig.textFormat` branch has no
/// tool to call, so telling the model to call one would be actively misleading
/// on the branch both shipped Anthropic models take
/// (`adapters::bedrock::converse::append_corrective_instruction`).
pub fn corrective_reason_detail(reason: Option<&str>) -> String {
    match reason {
        Some(r) => format!("Your previous response failed validation: {r}. "),
        None => {
            "Your previous response could not be parsed into the required structure. ".to_string()
        }
    }
}

/// Append a corrective instruction to a chat request's `messages` array so the
/// next structured-output attempt carries the failure reason, the way instructor
/// reasks with the validation error. When `reason` is `Some`, it is surfaced
/// verbatim (e.g. a serde "missing field type" message) so the model knows
/// precisely which required field or structural constraint the previous response
/// violated.
///
/// Extends the last user turn in place when there is one; otherwise (the last
/// turn is an assistant message, or there is no turn) it pushes a new user turn
/// carrying the instruction, so the correction is never silently dropped.
/// `tool_name` / `tool_kind` name the forced extractor as the provider addresses
/// it — e.g. `"extract_structured_data"` + `"tool"` for Anthropic, or
/// `+ "function"` for OpenAI. Shared by both adapters (previously a near-verbatim
/// copy in each) so the corrective-retry prompt stays consistent across
/// providers.
pub fn append_corrective_instruction(
    request: &mut Value,
    reason: Option<&str>,
    tool_name: &str,
    tool_kind: &str,
) {
    let instruction = corrective_instruction(reason, tool_name, tool_kind);
    let Some(messages) = request["messages"].as_array_mut() else {
        return;
    };
    match messages.last_mut() {
        // Extend the existing user turn in place.
        Some(last) if last["role"] == "user" => {
            let original = last["content"].as_str().unwrap_or("").to_string();
            last["content"] = json!(format!("{original}\n\n{instruction}"));
        }
        // Last turn is assistant, or there is no turn: a bare append would be a
        // no-op and the correction would be silently dropped, so add a new user
        // turn carrying it.
        _ => messages.push(json!({ "role": "user", "content": instruction })),
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "test code — panics are acceptable"
    )]
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, JsonSchema)]
    struct TestPerson {
        name: String,
        age: u32,
        email: Option<String>,
    }

    #[test]
    fn test_generate_json_schema() {
        let schema = generate_json_schema::<TestPerson>();
        assert!(schema.is_object());

        // Check that schema has expected properties
        let schema_obj = schema.as_object().unwrap();
        assert!(schema_obj.contains_key("$schema") || schema_obj.contains_key("properties"));
    }

    #[test]
    fn test_generate_json_schema_string() {
        let schema_str = generate_json_schema_string::<TestPerson>(false);
        assert!(!schema_str.is_empty());
        assert!(schema_str.contains("name"));
        assert!(schema_str.contains("age"));

        // Test pretty formatting
        let pretty_str = generate_json_schema_string::<TestPerson>(true);
        assert!(pretty_str.contains('\n')); // Pretty print should have newlines
    }

    #[test]
    fn test_build_schema_prompt() {
        let prompt = build_schema_prompt::<TestPerson>("Extract person information.");
        assert!(prompt.contains("Extract person information"));
        assert!(prompt.contains("valid JSON"));
        assert!(prompt.contains("schema"));
        assert!(prompt.contains("name"));
    }

    #[test]
    fn test_graph_model_to_schema() {
        let schema = graph_model_to_schema::<TestPerson>();
        assert!(schema.is_object());
        // Should be identical to generate_json_schema
        let expected = generate_json_schema::<TestPerson>();
        assert_eq!(schema, expected);
    }

    #[test]
    fn test_graph_model_to_schema_string() {
        let schema_str = graph_model_to_schema_string::<TestPerson>(false);
        let expected = generate_json_schema_string::<TestPerson>(false);
        assert_eq!(schema_str, expected);

        let pretty_str = graph_model_to_schema_string::<TestPerson>(true);
        assert!(pretty_str.contains('\n'));
    }

    #[test]
    fn schema_required_validator_enforces_required_fields() {
        // The raw structured-output path synthesises this validator so an omitted
        // required field is a retryable miss (not silently accepted). Shared by
        // the OpenAI and Anthropic adapters.
        let schema = json!({
            "type": "object",
            "required": ["summary"],
            "properties": {"summary": {"type": "string"}}
        });
        let validate = schema_required_validator(&schema);
        assert!(validate(&json!({"summary": "hello"})).is_ok());
        assert!(validate(&json!({"other": "x"})).is_err());
        assert!(validate(&json!({"summary": null})).is_err());
        assert!(validate(&json!("not an object")).is_err());

        // No `required` array → nothing to enforce.
        let loose = json!({"type": "object"});
        let validate = schema_required_validator(&loose);
        assert!(validate(&json!({})).is_ok());
    }
}
