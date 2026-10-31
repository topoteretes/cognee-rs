#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! `update_data_node_set` is the ingest pipeline's re-add refresh (SDK-1129).
//! It must write `node_set`, `external_metadata` and `updated_at` only: the
//! full-row `update_data(Data)` it replaced wrote every column from the
//! pipeline's earlier read, rolling back a `token_count`, `last_accessed` or
//! `pipeline_status` written in between.
//!
//! `update_data_stored_text`, the re-add repair of a row whose stored text is
//! gone, must likewise write only its own columns.
//!
//! Runs on in-memory SQLite, so it needs no external database.
#![cfg(feature = "sqlite")]

use chrono::{TimeZone, Utc};
use cognee_database::ops::data::{
    DATA_ITEM_PROCESSING_COMPLETED, create_data, get_data, mark_cognify_pipeline_status_complete,
    pipeline_status_dataset_key, update_data_node_set, update_data_stored_text,
    update_data_token_count, update_last_accessed,
};
use cognee_database::{DatabaseError, connect, initialize};
use cognee_models::Data;
use uuid::Uuid;

async fn seed_data(db: &cognee_database::DatabaseConnection) -> Uuid {
    let id = Uuid::new_v4();
    let d = Data::builder(
        id,
        format!("data-{id}"),
        "file:///tmp/raw",
        "file:///tmp/raw",
        "txt",
        "text/plain",
        format!("hash-{id}"),
        Uuid::new_v4(),
    )
    .external_metadata(r#"{"source":"crm"}"#)
    .build();
    create_data(db, d).await.expect("create_data");
    id
}

#[tokio::test]
async fn update_data_node_set_touches_only_its_columns() {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("initialize");
    let id = seed_data(&db).await;
    let dataset_id = Uuid::new_v4();

    // Columns other writers own, written before the refresh.
    update_data_token_count(&db, id, 42).await.unwrap();
    let accessed = Utc.with_ymd_and_hms(2025, 1, 2, 3, 4, 5).unwrap();
    update_last_accessed(&db, &[id], accessed).await.unwrap();
    mark_cognify_pipeline_status_complete(&db, id, dataset_id)
        .await
        .unwrap();

    let written = update_data_node_set(
        &db,
        id,
        Some(r#"["alpha"]"#),
        Some(r#"{"source":"crm","node_set":["alpha"]}"#),
    )
    .await
    .expect("refresh");

    let row = get_data(&db, id).await.unwrap().expect("row exists");
    assert_eq!(row.node_set.as_deref(), Some(r#"["alpha"]"#));
    assert_eq!(
        row.external_metadata.as_deref(),
        Some(r#"{"source":"crm","node_set":["alpha"]}"#)
    );
    assert_eq!(
        row.updated_at,
        Some(written),
        "updated_at is stamped with exactly the value the call returned, so a \
         caller can patch its snapshot instead of reading the row back"
    );
    assert_eq!(row.token_count, 42, "token_count survives the refresh");
    assert_eq!(row.last_accessed, Some(accessed), "last_accessed survives");
    let status: serde_json::Value =
        serde_json::from_str(row.pipeline_status.as_deref().expect("marker kept")).unwrap();
    assert_eq!(
        status["cognify_pipeline"][pipeline_status_dataset_key(dataset_id)],
        serde_json::json!(DATA_ITEM_PROCESSING_COMPLETED),
        "the cognify completion marker survives"
    );

    // Clearing both columns is a valid refresh too.
    update_data_node_set(&db, id, None, None).await.unwrap();
    let row = get_data(&db, id).await.unwrap().unwrap();
    assert_eq!(row.node_set, None);
    assert_eq!(row.external_metadata, None);
    assert_eq!(row.token_count, 42);
}

#[tokio::test]
async fn update_data_node_set_on_a_missing_row_is_not_found() {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("initialize");

    let err = update_data_node_set(&db, Uuid::new_v4(), Some("[\"a\"]"), None)
        .await
        .expect_err("no row");
    assert!(matches!(err, DatabaseError::NotFound(_)), "got {err:?}");
}

#[tokio::test]
async fn update_data_stored_text_touches_only_its_columns() {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("initialize");
    let id = seed_data(&db).await;
    let dataset_id = Uuid::new_v4();
    update_data_token_count(&db, id, 42).await.unwrap();
    let accessed = Utc.with_ymd_and_hms(2025, 1, 2, 3, 4, 5).unwrap();
    update_last_accessed(&db, &[id], accessed).await.unwrap();
    mark_cognify_pipeline_status_complete(&db, id, dataset_id)
        .await
        .unwrap();
    update_data_node_set(&db, id, Some(r#"["alpha"]"#), None)
        .await
        .unwrap();
    let before = get_data(&db, id).await.unwrap().unwrap();

    let written = update_data_stored_text(
        &db,
        id,
        "file:///tmp/new/text_x.txt",
        "new-hash",
        "txt",
        "text/plain",
        "audio_loader",
    )
    .await
    .expect("repair");

    let row = get_data(&db, id).await.unwrap().expect("row exists");
    assert_eq!(row.raw_data_location, "file:///tmp/new/text_x.txt");
    assert_eq!(row.raw_content_hash.as_deref(), Some("new-hash"));
    assert_eq!(row.extension, "txt");
    assert_eq!(row.mime_type, "text/plain");
    assert_eq!(row.loader_engine.as_deref(), Some("audio_loader"));
    assert_eq!(row.updated_at, Some(written));
    assert_eq!(row.token_count, 42, "token_count survives the repair");
    assert_eq!(row.last_accessed, Some(accessed), "last_accessed survives");
    assert_eq!(
        row.node_set.as_deref(),
        Some(r#"["alpha"]"#),
        "node_set survives"
    );
    assert_eq!(
        row.pipeline_status, before.pipeline_status,
        "the cognify marker survives"
    );
    assert_eq!(
        row.content_hash, before.content_hash,
        "content_hash is unchanged"
    );
    assert_eq!(row.original_data_location, before.original_data_location);
}

#[tokio::test]
async fn update_data_stored_text_on_a_missing_row_is_not_found() {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("initialize");

    let err = update_data_stored_text(&db, Uuid::new_v4(), "x", "h", "txt", "text/plain", "e")
        .await
        .expect_err("no row");
    assert!(matches!(err, DatabaseError::NotFound(_)), "got {err:?}");
}
