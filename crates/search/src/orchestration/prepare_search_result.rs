use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};
use tracing::debug;
use uuid::Uuid;

use crate::types::{
    SearchContext, SearchGraph, SearchGraphEdge, SearchGraphNode, SearchOutput, SearchResponse,
    SearchType,
};

const CONTEXT_LABEL_COMBINED: &str = "combined";
const CONTEXT_LABEL_DEFAULT: &str = "default";

pub fn prepare_search_result(
    search_type: SearchType,
    result: SearchOutput,
    context: Option<SearchContext>,
    datasets: Option<Vec<Uuid>>,
    only_context: bool,
    use_combined_context: bool,
    verbose: bool,
) -> SearchResponse {
    let context_label = if use_combined_context {
        CONTEXT_LABEL_COMBINED.to_string()
    } else {
        CONTEXT_LABEL_DEFAULT.to_string()
    };

    let context_map = context
        .clone()
        .map(|items| HashMap::from([(context_label.clone(), items)]));

    let graphs = context
        .as_ref()
        .and_then(transform_context_to_graph)
        .map(|graph| HashMap::from([(context_label.clone(), graph)]));

    let context_item_count = context
        .as_ref()
        .map(|items| items.len())
        .unwrap_or_default();
    let graph_edge_count = graphs
        .as_ref()
        .and_then(|graphs_by_dataset| graphs_by_dataset.get(&context_label))
        .map(|graph| graph.edges.len())
        .unwrap_or_default();
    debug!(
        context_item_count,
        graph_edge_count, "search context prepared"
    );

    let diagnostics = if tracing::enabled!(tracing::Level::DEBUG) {
        Some(HashMap::from([
            ("context_item_count".to_string(), json!(context_item_count)),
            ("graph_edge_count".to_string(), json!(graph_edge_count)),
        ]))
    } else {
        None
    };

    // When neither verbose nor only_context is set, strip context and graph
    // from the response to reduce payload size.
    let (final_context, final_graphs) = if verbose || only_context {
        (context_map, graphs)
    } else {
        (None, None)
    };

    SearchResponse {
        search_type,
        result,
        context: final_context,
        graphs: final_graphs,
        diagnostics,
        datasets,
        only_context,
        use_combined_context,
        verbose,
    }
}

fn transform_context_to_graph(context: &SearchContext) -> Option<SearchGraph> {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut seen_node_ids = HashSet::new();

    for item in context {
        let source_id = item
            .payload
            .get("source_id")
            .and_then(Value::as_str)
            .or_else(|| item.payload.get("source_name").and_then(Value::as_str));

        let target_id = item
            .payload
            .get("target_id")
            .and_then(Value::as_str)
            .or_else(|| item.payload.get("target_name").and_then(Value::as_str));

        let relationship = item
            .payload
            .get("relationship")
            .and_then(Value::as_str)
            .or_else(|| {
                item.payload
                    .get("relationship_name")
                    .and_then(Value::as_str)
            });

        if let (Some(source_id), Some(target_id), Some(relationship)) =
            (source_id, target_id, relationship)
        {
            let source_label = item
                .payload
                .get("source_name")
                .and_then(Value::as_str)
                .unwrap_or(source_id);
            let target_label = item
                .payload
                .get("target_name")
                .and_then(Value::as_str)
                .unwrap_or(target_id);

            if seen_node_ids.insert(source_id.to_string()) {
                nodes.push(SearchGraphNode {
                    id: source_id.to_string(),
                    label: source_label.to_string(),
                });
            }

            if seen_node_ids.insert(target_id.to_string()) {
                nodes.push(SearchGraphNode {
                    id: target_id.to_string(),
                    label: target_label.to_string(),
                });
            }

            edges.push(SearchGraphEdge {
                source: source_id.to_string(),
                target: target_id.to_string(),
                relationship: relationship.to_string(),
                weight: item.score,
            });
        }
    }

    if edges.is_empty() {
        None
    } else {
        Some(SearchGraph { nodes, edges })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::types::{SearchItem, SearchOutput, SearchType};

    #[test]
    fn creates_graph_from_context() {
        let context = vec![SearchItem {
            id: None,
            score: Some(0.8),
            payload: json!({
                "source_id": "a",
                "target_id": "b",
                "source_name": "Alice",
                "target_name": "Bob",
                "relationship": "KNOWS"
            }),
        }];

        let response = super::prepare_search_result(
            SearchType::GraphCompletion,
            SearchOutput::Text("answer".to_string()),
            Some(context),
            None,
            false,
            false,
            true,
        );

        let graphs = response.graphs.expect("graph must be present");
        let graph = graphs.get("default").expect("default graph must exist");
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.edges.len(), 1);
    }
}
