//! Extract triplets from graph database for the memify pipeline.
//!
//! Reads nodes and edges from the graph DB (optionally filtered by node type/name),
//! and produces `Triplet` values with embeddable text matching the format used by
//! `create_triplets_from_graph()` in `triplet_creation.rs`.

use std::collections::HashMap;

use cognee_graph::{GraphDBTrait, NodeData};
use cognee_models::Triplet;
use tracing::{info, warn};
use uuid::Uuid;

use super::config::MemifyConfig;
use super::error::MemifyError;

/// Extract triplets from the graph database.
///
/// Reads the graph (all data or a filtered subgraph depending on config) and
/// converts each edge into a [`Triplet`] with embeddable text in the format:
/// `"source_text -› relationship_text-›target_text"`
///
/// # Arguments
/// * `graph_db` - Graph database to read from
/// * `config` - Memify configuration controlling optional node filters
///
/// # Errors
/// Returns `MemifyError::GraphDBError` if graph reads or UUID parsing fails.
pub async fn extract_triplets_from_graph_db(
    graph_db: &dyn GraphDBTrait,
    config: &MemifyConfig,
) -> Result<Vec<Triplet>, MemifyError> {
    // Step 1: Read graph data (filtered or full)
    let (nodes, edges) = match (&config.node_type_filter, &config.node_name_filter) {
        (Some(node_type), Some(node_names)) => graph_db
            .get_nodeset_subgraph(node_type, node_names, &config.node_name_filter_operator)
            .await
            .map_err(|e| MemifyError::GraphDBError(e.to_string()))?,
        _ => graph_db
            .get_graph_data()
            .await
            .map_err(|e| MemifyError::GraphDBError(e.to_string()))?,
    };

    // Step 2: Build node lookup map for O(1) access by node_id
    let node_map: HashMap<&str, &NodeData> = nodes
        .iter()
        .map(|(node_id, node_data)| (node_id.as_str(), node_data))
        .collect();

    // Step 3: Iterate edges and build triplets
    let mut triplets = Vec::new();
    let mut skipped_count: usize = 0;

    for (source_id, target_id, relationship_name, edge_props) in &edges {
        // Look up source and target nodes
        let (source_data, target_data) = match (
            node_map.get(source_id.as_str()),
            node_map.get(target_id.as_str()),
        ) {
            (Some(src), Some(tgt)) => (*src, *tgt),
            _ => {
                skipped_count += 1;
                continue;
            }
        };

        // Build embeddable text for source and target
        let source_text = build_node_text(source_data);
        let target_text = build_node_text(target_data);

        // Extract relationship text: prefer edge_text property, fall back to relationship_name
        let relationship_text = edge_props
            .get("edge_text")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(relationship_name.as_str());

        // Format triplet text matching Python's add_data_points.py:242:
        //   f"{source_node_text} -› {relationship_text}-›{target_node_text}"
        let text = format!("{source_text} -\u{203a} {relationship_text}-\u{203a}{target_text}");

        // Parse source and target IDs as UUIDs
        let source_uuid = Uuid::parse_str(source_id).map_err(|e| {
            MemifyError::GraphDBError(format!("invalid source UUID '{source_id}': {e}"))
        })?;
        let target_uuid = Uuid::parse_str(target_id).map_err(|e| {
            MemifyError::GraphDBError(format!("invalid target UUID '{target_id}': {e}"))
        })?;

        // Extract node names for display
        let source_name = extract_string_prop(source_data, "name");
        let target_name = extract_string_prop(target_data, "name");

        let triplet = Triplet::new(source_uuid, target_uuid, relationship_name.clone(), text)
            .with_names(source_name, target_name);

        triplets.push(triplet);
    }

    info!(
        total_nodes = nodes.len(),
        total_edges = edges.len(),
        triplets_created = triplets.len(),
        skipped = skipped_count,
        "Extracted triplets from graph DB"
    );

    if skipped_count > 0 {
        warn!(
            skipped_count,
            "Skipped edges with missing source or target nodes"
        );
    }

    Ok(triplets)
}

/// Build embeddable text from a graph node's properties.
///
/// If the node has a non-empty "description", returns `"name: description"`.
/// Otherwise returns just the name. The result is trimmed.
fn build_node_text(node: &NodeData) -> String {
    let name = node.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let description = node
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if !description.is_empty() {
        format!("{name}: {description}").trim().to_string()
    } else {
        name.trim().to_string()
    }
}

/// Extract a string property from node data, returning an empty string if missing.
fn extract_string_prop(data: &NodeData, key: &str) -> String {
    data.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::borrow::Cow;

    fn make_node_data(name: &str, description: &str) -> NodeData {
        let mut data = NodeData::new();
        data.insert(Cow::Borrowed("name"), json!(name));
        if !description.is_empty() {
            data.insert(Cow::Borrowed("description"), json!(description));
        }
        data
    }

    #[test]
    fn test_build_node_text_with_description() {
        let data = make_node_data("Alice", "Software engineer");
        assert_eq!(build_node_text(&data), "Alice: Software engineer");
    }

    #[test]
    fn test_build_node_text_without_description() {
        let data = make_node_data("Alice", "");
        assert_eq!(build_node_text(&data), "Alice");
    }

    #[test]
    fn test_build_node_text_empty() {
        let data = NodeData::new();
        assert_eq!(build_node_text(&data), "");
    }

    #[test]
    fn test_extract_string_prop() {
        let data = make_node_data("Alice", "");
        assert_eq!(extract_string_prop(&data, "name"), "Alice");
        assert_eq!(extract_string_prop(&data, "missing_key"), "");
    }

    #[test]
    fn test_extract_string_prop_trims_whitespace() {
        let mut data = NodeData::new();
        data.insert(Cow::Borrowed("name"), json!("  Alice  "));
        assert_eq!(extract_string_prop(&data, "name"), "Alice");
    }
}
