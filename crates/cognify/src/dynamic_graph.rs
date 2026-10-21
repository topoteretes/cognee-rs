//! Runtime custom graph models compiled from Pydantic-compatible JSON Schema.
//!
//! Python can manufacture `DataPoint` subclasses at runtime. Rust cannot, so
//! this module compiles the same serialized schema boundary into a small
//! descriptor and materializes LLM answers into an arena of dynamic nodes and
//! field relationships.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Mutex, OnceLock, PoisonError};

use cognee_models::DataPoint;
use serde_json::{Map, Value};
use thiserror::Error;
use tracing::warn;
use unicode_normalization::UnicodeNormalization;
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
    /// The `metadata` default as declared, which Python's instance holds as
    /// its `metadata` value.
    metadata: Option<Value>,
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
    /// Python `str()` of each non-null scalar property as the Pydantic
    /// instance holds it: an enum member prints `Role.lead_dev`, a boolean
    /// `True`, a `date-time` the `datetime` text, a list its `repr`, a scalar
    /// `$defs` value `root=...`. A schema default Pydantic left unvalidated
    /// prints as the raw JSON value. Edge labels and triplet text use these.
    pub property_strs: BTreeMap<String, String>,
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

    /// Each declared index field's Python `str()`, stripped, skipping a
    /// `None` and a blank text — the values Python's `_get_node_label` and
    /// `_extract_embeddable_text_from_datapoint` read.
    fn index_field_strs(&self) -> impl Iterator<Item = &str> + '_ {
        self.index_fields.iter().filter_map(|field| {
            let text = self.property_strs.get(field)?.trim();
            (!text.is_empty()).then_some(text)
        })
    }

    /// Python `_extract_embeddable_text_from_datapoint`: every indexable
    /// field's `str()` joined with a space.
    pub fn embeddable_text(&self) -> String {
        self.index_field_strs().collect::<Vec<_>>().join(" ")
    }

    /// Python `_get_node_label`: the first indexable field's `str()` trimmed
    /// by `_trim_preview`, else a nonblank `name` (only stripped), else the
    /// type name. The result is final: insert it with
    /// [`crate::graph_extraction::EdgeLabels::insert_label`].
    pub fn label(&self) -> String {
        if let Some(text) = self.index_field_strs().next() {
            return crate::graph_extraction::trim_preview(text);
        }
        self.property_strs
            .get("name")
            .map(|name| name.trim())
            .filter(|name| !name.is_empty())
            .map_or_else(|| self.base.data_type.clone(), str::to_string)
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
    let metadata_default = properties
        .get("metadata")
        .and_then(Value::as_object)
        .and_then(|metadata| metadata.get("default"));
    let metadata = metadata_default.and_then(Value::as_object);
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
        } else if first_warning(&format!("index_field:{name}.{field}")) {
            // Once per process: `validate()` compiles the schema at every
            // entry point, so an unkeyed warning repeats several times a call.
            warn!(
                model = name,
                field = %field,
                "custom graph index field is not a scalar field of the model; it is not indexed"
            );
        }
    }
    // Python derives the id from these fields at runtime: an unknown name
    // makes every instance fall back to a random uuid4, and a relationship
    // field or a per-instance base field (`id`, timestamps) is random too.
    // None of that is an error there, so the names are kept and only warned
    // about. Other DataPoint base fields hold fixed defaults and give a
    // stable id, as in Python.
    for field in &identity_fields {
        let stable = match field_kind(field) {
            Some(DynamicFieldKind::Scalar) => true,
            Some(DynamicFieldKind::Relationship { .. }) => false,
            None => !matches!(
                base_identity_text(field, name, metadata_default),
                None | Some(BaseIdentity::Random)
            ),
        };
        if !stable && first_warning(&format!("identity_field:{name}.{field}")) {
            warn!(
                model = name,
                field = %field,
                "custom graph identity field has no stable value; node ids fall back to \
                 random UUIDs"
            );
        }
    }

    Ok(DynamicModelSchema {
        name: name.to_string(),
        identity_fields,
        index_fields,
        transparent,
        fields,
        metadata: metadata_default.cloned(),
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
/// Python's `graph_schema_to_graph_model()` lookup `namespace[schema["title"]]` finds the
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
/// inside a field schema, following references into scalar definitions: a
/// non-object `$defs` entry such as `People = list[Person]` wraps models and
/// is not a scalar value either.
fn contains_model_reference(value: &Value, names: &SchemaNames<'_>) -> bool {
    contains_model_reference_from(value, names, &mut HashSet::new())
}

fn contains_model_reference_from<'v>(
    value: &'v Value,
    names: &SchemaNames<'v>,
    followed: &mut HashSet<&'v str>,
) -> bool {
    match value {
        Value::Object(map) => {
            if let Some(reference) = map.get("$ref").and_then(Value::as_str) {
                let Some((target, definition)) = reference
                    .strip_prefix("#/$defs/")
                    .and_then(|target| names.scalars.get_key_value(target))
                else {
                    return true;
                };
                if followed.insert(target.as_str())
                    && contains_model_reference_from(definition, names, followed)
                {
                    return true;
                }
            }
            map.values()
                .any(|value| contains_model_reference_from(value, names, followed))
        }
        Value::Array(values) => values
            .iter()
            .any(|value| contains_model_reference_from(value, names, followed)),
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
        let mut scalar_python_values: HashMap<&str, PyValue> = HashMap::new();
        let mut relationship_values = Vec::new();

        // Pydantic does not validate a default: the instance holds the raw
        // JSON value, so it is neither checked, coerced nor typed.
        let mut defaulted = HashSet::new();
        for field in &model.fields {
            let value = match object.get(&field.name) {
                Some(value) => Some(value),
                None => {
                    defaulted.insert(field.name.as_str());
                    field.default.as_ref()
                }
            };
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
                DynamicFieldKind::Scalar if defaulted.contains(field.name.as_str()) => {
                    properties.insert(field.name.clone(), value.clone());
                }
                DynamicFieldKind::Scalar => {
                    let definitions = &self.schema.scalar_definitions;
                    // Store what Pydantic hands to storage: the validated,
                    // coerced value rather than the answer's raw JSON, and
                    // keep the Python object it became for the identity.
                    let coerced = coerce_scalar_value(
                        value,
                        &field.schema,
                        definitions,
                        Some(&python_class_name(&field.name)),
                        0,
                    )
                    .map_err(|reason| {
                        DynamicGraphError::InvalidScalarValue {
                            model: model_name.to_string(),
                            field: field.name.clone(),
                            reason,
                        }
                    })?;
                    properties.insert(field.name.clone(), coerced.stored);
                    scalar_python_values.insert(field.name.as_str(), coerced.python);
                }
                DynamicFieldKind::Relationship { targets, many } => {
                    relationship_values.push((field.name.clone(), targets, *many, value));
                }
            }
        }

        let property_python_values: BTreeMap<&str, PyValue> = model
            .fields
            .iter()
            .filter_map(|field| {
                let value = properties.get(&field.name)?;
                let python = match scalar_python_values.remove(field.name.as_str()) {
                    Some(python) if !defaulted.contains(field.name.as_str()) => python,
                    _ => plain_python_value(value),
                };
                Some((field.name.as_str(), python))
            })
            .collect();
        let id = dynamic_id(model, object, &property_python_values);
        let property_strs = property_python_values
            .iter()
            .filter(|(_, value)| **value != PyValue::None)
            .map(|(field, value)| ((*field).to_string(), value.to_str()))
            .collect();
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
            property_strs,
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
    /// occurrence id. The first declared model that validates wins, keys it
    /// does not declare being ignored: Python's generated models subclass
    /// `DataPoint`, whose custom `__init__` keeps Pydantic's smart-mode union
    /// from ranking candidates by how many fields they set, so it falls back
    /// to left-to-right order. A rejected candidate leaves nothing behind.
    fn visit_any(&mut self, models: &[String], value: &Value) -> Result<Uuid, DynamicGraphError> {
        let mut first_error = None;
        for model in models {
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
/// the Pydantic-validated values of the identity fields, each `str()`-ed
/// (`properties` holds them; a field filled from its schema default holds the
/// raw JSON value, as Pydantic leaves defaults unvalidated).
///
/// Every declared field is in the validated instance: an omitted optional
/// field holds its `None` default, so it contributes `"None"`. An undeclared
/// DataPoint base field holds its default (see [`base_identity_text`]); only
/// `id`, the timestamps and a name that is neither make it random. A
/// relationship value is a whole node whose `str()` carries fresh timestamps,
/// so it is random in Python too; only an empty one (`None`, `[]`) is stable.
fn dynamic_id(
    model: &DynamicModelSchema,
    answer: &Map<String, Value>,
    properties: &BTreeMap<&str, PyValue>,
) -> Uuid {
    if model.identity_fields.is_empty() {
        return Uuid::new_v4();
    }
    let mut values = Vec::with_capacity(model.identity_fields.len());
    for name in &model.identity_fields {
        let Some(field) = model.fields.iter().find(|field| field.name == *name) else {
            match base_identity_text(name, &model.name, model.metadata.as_ref()) {
                Some(BaseIdentity::Fixed(text)) => {
                    values.push(text);
                    continue;
                }
                Some(BaseIdentity::Random) | None => return Uuid::new_v4(),
            }
        };
        let text = match field.kind {
            DynamicFieldKind::Scalar => properties
                .get(name.as_str())
                .map_or_else(|| "None".to_string(), python_identity_text),
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

/// The identity value of a DataPoint base field the schema does not declare.
enum BaseIdentity {
    /// The normalized text of the fixed value Python's fresh instance holds.
    Fixed(String),
    /// A per-instance value (`id`, `created_at`, `updated_at`).
    Random,
}

/// What a fresh Python instance holds for an inherited DataPoint field when
/// `_generate_identity_id` reads it from `self.__dict__`: `type` is already
/// the class name, `metadata` the declared default, and every other field
/// its `DataPoint` default (cognee @ f154b32). `None` for a name that is not
/// a base field, which makes Python fall back to uuid4.
fn base_identity_text(
    field: &str,
    model_name: &str,
    metadata: Option<&Value>,
) -> Option<BaseIdentity> {
    let text = match field {
        "id" | "created_at" | "updated_at" => return Some(BaseIdentity::Random),
        "type" => python_identity_text(&PyValue::Str(model_name.to_string())),
        "metadata" => plain_python_value(metadata.unwrap_or(&Value::Null)).to_str(),
        "ontology_valid" => "False".to_string(),
        "version" => "1".to_string(),
        "topological_rank" => "0".to_string(),
        "feedback_weight" | "importance_weight" => "0.5".to_string(),
        "ontology_uri"
        | "valid_to"
        | "belongs_to_set"
        | "source_pipeline"
        | "source_task"
        | "source_node_set"
        | "source_user"
        | "source_content_hash" => "None".to_string(),
        _ => return None,
    };
    Some(BaseIdentity::Fixed(text))
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
    /// A value whose `str()` and `repr()` are not derived from a plain JSON
    /// shape: an aware `datetime`, a `date`, a `time` or a `UUID`.
    Opaque {
        text: String,
        repr: String,
    },
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
            Self::Str(text) | Self::Opaque { text, .. } => text.clone(),
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
            Self::Opaque { repr, .. } => repr.clone(),
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

/// datamodel-code-generator's enum member names for `choices`, in order, as
/// `parse_enum` assigns them: a string value (or any value of a `string`
/// enum) is named by its text; any other value by `<type>_<str(value)>`,
/// with the schema type or, for an untyped enum, its Python type
/// (`integer_1`, `number_1_5`, `int_1`, `bool_True`). Each name then goes
/// through [`python_enum_member_identifier`], which sees the names already
/// taken so collisions get a numeric suffix. A `null` member of a `string`
/// enum makes the enum nullable instead of naming a member.
fn python_enum_member_names(choices: &[Value], kind: Option<&str>) -> Vec<Option<String>> {
    let mut taken = HashSet::new();
    choices
        .iter()
        .map(|choice| {
            if choice.is_null() && kind == Some("string") {
                return None;
            }
            let text = python_enum_value_text(choice);
            let source = if kind == Some("string") || choice.is_string() {
                text
            } else {
                let prefix = kind.unwrap_or(match choice {
                    Value::Null => "NoneType",
                    Value::Bool(_) => "bool",
                    Value::Number(number) if number.is_f64() => "float",
                    Value::Number(_) => "int",
                    Value::Array(_) => "list",
                    _ => "dict",
                });
                format!("{prefix}_{text}")
            };
            let name = python_enum_member_identifier(&source, &taken);
            taken.insert(name.clone());
            // Python normalizes identifiers to NFKC when it parses the
            // generated module, so `field_Ⅷ` is the member `field_VIII`.
            Some(if name.is_ascii() {
                name
            } else {
                name.nfkc().collect()
            })
        })
        .collect()
}

/// Python `str()` of a JSON enum value as `json.loads` builds it.
fn python_enum_value_text(choice: &Value) -> String {
    match choice {
        Value::String(text) => text.clone(),
        Value::Number(number) if number.is_f64() => number
            .as_f64()
            .map_or_else(|| number.to_string(), python_float_repr),
        Value::Number(number) => number.to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Null => "None".to_string(),
        other => other.to_string(),
    }
}

/// Superscript digits datamodel-code-generator replaces even though Python's
/// `\w` matches them (`⁰` is not among them).
const SUPERSCRIPT_DIGITS: &str = "¹²³⁴⁵⁶⁷⁸⁹";

/// datamodel-code-generator's `EnumFieldNameResolver.get_valid_name`: `mro`
/// is renamed, an empty name becomes `_` and a leading `#` is dropped; every
/// superscript digit and non-word character becomes `_`; a leading numeric
/// character gets a `field_` prefix and a leading `_` a `field` prefix; a
/// keyword gets a `_` suffix; a name that is still not an identifier falls
/// back to `_uXXXX_` escapes; and a name already in `taken` (or `mro`) gets
/// `_1`, `_2`, ... appended.
///
/// Unicode classes come from Rust's tables rather than the target Python's,
/// so a few non-ASCII characters classify differently: a combining mark that
/// Rust counts as alphabetic is kept where Python's `\w` replaces it, and a
/// numeric ideograph such as `一` gets no `field_` prefix. ASCII values are
/// exact.
fn python_enum_member_identifier(source: &str, taken: &HashSet<String>) -> String {
    let mut name = if source == "mro" {
        "mro_".to_string()
    } else {
        source.to_string()
    };
    if name.is_empty() {
        name = "_".to_string();
    }
    if let Some(rest) = name.strip_prefix('#') {
        name = if rest.is_empty() {
            "_".to_string()
        } else {
            rest.to_string()
        };
    }
    name = name
        .chars()
        .map(|c| {
            if SUPERSCRIPT_DIGITS.contains(c) || !(c.is_alphanumeric() || c == '_') {
                '_'
            } else {
                c
            }
        })
        .collect();
    if name.starts_with(char::is_numeric) {
        name = format!("field_{name}");
    }
    if name.starts_with('_') {
        name = format!("field{name}");
    }
    if PYTHON_KEYWORDS.contains(&name.as_str()) {
        name.push('_');
    }
    let excluded = |candidate: &str| candidate == "mro" || taken.contains(candidate);
    let mut candidate = name.clone();
    let mut count = 1;
    while !is_python_identifier(&candidate)
        || PYTHON_KEYWORDS.contains(&candidate.as_str())
        || excluded(&candidate)
    {
        if !candidate.is_ascii() && !is_python_identifier(&candidate) {
            name = python_ascii_identifier_fallback(&name);
            count = 1;
            candidate = name.clone();
            continue;
        }
        candidate = format!("{name}_{count}");
        count += 1;
    }
    candidate
}

/// Python `str.isidentifier()`.
fn is_python_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|first| first == '_' || unicode_ident::is_xid_start(first))
        && chars.all(unicode_ident::is_xid_continue)
}

/// datamodel-code-generator's `_ascii_identifier_fallback`.
fn python_ascii_identifier_fallback(name: &str) -> String {
    let fallback: String = name
        .chars()
        .map(|c| {
            if c == '_' || c.is_ascii_alphanumeric() {
                c.to_string()
            } else {
                format!("_u{:x}_", c as u32)
            }
        })
        .collect();
    if fallback.starts_with('_') {
        format!("field{fallback}")
    } else if fallback.is_empty() || fallback.starts_with(char::is_numeric) {
        format!("field_{fallback}")
    } else {
        fallback
    }
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
pub(crate) fn python_str_repr(text: &str) -> String {
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

type CharStream<'a> = std::iter::Peekable<std::str::Chars<'a>>;

/// `HH:MM`, optional `:SS` with an optional `.` or `,` fraction (truncated to
/// microseconds), as speedate reads a time of day. A second of 60 is refused.
fn parse_clock(chars: &mut CharStream<'_>) -> Option<chrono::NaiveTime> {
    let hour = fixed_digits(chars, 2)?;
    expect_char(chars, &[':'])?;
    let minute = fixed_digits(chars, 2)?;
    let mut second = 0;
    let mut micros = 0;
    if chars.next_if_eq(&':').is_some() {
        second = fixed_digits(chars, 2)?;
        if chars.next_if(|c| matches!(c, '.' | ',')).is_some() {
            let mut seen = 0;
            while let Some(digit) = chars.next_if(char::is_ascii_digit) {
                if seen < 6 {
                    micros = micros * 10 + digit.to_digit(10)?;
                }
                seen += 1;
            }
            if seen == 0 {
                return None;
            }
            micros *= 10u32.pow(6u32.saturating_sub(seen));
        }
    }
    chrono::NaiveTime::from_hms_micro_opt(hour, minute, second, micros)
}

/// An optional UTC offset after a clock: `Some(None)` when the text ends
/// here, `Some(Some(seconds))` for `Z`/`z` or `±HH:MM`/`±HHMM` (`−` counts as
/// a minus), `None` when what follows is not an offset.
fn parse_offset(chars: &mut CharStream<'_>) -> Option<Option<i32>> {
    match chars.next() {
        None => Some(None),
        Some('Z' | 'z') => Some(Some(0)),
        Some(sign @ ('+' | '-' | '\u{2212}')) => {
            let hours = fixed_digits(chars, 2)?;
            chars.next_if_eq(&':');
            let minutes = fixed_digits(chars, 2)?;
            if hours >= 24 || minutes >= 60 {
                return None;
            }
            let magnitude = i32::try_from(hours * 3600 + minutes * 60).ok()?;
            Some(Some(if sign == '+' { magnitude } else { -magnitude }))
        }
        Some(_) => None,
    }
}

/// `YYYY-MM-DD` with a year of at least 1.
fn parse_date_chars(chars: &mut CharStream<'_>) -> Option<chrono::NaiveDate> {
    let year = fixed_digits(chars, 4)?;
    expect_char(chars, &['-'])?;
    let month = fixed_digits(chars, 2)?;
    expect_char(chars, &['-'])?;
    let day = fixed_digits(chars, 2)?;
    if year == 0 {
        return None;
    }
    chrono::NaiveDate::from_ymd_opt(i32::try_from(year).ok()?, month, day)
}

/// A bare `YYYY-MM-DD` date and nothing else.
fn parse_date_text(text: &str) -> Option<chrono::NaiveDate> {
    let mut chars = text.chars().peekable();
    let date = parse_date_chars(&mut chars)?;
    chars.next().is_none().then_some(date)
}

/// A time of day with an optional offset (`03:04:05`, `03:04+02:00`).
fn parse_time_text(text: &str) -> Option<(chrono::NaiveTime, Option<i32>)> {
    let mut chars = text.chars().peekable();
    let time = parse_clock(&mut chars)?;
    let offset = parse_offset(&mut chars)?;
    chars.next().is_none().then_some((time, offset))
}

/// An ISO 8601 date and time as speedate reads it: `YYYY-MM-DD`, a `T`, `t`,
/// `_` or space separator, a clock, then an optional offset. Nothing may
/// surround it.
fn parse_iso_datetime(text: &str) -> Option<(chrono::NaiveDateTime, Option<i32>)> {
    let mut chars = text.chars().peekable();
    let date = parse_date_chars(&mut chars)?;
    expect_char(&mut chars, &['T', 't', '_', ' '])?;
    let time = parse_clock(&mut chars)?;
    let offset = parse_offset(&mut chars)?;
    chars
        .next()
        .is_none()
        .then_some((date.and_time(time), offset))
}

/// Parse a string the way Pydantic's `AwareDatetime` (speedate) accepts an
/// ISO 8601 datetime in JSON: `YYYY-MM-DD`, a `T`, `t`, `_` or space
/// separator, `HH:MM`, optional `:SS` with an optional `.` or `,` fraction
/// (truncated to microseconds), then a `Z`/`z` or `±HH:MM`/`±HHMM` offset
/// (`−` counts as a minus). Nothing may surround it, a second of 60 is
/// rejected, and a naive datetime is rejected. Unix-timestamp text is not
/// ISO; [`parse_aware_datetime_lax`] adds it.
fn parse_aware_datetime(text: &str) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    use chrono::TimeZone;

    let (naive, offset) = parse_iso_datetime(text)?;
    chrono::FixedOffset::east_opt(offset?)?
        .from_local_datetime(&naive)
        .single()
}

/// speedate's watershed between a Unix timestamp in seconds and one in
/// milliseconds: a magnitude above it is read as milliseconds.
const TIMESTAMP_MS_WATERSHED: i64 = 20_000_000_000;

/// The UTC datetime of Unix timestamp `ts`, read as milliseconds when
/// `millis` and as seconds otherwise, plus `extra_micros`. Years outside
/// 1..=9999 are refused.
fn naive_from_timestamp(ts: i64, millis: bool, extra_micros: i64) -> Option<chrono::NaiveDateTime> {
    use chrono::Datelike;

    let (mut seconds, mut micros) = if millis {
        (ts.div_euclid(1000), ts.rem_euclid(1000) * 1000)
    } else {
        (ts, 0)
    };
    micros += extra_micros;
    seconds = seconds.checked_add(micros.div_euclid(1_000_000))?;
    micros = micros.rem_euclid(1_000_000);
    let nanos = u32::try_from(micros).ok()?.checked_mul(1000)?;
    let naive = chrono::DateTime::from_timestamp(seconds, nanos)?.naive_utc();
    (1..=9999).contains(&naive.year()).then_some(naive)
}

/// Whether an integer timestamp is read as milliseconds.
fn is_millisecond_timestamp(magnitude: f64) -> bool {
    magnitude > TIMESTAMP_MS_WATERSHED as f64
}

/// The whole seconds of a fractional timestamp, or `None` when it does not
/// fit an `i64`.
fn timestamp_floor(float: f64) -> Option<i64> {
    let floor = float.floor();
    // The bound keeps the cast inside `i64`.
    (floor.is_finite() && floor.abs() < 9.2e18).then_some(floor as i64)
}

/// A JSON number as a Unix timestamp. pydantic-core floors a float and adds
/// the absolute value of its fractional part, so a negative fraction runs
/// the wrong way (`-1.25` is `23:59:58.25`). The fraction is read as
/// milliseconds when the whole float is past the watershed, while the whole
/// seconds are judged on their own.
fn naive_from_json_timestamp(number: &serde_json::Number) -> Option<chrono::NaiveDateTime> {
    if let Some(ts) = number.as_i64() {
        return naive_from_timestamp(ts, is_millisecond_timestamp(ts.unsigned_abs() as f64), 0);
    }
    if number.is_u64() {
        // Beyond `i64`: every year it names is out of range.
        return None;
    }
    let float = number.as_f64()?;
    let ts = timestamp_floor(float)?;
    let scale = if is_millisecond_timestamp(float.abs()) {
        1_000.0
    } else {
        1_000_000.0
    };
    naive_from_timestamp(
        ts,
        is_millisecond_timestamp(ts.unsigned_abs() as f64),
        (float.fract().abs() * scale).round() as i64,
    )
}

/// Unix-timestamp text as speedate falls back to when ISO parsing fails:
/// `[+-]digits`, or a decimal containing a `.` (an exponent is allowed there:
/// `1.7e9`, but `1e9` is not a timestamp). A text fraction runs the right
/// way, unlike a JSON float, and the watershed is judged on the whole value.
fn naive_from_timestamp_text(text: &str) -> Option<chrono::NaiveDateTime> {
    if is_integer_timestamp_text(text) {
        let ts = text.parse::<i64>().ok()?;
        return naive_from_timestamp(ts, is_millisecond_timestamp(ts.unsigned_abs() as f64), 0);
    }
    if text.contains('.') {
        let float: f64 = text.parse().ok()?;
        let ts = timestamp_floor(float)?;
        let millis = is_millisecond_timestamp(float.abs());
        let scale = if millis { 1_000.0 } else { 1_000_000.0 };
        return naive_from_timestamp(ts, millis, ((float - float.floor()) * scale).round() as i64);
    }
    None
}

/// Whether `text` is `[+-]digits`, an integer Unix timestamp.
fn is_integer_timestamp_text(text: &str) -> bool {
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

/// An aware datetime from ISO text, Unix-timestamp text or a number, as
/// Pydantic's `AwareDatetime` takes JSON. A timestamp is UTC.
fn parse_aware_datetime_lax(value: &Value) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    use chrono::TimeZone;

    let utc = chrono::FixedOffset::east_opt(0)?;
    let naive = match value {
        Value::String(text) => {
            if let Some(aware) = parse_aware_datetime(text) {
                return Some(aware);
            }
            naive_from_timestamp_text(text)?
        }
        Value::Number(number) => naive_from_json_timestamp(number)?,
        _ => return None,
    };
    utc.from_local_datetime(&naive).single()
}

/// A `date` from `YYYY-MM-DD`, from a datetime text or timestamp that falls
/// exactly on midnight (any offset is ignored), or from a timestamp number,
/// with how exactly it matched. speedate's own date parser reads a date and
/// integer timestamp text (strict); pydantic-core's fallback through a
/// datetime, and any JSON number, is lax.
fn parse_date_lax(value: &Value) -> Option<(chrono::NaiveDate, Exactness)> {
    let (naive, exactness) = match value {
        Value::String(text) => {
            if let Some(date) = parse_date_text(text) {
                return Some((date, Exactness::Strict));
            }
            match parse_iso_datetime(text) {
                Some((naive, _)) => (naive, Exactness::Lax),
                None if is_integer_timestamp_text(text) => {
                    (naive_from_timestamp_text(text)?, Exactness::Strict)
                }
                None => (naive_from_timestamp_text(text)?, Exactness::Lax),
            }
        }
        Value::Number(number) => (naive_from_json_timestamp(number)?, Exactness::Lax),
        _ => return None,
    };
    (naive.time() == chrono::NaiveTime::MIN).then(|| (naive.date(), exactness))
}

/// A `time` from `HH:MM[:SS[.f]]` with an optional offset, or from a number
/// of seconds since midnight below 86400 (which Pydantic reads as UTC). Text
/// holding a number is refused.
fn parse_time_lax(value: &Value) -> Option<(chrono::NaiveTime, Option<i32>)> {
    match value {
        Value::String(text) => parse_time_text(text),
        Value::Number(number) => {
            let (seconds, micros) = match number.as_i64() {
                Some(seconds) => (seconds, 0),
                None if number.is_u64() => return None,
                None => {
                    let float = number.as_f64()?;
                    if !float.is_finite() || float < 0.0 {
                        return None;
                    }
                    (
                        float.floor() as i64,
                        (float.fract() * 1_000_000.0).round() as i64,
                    )
                }
            };
            let total = seconds.checked_mul(1_000_000)?.checked_add(micros)?;
            if !(0..86_400_000_000).contains(&total) {
                return None;
            }
            let time = chrono::NaiveTime::from_num_seconds_from_midnight_opt(
                u32::try_from(total / 1_000_000).ok()?,
                u32::try_from(total % 1_000_000).ok()?.checked_mul(1000)?,
            )?;
            Some((time, Some(0)))
        }
        _ => None,
    }
}

/// Exactly `width` ASCII digits as a number.
fn fixed_digits(chars: &mut CharStream<'_>, width: usize) -> Option<u32> {
    let mut value = 0;
    for _ in 0..width {
        value = value * 10 + chars.next_if(char::is_ascii_digit)?.to_digit(10)?;
    }
    Some(value)
}

fn expect_char(chars: &mut CharStream<'_>, allowed: &[char]) -> Option<()> {
    chars.next_if(|c| allowed.contains(c)).map(|_| ())
}

/// Python `isoformat()` / `str()` of a time: `HH:MM:SS`, `.ffffff` when
/// there are microseconds, then the `+HH:MM` offset when there is one.
fn python_time_str(time: chrono::NaiveTime, offset: Option<i32>) -> String {
    use chrono::Timelike;

    let mut text = time.format("%H:%M:%S").to_string();
    let micros = time.nanosecond() / 1_000 % 1_000_000;
    if micros != 0 {
        text.push_str(&format!(".{micros:06}"));
    }
    if let Some(offset) = offset {
        text.push_str(&python_offset_str(offset));
    }
    text
}

/// The `+HH:MM` offset text Python prints (with `:SS` for an odd offset).
fn python_offset_str(offset: i32) -> String {
    let (sign, magnitude) = if offset < 0 {
        ('-', -offset)
    } else {
        ('+', offset)
    };
    let mut text = format!("{sign}{:02}:{:02}", magnitude / 3600, magnitude % 3600 / 60);
    if magnitude % 60 != 0 {
        text.push_str(&format!(":{:02}", magnitude % 60));
    }
    text
}

/// The constructor arguments Python's `repr` prints for a time of day: the
/// seconds and microseconds only when they are not zero.
fn python_clock_repr(time: chrono::NaiveTime) -> String {
    use chrono::Timelike;

    let micros = time.nanosecond() / 1_000 % 1_000_000;
    let mut text = format!("{}, {}", time.hour(), time.minute());
    if time.second() != 0 || micros != 0 {
        text.push_str(&format!(", {}", time.second()));
    }
    if micros != 0 {
        text.push_str(&format!(", {micros}"));
    }
    text
}

/// pydantic-core's `tzinfo=TzInfo(<seconds>)` suffix of a repr.
fn python_tzinfo_repr(offset: Option<i32>) -> String {
    offset.map_or_else(String::new, |offset| format!(", tzinfo=TzInfo({offset})"))
}

/// Coerce a value for a `string` field of `format` as Pydantic does: the
/// form Python's storage encoder writes (`isoformat()` for a datetime, date
/// or time, the hyphenated text of a UUID), the `datetime`, `date`, `time` or
/// `UUID` Python holds, and how exactly the input matched. `None` when the
/// field has no such format.
fn coerce_formatted(format: &str, value: &Value) -> Option<Result<Coerced, String>> {
    use chrono::Datelike;

    // pydantic-core reads JSON text for a datetime or time strictly and a
    // JSON number laxly; a UUID string is an exact match.
    let text_exactness = if value.is_string() {
        Exactness::Strict
    } else {
        Exactness::Lax
    };
    let coerced = match format {
        "date-time" => parse_aware_datetime_lax(value).map(|date| {
            let naive = date.naive_local();
            let offset = date.offset().local_minus_utc();
            let text = python_datetime_str(&date);
            let mut iso = text.clone();
            // `isoformat()` separates the date from the time with a `T`.
            iso.replace_range(10..11, "T");
            Coerced {
                stored: Value::String(iso),
                python: PyValue::Opaque {
                    text,
                    repr: format!(
                        "datetime.datetime({}, {}, {}, {}{})",
                        naive.year(),
                        naive.month(),
                        naive.day(),
                        python_clock_repr(naive.time()),
                        python_tzinfo_repr(Some(offset))
                    ),
                },
                exactness: text_exactness,
            }
        }),
        "date" => parse_date_lax(value).map(|(date, exactness)| {
            let text = date.format("%Y-%m-%d").to_string();
            Coerced {
                stored: Value::String(text.clone()),
                python: PyValue::Opaque {
                    text,
                    repr: format!(
                        "datetime.date({}, {}, {})",
                        date.year(),
                        date.month(),
                        date.day()
                    ),
                },
                exactness,
            }
        }),
        "time" => parse_time_lax(value).map(|(time, offset)| {
            let text = python_time_str(time, offset);
            Coerced {
                stored: Value::String(text.clone()),
                python: PyValue::Opaque {
                    text,
                    repr: format!(
                        "datetime.time({}{})",
                        python_clock_repr(time),
                        python_tzinfo_repr(offset)
                    ),
                },
                exactness: text_exactness,
            }
        }),
        "uuid" => value
            .as_str()
            .and_then(|text| Uuid::parse_str(text).ok())
            .map(|uuid| Coerced {
                stored: Value::String(uuid.to_string()),
                python: PyValue::Opaque {
                    text: uuid.to_string(),
                    repr: format!("UUID('{uuid}')"),
                },
                exactness: Exactness::Exact,
            }),
        _ => return None,
    };
    Some(coerced.ok_or_else(|| {
        let kind = match format {
            "date-time" => "a timezone-aware date-time or Unix timestamp",
            "date" => "a date",
            "time" => "a time of day",
            _ => "a UUID",
        };
        format!("is not {kind}")
    }))
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
    text.push_str(&python_offset_str(date.offset().local_minus_utc()));
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

/// 2^63: Pydantic accepts an integral float for an `int` field only when its
/// magnitude is strictly below this.
const PYDANTIC_FLOAT_AS_INT_LIMIT: f64 = 9_223_372_036_854_775_808.0;

/// Reference chains between scalar definitions deeper than this are treated
/// as cycles and rejected.
const MAX_SCALAR_REFERENCE_DEPTH: usize = 32;

/// Python `==` between two JSON values, where `True == 1` and `1 == 1.0`:
/// how Pydantic matches an input against an `Enum` member's value.
fn python_equal(left: &Value, right: &Value) -> bool {
    let number = |value: &Value| match value {
        Value::Number(number) => number.as_f64(),
        Value::Bool(flag) => Some(f64::from(u8::from(*flag))),
        _ => None,
    };
    match (number(left), number(right)) {
        (Some(left), Some(right)) => left == right,
        (Some(_), None) | (None, Some(_)) => false,
        (None, None) => match (left, right) {
            (Value::Array(left), Value::Array(right)) => {
                left.len() == right.len()
                    && left
                        .iter()
                        .zip(right)
                        .all(|(left, right)| python_equal(left, right))
            }
            (Value::Object(left), Value::Object(right)) => {
                left.len() == right.len()
                    && left.iter().all(|(key, value)| {
                        right
                            .get(key)
                            .is_some_and(|other| python_equal(value, other))
                    })
            }
            _ => left == right,
        },
    }
}

/// Whether every `_` of `text` sits between two ASCII letters or digits, as
/// pydantic-core requires before it ignores underscores in a number.
fn underscores_are_separators(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.iter().enumerate().all(|(index, byte)| {
        *byte != b'_'
            || (index > 0
                && bytes[index - 1].is_ascii_alphanumeric()
                && bytes.get(index + 1).is_some_and(u8::is_ascii_alphanumeric))
    })
}

/// A `number` field's value as pydantic-core's lax `float` takes it: a JSON
/// number, a bool (`true` is `1.0`), or numeric text (trimmed, `_` between
/// digits, `inf` and `nan` in any case).
fn coerce_float(value: &Value) -> Result<Value, String> {
    let float = match value {
        Value::Number(number) => number.as_f64(),
        Value::Bool(flag) => Some(f64::from(u8::from(*flag))),
        Value::String(text) => {
            let text = text.trim();
            if !underscores_are_separators(text) {
                return Err("expected number".to_string());
            }
            text.replace('_', "").parse::<f64>().ok()
        }
        _ => None,
    }
    .ok_or_else(|| "expected number".to_string())?;
    // A JSON value cannot hold `inf` or `nan`, which Pydantic accepts.
    serde_json::Number::from_f64(float)
        .map(Value::Number)
        .ok_or_else(|| "is not a finite number".to_string())
}

/// An `integer` field's value as pydantic-core's lax `int` takes it: a JSON
/// integer, an integral float below 2^63 in magnitude, a bool, or text of
/// digits (trimmed, optionally signed, `_` between digits, and an all-zero
/// fraction such as `3.0`).
fn coerce_int(value: &Value) -> Result<Value, String> {
    match value {
        Value::Number(number) if number.is_i64() || number.is_u64() => Ok(value.clone()),
        // serde parses an integer literal beyond u64 as a float, so such a
        // literal is refused here although Python takes it as an `int`.
        Value::Number(number) => number
            .as_f64()
            .filter(|float| float.fract() == 0.0 && float.abs() < PYDANTIC_FLOAT_AS_INT_LIMIT)
            .map(|float| Value::from(float as i64))
            .ok_or_else(|| "expected integer".to_string()),
        Value::Bool(flag) => Ok(Value::from(u8::from(*flag))),
        Value::String(text) => parse_int_text(text),
        _ => Err("expected integer".to_string()),
    }
}

fn parse_int_text(text: &str) -> Result<Value, String> {
    let invalid = || "expected integer".to_string();
    let text = text.trim();
    let (negative, unsigned) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let (whole, fraction) = match unsigned.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (unsigned, None),
    };
    // `3.0` is `3`, but `3.5`, `3.` and `3.0_0` are not integers.
    if fraction.is_some_and(|zeros| zeros.is_empty() || !zeros.bytes().all(|b| b == b'0')) {
        return Err(invalid());
    }
    if !whole.bytes().next().is_some_and(|b| b.is_ascii_digit())
        || !whole.bytes().all(|b| b.is_ascii_digit() || b == b'_')
        || !underscores_are_separators(whole)
    {
        return Err(invalid());
    }
    let range = || "is outside the supported integer range".to_string();
    let magnitude: u128 = whole.replace('_', "").parse().map_err(|_| range())?;
    if negative {
        // `i64::MIN` has no positive counterpart, hence the signed detour.
        let negated = i128::try_from(magnitude).map(|n| -n).map_err(|_| range())?;
        Ok(Value::from(i64::try_from(negated).map_err(|_| range())?))
    } else {
        Ok(Value::from(u64::try_from(magnitude).map_err(|_| range())?))
    }
}

/// A `boolean` field's value as pydantic-core's lax `bool` takes it: a bool,
/// `0`/`1` (or `0.0`/`1.0`), or one of the texts `0 off f false n no` and
/// `1 on t true y yes` in any case.
fn coerce_bool(value: &Value) -> Result<bool, String> {
    let invalid = || "expected boolean".to_string();
    match value {
        Value::Bool(flag) => Ok(*flag),
        Value::Number(number) => match number.as_f64() {
            Some(0.0) => Ok(false),
            Some(1.0) => Ok(true),
            _ => Err(invalid()),
        },
        Value::String(text) => match text.to_lowercase().as_str() {
            "0" | "off" | "f" | "false" | "n" | "no" => Ok(false),
            "1" | "on" | "t" | "true" | "y" | "yes" => Ok(true),
            _ => Err(invalid()),
        },
        _ => Err(invalid()),
    }
}

/// How exactly an input matched a type, as pydantic-core grades a JSON
/// input: a smart-mode union keeps the most exact variant, and the first
/// declared on a tie. Ordered from least to most exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Exactness {
    /// A coercion: numeric text for a number, `1` for a bool, any input for
    /// an `Enum`, a timestamp number for a datetime.
    Lax,
    /// Allowed in strict mode without being the type itself: an int for a
    /// `float`, any JSON string for `str`, ISO or timestamp text for a
    /// datetime, a `RootModel` built from its root.
    Strict,
    /// The type itself: a JSON integer for `int`, a float for `float`, a
    /// bool for `bool`, a string for `UUID`, a list for `list`.
    Exact,
}

/// A scalar answer after validation: the JSON form Python's storage encoder
/// writes, the Python object Pydantic holds (for its `str()` and the node
/// identity), and how exactly the input matched.
#[derive(Debug)]
struct Coerced {
    stored: Value,
    python: PyValue,
    exactness: Exactness,
}

impl Coerced {
    fn untyped(value: &Value, exactness: Exactness) -> Self {
        Self {
            stored: value.clone(),
            python: plain_python_value(value),
            exactness,
        }
    }
}

/// The Python value of a JSON value declared as `kind`: a `number` is a
/// `float` and an `integer` an `int`, anything else as `json.loads` gives it.
fn typed_python_value(kind: Option<&str>, value: &Value) -> PyValue {
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
        _ => plain_python_value(value),
    }
}

/// Match `value` against an enum's members. A string enum takes only its
/// text. An `IntEnum` (`"type": "integer"`) first reads the input as a lax
/// `int`, so `"1"` and `1.0` select `1`. Any other enum matches by Python
/// equality, where `true` is `1` and `1.0` is `1`. Returns the member's
/// position.
fn coerce_enum(value: &Value, schema: &Value, choices: &[Value]) -> Result<usize, String> {
    let probe = if schema.get("type").and_then(Value::as_str) == Some("integer") {
        coerce_int(value)?
    } else {
        value.clone()
    };
    choices
        .iter()
        .position(|choice| python_equal(choice, &probe))
        .ok_or_else(|| "is not one of the declared enum values".to_string())
}

/// Validate a scalar answer against its schema as Pydantic's lax mode does
/// (`model_validate_json` on the generated model) and return what Python
/// holds: the value in the JSON form its storage encoder writes (a `number`
/// is a float, an `integer` an int, a bool a bool, an enum member its value,
/// a `date-time`, `date` or `time` its `isoformat()` text and a `uuid` its
/// hyphenated text) together with the Python object, typed as
/// datamodel-code-generator declares it (an enum a generated `Enum` member,
/// `enum_class` naming an inline enum's class, and a scalar `$defs` entry a
/// `RootModel`). Strings, numbers and bools are coerced the way pydantic-core
/// coerces them; see `docs/guides/custom-graph-schema.md`.
fn coerce_scalar_value(
    value: &Value,
    schema: &Value,
    definitions: &HashMap<String, Value>,
    enum_class: Option<&str>,
    depth: usize,
) -> Result<Coerced, String> {
    // Compilation routes every model reference to a relationship, so a
    // reference reaching scalar validation must name a scalar definition and
    // must not match vacuously.
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let (name, definition) = reference
            .strip_prefix("#/$defs/")
            .and_then(|name| definitions.get(name).map(|definition| (name, definition)))
            .ok_or_else(|| "is a model reference, not a scalar".to_string())?;
        if depth >= MAX_SCALAR_REFERENCE_DEPTH {
            return Err("nests scalar definition references too deeply".to_string());
        }
        let class = python_class_name(name);
        if definition.get("enum").is_some() {
            return coerce_scalar_value(value, definition, definitions, Some(&class), depth + 1);
        }
        let inner = coerce_scalar_value(value, definition, definitions, None, depth + 1)?;
        // Building a model is never an exact match.
        return Ok(Coerced {
            stored: inner.stored,
            python: PyValue::Root {
                class,
                root: Box::new(inner.python),
            },
            exactness: inner.exactness.min(Exactness::Strict),
        });
    }
    if let Some(variants) = schema.get("anyOf").and_then(Value::as_array) {
        // Pydantic's smart-mode union tries every variant and keeps the most
        // exact match, the first declared on a tie: `3` stays an `int` in
        // `float | int` but `"3"` becomes `3.0`, and `1700000000` is a
        // `float` in `datetime | float`.
        let mut best: Option<Coerced> = None;
        for variant in variants {
            let Ok(candidate) = coerce_scalar_value(value, variant, definitions, enum_class, depth)
            else {
                continue;
            };
            if best
                .as_ref()
                .is_none_or(|current| current.exactness < candidate.exactness)
            {
                let exact = candidate.exactness == Exactness::Exact;
                best = Some(candidate);
                if exact {
                    break;
                }
            }
        }
        return best.ok_or_else(|| "does not match any allowed variant".to_string());
    }
    let kind = schema.get("type").and_then(Value::as_str);
    // An untyped value is an `Any`, which pydantic-core grades strict.
    let mut coerced = Coerced::untyped(value, Exactness::Strict);
    if let Some(choices) = schema.get("enum").and_then(Value::as_array) {
        let position = coerce_enum(value, schema, choices)?;
        let choice = &choices[position];
        let member = enum_class.and_then(|class| {
            python_enum_member_names(choices, kind)
                .into_iter()
                .nth(position)
                .flatten()
                .map(|member| (class, member))
        });
        coerced = Coerced {
            stored: choice.clone(),
            python: match member {
                Some((class, member)) => PyValue::Enum {
                    class: class.to_string(),
                    member,
                    value: Box::new(plain_python_value(choice)),
                    int_enum: kind == Some("integer"),
                },
                None => typed_python_value(kind, choice),
            },
            // An `Enum` built from a JSON value is always a lax match.
            exactness: Exactness::Lax,
        };
    } else if let Some(expected) = kind {
        coerced = match expected {
            "null" if value.is_null() => Coerced {
                stored: Value::Null,
                python: PyValue::None,
                exactness: Exactness::Exact,
            },
            "boolean" => {
                let flag = coerce_bool(value)?;
                Coerced {
                    stored: Value::Bool(flag),
                    python: PyValue::Bool(flag),
                    exactness: if value.is_boolean() {
                        Exactness::Exact
                    } else {
                        Exactness::Lax
                    },
                }
            }
            "integer" => {
                let stored = coerce_int(value)?;
                Coerced {
                    python: PyValue::Int(stored.to_string()),
                    stored,
                    exactness: if value.is_i64() || value.is_u64() {
                        Exactness::Exact
                    } else {
                        Exactness::Lax
                    },
                }
            }
            "number" => {
                let stored = coerce_float(value)?;
                Coerced {
                    python: typed_python_value(kind, &stored),
                    stored,
                    exactness: if value.is_f64() {
                        Exactness::Exact
                    } else if value.is_number() {
                        Exactness::Strict
                    } else {
                        Exactness::Lax
                    },
                }
            }
            "string" => {
                let formatted = schema
                    .get("format")
                    .and_then(Value::as_str)
                    .and_then(|format| coerce_formatted(format, value));
                match formatted {
                    Some(result) => result?,
                    // A JSON string for `str` is strict, not exact.
                    None if value.is_string() => Coerced::untyped(value, Exactness::Strict),
                    None => return Err("expected string".to_string()),
                }
            }
            // A `list[Any]` with items is strict; typed items grade below.
            "array" if value.is_array() => Coerced::untyped(
                value,
                if schema.get("items").is_some() || value.as_array().is_some_and(Vec::is_empty) {
                    Exactness::Exact
                } else {
                    Exactness::Strict
                },
            ),
            // A `dict[str, Any]` or a nested model: strict at best.
            "object" if value.is_object() => Coerced::untyped(value, Exactness::Strict),
            "null" | "array" | "object" => return Err(format!("expected {expected}")),
            _ => coerced,
        };
    }
    if let Some(constant) = schema.get("const")
        && !json_equal(value, constant)
    {
        return Err("does not equal the declared constant".to_string());
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
        // A list is as exact as its least exact item.
        let mut stored = Vec::with_capacity(items.len());
        let mut python = Vec::with_capacity(items.len());
        let mut exactness = coerced.exactness;
        for item in items {
            let item = coerce_scalar_value(item, item_schema, definitions, None, depth)?;
            exactness = exactness.min(item.exactness);
            stored.push(item.stored);
            python.push(item.python);
        }
        coerced = Coerced {
            stored: Value::Array(stored),
            python: PyValue::List(python),
            exactness,
        };
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
            let mut stored = Map::new();
            let mut python = Vec::with_capacity(object.len());
            for (field, field_value) in object {
                let entry = match properties.get(field) {
                    Some(field_schema) => {
                        coerce_scalar_value(field_value, field_schema, definitions, None, depth)?
                    }
                    None => Coerced::untyped(field_value, Exactness::Strict),
                };
                stored.insert(field.clone(), entry.stored);
                python.push((field.clone(), entry.python));
            }
            coerced = Coerced {
                stored: Value::Object(stored),
                python: PyValue::Dict(python),
                exactness: coerced.exactness.min(Exactness::Strict),
            };
        }
    }
    Ok(coerced)
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
            (
                "Odd".to_string(),
                json!({"type": "string", "enum": ["_x", " a", "mro", "Ⅷ", "x²"]}),
            ),
            ("Big".to_string(), json!({"type": "number", "enum": [1e20]})),
        ]);
        let coerced = coerce_scalar_value(
            &value,
            &schema,
            &definitions,
            Some(&python_class_name(field)),
            0,
        )
        .unwrap_or_else(|error| panic!("{value} validates: {error}"));
        python_identity_text(&coerced.python)
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
            (
                json!("_x"),
                json!({"$ref": "#/$defs/Odd"}),
                "o",
                "Odd.field_x",
            ),
            (
                json!(" a"),
                json!({"$ref": "#/$defs/Odd"}),
                "o",
                "Odd.field_a",
            ),
            (
                json!("mro"),
                json!({"$ref": "#/$defs/Odd"}),
                "o",
                "Odd.mro_",
            ),
            (
                json!("Ⅷ"),
                json!({"$ref": "#/$defs/Odd"}),
                "o",
                "Odd.field_VIII",
            ),
            (json!("x²"), json!({"$ref": "#/$defs/Odd"}), "o", "Odd.x_"),
            (
                json!(1e20),
                json!({"$ref": "#/$defs/Big"}),
                "b",
                "Big.number_1e_20",
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
    fn enum_member_names_match_datamodel_code_generator() {
        // Member names datamodel-code-generator 0.82 generates for each enum,
        // as Python reports them after parsing the generated module.
        for (kind, choices, expected) in [
            (
                Some("string"),
                json!([
                    "_x",
                    "__x",
                    " a",
                    "_",
                    "mro",
                    "Ⅷ",
                    "x²",
                    "",
                    "1abc",
                    "class",
                    "Foo-Bar",
                    "dark green",
                    "#x",
                    "#",
                    "ＡＢ",
                    "None",
                    "é",
                    "x⁰",
                    "a b",
                    "a_b",
                    "٣x",
                    "mro_",
                    "  ",
                    "a-",
                    "self",
                    "name",
                    "value"
                ]),
                vec![
                    "field_x",
                    "field__x",
                    "field_a",
                    "field_",
                    "mro_",
                    "field_VIII",
                    "x_",
                    "field__1",
                    "field_1abc",
                    "class_",
                    "Foo_Bar",
                    "dark_green",
                    "x",
                    "field__2",
                    "AB",
                    "None_",
                    "é",
                    "x_u2070_",
                    "a_b",
                    "a_b_1",
                    "field_٣x",
                    "mro__1",
                    "field__",
                    "a_",
                    "self",
                    "name",
                    "value",
                ],
            ),
            (
                Some("number"),
                json!([1e20, 2.0, 1.5, -1, 0.0001, 1e-05, 100]),
                vec![
                    "number_1e_20",
                    "number_2_0",
                    "number_1_5",
                    "number__1",
                    "number_0_0001",
                    "number_1e_05",
                    "number_100",
                ],
            ),
            (
                Some("integer"),
                json!([1, -1, 10]),
                vec!["integer_1", "integer__1", "integer_10"],
            ),
            (
                None,
                json!([1, 1.5, "s", null]),
                vec!["int_1", "float_1_5", "s", "NoneType_None"],
            ),
            (
                Some("string"),
                json!(["a b", "a_b", "a-b"]),
                vec!["a_b", "a_b_1", "a_b_2"],
            ),
        ] {
            let choices = choices.as_array().unwrap();
            let names: Vec<String> = python_enum_member_names(choices, kind)
                .into_iter()
                .map(Option::unwrap)
                .collect();
            assert_eq!(names, expected, "{kind:?} {choices:?}");
        }
        // A `null` member of a string enum makes it nullable; it takes no name.
        assert_eq!(
            python_enum_member_names(&[json!("a"), Value::Null], Some("string")),
            [Some("a".to_string()), None]
        );
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
    fn undeclared_base_identity_fields_use_python_instance_defaults() {
        // Expected ids come from the model datamodel-code-generator builds,
        // whose instance holds the DataPoint defaults when the id is derived.
        let tag = |identity: &str, metadata_extra: Value| {
            let mut metadata = json!({"index_fields": ["name"], "identity_fields": [identity]});
            if let (Some(metadata), Some(extra)) =
                (metadata.as_object_mut(), metadata_extra.as_object())
            {
                metadata.extend(extra.clone());
            }
            let schema = json!({
                "title": "Tag", "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "metadata": {"type": "object", "default": metadata}
                },
                "required": ["name"]
            });
            let compiled = DynamicGraphSchema::compile(&schema).unwrap();
            let first = compiled
                .materialize(&json!({"name": "x"}), Uuid::nil())
                .unwrap()
                .root;
            let second = compiled
                .materialize(&json!({"name": "y"}), Uuid::nil())
                .unwrap()
                .root;
            (first, second)
        };
        for (identity, expected) in [
            ("type", "a5e18678-1459-5b9f-985e-0c7b06301289"),
            ("version", "a809bdb3-bfc2-5b81-b2dc-c16e79737e52"),
            ("ontology_valid", "7fc850e7-22f5-5ebf-844a-bc12fe93947a"),
            ("ontology_uri", "5fb02635-8639-5e8e-87d2-b46ec6a9ca34"),
            ("topological_rank", "43f7bb39-9346-5e61-a86b-f156700c5392"),
            ("valid_to", "5fb02635-8639-5e8e-87d2-b46ec6a9ca34"),
            ("belongs_to_set", "5fb02635-8639-5e8e-87d2-b46ec6a9ca34"),
            ("source_pipeline", "5fb02635-8639-5e8e-87d2-b46ec6a9ca34"),
            ("feedback_weight", "91bb3be1-a505-5353-b6a3-42410898bacd"),
            ("importance_weight", "91bb3be1-a505-5353-b6a3-42410898bacd"),
        ] {
            let (first, second) = tag(identity, json!({"transparent": false, "x": 1.0}));
            assert_eq!(first, Uuid::parse_str(expected).unwrap(), "{identity}");
            assert_eq!(first, second, "{identity}");
        }
        // `metadata` is the declared default dict, printed by `repr`.
        let (first, second) = tag("metadata", json!({"transparent": false, "x": 1.0}));
        assert_eq!(
            first,
            Uuid::parse_str("d35de72e-eaca-5e4b-ae3d-4c1d5bbd5711").unwrap()
        );
        assert_eq!(first, second);
        // Per-instance base fields stay random.
        for identity in ["id", "created_at", "updated_at"] {
            let (first, second) = tag(identity, json!({}));
            assert_ne!(first, second, "{identity}");
            assert_eq!(first.get_version_num(), 4, "{identity}");
        }
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
    fn a_scalar_definition_wrapping_models_is_rejected() {
        // Pydantic emits `type People = list[Person]` as a non-object
        // `$defs` entry; it holds models, so it is not a scalar value.
        let schema = json!({
            "$defs": {
                "Person": {"title": "Person", "type": "object",
                    "properties": {"name": {"type": "string"}}},
                "People": {"type": "array", "items": {"$ref": "#/$defs/Person"}},
                "Crowd": {"type": "array", "items": {"$ref": "#/$defs/People"}},
                "Loop": {"type": "array", "items": {"$ref": "#/$defs/Loop"}}
            },
            "title": "Team", "type": "object",
            "properties": {"people": {"$ref": "#/$defs/People"}},
            "required": ["people"]
        });
        assert_eq!(
            DynamicGraphSchema::compile(&schema).expect_err("People wraps a model"),
            DynamicGraphError::UnsupportedReferenceShape {
                model: "Team".to_string(),
                field: "people".to_string(),
            }
        );

        // Through a chain of scalar definitions too.
        let mut chained = schema.clone();
        chained["properties"] = json!({"crowd": {"$ref": "#/$defs/Crowd"}});
        chained["required"] = json!([]);
        assert_eq!(
            DynamicGraphSchema::compile(&chained).expect_err("Crowd wraps People"),
            DynamicGraphError::UnsupportedReferenceShape {
                model: "Team".to_string(),
                field: "crowd".to_string(),
            }
        );

        // A self-referencing scalar definition terminates and stays a value.
        let mut looped = schema;
        looped["properties"] = json!({"tags": {"$ref": "#/$defs/Loop"}});
        looped["required"] = json!([]);
        let compiled = DynamicGraphSchema::compile(&looped).expect("Loop holds no model");
        assert_eq!(
            compiled.root_model().fields[0].kind,
            DynamicFieldKind::Scalar
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
        // Accepted and rejected exactly as by the Python model's
        // `model_validate_json`, with Python's `str()` of the value.
        for (accepted, python_str) in [
            ("2024-03-01T10:00:00Z", "2024-03-01 10:00:00+00:00"),
            ("2024-03-01 10:00+01:00", "2024-03-01 10:00:00+01:00"),
            (
                "2024-03-01T10:00:00.25-05:00",
                "2024-03-01 10:00:00.250000-05:00",
            ),
            ("2024-03-01t10:00:00Z", "2024-03-01 10:00:00+00:00"),
            ("2024-03-01_10:00:00Z", "2024-03-01 10:00:00+00:00"),
            ("2024-03-01T10:00:00,5Z", "2024-03-01 10:00:00.500000+00:00"),
            ("2024-03-01T10:00:00+0100", "2024-03-01 10:00:00+01:00"),
            ("2024-03-01T10:00:00z", "2024-03-01 10:00:00+00:00"),
            (
                "2024-03-01T10:00:00.123456789Z",
                "2024-03-01 10:00:00.123456+00:00",
            ),
            ("2024-03-01T10:00:00.0000001Z", "2024-03-01 10:00:00+00:00"),
            ("2024-03-01T10:00:00+23:59", "2024-03-01 10:00:00+23:59"),
            ("2024-03-01T10:00:00-00:00", "2024-03-01 10:00:00+00:00"),
            (
                "2024-03-01T10:00:00\u{2212}01:00",
                "2024-03-01 10:00:00-01:00",
            ),
            ("2024-02-29T10:00:00Z", "2024-02-29 10:00:00+00:00"),
        ] {
            compiled
                .materialize(&json!({"at": accepted}), Uuid::nil())
                .unwrap_or_else(|error| panic!("{accepted}: {error}"));
            let parsed = parse_aware_datetime(accepted).expect(accepted);
            assert_eq!(python_datetime_str(&parsed), python_str, "{accepted}");
        }
        for rejected in [
            "2024-03-01T10:00:00",
            "yesterday",
            " 2024-03-01T10:00:00Z",
            "2024-03-01T10:00:00Z ",
            "2024-03-01T10:00:00Z\n",
            "2024-03-01T10:00:60Z",
            "2024-03-01T10:00:00+01",
            "2024-03-01T10Z",
            "2024-03-01T10:00:00.Z",
            "2024-03-01T10:00.5Z",
            "2024-03-01T24:00:00Z",
            "2024-02-30T10:00:00Z",
            "2024-03-01T10:00:00+24:00",
            "2024-03-01T10:00:00+01:60",
            "2024-03-01",
            "2024-03-01T1:00:00Z",
            "2024-3-01T10:00:00Z",
            "+2024-03-01T10:00:00Z",
            "2024-03-01T10:00:00+01:00:30",
            "2024-03-01T10:00:00 Z",
            "0000-01-01T00:00:00Z",
            "２024-03-01T10:00:00Z",
        ] {
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
        // Pydantic takes an integral float only below 2^63 in magnitude.
        for (accepted, stored) in [
            (json!(9.2e18), json!(9_200_000_000_000_000_000_i64)),
            (json!(-9.2e18), json!(-9_200_000_000_000_000_000_i64)),
            (json!(-0.0), json!(0)),
        ] {
            let graph = compiled
                .materialize(&json!({"n": accepted}), Uuid::nil())
                .unwrap_or_else(|error| panic!("{accepted}: {error}"));
            assert_eq!(graph.nodes[0].properties["n"], stored);
        }
        for rejected in [
            json!(1e20),
            json!(9.3e18),
            json!(9_223_372_036_854_775_808.0_f64),
            json!(-9_223_372_036_854_775_808.0_f64),
        ] {
            assert!(
                compiled
                    .materialize(&json!({"n": rejected}), Uuid::nil())
                    .is_err(),
                "{rejected}"
            );
        }
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

    /// Python's recursive `_resolve_children`: a transparent container whose
    /// child is itself transparent resolves to the stored grandchildren, and
    /// an inner container reached through two fields of the same object (a
    /// diamond) is expanded once, so each stored id appears once.
    #[test]
    fn nested_transparent_containers_resolve_to_stored_grandchildren() {
        let node = |data_type: &str, name: &str, transparent: bool| {
            let mut base = DataPoint::new(data_type, Some(Uuid::nil()));
            base.id = Uuid::new_v5(
                &Uuid::NAMESPACE_OID,
                format!("{data_type}:{name}").as_bytes(),
            );
            DynamicDataPoint {
                occurrence_id: Uuid::new_v4(),
                base,
                properties: Map::new(),
                property_strs: BTreeMap::new(),
                index_fields: Vec::new(),
                transparent,
            }
        };
        let relationship = |source: &DynamicDataPoint, field: &str, target: &DynamicDataPoint| {
            DynamicRelationship {
                source: source.occurrence_id,
                target: target.occurrence_id,
                field_name: field.to_string(),
            }
        };
        let owner = node("Person", "owner", false);
        let outer = node("Outer", "outer", true);
        let inner = node("Inner", "inner", true);
        let alice = node("Person", "alice", false);
        let bob = node("Person", "bob", false);
        let carol = node("Person", "carol", false);
        let relationships = vec![
            relationship(&owner, "team", &outer),
            // The same inner occurrence through two fields: a diamond.
            relationship(&outer, "left", &inner),
            relationship(&outer, "right", &inner),
            relationship(&inner, "members", &alice),
            relationship(&inner, "members", &bob),
            relationship(&outer, "lead", &carol),
        ];
        let nodes = vec![
            owner.clone(),
            outer.clone(),
            inner.clone(),
            alice.clone(),
            bob.clone(),
            carol.clone(),
        ];
        let grandchildren = vec![alice.base.id, bob.base.id, carol.base.id];

        // Behind a stored owner, the owning field edge is hoisted onto every
        // stored descendant, once each.
        let graph = DynamicGraph {
            root: owner.base.id,
            root_occurrence: owner.occurrence_id,
            nodes: nodes.clone(),
            relationships: relationships.clone(),
        };
        let stored = graph.stored_graph();
        assert_eq!(
            stored_ids(&stored),
            [owner.base.id, alice.base.id, bob.base.id, carol.base.id]
        );
        assert_eq!(
            stored.relationships,
            grandchildren
                .iter()
                .map(|target| edge(owner.base.id, "team", *target))
                .collect::<Vec<_>>()
        );
        assert_eq!(graph.direct_attachment_targets(), [owner.base.id]);
        assert_eq!(
            graph.all_attachment_targets(),
            [owner.base.id, alice.base.id, bob.base.id, carol.base.id]
        );

        // As the root, the nested containers vanish entirely.
        let graph = DynamicGraph {
            root: outer.base.id,
            root_occurrence: outer.occurrence_id,
            nodes,
            relationships,
        };
        let stored = graph.stored_graph();
        assert_eq!(stored_ids(&stored), grandchildren);
        assert!(stored.relationships.is_empty());
        assert_eq!(graph.direct_attachment_targets(), grandchildren);
        assert_eq!(graph.all_attachment_targets(), grandchildren);
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
                property_strs: BTreeMap::from([("name".to_string(), name.to_string())]),
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
            property_strs: BTreeMap::new(),
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
            "title": "Person",
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
            property_strs: BTreeMap::new(),
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
        // The first declared variant that validates wins, as in Python: an
        // object with an Org-only `domain` key still validates as a Person.
        assert_eq!(type_of("Acme").as_deref(), Some("Person"));
        assert_eq!(type_of("Alice").as_deref(), Some("Person"));
        assert_eq!(type_of("Bob").as_deref(), Some("Person"));
        assert_eq!(type_of("Initech").as_deref(), Some("Person"));
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

    /// A one-node schema whose `v` field is the label source.
    fn label_schema(v: Value, defs: Value) -> Value {
        json!({
            "title": "Thing", "type": "object", "$defs": defs,
            "properties": {
                "name": {"type": "string"},
                "v": v,
                "metadata": {"type": "object", "default": {
                    "index_fields": ["v"], "identity_fields": ["name"]
                }}
            },
            "required": ["name", "v"]
        })
    }

    /// Python `_get_node_label` and the triplet text `str()` the Pydantic
    /// value; expected texts from cognee @ f154b32's `ensure_default_edge_properties`.
    #[test]
    fn labels_print_the_python_value_not_the_json_text() {
        let role = json!({"Role": {"title": "Role", "type": "string", "enum": ["lead dev", "qa"]}});
        let code = json!({"Code": {"title": "Code", "type": "string", "pattern": "^[A-Z]+$"}});
        for (field, defs, value, expected) in [
            (
                json!({"$ref": "#/$defs/Role"}),
                role,
                json!("lead dev"),
                "Role.lead_dev",
            ),
            (json!({"type": "boolean"}), json!({}), json!(true), "True"),
            (
                json!({"type": "string", "format": "date-time"}),
                json!({}),
                json!("2024-03-01T10:00:00Z"),
                "2024-03-01 10:00:00+00:00",
            ),
            (
                json!({"type": "array", "items": {"type": "string"}}),
                json!({}),
                json!(["x", "y"]),
                "['x', 'y']",
            ),
            (
                json!({"$ref": "#/$defs/Code"}),
                code,
                json!("ABC"),
                "root='ABC'",
            ),
            (json!({"type": "number"}), json!({}), json!(3), "3.0"),
        ] {
            let graph = DynamicGraphSchema::compile(&label_schema(field, defs))
                .expect("schema should compile")
                .materialize(&json!({"name": "A", "v": value}), Uuid::nil())
                .expect("answer should materialize");
            assert_eq!(graph.nodes[0].label(), expected, "{value}");
            assert_eq!(graph.nodes[0].embeddable_text(), expected, "{value}");
        }
    }

    /// `_trim_preview` applies to a label taken from an index field only; the
    /// `name` fallback is just stripped.
    #[test]
    fn only_an_index_field_label_is_trimmed() {
        let long_name = format!("Alice   Smith {}", "x".repeat(90));
        let schema = |index_fields: Value| {
            json!({
                "title": "Person", "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "metadata": {"type": "object", "default": {
                        "index_fields": index_fields, "identity_fields": ["name"]
                    }}
                },
                "required": ["name"]
            })
        };
        let label = |index_fields: Value| {
            DynamicGraphSchema::compile(&schema(index_fields))
                .expect("schema should compile")
                .materialize(&json!({"name": format!(" {long_name} ")}), Uuid::nil())
                .expect("answer should materialize")
                .nodes[0]
                .label()
        };
        assert_eq!(label(json!([])), long_name);
        assert_eq!(
            label(json!(["name"])),
            format!("Alice Smith {}", "x".repeat(68))
        );
    }

    /// Pydantic leaves a field default unvalidated, so the identity reads the
    /// raw JSON value: ids from cognee @ f154b32.
    #[test]
    fn defaults_feed_the_identity_unvalidated_like_pydantic() {
        let item = DynamicGraphSchema::compile(&json!({
            "title": "Item", "type": "object",
            "properties": {
                "name": {"type": "string"},
                "kind": {"type": "string", "default": "Thing One"},
                "n": {"type": "number", "default": 2},
                "metadata": {"type": "object", "default": {
                    "index_fields": ["name"], "identity_fields": ["kind", "n", "name"]
                }}
            },
            "required": ["name"]
        }))
        .expect("schema should compile");
        let defaulted = item
            .materialize(&json!({"name": "x"}), Uuid::nil())
            .expect("answer should materialize");
        assert_eq!(
            defaulted.root,
            Uuid::parse_str("3a5567aa-2559-56ef-bf6b-8b97c6ec77ec").expect("uuid")
        );
        assert_eq!(defaulted.nodes[0].properties["n"], json!(2));
        // The same value given explicitly is validated into a float.
        let explicit = item
            .materialize(&json!({"name": "x", "n": 2}), Uuid::nil())
            .expect("answer should materialize");
        assert_eq!(
            explicit.root,
            Uuid::new_v5(&Uuid::NAMESPACE_OID, b"Item:thing_one|2.0|x")
        );

        let graph = DynamicGraphSchema::compile(&json!({
            "title": "D", "type": "object",
            "$defs": {"Role": {"title": "Role", "type": "string", "enum": ["lead dev", "qa"]}},
            "properties": {
                "name": {"type": "string"},
                "role": {"$ref": "#/$defs/Role", "default": "lead dev"},
                "when": {"type": "string", "format": "date-time", "default": "2024-03-01T00:00:00Z"},
                "cnt": {"type": "integer", "default": 3},
                "metadata": {"type": "object", "default": {
                    "index_fields": ["name"], "identity_fields": ["name", "role", "when", "cnt"]
                }}
            },
            "required": ["name"]
        }))
        .expect("schema should compile")
        .materialize(&json!({"name": "x"}), Uuid::nil())
        .expect("answer should materialize");
        assert_eq!(
            graph.root,
            Uuid::parse_str("cfcbc722-d497-5560-814e-5a81a2cd390c").expect("uuid")
        );
    }

    /// The first declared union variant that validates wins, even when a later
    /// one declares more of the object's keys: Cat/Dog ids from cognee @ f154b32.
    #[test]
    fn a_union_takes_the_first_declared_model_that_validates() {
        let animal = |required: &[&str], extra: (&str, Value)| {
            let mut properties = json!({
                "name": {"type": "string"},
                "metadata": {"type": "object", "default": {
                    "index_fields": ["name"], "identity_fields": ["name"]
                }}
            });
            properties[extra.0] = extra.1;
            json!({"type": "object", "properties": properties, "required": required})
        };
        let graph = DynamicGraphSchema::compile(&json!({
            "title": "Box", "type": "object",
            "$defs": {
                "Cat": animal(&["name"], ("meows", json!({"type": "boolean", "default": true}))),
                "Dog": animal(&["name", "barks"], ("barks", json!({"type": "boolean"})))
            },
            "properties": {
                "name": {"type": "string"},
                "items": {"type": "array", "items": {
                    "anyOf": [{"$ref": "#/$defs/Cat"}, {"$ref": "#/$defs/Dog"}]
                }},
                "metadata": {"type": "object", "default": {
                    "index_fields": ["name"], "identity_fields": ["name"]
                }}
            },
            "required": ["name", "items"]
        }))
        .expect("schema should compile")
        .materialize(
            &json!({"name": "B", "items": [{"name": "Rex", "barks": true}, {"name": "Tom"}]}),
            Uuid::nil(),
        )
        .expect("answer should materialize");
        let rex = graph
            .nodes
            .iter()
            .find(|node| node.properties.get("name") == Some(&json!("Rex")))
            .expect("Rex is materialized");
        assert_eq!(rex.base.data_type, "Cat");
        assert_eq!(
            rex.base.id,
            Uuid::parse_str("be1d4b52-2e2d-5bb4-b4cb-552e7eb4681c").expect("uuid")
        );
        assert_eq!(rex.properties.get("meows"), Some(&json!(true)));
        assert!(!rex.properties.contains_key("barks"));
    }

    /// One field's schema as a one-field custom model whose only identity
    /// field is that field, so the node id depends on the validated value.
    fn lax_model(table: &Value, field: &str) -> DynamicGraphSchema {
        let schema = json!({
            "title": "Thing",
            "type": "object",
            "properties": {
                field: table["fields"][field],
                "metadata": {"type": "object", "default": {"identity_fields": [field]}},
            },
            "$defs": table["defs"],
        });
        DynamicGraphSchema::compile(&schema).expect("table schema compiles")
    }

    /// Every row of the ground-truth table recorded from real Pydantic
    /// (`tests/fixtures/pydantic_lax/generate.py`, `model_validate_json` on
    /// the model datamodel-code-generator builds): accepted and rejected
    /// inputs, the value held in storage form, its `str()`, and the node id
    /// cognee derives from it, computed by Python's `uuid5`.
    #[test]
    fn pydantic_lax_table_is_reproduced() {
        let table: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/pydantic_lax/pydantic_lax_table.json"
        ))
        .expect("the fixture is valid JSON");
        let rows = table["rows"].as_array().expect("rows is an array");
        let mut accepted_rows = 0;
        let mut rejected_rows = 0;
        let mut gap_rows = 0;
        for row in rows {
            let field = row["field"].as_str().expect("field is text");
            let input = row["input"].as_str().expect("input is text");
            let label = format!("{field} <- {input}");
            let answer: Value = serde_json::from_str(input).expect("input is JSON");
            let compiled = lax_model(&table, field);
            let result = compiled.materialize(&json!({ field: answer }), Uuid::nil());
            if row["accepted"] != json!(true) {
                rejected_rows += 1;
                assert!(result.is_err(), "{label}: Pydantic rejects it");
                continue;
            }
            if let Some(gap) = row.get("gap") {
                // Pydantic holds a value a JSON `Value` cannot (`inf`, `nan`,
                // an integer beyond 64 bits); the answer is refused instead.
                gap_rows += 1;
                assert!(result.is_err(), "{label}: known gap ({gap})");
                continue;
            }
            accepted_rows += 1;
            let graph = result.unwrap_or_else(|error| panic!("{label}: {error}"));
            let node = &graph.nodes[0];
            assert_eq!(
                node.base.id.to_string(),
                row["id"].as_str().expect("id is text"),
                "{label}: node id"
            );
            assert_eq!(node.properties[field], row["stored"], "{label}: stored");
            if !row["stored"].is_null() {
                assert_eq!(
                    node.property_strs[field],
                    row["str"].as_str().expect("str is text"),
                    "{label}: str()"
                );
            }
        }
        // The table must not silently shrink.
        assert!(accepted_rows >= 356, "{accepted_rows} accepted rows");
        assert!(rejected_rows >= 178, "{rejected_rows} rejected rows");
        assert_eq!(gap_rows, 8, "{gap_rows} known-gap rows");
    }

    /// One identity UUID per coercion family, computed with Python's
    /// `uuid5(NAMESPACE_OID, "Thing:<str(value)>")` (cognee's identity
    /// algorithm), never from this code.
    #[test]
    fn coerced_values_hash_to_the_ids_python_computes() {
        let defs = json!({
            "Flag": {"enum": [true, false]},
            "Level": {"enum": [1, 2], "type": "integer"},
        });
        let date_time = json!({"type": "string", "format": "date-time"});
        for (schema, answer, expected) in [
            // numeric string for number: `"3"` is `3.0`.
            (
                json!({"type": "number"}),
                json!("3"),
                "bbeb856f-23d1-52a8-9856-cc9367ad9697",
            ),
            // numeric string for integer: `"3"` is `3`.
            (
                json!({"type": "integer"}),
                json!("3"),
                "8e8006e1-4b68-57d1-ae5c-97ad0354780e",
            ),
            // text for boolean: `"yes"` is `True`.
            (
                json!({"type": "boolean"}),
                json!("yes"),
                "57542da9-7a39-5ead-b901-778ef0ea0a79",
            ),
            // bool matches an int enum member: `true` is `Level.integer_1`,
            // which prints `1`.
            (
                json!({"$ref": "#/$defs/Level"}),
                json!(true),
                "b61468a2-d621-5c40-b148-12963ff9b2de",
            ),
            // int matches a bool enum member: `1` is `Flag.bool_True`.
            (
                json!({"$ref": "#/$defs/Flag"}),
                json!(1),
                "705a39b0-871a-521a-8cd6-db99aec6fd55",
            ),
            // Unix timestamp for date-time, as a number and as text.
            (
                date_time.clone(),
                json!(1_700_000_000),
                "37944ed6-b3f5-5590-893b-cf823edb7c86",
            ),
            (
                date_time,
                json!("1700000000"),
                "37944ed6-b3f5-5590-893b-cf823edb7c86",
            ),
            // a timestamp on midnight for date.
            (
                json!({"type": "string", "format": "date"}),
                json!(86_400),
                "33af0cff-08f2-553c-a421-7d86a6ad5974",
            ),
            // seconds since midnight for time, read as UTC.
            (
                json!({"type": "string", "format": "time"}),
                json!(3661),
                "56a9c8b0-0a73-5e38-b8a0-9e467805dfc1",
            ),
            // a braced UUID prints hyphenated.
            (
                json!({"type": "string", "format": "uuid"}),
                json!("{12345678-1234-5678-1234-567812345678}"),
                "6d5af9c5-f5e8-56ac-a65f-35a74be1a911",
            ),
            // coercion applies inside a list: `[1, "2"]` prints `[1.0, 2.0]`.
            (
                json!({"type": "array", "items": {"type": "number"}}),
                json!([1, "2"]),
                "d3124fad-7cbe-5abf-997c-b2f8c3cba75c",
            ),
        ] {
            let schema_json = json!({
                "title": "Thing", "type": "object",
                "properties": {
                    "v": schema,
                    "metadata": {"type": "object", "default": {"identity_fields": ["v"]}},
                },
                "$defs": defs,
            });
            let compiled = DynamicGraphSchema::compile(&schema_json).expect("schema compiles");
            let graph = compiled
                .materialize(&json!({ "v": answer }), Uuid::nil())
                .unwrap_or_else(|error| panic!("{answer}: {error}"));
            assert_eq!(graph.nodes[0].base.id.to_string(), expected, "{answer}");
        }
    }

    /// Stored properties are the values Python holds, in the form its storage
    /// encoder writes.
    #[test]
    fn coerced_properties_are_stored_as_python_holds_them() {
        let schema = json!({
            "title": "Thing", "type": "object",
            "properties": {
                "n": {"type": "number"}, "i": {"type": "integer"}, "b": {"type": "boolean"},
                "when": {"type": "string", "format": "date-time"},
                "day": {"type": "string", "format": "date"},
                "at": {"type": "string", "format": "time"},
                "ref": {"type": "string", "format": "uuid"},
            },
        });
        let compiled = DynamicGraphSchema::compile(&schema).expect("schema compiles");
        let answer = |id: &str| {
            json!({
                "n": "3", "i": " 7 ", "b": "off", "when": 1_700_000_000_i64,
                "day": "2024-01-02T00:00:00Z", "at": "03:04", "ref": id,
            })
        };
        let error = compiled
            .materialize(&answer("not-a-uuid"), Uuid::nil())
            .expect_err("`not-a-uuid` is not a UUID");
        assert!(error.to_string().contains("`Thing.ref`"), "{error}");
        let graph = compiled
            .materialize(&answer("ABCDEF78-1234-5678-1234-567812345678"), Uuid::nil())
            .expect("every value coerces");
        let properties = &graph.nodes[0].properties;
        assert_eq!(properties["n"], json!(3.0));
        assert!(properties["n"].is_f64());
        assert_eq!(properties["i"], json!(7));
        assert_eq!(properties["b"], json!(false));
        assert_eq!(properties["when"], json!("2023-11-14T22:13:20+00:00"));
        assert_eq!(properties["day"], json!("2024-01-02"));
        assert_eq!(properties["at"], json!("03:04:00"));
        assert_eq!(
            properties["ref"],
            json!("abcdef78-1234-5678-1234-567812345678")
        );
        let strs = &graph.nodes[0].property_strs;
        assert_eq!(strs["when"], "2023-11-14 22:13:20+00:00");
        assert_eq!(strs["day"], "2024-01-02");
    }

    /// A smart-mode union keeps the most exact variant (exact, then strict,
    /// then lax) and the first declared on a tie. Expectations recorded from
    /// Pydantic 2.13.5 (`un_*` rows of the ground-truth table).
    #[test]
    fn a_union_keeps_the_most_exact_variant_like_pydantic() {
        let defs = json!({
            "Digits": {"enum": ["1", "2"]},
            "Txt": {"type": "string"},
        });
        let number = json!({"type": "number"});
        let date_time = json!({"type": "string", "format": "date-time"});
        let uuid = json!({"type": "string", "format": "uuid"});
        for (variants, answer, stored, text) in [
            // An int is strict for `float` but lax for `datetime`.
            (
                json!([date_time, number]),
                json!(1_700_000_000),
                json!(1_700_000_000.0),
                "1700000000.0",
            ),
            // Timestamp text is strict for `datetime`, lax for `float`.
            (
                json!([number, date_time]),
                json!("1700000000"),
                json!("2023-11-14T22:13:20+00:00"),
                "2023-11-14 22:13:20+00:00",
            ),
            // An `Enum` is always lax, so the earlier lax `float` wins.
            (
                json!([number, {"$ref": "#/$defs/Digits"}]),
                json!("1"),
                json!(1.0),
                "1.0",
            ),
            // A `RootModel[str]` is strict, which beats a lax `float`.
            (
                json!([number, {"$ref": "#/$defs/Txt"}]),
                json!("1"),
                json!("1"),
                "root='1'",
            ),
            // A UUID string is exact, which beats a strict `str`.
            (
                json!([{"type": "string"}, uuid]),
                json!("ABCDEF78-1234-5678-1234-567812345678"),
                json!("abcdef78-1234-5678-1234-567812345678"),
                "abcdef78-1234-5678-1234-567812345678",
            ),
        ] {
            let schema = json!({
                "title": "Thing", "type": "object",
                "properties": {"v": {"anyOf": variants}},
                "$defs": defs,
            });
            let compiled = DynamicGraphSchema::compile(&schema).expect("schema compiles");
            let graph = compiled
                .materialize(&json!({ "v": answer }), Uuid::nil())
                .unwrap_or_else(|error| panic!("{variants} <- {answer}: {error}"));
            let node = &graph.nodes[0];
            assert_eq!(node.properties["v"], stored, "{variants} <- {answer}");
            assert_eq!(node.property_strs["v"], text, "{variants} <- {answer}");
        }
    }

    /// `i64::MIN` as a timestamp is refused, as Pydantic refuses it, instead
    /// of overflowing while its magnitude is taken.
    #[test]
    fn an_i64_min_timestamp_is_refused_without_overflow() {
        for format in ["date-time", "date", "time"] {
            let schema = json!({
                "title": "Thing", "type": "object",
                "properties": {"v": {"type": "string", "format": format}},
            });
            let compiled = DynamicGraphSchema::compile(&schema).expect("schema compiles");
            for answer in [json!(i64::MIN), json!(i64::MIN.to_string())] {
                assert!(
                    compiled
                        .materialize(&json!({ "v": answer }), Uuid::nil())
                        .is_err(),
                    "{format} <- {answer}"
                );
            }
        }
    }
}
