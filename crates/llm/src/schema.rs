//! JSON Schema generation utilities for structured output.
//!
//! This module provides helpers for generating JSON schemas from Rust types
//! using the `schemars` crate. The schemas are used to guide LLMs in producing
//! correctly structured output.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

use schemars::{JsonSchema, schema_for};
use serde_json::{Map, Value, json};

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

/// Apply `rewrite` to every **object node** of a JSON schema, in place of the
/// three near-identical hand-rolled traversals this crate would otherwise carry.
///
/// "Object node" means the root, each entry of `properties`, `items` (both the
/// single-schema and tuple forms) and `prefixItems`, each entry of `$defs` /
/// `definitions`, and each branch of `anyOf` / `allOf` / `oneOf`. Children are
/// rewritten before the node itself, so `rewrite` always sees already-rewritten
/// descendants; none of the current callers depend on that, but a caller that
/// reads a child's keys would.
///
/// **That list is the covered set, not every position JSON Schema allows.**
/// Subschemas under `patternProperties`, `not`, `if` / `then` / `else`,
/// `contains`, `propertyNames`, `dependentSchemas` and the schema form of
/// `additionalProperties` are left untouched. This is not an oversight to fix by
/// widening the traversal: OpenAI's strict subset does not accept those keywords
/// at all, so reaching into them could not make such a schema strict-valid — it
/// would only change *which* part the provider refuses. A schema using them is
/// meant to be refused and demoted to the cascade, which answers it correctly.
/// See [`close_object_node`] for the one case where that distinction is
/// load-bearing rather than cosmetic.
///
/// The Bedrock caller wants those positions closed and does not get them, which
/// is a pre-existing gap this extraction preserves rather than introduces.
///
/// Non-object input is returned unchanged rather than erroring: a schema
/// fragment may legitimately be a bare `true` / `false` (JSON Schema's
/// always-accept / always-reject forms) and the rewriters have nothing to say
/// about those.
fn rewrite_object_nodes<F>(schema: &Value, rewrite: &F) -> Value
where
    F: Fn(&mut Map<String, Value>),
{
    let Some(object) = schema.as_object() else {
        return schema.clone();
    };
    let mut out = object.clone();

    // A map whose *values* are each a schema: `properties`, `$defs`,
    // `definitions`.
    let recurse_map = |map: &Value| -> Value {
        match map.as_object() {
            Some(entries) => Value::Object(
                entries
                    .iter()
                    .map(|(key, value)| (key.clone(), rewrite_object_nodes(value, rewrite)))
                    .collect(),
            ),
            None => map.clone(),
        }
    };

    if let Some(properties) = out.get("properties") {
        let rewritten = recurse_map(properties);
        out.insert("properties".to_string(), rewritten);
    }
    // `items` is a single schema in draft 2020-12 and may be an *array* of
    // per-position schemas in the older tuple form; `prefixItems` is 2020-12's
    // spelling of that tuple. All three carry object nodes — schemars renders a
    // Rust tuple field that way — and a node this misses is a node the strict
    // rewrite never closes, which is exactly what makes a strict request get
    // rejected.
    for items_key in ["items", "prefixItems"] {
        match out.get(items_key) {
            Some(Value::Array(entries)) => {
                let rewritten: Vec<Value> = entries
                    .iter()
                    .map(|entry| rewrite_object_nodes(entry, rewrite))
                    .collect();
                out.insert(items_key.to_string(), Value::Array(rewritten));
            }
            Some(items @ Value::Object(_)) => {
                let rewritten = rewrite_object_nodes(items, rewrite);
                out.insert(items_key.to_string(), rewritten);
            }
            _ => {}
        }
    }
    for defs_key in ["$defs", "definitions"] {
        if let Some(defs) = out.get(defs_key) {
            let rewritten = recurse_map(defs);
            out.insert(defs_key.to_string(), rewritten);
        }
    }
    for combinator in ["anyOf", "allOf", "oneOf"] {
        if let Some(Value::Array(branches)) = out.get(combinator) {
            let rewritten: Vec<Value> = branches
                .iter()
                .map(|branch| rewrite_object_nodes(branch, rewrite))
                .collect();
            out.insert(combinator.to_string(), Value::Array(rewritten));
        }
    }

    rewrite(&mut out);
    Value::Object(out)
}

/// Recursively force `"additionalProperties": false` onto every object node.
///
/// Extracted from the Bedrock Converse adapter, which needs exactly this for
/// `outputConfig` (`_add_additional_properties_to_schema`), so the OpenAI strict
/// transform below and the Bedrock one cannot drift apart. Only sets the key
/// where it is absent, so a schema that deliberately allows extra properties on
/// some node keeps saying so.
pub fn force_additional_properties_false(schema: &Value) -> Value {
    rewrite_object_nodes(schema, &|node: &mut Map<String, Value>| {
        // `declares_object` rather than a string-only `type` check: schemars
        // renders a nullable object as `{"type": ["object", "null"]}`, and such
        // a node is still an object node Bedrock's validator expects closed.
        if declares_object(node) {
            close_object_node(node);
        }
    })
}

/// Rewrite a schema into the shape OpenAI's **strict** structured output
/// requires: every object node carries `additionalProperties: false` and lists
/// *every* one of its properties in `required`.
///
/// This is the constrained-decoding ("grammar") form — the request body it goes
/// into is `response_format: {"type": "json_schema", "json_schema": {"strict":
/// true, …}}`, which OpenAI rejects outright unless the schema satisfies both
/// rules. It is deliberately **much** stronger than
/// [`crate::adapters::openai`]'s shallow top-level `required` recompute, and it
/// is the transform whose non-strict variant is MEASURED to make Baseten's
/// `gpt-oss-120b` answer HTTP 501 — which is why the caller that sends it also
/// carries a demotion ladder rather than assuming it will be accepted.
///
/// Making an optional field `required` is not a semantic change here: schemars
/// renders `Option<T>` as a nullable type (`"type": ["string", "null"]` or an
/// `anyOf` with a `"null"` branch), so the model can still decline to supply a
/// value — it just has to say so explicitly, which is precisely what OpenAI's
/// strict mode asks for and what makes the field impossible to silently drop.
///
/// `default` is dropped wherever it appears. Under strict decoding every
/// property is `required`, so the model must emit each one and a default can
/// never apply — the keyword is dead weight at best, and OpenAI's strict subset
/// is documented as accepting only a listed set of keywords, so leaving it in
/// risks a rejection that demotes a schema which would otherwise have worked.
/// schemars emits one for every `#[serde(default)]` field on a type that derives
/// `Default`, which includes real cognee models, so this is not hypothetical.
/// Note the interaction with the shallow `recompute_top_level_required`, which
/// does the opposite — it treats a literal `default` as the marker for
/// *optional*. That is correct there (it reproduces instructor's non-strict
/// rewrite) and wrong here, where OpenAI requires every property listed.
///
/// `$schema` is stripped from the root: it is a meta-annotation rather than a
/// constraint, and providers reject or ignore it inconsistently (Bedrock
/// rejects it outright — see the Converse adapter's `sanitize_schema`).
pub fn strict_json_schema(schema: &Value) -> Value {
    let mut out = rewrite_object_nodes(schema, &|node: &mut Map<String, Value>| {
        node.remove("default");
        // `{"type": "object"}` with no `properties` is a valid object node and
        // still has to be closed — returning early on the missing `properties`
        // map left it as the one node in the document outside the strict
        // subset, which is enough for the whole request to be rejected.
        let Some(properties) = node.get("properties").and_then(Value::as_object) else {
            if declares_object(node) {
                // Same reconciliation as the empty-`properties` branch below,
                // and for the same reason: `{"type": "object", "required":
                // ["x"]}` names a property the node does not describe, so
                // closing it demands a key the model is forbidden to emit. The
                // provider rejects that, and the schema is demoted for a fault
                // the rewrite introduced rather than one it had.
                node.remove("required");
                close_object_node(node);
            }
            return;
        };
        let mut required: Vec<String> = properties.keys().cloned().collect();
        required.sort();
        if required.is_empty() {
            // `"properties": {}` with a leftover `required` naming something
            // that does not exist. Leaving it while closing the node builds a
            // schema demanding a property the model is forbidden to emit —
            // unsatisfiable, and rejected by providers that check.
            node.remove("required");
        } else {
            node.insert("required".to_string(), json!(required));
        }
        close_object_node(node);
    });
    if let Some(object) = out.as_object_mut() {
        object.remove("$schema");
    }
    out
}

/// Whether a schema's root *declares* itself an object.
///
/// Used to decide whether a schema can travel in a
/// `response_format: {"type": "json_schema"}` at all, which requires an object
/// root. Deliberately does **not** infer objecthood from the presence of
/// `properties`: that keyword constrains an instance only *if* it is an object
/// and asserts nothing on its own, so a schema relying on it would be rejected
/// by the provider anyway — and synthesising the missing `type` would narrow a
/// schema its author chose to leave open.
///
/// Stricter than [`declares_object`], which the *nested* rewrite uses: a
/// nullable object (`{"type": ["object", "null"]}`) is a legitimate node inside
/// a document and has to be closed there, but as a **root** it permits a bare
/// `null` response, which structured outputs do not accept. So the root must say
/// `"type": "object"` and nothing else.
#[must_use]
pub fn declares_object_root(schema: &Value) -> bool {
    schema
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|declared| declared == "object")
}

/// Whether a node's `type` says it is an object.
///
/// Accepts the array form as well as the string one: schemars renders a
/// nullable object as `{"type": ["object", "null"]}`, and reading only
/// `as_str()` left such a node unclosed.
fn declares_object(node: &Map<String, Value>) -> bool {
    match node.get("type") {
        Some(Value::String(one)) => one == "object",
        Some(Value::Array(many)) => many.iter().any(|t| t.as_str() == Some("object")),
        _ => false,
    }
}

/// Set `additionalProperties: false` — **unless** the node is using the schema
/// form of that keyword, which is how JSON Schema spells a map.
///
/// `{"type": "object", "additionalProperties": {"type": "string"}}` is a
/// `HashMap<String, String>`. Overwriting it with `false` produces a schema the
/// provider happily **accepts** and which forbids every key, so the model
/// returns a valid, permanently empty map — wrong data, silently, with no
/// rejection to demote on and nothing in the logs. Leaving the keyword alone
/// means the node stays outside OpenAI's strict subset, the request is refused,
/// and the ladder falls back to the cascade, which returns the right answer.
/// Between a silent wrong result and a noisy fallback, take the fallback.
///
/// An explicit `additionalProperties: true` is preserved for the same reason,
/// which is the one place this differs from a first reading of "strict mode
/// closes every object". `true` says the author *wants* keys beyond
/// `properties`; rewriting it to `false` produces a schema the provider accepts
/// and which drops exactly those keys — silent field loss, again with no
/// rejection to demote on. Only an **absent** keyword is filled in, which is
/// also what makes this identical to the Bedrock rewrite it shares a traversal
/// with.
fn close_object_node(node: &mut Map<String, Value>) {
    if node.contains_key("additionalProperties") {
        return;
    }
    node.insert("additionalProperties".to_string(), json!(false));
}

/// A **process-local** fingerprint of a schema, insensitive to object-key order.
///
/// Used to key the OpenAI adapter's per-schema structured-output demotion memo
/// (litellm caches the same demotion per `(model, response_model)`). Object-key
/// order has to be normalised because the workspace enables `serde_json`'s
/// `preserve_order`, so two logically identical schemas built in different key
/// orders are distinct `Map`s.
///
/// Array order *is* significant in JSON Schema and is kept — `anyOf` branches,
/// `prefixItems` positions, `enum` members — with one exception: `required` is a
/// **set** of property names, so its order carries no meaning and is sorted
/// before hashing. Without that, `["a","b"]` and `["b","a"]` describe the same
/// schema but take two separate trips down the demotion ladder. It is the one
/// keyword worth special-casing because every object schema has one and
/// `strict_json_schema` rewrites it.
///
/// Not stable across processes or releases, and must never be persisted or put
/// on the wire: `DefaultHasher`'s algorithm is explicitly unspecified. The memo
/// it keys lives only for the life of the adapter. (The cassette hash in
/// `crate::mock::cassette` is the stable, persisted counterpart — it is sha256
/// over a canonical *string*, and is feature-gated behind `mock`.)
#[must_use]
pub fn schema_fingerprint(schema: &Value) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hash_canonical(schema, &mut hasher);
    hasher.finish()
}

/// Feed `value` into `hasher` with object keys visited in sorted order.
fn hash_canonical<H: Hasher>(value: &Value, hasher: &mut H) {
    // Discriminate the variants so `null`, `"0"` and `0` cannot collide by
    // hashing to the same byte sequence.
    match value {
        Value::Null => 0u8.hash(hasher),
        Value::Bool(b) => {
            1u8.hash(hasher);
            b.hash(hasher);
        }
        Value::Number(n) => {
            2u8.hash(hasher);
            n.to_string().hash(hasher);
        }
        Value::String(s) => {
            3u8.hash(hasher);
            s.hash(hasher);
        }
        Value::Array(items) => {
            4u8.hash(hasher);
            items.len().hash(hasher);
            for item in items {
                hash_canonical(item, hasher);
            }
        }
        Value::Object(map) => {
            5u8.hash(hasher);
            map.len().hash(hasher);
            let sorted: BTreeMap<&String, &Value> = map.iter().collect();
            for (key, val) in sorted {
                key.hash(hasher);
                // `required` is a set; its order says nothing. Sorted here
                // rather than in `hash_canonical`'s array arm so every other
                // array keeps its order, which in JSON Schema does carry
                // meaning.
                match (key.as_str(), val) {
                    ("required", Value::Array(names)) => {
                        4u8.hash(hasher);
                        names.len().hash(hasher);
                        let mut sorted_names: Vec<&Value> = names.iter().collect();
                        sorted_names.sort_by_key(|name| name.as_str().map(str::to_string));
                        for name in sorted_names {
                            hash_canonical(name, hasher);
                        }
                    }
                    _ => hash_canonical(val, hasher),
                }
            }
        }
    }
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

    #[test]
    fn strict_json_schema_forces_all_required_at_every_depth() {
        // OpenAI rejects `strict: true` unless *every* object node lists all of
        // its properties in `required` and sets `additionalProperties: false`.
        // The nesting here is the shape that matters in practice: a `$defs`
        // entry reached through an array's `items`, which is how schemars
        // renders `Vec<Edge>` on `KnowledgeGraph`.
        let schema = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "required": ["nodes"],
            "properties": {
                "nodes": {"type": "array", "items": {"$ref": "#/$defs/Node"}},
                "edges": {"type": "array", "items": {"$ref": "#/$defs/Edge"}},
            },
            "$defs": {
                "Node": {
                    "type": "object",
                    "required": ["id"],
                    "properties": {
                        "id": {"type": "string"},
                        "description": {"type": ["string", "null"]},
                    },
                },
                "Edge": {"type": "object", "properties": {"rel": {"type": "string"}}},
            },
        });

        let strict = strict_json_schema(&schema);

        // Root: both properties required even though only one was.
        assert_eq!(strict["required"], json!(["edges", "nodes"]));
        assert_eq!(strict["additionalProperties"], json!(false));
        // A `$defs` entry whose optional (nullable) field was omitted from
        // `required` now carries it — the model must say `null` rather than
        // silently drop the key, which is the whole point of strict mode.
        assert_eq!(
            strict["$defs"]["Node"]["required"],
            json!(["description", "id"])
        );
        assert_eq!(
            strict["$defs"]["Node"]["additionalProperties"],
            json!(false)
        );
        // A node that had no `required` at all gains one.
        assert_eq!(strict["$defs"]["Edge"]["required"], json!(["rel"]));
        // `$schema` is a meta-annotation, not a constraint; providers disagree
        // about whether it is even allowed here.
        assert!(strict.get("$schema").is_none());
    }

    #[test]
    fn strict_json_schema_drops_defaults_and_closes_bare_object_nodes() {
        // `default` is what schemars emits for a `#[serde(default)]` field, and
        // it is exactly the marker the *shallow* rewrite reads as "optional" —
        // so it is both common on real cognee models and meaningless here,
        // where every property is required. A bare `{"type": "object"}` has no
        // properties to require but still has to be closed; leaving it open was
        // enough to put the whole document outside the strict subset.
        let schema = json!({
            "type": "object",
            "properties": {
                "description": {"type": "string", "default": ""},
                "bag": {"type": "object"},
            },
        });

        let strict = strict_json_schema(&schema);

        assert!(
            strict["properties"]["description"].get("default").is_none(),
            "default must not survive into a strict schema",
        );
        assert_eq!(strict["required"], json!(["bag", "description"]));
        assert_eq!(
            strict["properties"]["bag"]["additionalProperties"],
            json!(false),
            "a property-less object node still has to be closed",
        );
    }

    #[test]
    fn strict_json_schema_never_clobbers_a_map() {
        // `additionalProperties` holding a *schema* is how JSON Schema spells a
        // map (`HashMap<String, String>`). Overwriting it with `false` yields a
        // schema the provider **accepts** and which forbids every key, so the
        // model returns a valid, permanently empty map — wrong data, silently,
        // with no rejection to demote on. Leaving it means the node is refused
        // and the cascade answers correctly, which is the outcome to prefer.
        let schema = json!({
            "type": "object",
            "properties": {
                "labels": {"type": "object", "additionalProperties": {"type": "string"}},
                "open": {"type": "object", "additionalProperties": true},
            },
        });

        let strict = strict_json_schema(&schema);

        assert_eq!(
            strict["properties"]["labels"]["additionalProperties"],
            json!({"type": "string"}),
            "a map's value schema must survive the strict rewrite",
        );
        // An explicit `true` is preserved for the same reason: the author asked
        // for keys beyond `properties`, and closing the node would produce a
        // request the provider accepts while dropping exactly those keys.
        assert_eq!(
            strict["properties"]["open"]["additionalProperties"],
            json!(true),
            "an explicitly open object must not be silently closed",
        );
    }

    #[test]
    fn strict_json_schema_closes_a_nullable_object() {
        // schemars renders a nullable object as a `type` *array*. Reading only
        // `as_str()` left such a node unclosed, which is enough to put the
        // document outside the strict subset.
        let strict = strict_json_schema(&json!({
            "type": "object",
            "properties": {"maybe": {"type": ["object", "null"]}},
        }));

        assert_eq!(
            strict["properties"]["maybe"]["additionalProperties"],
            json!(false),
        );
    }

    #[test]
    fn declares_object_root_rejects_a_nullable_root() {
        // A nullable object is a legitimate *nested* node and has to be closed
        // there, but as a root it permits a bare `null` response, which
        // structured outputs do not accept. The root must be unambiguous.
        assert!(declares_object_root(&json!({"type": "object"})));
        assert!(!declares_object_root(&json!({"type": ["object", "null"]})));
        assert!(!declares_object_root(&json!({"type": "string"})));
        // `properties` alone asserts nothing about the instance's type.
        assert!(!declares_object_root(
            &json!({"properties": {"a": {"type": "string"}}})
        ));
        assert!(!declares_object_root(&json!(true)));
    }

    #[test]
    fn force_additional_properties_false_closes_a_nullable_object() {
        // The helper's contract is *every* object node. Reading only the string
        // form of `type` left a nullable object open, which Bedrock's native
        // schema validation rejects.
        let out = force_additional_properties_false(&json!({
            "type": "object",
            "properties": {"maybe": {"type": ["object", "null"], "properties": {}}},
        }));

        assert_eq!(
            out["properties"]["maybe"]["additionalProperties"],
            json!(false),
        );
    }

    #[test]
    fn strict_json_schema_drops_a_required_naming_nothing() {
        // `"properties": {}` with a leftover `required`. Closing the node while
        // keeping the entry builds a schema that demands a property the model is
        // forbidden to emit — unsatisfiable, and a rejection that would be
        // remembered as "this endpoint refuses constrained decoding".
        let strict = strict_json_schema(&json!({
            "type": "object",
            "properties": {},
            "required": ["missing"],
        }));

        assert!(
            strict.get("required").is_none(),
            "stale required is dropped"
        );
        assert_eq!(strict["additionalProperties"], json!(false));

        // The same reconciliation is owed when `properties` is absent
        // altogether, not merely empty — otherwise closing the node demands a
        // key the model is forbidden to emit, and the schema is demoted for a
        // fault the rewrite introduced rather than one it had.
        let no_properties = strict_json_schema(&json!({
            "type": "object",
            "required": ["x"],
        }));
        assert!(
            no_properties.get("required").is_none(),
            "a required naming nothing goes whether properties is empty or missing",
        );
        assert_eq!(no_properties["additionalProperties"], json!(false));
    }

    #[test]
    fn strict_json_schema_reaches_tuple_positions() {
        // `prefixItems` is draft 2020-12's tuple spelling and an array-valued
        // `items` is the older one; schemars uses them for a tuple field. A
        // node missed in either is a node the strict request is rejected for.
        let schema = json!({
            "type": "object",
            "properties": {
                "pair": {
                    "type": "array",
                    "prefixItems": [
                        {"type": "object", "properties": {"a": {"type": "string"}}},
                        {"type": "object", "properties": {"b": {"type": "string"}}},
                    ],
                },
                "legacy": {
                    "type": "array",
                    "items": [{"type": "object", "properties": {"c": {"type": "string"}}}],
                },
            },
        });

        let strict = strict_json_schema(&schema);

        for (key, index, field) in [("pair", 0, "a"), ("pair", 1, "b"), ("legacy", 0, "c")] {
            let node = &strict["properties"][key][if key == "pair" {
                "prefixItems"
            } else {
                "items"
            }][index];
            assert_eq!(node["required"], json!([field]), "{key}[{index}] required");
            assert_eq!(
                node["additionalProperties"],
                json!(false),
                "{key}[{index}] closed"
            );
        }
    }

    #[test]
    fn strict_json_schema_leaves_non_object_nodes_alone() {
        // A bare `true`/`false` subschema and a plain scalar node have no
        // properties to require, and rewriting them would corrupt the document.
        assert_eq!(strict_json_schema(&json!(true)), json!(true));
        let scalars = json!({
            "type": "object",
            "properties": {"n": {"type": "integer"}},
            "additionalProperties": true,
        });
        let strict = strict_json_schema(&scalars);
        assert_eq!(strict["properties"]["n"], json!({"type": "integer"}));
        // An explicit `additionalProperties: true` is left standing. Closing it
        // would be accepted by the provider and would drop every key the author
        // opened the object for — the same silent-loss shape as the map case
        // above. Refusal and a fallback to the cascade is the better outcome.
        assert_eq!(strict["additionalProperties"], json!(true));
    }

    #[test]
    fn force_additional_properties_false_keeps_bedrocks_shape() {
        // Regression guard for the traversal being shared with the Bedrock
        // Converse adapter: unlike `strict_json_schema` this must NOT touch
        // `required`, and must leave an explicit `additionalProperties: true`
        // where the schema author put it.
        let schema = json!({
            "type": "object",
            "required": ["a"],
            "properties": {
                "a": {"type": "string"},
                "nested": {"type": "object", "properties": {"b": {"type": "string"}}},
                "open": {"type": "object", "additionalProperties": true},
            },
            "anyOf": [{"type": "object", "properties": {"c": {"type": "string"}}}],
        });

        let out = force_additional_properties_false(&schema);

        assert_eq!(out["additionalProperties"], json!(false));
        assert_eq!(
            out["properties"]["nested"]["additionalProperties"],
            json!(false)
        );
        assert_eq!(
            out["properties"]["open"]["additionalProperties"],
            json!(true)
        );
        assert_eq!(out["anyOf"][0]["additionalProperties"], json!(false));
        // `required` untouched — Bedrock's native branch does not force it.
        assert_eq!(out["required"], json!(["a"]));
        assert!(out["properties"]["nested"].get("required").is_none());
    }

    #[test]
    fn schema_fingerprint_ignores_key_order_but_not_array_order() {
        // The workspace enables `serde_json/preserve_order`, so these two are
        // genuinely different `Map`s. They describe the same schema, and the
        // demotion memo must not probe the endpoint twice for them.
        let a = json!({"type": "object", "properties": {"x": {"type": "string"}}});
        let b = json!({"properties": {"x": {"type": "string"}}, "type": "object"});
        assert_eq!(schema_fingerprint(&a), schema_fingerprint(&b));

        // `required` is a set, so its order carries no meaning: two spellings of
        // the same schema must share one memo entry rather than each taking its
        // own trip down the demotion ladder.
        let c = json!({"required": ["x", "y"]});
        let d = json!({"required": ["y", "x"]});
        assert_eq!(schema_fingerprint(&c), schema_fingerprint(&d));

        // Every other array keeps its order, because in JSON Schema that order
        // is significant — `anyOf` branch precedence, `prefixItems` positions.
        let e = json!({"prefixItems": [{"type": "string"}, {"type": "number"}]});
        let f = json!({"prefixItems": [{"type": "number"}, {"type": "string"}]});
        assert_ne!(schema_fingerprint(&e), schema_fingerprint(&f));

        // Normalising `required` must not make it collide with a same-named key
        // holding the same strings in another position.
        assert_ne!(
            schema_fingerprint(&json!({"required": ["x"]})),
            schema_fingerprint(&json!({"enum": ["x"]})),
        );

        // Different values of the same shape must not collide.
        assert_ne!(
            schema_fingerprint(&json!({"n": 0})),
            schema_fingerprint(&json!({"n": "0"})),
        );
        assert_ne!(
            schema_fingerprint(&json!({"n": null})),
            schema_fingerprint(&json!({"n": false})),
        );
    }
}
