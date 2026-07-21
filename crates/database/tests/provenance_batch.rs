#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Regression: provenance upserts must chunk their inserts so a large graph
//! does not exceed SQLite's bound-variable cap (`SQLITE_MAX_VARIABLE_NUMBER`).
//!
//! A full-length book (e.g. War & Peace) produces several thousand provenance
//! nodes and edges. A single `insert_many` binds `rows × columns` parameters in
//! one statement; past ~3277 edges (×10 columns) that overflows SQLite's 32766
//! cap and fails with "too many SQL variables". `upsert_nodes`/`upsert_edges`
//! chunk by `PROVENANCE_INSERT_BATCH`, so this must succeed.

use chrono::Utc;
use cognee_database::ops::datasets::create_dataset;
use cognee_database::ops::graph_storage::{upsert_edges, upsert_nodes};
use cognee_database::{GraphEdge, GraphNode, connect, initialize};
use cognee_models::Dataset;
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn upserts_large_graph_without_variable_overflow() {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("migrate");

    // nodes/edges FK `datasets.id`, so seed the parent dataset row.
    let user = Uuid::new_v4();
    let dataset = Uuid::new_v4();
    create_dataset(&db, Dataset::new("war-peace".into(), user, None, dataset))
        .await
        .expect("seed dataset");

    // Well above PROVENANCE_INSERT_BATCH (500) and large enough that a single
    // multi-row insert would blow past SQLite's 32766-variable cap
    // (≈ 3277 edges × 10 columns). 4000 forces the chunked path.
    let n = 4000usize;
    let data = Uuid::new_v4();

    let nodes: Vec<GraphNode> = (0..n)
        .map(|_| GraphNode {
            id: Uuid::new_v4(),
            slug: Uuid::new_v4(),
            user_id: user,
            data_id: data,
            dataset_id: dataset,
            label: Some("n".into()),
            node_type: "Entity".into(),
            indexed_fields: json!({ "index_fields": ["name"] }),
            attributes: None,
            created_at: Utc::now(),
        })
        .collect();

    upsert_nodes(&db, &nodes)
        .await
        .expect("node upsert must chunk under the SQL-variable cap");

    // Edges reference real nodes so any FK constraint is satisfied.
    let edges: Vec<GraphEdge> = (0..n)
        .map(|i| GraphEdge {
            id: Uuid::new_v4(),
            slug: Uuid::new_v4(),
            user_id: user,
            data_id: data,
            dataset_id: dataset,
            source_node_id: nodes[i].id,
            destination_node_id: nodes[(i + 1) % n].id,
            relationship_name: "rel".into(),
            label: Some("e".into()),
            attributes: None,
            created_at: Utc::now(),
        })
        .collect();

    upsert_edges(&db, &edges)
        .await
        .expect("edge upsert must chunk under the SQL-variable cap");
}

/// Postgres-only provenance tests. Gated on the `postgres` feature so that,
/// without a Postgres driver compiled in, they neither compile nor run — a bare
/// `connect("postgres://…")` would otherwise panic instead of skipping. Each
/// test provisions its OWN throwaway database (`create_temp_postgres_db`) and
/// drops it afterwards, so they need no `#[serial]` guard (which would be a
/// no-op under the project's per-process `cargo nextest` runner anyway) and can
/// never clobber a developer's existing data.
#[cfg(feature = "postgres")]
mod postgres {
    use super::*;
    use cognee_database::ops::graph_storage::{get_edges_by_data, get_nodes_by_data};

    /// Regression (single-DB Postgres): a provenance upsert batch that contains
    /// the SAME primary key twice must not fail.
    ///
    /// On Postgres, `INSERT … ON CONFLICT (id) DO UPDATE …` errors with
    /// "ON CONFLICT DO UPDATE command cannot affect row a second time" when a
    /// single statement's VALUES list carries the conflict-target key more than
    /// once. `upsert_nodes`/`upsert_edges` de-duplicate by primary key within each
    /// batch, keeping the LAST occurrence to preserve upsert-update semantics — so
    /// this must succeed and store exactly one row reflecting the last write.
    ///
    /// Skipped when `TEST_POSTGRES_URL` is unset. SQLite tolerates the duplicate,
    /// so this scenario only reproduces on a real Postgres.
    #[tokio::test]
    async fn upsert_batch_with_duplicate_ids_dedups_keeping_last() {
        let Some(base_url) = cognee_test_utils::test_postgres_url() else {
            eprintln!(
                "TEST_POSTGRES_URL not set — skipping upsert_batch_with_duplicate_ids_dedups_keeping_last"
            );
            return;
        };
        // Own throwaway database: isolated from every other test and from any
        // pre-existing data on the server.
        let tmp = cognee_test_utils::create_temp_postgres_db(&base_url)
            .await
            .expect("create temp Postgres database");
        let db = connect(tmp.url()).await.expect("connect to temp Postgres");
        initialize(&db).await.expect("migrate relational schema");

        let user = Uuid::new_v4();
        let dataset = Uuid::new_v4();
        let data = Uuid::new_v4();
        create_dataset(&db, Dataset::new("dup-upsert".into(), user, None, dataset))
            .await
            .expect("seed dataset");

        // --- Nodes: same id twice in one batch, different label. --------------
        let node_id = Uuid::new_v4();
        let mk_node = |label: &str| GraphNode {
            id: node_id,
            slug: Uuid::new_v4(),
            user_id: user,
            data_id: data,
            dataset_id: dataset,
            label: Some(label.into()),
            node_type: "Entity".into(),
            indexed_fields: json!({ "index_fields": ["name"] }),
            attributes: None,
            created_at: Utc::now(),
        };
        upsert_nodes(&db, &[mk_node("first"), mk_node("last")])
            .await
            .expect("node upsert with a duplicate id in the batch must succeed on Postgres");

        let stored = get_nodes_by_data(&db, data, dataset)
            .await
            .expect("read back nodes");
        assert_eq!(
            stored.len(),
            1,
            "duplicate id must collapse to a single node row"
        );
        assert_eq!(
            stored[0].label.as_deref(),
            Some("last"),
            "the LAST occurrence in the batch must win the upsert"
        );

        // --- Edges: two distinct endpoints, then a duplicate edge id. ---------
        let a_id = Uuid::new_v4();
        let b_id = Uuid::new_v4();
        let a = GraphNode {
            id: a_id,
            ..mk_node("endpoint-a")
        };
        let b = GraphNode {
            id: b_id,
            ..mk_node("endpoint-b")
        };
        upsert_nodes(&db, &[a, b])
            .await
            .expect("seed edge endpoints");

        let edge_id = Uuid::new_v4();
        let mk_edge = |rel: &str| GraphEdge {
            id: edge_id,
            slug: Uuid::new_v4(),
            user_id: user,
            data_id: data,
            dataset_id: dataset,
            source_node_id: a_id,
            destination_node_id: b_id,
            relationship_name: rel.into(),
            label: Some(rel.into()),
            attributes: None,
            created_at: Utc::now(),
        };
        upsert_edges(&db, &[mk_edge("first_rel"), mk_edge("last_rel")])
            .await
            .expect("edge upsert with a duplicate id in the batch must succeed on Postgres");

        let stored_edges = get_edges_by_data(&db, data, dataset)
            .await
            .expect("read back edges");
        assert_eq!(
            stored_edges.len(),
            1,
            "duplicate id must collapse to a single edge row"
        );
        assert_eq!(
            stored_edges[0].relationship_name, "last_rel",
            "the LAST occurrence in the batch must win the upsert"
        );

        // Close the connection before dropping the database so cleanup does not
        // depend on `WITH (FORCE)` terminating our own backend.
        drop(db);
        tmp.cleanup().await;
    }

    /// A provisioned throwaway database is pristine and `initialize()` succeeds on
    /// it — and doing so repeatedly (each run gets its own database) works without
    /// any manual reset between runs. This is the per-test-database replacement
    /// for the old shared-database + manual-reset workflow.
    #[tokio::test]
    async fn temp_database_supports_back_to_back_initialize() {
        let Some(base_url) = cognee_test_utils::test_postgres_url() else {
            eprintln!(
                "TEST_POSTGRES_URL not set — skipping temp_database_supports_back_to_back_initialize"
            );
            return;
        };

        for run in 0..2 {
            let tmp = cognee_test_utils::create_temp_postgres_db(&base_url)
                .await
                .unwrap_or_else(|e| panic!("create temp DB (run {run}): {e}"));
            let db = connect(tmp.url())
                .await
                .unwrap_or_else(|e| panic!("connect (run {run}): {e}"));
            initialize(&db)
                .await
                .unwrap_or_else(|e| panic!("initialize on a fresh database (run {run}): {e}"));
            // Schema is usable.
            create_dataset(
                &db,
                Dataset::new(format!("probe-{run}"), Uuid::new_v4(), None, Uuid::new_v4()),
            )
            .await
            .unwrap_or_else(|e| panic!("seed (run {run}): {e}"));
            // Close the connection before dropping the database.
            drop(db);
            tmp.cleanup().await;
        }
    }
}
