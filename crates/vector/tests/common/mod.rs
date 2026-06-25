#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Shared VectorDB contract tests.
//!
//! Each function exercises one aspect of the [`VectorDB`] trait and can be
//! called with *any* backend (Qdrant, PgVector, Mock, …). Backend-specific
//! integration tests just need to construct their adapter and call these
//! helpers.

use cognee_vector::{VectorDB, VectorDBError, VectorPoint};
use serde_json::json;
use uuid::Uuid;

// -- collection lifecycle ---------------------------------------------------

pub async fn test_create_and_has_collection(db: &dyn VectorDB) {
    db.create_collection("DocChunk", "text", 3).await.unwrap();
    assert!(db.has_collection("DocChunk", "text").await.unwrap());
    assert!(!db.has_collection("DocChunk", "other").await.unwrap());
}

pub async fn test_create_duplicate_errors(db: &dyn VectorDB) {
    db.create_collection("Entity", "name", 3).await.unwrap();
    let err = db.create_collection("Entity", "name", 3).await;
    assert!(
        matches!(err, Err(VectorDBError::CollectionExists(_))),
        "duplicate create should return CollectionExists, got {err:?}"
    );
}

pub async fn test_delete_collection(db: &dyn VectorDB) {
    db.create_collection("Del", "field", 2).await.unwrap();
    assert!(db.has_collection("Del", "field").await.unwrap());

    db.delete_collection("Del", "field").await.unwrap();
    assert!(!db.has_collection("Del", "field").await.unwrap());
}

pub async fn test_list_collections(db: &dyn VectorDB) {
    db.create_collection("Alpha", "text", 3).await.unwrap();
    db.create_collection("Beta", "name", 3).await.unwrap();

    let mut cols = db.list_collections().await.unwrap();
    // Filter to only the ones we created (shared DB may have others).
    cols.retain(|(dt, _)| dt == "Alpha" || dt == "Beta");
    cols.sort();

    assert_eq!(cols.len(), 2);
    assert!(cols.contains(&("Alpha".into(), "text".into())));
    assert!(cols.contains(&("Beta".into(), "name".into())));
}

// -- indexing & size --------------------------------------------------------

pub async fn test_index_and_collection_size(db: &dyn VectorDB) {
    db.create_collection("Size", "f", 2).await.unwrap();

    let points = vec![
        VectorPoint::new(Uuid::new_v4(), vec![1.0, 0.0]),
        VectorPoint::new(Uuid::new_v4(), vec![0.0, 1.0]),
    ];
    db.index_points("Size", "f", &points).await.unwrap();

    assert_eq!(db.collection_size("Size", "f").await.unwrap(), 2);
}

pub async fn test_empty_points_index(db: &dyn VectorDB) {
    db.create_collection("Empty", "f", 2).await.unwrap();
    let empty: Vec<VectorPoint> = vec![];
    db.index_points("Empty", "f", &empty).await.unwrap();
    assert_eq!(db.collection_size("Empty", "f").await.unwrap(), 0);
}

pub async fn test_dimension_validation(db: &dyn VectorDB) {
    db.create_collection("Dim", "f", 3).await.unwrap();

    let points = vec![
        VectorPoint::new(Uuid::new_v4(), vec![1.0, 0.0, 0.0]),
        VectorPoint::new(Uuid::new_v4(), vec![0.0, 1.0]), // wrong dim
    ];

    let err = db.index_points("Dim", "f", &points).await;
    assert!(
        matches!(err, Err(VectorDBError::DimensionMismatch { .. })),
        "mismatched dimensions should error, got {err:?}"
    );
}

pub async fn test_upsert_overwrites(db: &dyn VectorDB) {
    db.create_collection("Upsert", "f", 2).await.unwrap();

    let id = Uuid::new_v4();
    let original = vec![VectorPoint::new(id, vec![1.0, 0.0]).with_metadata("v", json!(1))];
    db.index_points("Upsert", "f", &original).await.unwrap();
    assert_eq!(db.collection_size("Upsert", "f").await.unwrap(), 1);

    // Re-index same ID with different vector/metadata — should upsert, not
    // create a second row.
    let updated = vec![VectorPoint::new(id, vec![0.0, 1.0]).with_metadata("v", json!(2))];
    db.index_points("Upsert", "f", &updated).await.unwrap();
    assert_eq!(db.collection_size("Upsert", "f").await.unwrap(), 1);

    // Verify the updated metadata is returned.
    let results = db
        .search_similar("Upsert", "f", &[0.0, 1.0], 1)
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, id);
    assert_eq!(results[0].metadata.get("v"), Some(&json!(2)));
}

// -- search -----------------------------------------------------------------

pub async fn test_index_and_search(db: &dyn VectorDB) {
    db.create_collection("Search", "name", 3).await.unwrap();

    let points = vec![
        VectorPoint::new(Uuid::new_v4(), vec![1.0, 0.0, 0.0])
            .with_metadata("name", json!("Cognee")),
        VectorPoint::new(Uuid::new_v4(), vec![0.0, 1.0, 0.0])
            .with_metadata("name", json!("Knowledge")),
        VectorPoint::new(Uuid::new_v4(), vec![0.0, 0.0, 1.0]).with_metadata("name", json!("Rust")),
    ];
    db.index_points("Search", "name", &points).await.unwrap();

    let results = db
        .search_similar("Search", "name", &[1.0, 0.0, 0.0], 2)
        .await
        .unwrap();

    assert_eq!(results.len(), 2);
    // First result is the exact-match vector — should have the highest score.
    assert!(
        results[0].score >= results[1].score,
        "results should be ordered by similarity desc"
    );
}

pub async fn test_search_returns_top_k(db: &dyn VectorDB) {
    db.create_collection("TopK", "f", 2).await.unwrap();

    let points: Vec<VectorPoint> = (0..10)
        .map(|i| {
            VectorPoint::new(
                Uuid::new_v4(),
                vec![i as f32 / 10.0, 1.0 - (i as f32 / 10.0)],
            )
        })
        .collect();
    db.index_points("TopK", "f", &points).await.unwrap();

    let results = db
        .search_similar("TopK", "f", &[0.5, 0.5], 3)
        .await
        .unwrap();
    assert_eq!(results.len(), 3);
}

pub async fn test_metadata_preserved(db: &dyn VectorDB) {
    db.create_collection("Meta", "f", 2).await.unwrap();

    let id = Uuid::new_v4();
    let points = vec![
        VectorPoint::new(id, vec![1.0, 0.0])
            .with_metadata("type", json!("DocumentChunk"))
            .with_metadata("document_id", json!("doc-123"))
            .with_metadata("chunk_index", json!(42)),
    ];
    db.index_points("Meta", "f", &points).await.unwrap();

    let results = db
        .search_similar("Meta", "f", &[1.0, 0.0], 1)
        .await
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].metadata.get("type"),
        Some(&json!("DocumentChunk"))
    );
    assert_eq!(
        results[0].metadata.get("document_id"),
        Some(&json!("doc-123"))
    );
    assert_eq!(results[0].metadata.get("chunk_index"), Some(&json!(42)));
}

pub async fn test_uuid_round_trip(db: &dyn VectorDB) {
    db.create_collection("UUID", "f", 2).await.unwrap();

    let stored_id = Uuid::parse_str("f7ab8d87-553f-4509-b595-463cedc998be").unwrap();
    let points = vec![VectorPoint::new(stored_id, vec![1.0, 0.0])];
    db.index_points("UUID", "f", &points).await.unwrap();

    let results = db
        .search_similar("UUID", "f", &[1.0, 0.0], 1)
        .await
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].id, stored_id,
        "UUID round-trip must preserve all 128 bits"
    );
}

// -- deletion ---------------------------------------------------------------

pub async fn test_delete_points(db: &dyn VectorDB) {
    db.create_collection("DelPts", "f", 2).await.unwrap();

    let id1 = Uuid::new_v4();
    let id2 = Uuid::new_v4();
    let points = vec![
        VectorPoint::new(id1, vec![1.0, 0.0]),
        VectorPoint::new(id2, vec![0.0, 1.0]),
    ];
    db.index_points("DelPts", "f", &points).await.unwrap();
    assert_eq!(db.collection_size("DelPts", "f").await.unwrap(), 2);

    db.delete_points("DelPts", "f", &[id1]).await.unwrap();
    assert_eq!(db.collection_size("DelPts", "f").await.unwrap(), 1);
}

// -- batch search -----------------------------------------------------------

pub async fn test_batch_search(db: &dyn VectorDB) {
    db.create_collection("Batch", "f", 3).await.unwrap();

    let points = vec![
        VectorPoint::new(Uuid::new_v4(), vec![1.0, 0.0, 0.0]),
        VectorPoint::new(Uuid::new_v4(), vec![0.0, 1.0, 0.0]),
    ];
    db.index_points("Batch", "f", &points).await.unwrap();

    let queries = vec![vec![1.0, 0.0, 0.0], vec![0.0, 1.0, 0.0]];
    let results = db
        .batch_search_similar("Batch", "f", &queries, 5)
        .await
        .unwrap();

    assert_eq!(results.len(), 2, "one result set per query");
    assert!(!results[0].is_empty());
    assert!(!results[1].is_empty());
}
