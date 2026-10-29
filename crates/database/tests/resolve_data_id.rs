#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! `resolve_data_id` (Python's `resolve_data_id`, SDK-1039 P21): the exact id
//! wins; a miss falls back to the `legacy_id` of a forked row; both lookups
//! are scoped to the dataset, and neither is owner-scoped.
//! Mirrors `cognee/tests/integration/tasks/test_dataset_scoped_data.py`.
#![cfg(feature = "sqlite")]

use cognee_database::ops::data::{create_data, resolve_data_id};
use cognee_database::{DatabaseConnection, connect, initialize};
use cognee_models::Data;
use uuid::Uuid;

async fn fresh() -> DatabaseConnection {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("initialize");
    db
}

async fn seed(
    db: &DatabaseConnection,
    id: Uuid,
    dataset_id: Uuid,
    legacy_id: Option<Uuid>,
    owner: Uuid,
) {
    let mut row = Data::builder(
        id,
        format!("data-{id}"),
        "file:///tmp/raw",
        "file:///tmp/raw",
        "txt",
        "text/plain",
        format!("hash-{id}"),
        owner,
    )
    .dataset_id(dataset_id)
    .build();
    row.legacy_id = legacy_id;
    create_data(db, row).await.expect("create_data");
}

/// The exact id resolves to itself, in its own dataset only.
#[tokio::test]
async fn exact_id_resolves_in_its_dataset_only() {
    let db = fresh().await;
    let (ds, other_ds) = (Uuid::new_v4(), Uuid::new_v4());
    let id = Uuid::new_v4();
    seed(&db, id, ds, None, Uuid::new_v4()).await;

    assert_eq!(resolve_data_id(&db, ds, id).await.unwrap(), Some(id));
    assert_eq!(resolve_data_id(&db, other_ds, id).await.unwrap(), None);
    assert_eq!(
        resolve_data_id(&db, ds, Uuid::new_v4()).await.unwrap(),
        None
    );
}

/// A pre-fork id (`legacy_id`) resolves to the row it forked into — within
/// the fork's dataset; from another dataset it is unknown.
#[tokio::test]
async fn legacy_id_resolves_within_its_dataset() {
    let db = fresh().await;
    let (keeper_ds, fork_ds) = (Uuid::new_v4(), Uuid::new_v4());
    let original = Uuid::new_v4();
    let fork = Uuid::new_v4();
    let owner = Uuid::new_v4();
    seed(&db, original, keeper_ds, None, owner).await;
    seed(&db, fork, fork_ds, Some(original), owner).await;

    // The original id is the keeper's exact id in its dataset ...
    assert_eq!(
        resolve_data_id(&db, keeper_ds, original).await.unwrap(),
        Some(original)
    );
    // ... and the fork's legacy id in the fork's dataset.
    assert_eq!(
        resolve_data_id(&db, fork_ds, original).await.unwrap(),
        Some(fork)
    );
    // The fork's own id resolves exactly; a legacy id from another dataset
    // does not.
    assert_eq!(
        resolve_data_id(&db, fork_ds, fork).await.unwrap(),
        Some(fork)
    );
    assert_eq!(
        resolve_data_id(&db, Uuid::new_v4(), original)
            .await
            .unwrap(),
        None
    );
}

/// The exact id wins over a `legacy_id` match in the same dataset, and the
/// lookup does not look at the owner.
#[tokio::test]
async fn exact_id_wins_over_legacy_id_and_owner_is_ignored() {
    let db = fresh().await;
    let ds = Uuid::new_v4();
    let shared = Uuid::new_v4();
    // One row has `shared` as its id, another (same dataset, other owner)
    // records it as its legacy id.
    seed(&db, shared, ds, None, Uuid::new_v4()).await;
    seed(&db, Uuid::new_v4(), ds, Some(shared), Uuid::new_v4()).await;

    assert_eq!(
        resolve_data_id(&db, ds, shared).await.unwrap(),
        Some(shared)
    );
}
