//! Cross-dataset deduplication integration tests.
//!
//! Verifies that content-addressed deduplication works correctly when the same
//! data is added to multiple datasets, and that the MD5-based naming and
//! junction-table linking behave as expected.

use cognee_core::RayonThreadPool;
use cognee_database::{IngestDb, connect, initialize, ops};
use cognee_graph::MockGraphDB;
use cognee_ingestion::AddPipeline;
use cognee_models::DataInput;
use cognee_storage::{LocalStorage, StorageTrait};
use cognee_vector::MockVectorDB;
use std::io::Write;
use std::sync::Arc;
use tempfile::{NamedTempFile, TempDir};
use uuid::Uuid;

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
    )
    .with_thread_pool(Arc::new(RayonThreadPool::with_default_threads().unwrap()))
    .with_graph_db(Arc::new(MockGraphDB::new()))
    .with_vector_db(Arc::new(MockVectorDB::new()))
    .with_database(Arc::clone(&db));
    (pipeline, db, storage)
}

// ---------------------------------------------------------------------------
// C1.1 — Same content reused across datasets
// ---------------------------------------------------------------------------

#[tokio::test]
async fn same_content_reused_across_datasets() {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, database, _storage) = make_pipeline(&dir).await;
    let owner = Uuid::new_v4();

    let text = "Shared knowledge about neural networks and deep learning.";

    let result_a = pipeline
        .add(vec![DataInput::Text(text.to_string())], "A", owner, None)
        .await
        .expect("add to dataset A");

    let result_b = pipeline
        .add(vec![DataInput::Text(text.to_string())], "B", owner, None)
        .await
        .expect("add to dataset B");

    // Same content + same owner → same Data record
    assert_eq!(
        result_a[0].id, result_b[0].id,
        "identical content should yield the same data_id across datasets"
    );

    // Two distinct datasets exist
    let ds_a = ops::datasets::get_dataset_by_name(&database, "A", owner, None)
        .await
        .expect("get ds A")
        .expect("dataset A should exist");
    let ds_b = ops::datasets::get_dataset_by_name(&database, "B", owner, None)
        .await
        .expect("get ds B")
        .expect("dataset B should exist");

    assert_ne!(ds_a.id, ds_b.id, "datasets A and B must have different IDs");

    // Both datasets reference the same data_id
    let ds_a_data = ops::datasets::get_dataset_data(&database, ds_a.id)
        .await
        .expect("ds A data");
    let ds_b_data = ops::datasets::get_dataset_data(&database, ds_b.id)
        .await
        .expect("ds B data");

    assert_eq!(ds_a_data.len(), 1, "dataset A should have 1 data item");
    assert_eq!(ds_b_data.len(), 1, "dataset B should have 1 data item");
    assert_eq!(
        ds_a_data[0].id, ds_b_data[0].id,
        "both datasets must reference the same underlying data record"
    );
}

// ---------------------------------------------------------------------------
// C1.2 — Text dedup MD5 in filename
// ---------------------------------------------------------------------------

#[tokio::test]
async fn text_dedup_md5_in_filename() {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, _database, _storage) = make_pipeline(&dir).await;
    let owner = Uuid::new_v4();

    let text = "Hello, this is a test for MD5 naming.";

    let result = pipeline
        .add(
            vec![DataInput::Text(text.to_string())],
            "md5_test",
            owner,
            None,
        )
        .await
        .expect("add text");

    // Compute expected MD5 hex the same way the pipeline does:
    // md5(content_bytes).hexdigest()
    use md5::Digest;
    let expected_hash = format!("{:x}", md5::Md5::digest(text.as_bytes()));
    let expected_name = format!("text_{expected_hash}");

    assert_eq!(
        result[0].name, expected_name,
        "Data.name should follow the text_<md5> pattern"
    );
    assert!(
        result[0].name.starts_with("text_"),
        "name must start with text_"
    );
    // Also verify the content_hash matches
    assert_eq!(
        result[0].content_hash, expected_hash,
        "content_hash should be the MD5 hex digest"
    );
}

// ---------------------------------------------------------------------------
// C1.3 — Dedup file copies with different names
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dedup_file_copies_with_different_names() {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, database, _storage) = make_pipeline(&dir).await;
    let owner = Uuid::new_v4();

    let content = b"Identical content in two files with different names.";

    let mut file1 = NamedTempFile::new().expect("tmp file 1");
    file1.write_all(content).expect("write file 1");
    let path1 = file1.path().to_str().unwrap().to_string();

    let mut file2 = NamedTempFile::new().expect("tmp file 2");
    file2.write_all(content).expect("write file 2");
    let path2 = file2.path().to_str().unwrap().to_string();

    // Add both files in a single add() call to the same dataset
    let result = pipeline
        .add(
            vec![DataInput::FilePath(path1), DataInput::FilePath(path2)],
            "file_dedup",
            owner,
            None,
        )
        .await
        .expect("add two files");

    // Both results should reference the same data record
    assert_eq!(result.len(), 2, "should return 2 results (one per input)");
    assert_eq!(
        result[0].id, result[1].id,
        "identical content files should deduplicate to the same data_id"
    );

    // Only 1 row in the dataset_data junction
    let ds = ops::datasets::get_dataset_by_name(&database, "file_dedup", owner, None)
        .await
        .expect("get dataset")
        .expect("dataset should exist");
    let ds_data = ops::datasets::get_dataset_data(&database, ds.id)
        .await
        .expect("dataset data");
    assert_eq!(
        ds_data.len(),
        1,
        "only one data record should be linked to the dataset"
    );
}

// ---------------------------------------------------------------------------
// C1.4 — Dedup across multiple add calls
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dedup_across_multiple_add_calls() {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, database, _storage) = make_pipeline(&dir).await;
    let owner = Uuid::new_v4();

    let text = "hello";

    let result1 = pipeline
        .add(vec![DataInput::Text(text.to_string())], "ds", owner, None)
        .await
        .expect("add call 1");

    let result2 = pipeline
        .add(vec![DataInput::Text(text.to_string())], "ds", owner, None)
        .await
        .expect("add call 2");

    // Same data_id returned both times
    assert_eq!(
        result1[0].id, result2[0].id,
        "same text to same dataset should return the same data_id"
    );

    // Only 1 junction row (no duplicate attachment)
    let ds = ops::datasets::get_dataset_by_name(&database, "ds", owner, None)
        .await
        .expect("get dataset")
        .expect("dataset should exist");
    let ds_data = ops::datasets::get_dataset_data(&database, ds.id)
        .await
        .expect("dataset data");
    assert_eq!(
        ds_data.len(),
        1,
        "duplicate add should not create a second junction row"
    );

    // Also verify via link count
    let link_count = ops::data::count_data_dataset_links(&database, result1[0].id)
        .await
        .expect("count links");
    assert_eq!(link_count, 1, "data should be linked to exactly 1 dataset");
}
