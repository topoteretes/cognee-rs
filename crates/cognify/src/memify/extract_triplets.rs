use cognee_graph::{EdgeData, GraphDBTrait, GraphNode, NodeData};
use cognee_models::Triplet;
use std::borrow::Cow;
use std::collections::HashMap;
use tracing::{info, warn};
use uuid::Uuid;

use super::config::MemifyConfig;
use super::error::MemifyError;

/// Extract triplets from an existing graph database.
///
/// Reads all nodes and edges (or a filtered subgraph) via GraphDBTrait
/// and constructs Triplet objects with embeddable text.
///
/// Rust equivalent of Python's get_triplet_datapoints()
/// (cognee/tasks/memify/get_triplet_datapoints.py:169).
pub async fn extract_triplets_from_graph_db(
    graph_db: &dyn GraphDBTrait,
    config: &MemifyConfig,
) -> Result<Vec<Triplet>, MemifyError> {
    // Step 1: Read graph data (full or filtered)
    let (nodes, edges) = read_graph_data(graph_db, config).await?;

    info!(
        node_count = nodes.len(),
        edge_count = edges.len(),
        "Read graph data for triplet extraction"
    );

    if edges.is_empty() {
        return Ok(Vec::new());
    }

    // Step 2: Build node lookup: node_id -> NodeData
    let node_map: HashMap<&str, &NodeData> =
        nodes.iter().map(|(id, data)| (id.as_str(), data)).collect();

    // Step 3: Build triplets from edges
    let mut triplets = Vec::new();
    let mut skipped = 0usize;

    for (source_id, target_id, relationship_name, edge_props) in &edges {
        let source = match node_map.get(source_id.as_str()) {
            Some(data) => *data,
            None => {
                skipped += 1;
                continue;
            }
        };
        let target = match node_map.get(target_id.as_str()) {
            Some(data) => *data,
            None => {
                skipped += 1;
                continue;
            }
        };

        let source_text = build_node_text(source);
        let target_text = build_node_text(target);
        let relationship_text = extract_relationship_text(edge_props, relationship_name);

        if source_text.is_empty() && relationship_text.is_empty() && target_text.is_empty() {
            skipped += 1;
            continue;
        }

        // Format matches existing Rust create_triplets_from_graph():
        // "{source_text} -\u{203a} {relationship_text}-\u{203a}{target_text}"
        let text = format!("{source_text} -\u{203a} {relationship_text}-\u{203a}{target_text}");

        let source_uuid = parse_node_uuid(source_id)?;
        let target_uuid = parse_node_uuid(target_id)?;

        let triplet = Triplet::new(source_uuid, target_uuid, relationship_name.clone(), text)
            .with_names(
                extract_string_prop(source, "name"),
                extract_string_prop(target, "name"),
            );

        triplets.push(triplet);
    }

    if skipped > 0 {
        warn!(skipped, "Skipped edges (missing nodes or empty text)");
    }

    Ok(triplets)
}

/// Read graph data, applying filters from config if present.
async fn read_graph_data(
    graph_db: &dyn GraphDBTrait,
    config: &MemifyConfig,
) -> Result<(Vec<GraphNode>, Vec<EdgeData>), MemifyError> {
    match (&config.node_type_filter, &config.node_name_filter) {
        (Some(node_type), Some(node_names)) => graph_db
            .get_nodeset_subgraph(node_type, node_names, &config.node_name_filter_operator)
            .await
            .map_err(|e| MemifyError::GraphDBError(e.to_string())),
        _ => graph_db
            .get_graph_data()
            .await
            .map_err(|e| MemifyError::GraphDBError(e.to_string())),
    }
}

/// Build embeddable text from a graph node's properties.
///
/// Uses "name" and "description" fields, matching existing
/// create_triplets_from_graph() in triplet_creation.rs.
///
/// Format: "Name: Description" or just "Name" if description is empty.
fn build_node_text(node: &NodeData) -> String {
    let name = extract_string_prop(node, "name");
    let description = extract_string_prop(node, "description");

    if !description.is_empty() {
        format!("{name}: {description}")
    } else {
        name
    }
    .trim()
    .to_string()
}

/// Extract relationship text from edge properties.
///
/// Tries "edge_text" property first (matching Python's
/// _extract_relationship_text), falls back to relationship_name.
fn extract_relationship_text(
    edge_props: &HashMap<Cow<'static, str>, serde_json::Value>,
    relationship_name: &str,
) -> String {
    edge_props
        .get("edge_text")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or(relationship_name)
        .to_string()
}

/// Extract a string property from NodeData.
fn extract_string_prop(data: &NodeData, key: &str) -> String {
    data.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Parse a node ID string as UUID.
fn parse_node_uuid(id: &str) -> Result<Uuid, MemifyError> {
    Uuid::parse_str(id)
        .map_err(|e| MemifyError::GraphDBError(format!("Invalid node UUID '{id}': {e}")))
}
