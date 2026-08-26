#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Tests that `brute_force_triplet_search` scopes its graph load to the
//! neighborhood of the vector-search seed set instead of pulling the whole
//! graph, **without changing which edges come back**.
//!
//! The unfiltered path used to call `get_graph_data()`, materialising every
//! node and edge row on every graph query, and then discarded every edge with
//! no seed endpoint a few lines later. These tests pin both halves of the fix:
//!
//! - the scoped adapter method is the one actually called, and
//! - the ranked output is byte-identical to what the full-graph load produced,
//!   including the discriminating case of an edge between two *non-seed*
//!   neighbours, which `get_neighborhood(seeds, 1)` returns but the edge filter
//!   must still drop.
//!
//! Run with:
//!   cargo test --package cognee-search --test graph_load_scoping -- --nocapture

use std::sync::Arc;

use async_trait::async_trait;
use cognee_embedding::{EmbeddingEngine, EmbeddingResult};
use cognee_graph::{GraphDBTrait, MockGraphDB};
use cognee_search::graph_retrieval::{GraphRetrievalConfig, brute_force_triplet_search};
use cognee_vector::{MockVectorDB, VectorDB, VectorPoint};
use serde_json::json;
use uuid::Uuid;

/// Maps every input to the same 2-D unit vector, so every seeded point is an
/// exact nearest neighbour and retrieval is deterministic with no model.
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

/// Node ids for the fixture graph. `a`/`b` are seeds (indexed as `Entity`
/// vectors); `c`/`d` are one hop out; `e`/`f` are a disconnected component that
/// no query should ever touch.
struct Fixture {
    a: Uuid,
    b: Uuid,
    c: Uuid,
    d: Uuid,
    e: Uuid,
    f: Uuid,
}

/// Build the fixture:
///
/// ```text
///   a --a_to_b--> b        (both seeds)
///   a --a_to_c--> c        (one seed endpoint)
///   b --b_to_d--> d        (one seed endpoint)
///   c --c_to_d--> d        (NO seed endpoint — in the depth-1 neighborhood,
///                           but must be filtered out)
///   e --e_to_f--> f        (disconnected — never loaded, never returned)
/// ```
///
/// Only `a` and `b` are indexed as vectors, so only they become seeds.
async fn seed() -> (Arc<dyn VectorDB>, Arc<MockGraphDB>, Fixture) {
    let fx = Fixture {
        a: Uuid::new_v4(),
        b: Uuid::new_v4(),
        c: Uuid::new_v4(),
        d: Uuid::new_v4(),
        e: Uuid::new_v4(),
        f: Uuid::new_v4(),
    };

    let vector_db = MockVectorDB::new();
    vector_db
        .create_collection("Entity", "name", 2)
        .await
        .unwrap();
    vector_db
        .index_points(
            "Entity",
            "name",
            &[
                VectorPoint::new(fx.a, vec![1.0, 0.0])
                    .with_metadata("id", json!(fx.a.to_string()))
                    .with_metadata("name", json!("A")),
                VectorPoint::new(fx.b, vec![1.0, 0.0])
                    .with_metadata("id", json!(fx.b.to_string()))
                    .with_metadata("name", json!("B")),
            ],
        )
        .await
        .unwrap();

    let graph = MockGraphDB::new();
    for (id, name) in [
        (fx.a, "A"),
        (fx.b, "B"),
        (fx.c, "C"),
        (fx.d, "D"),
        (fx.e, "E"),
        (fx.f, "F"),
    ] {
        graph
            .add_node_raw(json!({ "id": id.to_string(), "name": name }))
            .await
            .unwrap();
    }

    for (src, tgt, rel) in [
        (fx.a, fx.b, "a_to_b"),
        (fx.a, fx.c, "a_to_c"),
        (fx.b, fx.d, "b_to_d"),
        (fx.c, fx.d, "c_to_d"),
        (fx.e, fx.f, "e_to_f"),
    ] {
        graph
            .add_edge(&src.to_string(), &tgt.to_string(), rel, None)
            .await
            .unwrap();
    }

    (Arc::new(vector_db), Arc::new(graph), fx)
}

fn config() -> GraphRetrievalConfig {
    GraphRetrievalConfig {
        top_k: 100,
        ..Default::default()
    }
}

/// The scoped adapter method is the one called — the full-graph scan is gone.
#[tokio::test]
async fn unfiltered_path_loads_the_neighborhood_not_the_whole_graph() {
    let (vector_db, graph, _fx) = seed().await;

    brute_force_triplet_search(
        "any query",
        vector_db.as_ref(),
        &AlignedEmbedding,
        graph.as_ref(),
        &config(),
    )
    .await
    .unwrap();

    let calls = graph.get_call_log();
    assert!(
        calls.contains(&"get_neighborhood".to_string()),
        "expected the seed-scoped load; call log was {calls:?}"
    );
    assert!(
        !calls.contains(&"get_graph_data".to_string()),
        "get_graph_data materialises every node and edge row — it must not be \
         called on the unfiltered search path; call log was {calls:?}"
    );
}

/// The discriminating case: scoping must not change the result set.
///
/// `get_neighborhood(seeds, 1)` returns `c --c_to_d--> d` because both endpoints
/// are one hop from a seed, so this asserts the edge filter still drops it —
/// exactly as it did when the full graph was loaded. It also pins that no edge
/// with a seed endpoint was lost, which is the failure mode of scoping via
/// `get_id_filtered_graph_data` (which requires *both* endpoints in the seed set).
#[tokio::test]
async fn scoped_load_returns_exactly_the_edges_the_full_scan_did() {
    let (vector_db, graph, fx) = seed().await;

    // Pin the precondition this test depends on: the scoped load really does
    // hand back c_to_d, whose endpoints are both non-seed. Without this the
    // assertion below would pass even if the load had never produced the edge,
    // and the filter — the thing that makes the swap result-identical — would
    // go untested.
    let (_, loaded) = graph
        .get_neighborhood(&[fx.a.to_string(), fx.b.to_string()], 1)
        .await
        .unwrap();
    let loaded_rels: Vec<&str> = loaded.iter().map(|(_, _, rel, _)| rel.as_str()).collect();
    assert!(
        loaded_rels.contains(&"c_to_d"),
        "precondition: the depth-1 neighborhood of {{a, b}} must include the \
         neighbor-to-neighbor edge c_to_d, so that the ranking filter is what \
         removes it. Loaded: {loaded_rels:?}"
    );
    assert!(
        !loaded_rels.contains(&"e_to_f"),
        "the disconnected component must not be loaded at all. Loaded: {loaded_rels:?}"
    );

    let ranked = brute_force_triplet_search(
        "any query",
        vector_db.as_ref(),
        &AlignedEmbedding,
        graph.as_ref(),
        &config(),
    )
    .await
    .unwrap();

    let mut rels: Vec<String> = ranked
        .iter()
        .map(|edge| edge.relationship_name.clone())
        .collect();
    rels.sort();

    assert_eq!(
        rels,
        vec![
            "a_to_b".to_string(),
            "a_to_c".to_string(),
            "b_to_d".to_string()
        ],
        "expected exactly the edges with at least one seed endpoint: a_to_b \
         (both seeds), a_to_c and b_to_d (one seed each). c_to_d has no seed \
         endpoint and e_to_f is disconnected — both must be absent."
    );
}

/// Node property lookups still resolve for non-seed endpoints. The scoped load
/// returns `seeds ∪ N(seeds)`, so `c` and `d` are present and their `name`
/// resolves; a regression here would surface raw uuids as node names.
#[tokio::test]
async fn non_seed_endpoints_keep_their_names() {
    let (vector_db, graph, fx) = seed().await;

    let ranked = brute_force_triplet_search(
        "any query",
        vector_db.as_ref(),
        &AlignedEmbedding,
        graph.as_ref(),
        &config(),
    )
    .await
    .unwrap();

    let a_to_c = ranked
        .iter()
        .find(|edge| edge.relationship_name == "a_to_c")
        .expect("a_to_c has a seed endpoint and must be returned");

    assert_eq!(a_to_c.source_name, "A");
    assert_eq!(
        a_to_c.target_name, "C",
        "C is one hop from a seed, so the scoped load must include its row; \
         falling back to the raw id ({}) means the node half was dropped",
        fx.c
    );
}

/// A graph with no edges touching the seed set yields nothing, and still does
/// not fall back to a full scan.
#[tokio::test]
async fn seeds_with_no_incident_edges_return_empty_without_a_full_scan() {
    let vector_db = MockVectorDB::new();
    vector_db
        .create_collection("Entity", "name", 2)
        .await
        .unwrap();
    let orphan = Uuid::new_v4();
    vector_db
        .index_points(
            "Entity",
            "name",
            &[VectorPoint::new(orphan, vec![1.0, 0.0])
                .with_metadata("id", json!(orphan.to_string()))
                .with_metadata("name", json!("Orphan"))],
        )
        .await
        .unwrap();

    let graph = MockGraphDB::new();
    graph
        .add_node_raw(json!({ "id": orphan.to_string(), "name": "Orphan" }))
        .await
        .unwrap();
    let far_src = Uuid::new_v4();
    let far_tgt = Uuid::new_v4();
    for (id, name) in [(far_src, "Far1"), (far_tgt, "Far2")] {
        graph
            .add_node_raw(json!({ "id": id.to_string(), "name": name }))
            .await
            .unwrap();
    }
    graph
        .add_edge(&far_src.to_string(), &far_tgt.to_string(), "far", None)
        .await
        .unwrap();

    let ranked = brute_force_triplet_search(
        "any query",
        &vector_db,
        &AlignedEmbedding,
        &graph,
        &config(),
    )
    .await
    .unwrap();

    assert!(
        ranked.is_empty(),
        "no edge touches the seed set, so nothing should rank; got {ranked:?}"
    );
    assert!(
        !graph.get_call_log().contains(&"get_graph_data".to_string()),
        "an empty result must not come from a full graph scan"
    );
}

/// The nodeset-filtered branch is untouched: when `node_type` and `node_name`
/// are both set, the subgraph query is still the one used.
#[tokio::test]
async fn node_filter_path_still_uses_nodeset_subgraph() {
    let (vector_db, graph, _fx) = seed().await;

    let cfg = GraphRetrievalConfig {
        top_k: 100,
        node_type: Some("Entity".to_string()),
        node_name: Some(vec!["A".to_string()]),
        ..Default::default()
    };

    brute_force_triplet_search(
        "any query",
        vector_db.as_ref(),
        &AlignedEmbedding,
        graph.as_ref(),
        &cfg,
    )
    .await
    .unwrap();

    let calls = graph.get_call_log();
    assert!(
        calls.contains(&"get_nodeset_subgraph".to_string()),
        "the node-filtered path must keep using get_nodeset_subgraph; call log \
         was {calls:?}"
    );
    assert!(
        !calls.contains(&"get_graph_data".to_string()),
        "call log was {calls:?}"
    );
}
