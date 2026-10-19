//! Runtime custom graph models compiled from Pydantic-compatible JSON Schema.
//!
//! Python can manufacture `DataPoint` subclasses at runtime. Rust cannot, so
//! this module compiles the same serialized schema boundary into a small
//! descriptor and materializes LLM answers into an arena of dynamic nodes and
//! field relationships.

use std::collections::{HashMap, HashSet};

use cognee_models::DataPoint;
use serde_json::{Map, Value};
use thiserror::Error;
use uuid::Uuid;

const DATA_POINT_FIELDS: &[&str] = &[
    "id",
    "created_at",
    "updated_at",
    "ontology_valid",
    "version",
    "topological_rank",
    "metadata",
    "type",
    "belongs_to_set",
    "source_pipeline",
    "source_task",
    "source_node_set",
    "source_user",
    "source_content_hash",
    "feedback_weight",
    "importance_weight",
    "valid_to",
];

/// A compiled custom graph model.
#[derive(Debug, Clone, PartialEq)]
pub struct DynamicGraphSchema {
    root: String,
    models: HashMap<String, DynamicModelSchema>,
    llm_schema: Value,
}

/// One DataPoint-derived type declared by a dynamic schema.
#[derive(Debug, Clone, PartialEq)]
pub struct DynamicModelSchema {
    /// Runtime DataPoint type name.
    pub name: String,
    /// Fields whose values determine the stable node UUID.
    pub identity_fields: Vec<String>,
    /// Scalar fields written to separate vector collections.
    pub index_fields: Vec<String>,
    /// Whether this type is a structural container rather than a stored node.
    pub transparent: bool,
    fields: Vec<DynamicFieldSchema>,
}

#[derive(Debug, Clone, PartialEq)]
struct DynamicFieldSchema {
    name: String,
    required: bool,
    default: Option<Value>,
    schema: Value,
    kind: DynamicFieldKind,
}

#[derive(Debug, Clone, PartialEq)]
enum DynamicFieldKind {
    Scalar,
    Relationship { target: String, many: bool },
}

/// A runtime DataPoint produced from a custom model answer.
#[derive(Debug, Clone, PartialEq)]
pub struct DynamicDataPoint {
    /// Standard storage metadata and provenance fields.
    pub base: DataPoint,
    /// Custom scalar properties belonging to this node.
    pub properties: Map<String, Value>,
    /// Fields to vector-index, copied from the compiled model.
    pub index_fields: Vec<String>,
    /// A transparent node is replaced by its relationship children at storage.
    pub transparent: bool,
}

impl DynamicDataPoint {
    /// Serialize to the flat node shape accepted by every graph adapter.
    pub fn to_graph_value(&self) -> Value {
        let mut value = match serde_json::to_value(&self.base) {
            Ok(Value::Object(map)) => map,
            _ => Map::new(),
        };
        value.extend(self.properties.clone());
        Value::Object(value)
    }
}

/// A field relationship between two dynamic DataPoints.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DynamicRelationship {
    pub source: Uuid,
    pub target: Uuid,
    pub field_name: String,
}

/// One custom-model answer materialized as runtime DataPoints.
#[derive(Debug, Clone, PartialEq)]
pub struct DynamicGraph {
    pub root: Uuid,
    pub nodes: Vec<DynamicDataPoint>,
    pub relationships: Vec<DynamicRelationship>,
}

/// Invalid schema or response errors detected before persistence.
#[derive(Debug, Error, Clone, PartialEq)]
pub enum DynamicGraphError {
    #[error("graph schema must be a JSON object")]
    SchemaNotObject,
    #[error("graph schema is missing a non-empty top-level `title`")]
    MissingTitle,
    #[error("external graph schema reference `{0}` is not allowed")]
    ExternalReference(String),
    #[error("graph schema reference `{0}` does not name a declared model")]
    UnknownReference(String),
    #[error("graph model `{model}` has no object `properties`")]
    MissingProperties { model: String },
    #[error("graph model `{model}` declares reserved DataPoint field `{field}`")]
    ReservedField { model: String, field: String },
    #[error("graph model `{model}` metadata `{kind}` names unknown scalar field `{field}`")]
    InvalidMetadataField {
        model: String,
        kind: &'static str,
        field: String,
    },
    #[error("custom graph answer for `{model}` must be an object")]
    AnswerNotObject { model: String },
    #[error("custom graph answer for `{model}` is missing required field `{field}`")]
    MissingRequiredField { model: String, field: String },
    #[error("custom graph answer field `{model}.{field}` must be {expected}")]
    InvalidFieldShape {
        model: String,
        field: String,
        expected: &'static str,
    },
    #[error("custom graph answer field `{model}.{field}` violates its schema: {reason}")]
    InvalidScalarValue {
        model: String,
        field: String,
        reason: String,
    },
}

impl DynamicGraphSchema {
    /// Compile the Pydantic-compatible JSON Schema used by Python's HTTP and
    /// dataset-configuration boundary.
    pub fn compile(schema: &Value) -> Result<Self, DynamicGraphError> {
        reject_external_references(schema)?;
        let root_obj = schema
            .as_object()
            .ok_or(DynamicGraphError::SchemaNotObject)?;
        let definitions = root_obj.get("$defs").and_then(Value::as_object);
        let root_definition = root_obj
            .get("$ref")
            .and_then(Value::as_str)
            .map(|reference| reference_target_from_definitions(reference, definitions))
            .transpose()?;
        let root = root_definition
            .clone()
            .or_else(|| {
                root_obj
                    .get("title")
                    .and_then(Value::as_str)
                    .filter(|title| !title.is_empty())
                    .map(str::to_string)
            })
            .ok_or(DynamicGraphError::MissingTitle)?;
        let root_schema = root_definition
            .as_ref()
            .and_then(|name| definitions.and_then(|defs| defs.get(name)))
            .and_then(Value::as_object)
            .unwrap_or(root_obj);

        let mut raw_models: HashMap<String, &Map<String, Value>> = HashMap::new();
        raw_models.insert(root.clone(), root_schema);
        if let Some(definitions) = definitions {
            for (name, definition) in definitions {
                if let Some(definition) = definition.as_object() {
                    raw_models.insert(name.clone(), definition);
                }
            }
        }

        let names: HashSet<String> = raw_models.keys().cloned().collect();
        let mut models = HashMap::new();
        for (name, model) in raw_models {
            models.insert(name.clone(), compile_model(&name, model, &names)?);
        }

        let mut llm_schema = schema.clone();
        strip_metadata_properties(&mut llm_schema);

        Ok(Self {
            root,
            models,
            llm_schema,
        })
    }

    /// JSON Schema sent to the LLM. DataPoint metadata is runtime policy, not
    /// content the model should invent.
    pub fn llm_schema(&self) -> &Value {
        &self.llm_schema
    }

    /// Compiled root type.
    pub fn root_model(&self) -> &DynamicModelSchema {
        // `compile` always inserts the root before constructing `Self`.
        &self.models[&self.root]
    }

    /// Materialize one structured-output response into dynamic DataPoints.
    pub fn materialize(
        &self,
        answer: &Value,
        dataset_id: Uuid,
    ) -> Result<DynamicGraph, DynamicGraphError> {
        let mut graph = Materializer::new(self, dataset_id);
        let root = graph.visit(&self.root, answer)?;
        Ok(DynamicGraph {
            root,
            nodes: graph.nodes.into_values().collect(),
            relationships: graph.relationships,
        })
    }
}

fn compile_model(
    name: &str,
    schema: &Map<String, Value>,
    model_names: &HashSet<String>,
) -> Result<DynamicModelSchema, DynamicGraphError> {
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| DynamicGraphError::MissingProperties {
            model: name.to_string(),
        })?;
    let required: HashSet<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let metadata = properties
        .get("metadata")
        .and_then(Value::as_object)
        .and_then(|metadata| metadata.get("default"))
        .and_then(Value::as_object);
    let identity_fields = string_list(metadata.and_then(|m| m.get("identity_fields")));
    let index_fields = string_list(metadata.and_then(|m| m.get("index_fields")));
    let transparent = metadata
        .and_then(|m| m.get("transparent"))
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut fields = Vec::new();
    for (field_name, field_schema) in properties {
        if field_name == "metadata" {
            continue;
        }
        if DATA_POINT_FIELDS.contains(&field_name.as_str()) {
            return Err(DynamicGraphError::ReservedField {
                model: name.to_string(),
                field: field_name.clone(),
            });
        }
        let kind =
            relationship_kind(field_schema, model_names)?.unwrap_or(DynamicFieldKind::Scalar);
        let default = field_schema.get("default").cloned();
        fields.push(DynamicFieldSchema {
            name: field_name.clone(),
            required: required.contains(field_name.as_str()),
            default,
            schema: field_schema.clone(),
            kind,
        });
    }

    for (kind, names) in [
        ("identity_fields", &identity_fields),
        ("index_fields", &index_fields),
    ] {
        for field in names {
            let valid = fields.iter().any(|candidate| {
                candidate.name == *field && candidate.kind == DynamicFieldKind::Scalar
            });
            if !valid {
                return Err(DynamicGraphError::InvalidMetadataField {
                    model: name.to_string(),
                    kind,
                    field: field.clone(),
                });
            }
        }
    }

    Ok(DynamicModelSchema {
        name: name.to_string(),
        identity_fields,
        index_fields,
        transparent,
        fields,
    })
}

fn relationship_kind(
    schema: &Value,
    model_names: &HashSet<String>,
) -> Result<Option<DynamicFieldKind>, DynamicGraphError> {
    if let Some(variants) = schema.get("anyOf").and_then(Value::as_array) {
        let non_null: Vec<&Value> = variants
            .iter()
            .filter(|variant| variant.get("type").and_then(Value::as_str) != Some("null"))
            .collect();
        if non_null.len() == 1 {
            return relationship_kind(non_null[0], model_names);
        }
    }
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        return Ok(Some(DynamicFieldKind::Relationship {
            target: reference_target(reference, model_names)?,
            many: false,
        }));
    }
    if schema.get("type").and_then(Value::as_str) == Some("array")
        && let Some(reference) = schema
            .get("items")
            .and_then(|items| items.get("$ref"))
            .and_then(Value::as_str)
    {
        return Ok(Some(DynamicFieldKind::Relationship {
            target: reference_target(reference, model_names)?,
            many: true,
        }));
    }
    Ok(None)
}

fn reference_target_from_definitions(
    reference: &str,
    definitions: Option<&Map<String, Value>>,
) -> Result<String, DynamicGraphError> {
    let Some(target) = reference.strip_prefix("#/$defs/") else {
        return Err(DynamicGraphError::ExternalReference(reference.to_string()));
    };
    if !definitions.is_some_and(|definitions| definitions.contains_key(target)) {
        return Err(DynamicGraphError::UnknownReference(reference.to_string()));
    }
    Ok(target.to_string())
}

fn reference_target(
    reference: &str,
    model_names: &HashSet<String>,
) -> Result<String, DynamicGraphError> {
    let Some(target) = reference.strip_prefix("#/$defs/") else {
        return Err(DynamicGraphError::ExternalReference(reference.to_string()));
    };
    if !model_names.contains(target) {
        return Err(DynamicGraphError::UnknownReference(reference.to_string()));
    }
    Ok(target.to_string())
}

fn reject_external_references(value: &Value) -> Result<(), DynamicGraphError> {
    match value {
        Value::Object(map) => {
            if let Some(reference) = map.get("$ref").and_then(Value::as_str)
                && !reference.starts_with('#')
            {
                return Err(DynamicGraphError::ExternalReference(reference.to_string()));
            }
            for value in map.values() {
                reject_external_references(value)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                reject_external_references(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn strip_metadata_properties(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(properties) = map.get_mut("properties").and_then(Value::as_object_mut) {
                properties.remove("metadata");
            }
            if let Some(required) = map.get_mut("required").and_then(Value::as_array_mut) {
                required.retain(|field| field.as_str() != Some("metadata"));
            }
            for child in map.values_mut() {
                strip_metadata_properties(child);
            }
        }
        Value::Array(values) => {
            for child in values {
                strip_metadata_properties(child);
            }
        }
        _ => {}
    }
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

struct Materializer<'a> {
    schema: &'a DynamicGraphSchema,
    dataset_id: Uuid,
    nodes: HashMap<Uuid, DynamicDataPoint>,
    relationships: Vec<DynamicRelationship>,
}

impl<'a> Materializer<'a> {
    fn new(schema: &'a DynamicGraphSchema, dataset_id: Uuid) -> Self {
        Self {
            schema,
            dataset_id,
            nodes: HashMap::new(),
            relationships: Vec::new(),
        }
    }

    fn visit(&mut self, model_name: &str, value: &Value) -> Result<Uuid, DynamicGraphError> {
        let model = &self.schema.models[model_name];
        let object = value
            .as_object()
            .ok_or_else(|| DynamicGraphError::AnswerNotObject {
                model: model_name.to_string(),
            })?;
        let mut properties = Map::new();
        let mut relationship_values = Vec::new();

        for field in &model.fields {
            let value = object.get(&field.name).or(field.default.as_ref());
            let Some(value) = value else {
                if field.required {
                    return Err(DynamicGraphError::MissingRequiredField {
                        model: model_name.to_string(),
                        field: field.name.clone(),
                    });
                }
                continue;
            };
            match &field.kind {
                DynamicFieldKind::Scalar => {
                    validate_scalar_value(value, &field.schema).map_err(|reason| {
                        DynamicGraphError::InvalidScalarValue {
                            model: model_name.to_string(),
                            field: field.name.clone(),
                            reason,
                        }
                    })?;
                    properties.insert(field.name.clone(), value.clone());
                }
                DynamicFieldKind::Relationship { target, many } => {
                    relationship_values.push((field.name.clone(), target.clone(), *many, value));
                }
            }
        }

        let id = dynamic_id(model, &properties);
        let mut base = DataPoint::new(model.name.clone(), Some(self.dataset_id));
        base.id = id;
        base.metadata.insert(
            "index_fields".to_string(),
            Value::Array(
                model
                    .index_fields
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        );
        if !model.identity_fields.is_empty() {
            base.metadata.insert(
                "identity_fields".to_string(),
                Value::Array(
                    model
                        .identity_fields
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                ),
            );
        }
        if model.transparent {
            base.metadata
                .insert("transparent".to_string(), Value::Bool(true));
        }
        self.nodes.entry(id).or_insert_with(|| DynamicDataPoint {
            base,
            properties,
            index_fields: model.index_fields.clone(),
            transparent: model.transparent,
        });

        for (field_name, target_model, many, value) in relationship_values {
            if value.is_null() {
                continue;
            }
            if many {
                let values =
                    value
                        .as_array()
                        .ok_or_else(|| DynamicGraphError::InvalidFieldShape {
                            model: model_name.to_string(),
                            field: field_name.clone(),
                            expected: "an array",
                        })?;
                for child in values {
                    let target = self.visit(&target_model, child)?;
                    self.relationships.push(DynamicRelationship {
                        source: id,
                        target,
                        field_name: field_name.clone(),
                    });
                }
            } else {
                let target = self.visit(&target_model, value)?;
                self.relationships.push(DynamicRelationship {
                    source: id,
                    target,
                    field_name,
                });
            }
        }
        Ok(id)
    }
}

fn dynamic_id(model: &DynamicModelSchema, properties: &Map<String, Value>) -> Uuid {
    if model.identity_fields.is_empty() {
        return Uuid::new_v4();
    }
    let Some(values) = model
        .identity_fields
        .iter()
        .map(|field| properties.get(field).map(normalize_identity_value))
        .collect::<Option<Vec<_>>>()
    else {
        return Uuid::new_v4();
    };
    Uuid::new_v5(
        &Uuid::NAMESPACE_OID,
        format!("{}:{}", model.name, values.join("|")).as_bytes(),
    )
}

fn normalize_identity_value(value: &Value) -> String {
    match value {
        Value::String(raw) => raw.to_lowercase().replace(' ', "_").replace('\'', ""),
        _ => python_string(value),
    }
}

fn python_string(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'")),
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(python_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Object(values) => format!(
            "{{{}}}",
            values
                .iter()
                .map(|(key, value)| format!(
                    "{}: {}",
                    python_string(&Value::String(key.clone())),
                    python_string(value)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn validate_scalar_value(value: &Value, schema: &Value) -> Result<(), String> {
    if let Some(variants) = schema.get("anyOf").and_then(Value::as_array) {
        if variants
            .iter()
            .any(|variant| validate_scalar_value(value, variant).is_ok())
        {
            return Ok(());
        }
        return Err("does not match any allowed variant".to_string());
    }
    if let Some(expected) = schema.get("type").and_then(Value::as_str) {
        let matches = match expected {
            "null" => value.is_null(),
            "boolean" => value.is_boolean(),
            "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
            "number" => value.is_number(),
            "string" => value.is_string(),
            "array" => value.is_array(),
            "object" => value.is_object(),
            _ => true,
        };
        if !matches {
            return Err(format!("expected {expected}"));
        }
    }
    if let Some(constant) = schema.get("const")
        && value != constant
    {
        return Err("does not equal the declared constant".to_string());
    }
    if let Some(choices) = schema.get("enum").and_then(Value::as_array)
        && !choices.contains(value)
    {
        return Err("is not one of the declared enum values".to_string());
    }
    if let (Some(text), Some(minimum)) = (
        value.as_str(),
        schema.get("minLength").and_then(Value::as_u64),
    ) && text.chars().count() < minimum as usize
    {
        return Err(format!("is shorter than minLength {minimum}"));
    }
    if let (Some(text), Some(maximum)) = (
        value.as_str(),
        schema.get("maxLength").and_then(Value::as_u64),
    ) && text.chars().count() > maximum as usize
    {
        return Err(format!("is longer than maxLength {maximum}"));
    }
    if let Some(items) = value.as_array()
        && let Some(item_schema) = schema.get("items")
    {
        for item in items {
            validate_scalar_value(item, item_schema)?;
        }
    }
    if let Some(object) = value.as_object() {
        let properties = schema.get("properties").and_then(Value::as_object);
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for field in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(field) {
                    return Err(format!("is missing required property `{field}`"));
                }
            }
        }
        if let Some(properties) = properties {
            for (field, field_value) in object {
                if let Some(field_schema) = properties.get(field) {
                    validate_scalar_value(field_value, field_schema)?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use serde_json::json;

    use super::*;

    fn schema() -> Value {
        json!({
            "$defs": {
                "Person": {
                    "title": "Person",
                    "type": "object",
                    "properties": {
                        "name": {"type": "string"},
                        "metadata": {"type": "object", "default": {
                            "index_fields": ["name"], "identity_fields": ["name"]
                        }}
                    },
                    "required": ["name"]
                }
            },
            "title": "Company",
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "employees": {"type": "array", "items": {"$ref": "#/$defs/Person"}},
                "metadata": {"type": "object", "default": {
                    "index_fields": ["name"], "identity_fields": ["name"], "transparent": true
                }}
            },
            "required": ["name", "employees"]
        })
    }

    #[test]
    fn compiles_python_json_schema_and_strips_metadata_for_llm() {
        let compiled = DynamicGraphSchema::compile(&schema()).expect("schema should compile");
        assert_eq!(compiled.root_model().name, "Company");
        assert!(compiled.root_model().transparent);
        assert!(
            compiled.llm_schema()["properties"]
                .get("metadata")
                .is_none()
        );
        assert!(
            compiled.llm_schema()["$defs"]["Person"]["properties"]
                .get("metadata")
                .is_none()
        );
    }

    #[test]
    fn rejects_external_references() {
        let error = DynamicGraphSchema::compile(&json!({
            "title": "Bad",
            "properties": {"child": {"$ref": "https://example.com/schema.json"}}
        }))
        .expect_err("external refs must be rejected");
        assert_eq!(
            error,
            DynamicGraphError::ExternalReference("https://example.com/schema.json".to_string())
        );
    }

    #[test]
    fn materializes_nodes_edges_and_python_identity_ids() {
        let compiled = DynamicGraphSchema::compile(&schema()).expect("schema should compile");
        let graph = compiled
            .materialize(
                &json!({
                    "name": "Acme Corp",
                    "employees": [{"name": "Alice"}, {"name": "Bob"}]
                }),
                Uuid::nil(),
            )
            .expect("answer should materialize");
        assert_eq!(graph.nodes.len(), 3);
        assert_eq!(graph.relationships.len(), 2);
        assert_eq!(
            graph.root,
            Uuid::new_v5(&Uuid::NAMESPACE_OID, b"Company:acme_corp")
        );
        assert!(
            graph.nodes.iter().any(|node| {
                node.base.id == Uuid::new_v5(&Uuid::NAMESPACE_OID, b"Person:alice")
            })
        );
    }

    #[test]
    fn validates_required_fields() {
        let compiled = DynamicGraphSchema::compile(&schema()).expect("schema should compile");
        let error = compiled
            .materialize(&json!({"employees": []}), Uuid::nil())
            .expect_err("missing name should fail");
        assert_eq!(
            error,
            DynamicGraphError::MissingRequiredField {
                model: "Company".to_string(),
                field: "name".to_string(),
            }
        );
    }

    #[test]
    fn materializes_optional_single_and_list_relationships() {
        let schema = json!({
            "$defs": {
                "Child": {
                    "title": "Child", "type": "object",
                    "properties": {"name": {"type": "string"}},
                    "required": ["name"]
                }
            },
            "title": "Root", "type": "object",
            "properties": {
                "child": {"anyOf": [{"$ref": "#/$defs/Child"}, {"type": "null"}]},
                "children": {"anyOf": [
                    {"type": "array", "items": {"$ref": "#/$defs/Child"}},
                    {"type": "null"}
                ]}
            }
        });
        let graph = DynamicGraphSchema::compile(&schema)
            .expect("optional relationships should compile")
            .materialize(
                &json!({"child": {"name": "one"}, "children": [{"name": "two"}]}),
                Uuid::nil(),
            )
            .expect("optional relationships should materialize");
        assert_eq!(graph.nodes.len(), 3);
        assert_eq!(graph.relationships.len(), 2);
    }

    #[test]
    fn resolves_top_level_ref_and_recursive_model() {
        let schema = json!({
            "$defs": {
                "Node": {
                    "title": "Node", "type": "object",
                    "properties": {
                        "name": {"type": "string"},
                        "next": {"anyOf": [{"$ref": "#/$defs/Node"}, {"type": "null"}]}
                    },
                    "required": ["name", "next"]
                }
            },
            "$ref": "#/$defs/Node"
        });
        let compiled = DynamicGraphSchema::compile(&schema).expect("root ref should compile");
        assert_eq!(compiled.root_model().name, "Node");
        let graph = compiled
            .materialize(&json!({"name": "first", "next": null}), Uuid::nil())
            .expect("finite recursive answer should materialize");
        assert_eq!(graph.nodes.len(), 1);
    }

    #[test]
    fn identity_values_match_python_stringification() {
        assert_eq!(normalize_identity_value(&json!(true)), "True");
        assert_eq!(normalize_identity_value(&Value::Null), "None");
        assert_eq!(normalize_identity_value(&json!([1, "A"])), "[1, 'A']");
        assert_eq!(
            normalize_identity_value(&json!({"enabled": false})),
            "{'enabled': False}"
        );
    }

    #[test]
    fn validates_scalar_schema_constraints() {
        let schema = json!({
            "title": "Constrained", "type": "object",
            "properties": {
                "kind": {"type": "string", "enum": ["allowed"]},
                "count": {"type": "integer"}
            },
            "required": ["kind", "count"]
        });
        let compiled = DynamicGraphSchema::compile(&schema).expect("schema should compile");
        let error = compiled
            .materialize(&json!({"kind": "other", "count": 1}), Uuid::nil())
            .expect_err("enum violation should fail");
        assert!(matches!(
            error,
            DynamicGraphError::InvalidScalarValue { .. }
        ));
        let error = compiled
            .materialize(&json!({"kind": "allowed", "count": "one"}), Uuid::nil())
            .expect_err("type violation should fail");
        assert!(matches!(
            error,
            DynamicGraphError::InvalidScalarValue { .. }
        ));
    }
}
