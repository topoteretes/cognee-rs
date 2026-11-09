//! The JSON payloads of `GET /visualize/json` and `GET /visualize/brains`.
//!
//! Port of `build_visualization_payload` and `build_brain_summary_payload`
//! in Python's `cognee_network_visualization.py`. Both run the same
//! [`preprocess`] the HTML path renders from, so the two cannot drift on the
//! data — only on how each packages it.

use cognee_graph::{EdgeData, GraphNode};
use serde_json::{Map, Value};

use crate::error::VisualizationError;
use crate::preprocessor::preprocess;

/// The keys of [`build_visualization_payload`], in order: every field of
/// `PreprocessedGraph` (Python `asdict(pre)`) plus `search_events`.
pub const VISUALIZATION_PAYLOAD_KEYS: [&str; 12] = [
    "nodes",
    "links",
    "color_maps",
    "schema_graph",
    "schema_data",
    "pipeline_stages",
    "edge_classes",
    "bundles",
    "provenance_index",
    "has_meaningful_topological_rank",
    "memory_map",
    "search_events",
];

/// JSON snapshot of the graph, for a client that renders it itself (Python
/// `build_visualization_payload`).
///
/// Every `PreprocessedGraph` field — not only the ones the HTML template turns
/// into tokens — then `search_events` (`[]` when none), keyed exactly as
/// [`VISUALIZATION_PAYLOAD_KEYS`]. `schema_data` is `null` when `None`.
/// Semantic positions/clusters are deliberately absent, as in Python (a
/// separate, heavier call there; not computed in Rust at all).
pub fn build_visualization_payload(
    nodes: Vec<GraphNode>,
    edges: Vec<EdgeData>,
    schema_data: Option<Value>,
    search_events: Vec<Value>,
) -> Result<Value, VisualizationError> {
    let pre = preprocess(nodes, edges, schema_data);
    let mut payload = match serde_json::to_value(&pre)? {
        Value::Object(map) => map,
        // A struct always serializes to an object.
        _ => Map::new(),
    };
    payload.insert("search_events".to_string(), Value::Array(search_events));
    Ok(Value::Object(payload))
}

/// One dataset's graph in the shape a multi-dataset overview needs (Python
/// `build_brain_summary_payload`): `{"name", "nodes", "links",
/// "node_set_colors"}`, the per-dataset shape a static Business-view export
/// embeds under `brainsData`.
pub fn build_brain_summary_payload(
    dataset_name: &str,
    nodes: Vec<GraphNode>,
    edges: Vec<EdgeData>,
) -> Result<Value, VisualizationError> {
    let pre = preprocess(nodes, edges, None);
    let mut payload = Map::new();
    payload.insert("name".to_string(), Value::String(dataset_name.to_string()));
    payload.insert("nodes".to_string(), Value::Array(pre.nodes));
    payload.insert("links".to_string(), Value::Array(pre.links));
    payload.insert(
        "node_set_colors".to_string(),
        serde_json::to_value(pre.color_maps.node_set)?,
    );
    Ok(Value::Object(payload))
}
