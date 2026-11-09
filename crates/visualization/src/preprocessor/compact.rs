//! The compact, chunk-by-chunk payload of the streamed graph read (SDK-787).
//!
//! Port of the "Streamed payload" section of Python's `preprocessor.py`
//! (`COMPACT_PROPERTY_KEYS`, `compact_node`, `compact_link`, `compact_chunk`,
//! `CompactGraphAccumulator`).
//!
//! A streamed read must never hold the whole graph with its properties, so
//! everything a chunk carries is computed from that chunk alone, with the same
//! helpers [`preprocess`](super::preprocess) uses, so the two cannot drift.
//! The one input from outside the chunk is each Entity's semantic type
//! ([`SEMANTIC_TYPE_KEY`], set by
//! [`crate::subgraph::apply_entity_types`]). When that is missing, a chunk
//! falls back to its own `is_a` links, and an entity whose `is_a` link
//! arrives in a later chunk is sent as `"Entity"` and corrected by an
//! `entity_type` in its summary entry. The graph-wide fields (importance,
//! label priority, the node-set colors) come at the end from the
//! [`CompactGraphAccumulator`].

use std::collections::{HashMap, HashSet};

use cognee_graph::{EdgeData, GraphNode, NodeData};
use serde_json::{Map, Value};

use super::naming::{NAME_FALLBACK_KEYS, derive_node_name, looks_like_identifier};
use super::{
    ALWAYS_LABEL_STAGES, ENTITY_TYPE_RELATION, LABEL_PRIORITY_PERCENTILE, SEMANTIC_TYPE_KEY,
    edge_class, is_truthy, label_priority_threshold, props_to_object, stage_for_node,
};
use crate::colors::build_node_set_colors;

/// Properties the store is asked for when streaming (Python
/// `COMPACT_PROPERTY_KEYS`): every key the compact fields are derived from,
/// including the name fallbacks — dropping `text` here would name every chunk
/// "Unnamed DocumentChunk". `id`, `name` and `type` are always kept by the
/// projection itself.
pub const COMPACT_PROPERTY_KEYS: [&str; 8] = [
    "name",
    NAME_FALLBACK_KEYS[0],
    NAME_FALLBACK_KEYS[1],
    NAME_FALLBACK_KEYS[2],
    NAME_FALLBACK_KEYS[3],
    NAME_FALLBACK_KEYS[4],
    "belongs_to_set",
    "source_node_set",
];

/// [`COMPACT_PROPERTY_KEYS`] as owned strings, the shape
/// `GraphDBTrait::get_bounded_neighborhood` takes.
pub fn compact_property_keys() -> Vec<String> {
    COMPACT_PROPERTY_KEYS
        .iter()
        .map(|key| (*key).to_string())
        .collect()
}

/// The per-node fields the Mindmap reads, computed as `preprocess()` does
/// (Python `compact_node`).
///
/// Keys, in order: `id`, `name`, `type`, `entity_type`, `stage`,
/// `is_unnamed`, then `belongs_to_set` / `source_node_set` when present and
/// not null.
pub fn compact_node(node_id: &str, node_info: &NodeData) -> Value {
    compact_node_from_map(node_id, &props_to_object(node_info.clone()))
}

fn compact_node_from_map(node_id: &str, info: &Map<String, Value>) -> Value {
    let name = derive_node_name(info, node_id);
    let is_unnamed =
        info.get("name").is_some_and(looks_like_identifier) || name.starts_with("Unnamed ");
    let node_type = info.get("type").cloned().unwrap_or(Value::Null);
    let semantic = info.get(SEMANTIC_TYPE_KEY).filter(|value| is_truthy(value));
    let entity_type = match semantic {
        Some(semantic) if node_type.as_str() == Some("Entity") => semantic.clone(),
        _ if is_truthy(&node_type) => node_type.clone(),
        _ => Value::String("Node".to_string()),
    };

    let mut node = Map::new();
    node.insert("id".to_string(), Value::String(node_id.to_string()));
    node.insert("name".to_string(), Value::String(name));
    node.insert("type".to_string(), node_type);
    node.insert("entity_type".to_string(), entity_type);
    node.insert(
        "stage".to_string(),
        Value::String(stage_for_node(info).to_string()),
    );
    node.insert("is_unnamed".to_string(), Value::Bool(is_unnamed));
    for key in ["belongs_to_set", "source_node_set"] {
        if let Some(value) = info.get(key).filter(|value| !value.is_null()) {
            node.insert(key.to_string(), value.clone());
        }
    }
    Value::Object(node)
}

/// The per-link fields the Mindmap reads, from an adapter edge tuple (Python
/// `compact_link`): `source`, `target`, `relation`, `edge_class`.
pub fn compact_link(edge: &EdgeData) -> Value {
    let (source, target, relation, props) = edge;
    let edge_info = props_to_object(props.clone());
    let mut link = Map::new();
    link.insert("source".to_string(), Value::String(source.clone()));
    link.insert("target".to_string(), Value::String(target.clone()));
    link.insert("relation".to_string(), Value::String(relation.clone()));
    link.insert(
        "edge_class".to_string(),
        Value::String(edge_class(relation, &edge_info).to_string()),
    );
    Value::Object(link)
}

fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// A chunk's compact nodes and links, typed as `preprocess()` would type them
/// (Python `compact_chunk`).
///
/// An Entity the store lookup did not type falls back to an `is_a` link in
/// this chunk, the same fallback `resolve_semantic_types` applies.
pub fn compact_chunk(nodes: &[GraphNode], edges: &[EdgeData]) -> (Vec<Value>, Vec<Value>) {
    let mut compact_nodes: Vec<Value> = nodes
        .iter()
        .map(|(node_id, info)| compact_node(node_id, info))
        .collect();
    let links: Vec<Value> = edges.iter().map(compact_link).collect();

    let names: HashMap<String, String> = compact_nodes
        .iter()
        .filter(|node| str_field(node, "type") == Some("EntityType"))
        .filter_map(|node| {
            Some((
                str_field(node, "id")?.to_string(),
                str_field(node, "name")?.to_string(),
            ))
        })
        .collect();
    // A dict comprehension in Python: a later link for the same source wins.
    let mut linked_types: HashMap<String, String> = HashMap::new();
    for link in &links {
        if str_field(link, "relation") != Some(ENTITY_TYPE_RELATION) {
            continue;
        }
        let (Some(source), Some(target)) = (str_field(link, "source"), str_field(link, "target"))
        else {
            continue;
        };
        if let Some(name) = names.get(target).filter(|name| !name.is_empty()) {
            linked_types.insert(source.to_string(), name.clone());
        }
    }
    for node in &mut compact_nodes {
        if str_field(node, "entity_type") != Some("Entity") {
            continue;
        }
        let resolved = str_field(node, "id").and_then(|id| linked_types.get(id).cloned());
        if let (Some(resolved), Some(object)) = (resolved, node.as_object_mut()) {
            object.insert("entity_type".to_string(), Value::String(resolved));
        }
    }
    (compact_nodes, links)
}

/// The per-node fields the summary needs.
#[derive(Debug, Clone)]
struct AccumulatedNode {
    stage: String,
    is_unnamed: bool,
}

/// What the graph-wide fields need, collected chunk by chunk (Python
/// `CompactGraphAccumulator`).
///
/// Degree counts, and per node its stage, placeholder flag and node-set value:
/// a few fields per node, never the properties. [`summary`](Self::summary)
/// returns the `importance` and `label_priority` `preprocess()` would assign,
/// and the `node_set` color map it would build, for the whole streamed graph.
/// A node a chunk sent as `"Entity"` whose `is_a` link and type node only
/// arrived later also gets the `entity_type` `preprocess()` would resolve.
#[derive(Debug, Clone, Default)]
pub struct CompactGraphAccumulator {
    /// Distinct node ids in first-seen order (Python dict insertion order).
    order: Vec<String>,
    nodes: HashMap<String, AccumulatedNode>,
    node_sets: Vec<Option<String>>,
    degree: HashMap<String, usize>,
    link_count: usize,
    untyped: HashSet<String>,
    type_names: HashMap<String, String>,
    is_a: HashMap<String, String>,
}

impl CompactGraphAccumulator {
    /// An empty accumulator.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold in one chunk's compact nodes and links (as [`compact_chunk`]
    /// returns them).
    pub fn add(&mut self, nodes: &[Value], links: &[Value]) {
        for node in nodes {
            let Some(id) = str_field(node, "id") else {
                continue;
            };
            let entry = AccumulatedNode {
                stage: str_field(node, "stage").unwrap_or("other").to_string(),
                is_unnamed: node
                    .get("is_unnamed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            };
            // Python reassigns the dict key: the value updates, the position
            // stays where the id was first seen.
            if self.nodes.insert(id.to_string(), entry).is_none() {
                self.order.push(id.to_string());
            }
            self.node_sets
                .push(str_field(node, "source_node_set").map(str::to_string));
            let node_type = str_field(node, "type");
            if node_type == Some("Entity") && str_field(node, "entity_type") == Some("Entity") {
                self.untyped.insert(id.to_string());
            } else if node_type == Some("EntityType")
                && let Some(name) = str_field(node, "name").filter(|name| !name.is_empty())
            {
                self.type_names.insert(id.to_string(), name.to_string());
            }
        }
        for link in links {
            let (Some(source), Some(target)) =
                (str_field(link, "source"), str_field(link, "target"))
            else {
                continue;
            };
            *self.degree.entry(source.to_string()).or_insert(0) += 1;
            *self.degree.entry(target.to_string()).or_insert(0) += 1;
            if str_field(link, "relation") == Some(ENTITY_TYPE_RELATION) {
                self.is_a.insert(source.to_string(), target.to_string());
            }
        }
        self.link_count += links.len();
    }

    /// Distinct nodes added so far (the `done` event's `nodes`).
    pub fn node_count(&self) -> usize {
        self.order.len()
    }

    /// Links added so far (the `done` event's `links`).
    pub fn link_count(&self) -> usize {
        self.link_count
    }

    /// Types for entities whose `is_a` link came in a later chunk than they did.
    fn late_entity_types(&self) -> HashMap<&str, &str> {
        self.untyped
            .iter()
            .filter_map(|id| {
                let target = self.is_a.get(id)?;
                let name = self.type_names.get(target)?;
                Some((id.as_str(), name.as_str()))
            })
            .collect()
    }

    /// `{"nodes": {id: {"importance", "label_priority"[, "entity_type"]}},
    /// "color_maps": {"node_set": {...}}}`, nodes in first-seen order.
    pub fn summary(&self) -> Value {
        // Same arithmetic as `preprocess` so the values are bit-identical.
        let max_degree = self.degree.values().copied().max().unwrap_or(1).max(1);
        let scale = (max_degree as f64).ln_1p();
        let importance: Vec<f64> = self
            .order
            .iter()
            .map(|id| (self.degree.get(id).copied().unwrap_or(0) as f64).ln_1p() / scale)
            .collect();
        let threshold = label_priority_threshold(&importance, LABEL_PRIORITY_PERCENTILE);
        let late = self.late_entity_types();

        let mut nodes = Map::new();
        for (id, importance) in self.order.iter().zip(importance) {
            let Some(node) = self.nodes.get(id) else {
                continue;
            };
            let label_priority = !node.is_unnamed
                && (ALWAYS_LABEL_STAGES.contains(&node.stage.as_str())
                    || (importance >= threshold && threshold > 0.0));
            let mut entry = Map::new();
            entry.insert("importance".to_string(), Value::from(importance));
            entry.insert("label_priority".to_string(), Value::Bool(label_priority));
            // Only present where it corrects what the chunk sent.
            if let Some(entity_type) = late.get(id.as_str()) {
                entry.insert(
                    "entity_type".to_string(),
                    Value::String((*entity_type).to_string()),
                );
            }
            nodes.insert(id.clone(), Value::Object(entry));
        }

        let node_set = build_node_set_colors(self.node_sets.iter().cloned());
        let mut color_maps = Map::new();
        color_maps.insert(
            "node_set".to_string(),
            serde_json::to_value(node_set).unwrap_or_else(|_| Value::Object(Map::new())),
        );
        let mut summary = Map::new();
        summary.insert("nodes".to_string(), Value::Object(nodes));
        summary.insert("color_maps".to_string(), Value::Object(color_maps));
        Value::Object(summary)
    }
}
