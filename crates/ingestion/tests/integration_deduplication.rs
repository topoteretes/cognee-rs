#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for content-addressed deduplication in `AddPipeline`.
//!
//! These tests exercise the full SQLite + LocalStorage stack (no mocks) to verify
//! that the pipeline deduplicates correctly under various scenarios matching the
//! Python `test_deduplication.py` E2E test.
//!
//! Each test is instantiated twice: once with SQLite and once with PostgreSQL.
//! The PostgreSQL variant is skipped automatically when `DB_PROVIDER` is not
//! set to `"postgres"` in the environment.

use cognee_core::RayonThreadPool;
use cognee_database::{DeleteDb, IngestDb, connect, initialize, ops};
use cognee_delete::{DeleteMode, DeleteRequest, DeleteScope, DeleteService};
use cognee_graph::MockGraphDB;
use cognee_ingestion::AddPipeline;
use cognee_models::DataInput;
use cognee_storage::{LocalStorage, StorageTrait};
use cognee_vector::MockVectorDB;
use std::io::Write;
use std::sync::Arc;
use tempfile::{NamedTempFile, TempDir};
use uuid::Uuid;

const NLP_TEXT: &str = include_str!("test_data/natural_language_processing.txt");
const QUANTUM_TEXT: &str = include_str!("test_data/quantum_computers.txt");

/// Build a SQLite URL backed by a file inside `dir`, creating the file first.
fn sqlite_db_url(dir: &TempDir) -> String {
    let db_path = dir.path().join("cognee.db");
    std::fs::File::create(&db_path).expect("sqlite db file should be created");
    format!("sqlite://{}", db_path.display())
}

/// Build a fresh `AddPipeline` backed by the given database URL and
/// `LocalStorage` inside `dir`. Returns the pipeline plus a shared database
/// handle for post-test assertions.
async fn make_pipeline(
    dir: &TempDir,
    db_url: &str,
) -> (
    AddPipeline,
    Arc<cognee_database::DatabaseConnection>,
    Arc<LocalStorage>,
) {
    let db = connect(db_url).await.expect("connect");
    initialize(&db).await.expect("initialize");
    let db = Arc::new(db);

    let storage = Arc::new(LocalStorage::new(dir.path().join("storage")));
    storage.initialize().await.expect("storage.initialize");

    let pipeline = AddPipeline::new(
        storage.clone() as Arc<dyn StorageTrait>,
        db.clone() as Arc<dyn IngestDb>,
    )
    .with_thread_pool(Arc::new(RayonThreadPool::with_default_threads().unwrap()))
    .with_graph_db(Arc::new(MockGraphDB::new()))
    .with_vector_db(Arc::new(MockVectorDB::new()))
    .with_database(db.clone());
    (pipeline, db, storage)
}

// ---------------------------------------------------------------------------
// Sub-test A — Identical file content in two datasets: two dataset-scoped rows
// ---------------------------------------------------------------------------

async fn impl_file_same_content_in_two_datasets_yields_two_records(db_url: &str) {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, database, _storage) = make_pipeline(&dir, db_url).await;
    let owner = Uuid::new_v4();

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

    // Rows are dataset-scoped (Python `ingest_data`): another dataset gets
    // its own row and uuid4 id; only the content hash is shared.
    assert_ne!(
        result1[0].id, result2[0].id,
        "identical content in another dataset is another row"
    );
    assert_eq!(result1[0].content_hash, result2[0].content_hash);

    let all_data_count = ops::datasets::list_datasets_by_owner(&database, owner)
        .await
        .expect("list datasets")
        .len();
    assert_eq!(all_data_count, 2, "should have 2 datasets");

    let ds1 = ops::datasets::get_dataset_by_name_in_tenant(&database, "dataset1", owner, None)
        .await
        .expect("get ds1")
        .expect("ds1 should exist");
    let ds2 = ops::datasets::get_dataset_by_name_in_tenant(&database, "dataset2", owner, None)
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
    assert_eq!(ds1_data[0].id, result1[0].id);
    assert_eq!(ds2_data[0].id, result2[0].id);
}

#[tokio::test]
async fn file_same_content_in_two_datasets_yields_two_records_sqlite() {
    let dir = TempDir::new().expect("tempdir for url");
    let url = sqlite_db_url(&dir);
    impl_file_same_content_in_two_datasets_yields_two_records(&url).await;
}

#[tokio::test]
async fn file_same_content_in_two_datasets_yields_two_records_pg() {
    let Some(url) = cognee_test_utils::pg_test_url() else {
        return;
    };
    impl_file_same_content_in_two_datasets_yields_two_records(&url).await;
}

// ---------------------------------------------------------------------------
// Sub-test B — Inline text added to two datasets across two add() calls
// ---------------------------------------------------------------------------

async fn impl_text_deduplication_across_two_calls(db_url: &str) {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, database, _storage) = make_pipeline(&dir, db_url).await;
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

    assert_ne!(
        result1[0].id, result2[0].id,
        "the same text in another dataset is another row"
    );
    assert!(
        result1[0].name.starts_with("text_"),
        "name should start with text_"
    );
    assert_eq!(result1[0].name, result2[0].name, "same content → same name");

    let ds1 = ops::datasets::get_dataset_by_name_in_tenant(&database, "dataset1", owner, None)
        .await
        .unwrap()
        .unwrap();
    let ds2 = ops::datasets::get_dataset_by_name_in_tenant(&database, "dataset2", owner, None)
        .await
        .unwrap()
        .unwrap();

    for row in [&result1[0], &result2[0]] {
        let link_count = ops::data::count_data_dataset_links(&database, row.id)
            .await
            .expect("count links");
        assert_eq!(link_count, 1, "a row belongs to exactly one dataset");
    }

    let ds1_data = ops::datasets::get_dataset_data(&database, ds1.id)
        .await
        .unwrap();
    let ds2_data = ops::datasets::get_dataset_data(&database, ds2.id)
        .await
        .unwrap();
    assert_eq!(ds1_data[0].id, result1[0].id);
    assert_eq!(ds2_data[0].id, result2[0].id);
}

#[tokio::test]
async fn text_deduplication_across_two_calls_sqlite() {
    let dir = TempDir::new().expect("tempdir for url");
    let url = sqlite_db_url(&dir);
    impl_text_deduplication_across_two_calls(&url).await;
}

#[tokio::test]
async fn text_deduplication_across_two_calls_pg() {
    let Some(url) = cognee_test_utils::pg_test_url() else {
        return;
    };
    impl_text_deduplication_across_two_calls(&url).await;
}

// ---------------------------------------------------------------------------
// Sub-test C — Cross-owner isolation (same content, different owners)
// ---------------------------------------------------------------------------

async fn impl_cross_owner_isolation_same_content_different_owners(db_url: &str) {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, database, _storage) = make_pipeline(&dir, db_url).await;
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

    assert_ne!(
        result1[0].id, result2[0].id,
        "different owners should produce different data_ids"
    );
    assert_eq!(
        result1[0].content_hash, result2[0].content_hash,
        "content hash is owner-independent (Python compat)"
    );

    assert_eq!(result1[0].owner_id, owner1);
    assert_eq!(result2[0].owner_id, owner2);

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

#[tokio::test]
async fn cross_owner_isolation_same_content_different_owners_sqlite() {
    let dir = TempDir::new().expect("tempdir for url");
    let url = sqlite_db_url(&dir);
    impl_cross_owner_isolation_same_content_different_owners(&url).await;
}

#[tokio::test]
async fn cross_owner_isolation_same_content_different_owners_pg() {
    let Some(url) = cognee_test_utils::pg_test_url() else {
        return;
    };
    impl_cross_owner_isolation_same_content_different_owners(&url).await;
}

// ---------------------------------------------------------------------------
// Sub-test D — Binary file content in two datasets
// ---------------------------------------------------------------------------
// Sub-test E — Deleting a dataset deletes only its own row
// ---------------------------------------------------------------------------

/// The same content in three datasets is three rows. Deleting a dataset
/// deletes its own row and leaves the other datasets' rows and their stored
/// files alone. (`LocalStorage` names every stored file uniquely; the shared
/// content-addressed file case is covered by the delete crate's
/// `shared_file_is_kept_until_its_last_row_is_deleted`.)
async fn impl_cascade_deletion_preserves_data_with_remaining_links(db_url: &str) {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, database, storage) = make_pipeline(&dir, db_url).await;
    let owner = Uuid::new_v4();

    let mut rows = Vec::new();
    for name in ["ds1", "ds2", "ds3"] {
        let added = pipeline
            .add(
                vec![DataInput::Text(QUANTUM_TEXT.to_string())],
                name,
                owner,
                None,
            )
            .await
            .expect("add");
        rows.push(added[0].clone());
    }

    let delete_svc = DeleteService::new(
        storage.clone() as Arc<dyn StorageTrait>,
        database.clone() as Arc<dyn DeleteDb>,
    );
    let delete = |name: &str| DeleteRequest {
        scope: DeleteScope::Dataset {
            owner_id: owner,
            dataset_name: name.to_string(),
            tenant_id: None,
        },
        mode: DeleteMode::Soft,
        memory_only: false,
    };

    for (i, name) in ["ds1", "ds2", "ds3"].into_iter().enumerate() {
        let result = delete_svc.execute(&delete(name)).await.expect("delete");
        assert_eq!(result.deleted_datasets, 1);
        assert_eq!(result.deleted_data, 1, "the dataset's own row is deleted");
        assert!(
            ops::data::get_data(&database, rows[i].id)
                .await
                .expect("get data")
                .is_none()
        );
        assert!(
            !storage
                .exists(&rows[i].raw_data_location)
                .await
                .expect("exists"),
            "the deleted row's own file goes with it"
        );
        for later in &rows[i + 1..] {
            assert!(
                ops::data::get_data(&database, later.id)
                    .await
                    .expect("get data")
                    .is_some(),
                "other datasets' rows are untouched"
            );
            assert!(
                storage
                    .exists(&later.raw_data_location)
                    .await
                    .expect("exists"),
                "other datasets' files are untouched"
            );
        }
    }
}

#[tokio::test]
async fn cascade_deletion_preserves_data_with_remaining_links_sqlite() {
    let dir = TempDir::new().expect("tempdir for url");
    let url = sqlite_db_url(&dir);
    impl_cascade_deletion_preserves_data_with_remaining_links(&url).await;
}

#[tokio::test]
async fn cascade_deletion_preserves_data_with_remaining_links_pg() {
    let Some(url) = cognee_test_utils::pg_test_url() else {
        return;
    };
    impl_cascade_deletion_preserves_data_with_remaining_links(&url).await;
}

// ---------------------------------------------------------------------------
// Sub-test F — Tenant isolation
// ---------------------------------------------------------------------------

async fn impl_same_owner_different_tenants_creates_separate_data_records(db_url: &str) {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, _db, _storage) = make_pipeline(&dir, db_url).await;

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

#[tokio::test]
async fn same_owner_different_tenants_creates_separate_data_records_sqlite() {
    let dir = TempDir::new().expect("tempdir for url");
    let url = sqlite_db_url(&dir);
    impl_same_owner_different_tenants_creates_separate_data_records(&url).await;
}

#[tokio::test]
async fn same_owner_different_tenants_creates_separate_data_records_pg() {
    let Some(url) = cognee_test_utils::pg_test_url() else {
        return;
    };
    impl_same_owner_different_tenants_creates_separate_data_records(&url).await;
}

// ---------------------------------------------------------------------------
// Sub-test G — Owner × tenant matrix isolation
// ---------------------------------------------------------------------------

async fn impl_datasets_isolated_by_owner_and_tenant(db_url: &str) {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, _db, _storage) = make_pipeline(&dir, db_url).await;

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

    let unique: std::collections::HashSet<_> = dataset_ids.iter().collect();
    assert_eq!(
        unique.len(),
        4,
        "each owner+tenant combination must produce a distinct data ID"
    );
}

#[tokio::test]
async fn datasets_isolated_by_owner_and_tenant_sqlite() {
    let dir = TempDir::new().expect("tempdir for url");
    let url = sqlite_db_url(&dir);
    impl_datasets_isolated_by_owner_and_tenant(&url).await;
}

#[tokio::test]
async fn datasets_isolated_by_owner_and_tenant_pg() {
    let Some(url) = cognee_test_utils::pg_test_url() else {
        return;
    };
    impl_datasets_isolated_by_owner_and_tenant(&url).await;
}
