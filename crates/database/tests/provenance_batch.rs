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
