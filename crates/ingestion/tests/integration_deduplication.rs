//! Integration tests for content-addressed deduplication in `AddPipeline`.
//!
//! These tests exercise the full SQLite + LocalStorage stack (no mocks) to verify
//! that the pipeline deduplicates correctly under various scenarios matching the
//! Python `test_deduplication.py` E2E test.

use cognee_database::{DeleteDb, IngestDb, connect, initialize, ops};
use cognee_delete::{DeleteMode, DeleteRequest, DeleteScope, DeleteService};
use cognee_ingestion::AddPipeline;
use cognee_models::DataInput;
use cognee_storage::{LocalStorage, StorageTrait};
use std::io::Write;
use std::sync::Arc;
use tempfile::{NamedTempFile, TempDir};
use uuid::Uuid;

const NLP_TEXT: &str = include_str!("test_data/natural_language_processing.txt");
const QUANTUM_TEXT: &str = include_str!("test_data/quantum_computers.txt");

/// Build a fresh `AddPipeline` backed by a real SQLite database and
/// `LocalStorage` inside `dir`. Returns the pipeline plus a shared database
/// handle for post-test assertions.
async fn make_pipeline(
    dir: &TempDir,
) -> (
    AddPipeline,
    Arc<cognee_database::DatabaseConnection>,
    Arc<LocalStorage>,
) {
    let db_path = dir.path().join("cognee.db");
    std::fs::File::create(&db_path).expect("sqlite db file should be created");
    let db_url = format!("sqlite://{}", db_path.display());

    let db = connect(&db_url).await.expect("connect");
    initialize(&db).await.expect("initialize");
    let db = Arc::new(db);

    let storage = Arc::new(LocalStorage::new(dir.path().join("storage")));
    storage.initialize().await.expect("storage.initialize");

    let pipeline = AddPipeline::new(
        storage.clone() as Arc<dyn StorageTrait>,
        db.clone() as Arc<dyn IngestDb>,
    );
    (pipeline, db, storage)
}

// ---------------------------------------------------------------------------
// Sub-test A — File deduplication (identical content, different filenames)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn file_deduplication_same_content_yields_one_record() {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, database, _storage) = make_pipeline(&dir).await;
    let owner = Uuid::new_v4();

    // Two temp files with identical content
    let mut file1 = NamedTempFile::new().expect("tmp file 1");
    file1.write_all(NLP_TEXT.as_bytes()).expect("write file 1");
    let path1 = file1.path().to_str().unwrap().to_string();

    let mut file2 = NamedTempFile::new().expect("tmp file 2");
    file2.write_all(NLP_TEXT.as_bytes()).expect("write file 2");
    let path2 = file2.path().to_str().unwrap().to_string();

    let result1 = pipeline
        .add(vec![DataInput::FilePath(path1)], "dataset1", owner, None)
        .await
        .expect("add file 1");
    let result2 = pipeline
        .add(vec![DataInput::FilePath(path2)], "dataset2", owner, None)
        .await
        .expect("add file 2");

    // Same content + same owner → same data_id
    assert_eq!(
        result1[0].id, result2[0].id,
        "identical content should yield the same data_id"
    );

    // Only one Data record in the database
    let all_data_count = ops::datasets::list_datasets(&database)
        .await
        .expect("list datasets")
        .len();
    assert_eq!(all_data_count, 2, "should have 2 datasets");

    let ds1 = ops::datasets::get_dataset_by_name(&database, "dataset1", owner, None)
        .await
        .expect("get ds1")
        .expect("ds1 should exist");
    let ds2 = ops::datasets::get_dataset_by_name(&database, "dataset2", owner, None)
        .await
        .expect("get ds2")
        .expect("ds2 should exist");

    let ds1_data = ops::datasets::get_dataset_data(&database, ds1.id)
        .await
        .expect("ds1 data");
    let ds2_data = ops::datasets::get_dataset_data(&database, ds2.id)
        .await
        .expect("ds2 data");

    assert_eq!(ds1_data.len(), 1);
    assert_eq!(ds2_data.len(), 1);
    // Both datasets reference the same underlying data record
    assert_eq!(
        ds1_data[0].id, ds2_data[0].id,
        "both datasets should reference the same data_id"
    );
}

// ---------------------------------------------------------------------------
// Sub-test B — Inline text deduplication across two add() calls
// ---------------------------------------------------------------------------

#[tokio::test]
async fn text_deduplication_across_two_calls() {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, database, _storage) = make_pipeline(&dir).await;
    let owner = Uuid::new_v4();

    let result1 = pipeline
        .add(
            vec![DataInput::Text(QUANTUM_TEXT.to_string())],
            "dataset1",
            owner,
            None,
        )
        .await
        .expect("add text 1");
    let result2 = pipeline
        .add(
            vec![DataInput::Text(QUANTUM_TEXT.to_string())],
            "dataset2",
            owner,
            None,
        )
        .await
        .expect("add text 2");

    assert_eq!(
        result1[0].id, result2[0].id,
        "same text should deduplicate to the same data_id"
    );
    // Name for text inputs is text_<md5_hash>
    assert!(
        result1[0].name.starts_with("text_"),
        "name should start with text_"
    );
    assert_eq!(result1[0].name, result2[0].name, "same content → same name");

    // Both datasets reference the same data record
    let ds1 = ops::datasets::get_dataset_by_name(&database, "dataset1", owner, None)
        .await
        .unwrap()
        .unwrap();
    let ds2 = ops::datasets::get_dataset_by_name(&database, "dataset2", owner, None)
        .await
        .unwrap()
        .unwrap();

    let link_count = ops::data::count_data_dataset_links(&database, result1[0].id)
        .await
        .expect("count links");
    assert_eq!(link_count, 2, "data should be linked to 2 datasets");

    let ds1_data = ops::datasets::get_dataset_data(&database, ds1.id)
        .await
        .unwrap();
    let ds2_data = ops::datasets::get_dataset_data(&database, ds2.id)
        .await
        .unwrap();
    assert_eq!(ds1_data[0].id, ds2_data[0].id);
}

// ---------------------------------------------------------------------------
// Sub-test C — Cross-owner isolation (same content, different owners)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cross_owner_isolation_same_content_different_owners() {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, database, _storage) = make_pipeline(&dir).await;
    let owner1 = Uuid::new_v4();
    let owner2 = Uuid::new_v4();

    let result1 = pipeline
        .add(
            vec![DataInput::Text(NLP_TEXT.to_string())],
            "dataset1",
            owner1,
            None,
        )
        .await
        .expect("add owner1");
    let result2 = pipeline
        .add(
            vec![DataInput::Text(NLP_TEXT.to_string())],
            "dataset2",
            owner2,
            None,
        )
        .await
        .expect("add owner2");

    // Different owners → different data_ids (owner_id is mixed into UUID5 seed)
    assert_ne!(
        result1[0].id, result2[0].id,
        "different owners should produce different data_ids"
    );
    // Content hash is content-only (Python compatible), so same content → same hash
    assert_eq!(
        result1[0].content_hash, result2[0].content_hash,
        "content hash is owner-independent (Python compat)"
    );

    assert_eq!(result1[0].owner_id, owner1);
    assert_eq!(result2[0].owner_id, owner2);

    // Each owner has exactly one dataset and one data record
    let owner1_datasets = ops::datasets::list_datasets_by_owner(&database, owner1)
        .await
        .expect("list owner1 datasets");
    let owner2_datasets = ops::datasets::list_datasets_by_owner(&database, owner2)
        .await
        .expect("list owner2 datasets");

    assert_eq!(owner1_datasets.len(), 1);
    assert_eq!(owner2_datasets.len(), 1);

    let ds1_data = ops::datasets::get_dataset_data(&database, owner1_datasets[0].id)
        .await
        .unwrap();
    let ds2_data = ops::datasets::get_dataset_data(&database, owner2_datasets[0].id)
        .await
        .unwrap();

    assert_eq!(ds1_data[0].owner_id, owner1);
    assert_eq!(ds2_data[0].owner_id, owner2);
}

// ---------------------------------------------------------------------------
// Sub-test D — Binary file deduplication
// ---------------------------------------------------------------------------

#[tokio::test]
async fn binary_file_deduplication() {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, database, _storage) = make_pipeline(&dir).await;
    let owner = Uuid::new_v4();

    // Identical binary bytes — PNG-like magic bytes
    let binary_content: &[u8] = &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0xFF];

    let mut bin1 = NamedTempFile::new().expect("bin tmp 1");
    bin1.write_all(binary_content).expect("write bin 1");
    let bin1_path = bin1.path().to_str().unwrap().to_string();

    let mut bin2 = NamedTempFile::new().expect("bin tmp 2");
    bin2.write_all(binary_content).expect("write bin 2");
    let bin2_path = bin2.path().to_str().unwrap().to_string();

    let result1 = pipeline
        .add(
            vec![DataInput::FilePath(bin1_path)],
            "bin_dataset1",
            owner,
            None,
        )
        .await
        .expect("add bin 1");
    let result2 = pipeline
        .add(
            vec![DataInput::FilePath(bin2_path)],
            "bin_dataset2",
            owner,
            None,
        )
        .await
        .expect("add bin 2");

    // Deduplication is MIME-type agnostic — only content + owner matter
    assert_eq!(
        result1[0].id, result2[0].id,
        "identical binary content should deduplicate"
    );

    let all_datasets = ops::datasets::list_datasets(&database)
        .await
        .expect("list datasets");
    assert_eq!(all_datasets.len(), 2);

    let link_count = ops::data::count_data_dataset_links(&database, result1[0].id)
        .await
        .expect("count links");
    assert_eq!(
        link_count, 2,
        "binary data should be linked to both datasets"
    );
}

// ---------------------------------------------------------------------------
// Sub-test E — Dataset link counting and cascade deletion
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cascade_deletion_preserves_data_with_remaining_links() {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, database, storage) = make_pipeline(&dir).await;
    let owner = Uuid::new_v4();

    // Add the same content to 3 datasets
    let r1 = pipeline
        .add(
            vec![DataInput::Text(QUANTUM_TEXT.to_string())],
            "ds1",
            owner,
            None,
        )
        .await
        .expect("add ds1");
    pipeline
        .add(
            vec![DataInput::Text(QUANTUM_TEXT.to_string())],
            "ds2",
            owner,
            None,
        )
        .await
        .expect("add ds2");
    pipeline
        .add(
            vec![DataInput::Text(QUANTUM_TEXT.to_string())],
            "ds3",
            owner,
            None,
        )
        .await
        .expect("add ds3");

    let data_id = r1[0].id;

    // All three datasets reference the same data record
    let link_count = ops::data::count_data_dataset_links(&database, data_id)
        .await
        .expect("count links before delete");
    assert_eq!(link_count, 3, "data should be linked to 3 datasets");

    let delete_svc = DeleteService::new(
        storage.clone() as Arc<dyn StorageTrait>,
        database.clone() as Arc<dyn DeleteDb>,
    );

    // Delete dataset1 — data still has 2 remaining links and must NOT be deleted
    let result1 = delete_svc
        .execute(&DeleteRequest {
            scope: DeleteScope::Dataset {
                owner_id: owner,
                dataset_name: "ds1".to_string(),
            },
            mode: DeleteMode::Soft,
        })
        .await
        .expect("delete ds1");

    assert_eq!(result1.deleted_datasets, 1);
    assert_eq!(
        result1.deleted_data, 0,
        "data should not be deleted while still linked to other datasets"
    );

    // Data record must still exist
    let data_still_exists = ops::data::get_data(&database, data_id)
        .await
        .expect("get data after ds1 delete");
    assert!(
        data_still_exists.is_some(),
        "data record should survive deletion of one dataset"
    );

    let remaining_links = ops::data::count_data_dataset_links(&database, data_id)
        .await
        .expect("count links after ds1 delete");
    assert_eq!(remaining_links, 2);

    // Delete dataset2 — data still has 1 remaining link
    let result2 = delete_svc
        .execute(&DeleteRequest {
            scope: DeleteScope::Dataset {
                owner_id: owner,
                dataset_name: "ds2".to_string(),
            },
            mode: DeleteMode::Soft,
        })
        .await
        .expect("delete ds2");

    assert_eq!(result2.deleted_datasets, 1);
    assert_eq!(result2.deleted_data, 0, "data still linked to ds3");

    // Delete dataset3 — last link removed; data record should be deleted
    let result3 = delete_svc
        .execute(&DeleteRequest {
            scope: DeleteScope::Dataset {
                owner_id: owner,
                dataset_name: "ds3".to_string(),
            },
            mode: DeleteMode::Soft,
        })
        .await
        .expect("delete ds3");

    assert_eq!(result3.deleted_datasets, 1);
    assert_eq!(
        result3.deleted_data, 1,
        "data record should be deleted when last link is removed"
    );

    let data_gone = ops::data::get_data(&database, data_id)
        .await
        .expect("get data after all deletes");
    assert!(
        data_gone.is_none(),
        "data record should be gone after all links removed"
    );
}

// ---------------------------------------------------------------------------
// Sub-test F — Tenant isolation
// ---------------------------------------------------------------------------

/// Same owner, same content, but different tenant_id → different data IDs.
/// Content hash must be identical (owner-independent hashing).
#[tokio::test]
async fn same_owner_different_tenants_creates_separate_data_records() {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, _db, _storage) = make_pipeline(&dir).await;

    let owner = Uuid::new_v4();
    let tenant1 = Some(Uuid::new_v4());
    let tenant2 = Some(Uuid::new_v4());

    let r1 = pipeline
        .add(
            vec![DataInput::Text(NLP_TEXT.to_string())],
            "ds_tenant1",
            owner,
            tenant1,
        )
        .await
        .expect("add with tenant1");

    let r2 = pipeline
        .add(
            vec![DataInput::Text(NLP_TEXT.to_string())],
            "ds_tenant2",
            owner,
            tenant2,
        )
        .await
        .expect("add with tenant2");

    assert_eq!(r1.len(), 1);
    assert_eq!(r2.len(), 1);

    assert_ne!(
        r1[0].id, r2[0].id,
        "different tenants must produce different data IDs"
    );
    assert_eq!(
        r1[0].content_hash, r2[0].content_hash,
        "content hash is tenant-independent"
    );
    assert_eq!(r1[0].tenant_id, tenant1);
    assert_eq!(r2[0].tenant_id, tenant2);
}

/// Two owners × two tenants each create the same dataset name → four distinct dataset IDs.
#[tokio::test]
async fn datasets_isolated_by_owner_and_tenant() {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, _db, _storage) = make_pipeline(&dir).await;

    let owner1 = Uuid::new_v4();
    let owner2 = Uuid::new_v4();
    let tenant1 = Some(Uuid::new_v4());
    let tenant2 = Some(Uuid::new_v4());

    let combos = [
        (owner1, tenant1, "o1t1"),
        (owner1, tenant2, "o1t2"),
        (owner2, tenant1, "o2t1"),
        (owner2, tenant2, "o2t2"),
    ];

    let mut dataset_ids = Vec::new();
    for (owner, tenant, label) in combos {
        let result = pipeline
            .add(
                vec![DataInput::Text(format!("content for {label}"))],
                "shared_dataset_name",
                owner,
                tenant,
            )
            .await
            .unwrap_or_else(|e| panic!("add {label}: {e}"));
        dataset_ids.push(result[0].id);
    }

    // All four data IDs must be distinct because owner and tenant differ
    let unique: std::collections::HashSet<_> = dataset_ids.iter().collect();
    assert_eq!(
        unique.len(),
        4,
        "each owner+tenant combination must produce a distinct data ID"
    );
}
