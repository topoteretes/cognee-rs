#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Graph triplet ranking weighs each element's `importance_weight` and
//! `feedback_weight` the way Python's
//! `CogneeGraph._calculate_query_top_triplet_importances` does.
//!
//! Fixture: a hub `H` (no vector, so it takes the 6.5 penalty) with one edge to
//! each of two entities, `A` and `B`, that are equally similar to the query
//! (cosine distance 0.4). `B` has the smaller id, so on a tie it would rank
//! first; every test makes `A` win, which only its weights can do.
//!
//! Run with:
//!   cargo test --package cognee-search --test triplet_importance_feedback -- --nocapture

use std::collections::HashMap;

use async_trait::async_trait;
use cognee_embedding::{EmbeddingEngine, EmbeddingResult};
use cognee_graph::{GraphDBTrait, MockGraphDB};
use cognee_models::EdgeType;
use cognee_search::graph_retrieval::{
    GraphRetrievalConfig, RankedGraphEdge, brute_force_triplet_search,
};
use cognee_vector::{MockVectorDB, VectorDB, VectorPoint};
use serde_json::{Value, json};
use uuid::Uuid;

/// Every query embeds to `[1, 0]`.
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

/// Cosine distance 0.4 from the query.
const NEAR: [f32; 2] = [0.6, 0.8];
/// The default penalty, scaled by `2 - 0.5` (the default importance).
const SCALED_PENALTY: f32 = 1.5 * 6.5;

const HUB: Uuid = Uuid::from_u128(0xF0);
const A: Uuid = Uuid::from_u128(0x02);
const B: Uuid = Uuid::from_u128(0x01);

/// Extra node properties for `A` and `B`, and extra edge properties for
/// `H → A` (`to_a`) and `H → B` (`to_b`).
#[derive(Default)]
struct Weights {
    a_node: Vec<(&'static str, Value)>,
    b_node: Vec<(&'static str, Value)>,
    to_a: Vec<(&'static str, Value)>,
    to_b: Vec<(&'static str, Value)>,
    /// Index an `EdgeType` row at distance 0.4 for both relationships, so the
    /// edges carry a real distance that feedback can blend.
    edge_vectors: bool,
}

fn object(id: Uuid, name: &str, extra: &[(&'static str, Value)]) -> Value {
    let mut node = json!({ "id": id.to_string(), "name": name });
    for (key, value) in extra {
        node[*key] = value.clone();
    }
    node
}

async fn rank(weights: Weights, feedback_influence: f32) -> Vec<RankedGraphEdge> {
    let vector_db = MockVectorDB::new();
    vector_db
        .create_collection("Entity", "name", 2)
        .await
        .unwrap();
    let points: Vec<VectorPoint> = [A, B]
        .iter()
        .map(|id| VectorPoint::new(*id, NEAR.to_vec()).with_metadata("id", json!(id.to_string())))
        .collect();
    vector_db
        .index_points("Entity", "name", &points)
        .await
        .unwrap();
    if weights.edge_vectors {
        vector_db
            .create_collection("EdgeType", "relationship_name", 2)
            .await
            .unwrap();
        let rows: Vec<VectorPoint> = ["to_a", "to_b"]
            .iter()
            .map(|rel| {
                let id = EdgeType::deterministic_id(rel);
                VectorPoint::new(id, NEAR.to_vec())
                    .with_metadata("id", json!(id.to_string()))
                    .with_metadata("relationship_name", json!(rel))
            })
            .collect();
        vector_db
            .index_points("EdgeType", "relationship_name", &rows)
            .await
            .unwrap();
    }

    let graph = MockGraphDB::new();
    for node in [
        object(HUB, "H", &[]),
        object(A, "A", &weights.a_node),
        object(B, "B", &weights.b_node),
    ] {
        graph.add_node_raw(node).await.unwrap();
    }
    for (target, rel, extra) in [(A, "to_a", &weights.to_a), (B, "to_b", &weights.to_b)] {
        let props = (!extra.is_empty()).then(|| {
            extra
                .iter()
                .map(|(key, value)| ((*key).into(), value.clone()))
                .collect::<HashMap<_, _>>()
        });
        graph
            .add_edge(&HUB.to_string(), &target.to_string(), rel, props)
            .await
            .unwrap();
    }

    let config = GraphRetrievalConfig {
        top_k: 10,
        feedback_influence,
        ..Default::default()
    };
    brute_force_triplet_search("q", &vector_db, &AlignedEmbedding, &graph, &config)
        .await
        .unwrap()
}

fn order_and_scores(ranked: &[RankedGraphEdge]) -> Vec<(&str, f32)> {
    ranked
        .iter()
        .map(|edge| (edge.relationship_name.as_str(), edge.score))
        .collect()
}

fn assert_ranking(ranked: &[RankedGraphEdge], expected: &[(&str, f32)]) {
    let actual = order_and_scores(ranked);
    assert_eq!(actual.len(), expected.len(), "{actual:?}");
    for ((rel, score), (want_rel, want_score)) in actual.iter().zip(expected) {
        assert_eq!(rel, want_rel, "order: {actual:?}");
        assert!(
            (score - want_score).abs() < 1e-4,
            "{rel}: expected {want_score}, got {score} ({actual:?})"
        );
    }
}

/// Without weights the two edges tie and the id tie-break puts `to_b` first —
/// the baseline the other tests overturn.
#[tokio::test]
async fn equal_weights_tie_on_the_id() {
    let ranked = rank(Weights::default(), 0.0).await;
    let tied = 2.0 * SCALED_PENALTY + 1.5 * 0.4;
    assert_ranking(&ranked, &[("to_b", tied), ("to_a", tied)]);
}

/// `importance_weight` scales each distance by `2 - w`: 1.0 keeps A's 0.4,
/// 0.0 doubles B's to 0.8. A string weight reads like Python's `float()`.
#[tokio::test]
async fn the_more_important_entity_ranks_first() {
    let ranked = rank(
        Weights {
            a_node: vec![("importance_weight", json!("1.0"))],
            b_node: vec![("importance_weight", json!(0.0))],
            ..Default::default()
        },
        0.0,
    )
    .await;
    assert_ranking(
        &ranked,
        &[
            ("to_a", 2.0 * SCALED_PENALTY + 0.4),
            ("to_b", 2.0 * SCALED_PENALTY + 0.8),
        ],
    );
}

/// Positive feedback lowers a node's distance. With influence 0.5 the scaled
/// 0.6 blends to `(0.5 * 0.3 + 0.5 * (1 - w)) * 2`: 0.3 for w = 1, 1.3 for
/// w = 0. The penalized hub and edge are not blended.
#[tokio::test]
async fn positively_rated_node_ranks_first() {
    let ranked = rank(
        Weights {
            a_node: vec![("feedback_weight", json!(1.0))],
            b_node: vec![("feedback_weight", json!(0.0))],
            ..Default::default()
        },
        0.5,
    )
    .await;
    assert_ranking(
        &ranked,
        &[
            ("to_a", 2.0 * SCALED_PENALTY + 0.3),
            ("to_b", 2.0 * SCALED_PENALTY + 1.3),
        ],
    );
}

/// Edge `feedback_weight` is blended too, once the edge has a real distance.
/// Both nodes blend at the default 0.5 to 0.8; the edges to 0.3 and 1.3.
#[tokio::test]
async fn positively_rated_edge_ranks_first() {
    let ranked = rank(
        Weights {
            to_a: vec![("feedback_weight", json!(1.0))],
            to_b: vec![("feedback_weight", json!(0.0))],
            edge_vectors: true,
            ..Default::default()
        },
        0.5,
    )
    .await;
    assert_ranking(
        &ranked,
        &[
            ("to_a", SCALED_PENALTY + 0.8 + 0.3),
            ("to_b", SCALED_PENALTY + 0.8 + 1.3),
        ],
    );
}

/// Feedback weights are ignored when feedback has no influence.
#[tokio::test]
async fn feedback_is_inert_at_zero_influence() {
    let ranked = rank(
        Weights {
            a_node: vec![("feedback_weight", json!(1.0))],
            b_node: vec![("feedback_weight", json!(0.0))],
            ..Default::default()
        },
        0.0,
    )
    .await;
    let tied = 2.0 * SCALED_PENALTY + 1.5 * 0.4;
    assert_ranking(&ranked, &[("to_b", tied), ("to_a", tied)]);
}
