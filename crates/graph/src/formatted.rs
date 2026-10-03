//! Formatted-graph-data helper — port of Python
//! `cognee.modules.graph.methods.get_formatted_graph_data`.
//!
//! Reads all nodes and edges from a graph DB and reshapes them into the
//! canonical wire payload:
//!
//! ```json
//! {
//!   "nodes": [{"id": "...", "label": "...", "type": "...", "properties": {...}}],
//!   "edges": [{"source": "...", "target": "...", "label": "..."}]
//! }
//! ```
//!
//! Field ordering matches the Python helper exactly — the WS frame is part
//! of the cross-SDK wire contract, see [`docs/http-server/websocket.md`].

use uuid::Uuid;

use crate::{GraphDBResult, GraphDBTrait};

/// Fetch the formatted graph snapshot for a dataset.
///
/// # Python parity
///
/// The Rust port intentionally omits Python's `set_database_global_context_variables`
/// branch — the Rust `GraphDBTrait` instance is already scoped to the caller's
/// owner/tenant by construction (per `tenants.md §3`). The `dataset_id` and
/// `user_id` parameters are accepted for API parity with the Python helper but
/// only `dataset_id` is used here to select graph rows whose stored
/// `belongs_to_set` membership includes that dataset.
///
/// # Shape
///
/// Each node produces:
/// - `id`     — string form of the node id
/// - `label`  — `properties["name"]` if non-empty, else `"{type}_{id}"`
/// - `type`   — `properties["type"]`
/// - `properties` — every other property whose value is not null, with
///   `id`, `type`, `name`, `created_at`, `updated_at` excluded.
///
/// Each edge produces:
/// - `source`, `target`, `label` (the relationship name).
pub async fn get_formatted_graph_data(
    graph_db: &dyn GraphDBTrait,
    dataset_id: Uuid,
    user_id: Uuid,
) -> GraphDBResult<serde_json::Value> {
    let (nodes, edges) = graph_db.get_graph_data().await?;

    let _ = user_id;

    let dataset_id = dataset_id.to_string();
    let filtered_nodes: Vec<_> = nodes
        .into_iter()
        .filter(|(_, node)| node_belongs_to_dataset(node, &dataset_id))
        .collect();

    let node_ids: std::collections::HashSet<String> = filtered_nodes
        .iter()
        .map(|(node_id, _)| node_id.clone())
        .collect();

    let node_values: Vec<serde_json::Value> = filtered_nodes
        .into_iter()
        .map(|(node_id, props)| format_node(&node_id, &props))
        .collect();

    let edge_values: Vec<serde_json::Value> = edges
        .into_iter()
        .filter(|(source, target, _, props)| {
            node_ids.contains(source)
                && node_ids.contains(target)
                && edge_belongs_to_dataset(props, &dataset_id)
        })
        .map(|(source, target, relationship_name, _props)| {
            serde_json::json!({
                "source": source,
                "target": target,
                "label": relationship_name,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "nodes": node_values,
        "edges": edge_values,
    }))
}

/// Build the per-node object matching Python's mapping shape.
///
/// `label = properties["name"]` if non-empty, else `"{type}_{id}"`.
/// `properties` excludes id, type, name, created_at, updated_at and drops
/// any value that is `null`.
fn format_node(node_id: &str, props: &crate::NodeData) -> serde_json::Value {
    let type_str = props
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let name = props.get("name").and_then(|v| v.as_str()).unwrap_or("");

    let label = if !name.is_empty() {
        name.to_string()
    } else {
        format!("{type_str}_{node_id}")
    };

    let mut properties_map = serde_json::Map::new();
    for (key, value) in props.iter() {
        let k = key.as_ref();
        if matches!(k, "id" | "type" | "name" | "created_at" | "updated_at") {
            continue;
        }
        if value.is_null() {
            continue;
        }
        properties_map.insert(k.to_string(), value.clone());
    }

    serde_json::json!({
        "id": node_id,
        "label": label,
        "type": type_str,
        "properties": serde_json::Value::Object(properties_map),
    })
}

fn node_belongs_to_dataset(node: &crate::NodeData, dataset_id: &str) -> bool {
    let Some(serde_json::Value::Array(entries)) = node.get("belongs_to_set") else {
        return false;
    };

    entries
        .iter()
        .any(|entry| entry.as_str().is_some_and(|value| value == dataset_id))
}

fn edge_belongs_to_dataset(
    props: &std::collections::HashMap<std::borrow::Cow<'static, str>, serde_json::Value>,
    dataset_id: &str,
) -> bool {
    let Some(serde_json::Value::Array(entries)) = props.get("belongs_to_set") else {
        return false;
    };

    entries
        .iter()
        .any(|entry| entry.as_str().is_some_and(|value| value == dataset_id))
}
