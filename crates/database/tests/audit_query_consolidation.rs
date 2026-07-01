#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Tests for the PR 3 query consolidations:
//! - `update_data_token_count` is a single UPDATE that still reports NotFound.
//! - `get_unique_nodes_for_data` / `get_unique_edges_for_data` fold their two
//!   selects into one correlated NOT EXISTS, returning only rows whose slug is
//!   not shared by another data_id in the same dataset.
//!
//! Runs on in-memory SQLite (NOT EXISTS is standard SQL, identical on Postgres).
#![cfg(feature = "sqlite")]

use chrono::Utc;
use cognee_database::ops::data::{create_data, get_data, update_data_token_count};
use cognee_database::ops::datasets::create_dataset;
use cognee_database::ops::graph_storage::{
    get_unique_edges_for_data, get_unique_nodes_for_data, upsert_edges, upsert_nodes,
};
use cognee_database::{DatabaseError, GraphEdge, GraphNode, connect, initialize};
use cognee_models::{Data, Dataset};
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn update_data_token_count_updates_and_reports_not_found() {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("migrate");

    let owner = Uuid::new_v4();
    let id = Uuid::new_v4();
    let d = Data::builder(
        id,
        "doc",
        "file:///tmp/raw",
        "file:///tmp/raw",
        "txt",
        "text/plain",
        "hash",
        owner,
    )
    .build();
    create_data(&db, d).await.expect("create_data");

    // Existing row: single UPDATE sets the count.
    update_data_token_count(&db, id, 123).await.expect("update");
    let row = get_data(&db, id).await.expect("get").expect("exists");
    assert_eq!(row.token_count, 123);

    // Missing row: rows_affected == 0 still surfaces NotFound (no read needed).
    let err = update_data_token_count(&db, Uuid::new_v4(), 5)
        .await
        .expect_err("missing id must error");
    assert!(
        matches!(err, DatabaseError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );
}

fn node(user: Uuid, data: Uuid, dataset: Uuid, slug: Uuid) -> GraphNode {
    GraphNode {
        id: Uuid::new_v4(),
        slug,
        user_id: user,
        data_id: data,
        dataset_id: dataset,
        label: Some("n".into()),
        node_type: "Entity".into(),
        indexed_fields: json!({ "index_fields": ["name"] }),
        attributes: None,
        created_at: Utc::now(),
    }
}

#[tokio::test]
async fn get_unique_nodes_excludes_slugs_shared_with_other_data() {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("migrate");

    let user = Uuid::new_v4();
    let dataset = Uuid::new_v4();
    create_dataset(&db, Dataset::new("uniq".into(), user, None, dataset))
        .await
        .expect("dataset");

    let data_a = Uuid::new_v4();
    let data_b = Uuid::new_v4();
    let slug_unique = Uuid::new_v4();
    let slug_shared = Uuid::new_v4();

    // data_a owns a unique-slug node and a shared-slug node; data_b also has the
    // shared slug, so only the unique-slug node should come back for data_a.
    let n_unique = node(user, data_a, dataset, slug_unique);
    let n_shared_a = node(user, data_a, dataset, slug_shared);
    let n_shared_b = node(user, data_b, dataset, slug_shared);
    upsert_nodes(&db, &[n_unique.clone(), n_shared_a, n_shared_b])
        .await
        .expect("upsert");

    let unique = get_unique_nodes_for_data(&db, data_a, dataset)
        .await
        .expect("query");
    assert_eq!(
        unique.len(),
        1,
        "only the non-shared slug is unique to data_a"
    );
    assert_eq!(unique[0].slug, slug_unique);
}

#[tokio::test]
async fn get_unique_edges_excludes_slugs_shared_with_other_data() {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("migrate");

    let user = Uuid::new_v4();
    let dataset = Uuid::new_v4();
    create_dataset(&db, Dataset::new("uniqe".into(), user, None, dataset))
        .await
        .expect("dataset");

    let data_a = Uuid::new_v4();
    let data_b = Uuid::new_v4();
    let slug_unique = Uuid::new_v4();
    let slug_shared = Uuid::new_v4();

    let mk_edge = |data: Uuid, slug: Uuid| GraphEdge {
        id: Uuid::new_v4(),
        slug,
        user_id: user,
        data_id: data,
        dataset_id: dataset,
        source_node_id: Uuid::new_v4(),
        destination_node_id: Uuid::new_v4(),
        relationship_name: "rel".into(),
        label: Some("e".into()),
        attributes: None,
        created_at: Utc::now(),
    };

    let e_unique = mk_edge(data_a, slug_unique);
    upsert_edges(
        &db,
        &[
            e_unique.clone(),
            mk_edge(data_a, slug_shared),
            mk_edge(data_b, slug_shared),
        ],
    )
    .await
    .expect("upsert");

    let unique = get_unique_edges_for_data(&db, data_a, dataset)
        .await
        .expect("query");
    assert_eq!(
        unique.len(),
        1,
        "only the non-shared slug is unique to data_a"
    );
    assert_eq!(unique[0].slug, slug_unique);
}
