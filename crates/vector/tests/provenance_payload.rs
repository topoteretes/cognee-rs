//! Provenance in vector payloads (SDK-1039, part B).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test code — panics are acceptable"
)]
//!
//! Vector rows have Python's `IndexSchema` shape
//! ([`cognee_models::index_schema_payload`]): the base fields sit at their
//! IndexSchema defaults, so a source point's `source_*` provenance stays on the
//! graph node and is `null` in the payload, as in Python's stored rows. This
//! replaces the earlier gap 05-10 contract, which copied the full DataPoint
//! dump into every payload and so contradicted Python.
//!
//! `MockVectorDB` keeps the payload byte-for-byte, which isolates the builder
//! from any adapter's own write rules. `cognee-test-utils` enables the
//! `testing` feature that compiles it.

use cognee_models::{DocumentChunk, IndexRefs, index_schema_payload};
use cognee_test_utils::MockVectorDB;
use cognee_vector::{VectorDB, VectorPoint};
use serde_json::{Value, json};
use uuid::Uuid;

#[tokio::test]
async fn source_provenance_does_not_reach_the_payload() {
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

    let refs = IndexRefs {
        document_id: Some(document_id),
        chunk_index: Some(0),
        ..IndexRefs::default()
    };
    let point = VectorPoint {
        id: chunk.base.id,
        vector: vec![0.0; 4],
        metadata: index_schema_payload(&chunk.base, chunk.text.clone(), &refs, None),
    };

    let db = MockVectorDB::new();
    db.create_collection("DocumentChunk", "text", 4)
        .await
        .unwrap();
    db.index_points("DocumentChunk", "text", &[point])
        .await
        .unwrap();

    let stored = db
        .get_payload("DocumentChunk", "text", chunk.base.id)
        .expect("indexed point must round-trip through MockVectorDB");

    for key in [
        "source_pipeline",
        "source_task",
        "source_user",
        "source_node_set",
        "source_content_hash",
    ] {
        assert_eq!(stored.get(key), Some(&Value::Null), "{key}");
    }
    assert_eq!(stored.get("type"), Some(&json!("IndexSchema")));
    assert_eq!(stored.get("text"), Some(&json!("hello")));
    assert_eq!(
        stored.get("document_id"),
        Some(&json!(document_id.to_string()))
    );
}
