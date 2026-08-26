//! Shared helpers for the crate's inline `#[cfg(test)]` modules.

use cognee_graph::{EdgeData, GraphNode};
use std::collections::HashSet;

/// Reference implementation of `GraphDBTrait::get_neighborhood` over an
/// in-memory `(nodes, edges)` pair, for the test doubles in this crate.
///
/// Matches what the real adapters return: the resolved id set is the seeds plus
/// every node within `depth` undirected hops, and the returned edges are those
/// with **both** endpoints in that resolved set (`pg_graph_adapter.rs` and
/// `ladybug.rs` both express exactly this). Test doubles that only stub
/// `get_connections` cannot rely on the trait's default BFS, which would walk
/// that stub and see an empty graph.
pub(crate) fn neighborhood_of(
    nodes: &[GraphNode],
    edges: &[EdgeData],
    seed_ids: &[String],
    depth: usize,
) -> (Vec<GraphNode>, Vec<EdgeData>) {
    if seed_ids.is_empty() {
        return (Vec::new(), Vec::new());
    }

    let mut resolved: HashSet<&str> = seed_ids.iter().map(String::as_str).collect();
    for _ in 0..depth {
        let mut next: HashSet<&str> = HashSet::new();
        for (src, tgt, _, _) in edges {
            if resolved.contains(src.as_str()) {
                next.insert(tgt.as_str());
            }
            if resolved.contains(tgt.as_str()) {
                next.insert(src.as_str());
            }
        }
        resolved.extend(next);
    }

    let out_nodes: Vec<GraphNode> = nodes
        .iter()
        .filter(|(id, _)| resolved.contains(id.as_str()))
        .cloned()
        .collect();

    let out_edges: Vec<EdgeData> = edges
        .iter()
        .filter(|(src, tgt, _, _)| {
            resolved.contains(src.as_str()) && resolved.contains(tgt.as_str())
        })
        .cloned()
        .collect();

    (out_nodes, out_edges)
}
