//! Provenance payload regression test (gap 05-10).
//!
//! Verifies that the full DataPoint dump produced by
//! [`cognee_models::DataPoint::vector_metadata`] (decision 5) lands in
//! the metadata payload of every indexed point, end-to-end, via the
//! vector-DB trait.
//!
//! Uses `MockVectorDB` so the assertion does not depend on Qdrant
//! initialization. The mock retains the `metadata: HashMap<String,
//! serde_json::Value>` byte-for-byte (no key remapping), which matches
//! the `QdrantAdapter` shape in production.
//!
//! `MockVectorDB` is only compiled when the `testing` feature is on;
//! `cognee-test-utils` enables that feature for us, so we route the
//! import through it instead of conditionally gating the whole test
//! module behind `cfg(feature = "testing")`.

use cognee_models::{DataPoint, DocumentChunk};
use cognee_test_utils::MockVectorDB;
use cognee_vector::{VectorDB, VectorPoint};
use uuid::Uuid;

#[tokio::test]
async fn vector_point_carries_full_datapoint_dump() {
    let document_id = Uuid::new_v4();
    let mut chunk = DocumentChunk::new(
        Uuid::new_v4(),
        "hello".into(),
        1,
        0,
        "paragraph_end".into(),
        document_id,
    );
    chunk.base.source_pipeline = Some("cognify_pipeline".into());
    chunk.base.source_task = Some("extract_chunks_from_documents".into());
    chunk.base.source_user = Some("alice@example.com".into());
    chunk.base.source_node_set = Some("text_nodes".into());
    chunk.base.source_content_hash = Some("md5:abcdef".into());

    // Build the point exactly as `crates/cognify/src/tasks.rs` does:
    // start with the canonical full DataPoint dump from
    // `vector_metadata()` and let the cognify call site layer
    // context-specific extras (`field`, `dataset_id`, etc.) on top via
    // `with_metadata`. The latter is out of scope for this test —
    // we only need to prove the canonical dump survives the round trip.
    let mut point = VectorPoint::new(chunk.base.id, vec![0.0; 384]);
    for (k, v) in chunk.base.vector_metadata() {
        point = point.with_metadata(k, v);
    }

    let db = MockVectorDB::new();
    db.create_collection("DocumentChunk", "text", 384)
        .await
        .unwrap();
    db.index_points("DocumentChunk", "text", &[point])
        .await
        .unwrap();

    let stored = db
        .get_payload("DocumentChunk", "text", chunk.base.id)
        .expect("indexed point must round-trip through MockVectorDB");

    assert_eq!(
        stored.get("source_pipeline").and_then(|v| v.as_str()),
        Some("cognify_pipeline")
    );
    assert_eq!(
        stored.get("source_task").and_then(|v| v.as_str()),
        Some("extract_chunks_from_documents")
    );
    assert_eq!(
        stored.get("source_user").and_then(|v| v.as_str()),
        Some("alice@example.com")
    );
    assert_eq!(
        stored.get("source_node_set").and_then(|v| v.as_str()),
        Some("text_nodes")
    );
    assert_eq!(
        stored.get("source_content_hash").and_then(|v| v.as_str()),
        Some("md5:abcdef")
    );
    assert_eq!(
        stored.get("type").and_then(|v| v.as_str()),
        Some("DocumentChunk"),
        "DataPoint::vector_metadata() must preserve the `type` rename"
    );
}

#[tokio::test]
async fn omitted_provenance_fields_are_absent_from_payload() {
    // Mirror Python's `model_dump(exclude_none=True)` shape: keys with
    // `None` values must not appear in the metadata at all. Catches a
    // regression where the `skip_serializing_if` annotation is dropped
    // from any of the five `source_*` fields.
    let dp = DataPoint::new("Entity", None);
    let mut point = VectorPoint::new(dp.id, vec![0.0; 4]);
    for (k, v) in dp.vector_metadata() {
        point = point.with_metadata(k, v);
    }

    let db = MockVectorDB::new();
    db.create_collection("Entity", "name", 4).await.unwrap();
    db.index_points("Entity", "name", &[point]).await.unwrap();

    let stored = db
        .get_payload("Entity", "name", dp.id)
        .expect("indexed point round-trip");

    for absent in [
        "source_pipeline",
        "source_task",
        "source_user",
        "source_node_set",
        "source_content_hash",
    ] {
        assert!(
            !stored.contains_key(absent),
            "expected {absent} to be omitted from payload when the DataPoint field is None"
        );
    }
}
