#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Cognify writes no `Triplet_text` collection (SDK-1039, Python SDK-1115).
//!
//! Python stopped embedding triplets at cognify time; Rust follows with no
//! opt-in. The graph still gets its edges and the `EdgeType` collection still
//! gets its points, so the assertion is not vacuous.
//!
//! Offline throughout — see `rollback_harness`.

mod rollback_harness;

use std::sync::Arc;

use cognee_test_utils::MockLlm;
use cognee_vector::VectorDB;
use rollback_harness::{Harness, canned_graph_response, extraction_config};

#[tokio::test]
async fn cognify_writes_no_triplet_collection() {
    let mut harness = Harness::new().await;
    let file = harness.add_file("Alice works at Acme.").await;

    let result = harness
        .run_over(
            &extraction_config(),
            &[file],
            Arc::new(MockLlm::new(vec![canned_graph_response(); 6])),
        )
        .await
        .expect("cognify completes");

    assert!(
        !result.edges.is_empty(),
        "the fixture must yield edges, or the assertions below prove nothing"
    );
    assert!(
        harness
            .vector_db
            .has_collection("EdgeType", "relationship_name")
            .await
            .expect("collection lookup"),
        "the edges' EdgeType points are still indexed"
    );
    assert!(
        !harness
            .vector_db
            .has_collection("Triplet", "text")
            .await
            .expect("collection lookup"),
        "cognify must not write a Triplet collection"
    );
}
