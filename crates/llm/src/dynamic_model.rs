//! Dynamic graph model for cross-SDK schema sharing.
//!
//! Provides [`DynamicGraphModel`], a runtime representation of a graph model's
//! JSON schema. This enables schema exchange between the Python and Rust cognee
//! SDKs without requiring compiled types on both sides.
//!
//! The Python SDK can serialize a Pydantic model to JSON schema, send it to the
//! Rust SDK as a [`DynamicGraphModel`], and vice versa. This mirrors Python's
//! `graph_model_to_graph_schema()` / `graph_schema_to_graph_model()` from
//! `cognee/shared/graph_model_utils.py`.
//!
//! # Usage
//!
//! ```
//! use cognee_llm::DynamicGraphModel;
//! use schemars::JsonSchema;
//! use serde::{Deserialize, Serialize};
//!
//! // From a Rust type
//! #[derive(Serialize, Deserialize, JsonSchema, Clone)]
//! struct MyModel {
//!     entities: Vec<String>,
//! }
//!
//! let model = DynamicGraphModel::from_type::<MyModel>("MyModel");
//! assert_eq!(model.name, "MyModel");
//!
//! // From a pre-existing JSON schema (e.g., received from Python)
//! let schema = serde_json::json!({
//!     "type": "object",
//!     "properties": {
//!         "name": { "type": "string" }
//!     },
//!     "required": ["name"]
//! });
//! let model = DynamicGraphModel::from_schema("ExternalModel", schema);
//! ```

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::schema::generate_json_schema;

/// A runtime representation of a graph model's JSON schema.
///
/// Stores the JSON schema for a graph model so it can be serialized, transmitted
/// between SDKs, and used for LLM structured output without requiring the
/// concrete Rust type at runtime.
///
/// # Fields
/// * `name` - Human-readable name for the model (e.g., "KnowledgeGraph", "ProgrammingLanguage")
/// * `schema` - The JSON schema as a `serde_json::Value`
/// * `description` - Optional description of what the model represents
/// * `source` - Optional source identifier (e.g., "python-sdk", "rust-sdk")
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicGraphModel {
    /// Human-readable name for the model.
    pub name: String,

    /// The JSON schema describing the model's structure.
    pub schema: Value,

    /// Optional description of what the model represents.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Optional source identifier (e.g., "python-sdk", "rust-sdk").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl DynamicGraphModel {
    /// Create a [`DynamicGraphModel`] from a Rust type that implements [`JsonSchema`].
    ///
    /// Generates the JSON schema at runtime via `schemars` and stores it alongside
    /// the given name. The `source` field is automatically set to `"rust-sdk"`.
    ///
    /// # Arguments
    /// * `name` - Human-readable name for the model
    ///
    /// # Example
    /// ```
    /// use cognee_llm::DynamicGraphModel;
    /// use schemars::JsonSchema;
    /// use serde::{Deserialize, Serialize};
    ///
    /// #[derive(Serialize, Deserialize, JsonSchema, Clone)]
    /// struct PersonGraph {
    ///     people: Vec<String>,
    ///     relationships: Vec<(String, String)>,
    /// }
    ///
    /// let model = DynamicGraphModel::from_type::<PersonGraph>("PersonGraph");
    /// assert_eq!(model.name, "PersonGraph");
    /// assert_eq!(model.source.as_deref(), Some("rust-sdk"));
    /// ```
    pub fn from_type<T: JsonSchema>(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            schema: generate_json_schema::<T>(),
            description: None,
            source: Some("rust-sdk".to_string()),
        }
    }

    /// Create a [`DynamicGraphModel`] from a pre-existing JSON schema.
    ///
    /// Use this when receiving a schema from an external source (e.g., the Python SDK
    /// serialized a Pydantic model to JSON schema).
    ///
    /// # Arguments
    /// * `name` - Human-readable name for the model
    /// * `schema` - The JSON schema as a `serde_json::Value`
    ///
    /// # Example
    /// ```
    /// use cognee_llm::DynamicGraphModel;
    ///
    /// let schema = serde_json::json!({
    ///     "type": "object",
    ///     "properties": {
    ///         "name": { "type": "string" }
    ///     },
    ///     "required": ["name"]
    /// });
    /// let model = DynamicGraphModel::from_schema("ExternalModel", schema);
    /// assert_eq!(model.name, "ExternalModel");
    /// assert!(model.source.is_none());
    /// ```
    pub fn from_schema(name: impl Into<String>, schema: Value) -> Self {
        Self {
            name: name.into(),
            schema,
            description: None,
            source: None,
        }
    }

    /// Set an optional description on this model.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Set an optional source identifier on this model.
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// Check whether the schema has a `"properties"` key (i.e., looks like an object schema).
    ///
    /// This is a lightweight structural check, not full JSON Schema validation.
    /// For full structural validation, deserialize with `serde_json::from_value::<T>()`
    /// which enforces all type constraints.
    pub fn has_properties(&self) -> bool {
        self.schema.get("properties").is_some()
    }

    /// Get the list of required field names from the schema, if any.
    ///
    /// Returns `None` if the schema has no `"required"` key. Returns `Some(vec)`
    /// with the field names otherwise.
    pub fn required_fields(&self) -> Option<Vec<&str>> {
        self.schema.get("required").and_then(|v| {
            v.as_array().map(|arr| {
                arr.iter()
                    .filter_map(|item| item.as_str())
                    .collect::<Vec<_>>()
            })
        })
    }

    /// Check whether a JSON value has all the required fields defined in this schema.
    ///
    /// This performs a lightweight check: it only verifies that required fields
    /// exist as keys in the JSON object. It does **not** validate types or nested
    /// structures. For full structural validation, use `serde_json::from_value::<T>()`.
    ///
    /// Returns `Ok(())` if all required fields are present (or if there are no
    /// required fields). Returns `Err` with a message listing missing fields.
    pub fn check_required_fields(&self, instance: &Value) -> Result<(), String> {
        let required = match self.required_fields() {
            Some(fields) => fields,
            None => return Ok(()),
        };

        let obj = instance.as_object().ok_or_else(|| {
            format!(
                "Expected a JSON object for model '{}', got {}",
                self.name,
                value_type_name(instance)
            )
        })?;

        let missing: Vec<&str> = required
            .iter()
            .filter(|field| !obj.contains_key(**field))
            .copied()
            .collect();

        if missing.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "Model '{}' is missing required fields: {}",
                self.name,
                missing.join(", ")
            ))
        }
    }
}

// ─── graph_schema_to_graph_model ──────────────────────────────────────────────

/// Errors emitted by [`graph_schema_to_graph_model`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum GraphModelError {
    #[error("graph schema must be a JSON object, got {0}")]
    NotAnObject(&'static str),

    #[error("graph schema is missing required key `{0}`")]
    MissingKey(&'static str),

    #[error("graph schema field `{0}` must be a list, got {1}")]
    NotAList(&'static str, &'static str),
}

/// Validate a JSON value against the canonical graph-model shape.
///
/// Mirrors Python's
/// [`graph_schema_to_graph_model`](https://github.com/topoteretes/cognee/blob/main/cognee/shared/graph_model_utils.py)
/// in *spirit only* — the Rust port does not generate runtime Pydantic classes
/// (the LLM-router handler only ever uses the error path to distinguish
/// "schema invalid" → 409 from "JSON parse error" → 422).
///
/// The validation rules are:
/// - Top-level value must be a JSON object.
/// - The object must carry an `entity_types` array.
/// - The object must carry a `relationship_types` array.
///
/// On success returns `Ok(())` (the success value is unused by the handler).
pub fn graph_schema_to_graph_model(value: &Value) -> Result<(), GraphModelError> {
    let obj = match value {
        Value::Object(map) => map,
        _ => return Err(GraphModelError::NotAnObject(value_type_name(value))),
    };

    let entity_types = obj
        .get("entity_types")
        .ok_or(GraphModelError::MissingKey("entity_types"))?;
    if !entity_types.is_array() {
        return Err(GraphModelError::NotAList(
            "entity_types",
            value_type_name(entity_types),
        ));
    }

    let relationship_types = obj
        .get("relationship_types")
        .ok_or(GraphModelError::MissingKey("relationship_types"))?;
    if !relationship_types.is_array() {
        return Err(GraphModelError::NotAList(
            "relationship_types",
            value_type_name(relationship_types),
        ));
    }

    Ok(())
}

/// Return a human-readable name for a JSON value type.
fn value_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
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

    /// A KnowledgeGraph-like model for testing.
    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
    struct TestNode {
        id: String,
        name: String,
        #[serde(rename = "type")]
        node_type: String,
        description: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
    struct TestEdge {
        source_node_id: String,
        target_node_id: String,
        relationship_name: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
    struct TestKnowledgeGraph {
        #[serde(default)]
        nodes: Vec<TestNode>,
        #[serde(default)]
        edges: Vec<TestEdge>,
    }

    #[test]
    fn test_from_type_produces_valid_schema() {
        let model = DynamicGraphModel::from_type::<TestKnowledgeGraph>("KnowledgeGraph");

        assert_eq!(model.name, "KnowledgeGraph");
        assert_eq!(model.source.as_deref(), Some("rust-sdk"));
        assert!(model.description.is_none());

        // Schema should be an object with standard JSON Schema keys
        assert!(model.schema.is_object());

        // Should have "properties" containing "nodes" and "edges"
        let props = &model.schema["properties"];
        assert!(props.is_object(), "schema should have 'properties'");
        assert!(
            props.get("nodes").is_some(),
            "schema should have 'nodes' property"
        );
        assert!(
            props.get("edges").is_some(),
            "schema should have 'edges' property"
        );
    }

    #[test]
    fn test_from_type_has_type_object() {
        let model = DynamicGraphModel::from_type::<TestKnowledgeGraph>("KnowledgeGraph");
        assert_eq!(model.schema["type"], "object");
    }

    #[test]
    fn test_from_schema_with_arbitrary_schema() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "language": { "type": "string" },
                "version": { "type": "number" }
            },
            "required": ["language"]
        });

        let model = DynamicGraphModel::from_schema("ProgrammingLanguage", schema.clone());

        assert_eq!(model.name, "ProgrammingLanguage");
        assert!(model.source.is_none());
        assert_eq!(model.schema, schema);
    }

    #[test]
    fn test_round_trip_serialization() {
        let original = DynamicGraphModel::from_type::<TestKnowledgeGraph>("KnowledgeGraph")
            .with_description("A knowledge graph model")
            .with_source("test-suite");

        // Serialize to JSON string
        let json_str = serde_json::to_string(&original).unwrap();

        // Deserialize back
        let restored: DynamicGraphModel = serde_json::from_str(&json_str).unwrap();

        assert_eq!(restored.name, original.name);
        assert_eq!(restored.schema, original.schema);
        assert_eq!(restored.description, original.description);
        assert_eq!(restored.source, original.source);
    }

    #[test]
    fn test_round_trip_through_value() {
        let original = DynamicGraphModel::from_type::<TestKnowledgeGraph>("KnowledgeGraph");

        // Serialize to Value and back
        let value = serde_json::to_value(&original).unwrap();
        let restored: DynamicGraphModel = serde_json::from_value(value).unwrap();

        assert_eq!(restored.name, original.name);
        assert_eq!(restored.schema, original.schema);
    }

    #[test]
    fn test_has_properties() {
        let model = DynamicGraphModel::from_type::<TestKnowledgeGraph>("KnowledgeGraph");
        assert!(model.has_properties());

        let empty = DynamicGraphModel::from_schema("Empty", serde_json::json!({}));
        assert!(!empty.has_properties());
    }

    #[test]
    fn test_required_fields() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "age": { "type": "integer" }
            },
            "required": ["name", "age"]
        });
        let model = DynamicGraphModel::from_schema("Person", schema);

        let required = model.required_fields().unwrap();
        assert_eq!(required, vec!["name", "age"]);
    }

    #[test]
    fn test_required_fields_none_when_absent() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" }
            }
        });
        let model = DynamicGraphModel::from_schema("Flexible", schema);
        assert!(model.required_fields().is_none());
    }

    #[test]
    fn test_check_required_fields_pass() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "value": { "type": "number" }
            },
            "required": ["name", "value"]
        });
        let model = DynamicGraphModel::from_schema("Item", schema);

        let instance = serde_json::json!({
            "name": "test",
            "value": 42,
            "extra": true
        });
        assert!(model.check_required_fields(&instance).is_ok());
    }

    #[test]
    fn test_check_required_fields_missing() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "value": { "type": "number" }
            },
            "required": ["name", "value"]
        });
        let model = DynamicGraphModel::from_schema("Item", schema);

        let instance = serde_json::json!({ "name": "test" });
        let err = model.check_required_fields(&instance).unwrap_err();
        assert!(
            err.contains("value"),
            "Error should mention missing field: {err}"
        );
    }

    #[test]
    fn test_check_required_fields_not_object() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["name"]
        });
        let model = DynamicGraphModel::from_schema("Item", schema);

        let instance = serde_json::json!("not an object");
        let err = model.check_required_fields(&instance).unwrap_err();
        assert!(
            err.contains("Expected a JSON object"),
            "Error should mention type mismatch: {err}"
        );
    }

    #[test]
    fn test_check_required_fields_no_required() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "name": { "type": "string" } }
        });
        let model = DynamicGraphModel::from_schema("Flexible", schema);

        // Any object should pass when there are no required fields
        let instance = serde_json::json!({});
        assert!(model.check_required_fields(&instance).is_ok());
    }

    #[test]
    fn test_builder_methods() {
        let model = DynamicGraphModel::from_schema("Test", serde_json::json!({}))
            .with_description("A test model")
            .with_source("python-sdk");

        assert_eq!(model.description.as_deref(), Some("A test model"));
        assert_eq!(model.source.as_deref(), Some("python-sdk"));
    }

    #[test]
    fn test_graph_schema_to_graph_model_accepts_canonical_shape() {
        let value = serde_json::json!({
            "entity_types": [{"name": "Person"}],
            "relationship_types": [{"name": "WORKS_AT"}],
        });
        assert!(graph_schema_to_graph_model(&value).is_ok());
    }

    #[test]
    fn test_graph_schema_to_graph_model_rejects_non_object() {
        let value = serde_json::json!([]);
        let err = graph_schema_to_graph_model(&value).unwrap_err();
        assert!(matches!(err, GraphModelError::NotAnObject(_)));
    }

    #[test]
    fn test_graph_schema_to_graph_model_missing_entity_types() {
        let value = serde_json::json!({"relationship_types": []});
        let err = graph_schema_to_graph_model(&value).unwrap_err();
        assert_eq!(err, GraphModelError::MissingKey("entity_types"));
    }

    #[test]
    fn test_graph_schema_to_graph_model_missing_relationship_types() {
        let value = serde_json::json!({"entity_types": []});
        let err = graph_schema_to_graph_model(&value).unwrap_err();
        assert_eq!(err, GraphModelError::MissingKey("relationship_types"));
    }

    #[test]
    fn test_graph_schema_to_graph_model_entity_types_must_be_array() {
        let value = serde_json::json!({
            "entity_types": "wrong",
            "relationship_types": [],
        });
        let err = graph_schema_to_graph_model(&value).unwrap_err();
        assert!(matches!(err, GraphModelError::NotAList("entity_types", _)));
    }

    #[test]
    fn test_skip_serializing_none_fields() {
        let model = DynamicGraphModel::from_schema("Minimal", serde_json::json!({}));
        let json = serde_json::to_value(&model).unwrap();
        let obj = json.as_object().unwrap();

        // description and source should not be present when None
        assert!(!obj.contains_key("description"));
        assert!(!obj.contains_key("source"));

        // name and schema should always be present
        assert!(obj.contains_key("name"));
        assert!(obj.contains_key("schema"));
    }
}
