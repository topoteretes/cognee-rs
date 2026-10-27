#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! The database edge-dedup check must look an edge up under the identity the
//! expansion pass actually stores it with (issues #75 and #76).
//!
//! Python builds every extracted edge's final `EdgeIdentity` first —
//! `generate_edge_name(relationship)` between the *resolved* endpoint entity
//! ids — and asks the graph which of exactly those identities exist
//! (`construct_data_points_and_edges` → `find_existing_edge_identities`). A
//! lookup key derived independently from the raw extraction diverges from the
//! stored edge whenever the two derivations disagree: a case/space-variant
//! relationship, or a node id that differs from the node name the entity id is
//! minted from. Either way the second run's copy misses the stored edge and is
//! written again.
//!
//! `MockGraphDB` appends on every write, so a missed dedup shows up here as a
//! second stored copy of one edge.
//!
//! Offline throughout — see `rollback_harness`.

mod rollback_harness;

use std::sync::Arc;

use cognee_graph::GraphDBTrait;
use cognee_models::Entity;
use cognee_test_utils::MockLlm;
use rollback_harness::{Harness, extraction_config};
use serde_json::json;
use uuid::Uuid;

/// One `Alice -relationship-> Acme` extraction, with caller-chosen node ids.
fn graph_response(alice_id: &str, acme_id: &str, relationship: &str) -> String {
    json!({
        "nodes": [
            {"id": alice_id, "name": "Alice", "type": "PERSON", "description": "A person."},
            {"id": acme_id, "name": "Acme", "type": "ORGANIZATION", "description": "A company."}
        ],
        "edges": [{
            "source_node_id": alice_id,
            "target_node_id": acme_id,
            "relationship_name": relationship,
            "description": "Alice works at Acme."
        }]
    })
    .to_string()
}

fn llm_answering(response: String) -> Arc<dyn cognee_llm::Llm> {
    Arc::new(MockLlm::new(vec![response; 6]))
}

/// Two files, one run each, extracting the same fact with the two responses.
async fn two_runs(first: String, second: String) -> (Harness, Uuid, Uuid) {
    let mut harness = Harness::new().await;
    let first_file = harness.add_file("Alice works at Acme.").await;
    let second_file = harness.add_file("Alice is employed by Acme.").await;

    harness
        .run_over(&extraction_config(), &[first_file], llm_answering(first))
        .await
        .expect("run 1 completes");
    harness
        .run_over(&extraction_config(), &[second_file], llm_answering(second))
        .await
        .expect("run 2 completes");

    (harness, first_file, second_file)
}

/// Every stored `Alice -> Acme` edge, as its relationship name.
async fn stored_alice_acme_relationships(harness: &Harness) -> Vec<String> {
    let alice = Entity::id_for("Alice").to_string();
    let acme = Entity::id_for("Acme").to_string();
    let (_nodes, edges) = harness
        .graph_db
        .get_graph_data()
        .await
        .expect("read the graph store");
    edges
        .into_iter()
        .filter(|(source, target, _, _)| *source == alice && *target == acme)
        .map(|(_, _, relationship, _)| relationship)
        .collect()
}

/// The data items holding an ownership row for the stored `Alice -> Acme` edge.
async fn alice_acme_claimants(harness: &Harness) -> Vec<Uuid> {
    let alice = Entity::id_for("Alice");
    let acme = Entity::id_for("Acme");
    let mut claimants: Vec<Uuid> = harness
        .ledger_edges()
        .await
        .into_iter()
        .filter(|edge| edge.source_node_id == alice && edge.destination_node_id == acme)
        .map(|edge| edge.data_id)
        .collect();
    claimants.sort();
    claimants.dedup();
    claimants
}

/// The relationship is stored under `generate_edge_name`, as Python stores it.
#[tokio::test]
async fn extracted_relationship_is_stored_normalized() {
    let mut harness = Harness::new().await;
    let file = harness.add_file("Alice works at Acme.").await;
    harness
        .run_over(
            &extraction_config(),
            &[file],
            llm_answering(graph_response("alice", "acme", "Works At")),
        )
        .await
        .expect("run completes");

    assert_eq!(
        stored_alice_acme_relationships(&harness).await,
        vec!["works_at".to_string()]
    );
}

/// #75: `works_at` and `Works At` are one relationship, so the second run's
/// copy is found in the graph instead of being written beside it.
#[tokio::test]
async fn case_variant_relationship_is_not_written_twice() {
    let (harness, first, second) = two_runs(
        graph_response("alice", "acme", "works_at"),
        graph_response("alice", "acme", "Works At"),
    )
    .await;

    assert_eq!(
        stored_alice_acme_relationships(&harness).await,
        vec!["works_at".to_string()],
        "one stored edge, under the normalized name"
    );
    let mut both = vec![first, second];
    both.sort();
    assert_eq!(
        alice_acme_claimants(&harness).await,
        both,
        "the second file still owns the edge it did not have to write"
    );
}

/// #76: the entity id derives from the node *name*, so an edge between nodes
/// whose ids differ from their names still names the stored edge. A lookup
/// keyed on `Entity::id_for(node_id)` never finds it.
#[tokio::test]
async fn edge_between_nodes_with_non_name_ids_is_not_written_twice() {
    let (harness, first, second) = two_runs(
        graph_response("alice", "acme", "works_at"),
        graph_response("person_1", "org_1", "works_at"),
    )
    .await;

    assert_eq!(
        stored_alice_acme_relationships(&harness).await,
        vec!["works_at".to_string()],
        "one stored edge"
    );
    let mut both = vec![first, second];
    both.sort();
    assert_eq!(alice_acme_claimants(&harness).await, both);
}
