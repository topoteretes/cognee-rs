#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Round-trip tests for the new `pipeline_run_payload_fields` table and its
//! `set_payload_field` / `get_payload` repository methods (LIB-06).

use std::sync::Arc;

use cognee_database::{
    DatabaseConnection, PipelineRunRepository, SeaOrmPipelineRunRepository, connect, initialize,
};
use uuid::Uuid;

async fn make_db() -> Arc<DatabaseConnection> {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("init");
    Arc::new(db)
}

fn make_repo(db: Arc<DatabaseConnection>) -> Arc<SeaOrmPipelineRunRepository> {
    Arc::new(SeaOrmPipelineRunRepository::new(db))
}

// ---------------------------------------------------------------------------
// 1. set_payload_field inserts a new row that get_payload reads back
// ---------------------------------------------------------------------------

#[tokio::test]
async fn set_payload_field_inserts_new_row() {
    let db = make_db().await;
    let repo = make_repo(db);
    let run_id = Uuid::new_v4();

    repo.set_payload_field(run_id, "items_processed", serde_json::json!(7))
        .await
        .expect("set_payload_field");

    let payload = repo.get_payload(run_id).await.expect("get_payload");
    assert_eq!(payload.len(), 1);
    assert_eq!(payload.get("items_processed"), Some(&serde_json::json!(7)));
}

// ---------------------------------------------------------------------------
// 2. set_payload_field upserts an existing key (last-write-wins per row)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn set_payload_field_upserts_existing_key() {
    let db = make_db().await;
    let repo = make_repo(db);
    let run_id = Uuid::new_v4();

    repo.set_payload_field(run_id, "k", serde_json::json!("first"))
        .await
        .expect("first set");
    repo.set_payload_field(run_id, "k", serde_json::json!("second"))
        .await
        .expect("second set");

    let payload = repo.get_payload(run_id).await.expect("get_payload");
    assert_eq!(payload.len(), 1, "upsert should not create a second row");
    assert_eq!(payload.get("k"), Some(&serde_json::json!("second")));
}

// ---------------------------------------------------------------------------
// 3. Concurrent set_payload_field calls with distinct keys all succeed
// ---------------------------------------------------------------------------

#[tokio::test]
async fn set_payload_field_concurrent_different_keys_succeeds() {
    let db = make_db().await;
    let repo = make_repo(db);
    let run_id = Uuid::new_v4();

    let mut handles = Vec::with_capacity(8);
    for i in 0..8 {
        let repo = Arc::clone(&repo);
        handles.push(tokio::spawn(async move {
            let key = format!("key_{i}");
            repo.set_payload_field(run_id, &key, serde_json::json!(i))
                .await
                .expect("set_payload_field in concurrent task");
        }));
    }

    for h in handles {
        h.await.expect("task join");
    }

    let payload = repo.get_payload(run_id).await.expect("get_payload");
    assert_eq!(
        payload.len(),
        8,
        "expected 8 distinct keys, got {payload:?}"
    );
    for i in 0..8 {
        let key = format!("key_{i}");
        assert_eq!(
            payload.get(&key),
            Some(&serde_json::json!(i)),
            "expected {key} = {i}"
        );
    }
}

// ---------------------------------------------------------------------------
// 4. get_payload returns an empty map for an unknown run id
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_payload_returns_empty_map_for_unknown_run() {
    let db = make_db().await;
    let repo = make_repo(db);

    let payload = repo
        .get_payload(Uuid::new_v4())
        .await
        .expect("get_payload should not error on unknown run");
    assert!(
        payload.is_empty(),
        "expected empty map, got {} entries",
        payload.len()
    );
}
