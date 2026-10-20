//! Runtime custom graph models compiled from Pydantic-compatible JSON Schema.
//!
//! Python can manufacture `DataPoint` subclasses at runtime. Rust cannot, so
//! this module compiles the same serialized schema boundary into a small
//! descriptor and materializes LLM answers into an arena of dynamic nodes and
//! field relationships.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock, PoisonError};

use cognee_models::DataPoint;
use serde_json::{Map, Value};
use thiserror::Error;
use tracing::warn;
use uuid::Uuid;

use crate::config::ChunkAttachment;

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
    /// `$defs` entries that are not models (enums, constrained scalars):
    /// Python generates an `Enum` or a `RootModel` for each, so a reference to
    /// one is a scalar value, never a node.
    scalar_definitions: HashMap<String, Value>,
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

    /// The text of each declared index field Python's `index_data_points`
    /// would embed, in declaration order. A `None` value is skipped, as
    /// Python skips it; strings are stripped and skipped when blank. Other
    /// scalars use their JSON text: Python hands them to the embedding engine
    /// as non-strings, which writes a zero vector no query can match, so a
    /// textual embedding is the useful equivalent.
    pub fn index_field_texts(&self) -> impl Iterator<Item = (&str, String)> + '_ {
        self.index_fields.iter().filter_map(|field| {
            let text = match self.properties.get(field)? {
                Value::Null => return None,
                Value::String(text) => text.trim().to_string(),
                value => value.to_string(),
            };
            (!text.is_empty()).then_some((field.as_str(), text))
        })
    }

    /// Python `_extract_embeddable_text_from_datapoint`: every indexable
    /// field's text joined with a space.
    pub fn embeddable_text(&self) -> String {
        self.index_field_texts()
            .map(|(_, text)| text)
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Python `_get_node_label`: the first indexable field's text, else a
    /// nonblank `name`, else the type name.
    pub fn label(&self) -> String {
        self.index_field_texts()
            .next()
            .map(|(_, text)| text)
            .or_else(|| {
                self.properties
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| self.base.data_type.clone())
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
        self.walk_from(&[self.root_occurrence], &mut DynamicWalkState::default())
    }

    /// Chunk targets for Python's `direct` attachment mode. A stored root is
    /// the sole target; a transparent root is replaced by its stored children,
    /// each node id once (Python's `unwrap_transparent`).
    pub fn direct_attachment_targets(&self) -> Vec<Uuid> {
        let index = GraphIndex::new(self);
        if index
            .nodes
            .get(&self.root_occurrence)
            .is_some_and(|node| !node.transparent)
        {
            return vec![self.root];
        }
        transparent_children(&index, self.root_occurrence)
            .into_iter()
            .filter_map(|occurrence| index.nodes.get(&occurrence).map(|node| node.base.id))
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

    /// Walk from `roots` with claims shared across calls, returning only what
    /// this walk newly claimed.
    fn walk_from(&self, roots: &[Uuid], state: &mut DynamicWalkState) -> StoredDynamicGraph {
        let index = GraphIndex::new(self);
        let mut walker = DynamicWalker {
            index: &index,
            state,
            nodes: Vec::new(),
            relationships: Vec::new(),
        };
        for root in roots {
            walker.walk(*root);
        }
        StoredDynamicGraph {
            nodes: walker.nodes.into_iter().cloned().collect(),
            relationships: walker.relationships,
        }
    }
}

/// The storage walk of one Python `add_data_points` batch over the chunk
/// graphs in `graphs`, in chunk order.
///
/// Python shares `added_nodes`/`added_edges` across every chunk of the batch:
/// a node id is claimed by its first occurrence and an already-claimed node is
/// never walked again, so a later occurrence contributes neither its
/// properties nor its outgoing edges. The edge leading to it is still kept.
/// `attachment` decides what each chunk's `contains` field holds and so where
/// its walk starts: the graph root (`direct`), or every node a standalone walk
/// of that graph would store (`all`, Python's `collect_stored_data_points`),
/// which can reach a node another chunk's earlier claim hid. The result holds
/// each graph's newly claimed nodes and edges, in `graphs` order.
pub fn store_dynamic_graph_batch<'g>(
    graphs: impl IntoIterator<Item = &'g DynamicGraph>,
    attachment: ChunkAttachment,
) -> Vec<StoredDynamicGraph> {
    let mut state = DynamicWalkState::default();
    graphs
        .into_iter()
        .map(|graph| {
            let roots = match attachment {
                ChunkAttachment::Direct => vec![graph.root_occurrence],
                ChunkAttachment::All => graph
                    .stored_graph()
                    .nodes
                    .iter()
                    .map(|node| node.occurrence_id)
                    .collect(),
            };
            graph.walk_from(&roots, &mut state)
        })
        .collect()
}

/// Python's `_WalkState`: the node and edge claims one batch shares.
#[derive(Default)]
struct DynamicWalkState {
    claimed_nodes: HashSet<Uuid>,
    claimed_edges: HashSet<DynamicRelationship>,
}

/// Occurrence lookups for one graph, built once per walk.
struct GraphIndex<'a> {
    nodes: HashMap<Uuid, &'a DynamicDataPoint>,
    outgoing: HashMap<Uuid, Vec<&'a DynamicRelationship>>,
}

impl<'a> GraphIndex<'a> {
    fn new(graph: &'a DynamicGraph) -> Self {
        let mut outgoing: HashMap<Uuid, Vec<&DynamicRelationship>> = HashMap::new();
        for relationship in &graph.relationships {
            outgoing
                .entry(relationship.source)
                .or_default()
                .push(relationship);
        }
        Self {
            nodes: graph
                .nodes
                .iter()
                .map(|node| (node.occurrence_id, node))
                .collect(),
            outgoing,
        }
    }

    fn outgoing(&self, occurrence: Uuid) -> &[&'a DynamicRelationship] {
        self.outgoing.get(&occurrence).map_or(&[], Vec::as_slice)
    }
}

struct DynamicWalker<'i, 'a, 's> {
    index: &'i GraphIndex<'a>,
    state: &'s mut DynamicWalkState,
    nodes: Vec<&'a DynamicDataPoint>,
    relationships: Vec<DynamicRelationship>,
}

impl DynamicWalker<'_, '_, '_> {
    /// Python `_walk_data_point`: a transparent node walks its resolved
    /// children; a stored node is claimed by id, and only its first
    /// occurrence emits its outgoing edges and walks their targets. Claiming
    /// before descending also ends every cycle.
    fn walk(&mut self, occurrence: Uuid) {
        let Some(node) = self.index.nodes.get(&occurrence).copied() else {
            return;
        };
        if node.transparent {
            for child in transparent_children(self.index, occurrence) {
                self.walk(child);
            }
            return;
        }
        if !self.state.claimed_nodes.insert(node.base.id) {
            return;
        }
        self.nodes.push(node);
        for relationship in self.index.outgoing(occurrence) {
            let targets = if self
                .index
                .nodes
                .get(&relationship.target)
                .is_some_and(|target| target.transparent)
            {
                transparent_children(self.index, relationship.target)
            } else {
                vec![relationship.target]
            };
            for target in targets {
                let Some(target_node) = self.index.nodes.get(&target) else {
                    continue;
                };
                let stored = DynamicRelationship {
                    source: node.base.id,
                    target: target_node.base.id,
                    field_name: relationship.field_name.clone(),
                };
                if self.state.claimed_edges.insert(stored.clone()) {
                    self.relationships.push(stored);
                }
                if !self.state.claimed_nodes.contains(&target_node.base.id) {
                    self.walk(target);
                }
            }
        }
    }
}

/// Python `unwrap_transparent`: the stored nodes that replace a transparent
/// container, recursively, in field then list order. Each node id appears
/// once, first reach winning.
fn transparent_children(index: &GraphIndex<'_>, container: Uuid) -> Vec<Uuid> {
    let mut resolution = TransparentResolution::default();
    resolution.resolve(index, container);
    resolution.resolved
}

/// The three guards of Python's `_resolve_children`: `active` holds the
/// occurrences on the current path (true cycles), `expanded` the containers
/// already resolved (diamonds), and `seen_ids` the node ids already returned.
#[derive(Default)]
struct TransparentResolution {
    active: HashSet<Uuid>,
    expanded: HashSet<Uuid>,
    seen_ids: HashSet<Uuid>,
    resolved: Vec<Uuid>,
}

impl TransparentResolution {
    fn resolve(&mut self, index: &GraphIndex<'_>, container: Uuid) {
        if self.expanded.contains(&container) || !self.active.insert(container) {
            return;
        }
        if let Some(node) = index.nodes.get(&container) {
            warn_dropped_transparent_fields(node);
        }
        for relationship in index.outgoing(container) {
            match index.nodes.get(&relationship.target) {
                Some(target) if target.transparent => self.resolve(index, relationship.target),
                Some(target) => {
                    if self.seen_ids.insert(target.base.id) {
                        self.resolved.push(relationship.target);
                    }
                }
                None => {}
            }
        }
        self.active.remove(&container);
        self.expanded.insert(container);
    }
}

/// Python `_warn_dropped_field`: a transparent node is never stored, so a
/// scalar it carries is lost. Only a value Python treats as truthy counts (a
/// null, empty string, zero, `false` or empty collection lost nothing), and
/// each class and field warns once per process.
fn warn_dropped_transparent_fields(node: &DynamicDataPoint) {
    for field in dropped_transparent_fields(node) {
        if first_warning(&format!("dropped_field:{}.{field}", node.base.data_type)) {
            warn!(
                node_type = %node.base.data_type,
                field,
                "custom graph type is marked transparent but carries data in this field; a \
                 transparent node is never stored, so the value is dropped"
            );
        }
    }
}

/// The scalar fields of a transparent node that hold a truthy value.
fn dropped_transparent_fields(node: &DynamicDataPoint) -> Vec<&str> {
    node.properties
        .iter()
        .filter(|(_, value)| python_truthy(value))
        .map(|(field, _)| field.as_str())
        .collect()
}

/// Python `bool()` of a JSON value.
fn python_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|number| number != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(values) => !values.is_empty(),
        Value::Object(values) => !values.is_empty(),
    }
}

/// Python `warn_once`: true the first time `key` is seen in this process.
fn first_warning(key: &str) -> bool {
    static WARNED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    WARNED
        .get_or_init(Mutex::default)
        .lock()
        // A poisoned set only risks repeating a warning.
        .unwrap_or_else(PoisonError::into_inner)
        .insert(key.to_string())
}

/// Invalid schema or response errors detected before persistence.
#[derive(Debug, Error, Clone, PartialEq)]
pub enum DynamicGraphError {
    #[error("graph schema must be a JSON object")]
    SchemaNotObject,
    #[error("graph schema root model is missing a non-empty `title`")]
    MissingTitle,
    #[error(
        "graph schema root model title `{0}` is not a valid class name; use an \
         identifier that starts with an uppercase letter and holds only letters and ASCII digits"
    )]
    InvalidTitle(String),
    #[error(
        "graph schema title `{title}` must name the model its root `$ref` points at \
         (`{reference}`), as Python looks the root class up by that title"
    )]
    ReferenceRootTitleMismatch { title: String, reference: String },
    #[error("external graph schema reference `{0}` is not allowed")]
    ExternalReference(String),
    #[error("graph schema reference `{0}` does not name a declared model")]
    UnknownReference(String),
    #[error("graph model `{model}` has no object `properties`")]
    MissingProperties { model: String },
    #[error("graph model `{model}` declares reserved DataPoint field `{field}`")]
    ReservedField { model: String, field: String },
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
        let root_schema = root_definition
            .as_ref()
            .and_then(|name| definitions.and_then(|defs| defs.get(name)))
            .and_then(Value::as_object)
            .unwrap_or(root_obj);
        // Python looks the generated root class up by the top-level schema's
        // `title` (`namespace[schema["title"]]`), so a root without a usable
        // class name cannot be built there. That holds for a `$ref` root too:
        // the referenced model's own `title` is not consulted, and the lookup
        // only finds a DataPoint class when the top-level title names the
        // referenced `$defs` key (any other title yields a `RootModel`
        // wrapper that Python cannot store).
        let title = root_obj
            .get("title")
            .and_then(Value::as_str)
            .filter(|title| !title.is_empty())
            .ok_or(DynamicGraphError::MissingTitle)?;
        if !is_python_class_name(title) {
            return Err(DynamicGraphError::InvalidTitle(title.to_string()));
        }
        if let Some(reference) = root_definition.as_deref()
            && reference != title
        {
            return Err(DynamicGraphError::ReferenceRootTitleMismatch {
                title: title.to_string(),
                reference: reference.to_string(),
            });
        }
        let root = title.to_string();

        let mut raw_models: HashMap<String, &Map<String, Value>> = HashMap::new();
        let mut scalar_definitions = HashMap::new();
        raw_models.insert(root.clone(), root_schema);
        if let Some(definitions) = definitions {
            for (name, definition) in definitions {
                let Some(definition) = definition.as_object() else {
                    continue;
                };
                if *name != root && is_scalar_definition(definition) {
                    scalar_definitions.insert(name.clone(), Value::Object(definition.clone()));
                } else {
                    raw_models.insert(name.clone(), definition);
                }
            }
        }

        let names = SchemaNames {
            models: raw_models.keys().cloned().collect(),
            scalars: &scalar_definitions,
        };
        let mut models = HashMap::new();
        for (name, model) in &raw_models {
            models.insert(name.clone(), compile_model(name, model, &names)?);
        }

        let mut llm_schema = schema.clone();
        strip_metadata_properties(&mut llm_schema);

        Ok(Self {
            root,
            models,
            scalar_definitions,
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

/// The names a field reference can resolve to.
struct SchemaNames<'a> {
    models: HashSet<String>,
    scalars: &'a HashMap<String, Value>,
}

impl SchemaNames<'_> {
    /// Whether `reference` names a non-model `$defs` entry.
    fn is_scalar(&self, reference: &str) -> bool {
        reference
            .strip_prefix("#/$defs/")
            .is_some_and(|target| self.scalars.contains_key(target))
    }
}

/// Whether a `$defs` entry is a value type rather than a model: an enum, a
/// constant or a non-object scalar, which datamodel-code-generator turns into
/// an `Enum` or a `RootModel` instead of a `DataPoint` subclass.
fn is_scalar_definition(definition: &Map<String, Value>) -> bool {
    if definition.contains_key("properties") {
        return false;
    }
    definition.contains_key("enum")
        || definition.contains_key("const")
        || definition
            .get("type")
            .is_some_and(|kind| kind.as_str() != Some("object"))
}

fn compile_model(
    name: &str,
    schema: &Map<String, Value>,
    names: &SchemaNames<'_>,
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
    let declared_index_fields = string_list(metadata.and_then(|m| m.get("index_fields")));
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
        let kind = match relationship_kind(field_schema, names)? {
            Some(kind) => kind,
            // Never store a model reference as an opaque scalar blob.
            None if contains_model_reference(field_schema, names) => {
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

    let field_kind = |field: &str| {
        fields
            .iter()
            .find(|candidate| candidate.name == field)
            .map(|candidate| &candidate.kind)
    };
    // Python's `index_data_points` skips an index field the instance has no
    // value for, so an unknown name is dropped rather than refused. A
    // relationship field would embed a whole node; skip it too.
    let mut index_fields = Vec::new();
    for field in declared_index_fields {
        if field_kind(&field) == Some(&DynamicFieldKind::Scalar) {
            index_fields.push(field);
        } else {
            warn!(
                model = name,
                field = %field,
                "custom graph index field is not a scalar field of the model; it is not indexed"
            );
        }
    }
    // Python derives the id from these fields at runtime: an unknown name
    // makes every instance fall back to a random uuid4, and a relationship
    // field stringifies a whole node, timestamps included. Neither is an
    // error there, so both are kept and only warned about.
    for field in &identity_fields {
        if field_kind(field) != Some(&DynamicFieldKind::Scalar) {
            warn!(
                model = name,
                field = %field,
                "custom graph identity field is not a scalar field of the model; node ids \
                 fall back to random UUIDs"
            );
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
    names: &SchemaNames<'_>,
) -> Result<Option<DynamicFieldKind>, DynamicGraphError> {
    if let Some(targets) = reference_targets(schema, names)? {
        return Ok(Some(DynamicFieldKind::Relationship {
            targets,
            many: false,
        }));
    }
    if let Some(variants) = schema.get("anyOf").and_then(Value::as_array) {
        let non_null = non_null_variants(variants);
        if non_null.len() == 1 {
            return relationship_kind(non_null[0], names);
        }
    }
    if schema.get("type").and_then(Value::as_str) == Some("array")
        && let Some(items) = schema.get("items")
        && let Some(targets) = reference_targets(items, names)?
    {
        return Ok(Some(DynamicFieldKind::Relationship {
            targets,
            many: true,
        }));
    }
    Ok(None)
}

/// Models referenced by a single `$ref`, or by an `anyOf` whose non-null
/// variants are all model `$ref`s. `None` when the schema is not a model
/// reference; a reference to a scalar definition is a value.
fn reference_targets(
    schema: &Value,
    names: &SchemaNames<'_>,
) -> Result<Option<Vec<String>>, DynamicGraphError> {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        if names.is_scalar(reference) {
            return Ok(None);
        }
        return Ok(Some(vec![reference_target(reference, &names.models)?]));
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
    // caller rejects it because a model reference is still reachable.
    if references.is_empty()
        || references.len() != non_null.len()
        || references
            .iter()
            .any(|reference| names.is_scalar(reference))
    {
        return Ok(None);
    }
    references
        .into_iter()
        .map(|reference| reference_target(reference, &names.models))
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

fn non_null_variants(variants: &[Value]) -> Vec<&Value> {
    variants
        .iter()
        .filter(|variant| variant.get("type").and_then(Value::as_str) != Some("null"))
        .collect()
}

/// Whether `title` survives Python's schema-to-model step unchanged, so
/// `graph_schema_to_graph_model`'s `namespace[schema["title"]]` finds the
/// generated class. datamodel-code-generator capitalizes the first letter,
/// drops `_`, `-` and spaces (`person_model` becomes `PersonModel`), prefixes
/// a leading digit or numeric character (`Ⅷ` becomes `FieldⅧ`), strips
/// superscript digits (`Company²` becomes `Company`) and suffixes the
/// `None`/`True`/`False` keywords; any of those makes the Python lookup raise
/// `KeyError`. `DataPoint` resolves to the cognee base class every generated
/// model inherits from rather than to the generated root. `BaseModel` is kept
/// as is, as in Python.
///
/// The check is slightly stricter than Python: a character Rust classes as
/// numeric is accepted only as an ASCII digit, so a non-ASCII decimal digit
/// that Python would keep (`Company٣`) is rejected here. It does not model
/// Python's NFKC identifier normalisation, so a compatibility character such
/// as a fullwidth letter still passes; plain ASCII titles are exact.
fn is_python_class_name(title: &str) -> bool {
    let is_letter = |c: char| c.is_alphabetic() && !c.is_numeric();
    let mut chars = title.chars();
    chars
        .next()
        .is_some_and(|first| first.is_uppercase() && is_letter(first))
        && chars.all(|c| is_letter(c) || c.is_ascii_digit())
        && !matches!(title, "None" | "True" | "False" | "DataPoint")
}

/// Whether a reference to anything but a scalar definition appears anywhere
/// inside a field schema.
fn contains_model_reference(value: &Value, names: &SchemaNames<'_>) -> bool {
    match value {
        Value::Object(map) => {
            map.get("$ref")
                .and_then(Value::as_str)
                .is_some_and(|reference| !names.is_scalar(reference))
                || map
                    .values()
                    .any(|value| contains_model_reference(value, names))
        }
        Value::Array(values) => values
            .iter()
            .any(|value| contains_model_reference(value, names)),
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
                    let definitions = &self.schema.scalar_definitions;
                    validate_scalar_value(value, &field.schema, definitions, 0).map_err(
                        |reason| DynamicGraphError::InvalidScalarValue {
                            model: model_name.to_string(),
                            field: field.name.clone(),
                            reason,
                        },
                    )?;
                    properties.insert(field.name.clone(), coerce_number(value, &field.schema));
                }
                DynamicFieldKind::Relationship { targets, many } => {
                    relationship_values.push((field.name.clone(), targets, *many, value));
                }
            }
        }

        let id = dynamic_id(model, object, &properties, &self.schema.scalar_definitions);
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

/// Python `DataPoint.__init__` + `_generate_identity_id`: the id comes from
/// the Pydantic-validated values of the identity fields, each `str()`-ed.
///
/// Every declared field is in the validated instance: an omitted optional
/// field holds its `None` default, so it contributes `"None"`. Only a name the
/// model does not declare makes Python fall back to a random uuid4. A
/// relationship value is a whole node whose `str()` carries fresh timestamps,
/// so it is random in Python too; only an empty one (`None`, `[]`) is stable.
fn dynamic_id(
    model: &DynamicModelSchema,
    answer: &Map<String, Value>,
    properties: &Map<String, Value>,
    definitions: &HashMap<String, Value>,
) -> Uuid {
    if model.identity_fields.is_empty() {
        return Uuid::new_v4();
    }
    let mut values = Vec::with_capacity(model.identity_fields.len());
    for name in &model.identity_fields {
        let Some(field) = model.fields.iter().find(|field| field.name == *name) else {
            return Uuid::new_v4();
        };
        let text = match field.kind {
            DynamicFieldKind::Scalar => match properties.get(name) {
                Some(value) => python_identity_text(&python_value(
                    value,
                    &field.schema,
                    definitions,
                    Some(&python_class_name(&field.name)),
                )),
                None => "None".to_string(),
            },
            DynamicFieldKind::Relationship { .. } => {
                match answer.get(name).or(field.default.as_ref()) {
                    None | Some(Value::Null) => "None".to_string(),
                    Some(Value::Array(items)) if items.is_empty() => "[]".to_string(),
                    Some(_) => return Uuid::new_v4(),
                }
            }
        };
        values.push(text);
    }
    Uuid::new_v5(
        &Uuid::NAMESPACE_OID,
        format!("{}:{}", model.name, values.join("|")).as_bytes(),
    )
}

/// Python `DataPoint._normalize_identity_value`: a `str` is lower-cased with
/// spaces turned into `_` and apostrophes dropped; anything else is `str()`-ed.
fn python_identity_text(value: &PyValue) -> String {
    match value {
        PyValue::Str(raw) => raw.to_lowercase().replace(' ', "_").replace('\'', ""),
        _ => value.to_str(),
    }
}

/// A JSON answer value as the Python object Pydantic validates it into,
/// enough to reproduce its `str()`.
#[derive(Debug, Clone, PartialEq)]
enum PyValue {
    None,
    Bool(bool),
    /// Integer digits.
    Int(String),
    Float(f64),
    Str(String),
    /// `str()` of an aware `datetime`.
    DateTime(String),
    /// A member of a generated `Enum`; an `IntEnum` member carries its value,
    /// which is what its `str()` prints.
    Enum {
        class: String,
        member: String,
        value: Box<PyValue>,
        int_enum: bool,
    },
    /// A generated `RootModel` wrapping a scalar definition.
    Root {
        class: String,
        root: Box<PyValue>,
    },
    List(Vec<PyValue>),
    Dict(Vec<(String, PyValue)>),
}

impl PyValue {
    /// Python `str()`.
    fn to_str(&self) -> String {
        match self {
            Self::Str(text) | Self::DateTime(text) => text.clone(),
            Self::Enum {
                class,
                member,
                value,
                int_enum,
            } => {
                if *int_enum {
                    value.to_str()
                } else {
                    format!("{class}.{member}")
                }
            }
            // Pydantic's `BaseModel.__str__` prints the fields.
            Self::Root { root, .. } => format!("root={}", root.to_repr()),
            _ => self.to_repr(),
        }
    }

    /// Python `repr()`, as containers print their items.
    fn to_repr(&self) -> String {
        match self {
            Self::None => "None".to_string(),
            Self::Bool(true) => "True".to_string(),
            Self::Bool(false) => "False".to_string(),
            Self::Int(digits) => digits.clone(),
            Self::Float(number) => python_float_repr(*number),
            Self::Str(text) => python_str_repr(text),
            // `repr(datetime)` is the constructor call; the `str()` form is
            // close enough for the rare list of datetimes.
            Self::DateTime(text) => text.clone(),
            Self::Enum {
                class,
                member,
                value,
                ..
            } => format!("<{class}.{member}: {}>", value.to_repr()),
            Self::Root { class, root } => format!("{class}(root={})", root.to_repr()),
            Self::List(items) => format!(
                "[{}]",
                items
                    .iter()
                    .map(Self::to_repr)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Dict(entries) => format!(
                "{{{}}}",
                entries
                    .iter()
                    .map(|(key, value)| format!("{}: {}", python_str_repr(key), value.to_repr()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

/// The Python value Pydantic builds from `value` for a field declared by
/// `schema`, as datamodel-code-generator types it: `number` is a `float`,
/// `integer` an `int`, a `date-time` string an aware `datetime`, an enum a
/// generated `Enum` member (`enum_class` names an inline enum's class), and a
/// scalar `$defs` entry a `RootModel`. Assumes `value` already validated.
fn python_value(
    value: &Value,
    schema: &Value,
    definitions: &HashMap<String, Value>,
    enum_class: Option<&str>,
) -> PyValue {
    if value.is_null() {
        return PyValue::None;
    }
    if let Some(name) = schema
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|reference| reference.strip_prefix("#/$defs/"))
        && let Some(definition) = definitions.get(name)
    {
        let class = python_class_name(name);
        if definition.get("enum").is_some() {
            return python_value(value, definition, definitions, Some(&class));
        }
        return PyValue::Root {
            root: Box::new(python_value(value, definition, definitions, None)),
            class,
        };
    }
    if let Some(variants) = schema.get("anyOf").and_then(Value::as_array) {
        // Pydantic's smart-mode union keeps an exact type match over a
        // coercing one: `3` stays an `int` in `float | int` but becomes `3.0`
        // in `str | float`.
        let valid =
            |variant: &&Value| validate_scalar_value(value, variant, definitions, 0).is_ok();
        let chosen = variants
            .iter()
            .filter(valid)
            .find(|variant| exact_type_match(value, variant))
            .or_else(|| variants.iter().find(valid));
        return match chosen {
            Some(variant) => python_value(value, variant, definitions, enum_class),
            None => plain_python_value(value),
        };
    }
    let kind = schema.get("type").and_then(Value::as_str);
    if let (Some(choices), Some(class)) = (schema.get("enum").and_then(Value::as_array), enum_class)
        && let Some(choice) = choices.iter().find(|choice| json_equal(choice, value))
    {
        return PyValue::Enum {
            class: class.to_string(),
            member: python_enum_member_name(choice, kind),
            value: Box::new(python_value(choice, &Value::Null, definitions, None)),
            int_enum: kind == Some("integer"),
        };
    }
    match (kind, value) {
        (Some("number"), Value::Number(number)) => number
            .as_f64()
            .map_or_else(|| plain_python_value(value), PyValue::Float),
        (Some("integer"), Value::Number(number)) => match number.as_f64() {
            Some(float) if number.as_i64().is_none() && number.as_u64().is_none() => {
                PyValue::Int(format!("{float:.0}"))
            }
            _ => PyValue::Int(number.to_string()),
        },
        (Some("string"), Value::String(text))
            if schema.get("format").and_then(Value::as_str) == Some("date-time") =>
        {
            parse_aware_datetime(text).map_or_else(
                || PyValue::Str(text.clone()),
                |date| PyValue::DateTime(python_datetime_str(&date)),
            )
        }
        (_, Value::Array(items)) => {
            let item_schema = schema.get("items").unwrap_or(&Value::Null);
            PyValue::List(
                items
                    .iter()
                    .map(|item| python_value(item, item_schema, definitions, None))
                    .collect(),
            )
        }
        (_, Value::Object(entries)) => {
            let properties = schema.get("properties");
            PyValue::Dict(
                entries
                    .iter()
                    .map(|(key, item)| {
                        let item_schema = properties
                            .and_then(|properties| properties.get(key))
                            .unwrap_or(&Value::Null);
                        (
                            key.clone(),
                            python_value(item, item_schema, definitions, None),
                        )
                    })
                    .collect(),
            )
        }
        _ => plain_python_value(value),
    }
}

/// The Python value `json.loads` gives for an untyped JSON value.
fn plain_python_value(value: &Value) -> PyValue {
    match value {
        Value::Null => PyValue::None,
        Value::Bool(flag) => PyValue::Bool(*flag),
        Value::Number(number) if number.is_f64() => {
            number.as_f64().map_or(PyValue::None, PyValue::Float)
        }
        Value::Number(number) => PyValue::Int(number.to_string()),
        Value::String(text) => PyValue::Str(text.clone()),
        Value::Array(items) => PyValue::List(items.iter().map(plain_python_value).collect()),
        Value::Object(entries) => PyValue::Dict(
            entries
                .iter()
                .map(|(key, value)| (key.clone(), plain_python_value(value)))
                .collect(),
        ),
    }
}

/// Whether a union variant takes `value` without coercion.
fn exact_type_match(value: &Value, variant: &Value) -> bool {
    match variant.get("type").and_then(Value::as_str) {
        Some("integer") => value.as_i64().is_some() || value.as_u64().is_some(),
        Some("number") => value.is_f64(),
        Some("string") => value.is_string(),
        Some("boolean") => value.is_boolean(),
        Some("array") => value.is_array(),
        Some("object") => value.is_object(),
        _ => false,
    }
}

/// Store `number` fields as floats and `integer` fields as integers, the
/// types Pydantic hands to storage (`3` becomes `3.0`, `3.0` becomes `3`).
fn coerce_number(value: &Value, schema: &Value) -> Value {
    let Value::Number(number) = value else {
        return value.clone();
    };
    // An enum member or a `RootModel` keeps the value it was declared with.
    if schema.get("enum").is_some() || schema.get("$ref").is_some() {
        return value.clone();
    }
    let kind = match schema.get("anyOf").and_then(Value::as_array) {
        Some(variants) => {
            let non_null = non_null_variants(variants);
            match non_null.as_slice() {
                [only] => only.get("type").and_then(Value::as_str),
                _ => None,
            }
        }
        None => schema.get("type").and_then(Value::as_str),
    };
    match kind {
        Some("number") if !number.is_f64() => number
            .as_f64()
            .and_then(serde_json::Number::from_f64)
            .map_or_else(|| value.clone(), Value::Number),
        Some("integer") if number.is_f64() => number
            .as_f64()
            .filter(|float| float.fract() == 0.0 && float.abs() < i64::MAX as f64)
            .map_or_else(|| value.clone(), |float| Value::from(float as i64)),
        _ => value.clone(),
    }
}

/// datamodel-code-generator's class name for a `$defs` key or an inline
/// enum's field: each `_`/`-`/space-separated part capitalized and joined
/// (`lower_def` becomes `LowerDef`).
fn python_class_name(name: &str) -> String {
    name.split(|c: char| !c.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect()
            })
        })
        .collect()
}

/// datamodel-code-generator's enum member name: a string value with every
/// non-identifier character turned into `_`, `field_`-prefixed when it starts
/// with a digit or is empty, `_`-suffixed when it is a keyword; a non-string
/// value prefixed with the schema type (`integer_1`, `number_1_5`) or, for an
/// untyped enum, its Python type (`int_1`, `bool_True`).
fn python_enum_member_name(choice: &Value, kind: Option<&str>) -> String {
    let (prefix, raw) = match choice {
        Value::String(text) => (None, text.clone()),
        Value::Number(number) => (
            Some(kind.unwrap_or(if number.is_f64() { "float" } else { "int" })),
            number.to_string(),
        ),
        Value::Bool(flag) => (
            Some(kind.unwrap_or("bool")),
            if *flag { "True" } else { "False" }.to_string(),
        ),
        Value::Null => (Some(kind.unwrap_or("NoneType")), "None".to_string()),
        _ => (None, choice.to_string()),
    };
    let sanitized: String = raw
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if let Some(prefix) = prefix {
        return format!("{prefix}_{sanitized}");
    }
    if sanitized.is_empty() || sanitized.starts_with(|c: char| c.is_ascii_digit()) {
        return format!("field_{sanitized}");
    }
    if PYTHON_KEYWORDS.contains(&sanitized.as_str()) {
        return format!("{sanitized}_");
    }
    sanitized
}

const PYTHON_KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
    "with", "yield",
];

/// Python `repr(float)`: the shortest round-tripping digits, positional for
/// exponents in `[-4, 16)` and scientific (`1e+16`, `1.5e-07`) outside.
fn python_float_repr(number: f64) -> String {
    if number.is_nan() {
        return "nan".to_string();
    }
    if number.is_infinite() {
        return if number > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    if number == 0.0 {
        return if number.is_sign_negative() {
            "-0.0"
        } else {
            "0.0"
        }
        .to_string();
    }
    let scientific = format!("{:e}", number.abs());
    // `{:e}` always prints `<digits>e<exponent>`; the fallbacks are unreachable.
    let Some((mantissa, exponent)) = scientific.split_once('e') else {
        return number.to_string();
    };
    let Ok(exponent) = exponent.parse::<i32>() else {
        return number.to_string();
    };
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let sign = if number < 0.0 { "-" } else { "" };
    if (-4..16).contains(&exponent) {
        let text = if exponent >= 0 {
            let point = exponent as usize + 1;
            if digits.len() > point {
                format!("{}.{}", &digits[..point], &digits[point..])
            } else {
                format!("{}{}.0", digits, "0".repeat(point - digits.len()))
            }
        } else {
            format!("0.{}{}", "0".repeat((-exponent - 1) as usize), digits)
        };
        return format!("{sign}{text}");
    }
    let mantissa = if digits.len() > 1 {
        format!("{}.{}", &digits[..1], &digits[1..])
    } else {
        digits
    };
    let exponent_sign = if exponent < 0 { '-' } else { '+' };
    format!("{sign}{mantissa}e{exponent_sign}{:02}", exponent.abs())
}

/// Python `repr(str)`: single quotes unless the text holds a single quote
/// and no double quote, with backslashes, the quote and control characters
/// escaped.
fn python_str_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || (0x7f..=0xa0).contains(&(c as u32)) => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Parse a string the way Pydantic's `AwareDatetime` accepts it: an ISO 8601
/// date and time, `T` or space separated, seconds optional, with a `Z` or
/// numeric offset. A naive datetime is rejected.
fn parse_aware_datetime(text: &str) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    let text = text.trim();
    let normalized = match text.strip_suffix(['Z', 'z']) {
        Some(stem) => format!("{stem}+00:00"),
        None => text.to_string(),
    };
    [
        "%Y-%m-%dT%H:%M:%S%.f%:z",
        "%Y-%m-%d %H:%M:%S%.f%:z",
        "%Y-%m-%dT%H:%M%:z",
        "%Y-%m-%d %H:%M%:z",
        "%Y-%m-%dT%H:%M:%S%.f%z",
        "%Y-%m-%d %H:%M:%S%.f%z",
    ]
    .iter()
    .find_map(|format| chrono::DateTime::parse_from_str(&normalized, format).ok())
}

/// Python `str(datetime)` for an aware datetime: `YYYY-MM-DD HH:MM:SS`, then
/// `.ffffff` when there are microseconds, then the `+HH:MM` offset.
fn python_datetime_str(date: &chrono::DateTime<chrono::FixedOffset>) -> String {
    use chrono::Timelike;
    let mut text = date.format("%Y-%m-%d %H:%M:%S").to_string();
    let micros = date.nanosecond() / 1_000 % 1_000_000;
    if micros != 0 {
        text.push_str(&format!(".{micros:06}"));
    }
    let offset = date.offset().local_minus_utc();
    let (sign, offset) = if offset < 0 {
        ('-', -offset)
    } else {
        ('+', offset)
    };
    text.push_str(&format!(
        "{sign}{:02}:{:02}",
        offset / 3600,
        offset % 3600 / 60
    ));
    if offset % 60 != 0 {
        text.push_str(&format!(":{:02}", offset % 60));
    }
    text
}

/// JSON equality with numbers compared by value, as Python compares
/// `2 == 2.0`.
fn json_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => left.as_f64() == right.as_f64(),
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| json_equal(left, right))
        }
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left.iter().all(|(key, value)| {
                    right.get(key).is_some_and(|other| json_equal(value, other))
                })
        }
        _ => left == right,
    }
}

/// Reference chains between scalar definitions deeper than this are treated
/// as cycles and rejected.
const MAX_SCALAR_REFERENCE_DEPTH: usize = 32;

fn validate_scalar_value(
    value: &Value,
    schema: &Value,
    definitions: &HashMap<String, Value>,
    depth: usize,
) -> Result<(), String> {
    // Compilation routes every model reference to a relationship, so a
    // reference reaching scalar validation must name a scalar definition and
    // must not match vacuously.
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let definition = reference
            .strip_prefix("#/$defs/")
            .and_then(|name| definitions.get(name))
            .ok_or_else(|| "is a model reference, not a scalar".to_string())?;
        if depth >= MAX_SCALAR_REFERENCE_DEPTH {
            return Err("nests scalar definition references too deeply".to_string());
        }
        return validate_scalar_value(value, definition, definitions, depth + 1);
    }
    if let Some(variants) = schema.get("anyOf").and_then(Value::as_array) {
        if variants
            .iter()
            .any(|variant| validate_scalar_value(value, variant, definitions, depth).is_ok())
        {
            return Ok(());
        }
        return Err("does not match any allowed variant".to_string());
    }
    if let Some(expected) = schema.get("type").and_then(Value::as_str) {
        let matches = match expected {
            "null" => value.is_null(),
            "boolean" => value.is_boolean(),
            // Pydantic's lax `int` takes a float with no fractional part.
            "integer" => {
                value.as_i64().is_some()
                    || value.as_u64().is_some()
                    || value
                        .as_f64()
                        .is_some_and(|number| number.is_finite() && number.fract() == 0.0)
            }
            "number" => value.is_number(),
            "string" => value.is_string(),
            "array" => value.is_array(),
            "object" => value.is_object(),
            _ => true,
        };
        if !matches {
            return Err(format!("expected {expected}"));
        }
        if expected == "string"
            && schema.get("format").and_then(Value::as_str) == Some("date-time")
            && let Some(text) = value.as_str()
            && parse_aware_datetime(text).is_none()
        {
            return Err("is not a timezone-aware date-time".to_string());
        }
    }
    if let Some(constant) = schema.get("const")
        && !json_equal(value, constant)
    {
        return Err("does not equal the declared constant".to_string());
    }
    if let Some(choices) = schema.get("enum").and_then(Value::as_array)
        && !choices.iter().any(|choice| json_equal(choice, value))
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
            validate_scalar_value(item, item_schema, definitions, depth)?;
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
                    validate_scalar_value(field_value, field_schema, definitions, depth)?;
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
    fn rejects_an_external_root_reference() {
        let error = DynamicGraphSchema::compile(&json!({
            "$ref": "other.json#/$defs/Person",
            "$defs": {"Person": {"title": "Person", "properties": {}}}
        }))
        .expect_err("an external root ref must be rejected");
        assert_eq!(
            error,
            DynamicGraphError::ExternalReference("other.json#/$defs/Person".to_string())
        );
    }

    #[test]
    fn rejects_a_reference_to_an_undeclared_model() {
        let error = DynamicGraphSchema::compile(&json!({
            "title": "Company",
            "properties": {"ceo": {"$ref": "#/$defs/Person"}}
        }))
        .expect_err("a dangling field ref must be rejected");
        assert_eq!(
            error,
            DynamicGraphError::UnknownReference("#/$defs/Person".to_string())
        );

        let error = DynamicGraphSchema::compile(&json!({"$ref": "#/$defs/Person"}))
            .expect_err("a dangling root ref must be rejected");
        assert_eq!(
            error,
            DynamicGraphError::UnknownReference("#/$defs/Person".to_string())
        );
    }

    #[test]
    fn rejects_reserved_data_point_fields() {
        for field in ["id", "type", "belongs_to_set", "importance_weight"] {
            let error = DynamicGraphSchema::compile(&json!({
                "title": "Company",
                "properties": {"name": {"type": "string"}, field: {"type": "string"}}
            }))
            .expect_err("a reserved DataPoint field must be rejected");
            assert_eq!(
                error,
                DynamicGraphError::ReservedField {
                    model: "Company".to_string(),
                    field: field.to_string(),
                }
            );
        }
        // Nested models are held to the same rule.
        let mut nested = schema();
        nested["$defs"]["Person"]["properties"]["created_at"] = json!({"type": "integer"});
        assert_eq!(
            DynamicGraphSchema::compile(&nested).expect_err("nested reserved field"),
            DynamicGraphError::ReservedField {
                model: "Person".to_string(),
                field: "created_at".to_string(),
            }
        );
    }

    #[test]
    fn requires_a_root_title() {
        for schema in [
            json!({"properties": {}}),
            json!({"title": "", "properties": {}}),
            json!({"title": 7, "properties": {}}),
        ] {
            assert_eq!(
                DynamicGraphSchema::compile(&schema).expect_err("untitled root"),
                DynamicGraphError::MissingTitle,
                "{schema}"
            );
        }
    }

    #[test]
    fn requires_a_root_title_python_can_use_as_a_class_name() {
        // Each of these is rewritten or refused by datamodel-code-generator,
        // so Python's `namespace[schema["title"]]` lookup raises `KeyError`.
        for title in [
            "company",
            "Company Graph",
            "Company_Graph",
            "Company-Graph",
            "1Company",
            "_Company",
            "None",
            "True",
            "DataPoint",
            // Python's generator prefixes a leading numeric character and
            // strips superscript digits and other non-decimal numerals.
            "Ⅷ",
            "AⅧ",
            "Company²",
            "Company½",
        ] {
            assert_eq!(
                DynamicGraphSchema::compile(&json!({"title": title, "properties": {}}))
                    .expect_err("title must be rejected"),
                DynamicGraphError::InvalidTitle(title.to_string()),
            );
        }
        // `BaseModel` survives the Python generator and its lookup unchanged.
        for title in ["Company", "CompanyGraph2", "COMPANY", "École", "BaseModel"] {
            let compiled = DynamicGraphSchema::compile(&json!({"title": title, "properties": {}}))
                .unwrap_or_else(|error| panic!("{title} should compile: {error}"));
            assert_eq!(compiled.root_model().name, title);
        }
    }

    #[test]
    fn a_reference_root_needs_a_top_level_title_naming_its_model() {
        // Python's `namespace[schema["title"]]` reads the top-level title; the
        // referenced model's own title is not consulted.
        let compiled = DynamicGraphSchema::compile(&json!({
            "title": "Person",
            "$ref": "#/$defs/Person",
            "$defs": {"Person": {"properties": {"name": {"type": "string"}}}}
        }))
        .expect("a ref root titled after its model should compile");
        assert_eq!(compiled.root_model().name, "Person");

        // No top-level title: Python raises `KeyError('title')`.
        let error = DynamicGraphSchema::compile(&json!({
            "$ref": "#/$defs/Person",
            "$defs": {"Person": {"title": "Person", "properties": {"name": {"type": "string"}}}}
        }))
        .expect_err("an untitled ref root must be rejected");
        assert_eq!(error, DynamicGraphError::MissingTitle);

        // Any other title makes Python generate a `RootModel` wrapper.
        let error = DynamicGraphSchema::compile(&json!({
            "title": "Root",
            "$ref": "#/$defs/Person",
            "$defs": {"Person": {"title": "Person", "properties": {"name": {"type": "string"}}}}
        }))
        .expect_err("a ref root titled after another class must be rejected");
        assert_eq!(
            error,
            DynamicGraphError::ReferenceRootTitleMismatch {
                title: "Root".to_string(),
                reference: "Person".to_string(),
            }
        );

        let error = DynamicGraphSchema::compile(&json!({
            "title": "person",
            "$ref": "#/$defs/person",
            "$defs": {"person": {"title": "person", "properties": {}}}
        }))
        .expect_err("a ref root title must be a class name");
        assert_eq!(error, DynamicGraphError::InvalidTitle("person".to_string()));
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
            "title": "Node",
            "$ref": "#/$defs/Node"
        });
        let compiled = DynamicGraphSchema::compile(&schema).expect("root ref should compile");
        assert_eq!(compiled.root_model().name, "Node");
        let graph = compiled
            .materialize(&json!({"name": "first", "next": null}), Uuid::nil())
            .expect("finite recursive answer should materialize");
        assert_eq!(graph.nodes.len(), 1);
    }

    /// `str()` of the value Pydantic validates `value` into, normalized as
    /// Python's identity derivation does.
    fn identity_text(value: Value, schema: Value, field: &str) -> String {
        let definitions = HashMap::from([
            (
                "Color".to_string(),
                json!({"title": "Color", "type": "string", "enum": ["red", "dark green", "Foo-Bar", "1abc", "class", ""]}),
            ),
            (
                "Level".to_string(),
                json!({"title": "Level", "type": "integer", "enum": [1, 2]}),
            ),
            (
                "Num".to_string(),
                json!({"title": "Num", "type": "number", "enum": [1.5, 2]}),
            ),
            (
                "Mixed".to_string(),
                json!({"title": "Mixed", "enum": ["a", 1]}),
            ),
            (
                "Code".to_string(),
                json!({"title": "Code", "type": "string"}),
            ),
            (
                "lower_def".to_string(),
                json!({"type": "string", "enum": ["x"]}),
            ),
        ]);
        python_identity_text(&python_value(
            &value,
            &schema,
            &definitions,
            Some(&python_class_name(field)),
        ))
    }

    #[test]
    fn identity_values_match_python_stringification() {
        // Expected texts are what Python prints for the Pydantic model
        // datamodel-code-generator builds from each schema.
        let untyped = Value::Null;
        for (value, schema, field, expected) in [
            (json!(true), untyped.clone(), "flag", "True"),
            (Value::Null, untyped.clone(), "missing", "None"),
            (json!([1, "A"]), untyped.clone(), "list", "[1, 'A']"),
            (
                json!({"enabled": false}),
                untyped.clone(),
                "map",
                "{'enabled': False}",
            ),
            (json!(3.0), untyped.clone(), "untyped_float", "3.0"),
            (
                json!("Big Cat's"),
                json!({"type": "string"}),
                "name",
                "big_cats",
            ),
            // `number` is a `float`, `integer` an `int`.
            (json!(3), json!({"type": "number"}), "score", "3.0"),
            (json!(3.0), json!({"type": "integer"}), "count", "3"),
            (json!(1e16), json!({"type": "number"}), "big", "1e+16"),
            (json!(1.5e-7), json!({"type": "number"}), "small", "1.5e-07"),
            (json!(0.0001), json!({"type": "number"}), "tiny", "0.0001"),
            (json!(-2.5), json!({"type": "number"}), "negative", "-2.5"),
            (
                json!(123456789.125),
                json!({"type": "number"}),
                "wide",
                "123456789.125",
            ),
            // A `date-time` string is an aware `datetime`, not a `str`, so it
            // is neither lower-cased nor underscored.
            (
                json!("2024-03-01T00:00:00Z"),
                json!({"type": "string", "format": "date-time"}),
                "when",
                "2024-03-01 00:00:00+00:00",
            ),
            (
                json!("2024-03-01T00:00:00.5+05:30"),
                json!({"type": "string", "format": "date-time"}),
                "when",
                "2024-03-01 00:00:00.500000+05:30",
            ),
            // Containers print their items with `repr()`.
            (
                json!(["it's", "Big Cat"]),
                json!({"type": "array", "items": {"type": "string"}}),
                "tags",
                "[\"it's\", 'Big Cat']",
            ),
            (
                json!([1, 2.5]),
                json!({"type": "array", "items": {"type": "number"}}),
                "values",
                "[1.0, 2.5]",
            ),
            (
                json!({"a": 1, "b": "it's"}),
                json!({"type": "object"}),
                "map",
                "{'a': 1, 'b': \"it's\"}",
            ),
            // Smart-mode unions keep an exact type match.
            (
                json!(3),
                json!({"anyOf": [{"type": "number"}, {"type": "integer"}]}),
                "u1",
                "3",
            ),
            (
                json!(3),
                json!({"anyOf": [{"type": "string"}, {"type": "number"}]}),
                "u3",
                "3.0",
            ),
            (
                Value::Null,
                json!({"anyOf": [{"type": "number"}, {"type": "null"}]}),
                "maybe",
                "None",
            ),
            // Enums are generated `Enum` classes; an `IntEnum` prints its value.
            (
                json!("dark green"),
                json!({"$ref": "#/$defs/Color"}),
                "c",
                "Color.dark_green",
            ),
            (
                json!("Foo-Bar"),
                json!({"$ref": "#/$defs/Color"}),
                "c",
                "Color.Foo_Bar",
            ),
            (
                json!("1abc"),
                json!({"$ref": "#/$defs/Color"}),
                "c",
                "Color.field_1abc",
            ),
            (
                json!("class"),
                json!({"$ref": "#/$defs/Color"}),
                "c",
                "Color.class_",
            ),
            (
                json!(""),
                json!({"$ref": "#/$defs/Color"}),
                "c",
                "Color.field_",
            ),
            (json!(1.0), json!({"$ref": "#/$defs/Level"}), "l", "1"),
            (
                json!(2),
                json!({"$ref": "#/$defs/Num"}),
                "n",
                "Num.number_2",
            ),
            (
                json!(1.5),
                json!({"$ref": "#/$defs/Num"}),
                "n",
                "Num.number_1_5",
            ),
            (
                json!(1),
                json!({"$ref": "#/$defs/Mixed"}),
                "mx",
                "Mixed.int_1",
            ),
            (
                json!("x"),
                json!({"$ref": "#/$defs/lower_def"}),
                "ld",
                "LowerDef.x",
            ),
            (
                json!("a"),
                json!({"type": "string", "enum": ["a", "b"]}),
                "my_kind",
                "MyKind.a",
            ),
            // A non-enum scalar definition is a `RootModel`.
            (
                json!("Z z"),
                json!({"$ref": "#/$defs/Code"}),
                "code",
                "root='Z z'",
            ),
            (json!("fixed"), json!({"const": "fixed"}), "k", "fixed"),
        ] {
            assert_eq!(
                identity_text(value.clone(), schema.clone(), field),
                expected,
                "{value} as {schema}"
            );
        }
    }

    #[test]
    fn python_float_repr_matches_python() {
        for (number, expected) in [
            (0.1, "0.1"),
            (100.0, "100.0"),
            (1e15, "1000000000000000.0"),
            (1e-5, "1e-05"),
            (1.23e100, "1.23e+100"),
            (-0.0, "-0.0"),
            (123456789012345678.0, "1.2345678901234568e+17"),
        ] {
            assert_eq!(python_float_repr(number), expected);
        }
    }

    #[test]
    fn python_repr_quotes_strings_like_python() {
        assert_eq!(python_str_repr("plain"), "'plain'");
        assert_eq!(python_str_repr("it's"), "\"it's\"");
        assert_eq!(python_str_repr("it's \"x\""), "'it\\'s \"x\"'");
        assert_eq!(python_str_repr("a\\b\nc\u{1}"), "'a\\\\b\\nc\\x01'");
    }

    fn rating_schema() -> Value {
        json!({
            "title": "Rating", "type": "object",
            "properties": {
                "score": {"type": "number"},
                "metadata": {"type": "object", "default": {
                    "index_fields": ["score"], "identity_fields": ["score"]
                }}
            },
            "required": ["score"]
        })
    }

    #[test]
    fn number_identity_uses_python_float_text() {
        // uuid5(NAMESPACE_OID, "Rating:3.0"): Pydantic makes the `number`
        // field a float before `str()`.
        let graph = DynamicGraphSchema::compile(&rating_schema())
            .unwrap()
            .materialize(&json!({"score": 3}), Uuid::nil())
            .unwrap();
        assert_eq!(
            graph.root,
            Uuid::parse_str("ee1a468b-d889-5276-a12b-50af1b7597ed").unwrap()
        );
        // Storage sees the float too.
        assert_eq!(graph.nodes[0].properties["score"], json!(3.0));
    }

    #[test]
    fn mixed_identity_fields_match_python_ids() {
        // Expected id computed by Python: the generated model validated from
        // this answer, then `DataPoint._generate_identity_id`.
        let schema = json!({
            "$defs": {"Color": {"title": "Color", "type": "string", "enum": ["red", "dark green"]}},
            "title": "Event", "type": "object",
            "properties": {
                "when": {"type": "string", "format": "date-time"},
                "count": {"type": "integer"},
                "tags": {"type": "array", "items": {"type": "string"}},
                "color": {"$ref": "#/$defs/Color"},
                "label": {"type": "string"},
                "metadata": {"type": "object", "default": {
                    "index_fields": [],
                    "identity_fields": ["when", "count", "tags", "color", "label"]
                }}
            },
            "required": ["when", "count", "tags", "color"]
        });
        let graph = DynamicGraphSchema::compile(&schema)
            .unwrap()
            .materialize(
                &json!({
                    "when": "2024-03-01T00:00:00Z", "count": 3.0,
                    "tags": ["it's", "Big Cat"], "color": "dark green"
                }),
                Uuid::nil(),
            )
            .unwrap();
        assert_eq!(
            graph.root,
            Uuid::parse_str("543ad1bc-edea-5c1e-b9bd-94b771fe4542").unwrap()
        );
        assert_eq!(graph.nodes[0].properties["count"], json!(3));
    }

    #[test]
    fn identity_falls_back_like_python() {
        // An omitted optional field is `None` in the validated instance, so
        // it still yields a stable id: uuid5(OID, "Person:None").
        let schema = json!({
            "title": "Person", "type": "object",
            "properties": {
                "name": {"type": "string"},
                "nickname": {"type": "string"},
                "metadata": {"type": "object", "default": {
                    "index_fields": [], "identity_fields": ["nickname"]
                }}
            },
            "required": ["name"]
        });
        let graph = DynamicGraphSchema::compile(&schema)
            .unwrap()
            .materialize(&json!({"name": "Alice"}), Uuid::nil())
            .unwrap();
        assert_eq!(
            graph.root,
            Uuid::parse_str("1d5fda59-4539-574b-b8ac-63a8f56e46bb").unwrap()
        );

        // A name the model does not declare has no value at all: Python
        // falls back to a random uuid4, and so does the compiled model.
        let schema = json!({
            "title": "Ghost", "type": "object",
            "properties": {
                "name": {"type": "string"},
                "metadata": {"type": "object", "default": {
                    "index_fields": [], "identity_fields": ["nope"]
                }}
            },
            "required": ["name"]
        });
        let compiled = DynamicGraphSchema::compile(&schema)
            .expect("an unknown identity field is not a schema error in Python");
        let first = compiled
            .materialize(&json!({"name": "A"}), Uuid::nil())
            .unwrap();
        let second = compiled
            .materialize(&json!({"name": "A"}), Uuid::nil())
            .unwrap();
        assert_ne!(first.root, second.root);
        assert_eq!(first.root.get_version_num(), 4);
    }

    #[test]
    fn unknown_or_relationship_index_fields_are_skipped() {
        let mut schema = schema();
        schema["properties"]["metadata"]["default"]["index_fields"] =
            json!(["nope", "name", "employees"]);
        let compiled = DynamicGraphSchema::compile(&schema)
            .expect("unknown index fields are skipped as in index_data_points");
        assert_eq!(compiled.root_model().index_fields, ["name"]);
    }

    fn enum_schema() -> Value {
        json!({
            "$defs": {
                "Color": {"title": "Color", "type": "string", "enum": ["red", "green"]},
                "Weight": {"title": "Weight", "type": "number", "enum": [1.5, 2]},
                "Code": {"title": "Code", "type": "string", "maxLength": 3}
            },
            "title": "Paint", "type": "object",
            "properties": {
                "color": {"$ref": "#/$defs/Color"},
                "weight": {"anyOf": [{"$ref": "#/$defs/Weight"}, {"type": "null"}], "default": null},
                "accents": {"type": "array", "items": {"$ref": "#/$defs/Color"}, "default": []},
                "code": {"$ref": "#/$defs/Code"},
                "metadata": {"type": "object", "default": {
                    "index_fields": ["color"], "identity_fields": ["color"]
                }}
            },
            "required": ["color", "code"]
        })
    }

    #[test]
    fn scalar_definitions_are_values_not_models() {
        let compiled =
            DynamicGraphSchema::compile(&enum_schema()).expect("enum and scalar defs compile");
        assert!(
            compiled
                .root_model()
                .fields
                .iter()
                .all(|field| field.kind == DynamicFieldKind::Scalar)
        );
        assert!(!compiled.models.contains_key("Color"));
        // The LLM still sees the enum.
        assert_eq!(
            compiled.llm_schema()["$defs"]["Color"]["enum"],
            json!(["red", "green"])
        );

        let graph = compiled
            .materialize(
                &json!({"color": "red", "weight": 2.0, "accents": ["green"], "code": "abc"}),
                Uuid::nil(),
            )
            .expect("enum members validate");
        assert_eq!(graph.nodes.len(), 1);
        assert!(graph.relationships.is_empty());
        assert_eq!(graph.nodes[0].properties["color"], json!("red"));
        assert_eq!(
            graph.root,
            Uuid::new_v5(&Uuid::NAMESPACE_OID, b"Paint:Color.red")
        );

        for answer in [
            json!({"color": "RED", "code": "abc"}),
            json!({"color": "red", "weight": 3, "code": "abc"}),
            json!({"color": "red", "accents": ["blue"], "code": "abc"}),
            json!({"color": "red", "code": "abcd"}),
        ] {
            assert!(
                matches!(
                    compiled.materialize(&answer, Uuid::nil()),
                    Err(DynamicGraphError::InvalidScalarValue { .. })
                ),
                "{answer} must be rejected"
            );
        }
    }

    #[test]
    fn a_union_of_a_model_and_a_scalar_definition_is_rejected() {
        let mut schema = enum_schema();
        schema["$defs"]["Swatch"] = json!({
            "title": "Swatch", "type": "object", "properties": {"name": {"type": "string"}}
        });
        schema["properties"]["mixed"] =
            json!({"anyOf": [{"$ref": "#/$defs/Swatch"}, {"$ref": "#/$defs/Color"}]});
        assert_eq!(
            DynamicGraphSchema::compile(&schema).expect_err("no single storage shape"),
            DynamicGraphError::UnsupportedReferenceShape {
                model: "Paint".to_string(),
                field: "mixed".to_string(),
            }
        );
    }

    #[test]
    fn date_time_fields_must_be_timezone_aware() {
        let schema = json!({
            "title": "Meeting", "type": "object",
            "properties": {"at": {"type": "string", "format": "date-time"}},
            "required": ["at"]
        });
        let compiled = DynamicGraphSchema::compile(&schema).unwrap();
        for accepted in [
            "2024-03-01T10:00:00Z",
            "2024-03-01 10:00+01:00",
            "2024-03-01T10:00:00.25-05:00",
        ] {
            compiled
                .materialize(&json!({"at": accepted}), Uuid::nil())
                .unwrap_or_else(|error| panic!("{accepted}: {error}"));
        }
        for rejected in ["2024-03-01T10:00:00", "yesterday"] {
            assert!(
                compiled
                    .materialize(&json!({"at": rejected}), Uuid::nil())
                    .is_err(),
                "{rejected}"
            );
        }
    }

    #[test]
    fn integer_fields_accept_integral_floats_only() {
        let schema = json!({
            "title": "Count", "type": "object",
            "properties": {"n": {"type": "integer"}}, "required": ["n"]
        });
        let compiled = DynamicGraphSchema::compile(&schema).unwrap();
        let graph = compiled
            .materialize(&json!({"n": 3.0}), Uuid::nil())
            .unwrap();
        assert_eq!(graph.nodes[0].properties["n"], json!(3));
        assert!(
            compiled
                .materialize(&json!({"n": 3.5}), Uuid::nil())
                .is_err()
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

    fn person_schema() -> Value {
        json!({
            "$defs": {
                "Company": {
                    "title": "Company", "type": "object",
                    "properties": {
                        "name": {"type": "string"},
                        "metadata": {"type": "object", "default": {
                            "index_fields": ["name"], "identity_fields": ["name"]
                        }}
                    },
                    "required": ["name"]
                },
                "Person": {
                    "title": "Person", "type": "object",
                    "properties": {
                        "name": {"type": "string"},
                        "role": {"type": "string"},
                        "employer": {"anyOf": [{"$ref": "#/$defs/Company"}, {"type": "null"}], "default": null},
                        "friend": {"anyOf": [{"$ref": "#/$defs/Person"}, {"type": "null"}], "default": null},
                        "metadata": {"type": "object", "default": {
                            "index_fields": ["name"], "identity_fields": ["name"]
                        }}
                    },
                    "required": ["name"]
                }
            },
            "$ref": "#/$defs/Person"
        })
    }

    fn person_graph(answer: Value) -> DynamicGraph {
        DynamicGraphSchema::compile(&person_schema())
            .expect("schema should compile")
            .materialize(&answer, Uuid::nil())
            .expect("answer should materialize")
    }

    fn id_of(type_name: &str, name: &str) -> Uuid {
        Uuid::new_v5(
            &Uuid::NAMESPACE_OID,
            format!("{type_name}:{name}").as_bytes(),
        )
    }

    fn edge(source: Uuid, field: &str, target: Uuid) -> DynamicRelationship {
        DynamicRelationship {
            source,
            target,
            field_name: field.to_string(),
        }
    }

    fn stored_ids(stored: &StoredDynamicGraph) -> Vec<Uuid> {
        stored.nodes.iter().map(|node| node.base.id).collect()
    }

    #[test]
    fn a_node_id_is_expanded_only_at_its_first_occurrence() {
        // Python `_walk_data_point` claims Alice at her first occurrence; the
        // second is never walked, so Initech is neither stored nor linked.
        let graph = person_graph(json!({
            "name": "Bob",
            "friend": {"name": "Alice", "employer": {"name": "Acme"},
                       "friend": {"name": "Alice", "employer": {"name": "Initech"}}}
        }));
        let stored = graph.stored_graph();
        let (bob, alice, acme) = (
            id_of("Person", "bob"),
            id_of("Person", "alice"),
            id_of("Company", "acme"),
        );
        assert_eq!(stored_ids(&stored), [bob, alice, acme]);
        assert_eq!(
            stored.relationships,
            [
                edge(bob, "friend", alice),
                edge(alice, "employer", acme),
                // The edge to an already-claimed node is still kept.
                edge(alice, "friend", alice),
            ]
        );
    }

    #[test]
    fn the_first_occurrence_of_an_id_keeps_its_properties() {
        // A diamond: two paths reach the same id with different values.
        let graph = person_graph(json!({
            "name": "Root",
            "employer": null,
            "friend": {"name": "Left", "friend": {"name": "Zed", "role": "first"}},
        }));
        let mut extra = person_graph(json!({
            "name": "Right", "friend": {"name": "Zed", "role": "second"}
        }));
        // Hang the second path off the root too: Root -> Left -> Zed and
        // Root -> Right -> Zed'.
        let right = extra.root_occurrence;
        let mut diamond = graph.clone();
        diamond.nodes.append(&mut extra.nodes);
        diamond.relationships.append(&mut extra.relationships);
        diamond
            .relationships
            .push(edge(graph.root_occurrence, "friend", right));

        let stored = diamond.stored_graph();
        let zed: Vec<&DynamicDataPoint> = stored
            .nodes
            .iter()
            .filter(|node| node.base.id == id_of("Person", "zed"))
            .collect();
        assert_eq!(zed.len(), 1);
        assert_eq!(zed[0].properties["role"], json!("first"));
        assert!(stored.relationships.contains(&edge(
            id_of("Person", "right"),
            "friend",
            id_of("Person", "zed")
        )));
        assert_eq!(stored.nodes.len(), 4);
    }

    #[test]
    fn a_cycle_by_id_terminates_and_keeps_both_edges() {
        let graph = person_graph(json!({
            "name": "Alice",
            "friend": {"name": "Bob", "friend": {"name": "Alice", "friend": {"name": "Carol"}}}
        }));
        let stored = graph.stored_graph();
        let (alice, bob) = (id_of("Person", "alice"), id_of("Person", "bob"));
        assert_eq!(stored_ids(&stored), [alice, bob]);
        assert_eq!(
            stored.relationships,
            [edge(alice, "friend", bob), edge(bob, "friend", alice)]
        );
    }

    #[test]
    fn a_batch_claims_ids_across_chunks_like_add_data_points() {
        // The audit repro: chunk 1 says Alice works at Acme, chunk 2 at
        // Initech. Python's batch walk stores only Alice -> Acme.
        let first = person_graph(json!({"name": "Alice", "employer": {"name": "Acme"}}));
        let second = person_graph(json!({"name": "Alice", "employer": {"name": "Initech"}}));
        let (alice, acme, initech) = (
            id_of("Person", "alice"),
            id_of("Company", "acme"),
            id_of("Company", "initech"),
        );

        let direct = store_dynamic_graph_batch([&first, &second], ChunkAttachment::Direct);
        assert_eq!(stored_ids(&direct[0]), [alice, acme]);
        assert_eq!(direct[0].relationships, [edge(alice, "employer", acme)]);
        assert!(direct[1].nodes.is_empty());
        assert!(direct[1].relationships.is_empty());
        // Chunk 2 still links to the stored Alice.
        assert_eq!(second.direct_attachment_targets(), [alice]);

        // `all` puts Initech in chunk 2's `contains`, so the batch walk
        // reaches and stores it, but never re-walks Alice to add her edge.
        let all = store_dynamic_graph_batch([&first, &second], ChunkAttachment::All);
        assert_eq!(stored_ids(&all[0]), [alice, acme]);
        assert_eq!(stored_ids(&all[1]), [initech]);
        assert!(all[1].relationships.is_empty());
        assert_eq!(second.all_attachment_targets(), [alice, initech]);

        // Each graph alone keeps its own edge.
        assert_eq!(
            second.stored_graph().relationships,
            [edge(alice, "employer", initech)]
        );
    }

    #[test]
    fn a_transparent_root_links_each_child_id_once() {
        // Python `unwrap_transparent` dedupes a container's children by id.
        let graph = DynamicGraphSchema::compile(&schema())
            .unwrap()
            .materialize(
                &json!({
                    "name": "Container",
                    "employees": [{"name": "Alice"}, {"name": "Bob"}, {"name": "Alice"}]
                }),
                Uuid::nil(),
            )
            .unwrap();
        assert_eq!(
            graph.direct_attachment_targets(),
            [id_of("Person", "alice"), id_of("Person", "bob")]
        );
        assert_eq!(graph.stored_graph().nodes.len(), 2);
    }

    #[test]
    fn only_truthy_transparent_scalars_count_as_dropped() {
        let mut base = DataPoint::new("Box", Some(Uuid::nil()));
        base.id = Uuid::new_v4();
        let node = DynamicDataPoint {
            occurrence_id: base.id,
            base,
            properties: Map::from_iter(
                [
                    ("label", json!("kept")),
                    ("count", json!(2)),
                    ("flag", json!(true)),
                    ("tags", json!(["x"])),
                    ("empty", json!("")),
                    ("zero", json!(0)),
                    ("zero_float", json!(0.0)),
                    ("off", json!(false)),
                    ("none", Value::Null),
                    ("no_tags", json!([])),
                    ("no_map", json!({})),
                ]
                .map(|(field, value)| (field.to_string(), value)),
            ),
            index_fields: Vec::new(),
            transparent: true,
        };
        assert_eq!(
            dropped_transparent_fields(&node),
            ["label", "count", "flag", "tags"]
        );
        let key = format!("dropped_field:Test{}.label", Uuid::new_v4());
        assert!(first_warning(&key));
        assert!(!first_warning(&key), "a class and field warn once");
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

    #[test]
    fn index_texts_skip_null_and_blank_values_like_python() {
        let schema = json!({
            "title": "Note", "type": "object",
            "properties": {
                "title": {"anyOf": [{"type": "string"}, {"type": "null"}], "default": null},
                "body": {"type": "string"},
                "score": {"type": "integer"},
                "metadata": {"type": "object", "default": {
                    "index_fields": ["title", "body", "score"]
                }}
            }
        });
        let graph = DynamicGraphSchema::compile(&schema)
            .expect("schema should compile")
            .materialize(
                &json!({"title": null, "body": "  hello  ", "score": 3}),
                Uuid::nil(),
            )
            .expect("answer should materialize");
        let node = &graph.nodes[0];
        let texts: Vec<(&str, String)> = node.index_field_texts().collect();
        assert_eq!(
            texts,
            [("body", "hello".to_string()), ("score", "3".to_string())]
        );
        assert_eq!(node.embeddable_text(), "hello 3");
        assert_eq!(node.label(), "hello");

        // Nothing indexable: the label falls back to the type name.
        let graph = DynamicGraphSchema::compile(&schema)
            .expect("schema should compile")
            .materialize(&json!({"body": " "}), Uuid::nil())
            .expect("answer should materialize");
        assert_eq!(graph.nodes[0].label(), "Note");
    }
}
