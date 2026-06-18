#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Span attribute integration tests for the Qdrant adapter.
#![cfg(feature = "qdrant")]

use cognee_test_utils::SpanCapture;
use cognee_vector::{QdrantAdapter, VectorDB, VectorPoint};
use tempfile::TempDir;
use uuid::Uuid;

/// Wrapper that guarantees correct drop order: the adapter must flush
/// before the `TempDir` removes the directory.
struct TestDb {
    db: QdrantAdapter,
    _dir: TempDir,
}

impl TestDb {
    fn new(dim: usize) -> Self {
        let dir = TempDir::new().expect("temp dir");
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
async fn search_emits_cognee_db_vector_search_span() {
    let capture = SpanCapture::install();
    let t = TestDb::new(4);

    // Seed the collection.
    let pid = Uuid::new_v4();
    let point = VectorPoint {
        id: pid,
        vector: vec![0.1, 0.2, 0.3, 0.4],
        metadata: std::collections::HashMap::new(),
    };
    t.index_points("DocumentChunk", "text", &[point])
        .await
        .expect("seed upsert");

    // Search.
    let results = t
        .search_similar("DocumentChunk", "text", &[0.1, 0.2, 0.3, 0.4], 5)
        .await
        .expect("search");
    assert_eq!(results.len(), 1);

    let spans = capture.spans();
    let s = spans
        .iter()
        .find(|s| s.name == "cognee.db.vector.search")
        .expect("expected vector search span");
    assert_eq!(s.field_str("cognee.db.system").as_deref(), Some("qdrant"));
    assert_eq!(
        s.field_str("cognee.vector.collection").as_deref(),
        Some("DocumentChunk_text"),
    );
    assert_eq!(s.field_i64("cognee.vector.result_count"), Some(1));
}

#[tokio::test]
async fn upsert_emits_cognee_db_vector_upsert_span_with_row_count() {
    let capture = SpanCapture::install();
    let t = TestDb::new(4);

    let points: Vec<VectorPoint> = (0..3)
        .map(|i| VectorPoint {
            id: Uuid::new_v4(),
            vector: vec![i as f32, 0.0, 0.0, 0.0],
            metadata: std::collections::HashMap::new(),
        })
        .collect();
    t.index_points("Entity", "name", &points)
        .await
        .expect("upsert");

    let spans = capture.spans();
    let s = spans
        .iter()
        .find(|s| s.name == "cognee.db.vector.upsert")
        .expect("expected upsert span");
    assert_eq!(s.field_str("cognee.db.system").as_deref(), Some("qdrant"));
    assert_eq!(
        s.field_str("cognee.vector.collection").as_deref(),
        Some("Entity_name"),
    );
    assert_eq!(s.field_i64("cognee.db.row_count"), Some(3));
}

#[tokio::test]
async fn delete_emits_cognee_db_vector_delete_span_with_row_count() {
    let capture = SpanCapture::install();
    let t = TestDb::new(4);

    // Seed first so the delete has something to remove.
    let pid = Uuid::new_v4();
    t.index_points(
        "DocumentChunk",
        "text",
        &[VectorPoint {
            id: pid,
            vector: vec![0.1, 0.0, 0.0, 0.0],
            metadata: std::collections::HashMap::new(),
        }],
    )
    .await
    .expect("seed");

    t.delete_points("DocumentChunk", "text", &[pid])
        .await
        .expect("delete");

    let spans = capture.spans();
    let s = spans
        .iter()
        .find(|s| s.name == "cognee.db.vector.delete")
        .expect("expected delete span");
    assert_eq!(s.field_str("cognee.db.system").as_deref(), Some("qdrant"));
    assert_eq!(s.field_i64("cognee.db.row_count"), Some(1));
}

#[tokio::test]
async fn empty_upsert_short_circuits_but_still_emits_span() {
    let capture = SpanCapture::install();
    let t = TestDb::new(4);
    t.index_points("DocumentChunk", "text", &[])
        .await
        .expect("empty upsert");

    let spans = capture.spans();
    assert!(
        spans.iter().any(|s| s.name == "cognee.db.vector.upsert"),
        "expected an upsert span even on empty input"
    );
}

#[tokio::test]
async fn delete_collection_emits_span() {
    let capture = SpanCapture::install();
    let t = TestDb::new(4);

    // Seed a collection so the directory exists.
    t.index_points(
        "Entity",
        "name",
        &[VectorPoint {
            id: Uuid::new_v4(),
            vector: vec![1.0, 0.0, 0.0, 0.0],
            metadata: std::collections::HashMap::new(),
        }],
    )
    .await
    .expect("seed");

    t.delete_collection("Entity", "name")
        .await
        .expect("delete collection");

    let spans = capture.spans();
    let s = spans
        .iter()
        .find(|s| s.name == "cognee.db.vector.delete_collection")
        .expect("expected delete_collection span");
    assert_eq!(s.field_str("cognee.db.system").as_deref(), Some("qdrant"));
    assert_eq!(
        s.field_str("cognee.vector.collection").as_deref(),
        Some("Entity_name"),
    );
}
