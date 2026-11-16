#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! `cleanup_unused_data` (SDK-1048), mirroring Python's
//! `cognee/tests/integration/test_delete_edge_cases.py`: five documents,
//! three of them unused (two stale, one never accessed) and two recently
//! accessed. A dry run reports the three and deletes nothing; a real run
//! deletes exactly the three.

use std::sync::Arc;

use chrono::{Duration, Utc};
use cognee::add::{HashAlgorithm, build_add_pipeline};
use cognee::api::{
    CleanupParams, CleanupStatus, REASON_NOTHING_TRACKED, REASON_TRACKING_DISABLED,
    cleanup_unused_data,
};
use cognee::core::{NoopWatcher, Value, execute};
use cognee::database::{DatabaseConnection, DeleteDb, IngestDb, ops};
use cognee::delete::DeleteService;
use cognee::models::{Data, DataInput};
use cognee::storage::StorageTrait;
use cognee_test_utils::{FailingDeleteDb, MockStorage, test_task_context};
use uuid::Uuid;

const THRESHOLD_MINUTES: u64 = 60;

struct Fixture {
    db: Arc<DatabaseConnection>,
    service: DeleteService,
    storage: Arc<dyn StorageTrait>,
    ctx: Arc<cognee::core::TaskContext>,
    _handle: cognee::core::CancellationHandle,
}

async fn fixture() -> Fixture {
    let (handle, ctx, db) = test_task_context().await;
    let storage: Arc<dyn StorageTrait> = Arc::new(MockStorage::new());
    let service = DeleteService::new(Arc::clone(&storage), db.clone() as Arc<dyn DeleteDb>);
    Fixture {
        db,
        service,
        storage,
        ctx,
        _handle: handle,
    }
}

/// Add one text document to `dataset` as `owner` through the add pipeline.
async fn add_text(f: &Fixture, dataset: &str, text: &str, owner: Uuid) -> Data {
    let pipeline = build_add_pipeline(
        Arc::clone(&f.storage),
        f.db.clone() as Arc<dyn IngestDb>,
        HashAlgorithm::default(),
        dataset,
        owner,
        None,
    );
    let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(DataInput::Text(text.to_string()))];
    let results = execute(&pipeline, inputs, Arc::clone(&f.ctx), &NoopWatcher)
        .await
        .unwrap();
    (*results[0])
        .as_any()
        .downcast_ref::<Data>()
        .expect("the add pipeline returns Data")
        .clone()
}

/// Set the document's `last_accessed` to `minutes_ago` minutes ago.
async fn accessed(f: &Fixture, data: &Data, minutes_ago: i64) {
    ops::data::update_last_accessed(
        &f.db,
        &[data.id],
        Utc::now() - Duration::minutes(minutes_ago),
    )
    .await
    .unwrap();
}

async fn exists(f: &Fixture, data: &Data) -> bool {
    ops::data::get_data(&f.db, data.id).await.unwrap().is_some()
}

/// Five documents for `owner`: two stale, one never accessed, two recent.
/// Returns `(unused, recent)`.
async fn five_documents(f: &Fixture, owner: Uuid) -> (Vec<Data>, Vec<Data>) {
    let mut docs = Vec::new();
    for i in 0..5 {
        docs.push(add_text(f, "cleanup_ds", &format!("document number {i}"), owner).await);
    }
    accessed(f, &docs[0], 600).await;
    accessed(f, &docs[1], 120).await;
    // docs[2] is never accessed.
    accessed(f, &docs[3], 10).await;
    accessed(f, &docs[4], 1).await;
    let recent = docs.split_off(3);
    (docs, recent)
}

fn params(dry_run: bool, user_id: Option<Uuid>) -> CleanupParams {
    CleanupParams {
        minutes_threshold: THRESHOLD_MINUTES,
        dry_run,
        user_id,
    }
}

// Python `test_cleanup_unused_data_dry_run`.
#[tokio::test]
async fn a_dry_run_reports_the_unused_documents_and_deletes_nothing() {
    let f = fixture().await;
    let owner = Uuid::new_v4();
    let (unused, recent) = five_documents(&f, owner).await;

    let result = cleanup_unused_data(params(true, Some(owner)), true, f.db.as_ref(), &f.service)
        .await
        .unwrap();
    assert_eq!(result.status, CleanupStatus::DryRun);
    assert_eq!(result.unused_count, 3);
    assert_eq!(result.deleted_count.unwrap().documents, 0);
    assert_eq!(result.preview.map(|p| p.documents), Some(3));
    for doc in unused.iter().chain(&recent) {
        assert!(exists(&f, doc).await, "dry run deleted {}", doc.id);
    }
}

// Python `test_cleanup_unused_data_actual_deletion`.
#[tokio::test]
async fn a_real_run_deletes_only_the_unused_documents() {
    let f = fixture().await;
    let owner = Uuid::new_v4();
    let (unused, recent) = five_documents(&f, owner).await;

    let result = cleanup_unused_data(params(false, Some(owner)), true, f.db.as_ref(), &f.service)
        .await
        .unwrap();
    assert_eq!(result.status, CleanupStatus::Completed);
    assert_eq!(result.unused_count, 3);
    assert_eq!(result.deleted_count.unwrap().documents, 3);
    assert_eq!(result.deleted_count.unwrap().data_items, 3);
    assert!(result.preview.is_none());
    for doc in &unused {
        assert!(!exists(&f, doc).await, "unused {} survived", doc.id);
    }
    for doc in &recent {
        assert!(exists(&f, doc).await, "recent {} was deleted", doc.id);
    }
}

/// Access tracking off (`ENABLE_LAST_ACCESSED` unset): nothing runs.
#[tokio::test]
async fn the_run_is_skipped_while_tracking_is_off() {
    let f = fixture().await;
    let owner = Uuid::new_v4();
    let (unused, _) = five_documents(&f, owner).await;

    let result = cleanup_unused_data(params(false, None), false, f.db.as_ref(), &f.service)
        .await
        .unwrap();
    assert_eq!(result.status, CleanupStatus::Skipped);
    assert_eq!(result.reason.as_deref(), Some(REASON_TRACKING_DISABLED));
    assert_eq!(result.unused_count, 0);
    assert!(result.deleted_count.is_none());
    assert!(exists(&f, &unused[0]).await);
}

/// Tracking just switched on, nothing tracked yet: every document would be
/// "never accessed", so nothing runs.
#[tokio::test]
async fn the_run_is_skipped_before_anything_was_tracked() {
    let f = fixture().await;
    let owner = Uuid::new_v4();
    let doc = add_text(&f, "cleanup_ds", "never searched", owner).await;

    let result = cleanup_unused_data(params(false, None), true, f.db.as_ref(), &f.service)
        .await
        .unwrap();
    assert_eq!(result.status, CleanupStatus::Skipped);
    assert_eq!(result.reason.as_deref(), Some(REASON_NOTHING_TRACKED));
    assert!(exists(&f, &doc).await);
}

/// An operator task: with `user_id` it deletes that user's unused documents
/// (not the default user's, unlike Python), and without it every owner's.
#[tokio::test]
async fn the_user_filter_narrows_the_selection_to_that_users_datasets() {
    let f = fixture().await;
    let (alice, bob) = (Uuid::new_v4(), Uuid::new_v4());
    let alice_doc = add_text(&f, "alice_ds", "alice's stale notes", alice).await;
    let bob_doc = add_text(&f, "bob_ds", "bob's stale notes", bob).await;
    accessed(&f, &alice_doc, 600).await;
    accessed(&f, &bob_doc, 600).await;

    let result = cleanup_unused_data(params(false, Some(bob)), true, f.db.as_ref(), &f.service)
        .await
        .unwrap();
    assert_eq!(result.deleted_count.unwrap().documents, 1);
    assert!(exists(&f, &alice_doc).await);
    assert!(!exists(&f, &bob_doc).await);

    let result = cleanup_unused_data(params(false, None), true, f.db.as_ref(), &f.service)
        .await
        .unwrap();
    assert_eq!(result.deleted_count.unwrap().documents, 1);
    assert!(!exists(&f, &alice_doc).await);
}

/// The result serialises with Python's field names and status strings.
#[tokio::test]
async fn the_result_serialises_with_pythons_field_names() {
    let f = fixture().await;
    let result = cleanup_unused_data(params(true, None), false, f.db.as_ref(), &f.service)
        .await
        .unwrap();
    let json = serde_json::to_value(&result).unwrap();
    assert_eq!(json["status"], "skipped");
    assert_eq!(json["reason"], REASON_TRACKING_DISABLED);
    assert_eq!(json["unused_count"], 0);
    // Python returns `"deleted_count": {}` on a skipped run.
    assert_eq!(json["deleted_count"], serde_json::json!({}));
    assert!(json["cleanup_date"].is_string());
    assert!(json.get("preview").is_none());
}

/// A document whose delete fails is logged and skipped: the run still
/// completes, counts it as unused but not deleted, and deletes the others.
#[tokio::test]
async fn a_failed_delete_is_skipped_and_not_counted() {
    let f = fixture().await;
    let owner = Uuid::new_v4();
    let stale = add_text(&f, "cleanup_ds", "stale and deletable", owner).await;
    let stuck = add_text(&f, "cleanup_ds", "stale but stuck", owner).await;
    accessed(&f, &stale, 600).await;
    accessed(&f, &stuck, 600).await;

    let failing = Arc::new(
        FailingDeleteDb::new(f.db.clone() as Arc<dyn DeleteDb>, None)
            .with_failing_delete_data(stuck.id),
    );
    let service = DeleteService::new(Arc::clone(&f.storage), failing as Arc<dyn DeleteDb>);
    let result = cleanup_unused_data(params(false, Some(owner)), true, f.db.as_ref(), &service)
        .await
        .unwrap();
    assert_eq!(result.status, CleanupStatus::Completed);
    assert_eq!(result.unused_count, 2);
    assert_eq!(result.deleted_count.unwrap().documents, 1);
    assert!(!exists(&f, &stale).await);
    assert!(exists(&f, &stuck).await);
}

/// A document with no dataset cannot be deleted by dataset: a run without
/// `user_id` finds it unused, skips it, and leaves it in place. (With a
/// `user_id` the owner's-datasets filter never matches it, as Python's join.)
#[tokio::test]
async fn a_document_without_a_dataset_is_skipped() {
    let f = fixture().await;
    let owner = Uuid::new_v4();
    let tracked = add_text(&f, "cleanup_ds", "keeps tracking on", owner).await;
    accessed(&f, &tracked, 1).await;

    let orphan_id = Uuid::new_v4();
    let orphan = Data::builder(
        orphan_id,
        "orphan.txt",
        "file:///tmp/orphan",
        "file:///tmp/orphan",
        "txt",
        "text/plain",
        format!("hash-{orphan_id}"),
        owner,
    )
    .build();
    assert!(orphan.dataset_id.is_none());
    ops::data::create_data(&f.db, orphan.clone()).await.unwrap();
    accessed(&f, &orphan, 600).await;

    let result = cleanup_unused_data(params(false, None), true, f.db.as_ref(), &f.service)
        .await
        .unwrap();
    assert_eq!(result.status, CleanupStatus::Completed);
    assert_eq!(result.unused_count, 1);
    assert_eq!(result.deleted_count.unwrap().documents, 0);
    assert!(exists(&f, &orphan).await);
    assert!(exists(&f, &tracked).await);
}
