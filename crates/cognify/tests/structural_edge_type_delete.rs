#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Structural edges (`is_part_of`, `contains`, `is_a`, ...) carry a nil
//! `data_id` in the ownership ledger, so a data-scoped delete never selected
//! them and the `EdgeType` points keyed on their retrieval text outlived the
//! item. These tests run the real cognify path and the real delete service and
//! assert on the points themselves.
//!
//! The `EdgeType` id is many-to-one over edges, so the second half of the
//! contract matters as much as the first: a point a surviving item's edge
//! still needs must not go.
//!
//! Offline throughout — see `rollback_harness`.

mod rollback_harness;

use std::collections::BTreeSet;
use std::sync::Arc;

use cognee_database::DeleteDb;
use cognee_delete::{DeleteMode, DeleteRequest, DeleteScope, DeleteService};
use cognee_graph::GraphDBTrait;
use cognee_models::EdgeType;
use cognee_test_utils::MockLlm;
use cognee_vector::VectorDB;
use rollback_harness::{Harness, canned_graph_response, extraction_config};
use uuid::Uuid;

fn llm(files: usize) -> Arc<dyn cognee_llm::Llm> {
    Arc::new(MockLlm::new(vec![canned_graph_response(); files * 2 + 4]))
}

async fn delete_item(harness: &Harness, data_id: Uuid) {
    DeleteService::new(
        Arc::clone(&harness.storage),
        harness.db.clone() as Arc<dyn DeleteDb>,
    )
    .with_graph_db(Arc::clone(&harness.graph_db) as Arc<dyn GraphDBTrait>)
    .with_vector_db(Arc::clone(&harness.vector_db) as Arc<dyn VectorDB>)
    .execute(&DeleteRequest {
        scope: DeleteScope::DataInDataset {
            dataset_id: harness.dataset_id,
            data_id,
            delete_dataset_if_empty: false,
        },
        mode: DeleteMode::Soft,
        memory_only: false,
    })
    .await
    .expect("delete succeeds");
}

async fn edge_type_points(harness: &Harness) -> usize {
    harness
        .vector_db
        .collection_size("EdgeType", "relationship_name")
        .await
        .unwrap_or(0)
}

fn has_edge_type_point(harness: &Harness, text: &str) -> bool {
    harness
        .vector_db
        .get_payload(
            "EdgeType",
            "relationship_name",
            EdgeType::deterministic_id(text),
        )
        .is_some()
}

/// The distinct retrieval texts the ledger's edge rows still claim.
async fn claimed_texts(harness: &Harness) -> BTreeSet<String> {
    harness
        .ledger_edges()
        .await
        .iter()
        .map(|row| {
            EdgeType::retrieval_text(
                row.attributes
                    .as_ref()
                    .and_then(|attributes| attributes.get("edge_text"))
                    .and_then(serde_json::Value::as_str),
                &row.relationship_name,
            )
        })
        .collect()
}

/// The regression: one item's structural `EdgeType` points are removed with it.
#[tokio::test]
async fn deleting_one_item_removes_its_structural_edge_type_points() {
    let mut harness = Harness::new().await;
    let data_id = harness.add_file("Alice works at Acme.").await;
    harness
        .run_over(&extraction_config(), &[data_id], llm(1))
        .await
        .expect("cognify succeeds");

    // Preconditions: the structural rows exist, carry a nil `data_id` (the
    // thing under test), and have their points.
    let structural: Vec<_> = harness
        .ledger_edges()
        .await
        .into_iter()
        .filter(|row| row.data_id.is_nil())
        .collect();
    assert!(
        structural
            .iter()
            .any(|row| row.relationship_name == "is_part_of"),
        "cognify records the structural is_part_of row with a nil data_id"
    );
    assert!(has_edge_type_point(&harness, "alice is a person."));
    assert!(has_edge_type_point(
        &harness,
        "Alice works at Acme. is part of rollback-0.txt."
    ));

    delete_item(&harness, data_id).await;

    assert_eq!(
        edge_type_points(&harness).await,
        0,
        "no EdgeType point outlives the only item that produced it"
    );
    assert!(
        harness.ledger_edges().await.is_empty(),
        "and no structural ledger row is left behind to claim a text"
    );
}

/// Two items share every structural text. Deleting one keeps exactly the
/// points the survivor's rows still claim; deleting the last removes them.
#[tokio::test]
async fn a_second_item_sharing_structural_edge_texts_keeps_its_points() {
    let mut harness = Harness::new().await;
    let first = harness.add_file("Alice works at Acme.").await;
    let second = harness.add_file("Alice also works at Acme.").await;
    harness
        .run_over(&extraction_config(), &[first, second], llm(2))
        .await
        .expect("cognify succeeds");
    let before = edge_type_points(&harness).await;
    assert!(before > 0);

    delete_item(&harness, first).await;

    let claimed = claimed_texts(&harness).await;
    // `alice is a person.` is one text two items' rows share; the `is_part_of`
    // sentence names the file, so each item has its own.
    assert!(
        claimed.contains("alice is a person.")
            && claimed.contains("Document chunk mentions alice: A person.")
            && claimed.contains("Alice also works at Acme. is part of rollback-1.txt."),
        "the survivor's structural rows are still in the ledger: {claimed:?}"
    );
    assert!(
        !has_edge_type_point(&harness, "Alice works at Acme. is part of rollback-0.txt."),
        "the deleted item's own is_part_of point is gone"
    );
    for text in &claimed {
        assert!(
            has_edge_type_point(&harness, text),
            "the point for {text:?} is still claimed by the second item"
        );
    }
    assert_eq!(
        edge_type_points(&harness).await,
        claimed.len(),
        "and nothing the first item alone claimed is left over"
    );

    delete_item(&harness, second).await;

    assert_eq!(edge_type_points(&harness).await, 0);
    assert!(harness.ledger_edges().await.is_empty());
}
