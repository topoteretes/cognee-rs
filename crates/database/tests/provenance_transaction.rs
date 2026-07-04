#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Provenance node/edge groups are written through `upsert_provenance_graph`,
//! which wraps both upserts in a single transaction, so a failure partway
//! through the group must roll the whole thing back (no half-written
//! provenance graph). These tests call that function directly — the same
//! seam the cognify pipeline uses — on in-memory SQLite (no external
//! services).
#![cfg(feature = "sqlite")]

use chrono::Utc;
use cognee_database::ops::datasets::create_dataset;
use cognee_database::ops::graph_storage::{
    get_edges_by_dataset, get_nodes_by_dataset, upsert_provenance_graph,
};
use cognee_database::{GraphEdge, GraphNode, connect, initialize};
use cognee_models::Dataset;
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

fn make_edge(user: Uuid, dataset: Uuid, data: Uuid, source: Uuid, dest: Uuid) -> GraphEdge {
    GraphEdge {
        id: Uuid::new_v4(),
        slug: Uuid::new_v4(),
        user_id: user,
        data_id: data,
        dataset_id: dataset,
        source_node_id: source,
        destination_node_id: dest,
        relationship_name: "rel".into(),
        label: Some("e".into()),
        attributes: None,
        created_at: Utc::now(),
    }
}

/// The happy path commits the whole group atomically.
#[tokio::test]
async fn provenance_graph_commits_atomically() {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("migrate");
    let (user, dataset) = seed_dataset(&db).await;
    let data = Uuid::new_v4();
    let nodes = make_nodes(user, dataset, data, 3);

    let edges: Vec<GraphEdge> = (0..nodes.len())
        .map(|i| {
            make_edge(
                user,
                dataset,
                data,
                nodes[i].id,
                nodes[(i + 1) % nodes.len()].id,
            )
        })
        .collect();

    upsert_provenance_graph(&db, &nodes, &edges)
        .await
        .expect("atomic provenance upsert");

    let persisted_nodes = get_nodes_by_dataset(&db, dataset).await.expect("query");
    assert_eq!(
        persisted_nodes.len(),
        nodes.len(),
        "committed nodes must persist"
    );
    let persisted_edges = get_edges_by_dataset(&db, dataset).await.expect("query");
    assert_eq!(
        persisted_edges.len(),
        edges.len(),
        "committed edges must persist"
    );
}

/// A failure in the edge write (after the nodes were written inside the same
/// transaction) rolls the node write back too. The failure is injected through
/// the public API: an edge referencing a dataset that does not exist violates
/// the `edges.dataset_id -> datasets.id` foreign key, which sqlx enforces by
/// default on SQLite.
#[tokio::test]
async fn provenance_graph_rolls_back_when_edge_upsert_fails() {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("migrate");
    let (user, dataset) = seed_dataset(&db).await;
    let data = Uuid::new_v4();
    let nodes = make_nodes(user, dataset, data, 3);

    // dataset_id with no `datasets` row -> FK violation midway through the group.
    let missing_dataset = Uuid::new_v4();
    let poison_edge = make_edge(user, missing_dataset, data, nodes[0].id, nodes[1].id);

    let result = upsert_provenance_graph(&db, &nodes, &[poison_edge]).await;
    assert!(
        result.is_err(),
        "edge upsert against a missing dataset must fail"
    );

    let persisted = get_nodes_by_dataset(&db, dataset).await.expect("query");
    assert!(
        persisted.is_empty(),
        "rolled-back node provenance must not persist, found {}",
        persisted.len()
    );
}
