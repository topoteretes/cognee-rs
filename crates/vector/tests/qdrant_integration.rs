//! Integration tests for `QdrantAdapter` using the shared VectorDB test suite.
#![cfg(feature = "qdrant")]

mod common;

use cognee_vector::QdrantAdapter;
use tempfile::TempDir;

/// Wrapper that guarantees correct drop order: adapter flushes before TempDir
/// removes the directory. Struct fields are dropped in declaration order.
struct TestDb {
    db: QdrantAdapter,
    _dir: TempDir,
}

impl TestDb {
    fn new(dim: usize) -> Self {
        let dir = TempDir::new().unwrap();
        let db = QdrantAdapter::new(dir.path().to_path_buf(), dim);
        Self { db, _dir: dir }
    }
}

impl std::ops::Deref for TestDb {
    type Target = QdrantAdapter;
    fn deref(&self) -> &Self::Target {
        &self.db
    }
}

#[tokio::test]
async fn create_and_has_collection() {
    let t = TestDb::new(3);
    common::test_create_and_has_collection(&*t).await;
}

#[tokio::test]
async fn create_duplicate_errors() {
    let t = TestDb::new(3);
    common::test_create_duplicate_errors(&*t).await;
}

#[tokio::test]
async fn delete_collection() {
    let t = TestDb::new(2);
    common::test_delete_collection(&*t).await;
}

#[tokio::test]
async fn list_collections() {
    let t = TestDb::new(3);
    common::test_list_collections(&*t).await;
}

#[tokio::test]
async fn index_and_collection_size() {
    let t = TestDb::new(2);
    common::test_index_and_collection_size(&*t).await;
}

#[tokio::test]
async fn empty_points_index() {
    let t = TestDb::new(2);
    common::test_empty_points_index(&*t).await;
}

#[tokio::test]
async fn dimension_validation() {
    let t = TestDb::new(3);
    common::test_dimension_validation(&*t).await;
}

#[tokio::test]
async fn upsert_overwrites() {
    let t = TestDb::new(2);
    common::test_upsert_overwrites(&*t).await;
}

#[tokio::test]
async fn index_and_search() {
    let t = TestDb::new(3);
    common::test_index_and_search(&*t).await;
}

#[tokio::test]
async fn search_returns_top_k() {
    let t = TestDb::new(2);
    common::test_search_returns_top_k(&*t).await;
}

#[tokio::test]
async fn metadata_preserved() {
    let t = TestDb::new(2);
    common::test_metadata_preserved(&*t).await;
}

#[tokio::test]
async fn uuid_round_trip() {
    let t = TestDb::new(2);
    common::test_uuid_round_trip(&*t).await;
}

#[tokio::test]
async fn delete_points() {
    let t = TestDb::new(2);
    common::test_delete_points(&*t).await;
}

#[tokio::test]
async fn batch_search() {
    let t = TestDb::new(3);
    common::test_batch_search(&*t).await;
}
