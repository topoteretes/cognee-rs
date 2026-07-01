#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Provenance node/edge upserts run inside a single transaction at the cognify
//! call site, so a failure partway through the group must roll the whole thing
//! back (no half-written provenance graph). `upsert_nodes`/`upsert_edges` are
//! generic over `ConnectionTrait`, so they run against either a `DatabaseConnection`
//! or a `DatabaseTransaction`. These tests exercise the transactional path on
//! in-memory SQLite (no external services).
#![cfg(feature = "sqlite")]

use chrono::Utc;
use cognee_database::ops::datasets::create_dataset;
use cognee_database::ops::graph_storage::{get_nodes_by_dataset, upsert_edges, upsert_nodes};
use cognee_database::{GraphEdge, GraphNode, connect, initialize};
use cognee_models::Dataset;
use sea_orm::TransactionTrait;
use serde_json::json;
use uuid::Uuid;

async fn seed_dataset(db: &cognee_database::DatabaseConnection) -> (Uuid, Uuid) {
    let user = Uuid::new_v4();
    let dataset = Uuid::new_v4();
    create_dataset(db, Dataset::new("txn-test".into(), user, None, dataset))
        .await
        .expect("seed dataset");
    (user, dataset)
}

fn make_nodes(user: Uuid, dataset: Uuid, data: Uuid, n: usize) -> Vec<GraphNode> {
    (0..n)
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
        .collect()
}

/// A failure partway through the node+edge group rolls the node write back too.
#[tokio::test]
async fn provenance_group_rolls_back_on_midway_failure() {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("migrate");
    let (user, dataset) = seed_dataset(&db).await;
    let data = Uuid::new_v4();
    let nodes = make_nodes(user, dataset, data, 3);

    let txn = db.begin().await.expect("begin");
    upsert_nodes(&txn, &nodes)
        .await
        .expect("nodes upsert in txn");
    // Simulate a failure between the node and edge writes: bail out before
    // commit. Dropping/rolling back the transaction must discard the nodes too.
    txn.rollback().await.expect("rollback");

    let persisted = get_nodes_by_dataset(&db, dataset).await.expect("query");
    assert!(
        persisted.is_empty(),
        "rolled-back node provenance must not persist, found {}",
        persisted.len()
    );
}

/// The happy path commits the whole group atomically.
#[tokio::test]
async fn provenance_group_commits_atomically() {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("migrate");
    let (user, dataset) = seed_dataset(&db).await;
    let data = Uuid::new_v4();
    let nodes = make_nodes(user, dataset, data, 3);

    let edges: Vec<GraphEdge> = (0..nodes.len())
        .map(|i| GraphEdge {
            id: Uuid::new_v4(),
            slug: Uuid::new_v4(),
            user_id: user,
            data_id: data,
            dataset_id: dataset,
            source_node_id: nodes[i].id,
            destination_node_id: nodes[(i + 1) % nodes.len()].id,
            relationship_name: "rel".into(),
            label: Some("e".into()),
            attributes: None,
            created_at: Utc::now(),
        })
        .collect();

    let txn = db.begin().await.expect("begin");
    upsert_nodes(&txn, &nodes).await.expect("nodes");
    upsert_edges(&txn, &edges).await.expect("edges");
    txn.commit().await.expect("commit");

    let persisted = get_nodes_by_dataset(&db, dataset).await.expect("query");
    assert_eq!(persisted.len(), nodes.len(), "committed nodes must persist");
}
