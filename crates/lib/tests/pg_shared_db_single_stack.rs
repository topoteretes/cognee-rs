#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Single-database Postgres coexistence test (no LLM / no embedding model).
//!
//! Proves that all three cognee stores share ONE Postgres database without
//! colliding — the Neon / Python-v2 "everything in one database" layout:
//!
//!   * relational core         (`initialize()`  -> `seaql_migrations`)
//!   * pgvector vector store    (`PgVectorAdapter`-> `seaql_migrations_pgvector`)
//!   * Postgres graph-as-tables (`PgGraphAdapter` -> `seaql_migrations_pggraph`)
//!
//! and that a provenance graph upsert whose batch repeats a primary key
//! succeeds (the ON-CONFLICT-affects-row-twice fix). Together this exercises
//! Fix 1 (duplicate-id upsert) and the already-landed per-migrator tracking
//! table fix.
//!
//! Unlike `pg_full_stack_e2e`, this test needs no LLM or ONNX model: it writes
//! raw vectors and graph rows directly. It requires only a Postgres instance:
//!
//!   TEST_POSTGRES_URL="postgres://user:pass@localhost:5432/cognee" \
//!     cargo test -p cognee --features pggraph,pgvector,postgres \
//!       --test pg_shared_db_single_stack -- --nocapture
//!
//! Skipped cleanly when `TEST_POSTGRES_URL` is unset. Provisions its own
//! throwaway database (dropped at the end), so it needs no serialization and
//! never touches unrelated data on the server.
#![cfg(all(feature = "pggraph", feature = "pgvector", feature = "postgres"))]

use cognee::database::ops::datasets::create_dataset;
use cognee::database::ops::graph_storage::{get_nodes_by_data, upsert_nodes};
use cognee::database::{GraphNode, connect, initialize};
use cognee::models::Dataset;

use cognee_graph::{GraphDBTrait, GraphDBTraitExt, PgGraphAdapter};
use cognee_vector::{PgVectorAdapter, VectorDB, VectorPoint};

use chrono::Utc;
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
struct TestNode {
    id: String,
    name: String,
    #[serde(rename = "type")]
    node_type: String,
}

#[tokio::test]
async fn relational_pgvector_pggraph_coexist_in_one_database() {
    let Some(base_url) = cognee_test_utils::test_postgres_url() else {
        eprintln!(
            "TEST_POSTGRES_URL not set — skipping relational_pgvector_pggraph_coexist_in_one_database"
        );
        return;
    };

    // Own throwaway database: the three stores share ONE database (the point of
    // this test), but it is isolated from every other test and from any
    // pre-existing data on the server, so no reset or `#[serial]` is needed.
    let tmp = cognee_test_utils::create_temp_postgres_db(&base_url)
        .await
        .expect("create temp Postgres database");
    let url = tmp.url();

    // ---- 1. Relational core: migrate into the shared DB. --------------------
    let db = connect(url).await.expect("connect relational");
    initialize(&db).await.expect("relational migrate");

    // ---- 2. pgvector: separate migrator, same DB. ---------------------------
    let vector = PgVectorAdapter::new(url, 3)
        .await
        .expect("PgVectorAdapter must initialize on the shared DB");

    // ---- 3. Postgres graph-as-tables: third migrator, same DB. --------------
    let graph = PgGraphAdapter::new(url)
        .await
        .expect("PgGraphAdapter must initialize on the shared DB");

    // A unique collection/label per run so the shared DB stays reusable.
    let tag = Uuid::new_v4().simple().to_string();
    let data_type = format!("Chunk_{tag}");

    // ---- 4. Vector write + search round-trips. ------------------------------
    vector
        .create_collection(&data_type, "text", 3)
        .await
        .expect("create vector collection");
    let points = vec![
        VectorPoint::new(Uuid::new_v4(), vec![1.0, 0.0, 0.0]).with_metadata("name", json!("a")),
        VectorPoint::new(Uuid::new_v4(), vec![0.0, 1.0, 0.0]).with_metadata("name", json!("b")),
    ];
    vector
        .index_points(&data_type, "text", &points)
        .await
        .expect("index vectors");
    assert_eq!(
        vector
            .collection_size(&data_type, "text")
            .await
            .expect("vector collection size"),
        2,
        "pgvector must persist the indexed points in the shared DB"
    );

    // ---- 5. Graph-as-tables write + read round-trips. -----------------------
    graph.delete_graph().await.expect("reset graph");
    let n1 = TestNode {
        id: format!("g1_{tag}"),
        name: "Alice".into(),
        node_type: "Person".into(),
    };
    let n2 = TestNode {
        id: format!("g2_{tag}"),
        name: "Bob".into(),
        node_type: "Person".into(),
    };
    graph.add_node(&n1).await.expect("add graph node 1");
    graph.add_node(&n2).await.expect("add graph node 2");
    graph
        .add_edge(&n1.id, &n2.id, "KNOWS", None)
        .await
        .expect("add graph edge");
    let (gnodes, gedges) = graph.get_graph_data().await.expect("read graph");
    assert!(
        gnodes.len() >= 2 && !gedges.is_empty(),
        "pggraph must persist nodes+edges in the shared DB (got {} nodes, {} edges)",
        gnodes.len(),
        gedges.len()
    );

    // ---- 6. Provenance duplicate-id upsert coexists in the same DB. ---------
    // The provenance `nodes` table lives in the same database; a batch repeating
    // a primary key must collapse to one row rather than tripping Postgres' ON
    // CONFLICT "cannot affect row a second time". The full node+edge dedup
    // contract is covered by `provenance_batch`; here we only confirm it works
    // alongside the pgvector and pggraph stores.
    let user = Uuid::new_v4();
    let dataset = Uuid::new_v4();
    let data = Uuid::new_v4();
    create_dataset(
        &db,
        Dataset::new(format!("shared-{tag}"), user, None, dataset),
    )
    .await
    .expect("seed dataset");

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
        .expect("duplicate-id provenance upsert must succeed alongside pgvector + pggraph");
    let pnodes = get_nodes_by_data(&db, data, dataset)
        .await
        .expect("read provenance nodes");
    assert_eq!(
        pnodes.len(),
        1,
        "duplicate provenance node id must collapse to a single row"
    );

    // ---- Cleanup. -----------------------------------------------------------
    let _ = vector.delete_collection(&data_type, "text").await;
    let _ = graph.delete_graph().await;
    // Close every connection before dropping the database so cleanup does not
    // depend on `WITH (FORCE)` terminating our own backends.
    drop(vector);
    drop(graph);
    drop(db);
    tmp.cleanup().await;
}
