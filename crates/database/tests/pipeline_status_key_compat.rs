#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Tests for the `Data.pipeline_status` dataset-key encoding.
//!
//! Python writes and clears the marker under `str(dataset.id)` — the dashed
//! 36-character form — while every other id Rust stores is the dashless
//! `uuid_hex::to_hex` form. On a database the two SDKs share, a clear that only
//! matched the hex form left every Python-written marker in place, so Python
//! went on skipping re-processing of data whose artifacts Rust had just
//! deleted.
//!
//! Rust now clears both forms, and `pipeline_status_dataset_key` is the single
//! place that says which form a future writer must use. Nothing in `crates/`
//! writes the column yet — the completion-marker writer arrives with run
//! orchestration.
//!
//! Runs on in-memory SQLite.
#![cfg(feature = "sqlite")]

use cognee_database::ops::data::{
    clear_cognify_pipeline_status_for_data, clear_pipeline_status_for_dataset, create_data,
    get_data, pipeline_status_dataset_key, update_data,
};
use cognee_database::ops::datasets::{attach_data_to_dataset, create_dataset};
use cognee_database::{DatabaseConnection, connect, initialize, uuid_hex};
use cognee_models::{Data, Dataset};
use uuid::Uuid;

/// A dataset with one attached data item and no `pipeline_status` yet.
async fn fixture() -> (DatabaseConnection, Uuid, Uuid) {
    let db = connect("sqlite::memory:").await.unwrap();
    initialize(&db).await.unwrap();

    let owner_id = Uuid::new_v4();
    let dataset_id = Uuid::new_v4();
    create_dataset(
        &db,
        Dataset::new("marker_ds".to_string(), owner_id, None, dataset_id),
    )
    .await
    .unwrap();

    let data_id = Uuid::new_v4();
    let data = Data::builder(
        data_id,
        "marker.txt",
        "file://marker.txt",
        "file://marker.txt",
        "txt",
        "text/plain",
        "hash_marker",
        owner_id,
    )
    .build();
    create_data(&db, data).await.unwrap();
    attach_data_to_dataset(&db, dataset_id, data_id)
        .await
        .unwrap();

    (db, dataset_id, data_id)
}

async fn set_pipeline_status(db: &DatabaseConnection, data_id: Uuid, status: serde_json::Value) {
    let stored = get_data(db, data_id).await.unwrap().unwrap();
    update_data(
        db,
        Data {
            pipeline_status: Some(status.to_string()),
            ..stored
        },
    )
    .await
    .unwrap();
}

async fn pipeline_status(db: &DatabaseConnection, data_id: Uuid) -> Option<serde_json::Value> {
    get_data(db, data_id)
        .await
        .unwrap()
        .unwrap()
        .pipeline_status
        .map(|json| serde_json::from_str(&json).unwrap())
}

#[test]
fn pipeline_status_dataset_key_is_pythons_dashed_form() {
    let dataset_id = Uuid::new_v4();
    assert_eq!(
        pipeline_status_dataset_key(dataset_id),
        dataset_id.to_string()
    );
    assert_ne!(
        pipeline_status_dataset_key(dataset_id),
        uuid_hex::to_hex(dataset_id),
        "the marker key is deliberately the one id in the schema that keeps its hyphens",
    );
}

#[tokio::test]
async fn clear_pipeline_status_for_dataset_clears_python_dashed_keys() {
    let (db, dataset_id, data_id) = fixture().await;
    set_pipeline_status(
        &db,
        data_id,
        serde_json::json!({
            "cognify_pipeline": {
                pipeline_status_dataset_key(dataset_id): "DATA_ITEM_PROCESSING_COMPLETED"
            }
        }),
    )
    .await;

    let updated = clear_pipeline_status_for_dataset(&db, dataset_id)
        .await
        .unwrap();

    assert_eq!(updated, 1, "the Python-written marker must be matched");
    assert!(
        pipeline_status(&db, data_id).await.is_none(),
        "clearing the only entry nulls the column out",
    );
}

#[tokio::test]
async fn clear_pipeline_status_for_dataset_still_clears_legacy_hex_keys() {
    let (db, dataset_id, data_id) = fixture().await;
    set_pipeline_status(
        &db,
        data_id,
        serde_json::json!({
            "cognify_pipeline": {
                uuid_hex::to_hex(dataset_id): "DATA_ITEM_PROCESSING_COMPLETED"
            }
        }),
    )
    .await;

    let updated = clear_pipeline_status_for_dataset(&db, dataset_id)
        .await
        .unwrap();

    assert_eq!(updated, 1, "markers Rust wrote before the fix still clear");
    assert!(pipeline_status(&db, data_id).await.is_none());
}

#[tokio::test]
async fn clear_pipeline_status_for_dataset_clears_both_forms_in_one_pass() {
    let (db, dataset_id, data_id) = fixture().await;
    let other_dataset_id = Uuid::new_v4();
    set_pipeline_status(
        &db,
        data_id,
        serde_json::json!({
            "cognify_pipeline": {
                dataset_id.to_string(): "DATA_ITEM_PROCESSING_COMPLETED",
                uuid_hex::to_hex(dataset_id): "DATA_ITEM_PROCESSING_COMPLETED",
            },
            "add_pipeline": {
                other_dataset_id.to_string(): "DATA_ITEM_PROCESSING_COMPLETED"
            }
        }),
    )
    .await;

    let updated = clear_pipeline_status_for_dataset(&db, dataset_id)
        .await
        .unwrap();

    assert_eq!(updated, 1);
    let remaining = pipeline_status(&db, data_id)
        .await
        .expect("an unrelated dataset's entry keeps the column alive");
    assert!(
        remaining.get("cognify_pipeline").is_none(),
        "both encodings go, so the now-empty pipeline entry is dropped: {remaining}",
    );
    assert_eq!(
        remaining["add_pipeline"][other_dataset_id.to_string()],
        serde_json::json!("DATA_ITEM_PROCESSING_COMPLETED"),
        "another dataset's marker is untouched",
    );
}

#[tokio::test]
async fn clear_cognify_pipeline_status_for_data_clears_python_dashed_keys() {
    let (db, dataset_id, data_id) = fixture().await;
    set_pipeline_status(
        &db,
        data_id,
        serde_json::json!({
            "cognify_pipeline": {
                dataset_id.to_string(): "DATA_ITEM_PROCESSING_COMPLETED"
            },
            "add_pipeline": {
                dataset_id.to_string(): "DATA_ITEM_PROCESSING_COMPLETED"
            }
        }),
    )
    .await;

    clear_cognify_pipeline_status_for_data(&db, data_id, dataset_id)
        .await
        .unwrap();

    let remaining = pipeline_status(&db, data_id)
        .await
        .expect("the add_pipeline entry survives");
    assert!(
        remaining.get("cognify_pipeline").is_none(),
        "the dashed cognify marker is cleared: {remaining}",
    );
    assert_eq!(
        remaining["add_pipeline"][dataset_id.to_string()],
        serde_json::json!("DATA_ITEM_PROCESSING_COMPLETED"),
        "`_forget_data_memory` parity: only the cognify pipeline is touched",
    );
}

#[tokio::test]
async fn clear_cognify_pipeline_status_for_data_clears_both_forms_in_one_pass() {
    let (db, dataset_id, data_id) = fixture().await;
    set_pipeline_status(
        &db,
        data_id,
        serde_json::json!({
            "cognify_pipeline": {
                dataset_id.to_string(): "DATA_ITEM_PROCESSING_COMPLETED",
                uuid_hex::to_hex(dataset_id): "DATA_ITEM_PROCESSING_COMPLETED",
            }
        }),
    )
    .await;

    clear_cognify_pipeline_status_for_data(&db, data_id, dataset_id)
        .await
        .unwrap();

    assert!(
        pipeline_status(&db, data_id).await.is_none(),
        "a row carrying both encodings is fully cleared in one pass",
    );
}
