//! Integration test for the in-memory brute-force `VectorDB` adapter.
//!
//! Exercises the full public surface — create / index / search / list /
//! prune — and verifies concurrent indexing through `tokio::spawn`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test — panics are acceptable"
)]

use std::collections::HashMap;
use std::sync::Arc;

use cognee_vector::{BruteForceVectorDB, VectorDB, VectorPoint};
use uuid::Uuid;

const DATA_TYPE: &str = "DataPoint";
const FIELD_NAME: &str = "embedding";
const DIM: usize = 8;

/// Build a deterministic 8-dim vector from `seed`.
///
/// Uses a small LCG so the resulting vectors are not all colinear but
/// don't require an RNG dependency.
fn deterministic_vector(seed: u32) -> Vec<f32> {
    let mut x = seed.wrapping_mul(2654435761).wrapping_add(1);
    (0..DIM)
        .map(|_| {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            // Map to [-1.0, 1.0).
            ((x >> 8) as f32 / (1u32 << 23) as f32) - 1.0
        })
        .collect()
}

fn build_point(seed: u128) -> VectorPoint {
    VectorPoint {
        id: Uuid::from_u128(seed),
        vector: deterministic_vector(seed as u32),
        metadata: HashMap::new(),
    }
}

#[tokio::test]
async fn brute_force_end_to_end_search_and_prune() {
    let db = BruteForceVectorDB::new();
    db.create_collection(DATA_TYPE, FIELD_NAME, DIM)
        .await
        .unwrap();

    let points: Vec<VectorPoint> = (1u128..=50).map(build_point).collect();
    let original_ids: std::collections::HashSet<Uuid> = points.iter().map(|p| p.id).collect();

    db.index_points(DATA_TYPE, FIELD_NAME, &points)
        .await
        .unwrap();
    assert_eq!(db.collection_size(DATA_TYPE, FIELD_NAME).await.unwrap(), 50);

    // Use one of the indexed vectors as the query so we expect a near-1.0
    // top score against itself.
    let query = points[0].vector.clone();
    let results = db
        .search_similar(DATA_TYPE, FIELD_NAME, &query, 5)
        .await
        .unwrap();

    assert_eq!(results.len(), 5, "top_k=5 should yield 5 results");
    for r in &results {
        assert!(
            original_ids.contains(&r.id),
            "result id {} should be one of the indexed points",
            r.id
        );
    }
    for w in results.windows(2) {
        assert!(
            w[0].score >= w[1].score,
            "scores must be sorted descending: {} then {}",
            w[0].score,
            w[1].score
        );
    }
    assert_eq!(
        results[0].id, points[0].id,
        "self-similarity should rank the query-source point first"
    );

    // Concurrent indexing: 4 tasks, 5 disjoint ids each → 20 new points,
    // total 70.
    let db = Arc::new(db);
    let mut handles = Vec::new();
    for task_idx in 0u128..4 {
        let db_handle = Arc::clone(&db);
        handles.push(tokio::spawn(async move {
            let base = 1000 + task_idx * 5;
            let batch: Vec<VectorPoint> = (0..5).map(|i| build_point(base + i)).collect();
            db_handle
                .index_points(DATA_TYPE, FIELD_NAME, &batch)
                .await
                .unwrap();
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    assert_eq!(
        db.collection_size(DATA_TYPE, FIELD_NAME).await.unwrap(),
        70,
        "50 original + 4 × 5 concurrent inserts = 70"
    );

    // prune() (default trait impl): list → delete each collection.
    db.prune().await.unwrap();
    assert!(db.list_collections().await.unwrap().is_empty());
}
