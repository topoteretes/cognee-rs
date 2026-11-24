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
//! The case provisions a throwaway database of its own on that server and
//! drops it again.

mod nodes_before_edges;

use std::sync::Arc;

use cognee_graph::{GraphDBTrait, PgGraphAdapter};
use cognee_test_utils::create_temp_postgres_db;

#[tokio::test]
async fn every_ledger_edge_reaches_a_postgres_graph() {
    let Ok(base_url) = std::env::var("PGGRAPH_TEST_URL") else {
        eprintln!("PGGRAPH_TEST_URL not set — skipping every_ledger_edge_reaches_a_postgres_graph");
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
        nodes_before_edges::every_ledger_edge_reaches_the_graph(Arc::clone(&graph)).await;
        graph.close().await.ok();
    })
    .await;
    tmp.cleanup().await;
    if let Err(join_err) = outcome {
        std::panic::resume_unwind(join_err.into_panic());
    }
}
