//! Regression test for the cross-dataset dedup bug.
//!
//! Reported (TS SDK v0.1.1): "the same text added to multiple datasets is only
//! cognified for the first dataset. Subsequent datasets have the item in the
//! metadata DB but not in the vector store."
//!
//! Vector point IDs are content-addressed (UUID v5 of the content), so identical
//! content produces the *same* point ID across datasets. The cognify indexer
//! re-embeds and re-`index_points` that shared ID once per dataset, tagging it
//! with a scalar `dataset_id`. Because every adapter upserts by ID with full
//! replacement, the second dataset's upsert used to overwrite the first
//! dataset's `dataset_id`, erasing its membership — so a search scoped to the
//! first dataset could no longer retrieve the content.
//!
//! The fix accumulates membership into a `dataset_ids` union array on upsert.
//! This test drives the **default** OSS adapter (`BruteForceVectorDB`) the way
//! the cognify indexer does and asserts the shared point belongs to both
//! datasets after the second dataset is indexed. It is fully offline (no LLM,
//! no embedding network).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]

use cognee_vector::{BruteForceVectorDB, DATASET_IDS_KEY, VectorDB, VectorPoint};
use serde_json::json;
use uuid::Uuid;

/// Build a chunk vector point exactly as `index_data_points` does: a
/// content-addressed id, the embedding, and the scalar `dataset_id` tag.
fn chunk_point(content_id: Uuid, dataset_id: Uuid, vector: Vec<f32>) -> VectorPoint {
    VectorPoint::new(content_id, vector)
        .with_metadata("field", json!("text"))
        .with_metadata("text", json!("Goran je CTO u Topoteretes."))
        .with_metadata("dataset_id", json!(dataset_id.to_string()))
}

#[tokio::test]
async fn identical_content_in_two_datasets_keeps_both_memberships() {
    let db = BruteForceVectorDB::new();
    db.create_collection("DocumentChunk", "text", 3)
        .await
        .expect("create collection");

    // Same text in dataset A and dataset B → identical content-addressed id.
    let content_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, b"Goran je CTO u Topoteretes.");
    let dataset_a = Uuid::new_v4();
    let dataset_b = Uuid::new_v4();
    let vector = vec![0.1, 0.2, 0.3];

    // Cognify dataset A, then dataset B (B's upsert hits the same id).
    db.index_points(
        "DocumentChunk",
        "text",
        &[chunk_point(content_id, dataset_a, vector.clone())],
    )
    .await
    .expect("index dataset A");
    db.index_points(
        "DocumentChunk",
        "text",
        &[chunk_point(content_id, dataset_b, vector.clone())],
    )
    .await
    .expect("index dataset B");

    // Exactly one physical point (content-addressed dedup is intentional)...
    assert_eq!(
        db.collection_size("DocumentChunk", "text").await.unwrap(),
        1,
        "content-addressed point should be deduplicated to a single physical row"
    );

    // ...but it must record membership in BOTH datasets.
    let results = db
        .search_similar("DocumentChunk", "text", &vector, 10)
        .await
        .expect("search");
    assert_eq!(results.len(), 1);
    let members: Vec<String> = results[0]
        .metadata
        .get(DATASET_IDS_KEY)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    assert!(
        members.contains(&dataset_a.to_string()),
        "dataset A membership was dropped when dataset B was cognified; \
         dataset_ids = {members:?}"
    );
    assert!(
        members.contains(&dataset_b.to_string()),
        "dataset B membership missing; dataset_ids = {members:?}"
    );
}
