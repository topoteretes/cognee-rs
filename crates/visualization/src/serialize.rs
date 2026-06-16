//! Serialize graph nodes/edges into the JSON shape expected by the embedded
//! HTML template.
//!
//! Mirrors the Python `cognee_network_visualization()` function
//! (`cognee_network_visualization.py:22–112`).

use std::collections::BTreeMap;

use cognee_graph::{EdgeData, GraphNode};
use serde_json::{Map, Value};

use crate::colors::{provenance_colors, type_color};

/// Intermediate, already-JSON-serialized form of the graph, plus the four
/// provenance color maps. Consumed by `html::build_html`.
pub(crate) struct Serialized {
    pub nodes: Vec<Value>,
    pub links: Vec<Value>,
    pub task_colors: BTreeMap<String, String>,
    pub pipeline_colors: BTreeMap<String, String>,
    pub nodeset_colors: BTreeMap<String, String>,
    pub user_colors: BTreeMap<String, String>,
}

/// Convert the raw `(nodes, edges)` returned by `GraphDBTrait::get_graph_data()`
/// into the JSON shape consumed by the HTML template.
///
/// Nodes are cloned into `serde_json::Map`s with the following additions:
///   * `id` is overwritten with the stringified node id
///   * `color` is derived from `type_color()` (or the ontology-valid override)
///   * `name` defaults to `id` if absent
///   * `created_at` / `updated_at` fields are stripped
///
/// Edges become `{source, target, relation, weight, all_weights,
/// relationship_type, edge_info}`.  Weights are flattened from three sources
/// that may all appear on the same edge:
///   1. A scalar `weight` field (becomes `all_weights["default"]`)
///   2. A nested `weights` object (merged into `all_weights`)
///   3. Any `weight_<key>` field (stored as `all_weights[<key>]`)
pub(crate) fn serialize_graph(nodes: Vec<GraphNode>, edges: Vec<EdgeData>) -> Serialized {
    let mut nodes_list: Vec<Value> = Vec::with_capacity(nodes.len());

    for (node_id, node_info) in nodes {
        let mut map = Map::new();
        for (k, v) in node_info.into_iter() {
            map.insert(k.into_owned(), v);
        }

        // Overwrite `id` with the canonical node id string from the tuple.
        map.insert("id".to_string(), Value::String(node_id.clone()));

        let node_type = map
            .get("type")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let ontology_valid = map
            .get("ontology_valid")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let color = type_color(node_type.as_deref(), ontology_valid);
        map.insert("color".to_string(), Value::String(color.to_string()));

        // Ensure `name` is present — derive from schema-node fields first, then
        // fall back to the node id.
        //
        // Ports Python's `preprocessor.py:223–237` 8-key fallback for schema-typed
        // nodes (`DatabaseSchema`, `SchemaTable`, `SchemaRelationship`, etc.).  The
        // keys are checked in priority order; the first one that yields a non-empty
        // string wins.
        if !map
            .get("name")
            .is_some_and(|v| matches!(v, Value::String(s) if !s.is_empty()))
        {
            const SCHEMA_FALLBACK_KEYS: &[&str] = &[
                "database_type",
                "primary_key",
                "source_table",
                "source_column",
                "target_table",
                "target_column",
                "relationship_type",
                "row_count_estimate",
            ];
            let derived = SCHEMA_FALLBACK_KEYS
                .iter()
                .find_map(|k| {
                    map.get(*k).and_then(|v| match v {
                        Value::String(s) if !s.is_empty() => Some(s.clone()),
                        Value::Number(n) => Some(n.to_string()),
                        _ => None,
                    })
                })
                .unwrap_or_else(|| node_id.clone());
            map.insert("name".to_string(), Value::String(derived));
        }

        map.remove("created_at");
        map.remove("updated_at");

        nodes_list.push(Value::Object(map));
    }

    let task_colors = provenance_colors(nodes_list.iter().map(|n| extract_str(n, "source_task")));
    let pipeline_colors =
        provenance_colors(nodes_list.iter().map(|n| extract_str(n, "source_pipeline")));
    let nodeset_colors =
        provenance_colors(nodes_list.iter().map(|n| extract_str(n, "source_node_set")));
    let user_colors = provenance_colors(nodes_list.iter().map(|n| extract_str(n, "source_user")));

    let mut links_list: Vec<Value> = Vec::with_capacity(edges.len());

    for (source, target, relation, edge_info_map) in edges {
        // Build a JSON object mirror of edge_info for embedding.
        let mut edge_info = Map::new();
        for (k, v) in edge_info_map.into_iter() {
            edge_info.insert(k.into_owned(), v);
        }

        let mut all_weights: Map<String, Value> = Map::new();
        let mut primary_weight: Option<Value> = None;

        if let Some(weight_val) = edge_info.get("weight").cloned() {
            all_weights.insert("default".to_string(), weight_val.clone());
            primary_weight = Some(weight_val);
        }

        if let Some(Value::Object(weights_map)) = edge_info.get("weights").cloned() {
            if primary_weight.is_none()
                && let Some((_, first)) = weights_map.iter().next()
            {
                primary_weight = Some(first.clone());
            }
            for (k, v) in weights_map.into_iter() {
                all_weights.insert(k, v);
            }
        }

        for (k, v) in edge_info.iter() {
            if let Some(suffix) = k.strip_prefix("weight_")
                && v.is_number()
            {
                all_weights.insert(suffix.to_string(), v.clone());
            }
        }

        let relationship_type = edge_info
            .get("relationship_type")
            .cloned()
            .unwrap_or(Value::Null);

        let mut link = Map::new();
        link.insert("source".to_string(), Value::String(source));
        link.insert("target".to_string(), Value::String(target));
        link.insert("relation".to_string(), Value::String(relation));
        link.insert("weight".to_string(), primary_weight.unwrap_or(Value::Null));
        link.insert("all_weights".to_string(), Value::Object(all_weights));
        link.insert("relationship_type".to_string(), relationship_type);
        link.insert("edge_info".to_string(), Value::Object(edge_info));

        links_list.push(Value::Object(link));
    }

    Serialized {
        nodes: nodes_list,
        links: links_list,
        task_colors,
        pipeline_colors,
        nodeset_colors,
        user_colors,
    }
}

/// Extract a string-valued field from a `serde_json::Value` object.
/// Returns `None` for non-string values, missing keys, or non-object inputs.
fn extract_str(node: &Value, key: &str) -> Option<String> {
    node.as_object()
        .and_then(|m| m.get(key))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;
    use std::collections::HashMap;

    fn node(id: &str, props: &[(&str, Value)]) -> GraphNode {
        let mut map: HashMap<Cow<'static, str>, Value> = HashMap::new();
        for (k, v) in props {
            map.insert(Cow::Owned((*k).to_string()), v.clone());
        }
        (id.to_string(), map)
    }

    fn edge(src: &str, tgt: &str, rel: &str, props: &[(&str, Value)]) -> EdgeData {
        let mut map: HashMap<Cow<'static, str>, Value> = HashMap::new();
        for (k, v) in props {
            map.insert(Cow::Owned((*k).to_string()), v.clone());
        }
        (src.to_string(), tgt.to_string(), rel.to_string(), map)
    }

    #[test]
    fn serialize_overwrites_id_and_assigns_color() {
        let n = node(
            "n1",
            &[
                ("type", Value::String("Entity".to_string())),
                ("id", Value::String("WRONG".to_string())),
                ("name", Value::String("Alice".to_string())),
            ],
        );
        let out = serialize_graph(vec![n], vec![]);
        let node_obj = out.nodes[0].as_object().expect("node is an object");
        assert_eq!(node_obj.get("id").and_then(Value::as_str), Some("n1"));
        assert_eq!(
            node_obj.get("color").and_then(Value::as_str),
            Some("#6510F4")
        );
        assert_eq!(node_obj.get("name").and_then(Value::as_str), Some("Alice"));
    }

    #[test]
    fn serialize_ontology_valid_override() {
        let n = node(
            "n2",
            &[
                ("type", Value::String("Entity".to_string())),
                ("ontology_valid", Value::Bool(true)),
            ],
        );
        let out = serialize_graph(vec![n], vec![]);
        let node_obj = out.nodes[0].as_object().expect("node is an object");
        assert_eq!(
            node_obj.get("color").and_then(Value::as_str),
            Some("#D8D8D8")
        );
    }

    #[test]
    fn serialize_strips_timestamps_and_defaults_name() {
        let n = node(
            "n3",
            &[
                ("created_at", Value::String("2024".to_string())),
                ("updated_at", Value::String("2024".to_string())),
            ],
        );
        let out = serialize_graph(vec![n], vec![]);
        let node_obj = out.nodes[0].as_object().expect("node is an object");
        assert!(!node_obj.contains_key("created_at"));
        assert!(!node_obj.contains_key("updated_at"));
        // fallback to id for name
        assert_eq!(node_obj.get("name").and_then(Value::as_str), Some("n3"));
    }

    #[test]
    fn serialize_flattens_edge_weights() {
        let e = edge(
            "a",
            "b",
            "knows",
            &[
                ("weight", Value::from(0.5)),
                (
                    "weights",
                    serde_json::json!({"semantic": 0.8, "lexical": 0.3}),
                ),
                ("weight_trust", Value::from(0.9)),
                ("relationship_type", Value::String("KNOWS".to_string())),
            ],
        );
        let out = serialize_graph(vec![], vec![e]);
        let link = out.links[0].as_object().expect("link is an object");
        assert_eq!(link.get("source").and_then(Value::as_str), Some("a"));
        assert_eq!(link.get("target").and_then(Value::as_str), Some("b"));
        assert_eq!(link.get("relation").and_then(Value::as_str), Some("knows"));
        assert_eq!(link.get("weight").and_then(Value::as_f64), Some(0.5));
        let all = link
            .get("all_weights")
            .and_then(Value::as_object)
            .expect("all_weights is an object");
        assert_eq!(all.get("default").and_then(Value::as_f64), Some(0.5));
        assert_eq!(all.get("semantic").and_then(Value::as_f64), Some(0.8));
        assert_eq!(all.get("lexical").and_then(Value::as_f64), Some(0.3));
        assert_eq!(all.get("trust").and_then(Value::as_f64), Some(0.9));
        assert_eq!(
            link.get("relationship_type").and_then(Value::as_str),
            Some("KNOWS")
        );
    }

    #[test]
    fn serialize_derives_provenance_color_maps() {
        let n1 = node(
            "n1",
            &[
                ("type", Value::String("Entity".to_string())),
                ("source_task", Value::String("ingest".to_string())),
                ("source_user", Value::String("alice".to_string())),
            ],
        );
        let n2 = node(
            "n2",
            &[
                ("type", Value::String("Entity".to_string())),
                ("source_task", Value::String("cognify".to_string())),
                ("source_user", Value::String("alice".to_string())),
            ],
        );
        let out = serialize_graph(vec![n1, n2], vec![]);
        assert_eq!(out.task_colors.len(), 2);
        assert_eq!(out.user_colors.len(), 1);
        assert!(out.pipeline_colors.is_empty());
        assert!(out.nodeset_colors.is_empty());
    }
}
