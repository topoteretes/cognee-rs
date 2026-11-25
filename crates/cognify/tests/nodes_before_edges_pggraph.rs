#![cfg(feature = "pggraph")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_methods,
    reason = "test code — panics are acceptable failures, and the server URL \
              is read from the process env like every Postgres-gated suite"
)]
//! Every edge a cognify run claims in the ledger is in a Postgres graph
//! (SDK-1694 O4a review). `graph_edge` has foreign keys to `graph_node`, so an
//! edge written before its endpoint fails the run with
//! `graph_edge_source_id_fkey`. See `nodes_before_edges/mod.rs` for the
//! scenario.
//!
//! Gated on `PGGRAPH_TEST_URL` (skips as `ok` without it, printing the skip
//! marker `assert_pg_suite_ran.sh` looks for) and on the `pggraph` feature:
//!
//! ```text
//! PGGRAPH_TEST_URL=postgres://postgres:postgres@localhost:5432/postgres \
//!   cargo test -p cognee-cognify --features pggraph \
//!   --test nodes_before_edges_pggraph -- --nocapture
//! ```
//!
//! Each case provisions a throwaway database of its own on that server and
//! drops it again.
//!
//! The two `standalone_*` cases drive the public standalone writers
//! (`extract_graph_from_data` with an ontology, `create_web_page_nodes` on
//! unstored chunks), which bypass the pipeline's ordered writer.

mod nodes_before_edges;

use std::future::Future;
use std::sync::Arc;

use cognee_graph::{GraphDBTrait, PgGraphAdapter};
use cognee_test_utils::create_temp_postgres_db;

/// Run `scenario` against a Postgres graph in a throwaway database, or skip
/// (printing the marker `assert_pg_suite_ran.sh` looks for) without
/// `PGGRAPH_TEST_URL`.
async fn on_a_postgres_graph<F, Fut>(name: &str, scenario: F)
where
    F: FnOnce(Arc<dyn GraphDBTrait>) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let Ok(base_url) = std::env::var("PGGRAPH_TEST_URL") else {
        eprintln!("PGGRAPH_TEST_URL not set — skipping {name}");
        return;
    };
    let tmp = create_temp_postgres_db(&base_url)
        .await
        .expect("PGGRAPH_TEST_URL is set, so CREATE DATABASE must succeed on that server");
    let url = tmp.url().to_string();
    // Everything fallible runs in a task, so a panic still reaches the
    // cleanup below instead of stranding the database.
    let outcome = tokio::spawn(async move {
        let graph: Arc<dyn GraphDBTrait> = Arc::new(
            PgGraphAdapter::new(&url)
                .await
                .expect("the adapter connects and migrates"),
        );
        scenario(Arc::clone(&graph)).await;
        graph.close().await.ok();
    })
    .await;
    tmp.cleanup().await;
    if let Err(join_err) = outcome {
        std::panic::resume_unwind(join_err.into_panic());
    }
}

#[tokio::test]
async fn every_ledger_edge_reaches_a_postgres_graph() {
    on_a_postgres_graph(
        "every_ledger_edge_reaches_a_postgres_graph",
        nodes_before_edges::every_ledger_edge_reaches_the_graph,
    )
    .await;
}

/// The EntityType nodes the ontology's `is_a` edges name are written before
/// those edges, or `graph_edge_source_id_fkey` fails the call.
#[tokio::test]
async fn standalone_extraction_edges_reach_a_postgres_graph() {
    on_a_postgres_graph(
        "standalone_extraction_edges_reach_a_postgres_graph",
        nodes_before_edges::standalone_extraction_edges_reach_the_graph,
    )
    .await;
}

/// The chunk node a `SOURCED_FROM` edge starts at is written before that
/// edge, or `graph_edge_source_id_fkey` fails the call.
#[tokio::test]
async fn standalone_web_page_edges_reach_a_postgres_graph() {
    on_a_postgres_graph(
        "standalone_web_page_edges_reach_a_postgres_graph",
        nodes_before_edges::standalone_web_page_edges_reach_the_graph,
    )
    .await;
}
