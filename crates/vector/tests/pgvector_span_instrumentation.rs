//! Span attribute integration tests for the PgVector adapter.
//!
//! Skipped silently when `cognee_test_utils::pg_test_url()` returns `None`
//! (i.e. `DB_PROVIDER` is not set to `postgres`). Mirrors the gating
//! pattern in `pgvector_integration.rs`.
#![cfg(feature = "pgvector")]

use cognee_test_utils::SpanCapture;
use cognee_vector::{PgVectorAdapter, VectorDB, VectorPoint};
use serial_test::serial;
use std::collections::HashMap;
use uuid::Uuid;

/// Returns a fresh adapter (with stale collections cleaned up) when a
/// Postgres URL is configured, or `None` to silently skip.
async fn make_adapter() -> Option<PgVectorAdapter> {
    let url = cognee_test_utils::pg_test_url()?;
    let adapter = PgVectorAdapter::new(&url, 4).await.ok()?;
    if let Ok(cols) = adapter.list_collections().await {
        for (dt, fname) in cols {
            let _ = adapter.delete_collection(&dt, &fname).await;
        }
    }
    Some(adapter)
}

#[tokio::test]
#[serial]
async fn upsert_emits_pgvector_span() {
    let Some(adapter) = make_adapter().await else {
        eprintln!("DB_PROVIDER!=postgres — skipping upsert_emits_pgvector_span");
        return;
    };
    let capture = SpanCapture::install();

    let points: Vec<VectorPoint> = (0..2)
        .map(|i| VectorPoint {
            id: Uuid::new_v4(),
            vector: vec![i as f32, 0.0, 0.0, 0.0],
            metadata: HashMap::new(),
        })
        .collect();
    adapter
        .index_points("DocumentChunk", "text", &points)
        .await
        .expect("upsert");

    let spans = capture.spans();
    let s = spans
        .iter()
        .find(|s| s.name == "cognee.db.vector.upsert")
        .expect("expected upsert span");
    assert_eq!(s.field_str("cognee.db.system").as_deref(), Some("pgvector"));
    assert_eq!(
        s.field_str("cognee.vector.collection").as_deref(),
        Some("DocumentChunk_text"),
    );
    assert_eq!(s.field_i64("cognee.db.row_count"), Some(2));
}

#[tokio::test]
#[serial]
async fn search_emits_pgvector_span() {
    let Some(adapter) = make_adapter().await else {
        eprintln!("DB_PROVIDER!=postgres — skipping search_emits_pgvector_span");
        return;
    };
    let capture = SpanCapture::install();

    let pid = Uuid::new_v4();
    adapter
        .index_points(
            "DocumentChunk",
            "text",
            &[VectorPoint {
                id: pid,
                vector: vec![0.1, 0.2, 0.3, 0.4],
                metadata: HashMap::new(),
            }],
        )
        .await
        .expect("seed");

    let results = adapter
        .search_similar("DocumentChunk", "text", &[0.1, 0.2, 0.3, 0.4], 5)
        .await
        .expect("search");
    assert!(!results.is_empty());

    let spans = capture.spans();
    let s = spans
        .iter()
        .find(|s| s.name == "cognee.db.vector.search")
        .expect("expected search span");
    assert_eq!(s.field_str("cognee.db.system").as_deref(), Some("pgvector"));
    assert_eq!(
        s.field_str("cognee.vector.collection").as_deref(),
        Some("DocumentChunk_text"),
    );
    assert!(s.field_i64("cognee.vector.result_count").unwrap_or(0) >= 1);
}

#[tokio::test]
#[serial]
async fn delete_emits_pgvector_span() {
    let Some(adapter) = make_adapter().await else {
        eprintln!("DB_PROVIDER!=postgres — skipping delete_emits_pgvector_span");
        return;
    };
    let capture = SpanCapture::install();

    let pid = Uuid::new_v4();
    adapter
        .index_points(
            "DocumentChunk",
            "text",
            &[VectorPoint {
                id: pid,
                vector: vec![0.1, 0.0, 0.0, 0.0],
                metadata: HashMap::new(),
            }],
        )
        .await
        .expect("seed");
    adapter
        .delete_points("DocumentChunk", "text", &[pid])
        .await
        .expect("delete");

    let spans = capture.spans();
    let s = spans
        .iter()
        .find(|s| s.name == "cognee.db.vector.delete")
        .expect("expected delete span");
    assert_eq!(s.field_str("cognee.db.system").as_deref(), Some("pgvector"));
}
