#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! `brute_force_triplet_search` must match an `EdgeType` vector hit to the graph
//! edge it was derived from.
//!
//! The `EdgeType_relationship_name` rows cognify writes are keyed on each edge's
//! *retrieval text* — the nonblank `edge_text` property, falling back to the bare
//! `relationship_name` (`EdgeType::retrieval_text`, a port of Python's
//! `get_edge_retrieval_text`). The row's point id is
//! `EdgeType::deterministic_id(retrieval_text)` and its `relationship_name`
//! metadata holds that same retrieval text — *not* the bare relation label.
//!
//! The graph-retrieval lane used to build its distance map keyed on that metadata
//! string and then look it up by the graph edge's bare `relationship_name`. Those
//! two agree only for an edge with no description, and
//! `fact_extraction/models.rs` asks the LLM for a description on every edge — so
//! in practice every lookup missed and every edge silently took the 6.5
//! `triplet_distance_penalty`. Python matches by point id instead
//! (`CogneeGraph.py:58-61` stamps `edge_type_id`, `:392` looks it up), and Rust's
//! own hybrid lane already did the same.
//!
//! Run with:
//!   cargo test --package cognee-search --test edge_type_vector_match -- --nocapture

use async_trait::async_trait;
use cognee_embedding::{EmbeddingEngine, EmbeddingResult};
use cognee_graph::{GraphDBTrait, MockGraphDB};
use cognee_models::EdgeType;
use cognee_search::graph_retrieval::{GraphRetrievalConfig, brute_force_triplet_search};
use cognee_vector::{MockVectorDB, VectorDB, VectorPoint};
use serde_json::{Value, json};
use std::borrow::Cow;
use std::collections::HashMap;
use uuid::Uuid;

/// Every text embeds to the same 2-D unit vector, so a stored point's own vector
/// alone decides its cosine similarity to the query and every distance in these
/// tests is exact and model-free.
struct AlignedEmbedding;

#[async_trait]
impl EmbeddingEngine for AlignedEmbedding {
    async fn embed(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
    }
    fn dimension(&self) -> usize {
        2
    }
    fn batch_size(&self) -> usize {
        8
    }
    fn max_sequence_length(&self) -> usize {
        128
    }
}

/// Cosine similarity `0.6` against the query vector `[1.0, 0.0]`, i.e. cosine
/// distance `0.4` — the value the matched edge must contribute.
const EDGE_VECTOR: [f32; 2] = [0.6, 0.8];
const EDGE_DISTANCE: f32 = 0.4;

/// The penalty an unmatched edge takes, mirroring Python's
/// `triplet_distance_penalty`.
const PENALTY: f32 = 6.5;

fn props(pairs: &[(&'static str, serde_json::Value)]) -> HashMap<Cow<'static, str>, Value> {
    pairs
        .iter()
        .map(|(k, v)| (Cow::Borrowed(*k), v.clone()))
        .collect()
}

/// Seed two entities that both embed onto the query vector (distance `0.0`
/// each), so an edge's total score is exactly its own edge distance.
async fn seed_entities(vector_db: &MockVectorDB, graph: &MockGraphDB, ids: &[(Uuid, &str)]) {
    vector_db
        .create_collection("Entity", "name", 2)
        .await
        .unwrap();
    let points: Vec<VectorPoint> = ids
        .iter()
        .map(|(id, name)| {
            VectorPoint::new(*id, vec![1.0, 0.0])
                .with_metadata("id", json!(id.to_string()))
                .with_metadata("name", json!(*name))
        })
        .collect();
    vector_db
        .index_points("Entity", "name", &points)
        .await
        .unwrap();

    for (id, name) in ids {
        graph
            .add_node_raw(json!({ "id": id.to_string(), "name": name }))
            .await
            .unwrap();
    }
}

/// Index one `EdgeType` row exactly as cognify's `tasks.rs` does: the point id is
/// `EdgeType::deterministic_id(retrieval_text)` and the `relationship_name`
/// metadata carries the retrieval text.
async fn index_edge_type(vector_db: &MockVectorDB, retrieval_text: &str, vector: Vec<f32>) {
    if !vector_db
        .has_collection("EdgeType", "relationship_name")
        .await
        .unwrap()
    {
        vector_db
            .create_collection("EdgeType", "relationship_name", 2)
            .await
            .unwrap();
    }
    let id = EdgeType::deterministic_id(retrieval_text);
    vector_db
        .index_points(
            "EdgeType",
            "relationship_name",
            &[VectorPoint::new(id, vector)
                .with_metadata("id", json!(id.to_string()))
                .with_metadata("relationship_name", json!(retrieval_text))],
        )
        .await
        .unwrap();
}

fn config() -> GraphRetrievalConfig {
    GraphRetrievalConfig {
        top_k: 100,
        ..Default::default()
    }
}

fn score_of(
    ranked: &[cognee_search::graph_retrieval::RankedGraphEdge],
    relationship_name: &str,
) -> f32 {
    ranked
        .iter()
        .find(|edge| edge.relationship_name == relationship_name)
        .unwrap_or_else(|| {
            panic!(
                "no ranked edge named {relationship_name:?}; got {:?}",
                ranked
                    .iter()
                    .map(|e| e.relationship_name.as_str())
                    .collect::<Vec<_>>()
            )
        })
        .score
}

/// **Regression case.** An edge carrying an LLM-written `edge_text` description —
/// so its retrieval text differs from its bare `relationship_name` — must receive
/// the `EdgeType` row's cosine distance, not the penalty.
///
/// Before the fix the distance map was keyed on the row's `relationship_name`
/// metadata (`"Alice works at Acme Corp as a staff engineer"`) while the lookup
/// used the graph edge's `"works_at"`, so the hit was never found and the edge
/// scored `6.5` instead of `0.4`.
#[tokio::test]
async fn described_edge_takes_its_vector_distance_not_the_penalty() {
    let alice = Uuid::from_u128(0x01);
    let acme = Uuid::from_u128(0x02);
    const DESCRIPTION: &str = "Alice works at Acme Corp as a staff engineer";

    let vector_db = MockVectorDB::new();
    let graph = MockGraphDB::new();
    seed_entities(&vector_db, &graph, &[(alice, "Alice"), (acme, "Acme Corp")]).await;

    // The row cognify would have written for this edge.
    index_edge_type(&vector_db, DESCRIPTION, EDGE_VECTOR.to_vec()).await;

    graph
        .add_edge(
            &alice.to_string(),
            &acme.to_string(),
            "works_at",
            Some(props(&[("edge_text", json!(DESCRIPTION))])),
        )
        .await
        .unwrap();
    // A second edge with no `EdgeType` row at all: it must still take the
    // penalty, so a fix that simply stopped penalising anything would fail here.
    graph
        .add_edge(
            &alice.to_string(),
            &acme.to_string(),
            "mentions",
            Some(props(&[("edge_text", json!("Alice mentions Acme Corp"))])),
        )
        .await
        .unwrap();

    let ranked = brute_force_triplet_search(
        "where does alice work",
        &vector_db,
        &AlignedEmbedding,
        &graph,
        &config(),
    )
    .await
    .unwrap();

    let matched = score_of(&ranked, "works_at");
    assert!(
        (matched - EDGE_DISTANCE).abs() < 1e-6,
        "the described edge must score 0.0 (source) + 0.0 (target) + {EDGE_DISTANCE} \
         (its EdgeType vector distance) = {EDGE_DISTANCE}, got {matched}. A score of \
         {} means the EdgeType hit was looked up by the bare relationship_name and \
         missed, so the edge fell through to the triplet_distance_penalty.",
        PENALTY
    );

    let unmatched = score_of(&ranked, "mentions");
    assert!(
        (unmatched - PENALTY).abs() < 1e-6,
        "an edge with no EdgeType row must still take the {PENALTY} penalty, got {unmatched}"
    );
}

/// The fallback half of `EdgeType::retrieval_text`: a blank `edge_text` means the
/// row was keyed on the bare `relationship_name`, and the lookup must agree.
#[tokio::test]
async fn blank_edge_text_falls_back_to_relationship_name_and_still_matches() {
    let alice = Uuid::from_u128(0x11);
    let bob = Uuid::from_u128(0x12);

    let vector_db = MockVectorDB::new();
    let graph = MockGraphDB::new();
    seed_entities(&vector_db, &graph, &[(alice, "Alice"), (bob, "Bob")]).await;

    // cognify keys the row on `retrieval_text(Some("   "), "knows") == "knows"`.
    index_edge_type(&vector_db, "knows", EDGE_VECTOR.to_vec()).await;

    graph
        .add_edge(
            &alice.to_string(),
            &bob.to_string(),
            "knows",
            Some(props(&[("edge_text", json!("   "))])),
        )
        .await
        .unwrap();

    let ranked = brute_force_triplet_search(
        "who does alice know",
        &vector_db,
        &AlignedEmbedding,
        &graph,
        &config(),
    )
    .await
    .unwrap();

    let matched = score_of(&ranked, "knows");
    assert!(
        (matched - EDGE_DISTANCE).abs() < 1e-6,
        "a blank edge_text must fall back to the relationship_name, so the edge \
         scores {EDGE_DISTANCE}, got {matched}"
    );
}

/// The same fallback when the edge carries no `edge_text` property at all.
#[tokio::test]
async fn absent_edge_text_falls_back_to_relationship_name_and_still_matches() {
    let alice = Uuid::from_u128(0x21);
    let bob = Uuid::from_u128(0x22);

    let vector_db = MockVectorDB::new();
    let graph = MockGraphDB::new();
    seed_entities(&vector_db, &graph, &[(alice, "Alice"), (bob, "Bob")]).await;

    index_edge_type(&vector_db, "knows", EDGE_VECTOR.to_vec()).await;

    graph
        .add_edge(&alice.to_string(), &bob.to_string(), "knows", None)
        .await
        .unwrap();

    let ranked = brute_force_triplet_search(
        "who does alice know",
        &vector_db,
        &AlignedEmbedding,
        &graph,
        &config(),
    )
    .await
    .unwrap();

    let matched = score_of(&ranked, "knows");
    assert!(
        (matched - EDGE_DISTANCE).abs() < 1e-6,
        "an edge with no edge_text property keys on its relationship_name, so it \
         scores {EDGE_DISTANCE}, got {matched}"
    );
}
