#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Regression: `update_last_accessed` must update every targeted row in a single
//! batched `UPDATE ... WHERE id IN (...)` (previously an N×(find + update) loop),
//! while leaving untargeted rows untouched and treating non-existent ids as no-ops.
//!
//! Runs on in-memory SQLite, so it needs no external database.
#![cfg(feature = "sqlite")]

use chrono::{TimeZone, Utc};
use cognee_database::ops::data::{create_data, get_data, update_last_accessed};
use cognee_database::{connect, initialize};
use cognee_models::Data;
use uuid::Uuid;

/// Insert a minimal `Data` row owned by `owner`, returning its id.
async fn seed_data(db: &cognee_database::DatabaseConnection, owner: Uuid) -> Uuid {
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
    .build();
    create_data(db, d).await.expect("create_data");
    id
}

#[tokio::test]
async fn update_last_accessed_batches_and_is_selective() {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("initialize");

    let owner = Uuid::new_v4();
    let id1 = seed_data(&db, owner).await;
    let id2 = seed_data(&db, owner).await;
    let untouched = seed_data(&db, owner).await;

    // Sanity: all rows start with no last_accessed.
    for id in [id1, id2, untouched] {
        let row = get_data(&db, id).await.expect("get_data").expect("exists");
        assert!(
            row.last_accessed.is_none(),
            "expected last_accessed to be unset initially"
        );
    }

    let ts = Utc.with_ymd_and_hms(2025, 1, 2, 3, 4, 5).unwrap();

    // Target id1 and id2, plus a non-existent id which must be a silent no-op.
    let missing = Uuid::new_v4();
    update_last_accessed(&db, &[id1, id2, missing], ts)
        .await
        .expect("update_last_accessed should succeed even with a non-existent id");

    // Targeted rows now carry the timestamp.
    for id in [id1, id2] {
        let row = get_data(&db, id).await.expect("get_data").expect("exists");
        assert_eq!(
            row.last_accessed,
            Some(ts),
            "targeted row {id} should have the new last_accessed"
        );
    }

    // The untargeted row is unchanged.
    let row = get_data(&db, untouched)
        .await
        .expect("get_data")
        .expect("exists");
    assert!(
        row.last_accessed.is_none(),
        "untargeted row must not be modified"
    );

    // The non-existent id was not inserted.
    assert!(
        get_data(&db, missing).await.expect("get_data").is_none(),
        "a non-existent id must remain absent (no upsert)"
    );
}

#[tokio::test]
async fn update_last_accessed_empty_is_noop() {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("initialize");

    // Empty batch must short-circuit without error.
    update_last_accessed(&db, &[], Utc::now())
        .await
        .expect("empty update_last_accessed is a no-op");
}
