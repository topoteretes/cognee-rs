#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Ownership of an edge a later run produced but did not have to write.
//!
//! Extraction filters out edges the graph already holds — that is what
//! `retrieve_existing_edges` is for. The filter used to drop the edge
//! completely, so the second file that produced it got no ownership row and
//! the first file's row was the only claim on it. Deleting the first file then
//! found the edge *exclusively* owned, and took its Triplet / EdgeType vectors
//! with it, out from under a second file whose graph still references it.
//!
//! That is the same cross-file damage per-producer ownership rows exist to
//! prevent, re-created one run later. These tests pin the claim, the
//! exclusivity verdict it changes, and what a delete of the first file leaves
//! behind.
//!
//! Offline throughout — see `rollback_harness`.

mod rollback_harness;

use std::collections::BTreeSet;
use std::sync::Arc;

use cognee_database::DeleteDb;
use cognee_database::ops::graph_storage::get_unique_edges_for_data;
use cognee_delete::{DeleteMode, DeleteRequest, DeleteScope, DeleteService};
use cognee_graph::GraphDBTrait;
use cognee_models::Triplet;
use cognee_test_utils::MockLlm;
use cognee_vector::VectorDB;
use rollback_harness::{Harness, canned_graph_response, extraction_config};
use uuid::Uuid;

/// The relationship every file in `canned_graph_response` produces.
const SHARED_RELATIONSHIP: &str = "works_at";

/// A clean LLM: canned graph responses, no failing markers.
fn clean_llm(files: usize) -> Arc<dyn cognee_llm::Llm> {
    Arc::new(MockLlm::new(vec![canned_graph_response(); files * 2 + 4]))
}

/// The harness fixture with triplet embeddings on, so the shared edge actually
/// gets the `Triplet_text` point whose survival the delete test turns on. They
/// are off by default (Python parity), which would leave nothing to observe.
fn triplet_indexing_config() -> cognee_cognify::CognifyConfig {
    extraction_config().with_triplet_embeddings(true)
}

/// Two files, one per run. Both yield the same `Alice -works_at-> Acme` edge,
/// so the second run's copy is the one the DB dedup filter keeps out of the
/// graph write.
async fn two_runs_over_one_shared_edge() -> (Harness, Uuid, Uuid) {
    let mut harness = Harness::new().await;
    let first = harness.add_file("Alice works at Acme.").await;
    let second = harness.add_file("Carol also works at Acme.").await;

    harness
        .run_over(&triplet_indexing_config(), &[first], clean_llm(1))
        .await
        .expect("run 1 completes");
    harness
        .run_over(&triplet_indexing_config(), &[second], clean_llm(1))
        .await
        .expect("run 2 completes");

    (harness, first, second)
}

/// The ownership rows for the shared edge, one per claiming data item.
async fn shared_edge_rows(harness: &Harness) -> Vec<cognee_database::GraphEdge> {
    harness
        .ledger_edges()
        .await
        .into_iter()
        .filter(|edge| edge.relationship_name == SHARED_RELATIONSHIP)
        .collect()
}

/// The claim itself: the second run records ownership of the edge it did not
/// write, and the two rows agree on the slug — which is what makes the edge
/// shared rather than exclusive.
#[tokio::test]
async fn the_second_run_claims_the_edge_the_dedup_filter_kept_it_from_writing() {
    let (harness, first, second) = two_runs_over_one_shared_edge().await;

    let rows = shared_edge_rows(&harness).await;
    let owners: BTreeSet<Uuid> = rows.iter().map(|row| row.data_id).collect();
    assert_eq!(
        owners,
        BTreeSet::from([first, second]),
        "both files produced the edge, so both must own it — the second file's \
         copy was filtered out of the graph write, not out of the ledger"
    );

    let slugs: BTreeSet<Uuid> = rows.iter().map(|row| row.slug).collect();
    assert_eq!(
        slugs.len(),
        1,
        "the rows claim one and the same edge; exclusivity is decided on the slug"
    );
}

/// What the claim buys: the first file no longer owns the edge exclusively, so
/// the exclusivity query that drives deletion passes it over.
#[tokio::test]
async fn the_shared_edge_is_no_longer_exclusive_to_the_first_file() {
    let (harness, first, second) = two_runs_over_one_shared_edge().await;

    for (label, data_id) in [("first", first), ("second", second)] {
        let exclusive = get_unique_edges_for_data(&harness.db, data_id, harness.dataset_id)
            .await
            .expect("exclusivity query");
        assert!(
            !exclusive
                .iter()
                .any(|edge| edge.relationship_name == SHARED_RELATIONSHIP),
            "the {label} file must not own the shared edge exclusively — the other \
             file still references it"
        );
    }
}

/// End to end: deleting the file that first created the edge leaves the edge,
/// and the vectors that make it retrievable, in place for the file that still
/// references it.
#[tokio::test]
async fn deleting_the_first_file_leaves_the_shared_edge_for_the_second() {
    let (harness, first, _second) = two_runs_over_one_shared_edge().await;

    let row = shared_edge_rows(&harness)
        .await
        .into_iter()
        .next()
        .expect("the shared edge has an ownership row");
    let triplet_id = Triplet::new(
        row.source_node_id,
        row.destination_node_id,
        row.relationship_name.clone(),
        String::new(),
    )
    .id;

    let before = harness
        .vector_db
        .retrieve("Triplet", "text", &[triplet_id])
        .await
        .expect("retrieve the shared edge's Triplet point");
    assert_eq!(
        before.len(),
        1,
        "cognify indexed the shared edge, so there is something for the delete to take"
    );

    let service = DeleteService::new(
        Arc::clone(&harness.storage),
        Arc::clone(&harness.db) as Arc<dyn DeleteDb>,
    )
    .with_graph_db(Arc::clone(&harness.graph_db) as Arc<dyn GraphDBTrait>)
    .with_vector_db(Arc::clone(&harness.vector_db) as Arc<dyn VectorDB>);

    service
        .execute(&DeleteRequest {
            scope: DeleteScope::Data {
                owner_id: harness.owner_id,
                data_id: first,
                dataset_name: None,
                delete_dataset_if_empty: false,
            },
            mode: DeleteMode::Soft,
            memory_only: false,
        })
        .await
        .expect("delete the first file");

    let after = harness
        .vector_db
        .retrieve("Triplet", "text", &[triplet_id])
        .await
        .expect("retrieve the shared edge's Triplet point after the delete");
    assert_eq!(
        after.len(),
        1,
        "the second file still references the edge, so its Triplet point must survive \
         the first file's deletion"
    );

    let (_nodes, edges) = harness
        .graph_db
        .get_graph_data()
        .await
        .expect("read the graph store");
    assert!(
        edges.iter().any(|(source, target, relationship, _)| {
            source == &row.source_node_id.to_string()
                && target == &row.destination_node_id.to_string()
                && relationship == SHARED_RELATIONSHIP
        }),
        "…and the edge itself is still in the graph"
    );
}
