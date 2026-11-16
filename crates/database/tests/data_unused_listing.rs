#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! `DeleteDb::count_tracked_data` / `list_unused_data`, the queries behind
//! `cleanup_unused_data` (SDK-1048, Python
//! `cognee/tasks/cleanup/cleanup_unused_data.py`): the tracking check counts
//! every owner's rows, and the unused selection is `last_accessed < cutoff`
//! **or** never accessed, optionally limited to one owner's datasets.
//!
//! Runs on in-memory SQLite, so it needs no external database.
#![cfg(feature = "sqlite")]

use chrono::{Duration, Utc};
use cognee_database::ops::data::{create_data, update_last_accessed};
use cognee_database::ops::datasets::create_dataset;
use cognee_database::{DatabaseConnection, DeleteDb, connect, initialize};
use cognee_models::{Data, Dataset};
use uuid::Uuid;

async fn db() -> DatabaseConnection {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("initialize");
    db
}

async fn seed_dataset(db: &DatabaseConnection, owner: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    create_dataset(db, Dataset::new(format!("ds-{id}"), owner, None, id))
        .await
        .expect("create_dataset");
    id
}

/// A Data row in `dataset`, owned by `owner`, last accessed `minutes_ago`
/// minutes ago (`None`: never).
async fn seed_data(
    db: &DatabaseConnection,
    owner: Uuid,
    dataset: Uuid,
    minutes_ago: Option<i64>,
) -> Uuid {
    let id = Uuid::new_v4();
    let d = Data::builder(
        id,
        format!("data-{id}"),
        "file:///tmp/raw",
        "file:///tmp/raw",
        "txt",
        "text/plain",
        format!("hash-{id}"),
        owner,
    )
    .dataset_id(dataset)
    .build();
    create_data(db, d).await.expect("create_data");
    if let Some(minutes) = minutes_ago {
        update_last_accessed(db, &[id], Utc::now() - Duration::minutes(minutes))
            .await
            .expect("update_last_accessed");
    }
    id
}

fn ids(rows: &[Data]) -> Vec<Uuid> {
    let mut ids: Vec<Uuid> = rows.iter().map(|d| d.id).collect();
    ids.sort();
    ids
}

fn sorted(mut ids: Vec<Uuid>) -> Vec<Uuid> {
    ids.sort();
    ids
}

#[tokio::test]
async fn tracked_rows_are_counted_across_owners() {
    let db = db().await;
    let (alice, bob) = (Uuid::new_v4(), Uuid::new_v4());
    let (a, b) = (seed_dataset(&db, alice).await, seed_dataset(&db, bob).await);
    seed_data(&db, alice, a, None).await;
    assert_eq!(db.count_tracked_data().await.unwrap(), 0);
    seed_data(&db, alice, a, Some(5)).await;
    seed_data(&db, bob, b, Some(500)).await;
    assert_eq!(db.count_tracked_data().await.unwrap(), 2);
}

#[tokio::test]
async fn unused_rows_are_stale_or_never_accessed() {
    let db = db().await;
    let owner = Uuid::new_v4();
    let ds = seed_dataset(&db, owner).await;
    let stale = seed_data(&db, owner, ds, Some(120)).await;
    let never = seed_data(&db, owner, ds, None).await;
    let _recent = seed_data(&db, owner, ds, Some(10)).await;

    let cutoff = Utc::now() - Duration::minutes(60);
    let unused = db.list_unused_data(cutoff, None).await.unwrap();
    assert_eq!(ids(&unused), sorted(vec![stale, never]));
}

#[tokio::test]
async fn the_owner_filter_keeps_only_that_owners_datasets() {
    let db = db().await;
    let (alice, bob) = (Uuid::new_v4(), Uuid::new_v4());
    let (a, b) = (seed_dataset(&db, alice).await, seed_dataset(&db, bob).await);
    let alice_stale = seed_data(&db, alice, a, Some(120)).await;
    let bob_stale = seed_data(&db, bob, b, Some(120)).await;

    let cutoff = Utc::now() - Duration::minutes(60);
    let all = db.list_unused_data(cutoff, None).await.unwrap();
    assert_eq!(ids(&all), sorted(vec![alice_stale, bob_stale]));
    let alice_only = db.list_unused_data(cutoff, Some(alice)).await.unwrap();
    assert_eq!(ids(&alice_only), vec![alice_stale]);
    // An owner with no datasets has nothing to clean up.
    let nobody = db
        .list_unused_data(cutoff, Some(Uuid::new_v4()))
        .await
        .unwrap();
    assert!(nobody.is_empty());
}
