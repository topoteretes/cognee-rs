//! Schema-graph extraction for the visualization preprocessor.
//!
//! Faithful port of the schema half of
//! [`cognee/modules/visualization/preprocessor.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/visualization/preprocessor.py):
//!
//! | Rust | Python |
//! |---|---|
//! | [`node_type_rank`] | `node_type_rank` (`:219-252`) |
//! | [`extract_schema_fields`] | `_coerce_json_value` / `_field_from_column` / `extract_schema_fields` (`:255-330`) |
//! | [`extract_schema_graph_data`] | `extract_schema_graph_data` (`:332-377`) |
//! | [`resolve_semantic_types`] | `resolve_semantic_types` (`:486-516`) |
//! | [`extract_type_schema_graph_data`] | `extract_type_schema_graph_data` (`:518-759`) |
//! | [`build_operation_layer`] | `build_operation_layer` (`:762-844`) |
//!
//! Inputs are the *already-normalized* preprocessor nodes/links produced by
//! [`super`]: each node is a JSON object carrying at least `id` (string),
//! `type`, `name` and `degree`; each link carries `source`, `target`,
//! `relation`, `edge_info`, `relationship_type` and `edge_class`.
//!
//! # Ordering
//!
//! Python leans on `dict`/`Counter` insertion order in several places, and
//! `Counter.most_common` breaks ties by first-seen order. Those orderings are
//! observable in the rendered output, so this port never iterates a `HashMap`
//! to produce output: it uses [`OrderedCounter`] (a first-seen-ordered counter
//! with a stable descending-count sort) wherever Python uses a `Counter`, and
//! `BTreeMap` wherever Python sorts explicitly.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde_json::{Map, Value, json};

use super::operations_catalog::{Effect, OPERATIONS, Operation};
use super::{ENTITY_TYPE_RELATION, link_relation, py_str};

// ── Constants ────────────────────────────────────────────────────────────────

/// Node types that make the schema view render the DLT/structured-schema graph
/// instead of the collapsed type graph (`preprocessor.py:24-29`).
pub const SCHEMA_GRAPH_NODE_TYPES: [&str; 4] = [
    "DatabaseSchema",
    "SchemaTable",
    "SchemaRelationship",
    "TableType",
];

/// Maximum sample instance names attached to each schema type node
/// (`preprocessor.py:32`).
pub const SCHEMA_SAMPLES_PER_TYPE: usize = 5;

/// Maximum semantic entity-type cards in the Schema view's Entity column
/// (`preprocessor.py:39`).
///
/// Entity-type diversity grows with the data (every new `EntityType` the LLM
/// extracts becomes its own card), so beyond this cap the long tail is rolled up
/// into a single [`OTHER_ENTITY_TYPES_LABEL`] card — the renderer stacks one
/// card per type per rank column, which otherwise made the Entity column
/// endless.
pub const SCHEMA_MAX_ENTITY_TYPES: usize = 12;

/// Display name of the rollup card holding the entity-type long tail
/// (`preprocessor.py:42`).
pub const OTHER_ENTITY_TYPES_LABEL: &str = "Other entities";

/// Internal graph taxonomy types that must not appear as separate type groups
/// in the schema view (`preprocessor.py:50`).
///
/// This is **empty** in the preprocessor: `EntityType` is deliberately surfaced
/// as its own schema card alongside the resolved semantic entity types
/// (Person/Field/…), while `Entity` instances still collapse to their semantic
/// type via the `is_a` edge. The constant (and the guards that reference it) are
/// kept so future genuinely-internal types can be added without re-plumbing.
/// Note the same-named set in Python's `get_schema_inventory.py` is *not* empty;
/// that divergence is deliberate upstream.
const INTERNAL_TYPES: &[&str] = &[];

/// True when a resolved type name must be hidden from the schema view.
fn is_internal(type_name: &str) -> bool {
    INTERNAL_TYPES.contains(&type_name)
}

// ── Python-semantics helpers ─────────────────────────────────────────────────
//
// Python's `str()` and truthiness rules live once, in [`super`]. Two copies of
// either would have to be hand-synced, and a parity fix applied to only one
// would silently make the Schema view and the Memory/Story views disagree about
// the same property — so the two adapters below delegate rather than reimplement.

/// Python truthiness for an *optional* JSON value — [`super::is_truthy`] with a
/// missing key folded in as falsy (Python's `node.get(k)` yields `None`).
fn py_truthy(value: Option<&Value>) -> bool {
    value.is_some_and(super::is_truthy)
}

/// Return `value` when it is Python-truthy, so `a or b` chains read naturally.
fn truthy(value: Option<&Value>) -> Option<&Value> {
    value.filter(|inner| super::is_truthy(inner))
}

/// Python `round()` — half-way values round to the **even** neighbour.
///
/// `f64::round` rounds half away from zero, which would report a 12.5% field
/// coverage as 13% where Python reports 12%.
fn py_round(value: f64) -> i64 {
    let fract = value - value.trunc();
    if fract.abs() == 0.5 {
        let floor = value.floor() as i64;
        if floor % 2 == 0 {
            floor
        } else {
            value.ceil() as i64
        }
    } else {
        value.round() as i64
    }
}

/// Stringified node id, mirroring Python's `str(node_id)` / `str(link["source"])`.
fn node_id(node: &Value) -> Option<String> {
    node.get("id").map(py_str)
}

/// Stringified link endpoint. Python indexes `link["source"]` directly and would
/// raise on a malformed link; the port degrades to an empty id, which simply
/// fails every downstream lookup.
fn link_endpoint(link: &Value, key: &str) -> String {
    link.get(key).map(py_str).unwrap_or_default()
}

/// A node's `degree`, mirroring Python's `tn.get("degree") or 0`.
fn node_degree(node: &Value) -> i64 {
    match node.get("degree") {
        Some(Value::Number(number)) => match number.as_i64() {
            Some(degree) => degree,
            None => number.as_f64().map_or(0, |float| float as i64),
        },
        _ => 0,
    }
}

/// A node's display name as a sort key, mirroring `tn.get("name") or ""`.
fn node_name_key(node: &Value) -> String {
    truthy(node.get("name")).map(py_str).unwrap_or_default()
}

// ── Insertion-ordered counter ────────────────────────────────────────────────

/// Insertion-ordered multiset mirroring Python's `collections.Counter`.
///
/// `Counter.most_common()` sorts by descending count with a *stable* sort, so
/// equal counts keep first-seen order; `most_common(n)` uses `heapq.nlargest`,
/// which breaks ties the same way. Both are reproduced by [`Self::most_common`]
/// (a stable sort over the first-seen key order), which is why this type exists
/// instead of a bare `HashMap`.
#[derive(Debug, Clone)]
struct OrderedCounter<K> {
    /// Keys in first-seen order.
    order: Vec<K>,
    /// Count per key.
    counts: HashMap<K, usize>,
}

impl<K> Default for OrderedCounter<K> {
    fn default() -> Self {
        Self {
            order: Vec::new(),
            counts: HashMap::new(),
        }
    }
}

impl<K: std::hash::Hash + Eq + Clone> OrderedCounter<K> {
    /// An empty counter.
    fn new() -> Self {
        Self::default()
    }

    /// Python `counter[key] += 1`.
    fn increment(&mut self, key: K) {
        match self.counts.get_mut(&key) {
            Some(count) => *count += 1,
            None => {
                self.order.push(key.clone());
                self.counts.insert(key, 1);
            }
        }
    }

    /// Python `counter[key]` (0 for unseen keys).
    fn get(&self, key: &K) -> usize {
        self.counts.get(key).copied().unwrap_or(0)
    }

    /// Python `key in counter`.
    fn contains(&self, key: &K) -> bool {
        self.counts.contains_key(key)
    }

    /// Python `len(counter)` — the number of distinct keys.
    fn distinct(&self) -> usize {
        self.order.len()
    }

    /// Python `sum(counter.values())`.
    fn total(&self) -> usize {
        self.counts.values().sum()
    }

    /// Python `counter.most_common()`: descending count, first-seen order on ties.
    fn most_common(&self) -> Vec<(K, usize)> {
        let mut items: Vec<(K, usize)> = self
            .order
            .iter()
            .map(|key| (key.clone(), self.get(key)))
            .collect();
        // Stable sort: equal counts keep their first-seen relative order.
        items.sort_by(|left, right| right.1.cmp(&left.1));
        items
    }

    /// Python `counter.most_common(1)[0][0]`, or `None` on an empty counter.
    fn top(&self) -> Option<K> {
        self.most_common().into_iter().next().map(|(key, _)| key)
    }

    /// Python `counter.items()` in first-seen order.
    fn items(&self) -> Vec<(K, usize)> {
        self.order
            .iter()
            .map(|key| (key.clone(), self.get(key)))
            .collect()
    }
}

// ── node_type_rank ───────────────────────────────────────────────────────────

/// Column rank for a node type in the Schema view (`preprocessor.py:219-252`).
///
/// Despite Python's docstring calling it a `topological_rank` fallback, the only
/// consumer is the Schema tab's column layout (`views/schema_view.js:38-60`,
/// which maps `-5..5` onto Organization/People/Agents/Sessions/Brain/Documents/…
/// /Context), so the exact numbers are observable and must not be renumbered.
///
/// Actor / ownership types occupy negative ranks so they flow in *before* the
/// document pipeline: agents write sessions, which are recorded into the brains
/// they belong to. Unknown types fall through to `4`.
pub fn node_type_rank(node_type: Option<&str>) -> i32 {
    match node_type {
        // Actor & ownership layer (left of the document pipeline)
        Some("Tenant") => -5,
        Some("User") => -4,
        Some("Agent") => -3,
        Some("Session") => -2,
        Some("Dataset") => -1,
        // Document → memory pipeline
        Some("TextDocument") => 0,
        Some("DocumentChunk") => 1,
        Some("Entity") => 2,
        Some("EntityType") => 3,
        Some("TextSummary") => 4,
        Some("GlobalContextSummary") => 5,
        // Structured-schema (DLT) taxonomy
        Some("DatabaseSchema") => 0,
        Some("SchemaTable") => 1,
        Some("SchemaRelationship") => 2,
        Some("TableType") => 1,
        Some("TableRow") => 2,
        Some("ColumnValue") => 3,
        _ => 4,
    }
}

// ── DLT / structured-schema fields ───────────────────────────────────────────

/// Python `_coerce_json_value` (`preprocessor.py:255-263`).
///
/// Dicts/lists pass through; a string field that parses as JSON is accepted;
/// anything else is `None`.
fn coerce_json_value(value: Option<&Value>) -> Option<Value> {
    match value {
        Some(inner @ (Value::Object(_) | Value::Array(_))) => Some(inner.clone()),
        Some(Value::String(text)) if !text.trim().is_empty() => serde_json::from_str(text).ok(),
        _ => None,
    }
}

/// Python `_field_from_column` (`preprocessor.py:266-290`).
fn field_from_column(column: &Value) -> Option<Value> {
    if let Value::String(text) = column {
        return Some(json!({ "name": text, "type": "column", "required": false }));
    }
    let map = column.as_object()?;

    let name = truthy(map.get("name"))
        .or_else(|| truthy(map.get("column_name")))
        .or_else(|| truthy(map.get("field")))
        .or_else(|| truthy(map.get("key")))?;

    let column_type = truthy(map.get("type"))
        .or_else(|| truthy(map.get("data_type")))
        .or_else(|| truthy(map.get("python_type")))
        .or_else(|| truthy(map.get("nullable")))
        .map(py_str)
        .unwrap_or_else(|| "column".to_string());

    let mut required = py_truthy(map.get("primary_key")) || py_truthy(map.get("required"));
    if map.get("nullable") == Some(&Value::Bool(false)) {
        required = true;
    }

    Some(json!({ "name": py_str(name), "type": column_type, "required": required }))
}

/// Python `extract_schema_fields` (`preprocessor.py:293-330`).
///
/// Reads the DLT `columns` payload (a JSON object, a JSON array, or either of
/// those encoded as a string) and falls back to a fixed list of structured-schema
/// properties when no columns are declared.
///
/// Parity note: for the **object**-shaped `columns` payload Python emits fields
/// in the payload's own key order, which requires an insertion-ordered
/// `serde_json::Map`. The workspace root therefore enables
/// `serde_json/preserve_order` for every crate, so a `-p cognee-visualization`
/// build observes the same field order as the shipped workspace binary instead
/// of key-sorting the payload. The array-shaped payload (what DLT actually
/// writes) is unaffected either way, as is every field's name/type/required
/// content.
pub fn extract_schema_fields(node: &Value) -> Vec<Value> {
    let mut fields: Vec<Value> = Vec::new();
    let columns = coerce_json_value(node.get("columns"));

    match columns {
        Some(Value::Object(map)) => {
            for (name, column) in &map {
                let field = if let Some(column_map) = column.as_object() {
                    // Python `{"name": name, **column}`: an explicit "name" key
                    // inside the column payload overrides the map key.
                    let mut merged = Map::new();
                    merged.insert("name".to_string(), Value::String(name.clone()));
                    for (key, value) in column_map {
                        merged.insert(key.clone(), value.clone());
                    }
                    field_from_column(&Value::Object(merged))
                } else {
                    Some(json!({
                        "name": name,
                        "type": py_str(column),
                        "required": false,
                    }))
                };
                if let Some(field) = field {
                    fields.push(field);
                }
            }
        }
        Some(Value::Array(items)) => {
            for column in &items {
                if let Some(field) = field_from_column(column) {
                    fields.push(field);
                }
            }
        }
        _ => {}
    }

    if !fields.is_empty() {
        return fields;
    }

    const FALLBACK_KEYS: [&str; 8] = [
        "database_type",
        "primary_key",
        "source_table",
        "source_column",
        "target_table",
        "target_column",
        "relationship_type",
        "row_count_estimate",
    ];
    for key in FALLBACK_KEYS {
        match node.get(key) {
            None | Some(Value::Null) => continue,
            Some(Value::String(text)) if text.is_empty() => continue,
            Some(value) => {
                fields.push(json!({ "name": key, "type": py_str(value), "required": false }));
            }
        }
    }

    fields
}

// ── extract_schema_graph_data ────────────────────────────────────────────────

/// Build the DLT/structured-schema graph from `SchemaTable`/`SchemaRelationship`
/// nodes, falling back to [`extract_type_schema_graph_data`] when no schema
/// nodes are present (`preprocessor.py:332-377`).
///
/// The DLT branch emits only `{nodes, links}`; the (far more common) type-graph
/// fallback additionally emits `instances_by_type` and `instance_index`.
/// [`build_operation_layer`] then injects `operations` / `operation_links` into
/// whichever object was produced.
pub fn extract_schema_graph_data(nodes: &[Value], links: &[Value]) -> Value {
    let mut schema_nodes: Vec<Value> = Vec::new();
    let mut schema_node_ids: HashSet<String> = HashSet::new();

    for node in nodes {
        let is_schema_node = node
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|node_type| SCHEMA_GRAPH_NODE_TYPES.contains(&node_type));
        if !is_schema_node {
            continue;
        }
        let Some(id) = node_id(node) else { continue };

        schema_node_ids.insert(id.clone());
        let name = truthy(node.get("name"))
            .cloned()
            .unwrap_or_else(|| Value::String(id.clone()));
        schema_nodes.push(json!({
            "id": id,
            "name": name,
            "type": node.get("type").cloned().unwrap_or(Value::Null),
            "description": node.get("description").cloned().unwrap_or(Value::Null),
            "fields": extract_schema_fields(node),
            "source_table": node.get("source_table").cloned().unwrap_or(Value::Null),
            "target_table": node.get("target_table").cloned().unwrap_or(Value::Null),
            "relationship_type": node.get("relationship_type").cloned().unwrap_or(Value::Null),
        }));
    }

    if schema_nodes.is_empty() {
        return extract_type_schema_graph_data(nodes, links);
    }

    let mut schema_links: Vec<Value> = Vec::new();
    let mut seen_links: HashSet<(String, String, String)> = HashSet::new();
    for link in links {
        let source = link_endpoint(link, "source");
        let target = link_endpoint(link, "target");
        if !schema_node_ids.contains(&source) || !schema_node_ids.contains(&target) {
            continue;
        }
        let label = link_relation(link);
        if !seen_links.insert((source.clone(), target.clone(), label.clone())) {
            continue;
        }
        schema_links.push(json!({ "source": source, "target": target, "label": label }));
    }

    json!({ "nodes": schema_nodes, "links": schema_links })
}

// ── Type-graph aggregation ───────────────────────────────────────────────────

/// Python `_schema_value_type` (`preprocessor.py:380-393`).
///
/// `boolean` is checked before `integer` because Python bools *are* ints.
fn schema_value_type(value: &Value) -> &'static str {
    match value {
        Value::Bool(_) => "boolean",
        Value::Number(number) => {
            if number.is_f64() {
                "number"
            } else {
                "integer"
            }
        }
        Value::Array(_) => "array",
        Value::Object(_) => "object",
        Value::Null => "nullable",
        Value::String(_) => "string",
    }
}

/// Python `extract_type_schema_fields` (`preprocessor.py:396-457`).
///
/// The first field is always the instance `count`; up to five more follow,
/// ordered by the `preferred_fields` whitelist and then by descending
/// prevalence, each labelled `"<value type> <coverage>%"`.
///
/// Parity note — the tie-break, which is observable on every type card. Python's
/// `Counter.most_common()` breaks ties by the order in which the keys were first
/// seen while iterating the node dicts, and Python's node dict is
/// "adapter properties first, then the keys `preprocess()` appends". Two
/// consequences, in decreasing order of severity:
///
/// 1. **Derived keys must never outrank adapter properties.** They do not,
///    because `super::props_to_object` inserts the adapter's properties before
///    `preprocess()` stamps `degree`/`importance`/`stage`/… — but only while
///    `serde_json::Map` is insertion ordered. The workspace root therefore
///    enables `serde_json/preserve_order`; without it `Map` is a `BTreeMap`,
///    *every* key is globally alphabetical, and a `DocumentChunk` card reads
///    `…, chunk_index, degree` where Python reads `…, chunk_index, document_id`.
///    `tests/preprocessor_test.rs::type_card_fields_prefer_database_properties_
///    over_derived_keys` pins this.
/// 2. **Ties *between* adapter properties are still ordered differently.**
///    Python sees them in `json.dumps()` (model-field) order; Rust sees them
///    key-sorted, because `super::props_to_object` must impose *some* order on
///    a `cognee_graph::NodeData`, which is a `HashMap` with a randomly seeded
///    hasher. Closing this gap needs an order-preserving `NodeData` across the
///    workspace, so it is a known, bounded divergence rather than a fixed one: it
///    can only reshuffle equally-prevalent non-preferred properties among
///    themselves.
///
/// Counts, coverage, `required` and the preferred-field ordering are unaffected
/// by either.
fn extract_type_schema_fields(type_nodes: &[&Value]) -> Vec<Value> {
    const PREFERRED_FIELDS: [&str; 8] = [
        "source_task",
        "source_pipeline",
        "source_node_set",
        "source_user",
        "global_context_bucket_id",
        "level",
        "is_root",
        "topological_rank",
    ];
    const EXCLUDED_FIELDS: [&str; 15] = [
        "id",
        "type",
        "name",
        "color",
        "text",
        "summary",
        "content",
        "description",
        "metadata",
        "properties",
        "source_content_hash",
        "belongs_to_set",
        "ontology_valid",
        "feedback_weight",
        "importance_weight",
    ];

    let mut field_counts: OrderedCounter<String> = OrderedCounter::new();
    let mut field_types: HashMap<String, &'static str> = HashMap::new();

    for node in type_nodes {
        let Some(map) = node.as_object() else {
            continue;
        };
        for (key, value) in map {
            let skip = key.starts_with('_')
                || EXCLUDED_FIELDS.contains(&key.as_str())
                || value.is_null()
                || matches!(value, Value::String(text) if text.is_empty());
            if skip {
                continue;
            }
            field_counts.increment(key.clone());
            field_types
                .entry(key.clone())
                .or_insert_with(|| schema_value_type(value));
        }
    }

    let instance_count = type_nodes.len();
    let mut fields: Vec<Value> =
        vec![json!({ "name": "count", "type": instance_count.to_string(), "required": true })];

    let mut ordered_field_names: Vec<String> = Vec::new();
    for key in PREFERRED_FIELDS {
        let key = key.to_string();
        if field_counts.contains(&key) {
            ordered_field_names.push(key);
        }
    }
    for (key, _) in field_counts.most_common() {
        if !ordered_field_names.contains(&key) {
            ordered_field_names.push(key);
        }
    }

    for key in ordered_field_names.into_iter().take(5) {
        let count = field_counts.get(&key);
        let coverage = py_round(count as f64 / instance_count.max(1) as f64 * 100.0);
        let value_type = field_types.get(&key).copied().unwrap_or("any");
        fields.push(json!({
            "name": key,
            "type": format!("{value_type} {coverage}%"),
            "required": count == instance_count,
        }));
    }

    fields
}

/// Python `_relationship_label` (`preprocessor.py:460-466`).
///
/// `"<name> (<count>)"` for the top two relations joined by `", "`, plus
/// `"+<K> more"` when more distinct relations exist; `"<total> edges"` when the
/// counter is empty.
fn relationship_label(relation_counts: &OrderedCounter<String>) -> String {
    let total = relation_counts.total();
    let ranked = relation_counts.most_common();
    let top: Vec<(String, usize)> = ranked.into_iter().take(2).collect();
    let mut parts: Vec<String> = top
        .iter()
        .map(|(name, count)| format!("{name} ({count})"))
        .collect();
    if relation_counts.distinct() > top.len() {
        parts.push(format!("+{} more", relation_counts.distinct() - top.len()));
    }
    if parts.is_empty() {
        format!("{total} edges")
    } else {
        parts.join(", ")
    }
}

/// Map each node id to its semantic type name (`preprocessor.py:486-516`).
///
/// Non-`Entity` nodes keep their raw `type` property. `Entity` nodes resolve to
/// the `EntityType` `name` reached via the [`ENTITY_TYPE_RELATION`] edge, so
/// semantic types (Person/Tool/Broker) surface instead of the literal
/// `"Entity"`; a missing or falsy `EntityType` name leaves the raw type in
/// place, and a missing raw type becomes the literal `"Node"`.
///
/// Returns a `BTreeMap` (node-id ordered) — callers that need Python's node-list
/// iteration order walk the node slice instead of this map.
pub fn resolve_semantic_types(nodes: &[Value], links: &[Value]) -> BTreeMap<String, String> {
    let mut nodes_by_id: HashMap<String, &Value> = HashMap::new();
    for node in nodes {
        if let Some(id) = node_id(node) {
            // Python's dict comprehension keeps the *last* node for a dup id.
            nodes_by_id.insert(id, node);
        }
    }

    // Collect the EntityType target name for each Entity source via the is_a edge.
    let mut entity_type_name: HashMap<String, Option<Value>> = HashMap::new();
    for link in links {
        let target = link_endpoint(link, "target");
        if link_relation(link) != ENTITY_TYPE_RELATION {
            continue;
        }
        if let Some(target_node) = nodes_by_id.get(&target) {
            entity_type_name.insert(
                link_endpoint(link, "source"),
                target_node.get("name").cloned(),
            );
        }
    }

    let mut node_type: BTreeMap<String, String> = BTreeMap::new();
    for node in nodes {
        let Some(id) = node_id(node) else { continue };
        let raw_type = node.get("type");
        let is_entity = raw_type.and_then(Value::as_str) == Some("Entity");
        let resolved = entity_type_name
            .get(&id)
            .and_then(Option::as_ref)
            .and_then(|name| truthy(Some(name)));
        if is_entity && let Some(name) = resolved {
            node_type.insert(id, py_str(name));
        } else {
            node_type.insert(
                id,
                truthy(raw_type)
                    .map(py_str)
                    .unwrap_or_else(|| "Node".to_string()),
            );
        }
    }
    node_type
}

/// Per-instance adjacency record backing `instance_index`.
struct InstanceRecord {
    /// Display name (`name` or the stringified id).
    name: Value,
    /// Resolved semantic type name.
    type_name: String,
    /// Outgoing `{relation, id}` entries, in link-array order.
    out_edges: Vec<Value>,
    /// Incoming `{relation, id}` entries, in link-array order.
    in_edges: Vec<Value>,
}

/// Fallback schema view: collapse the graph to one node per semantic type
/// (`preprocessor.py:518-759`).
///
/// Emits `{nodes, links, instances_by_type, instance_index}` where `nodes`
/// carries one `GraphNodeType` card per semantic type followed by one
/// `GraphRelationshipType` node per `(source type, target type)` pair.
pub fn extract_type_schema_graph_data(nodes: &[Value], links: &[Value]) -> Value {
    let mut node_type_by_id = resolve_semantic_types(nodes, links);

    // Python iterates `node_type_by_id.values()`, i.e. unique node ids in
    // node-list order. Rebuild that order explicitly since we key by BTreeMap.
    let mut id_order: Vec<String> = Vec::new();
    let mut seen_ids: HashSet<String> = HashSet::new();
    for node in nodes {
        if let Some(id) = node_id(node)
            && seen_ids.insert(id.clone())
        {
            id_order.push(id);
        }
    }

    // Names reached via the is_a edge are semantic *entity* types (Person,
    // Broker, …) — rank them in the Entity column rather than letting them fall
    // through to the default ("Summaries") rank.
    let mut nodes_by_id_lookup: HashMap<String, &Value> = HashMap::new();
    for node in nodes {
        if let Some(id) = node_id(node) {
            nodes_by_id_lookup.insert(id, node);
        }
    }
    let mut semantic_type_names: HashSet<String> = HashSet::new();
    for link in links {
        if link_relation(link) != ENTITY_TYPE_RELATION {
            continue;
        }
        if let Some(target_node) = nodes_by_id_lookup.get(&link_endpoint(link, "target"))
            && let Some(name) = truthy(target_node.get("name"))
        {
            semantic_type_names.insert(py_str(name));
        }
    }

    // Bound the Entity column: keep the most-populated semantic entity types as
    // their own cards and remap the long tail onto one rollup type. The remap
    // happens on `node_type_by_id` *before* any downstream aggregation, so
    // relationship distributions, pair edges, instance drill-down and the
    // operation layer all treat the rollup as an ordinary type.
    let mut rolled_up: Vec<(String, usize)> = Vec::new();
    let mut entity_type_counts: OrderedCounter<String> = OrderedCounter::new();
    for id in &id_order {
        if let Some(type_name) = node_type_by_id.get(id)
            && semantic_type_names.contains(type_name)
        {
            entity_type_counts.increment(type_name.clone());
        }
    }
    if entity_type_counts.distinct() > SCHEMA_MAX_ENTITY_TYPES {
        let ranked = entity_type_counts.most_common();
        let kept: HashSet<String> = ranked
            .iter()
            .take(SCHEMA_MAX_ENTITY_TYPES - 1)
            .map(|(name, _)| name.clone())
            .collect();
        let rolled: HashSet<String> = ranked
            .iter()
            .filter(|(name, _)| !kept.contains(name))
            .map(|(name, _)| name.clone())
            .collect();
        rolled_up = ranked
            .into_iter()
            .filter(|(name, _)| rolled.contains(name))
            .collect();
        for type_name in node_type_by_id.values_mut() {
            if rolled.contains(type_name) {
                *type_name = OTHER_ENTITY_TYPES_LABEL.to_string();
            }
        }
        semantic_type_names.retain(|name| !rolled.contains(name));
        semantic_type_names.insert(OTHER_ENTITY_TYPES_LABEL.to_string());
    }
    let rolled_up_types: Vec<Value> = rolled_up
        .iter()
        .map(|(name, count)| json!({ "name": name, "count": count }))
        .collect();

    // Python `_rank_for` (`preprocessor.py:559-562`): semantic entity type names
    // land in the Entity column, everything else uses its own type rank.
    let rank_for = |type_name: &str| -> i32 {
        if semantic_type_names.contains(type_name) {
            node_type_rank(Some("Entity"))
        } else {
            node_type_rank(Some(type_name))
        }
    };

    // Python `sorted(nodes_by_type.items())` — type name ascending.
    let mut nodes_by_type: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for node in nodes {
        let Some(id) = node_id(node) else { continue };
        let Some(type_name) = node_type_by_id.get(&id) else {
            continue;
        };
        if is_internal(type_name) {
            continue;
        }
        nodes_by_type
            .entry(type_name.clone())
            .or_default()
            .push(node);
    }

    // Aggregate the full per-source-type relationship distribution keyed by
    // (relation, target_type). Both directions are tracked so types whose
    // primary connections are incoming (TextDocument→contains→DocumentChunk)
    // are not shown as isolated nodes.
    let mut relationships_by_type: HashMap<String, OrderedCounter<(String, String)>> =
        HashMap::new();
    for link in links {
        let source = link_endpoint(link, "source");
        let target = link_endpoint(link, "target");
        let (Some(source_type), Some(target_type)) =
            (node_type_by_id.get(&source), node_type_by_id.get(&target))
        else {
            continue;
        };
        if is_internal(source_type) || is_internal(target_type) {
            continue;
        }
        let (source_type, target_type) = (source_type.clone(), target_type.clone());
        let relation = link_relation(link);
        relationships_by_type
            .entry(source_type.clone())
            .or_default()
            .increment((relation.clone(), target_type.clone()));
        relationships_by_type
            .entry(target_type)
            .or_default()
            .increment((format!("\u{2190} {relation}"), source_type));
    }

    let mut schema_nodes: Vec<Value> = Vec::new();
    for (type_name, type_nodes) in &nodes_by_type {
        // Surface the most-common pipeline / task / user that produced this type
        // so the Schema card can show "produced by cognify_pipeline /
        // extract_graph_from_data" prominently rather than burying it as one of
        // many fields with a "string 100%" coverage label.
        let mut pipeline_counts: OrderedCounter<String> = OrderedCounter::new();
        let mut task_counts: OrderedCounter<String> = OrderedCounter::new();
        let mut user_counts: OrderedCounter<String> = OrderedCounter::new();
        for node in type_nodes {
            if let Some(value) = truthy(node.get("source_pipeline")) {
                pipeline_counts.increment(py_str(value));
            }
            if let Some(value) = truthy(node.get("source_task")) {
                task_counts.increment(py_str(value));
            }
            if let Some(value) = truthy(node.get("source_user")) {
                user_counts.increment(py_str(value));
            }
        }
        let as_value = |top: Option<String>| top.map(Value::String).unwrap_or(Value::Null);

        // Rank instances by descending degree, then name, so the sample list is
        // deterministic rather than map-order-dependent.
        let mut ranked: Vec<&Value> = type_nodes.to_vec();
        ranked.sort_by(|left, right| {
            node_degree(right)
                .cmp(&node_degree(left))
                .then_with(|| node_name_key(left).cmp(&node_name_key(right)))
        });
        let samples: Vec<Value> = ranked
            .iter()
            .take(SCHEMA_SAMPLES_PER_TYPE)
            .map(|node| node.get("name").cloned().unwrap_or(Value::Null))
            .collect();
        let sample_size = samples.len();

        // Full per-pair relationship distribution for this source type, sorted
        // by descending count then target/relation as stable tiebreakers.
        let mut relationship_items: Vec<((String, String), usize)> = relationships_by_type
            .get(type_name)
            .map(OrderedCounter::items)
            .unwrap_or_default();
        relationship_items.sort_by(|left, right| {
            right
                .1
                .cmp(&left.1)
                .then_with(|| left.0.1.cmp(&right.0.1))
                .then_with(|| left.0.0.cmp(&right.0.0))
        });
        let relationships: Vec<Value> = relationship_items
            .into_iter()
            .map(|((relation, to_type), count)| {
                json!({ "to_type": to_type, "relation": relation, "count": count })
            })
            .collect();

        let mut schema_node = json!({
            "id": format!("type:{type_name}"),
            "name": type_name,
            "type": "GraphNodeType",
            "rank": rank_for(type_name),
            "fields": extract_type_schema_fields(type_nodes),
            "source_pipeline": as_value(pipeline_counts.top()),
            "source_task": as_value(task_counts.top()),
            "source_user": as_value(user_counts.top()),
            "instance_count": type_nodes.len(),
            "samples": samples,
            "sample_size": sample_size,
            "relationships": relationships,
        });

        if type_name == OTHER_ENTITY_TYPES_LABEL
            && !rolled_up.is_empty()
            && let Some(object) = schema_node.as_object_mut()
        {
            object.insert("rollup".to_string(), Value::Bool(true));
            object.insert(
                "rolled_up_types".to_string(),
                Value::Array(rolled_up_types.clone()),
            );
            // Lead the card with the tail size and its largest types so the
            // rollup is self-explanatory without inspector drill-down.
            let top_tail = rolled_up
                .iter()
                .take(3)
                .map(|(name, count)| format!("{name} ({count})"))
                .collect::<Vec<String>>()
                .join(", ");
            let lead_field = json!({
                "name": "entity types",
                "type": format!("{} rolled up: {top_tail}, …", rolled_up.len()),
                "required": true,
            });
            if let Some(Value::Array(fields)) = object.get_mut("fields") {
                fields.insert(1, lead_field);
            }
        }
        schema_nodes.push(schema_node);
    }

    // Lossy per-pair edge labels: one relationship node per (source, target).
    let mut relation_counts_by_pair: HashMap<(String, String), OrderedCounter<String>> =
        HashMap::new();
    for link in links {
        let source = link_endpoint(link, "source");
        let target = link_endpoint(link, "target");
        let (Some(source_type), Some(target_type)) =
            (node_type_by_id.get(&source), node_type_by_id.get(&target))
        else {
            continue;
        };
        if is_internal(source_type) || is_internal(target_type) {
            continue;
        }
        relation_counts_by_pair
            .entry((source_type.clone(), target_type.clone()))
            .or_default()
            .increment(link_relation(link));
    }

    // Python `sorted(..., key=lambda item: (-sum(counts), src, tgt))`.
    let mut pairs: Vec<((String, String), OrderedCounter<String>)> =
        relation_counts_by_pair.into_iter().collect();
    pairs.sort_by(|left, right| {
        right
            .1
            .total()
            .cmp(&left.1.total())
            .then_with(|| left.0.0.cmp(&right.0.0))
            .then_with(|| left.0.1.cmp(&right.0.1))
    });

    let mut schema_links: Vec<Value> = Vec::new();
    for (index, ((source_type, target_type), relation_counts)) in pairs.into_iter().enumerate() {
        let rel_id = format!("rel:{index}:{source_type}:{target_type}");
        let source_rank = f64::from(rank_for(&source_type));
        let target_rank = f64::from(rank_for(&target_type));
        // Self-links (and any equal-rank pair) sit half a column to the right of
        // their type so the node is not drawn on top of the type card.
        let rel_rank = if source_rank == target_rank {
            source_rank + 0.5
        } else {
            (source_rank + target_rank) / 2.0
        };
        let edge_count = relation_counts.total();

        schema_nodes.push(json!({
            "id": rel_id,
            "name": if source_type == target_type {
                format!("{source_type} self-links")
            } else {
                format!("{source_type} to {target_type}")
            },
            "type": "GraphRelationshipType",
            "rank": rel_rank,
            "source_type": source_type,
            "target_type": target_type,
            "relationship_label": relationship_label(&relation_counts),
            "edge_count": edge_count,
            "fields": [
                { "name": "edges", "type": edge_count.to_string(), "required": true },
                {
                    "name": "top relation",
                    "type": relation_counts.top().unwrap_or_default(),
                    "required": true,
                },
                {
                    "name": "relation types",
                    "type": relation_counts.distinct().to_string(),
                    "required": true,
                },
            ],
        }));
        schema_links.push(
            json!({ "source": format!("type:{source_type}"), "target": rel_id, "label": "from" }),
        );
        schema_links.push(
            json!({ "source": rel_id, "target": format!("type:{target_type}"), "label": "to" }),
        );
    }

    // Instance-level drill-down data so the inspector can navigate
    // Type → instance → neighbours without dropping to the global graph:
    //   * instances_by_type: every instance name (not just the 5 samples)
    //   * instance_index: a compact per-instance adjacency
    // NOTE: for very large graphs this should be scoped/paginated; it is sized
    // to the schema graph (one entry per instance), fine for typical graphs.
    let mut instance_type_order: Vec<String> = Vec::new();
    let mut instances_by_type: HashMap<String, Vec<Value>> = HashMap::new();
    let mut instance_order: Vec<String> = Vec::new();
    let mut instance_index: HashMap<String, InstanceRecord> = HashMap::new();
    for node in nodes {
        let Some(id) = node_id(node) else { continue };
        let Some(type_name) = node_type_by_id.get(&id) else {
            continue;
        };
        if is_internal(type_name) {
            continue;
        }
        let display_name = truthy(node.get("name"))
            .cloned()
            .unwrap_or_else(|| Value::String(id.clone()));
        if !instances_by_type.contains_key(type_name) {
            instance_type_order.push(type_name.clone());
            instances_by_type.insert(type_name.clone(), Vec::new());
        }
        if let Some(bucket) = instances_by_type.get_mut(type_name) {
            bucket.push(json!({ "id": id, "name": display_name.clone() }));
        }
        if !instance_index.contains_key(&id) {
            instance_order.push(id.clone());
        }
        instance_index.insert(
            id,
            InstanceRecord {
                name: display_name,
                type_name: type_name.clone(),
                out_edges: Vec::new(),
                in_edges: Vec::new(),
            },
        );
    }
    for bucket in instances_by_type.values_mut() {
        // Python `sort(key=lambda rec: rec["name"])` — stable, name ascending.
        bucket.sort_by(|left, right| {
            let left_key = left.get("name").map(py_str).unwrap_or_default();
            let right_key = right.get("name").map(py_str).unwrap_or_default();
            left_key.cmp(&right_key)
        });
    }
    for link in links {
        let source = link_endpoint(link, "source");
        let target = link_endpoint(link, "target");
        if !instance_index.contains_key(&source) || !instance_index.contains_key(&target) {
            continue;
        }
        let relation = link_relation(link);
        if let Some(record) = instance_index.get_mut(&source) {
            record
                .out_edges
                .push(json!({ "relation": relation, "id": target.clone() }));
        }
        if let Some(record) = instance_index.get_mut(&target) {
            record
                .in_edges
                .push(json!({ "relation": relation, "id": source }));
        }
    }

    let mut instances_json = Map::new();
    for type_name in instance_type_order {
        if let Some(bucket) = instances_by_type.remove(&type_name) {
            instances_json.insert(type_name, Value::Array(bucket));
        }
    }
    let mut instance_index_json = Map::new();
    for id in instance_order {
        if let Some(record) = instance_index.remove(&id) {
            instance_index_json.insert(
                id.clone(),
                json!({
                    "id": id,
                    "name": record.name,
                    "type": record.type_name,
                    "out": record.out_edges,
                    "in": record.in_edges,
                }),
            );
        }
    }

    json!({
        "nodes": schema_nodes,
        "links": schema_links,
        "instances_by_type": Value::Object(instances_json),
        "instance_index": Value::Object(instance_index_json),
    })
}

// ── Operation impact layer ───────────────────────────────────────────────────

/// Attach the transformation impact-layer to `schema_graph` in place
/// (`preprocessor.py:762-844`).
///
/// For each catalog operation whose effects touch a schema type present in the
/// graph, emit a `GraphOperation` node and typed impact links
/// (produces/enriches/modifies/removes). `"Entity"` effects expand to the
/// semantic entity types actually present (Person/Broker/…). Links are flagged
/// `observed` when the live provenance (a type's modal `source_pipeline`)
/// matches the operation's pipeline. Operations that resolve to zero links are
/// dropped entirely; `operations` and `operation_links` are always set, even
/// when empty. Existing nodes/links are left untouched.
///
/// Parity note: Python iterates the resolved target names as a `set`, whose
/// order is arbitrary (and salted per interpreter run). This port iterates them
/// sorted by name so the emitted link order is reproducible.
pub fn build_operation_layer(schema_graph: &mut Value, nodes: &[Value], links: &[Value]) {
    let mut present: BTreeSet<String> = BTreeSet::new();
    let mut pipeline_by_type: HashMap<String, Option<String>> = HashMap::new();
    if let Some(type_nodes) = schema_graph.get("nodes").and_then(Value::as_array) {
        for node in type_nodes {
            if node.get("type").and_then(Value::as_str) != Some("GraphNodeType") {
                continue;
            }
            let Some(name) = node.get("name").map(py_str) else {
                continue;
            };
            present.insert(name.clone());
            pipeline_by_type.insert(
                name,
                node.get("source_pipeline")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            );
        }
    }

    // Semantic entity types are those reached via the is_a edge (Person/Broker/…).
    let mut nodes_by_id: HashMap<String, &Value> = HashMap::new();
    for node in nodes {
        if let Some(id) = node_id(node) {
            nodes_by_id.insert(id, node);
        }
    }
    let mut semantic_entity_types: BTreeSet<String> = BTreeSet::new();
    for link in links {
        if link_relation(link) != ENTITY_TYPE_RELATION {
            continue;
        }
        if let Some(target) = nodes_by_id.get(&link_endpoint(link, "target"))
            && let Some(name) = truthy(target.get("name"))
        {
            semantic_entity_types.insert(py_str(name));
        }
    }

    let resolve_targets = |effect: &Effect| -> BTreeSet<String> {
        let mut names: BTreeSet<String> = BTreeSet::new();
        if effect.target_type == "Entity" {
            names.extend(semantic_entity_types.intersection(&present).cloned());
            if present.contains("Entity") {
                names.insert("Entity".to_string());
            }
        } else if present.contains(effect.target_type) {
            names.insert(effect.target_type.to_string());
        }
        if let Some(node_set) = effect.target_node_set
            && present.contains(node_set)
        {
            names.insert(node_set.to_string());
        }
        names
    };

    let mut operations: Vec<Value> = Vec::new();
    let mut operation_links: Vec<Value> = Vec::new();
    for operation in OPERATIONS {
        let Operation {
            name,
            label,
            kind,
            scope,
            pipeline_name,
            summary,
            effects,
        } = operation;
        let mut seen: HashSet<(&str, String)> = HashSet::new();
        let mut links_for_op: Vec<Value> = Vec::new();
        for effect in *effects {
            for type_name in resolve_targets(effect) {
                if !seen.insert((effect.effect, type_name.clone())) {
                    continue;
                }
                let observed = pipeline_name.is_some()
                    && pipeline_by_type.get(&type_name).and_then(Option::as_deref)
                        == *pipeline_name;
                links_for_op.push(json!({
                    "source": format!("op:{name}"),
                    "target": format!("type:{type_name}"),
                    "effect": effect.effect,
                    "property": effect.property,
                    "observed": observed,
                }));
            }
        }
        if links_for_op.is_empty() {
            // Operation doesn't touch any type present in this graph.
            continue;
        }
        operations.push(json!({
            "id": format!("op:{name}"),
            "name": label,
            "type": "GraphOperation",
            "op_kind": kind,
            "scope": scope,
            "summary": summary,
        }));
        operation_links.extend(links_for_op);
    }

    if let Some(object) = schema_graph.as_object_mut() {
        object.insert("operations".to_string(), Value::Array(operations));
        object.insert("operation_links".to_string(), Value::Array(operation_links));
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    fn node(id: &str, node_type: &str, name: &str) -> Value {
        json!({ "id": id, "type": node_type, "name": name, "degree": 0 })
    }

    fn link(source: &str, target: &str, relation: &str) -> Value {
        json!({ "source": source, "target": target, "relation": relation, "edge_info": {} })
    }

    #[test]
    fn py_round_uses_bankers_rounding() {
        assert_eq!(py_round(12.5), 12);
        assert_eq!(py_round(37.5), 38);
        assert_eq!(py_round(50.0), 50);
        assert_eq!(py_round(33.333_333), 33);
        assert_eq!(py_round(66.666_666), 67);
    }

    #[test]
    fn ordered_counter_breaks_ties_by_first_seen() {
        let mut counter: OrderedCounter<&str> = OrderedCounter::new();
        counter.increment("b");
        counter.increment("a");
        counter.increment("c");
        counter.increment("c");
        assert_eq!(counter.most_common(), vec![("c", 2), ("b", 1), ("a", 1)]);
        assert_eq!(counter.top(), Some("c"));
        assert_eq!(counter.distinct(), 3);
        assert_eq!(counter.total(), 4);
    }

    #[test]
    fn relationship_label_matches_python_shapes() {
        let mut counter: OrderedCounter<String> = OrderedCounter::new();
        assert_eq!(relationship_label(&counter), "0 edges");

        counter.increment("contains".to_string());
        counter.increment("contains".to_string());
        assert_eq!(relationship_label(&counter), "contains (2)");

        counter.increment("is_a".to_string());
        assert_eq!(relationship_label(&counter), "contains (2), is_a (1)");

        counter.increment("knows".to_string());
        counter.increment("owns".to_string());
        assert_eq!(
            relationship_label(&counter),
            "contains (2), is_a (1), +2 more"
        );
    }

    #[test]
    fn schema_value_type_distinguishes_bools_from_integers() {
        assert_eq!(schema_value_type(&json!(true)), "boolean");
        assert_eq!(schema_value_type(&json!(3)), "integer");
        assert_eq!(schema_value_type(&json!(3.5)), "number");
        assert_eq!(schema_value_type(&json!([1])), "array");
        assert_eq!(schema_value_type(&json!({ "a": 1 })), "object");
        assert_eq!(schema_value_type(&Value::Null), "nullable");
        assert_eq!(schema_value_type(&json!("x")), "string");
    }

    #[test]
    fn extract_schema_fields_reads_columns_in_every_shape() {
        // JSON-encoded list of column dicts.
        let node = json!({
            "id": "t1",
            "type": "SchemaTable",
            "columns": "[{\"name\": \"id\", \"data_type\": \"bigint\", \"primary_key\": true}]",
        });
        assert_eq!(
            extract_schema_fields(&node),
            vec![json!({ "name": "id", "type": "bigint", "required": true })]
        );

        // Object of name -> column dict; nullable=false implies required.
        let node = json!({
            "id": "t1",
            "columns": { "email": { "type": "text", "nullable": false } },
        });
        assert_eq!(
            extract_schema_fields(&node),
            vec![json!({ "name": "email", "type": "text", "required": true })]
        );

        // Object of name -> scalar type.
        let node = json!({ "id": "t1", "columns": { "age": "int" } });
        assert_eq!(
            extract_schema_fields(&node),
            vec![json!({ "name": "age", "type": "int", "required": false })]
        );

        // Bare string columns.
        let node = json!({ "id": "t1", "columns": ["a"] });
        assert_eq!(
            extract_schema_fields(&node),
            vec![json!({ "name": "a", "type": "column", "required": false })]
        );

        // Fallback keys when no columns are declared; empty strings are skipped.
        let node = json!({
            "id": "r1",
            "database_type": "postgres",
            "source_table": "orders",
            "target_table": "",
            "relationship_type": "many_to_one",
        });
        assert_eq!(
            extract_schema_fields(&node),
            vec![
                json!({ "name": "database_type", "type": "postgres", "required": false }),
                json!({ "name": "source_table", "type": "orders", "required": false }),
                json!({ "name": "relationship_type", "type": "many_to_one", "required": false }),
            ]
        );
    }

    #[test]
    fn field_from_column_rejects_nameless_columns() {
        assert!(field_from_column(&json!({ "data_type": "text" })).is_none());
        assert!(field_from_column(&json!(7)).is_none());
        assert_eq!(
            field_from_column(&json!({ "key": "k" })),
            Some(json!({ "name": "k", "type": "column", "required": false }))
        );
    }

    #[test]
    fn coerce_json_value_only_accepts_parsable_strings() {
        assert_eq!(
            coerce_json_value(Some(&json!({ "a": 1 }))),
            Some(json!({ "a": 1 }))
        );
        assert_eq!(coerce_json_value(Some(&json!("[1]"))), Some(json!([1])));
        assert_eq!(coerce_json_value(Some(&json!("not json"))), None);
        assert_eq!(coerce_json_value(Some(&json!("   "))), None);
        assert_eq!(coerce_json_value(Some(&json!(5))), None);
        assert_eq!(coerce_json_value(None), None);
    }

    #[test]
    fn resolve_semantic_types_falls_back_to_node_and_raw_type() {
        let nodes = vec![
            node("e1", "Entity", "Alice"),
            node("t1", "EntityType", "Person"),
            // Entity with an is_a edge to an unnamed EntityType keeps "Entity".
            node("e2", "Entity", "Bob"),
            json!({ "id": "t2", "type": "EntityType", "name": "" }),
            // No type at all -> "Node".
            json!({ "id": "x1", "name": "mystery" }),
        ];
        let links = vec![link("e1", "t1", "is_a"), link("e2", "t2", "is_a")];
        let resolved = resolve_semantic_types(&nodes, &links);
        assert_eq!(resolved["e1"], "Person");
        assert_eq!(resolved["e2"], "Entity");
        assert_eq!(resolved["t1"], "EntityType");
        assert_eq!(resolved["x1"], "Node");
    }

    #[test]
    fn extract_type_schema_fields_leads_with_count_and_preferred_fields() {
        let first = json!({
            "id": "a",
            "type": "DocumentChunk",
            "name": "chunk a",
            "text": "excluded",
            "source_pipeline": "cognify_pipeline",
            "source_task": "extract_chunks_from_documents",
            "chunk_index": 0,
        });
        let second = json!({
            "id": "b",
            "type": "DocumentChunk",
            "name": "chunk b",
            "source_pipeline": "cognify_pipeline",
        });
        let fields = extract_type_schema_fields(&[&first, &second]);
        assert_eq!(
            fields[0],
            json!({ "name": "count", "type": "2", "required": true })
        );
        // `source_task` precedes `source_pipeline` (whitelist order) even though
        // it is the less prevalent field.
        assert_eq!(fields[1]["name"], "source_task");
        assert_eq!(fields[1]["type"], "string 50%");
        assert_eq!(fields[1]["required"], false);
        assert_eq!(fields[2]["name"], "source_pipeline");
        assert_eq!(fields[2]["type"], "string 100%");
        assert_eq!(fields[2]["required"], true);
        // Excluded keys never surface.
        let names: Vec<&str> = fields.iter().filter_map(|f| f["name"].as_str()).collect();
        assert!(!names.contains(&"text"));
        assert!(!names.contains(&"name"));
        assert!(!names.contains(&"id"));
        // `chunk_index` is a non-preferred integer field and is kept.
        assert!(names.contains(&"chunk_index"));
    }

    #[test]
    fn extract_type_schema_fields_caps_at_six_entries() {
        let node = json!({
            "id": "a",
            "source_task": "t",
            "source_pipeline": "p",
            "source_node_set": "s",
            "source_user": "u",
            "level": 1,
            "is_root": true,
            "topological_rank": 3,
        });
        let fields = extract_type_schema_fields(&[&node]);
        assert_eq!(fields.len(), 6, "count + at most 5 fields");
    }

    #[test]
    fn dlt_branch_wins_over_the_type_graph() {
        let nodes = vec![
            json!({
                "id": "tbl",
                "type": "SchemaTable",
                "name": "orders",
                "description": "order rows",
                "columns": [{ "name": "id", "type": "bigint", "primary_key": true }],
            }),
            json!({ "id": "rel", "type": "SchemaRelationship", "relationship_type": "fk" }),
            node("e1", "Entity", "Alice"),
        ];
        let links = vec![
            link("tbl", "rel", "has_relationship"),
            // Duplicate triple is dropped.
            link("tbl", "rel", "has_relationship"),
            // Endpoint outside the schema node set is dropped.
            link("tbl", "e1", "contains"),
        ];
        let graph = extract_schema_graph_data(&nodes, &links);

        assert!(graph.get("instances_by_type").is_none());
        assert!(graph.get("instance_index").is_none());
        let graph_nodes = graph["nodes"].as_array().expect("nodes is an array");
        assert_eq!(graph_nodes.len(), 2);
        assert_eq!(graph_nodes[0]["name"], "orders");
        assert_eq!(graph_nodes[0]["fields"][0]["name"], "id");
        // The nameless relationship node falls back to its id.
        assert_eq!(graph_nodes[1]["name"], "rel");
        assert_eq!(
            graph["links"],
            json!([
                { "source": "tbl", "target": "rel", "label": "has_relationship" }
            ])
        );
    }

    #[test]
    fn build_operation_layer_always_sets_both_keys() {
        let mut graph = json!({ "nodes": [], "links": [] });
        build_operation_layer(&mut graph, &[], &[]);
        assert_eq!(graph["operations"], json!([]));
        assert_eq!(graph["operation_links"], json!([]));
    }

    #[test]
    fn build_operation_layer_flags_observed_links() {
        let nodes = vec![json!({
            "id": "d1",
            "type": "TextDocument",
            "name": "a.txt",
            "source_pipeline": "cognify_pipeline",
        })];
        let mut graph = extract_schema_graph_data(&nodes, &[]);
        build_operation_layer(&mut graph, &nodes, &[]);

        let observed: Vec<&Value> = graph["operation_links"]
            .as_array()
            .expect("operation_links is an array")
            .iter()
            .filter(|entry| entry["source"] == "op:cognify")
            .collect();
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0]["target"], "type:TextDocument");
        assert_eq!(observed[0]["observed"], true);
        assert_eq!(observed[0]["property"], Value::Null);

        // `forget` has no pipeline, so its links can never be observed.
        let forget: Vec<&Value> = graph["operation_links"]
            .as_array()
            .expect("operation_links is an array")
            .iter()
            .filter(|entry| entry["source"] == "op:forget")
            .collect();
        assert_eq!(forget.len(), 1);
        assert_eq!(forget[0]["observed"], false);
    }
}
