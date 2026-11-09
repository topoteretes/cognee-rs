//! Visualization preprocessor — the single place where every renderer-facing
//! field is derived from raw graph-adapter output.
//!
//! Port of `cognee/modules/visualization/preprocessor.py` (the pure core:
//! constants, node/link enrichment, color maps, memory map). The renderer
//! should *only* read fields produced here — synthesising stage or bundling
//! information in JavaScript is what made the original visualization a mess.
//!
//! Layout of the port:
//!   * this module — constants, `preprocess()` and the small `_stage_for_node`
//!     / `_visual_rank` / `_edge_class` / `_bundle_key` / `_compact_provenance`
//!     / `_link_relation` helpers (`preprocessor.py:848-892`, `1219-1399`);
//!   * [`naming`] — `derive_node_name` and friends;
//!   * [`compact`] — the streamed read's compact chunk payload and its
//!     summary accumulator (SDK-787);
//!   * [`memory_map`] — the Memory-tab payload (`preprocessor.py:913-1175`);
//!   * [`schema_graph`] / [`operations_catalog`] — the Schema view layers.

pub mod compact;
pub mod memory_map;
pub mod naming;
pub mod operations_catalog;
pub mod schema_graph;

use std::collections::{BTreeMap, HashMap};

use cognee_graph::{EdgeData, GraphNode};
use serde_json::{Map, Value};

pub use crate::colors::build_node_set_colors;
use crate::colors::{provenance_colors, type_color};
pub use compact::{
    COMPACT_PROPERTY_KEYS, CompactGraphAccumulator, compact_chunk, compact_link, compact_node,
    compact_property_keys,
};

// ── Constants (verbatim from preprocessor.py) ────────────────────────────────

// `SCHEMA_TABLE_NODE_TYPES` / `SCHEMA_TABLE_RANK` live in [`schema_graph`],
// their only consumer, together with the other Schema-view caps.

/// Stage assignment by node type — drives the left-to-right Story layout.
/// Unknown types fall through to `"other"`, unless they declare identity
/// fields (see [`stage_for_node`]) (`preprocessor.py` `_STAGE_BY_TYPE`).
pub(crate) const STAGE_BY_TYPE: [(&str, &str); 15] = [
    ("TextDocument", "document"),
    ("DocumentChunk", "chunk"),
    ("TextSummary", "summary"),
    ("GlobalContextSummary", "context"),
    ("Entity", "entity"),
    ("EntityType", "type"),
    ("DatabaseSchema", "schema"),
    ("SchemaTable", "schema"),
    ("SchemaColumn", "schema"),
    ("SchemaRelationship", "schema"),
    ("TableType", "schema"),
    ("TableRow", "schema"),
    ("ColumnValue", "schema"),
    ("DltColumn", "schema"),
    ("DltRow", "chunk"),
];

/// Built-in node types outside [`STAGE_BY_TYPE`] that also declare
/// `identity_fields` but are not entities (`preprocessor.py`
/// `_BUILTIN_IDENTITY_TYPES`, SDK-863).
pub(crate) const BUILTIN_IDENTITY_TYPES: [&str; 5] = [
    "EdgeType",
    "NodeSet",
    "Skill",
    "SkillRun",
    "SkillImprovementProposal",
];

/// Visual ordering of stages along the Story view's left-to-right spine
/// (`preprocessor.py:72-81`).
pub const STAGE_ORDER: [&str; 8] = [
    "document", "chunk", "entity", "type", "summary", "context", "schema", "other",
];

/// Relationship names that connect a structural parent to its children; edges
/// of these types are bundled in the Story view to cut visual noise on dense
/// graphs (`preprocessor.py` `_STRUCTURAL_RELATIONS`; `child_of` is
/// `StructureContainer.CHILD_OF`).
pub(crate) const STRUCTURAL_RELATIONS: [&str; 9] = [
    "contains",
    "is_a",
    "part_of",
    "is_part_of",
    "is_table_of",
    "has_relationship",
    "made_from",
    "summarized_in",
    "child_of",
];

/// Relationship name of the `Entity -> EntityType` edge used to resolve the
/// semantic type of extracted entities (`preprocessor.py:471`).
pub(crate) const ENTITY_TYPE_RELATION: &str = "is_a";

/// Node property a bounded read sets on each `Entity` whose semantic type the
/// store resolved (Python `SEMANTIC_TYPE_KEY`, set by
/// `subgraph_data.resolve_entity_types`). Underscored so it never collides
/// with a custom graph-model field; [`preprocess`] turns it into the public
/// `entity_type` field and strips it from the emitted nodes.
pub const SEMANTIC_TYPE_KEY: &str = "_semantic_type";

/// Node stages whose labels are always shown in Key mode — documents and
/// entity types are the natural landmarks of the Story view
/// (`preprocessor.py:1216`).
const ALWAYS_LABEL_STAGES: [&str; 2] = ["document", "type"];

/// Percentile used for the Key-mode label budget (`preprocessor.py:1203`).
const LABEL_PRIORITY_PERCENTILE: f64 = 0.75;

// ── Small value helpers ──────────────────────────────────────────────────────

/// Python truthiness for a JSON value: `None`/`false`/`0`/`""`/`[]`/`{}` are
/// falsy, everything else is truthy. Used wherever the Python source writes
/// `if node_info.get(key):`.
pub(crate) fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().unwrap_or(1.0) != 0.0,
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// `str(value)` for the JSON values the preprocessor stringifies (ids,
/// `document_id`, `global_context_bucket_id`, placeholder node types).
pub(crate) fn py_str(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Null => "None".to_string(),
        Value::Number(number) => number.to_string(),
        other => other.to_string(),
    }
}

/// Turn a graph-adapter property map into a JSON object.
///
/// Keys are inserted in sorted order because `cognee_graph::NodeData` is a
/// `HashMap` with a randomly seeded hasher: iterating it directly would make the
/// emitted JSON — and, through `Counter`-style tie-breaks, the Schema tab's field
/// selection — differ between two runs over the same graph. Python iterates a
/// `dict` and therefore sees the adapter's own (model-field) order; that residual
/// divergence is documented on
/// `schema_graph::extract_type_schema_fields`.
///
/// What *is* load-bearing and preserved: these keys land in the object **before**
/// [`preprocess`] stamps its derived ones (`stage`, `degree`, `importance`, …), so
/// with `serde_json/preserve_order` enabled a database property always outranks a
/// renderer internal on a type card, exactly as in Python.
fn props_to_object(props: HashMap<std::borrow::Cow<'static, str>, Value>) -> Map<String, Value> {
    let mut entries: Vec<(String, Value)> = props
        .into_iter()
        .map(|(key, value)| (key.into_owned(), value))
        .collect();
    entries.sort_by(|left, right| left.0.cmp(&right.0));

    let mut object = Map::new();
    for (key, value) in entries {
        object.insert(key, value);
    }
    object
}

/// Read a string-valued property, ignoring non-string and missing values.
fn str_prop<'a>(node: &'a Value, key: &str) -> Option<&'a str> {
    node.get(key).and_then(Value::as_str)
}

// ── Story-view enrichment helpers (preprocessor.py:848-892) ──────────────────

/// `_stage_for_node()` — the [`STAGE_BY_TYPE`] entry for the node's type;
/// otherwise `"entity"` for a node that declares identity fields (a custom
/// graph-model node such as `Person`, SDK-863) unless its type is one of
/// [`BUILTIN_IDENTITY_TYPES`]; otherwise `"other"`.
///
/// Note that unlike the color lookup, an absent or `null` `type` never yields
/// a `"default"` entry.
pub(crate) fn stage_for_node(node_info: &Map<String, Value>) -> &'static str {
    let node_type = node_info.get("type").and_then(Value::as_str);
    if let Some(stage) = node_type.and_then(|name| {
        STAGE_BY_TYPE
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, stage)| *stage)
    }) {
        return stage;
    }
    let is_builtin = node_type.is_some_and(|name| BUILTIN_IDENTITY_TYPES.contains(&name));
    if !is_builtin && declares_identity(node_info) {
        return "entity";
    }
    "other"
}

/// `_declares_identity()` — whether `metadata.identity_fields` is truthy.
///
/// `metadata` arrives as an object from most adapters and as a JSON string from
/// others (Python names Neo4j); a string that does not parse, or parses to
/// something other than an object, declares nothing.
fn declares_identity(node_info: &Map<String, Value>) -> bool {
    let parsed;
    let metadata = match node_info.get("metadata") {
        Some(Value::String(raw)) => match serde_json::from_str::<Value>(raw) {
            Ok(value) => {
                parsed = value;
                &parsed
            }
            Err(_) => return false,
        },
        Some(other) => other,
        None => return false,
    };
    metadata
        .as_object()
        .and_then(|map| map.get("identity_fields"))
        .is_some_and(is_truthy)
}

/// `_visual_rank()` — prefer the runtime-stamped `topological_rank`, fall back
/// to the 1-based [`STAGE_ORDER`] position (`preprocessor.py:855-867`).
///
/// Floats are truncated, exactly like Python's `int(rank)`.
fn visual_rank(node_info: &Map<String, Value>, stage: &str) -> i64 {
    if let Some(Value::Number(number)) = node_info.get("topological_rank") {
        match number.as_i64() {
            // `isinstance(rank, int) and rank > 0`
            Some(integer) if integer > 0 => return integer,
            Some(_) => {}
            // `isinstance(rank, float) and rank > 0` → int(rank) truncates.
            None => {
                if let Some(float) = number.as_f64()
                    && float > 0.0
                {
                    return float as i64;
                }
            }
        }
    }
    STAGE_ORDER
        .iter()
        .position(|candidate| *candidate == stage)
        .map_or(STAGE_ORDER.len() as i64, |index| index as i64 + 1)
}

/// `_edge_class()` — classify an edge so the renderer can bundle structural
/// noise and keep semantic relations visible (`preprocessor.py:870-879`).
fn edge_class(relation: &str, edge_info: &Map<String, Value>) -> &'static str {
    let relation = relation.to_lowercase();
    let edge_relation = edge_info
        .get("relationship_name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();

    if STRUCTURAL_RELATIONS.contains(&relation.as_str())
        || STRUCTURAL_RELATIONS.contains(&edge_relation.as_str())
    {
        "structural"
    } else {
        "semantic"
    }
}

/// `_bundle_key()` (`preprocessor.py:882-883`).
fn bundle_key(source_stage: &str, target_stage: &str, edge_class: &str, relation: &str) -> String {
    format!("{source_stage}|{target_stage}|{edge_class}|{relation}")
}

/// `_compact_provenance()` — only emit a provenance object when at least one
/// field is truthy, so the inspector can hide the section cleanly on legacy
/// graphs (`preprocessor.py:886-891`).
fn compact_provenance(node_info: &Map<String, Value>) -> Option<Value> {
    const KEYS: [&str; 4] = [
        "source_task",
        "source_pipeline",
        "source_node_set",
        "source_user",
    ];

    let mut payload = Map::new();
    for key in KEYS {
        if let Some(value) = node_info.get(key)
            && is_truthy(value)
        {
            payload.insert(key.to_string(), value.clone());
        }
    }
    (!payload.is_empty()).then_some(Value::Object(payload))
}

/// `_link_relation()` — read a link's relation name across the shapes the
/// preprocessor emits (`preprocessor.py:474-483`).
///
/// The precedence is load-bearing: `relationship_type` on the link wins over
/// `edge_info["relationship_name"]`, then `edge_info["relationship_type"]`,
/// then the raw `relation`, and finally the literal `"related"`.
pub(crate) fn link_relation(link: &Value) -> String {
    let edge_info = link.get("edge_info");
    let candidates = [
        link.get("relationship_type"),
        edge_info.and_then(|info| info.get("relationship_name")),
        edge_info.and_then(|info| info.get("relationship_type")),
        link.get("relation"),
    ];
    for candidate in candidates {
        if let Some(value) = candidate
            && is_truthy(value)
        {
            return py_str(value);
        }
    }
    "related".to_string()
}

/// `_label_priority_threshold()` — the importance above which a node earns a
/// Key-mode label (`preprocessor.py:1203-1211`).
fn label_priority_threshold(importances: &[f64], percentile: f64) -> f64 {
    let mut finite: Vec<f64> = importances
        .iter()
        .copied()
        .filter(|value| value.is_finite())
        .collect();
    if finite.is_empty() {
        return 0.0;
    }
    finite.sort_by(f64::total_cmp);
    let last = finite.len() - 1;
    // Python: max(0, min(len-1, int(percentile * (len - 1))))
    let rank = ((percentile * last as f64) as usize).min(last);
    finite[rank]
}

// ── Public API ───────────────────────────────────────────────────────────────

/// The four provenance color maps the renderer overlays read.
///
/// Mirrors Python's `color_maps` dict keys (`preprocessor.py:1367-1372`);
/// `BTreeMap` keeps serialization deterministic.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ColorMaps {
    /// `source_task` → hex color.
    pub task: BTreeMap<String, String>,
    /// `source_pipeline` → hex color.
    pub pipeline: BTreeMap<String, String>,
    /// `source_node_set` → hex color, with the self-improvement node sets
    /// pinned to their stable colors.
    pub node_set: BTreeMap<String, String>,
    /// `source_user` → hex color.
    pub user: BTreeMap<String, String>,
}

/// Renderer-facing snapshot of a cognee graph.
///
/// Port of Python's `PreprocessedGraph` dataclass (`preprocessor.py:1181-1200`).
/// Every derivation the JavaScript renderer needs is computed once here; the
/// renderer must not synthesize stage/rank/edge_class on its own.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PreprocessedGraph {
    /// Enriched nodes; each entry is a JSON object.
    pub nodes: Vec<Value>,
    /// Enriched links; each entry is a JSON object with the 11 keys Python emits.
    pub links: Vec<Value>,
    /// The four provenance color maps.
    pub color_maps: ColorMaps,
    /// Schema-view payload; `{"nodes": [], "links": []}` when the graph has none.
    pub schema_graph: Value,
    /// Caller-supplied database-schema payload, passed through untouched.
    pub schema_data: Option<Value>,
    /// Stages present in the graph, in canonical left-to-right order.
    pub pipeline_stages: Vec<String>,
    /// `edge_class` → count.
    pub edge_classes: BTreeMap<String, usize>,
    /// `bundle_key` → count.
    pub bundles: BTreeMap<String, usize>,
    /// Node id → its `provenance` object, for nodes that have one.
    pub provenance_index: BTreeMap<String, Value>,
    /// True when at least one node carries a non-zero numeric
    /// `topological_rank`; the Flow layout keys off this.
    pub has_meaningful_topological_rank: bool,
    /// Memory-tab payload (structure only — ids, ordering, link positions).
    pub memory_map: Value,
}

impl Default for PreprocessedGraph {
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            links: Vec::new(),
            color_maps: ColorMaps::default(),
            schema_graph: serde_json::json!({"nodes": [], "links": []}),
            schema_data: None,
            pipeline_stages: Vec::new(),
            edge_classes: BTreeMap::new(),
            bundles: BTreeMap::new(),
            provenance_index: BTreeMap::new(),
            has_meaningful_topological_rank: false,
            memory_map: Value::Object(Map::new()),
        }
    }
}

/// Turn raw `(nodes, edges)` from the graph adapter into a fully-enriched
/// snapshot.
///
/// Port of Python's `preprocess()` (`preprocessor.py:1219-1399`). Nodes gain
/// `id`/`color`/`name`/`is_unnamed`/`is_memory_learning`/`t_created`/`stage`/
/// `visual_rank`/`degree`/`importance`/`label_priority`/`provenance`; links
/// gain `edge_class`, `bundle_key` and the endpoint stages.
///
/// Rust edges are always 4-tuples, so Python's 3-tuple tolerance
/// (`preprocessor.py:1291-1297`) maps onto "the property map is empty": an
/// empty map yields `weight: null`, `all_weights: {}`,
/// `relationship_type: null` and `edge_info: {}`, exactly like Python's falsy
/// `edge_info` branch.
pub fn preprocess(
    nodes: Vec<GraphNode>,
    edges: Vec<EdgeData>,
    schema_data: Option<Value>,
) -> PreprocessedGraph {
    // ── Nodes pass 1: normalize, color, name, stage ──────────────────────────
    let mut out_nodes: Vec<Value> = Vec::with_capacity(nodes.len());
    let mut has_meaningful_rank = false;

    for (node_id, props) in nodes {
        let mut node = props_to_object(props);
        node.insert("id".to_string(), Value::String(node_id.clone()));

        let ontology_valid = matches!(node.get("ontology_valid"), Some(Value::Bool(true)));
        let color = type_color(node.get("type"), ontology_valid);
        node.insert("color".to_string(), Value::String(color.to_string()));

        // Distilled session-learning nodes get a ring overlay in the renderer
        // (the type fill is preserved) so the self-improvement feature is visible.
        let is_memory_learning = naming::is_distilled_learning_node(&node);
        node.insert(
            "is_memory_learning".to_string(),
            Value::Bool(is_memory_learning),
        );

        let raw_name = node.get("name").cloned();
        let name = naming::derive_node_name(&node, &node_id);
        // Unnamed nodes (UUID/hash names) must never become Key-mode label
        // landmarks; pass 3 reads this flag.
        let is_unnamed = raw_name.as_ref().is_some_and(naming::looks_like_identifier)
            || name.starts_with("Unnamed ");
        node.insert("name".to_string(), Value::String(name));
        node.insert("is_unnamed".to_string(), Value::Bool(is_unnamed));

        // Preserve the creation timestamp (epoch ms) for the Memory timeline
        // before the raw audit columns are dropped. Python requires a real
        // `int` here (`bool` excluded), which `Number::as_i64` reproduces —
        // it never matches a JSON boolean or float.
        let created_at = node.get("created_at").and_then(|value| match value {
            Value::Number(number) => number.as_i64().map(|_| value.clone()),
            _ => None,
        });
        if let Some(created_at) = created_at {
            node.insert("t_created".to_string(), created_at);
        }
        node.remove("updated_at");
        node.remove("created_at");

        let stage = stage_for_node(&node);
        node.insert("stage".to_string(), Value::String(stage.to_string()));
        node.insert(
            "visual_rank".to_string(),
            Value::from(visual_rank(&node, stage)),
        );
        node.insert("degree".to_string(), Value::from(0)); // filled in pass 2
        node.insert("importance".to_string(), Value::from(0.0)); // filled in pass 2
        node.insert("label_priority".to_string(), Value::Bool(false)); // pass 3

        if let Some(provenance) = compact_provenance(&node) {
            node.insert("provenance".to_string(), provenance);
        }

        // `topological_rank` itself survives into the emitted node: the
        // vendored JS re-derives its own copy of this flag from it
        // (`views/story_view.js:106-109`).
        if let Some(Value::Number(number)) = node.get("topological_rank")
            && number.as_f64().unwrap_or(0.0) != 0.0
        {
            has_meaningful_rank = true;
        }

        out_nodes.push(Value::Object(node));
    }

    let stage_by_id: HashMap<String, String> = out_nodes
        .iter()
        .filter_map(|node| {
            Some((
                str_prop(node, "id")?.to_string(),
                str_prop(node, "stage")?.to_string(),
            ))
        })
        .collect();

    // ── Links pass: normalize, classify, weight, bundle ──────────────────────
    let mut links: Vec<Value> = Vec::with_capacity(edges.len());
    let mut edge_classes: BTreeMap<String, usize> = BTreeMap::new();
    let mut bundles: BTreeMap<String, usize> = BTreeMap::new();
    let mut degree_counter: HashMap<String, usize> = HashMap::new();

    for (source, target, relation, props) in edges {
        let edge_info = props_to_object(props);

        let mut all_weights: Map<String, Value> = Map::new();
        let mut primary_weight: Option<Value> = None;
        // Python's `if edge_info:` — an empty property map skips weights and
        // leaves `relationship_type` null.
        if !edge_info.is_empty() {
            if let Some(weight) = edge_info.get("weight") {
                all_weights.insert("default".to_string(), weight.clone());
                primary_weight = Some(weight.clone());
            }
            if let Some(Value::Object(weights)) = edge_info.get("weights") {
                for (key, value) in weights {
                    all_weights.insert(key.clone(), value.clone());
                }
                if primary_weight.is_none()
                    && let Some((_, first)) = weights.iter().next()
                {
                    primary_weight = Some(first.clone());
                }
            }
            for (key, value) in &edge_info {
                if let Some(suffix) = key.strip_prefix("weight_")
                    && value.is_number()
                {
                    all_weights.insert(suffix.to_string(), value.clone());
                }
            }
        }

        let class = edge_class(&relation, &edge_info);
        let source_stage = stage_by_id.get(&source).map_or("other", String::as_str);
        let target_stage = stage_by_id.get(&target).map_or("other", String::as_str);
        let bundle = bundle_key(source_stage, target_stage, class, &relation);

        *edge_classes.entry(class.to_string()).or_insert(0) += 1;
        *bundles.entry(bundle.clone()).or_insert(0) += 1;
        // Undirected and unconditional: a self-loop counts twice, and endpoints
        // missing from the node list still accumulate (Python Counter).
        *degree_counter.entry(source.clone()).or_insert(0) += 1;
        *degree_counter.entry(target.clone()).or_insert(0) += 1;

        let relationship_type = if edge_info.is_empty() {
            Value::Null
        } else {
            edge_info
                .get("relationship_type")
                .cloned()
                .unwrap_or(Value::Null)
        };

        let mut link = Map::new();
        link.insert("source".to_string(), Value::String(source));
        link.insert("target".to_string(), Value::String(target));
        link.insert("relation".to_string(), Value::String(relation));
        link.insert("weight".to_string(), primary_weight.unwrap_or(Value::Null));
        link.insert("all_weights".to_string(), Value::Object(all_weights));
        link.insert("relationship_type".to_string(), relationship_type);
        link.insert("edge_info".to_string(), Value::Object(edge_info));
        link.insert("edge_class".to_string(), Value::String(class.to_string()));
        link.insert("bundle_key".to_string(), Value::String(bundle));
        link.insert(
            "source_stage".to_string(),
            Value::String(source_stage.to_string()),
        );
        link.insert(
            "target_stage".to_string(),
            Value::String(target_stage.to_string()),
        );
        links.push(Value::Object(link));
    }

    // ── Nodes pass 2: degree, importance, semantic type ──────────────────────
    let semantic_types = schema_graph::resolve_semantic_types(&out_nodes, &links);
    for node in &mut out_nodes {
        let entity_type = str_prop(node, "id")
            .and_then(|id| semantic_types.get(id))
            .cloned()
            .unwrap_or_else(|| "Node".to_string());
        if let Some(object) = node.as_object_mut() {
            object.insert("entity_type".to_string(), Value::String(entity_type));
        }
    }

    let max_degree = degree_counter.values().copied().max().unwrap_or(1).max(1);
    let scale = (max_degree as f64).ln_1p();
    for node in &mut out_nodes {
        let degree = str_prop(node, "id")
            .and_then(|id| degree_counter.get(id).copied())
            .unwrap_or(0);
        if let Some(object) = node.as_object_mut() {
            object.insert("degree".to_string(), Value::from(degree));
            // log-scaled, capped — importance is a normalized 0..1 visual
            // weight, not a semantic score.
            object.insert(
                "importance".to_string(),
                Value::from((degree as f64).ln_1p() / scale),
            );
        }
    }

    // ── Nodes pass 3: label priority budget ──────────────────────────────────
    let importances: Vec<f64> = out_nodes
        .iter()
        .map(|node| {
            node.get("importance")
                .and_then(Value::as_f64)
                .unwrap_or(0.0)
        })
        .collect();
    let threshold = label_priority_threshold(&importances, LABEL_PRIORITY_PERCENTILE);
    for node in &mut out_nodes {
        let is_unnamed = node
            .get("is_unnamed")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let stage = str_prop(node, "stage").unwrap_or("other").to_string();
        let importance = node
            .get("importance")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);

        let label_priority = if is_unnamed {
            // A placeholder name is never worth a Key-mode label slot.
            false
        } else if ALWAYS_LABEL_STAGES.contains(&stage.as_str()) {
            true
        } else {
            importance >= threshold && threshold > 0.0
        };
        if let Some(object) = node.as_object_mut() {
            object.insert("label_priority".to_string(), Value::Bool(label_priority));
        }
    }

    // ── Color maps (verbatim shape from the original orchestrator) ───────────
    let color_maps = ColorMaps {
        task: provenance_colors(node_strings(&out_nodes, "source_task")),
        pipeline: provenance_colors(node_strings(&out_nodes, "source_pipeline")),
        // Hue rotation with the self-improvement node sets pinned.
        node_set: build_node_set_colors(node_strings(&out_nodes, "source_node_set")),
        user: provenance_colors(node_strings(&out_nodes, "source_user")),
    };

    let mut schema_graph = schema_graph::extract_schema_graph_data(&out_nodes, &links);
    schema_graph::build_operation_layer(&mut schema_graph, &out_nodes, &links);
    // Internal to the read; clients get it as `entity_type`.
    for node in &mut out_nodes {
        if let Some(object) = node.as_object_mut() {
            object.shift_remove(SEMANTIC_TYPE_KEY);
        }
    }

    // Stages present in the graph, in canonical left-to-right order.
    let pipeline_stages: Vec<String> = STAGE_ORDER
        .iter()
        .filter(|stage| {
            out_nodes
                .iter()
                .any(|node| str_prop(node, "stage") == Some(**stage))
        })
        .map(|stage| (*stage).to_string())
        .collect();

    let provenance_index: BTreeMap<String, Value> = out_nodes
        .iter()
        .filter_map(|node| {
            let provenance = node.get("provenance").filter(|value| is_truthy(value))?;
            Some((str_prop(node, "id")?.to_string(), provenance.clone()))
        })
        .collect();

    let memory_map = memory_map::build(&out_nodes, &links);

    PreprocessedGraph {
        nodes: out_nodes,
        links,
        color_maps,
        schema_graph,
        schema_data,
        pipeline_stages,
        edge_classes,
        bundles,
        provenance_index,
        has_meaningful_topological_rank: has_meaningful_rank,
        memory_map,
    }
}

/// Collect a string-valued provenance property across nodes, for the color maps.
fn node_strings(nodes: &[Value], key: &str) -> Vec<Option<String>> {
    nodes
        .iter()
        .map(|node| str_prop(node, key).map(str::to_string))
        .collect()
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

    fn object(value: Value) -> Map<String, Value> {
        value
            .as_object()
            .expect("test fixture is a JSON object")
            .clone()
    }

    #[test]
    fn stage_for_known_and_unknown_types() {
        for (node_type, stage) in STAGE_BY_TYPE {
            let node = object(json!({"type": node_type}));
            assert_eq!(stage_for_node(&node), stage, "type {node_type}");
        }
        assert_eq!(stage_for_node(&object(json!({"type": "Mystery"}))), "other");
        // Unlike the color lookup, an absent/null type is "other", not a
        // "default" entry.
        assert_eq!(stage_for_node(&object(json!({}))), "other");
        assert_eq!(stage_for_node(&object(json!({"type": null}))), "other");
    }

    #[test]
    fn visual_rank_prefers_positive_topological_rank() {
        let node = object(json!({"topological_rank": 7}));
        assert_eq!(visual_rank(&node, "document"), 7);
        // Floats truncate.
        let float = object(json!({"topological_rank": 3.9}));
        assert_eq!(visual_rank(&float, "document"), 3);
        // Zero / negative / null / absent fall back to the stage order.
        for rank in [json!(0), json!(-4), json!(null), json!("2")] {
            let node = object(json!({"topological_rank": rank}));
            assert_eq!(visual_rank(&node, "entity"), 3);
        }
        assert_eq!(visual_rank(&object(json!({})), "other"), 8);
        assert_eq!(visual_rank(&object(json!({})), "nonsense"), 8);
    }

    #[test]
    fn edge_class_reads_relation_and_relationship_name() {
        let empty = Map::new();
        assert_eq!(edge_class("contains", &empty), "structural");
        assert_eq!(edge_class("CONTAINS", &empty), "structural");
        assert_eq!(edge_class("knows", &empty), "semantic");
        let structural_info = object(json!({"relationship_name": "made_from"}));
        assert_eq!(edge_class("weird", &structural_info), "structural");
    }

    #[test]
    fn link_relation_precedence() {
        let link = json!({
            "relationship_type": "TOP",
            "edge_info": {"relationship_name": "middle", "relationship_type": "low"},
            "relation": "bottom",
        });
        assert_eq!(link_relation(&link), "TOP");

        let no_top = json!({
            "relationship_type": null,
            "edge_info": {"relationship_name": "middle", "relationship_type": "low"},
            "relation": "bottom",
        });
        assert_eq!(link_relation(&no_top), "middle");

        let only_edge_type = json!({"edge_info": {"relationship_type": "low"}, "relation": "b"});
        assert_eq!(link_relation(&only_edge_type), "low");

        let only_relation = json!({"edge_info": {}, "relation": "bottom"});
        assert_eq!(link_relation(&only_relation), "bottom");

        assert_eq!(link_relation(&json!({"relation": ""})), "related");
        assert_eq!(link_relation(&json!({})), "related");
    }

    #[test]
    fn compact_provenance_only_keeps_truthy_fields() {
        let node = object(json!({
            "source_task": "t",
            "source_pipeline": "",
            "source_node_set": null,
            "source_user": "u",
        }));
        assert_eq!(
            compact_provenance(&node),
            Some(json!({"source_task": "t", "source_user": "u"}))
        );
        assert_eq!(compact_provenance(&object(json!({"other": 1}))), None);
    }

    #[test]
    fn label_priority_threshold_matches_percentile_rank() {
        assert_eq!(label_priority_threshold(&[], 0.75), 0.0);
        // len 5 → rank = int(0.75 * 4) = 3 → the 4th smallest value.
        let values = [0.0, 0.25, 0.5, 0.75, 1.0];
        assert_eq!(label_priority_threshold(&values, 0.75), 0.75);
        // Non-finite values are dropped before ranking.
        let with_nan = [f64::NAN, 0.5, 1.0];
        assert!(label_priority_threshold(&with_nan, 0.75) > 0.0);
    }

    #[test]
    fn py_str_and_truthiness() {
        assert_eq!(py_str(&json!("text")), "text");
        assert_eq!(py_str(&json!(12)), "12");
        assert!(is_truthy(&json!("x")));
        assert!(!is_truthy(&json!("")));
        assert!(!is_truthy(&json!(0)));
        assert!(!is_truthy(&json!(null)));
        assert!(is_truthy(&json!(0.5)));
        assert!(!is_truthy(&json!({})));
        assert!(is_truthy(&json!({"a": 1})));
    }
}
