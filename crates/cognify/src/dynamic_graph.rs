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
use tracing::warn;
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
    /// `targets` holds every model a union field may hold, in declaration
    /// order; a plain model reference has exactly one.
    Relationship {
        targets: Vec<String>,
        many: bool,
    },
}

/// A runtime DataPoint produced from a custom model answer.
#[derive(Debug, Clone, PartialEq)]
pub struct DynamicDataPoint {
    /// Per-answer object identity used while resolving transparent containers.
    pub occurrence_id: Uuid,
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
    pub root_occurrence: Uuid,
    pub nodes: Vec<DynamicDataPoint>,
    pub relationships: Vec<DynamicRelationship>,
}

/// The graph artifacts produced by the Python-compatible storage walk.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredDynamicGraph {
    pub nodes: Vec<DynamicDataPoint>,
    pub relationships: Vec<DynamicRelationship>,
}

impl DynamicGraph {
    /// Walk the runtime graph using Python's transparent-container rules.
    pub fn stored_graph(&self) -> StoredDynamicGraph {
        let node_map: HashMap<Uuid, &DynamicDataPoint> = self
            .nodes
            .iter()
            .map(|node| (node.occurrence_id, node))
            .collect();
        let mut walker = DynamicWalker {
            graph: self,
            node_map,
            nodes: Vec::new(),
            seen_node_ids: HashSet::new(),
            relationships: Vec::new(),
            seen_relationships: HashSet::new(),
            expanded: HashSet::new(),
        };
        walker.walk(self.root_occurrence, &mut HashSet::new());
        StoredDynamicGraph {
            nodes: walker.nodes.into_iter().cloned().collect(),
            relationships: walker.relationships,
        }
    }

    /// Chunk targets for Python's `direct` attachment mode. A stored root is
    /// the sole target; a transparent root is replaced by its stored children.
    pub fn direct_attachment_targets(&self) -> Vec<Uuid> {
        let node_map: HashMap<Uuid, &DynamicDataPoint> = self
            .nodes
            .iter()
            .map(|node| (node.occurrence_id, node))
            .collect();
        if node_map
            .get(&self.root_occurrence)
            .is_some_and(|node| !node.transparent)
        {
            return vec![self.root];
        }
        transparent_children(self, &node_map, self.root_occurrence, &mut HashSet::new())
            .into_iter()
            .filter_map(|occurrence| node_map.get(&occurrence).map(|node| node.base.id))
            .collect()
    }

    /// Chunk targets for Python's `all` attachment mode.
    pub fn all_attachment_targets(&self) -> Vec<Uuid> {
        self.stored_graph()
            .nodes
            .into_iter()
            .map(|node| node.base.id)
            .collect()
    }
}

struct DynamicWalker<'a> {
    graph: &'a DynamicGraph,
    node_map: HashMap<Uuid, &'a DynamicDataPoint>,
    nodes: Vec<&'a DynamicDataPoint>,
    seen_node_ids: HashSet<Uuid>,
    relationships: Vec<DynamicRelationship>,
    seen_relationships: HashSet<DynamicRelationship>,
    expanded: HashSet<Uuid>,
}

impl DynamicWalker<'_> {
    fn walk(&mut self, id: Uuid, active: &mut HashSet<Uuid>) {
        if self.expanded.contains(&id) || !active.insert(id) {
            return;
        }
        let Some(node) = self.node_map.get(&id).copied() else {
            active.remove(&id);
            return;
        };

        if node.transparent {
            if !node.properties.is_empty() {
                warn!(
                    node_type = %node.base.data_type,
                    fields = ?node.properties.keys().collect::<Vec<_>>(),
                    "dropping scalar properties from transparent custom graph node"
                );
            }
            active.remove(&id);
            let children = transparent_children(self.graph, &self.node_map, id, active);
            for child in children {
                self.walk(child, active);
            }
            self.expanded.insert(id);
            return;
        }

        if self.seen_node_ids.insert(node.base.id) {
            self.nodes.push(node);
        }
        let outgoing: Vec<DynamicRelationship> = self
            .graph
            .relationships
            .iter()
            .filter(|relationship| relationship.source == id)
            .cloned()
            .collect();
        for relationship in outgoing {
            let targets = if self
                .node_map
                .get(&relationship.target)
                .is_some_and(|target| target.transparent)
            {
                transparent_children(self.graph, &self.node_map, relationship.target, active)
            } else {
                vec![relationship.target]
            };
            for target in targets {
                let Some(target_node) = self.node_map.get(&target) else {
                    continue;
                };
                let stored = DynamicRelationship {
                    source: node.base.id,
                    target: target_node.base.id,
                    field_name: relationship.field_name.clone(),
                };
                if self.seen_relationships.insert(stored.clone()) {
                    self.relationships.push(stored);
                }
                self.walk(target, active);
            }
        }
        self.expanded.insert(id);
        active.remove(&id);
    }
}

fn transparent_children(
    graph: &DynamicGraph,
    node_map: &HashMap<Uuid, &DynamicDataPoint>,
    container: Uuid,
    active: &mut HashSet<Uuid>,
) -> Vec<Uuid> {
    if !active.insert(container) {
        return Vec::new();
    }
    let mut children = Vec::new();
    let mut seen = HashSet::new();
    for relationship in graph
        .relationships
        .iter()
        .filter(|relationship| relationship.source == container)
    {
        let targets = if node_map
            .get(&relationship.target)
            .is_some_and(|node| node.transparent)
        {
            transparent_children(graph, node_map, relationship.target, active)
        } else {
            vec![relationship.target]
        };
        for target in targets {
            if seen.insert(target) {
                children.push(target);
            }
        }
    }
    active.remove(&container);
    children
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
    #[error(
        "graph model `{model}` field `{field}` references a model in an unsupported shape; \
         use a model, an optional model, a union of models, or a list of those"
    )]
    UnsupportedReferenceShape { model: String, field: String },
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
        let (root_occurrence, root) = graph.visit(&self.root, answer)?;
        Ok(DynamicGraph {
            root,
            root_occurrence,
            nodes: graph.nodes,
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
        let kind = match relationship_kind(field_schema, model_names)? {
            Some(kind) => kind,
            // Never store a model reference as an opaque scalar blob.
            None if contains_reference(field_schema) => {
                return Err(DynamicGraphError::UnsupportedReferenceShape {
                    model: name.to_string(),
                    field: field_name.clone(),
                });
            }
            None => DynamicFieldKind::Scalar,
        };
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

/// Classify a field as a relationship the way Python's storage walk would see
/// it: any value that is a DataPoint becomes a node and an edge, whatever
/// `Optional`/`Union`/`List` nesting the annotation used.
fn relationship_kind(
    schema: &Value,
    model_names: &HashSet<String>,
) -> Result<Option<DynamicFieldKind>, DynamicGraphError> {
    if let Some(targets) = reference_targets(schema, model_names)? {
        return Ok(Some(DynamicFieldKind::Relationship {
            targets,
            many: false,
        }));
    }
    if let Some(variants) = schema.get("anyOf").and_then(Value::as_array) {
        let non_null = non_null_variants(variants);
        if non_null.len() == 1 {
            return relationship_kind(non_null[0], model_names);
        }
    }
    if schema.get("type").and_then(Value::as_str) == Some("array")
        && let Some(items) = schema.get("items")
        && let Some(targets) = reference_targets(items, model_names)?
    {
        return Ok(Some(DynamicFieldKind::Relationship {
            targets,
            many: true,
        }));
    }
    Ok(None)
}

/// Models referenced by a single `$ref`, or by an `anyOf` whose non-null
/// variants are all `$ref`s. `None` when the schema is not a reference.
fn reference_targets(
    schema: &Value,
    model_names: &HashSet<String>,
) -> Result<Option<Vec<String>>, DynamicGraphError> {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        return Ok(Some(vec![reference_target(reference, model_names)?]));
    }
    let Some(variants) = schema.get("anyOf").and_then(Value::as_array) else {
        return Ok(None);
    };
    let non_null = non_null_variants(variants);
    let references: Vec<&str> = non_null
        .iter()
        .filter_map(|variant| variant.get("$ref").and_then(Value::as_str))
        .collect();
    // A union mixing models with scalars has no single storage shape; the
    // caller rejects it because a reference is still reachable.
    if references.is_empty() || references.len() != non_null.len() {
        return Ok(None);
    }
    references
        .into_iter()
        .map(|reference| reference_target(reference, model_names))
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

fn non_null_variants(variants: &[Value]) -> Vec<&Value> {
    variants
        .iter()
        .filter(|variant| variant.get("type").and_then(Value::as_str) != Some("null"))
        .collect()
}

/// Whether a model reference appears anywhere inside a field schema.
fn contains_reference(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            map.get("$ref").is_some_and(Value::is_string) || map.values().any(contains_reference)
        }
        Value::Array(values) => values.iter().any(contains_reference),
        _ => false,
    }
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
    nodes: Vec<DynamicDataPoint>,
    relationships: Vec<DynamicRelationship>,
}

impl<'a> Materializer<'a> {
    fn new(schema: &'a DynamicGraphSchema, dataset_id: Uuid) -> Self {
        Self {
            schema,
            dataset_id,
            nodes: Vec::new(),
            relationships: Vec::new(),
        }
    }

    /// Materialize one object, returning its occurrence id and node UUID.
    fn visit(
        &mut self,
        model_name: &str,
        value: &Value,
    ) -> Result<(Uuid, Uuid), DynamicGraphError> {
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
                DynamicFieldKind::Relationship { targets, many } => {
                    relationship_values.push((field.name.clone(), targets, *many, value));
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
        let occurrence_id = Uuid::new_v4();
        self.nodes.push(DynamicDataPoint {
            occurrence_id,
            base,
            properties,
            index_fields: model.index_fields.clone(),
            transparent: model.transparent,
        });

        for (field_name, target_models, many, value) in relationship_values {
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
                // Python skips `None` list items: they are not DataPoints.
                for child in values.iter().filter(|child| !child.is_null()) {
                    let target = self.visit_any(target_models, child)?;
                    self.relationships.push(DynamicRelationship {
                        source: occurrence_id,
                        target,
                        field_name: field_name.clone(),
                    });
                }
            } else {
                let target = self.visit_any(target_models, value)?;
                self.relationships.push(DynamicRelationship {
                    source: occurrence_id,
                    target,
                    field_name,
                });
            }
        }
        Ok((occurrence_id, id))
    }

    /// Materialize a value declared as one of several models, returning its
    /// occurrence id. Like Pydantic's smart-mode union validation, a model
    /// whose fields cover every key of the object is preferred over a looser
    /// match; ties keep declaration order. A rejected candidate leaves nothing
    /// behind.
    fn visit_any(&mut self, models: &[String], value: &Value) -> Result<Uuid, DynamicGraphError> {
        let mut candidates: Vec<&String> = models.iter().collect();
        if let Some(object) = value.as_object() {
            candidates.sort_by_key(|model| {
                let fields = &self.schema.models[model.as_str()].fields;
                !object
                    .keys()
                    .all(|key| fields.iter().any(|field| field.name == *key))
            });
        }
        let mut first_error = None;
        for model in candidates {
            let (nodes, relationships) = (self.nodes.len(), self.relationships.len());
            match self.visit(model, value) {
                Ok((occurrence, _)) => return Ok(occurrence),
                Err(error) => {
                    self.nodes.truncate(nodes);
                    self.relationships.truncate(relationships);
                    first_error.get_or_insert(error);
                }
            }
        }
        Err(
            first_error.unwrap_or_else(|| DynamicGraphError::AnswerNotObject {
                model: models.join(" | "),
            }),
        )
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
    // Compilation routes every model reference to a relationship, so a
    // reference reaching scalar validation must not match vacuously.
    if schema.get("$ref").is_some() {
        return Err("is a model reference, not a scalar".to_string());
    }
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
#[allow(clippy::expect_used, clippy::unwrap_used)]
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

    #[test]
    fn transparent_root_is_replaced_by_children() {
        let compiled = DynamicGraphSchema::compile(&schema()).expect("schema should compile");
        let graph = compiled
            .materialize(
                &json!({
                    "name": "Container",
                    "employees": [{"name": "Alice"}, {"name": "Bob"}]
                }),
                Uuid::nil(),
            )
            .expect("answer should materialize");

        let stored = graph.stored_graph();
        assert_eq!(stored.nodes.len(), 2);
        assert!(stored.relationships.is_empty());
        assert!(!stored.nodes.iter().any(|node| node.base.id == graph.root));
        assert_eq!(graph.direct_attachment_targets().len(), 2);
        assert_eq!(graph.all_attachment_targets().len(), 2);
    }

    #[test]
    fn transparent_target_preserves_owning_field_edge() {
        let person = |name: &str| {
            let mut base = DataPoint::new("Person", Some(Uuid::nil()));
            base.id = Uuid::new_v5(&Uuid::NAMESPACE_OID, format!("Person:{name}").as_bytes());
            DynamicDataPoint {
                occurrence_id: base.id,
                base,
                properties: Map::from_iter([("name".to_string(), json!(name))]),
                index_fields: vec!["name".to_string()],
                transparent: false,
            }
        };
        let owner = person("owner");
        let child = person("child");
        let mut container_base = DataPoint::new("Container", Some(Uuid::nil()));
        container_base.id = Uuid::new_v4();
        let container = DynamicDataPoint {
            occurrence_id: container_base.id,
            base: container_base,
            properties: Map::new(),
            index_fields: Vec::new(),
            transparent: true,
        };
        let graph = DynamicGraph {
            root: owner.base.id,
            root_occurrence: owner.occurrence_id,
            nodes: vec![owner.clone(), container.clone(), child.clone()],
            relationships: vec![
                DynamicRelationship {
                    source: owner.base.id,
                    target: container.base.id,
                    field_name: "team".to_string(),
                },
                DynamicRelationship {
                    source: container.base.id,
                    target: child.base.id,
                    field_name: "members".to_string(),
                },
            ],
        };

        let stored = graph.stored_graph();
        assert_eq!(stored.nodes.len(), 2);
        assert_eq!(stored.relationships.len(), 1);
        assert!(stored.relationships.contains(&DynamicRelationship {
            source: owner.base.id,
            target: child.base.id,
            field_name: "team".to_string(),
        }));
    }

    #[test]
    fn distinct_transparent_instances_with_same_node_id_keep_their_children() {
        let schema = json!({
            "$defs": {
                "Person": {
                    "title": "Person", "type": "object",
                    "properties": {
                        "name": {"type": "string"},
                        "metadata": {"type": "object", "default": {
                            "identity_fields": ["name"], "index_fields": ["name"]
                        }}
                    }, "required": ["name"]
                },
                "Group": {
                    "title": "Group", "type": "object",
                    "properties": {
                        "name": {"type": "string"},
                        "members": {"type": "array", "items": {"$ref": "#/$defs/Person"}},
                        "metadata": {"type": "object", "default": {
                            "identity_fields": ["name"], "index_fields": ["name"],
                            "transparent": true
                        }}
                    }, "required": ["name", "members"]
                }
            },
            "title": "Holder", "type": "object",
            "properties": {
                "name": {"type": "string"},
                "left": {"$ref": "#/$defs/Group"},
                "right": {"$ref": "#/$defs/Group"},
                "metadata": {"type": "object", "default": {
                    "identity_fields": ["name"], "index_fields": ["name"]
                }}
            }, "required": ["name", "left", "right"]
        });
        let graph = DynamicGraphSchema::compile(&schema)
            .unwrap()
            .materialize(
                &json!({
                    "name": "Holder",
                    "left": {"name": "Team", "members": [{"name": "Alice"}]},
                    "right": {"name": "Team", "members": [{"name": "Bob"}]}
                }),
                Uuid::nil(),
            )
            .unwrap();
        let stored = graph.stored_graph();
        let alice = Uuid::new_v5(&Uuid::NAMESPACE_OID, b"Person:alice");
        let bob = Uuid::new_v5(&Uuid::NAMESPACE_OID, b"Person:bob");
        assert!(stored.relationships.contains(&DynamicRelationship {
            source: graph.root,
            target: alice,
            field_name: "left".to_string(),
        }));
        assert!(stored.relationships.contains(&DynamicRelationship {
            source: graph.root,
            target: bob,
            field_name: "right".to_string(),
        }));
    }

    fn union_schema() -> Value {
        json!({
            "$defs": {
                "Person": {
                    "title": "Person", "type": "object",
                    "properties": {
                        "name": {"type": "string"},
                        "metadata": {"type": "object", "default": {
                            "index_fields": ["name"], "identity_fields": ["name"]
                        }}
                    },
                    "required": ["name"]
                },
                "Org": {
                    "title": "Org", "type": "object",
                    "properties": {
                        "name": {"type": "string"},
                        "domain": {"type": "string"},
                        "metadata": {"type": "object", "default": {
                            "index_fields": ["name"], "identity_fields": ["name"]
                        }}
                    },
                    "required": ["name", "domain"]
                }
            },
            "title": "Root", "type": "object",
            "properties": {
                "owner": {"anyOf": [{"$ref": "#/$defs/Person"}, {"$ref": "#/$defs/Org"}]},
                "members": {"type": "array", "items": {
                    "anyOf": [{"$ref": "#/$defs/Person"}, {"type": "null"}]
                }, "default": []},
                "partners": {"anyOf": [
                    {"type": "array", "items": {
                        "anyOf": [{"$ref": "#/$defs/Person"}, {"$ref": "#/$defs/Org"}]
                    }},
                    {"type": "null"}
                ], "default": null}
            },
            "required": ["owner"]
        })
    }

    #[test]
    fn materializes_union_and_optional_item_relationships_as_edges() {
        let graph = DynamicGraphSchema::compile(&union_schema())
            .expect("union relationships should compile")
            .materialize(
                &json!({
                    "owner": {"name": "Acme", "domain": "acme.test"},
                    "members": [{"name": "Alice"}, null],
                    "partners": [{"name": "Bob"}, {"name": "Initech", "domain": "initech.test"}]
                }),
                Uuid::nil(),
            )
            .expect("answer should materialize");
        let type_of = |name: &str| {
            graph
                .nodes
                .iter()
                .find(|node| node.properties.get("name") == Some(&json!(name)))
                .map(|node| node.base.data_type.clone())
        };
        assert_eq!(type_of("Acme").as_deref(), Some("Org"));
        assert_eq!(type_of("Alice").as_deref(), Some("Person"));
        assert_eq!(type_of("Bob").as_deref(), Some("Person"));
        assert_eq!(type_of("Initech").as_deref(), Some("Org"));
        // Root + four children; the null member is skipped.
        assert_eq!(graph.nodes.len(), 5);
        let mut fields: Vec<&str> = graph
            .relationships
            .iter()
            .map(|relationship| relationship.field_name.as_str())
            .collect();
        fields.sort_unstable();
        assert_eq!(fields, ["members", "owner", "partners", "partners"]);
        assert!(graph.nodes.iter().all(|node| {
            node.properties
                .values()
                .all(|value| !value.is_object() && !value.is_array())
        }));
    }

    #[test]
    fn union_falls_back_to_a_later_model_without_leaving_partial_nodes() {
        // `Team` is tried first and is recorded before its `lead` fails as a
        // `Manager`; that partial node must be rolled back.
        let schema = json!({
            "$defs": {
                "Person": {
                    "title": "Person", "type": "object",
                    "properties": {"name": {"type": "string"}},
                    "required": ["name"]
                },
                "Manager": {
                    "title": "Manager", "type": "object",
                    "properties": {"name": {"type": "string"}, "level": {"type": "integer"}},
                    "required": ["name", "level"]
                },
                "Team": {
                    "title": "Team", "type": "object",
                    "properties": {
                        "lead": {"$ref": "#/$defs/Manager"},
                        "name": {"type": "string"}
                    },
                    "required": ["lead", "name"]
                },
                "Group": {
                    "title": "Group", "type": "object",
                    "properties": {
                        "lead": {"$ref": "#/$defs/Person"},
                        "name": {"type": "string"}
                    },
                    "required": ["lead", "name"]
                }
            },
            "title": "Root", "type": "object",
            "properties": {
                "unit": {"anyOf": [{"$ref": "#/$defs/Team"}, {"$ref": "#/$defs/Group"}]}
            },
            "required": ["unit"]
        });
        let graph = DynamicGraphSchema::compile(&schema)
            .expect("schema should compile")
            .materialize(
                &json!({"unit": {"lead": {"name": "Alice"}, "name": "Platform"}}),
                Uuid::nil(),
            )
            .expect("answer should materialize");
        let types: Vec<&str> = graph
            .nodes
            .iter()
            .map(|node| node.base.data_type.as_str())
            .collect();
        assert_eq!(types, ["Root", "Group", "Person"]);
        assert_eq!(graph.relationships.len(), 2);
    }

    #[test]
    fn rejects_reference_shapes_that_cannot_be_stored_as_edges() {
        for field in [
            json!({"anyOf": [{"$ref": "#/$defs/Child"}, {"type": "string"}]}),
            json!({"type": "object", "additionalProperties": {"$ref": "#/$defs/Child"}}),
            json!({"type": "array", "items": {"type": "array", "items": {"$ref": "#/$defs/Child"}}}),
        ] {
            let error = DynamicGraphSchema::compile(&json!({
                "$defs": {"Child": {
                    "title": "Child", "type": "object",
                    "properties": {"name": {"type": "string"}}
                }},
                "title": "Root", "type": "object",
                "properties": {"child": field}
            }))
            .expect_err("unsupported reference shapes must be rejected");
            assert_eq!(
                error,
                DynamicGraphError::UnsupportedReferenceShape {
                    model: "Root".to_string(),
                    field: "child".to_string(),
                }
            );
        }
    }
}
