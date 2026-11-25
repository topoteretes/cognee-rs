#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Every edge a cognify run claims in the ledger is in a Ladybug graph
//! (SDK-1694 O4a review). Ladybug's `add_edges` is `UNWIND … MATCH … MERGE`,
//! which silently drops an edge whose endpoint node does not exist yet, so this
//! fails whenever the persistence layer writes an edge before its endpoints.
//! See `nodes_before_edges/mod.rs` for the scenario.

mod nodes_before_edges;

use std::sync::Arc;

use cognee_graph::{GraphDBTrait, LadybugAdapter};

async fn ladybug(dir: &tempfile::TempDir) -> Arc<dyn GraphDBTrait> {
    let graph: Arc<dyn GraphDBTrait> = Arc::new(
        LadybugAdapter::new(&dir.path().join("graph").to_string_lossy())
            .await
            .expect("ladybug"),
    );
    graph.initialize().await.expect("initialize graph");
    graph
}

#[tokio::test]
async fn every_ledger_edge_reaches_a_ladybug_graph() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    nodes_before_edges::every_ledger_edge_reaches_the_graph(ladybug(&dir).await).await;
}

/// The standalone `extract_graph_from_data` writes the EntityType nodes the
/// ontology's `is_a` edges name before those edges.
#[tokio::test]
async fn standalone_extraction_edges_reach_a_ladybug_graph() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    nodes_before_edges::standalone_extraction_edges_reach_the_graph(ladybug(&dir).await).await;
}

/// The standalone `create_web_page_nodes` writes the chunk node its
/// `SOURCED_FROM` edge starts at before that edge.
#[tokio::test]
async fn standalone_web_page_edges_reach_a_ladybug_graph() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    nodes_before_edges::standalone_web_page_edges_reach_the_graph(ladybug(&dir).await).await;
}
