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

#[tokio::test]
async fn every_ledger_edge_reaches_a_ladybug_graph() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let graph: Arc<dyn GraphDBTrait> = Arc::new(
        LadybugAdapter::new(&dir.path().join("graph").to_string_lossy())
            .await
            .expect("ladybug"),
    );
    graph.initialize().await.expect("initialize graph");
    nodes_before_edges::every_ledger_edge_reaches_the_graph(graph).await;
}
