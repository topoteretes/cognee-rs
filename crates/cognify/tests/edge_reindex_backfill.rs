//! The whole-graph `EdgeType` vector backfill: what it counts, what it writes,
//! and what it must not do.
//!
//! The orphan these tests describe is what a SIGKILL between cognify's graph
//! write and its vector write leaves behind — a graph edge whose
//! `EdgeType_relationship_name` point was never written, which no retry
//! re-emits because `retrieve_existing_edges` routes the edge into the
//! claim-only bucket. See `cognee_cognify::edge_reindex` for the full chain.
//!
//! Four properties are load-bearing enough to pin by exact count and exact id:
//!
//! 1. A run writes one point per **distinct retrieval text**, not per edge.
//! 2. Those points carry the ids `EdgeType::deterministic_id` produces, or the
//!    readers that recompute the id at query time never find them.
//! 3. A second run is a no-op — nothing re-embedded, nothing rewritten.
//! 4. The default (report-only) run writes nothing *and embeds nothing*, so it
//!    is free to run against production.

#![cfg(feature = "testing")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]

use std::borrow::Cow;
use std::collections::HashMap;

use cognee_cognify::{EdgeReindexOptions, reindex_edge_types};
use cognee_embedding::MockEmbeddingEngine;
use cognee_graph::{EdgeData, GraphDBTrait, MockGraphDB};
use cognee_models::EdgeType;
use cognee_vector::{MockVectorDB, VectorDB};
use serde_json::json;
use uuid::Uuid;

const DIM: usize = 8;
const DATA_TYPE: &str = "EdgeType";
const FIELD: &str = "relationship_name";

/// Build an edge with an optional `edge_text` property, the shape cognify's
/// `build_edge_props` writes.
fn edge(source: &str, target: &str, relationship: &str, edge_text: Option<&str>) -> EdgeData {
    let mut properties: HashMap<Cow<'static, str>, serde_json::Value> = HashMap::new();
    properties.insert(
        Cow::Borrowed("relationship_name"),
        json!(relationship.to_string()),
    );
    if let Some(text) = edge_text {
        properties.insert(Cow::Borrowed("edge_text"), json!(text.to_string()));
    }
    (
        source.to_string(),
        target.to_string(),
        relationship.to_string(),
        properties,
    )
}

/// A graph whose six edges collapse to exactly **three** distinct retrieval
/// texts — the fan-out that makes per-edge counting visibly wrong.
///
/// - "Alice works at Acme" x3 (three edges, one text)
/// - "Bob knows Carol"      x2 (two edges, one text)
/// - "located_in"           x1 (no edge_text, falls back to the relation name)
async fn seeded_graph() -> MockGraphDB {
    let graph = MockGraphDB::new();
    let edges = vec![
        edge("a", "b", "works_at", Some("Alice works at Acme")),
        edge("a", "c", "works_at", Some("Alice works at Acme")),
        edge("a", "d", "works_at", Some("Alice works at Acme")),
        edge("b", "c", "knows", Some("Bob knows Carol")),
        edge("b", "d", "knows", Some("Bob knows Carol")),
        edge("e", "f", "located_in", None),
    ];
    graph.add_edges(&edges).await.expect("seed edges");
    graph
}

fn expected_ids() -> Vec<(&'static str, Uuid)> {
    vec![
        (
            "Alice works at Acme",
            EdgeType::deterministic_id("Alice works at Acme"),
        ),
        (
            "Bob knows Carol",
            EdgeType::deterministic_id("Bob knows Carol"),
        ),
        ("located_in", EdgeType::deterministic_id("located_in")),
    ]
}

/// The core contract: six edges, three distinct texts, three points written —
/// with exactly the ids the writer/reader derivation produces.
#[tokio::test]
async fn apply_writes_one_point_per_distinct_retrieval_text() {
    let graph = seeded_graph().await;
    let vector = MockVectorDB::new();
    let embed = MockEmbeddingEngine::deterministic(DIM);

    let report = reindex_edge_types(
        &graph,
        &vector,
        &embed,
        &EdgeReindexOptions {
            apply: true,
            ..Default::default()
        },
    )
    .await
    .expect("reindex");

    assert_eq!(report.edges_scanned, 6, "all six edges must be scanned");
    assert_eq!(
        report.distinct_texts, 3,
        "six edges collapse to three distinct retrieval texts"
    );
    assert_eq!(
        report.orphaned_texts, 3,
        "the collection is empty, so all three texts are orphaned"
    );
    assert_eq!(
        report.points_written, 3,
        "one point per distinct text — NOT one per edge (which would be 6)"
    );
    assert!(report.applied);
    assert_eq!(report.resume_cursor, None, "the pass covered everything");

    assert_eq!(
        vector.collection_size(DATA_TYPE, FIELD).await.unwrap(),
        3,
        "exactly three rows land in the collection"
    );

    // Every expected id must be present, by exact id.
    let ids: Vec<Uuid> = expected_ids().into_iter().map(|(_, id)| id).collect();
    let found = vector.retrieve(DATA_TYPE, FIELD, &ids).await.unwrap();
    assert_eq!(
        found.len(),
        3,
        "all three deterministic ids must be retrievable"
    );

    // And each carries the right relationship_name and per-text edge count.
    for (text, id) in expected_ids() {
        let hit = found
            .iter()
            .find(|h| h.id == id)
            .unwrap_or_else(|| panic!("no point for {text:?} at id {id}"));
        assert_eq!(
            hit.metadata.get("relationship_name").unwrap(),
            &json!(text),
            "relationship_name payload for {text:?}"
        );
    }

    // number_of_edges is the per-text fan-out, not 1 and not the edge total.
    let counts: HashMap<String, i64> = found
        .iter()
        .map(|h| {
            (
                h.metadata
                    .get("relationship_name")
                    .and_then(|v| v.as_str())
                    .unwrap()
                    .to_string(),
                h.metadata
                    .get("number_of_edges")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap(),
            )
        })
        .collect();
    assert_eq!(counts.get("Alice works at Acme"), Some(&3));
    assert_eq!(counts.get("Bob knows Carol"), Some(&2));
    assert_eq!(counts.get("located_in"), Some(&1));
}

/// Dry-run is the default, and it must be genuinely free: no writes, and — the
/// half that costs money — no embedding calls at all.
#[tokio::test]
async fn dry_run_writes_nothing_and_embeds_nothing() {
    let graph = seeded_graph().await;
    let vector = MockVectorDB::new();
    let embed = MockEmbeddingEngine::deterministic(DIM);

    // Default options: `apply` is false.
    let report = reindex_edge_types(&graph, &vector, &embed, &EdgeReindexOptions::default())
        .await
        .expect("reindex");

    assert_eq!(
        report.orphaned_texts, 3,
        "the count is still produced — that is the point of a dry run"
    );
    assert_eq!(report.points_written, 0, "a dry run must write no points");
    assert_eq!(report.batches, 0, "a dry run must issue no embedding batch");
    assert!(!report.applied);

    assert_eq!(
        embed.call_count(),
        0,
        "a dry run must not call the embedding engine even once"
    );
    assert_eq!(
        embed.embedded_text_count(),
        0,
        "a dry run must not embed a single text"
    );
    assert_eq!(
        vector.index_points_call_count(),
        0,
        "a dry run must not issue an index_points call"
    );
    assert_eq!(
        vector.create_collection_count(),
        0,
        "a dry run must not even create the collection"
    );
}

/// The orphan count is per distinct retrieval text. With six edges over three
/// texts, a per-edge count would report 6 — this pins 3.
#[tokio::test]
async fn orphan_count_is_per_distinct_text_not_per_edge() {
    let graph = seeded_graph().await;
    let vector = MockVectorDB::new();
    let embed = MockEmbeddingEngine::deterministic(DIM);

    let report = reindex_edge_types(&graph, &vector, &embed, &EdgeReindexOptions::default())
        .await
        .expect("reindex");

    assert_eq!(report.edges_scanned, 6);
    assert_eq!(
        report.orphaned_texts, 3,
        "three distinct texts are orphaned; counting per edge would say 6"
    );
    assert_ne!(
        report.orphaned_texts, report.edges_scanned,
        "the whole point: the orphan count must not equal the edge count"
    );
}

/// Once the backfill has run, running it again must cost nothing — the existence
/// probe finds every point and no text is re-embedded or rewritten.
#[tokio::test]
async fn second_run_is_a_no_op() {
    let graph = seeded_graph().await;
    let vector = MockVectorDB::new();
    let embed = MockEmbeddingEngine::deterministic(DIM);
    let options = EdgeReindexOptions {
        apply: true,
        ..Default::default()
    };

    let first = reindex_edge_types(&graph, &vector, &embed, &options)
        .await
        .expect("first reindex");
    assert_eq!(first.points_written, 3);

    let calls_after_first = embed.call_count();
    let texts_after_first = embed.embedded_text_count();
    let writes_after_first = vector.index_points_call_count();
    assert_eq!(texts_after_first, 3, "the first run embeds the three texts");

    let second = reindex_edge_types(&graph, &vector, &embed, &options)
        .await
        .expect("second reindex");

    assert_eq!(
        second.distinct_texts, 3,
        "the graph is unchanged, so the same three texts are considered"
    );
    assert_eq!(
        second.orphaned_texts, 0,
        "every point now exists, so nothing is orphaned"
    );
    assert_eq!(second.points_written, 0, "a second run writes nothing");
    assert_eq!(second.batches, 0, "a second run issues no embedding batch");

    assert_eq!(
        embed.call_count(),
        calls_after_first,
        "a second run must not call the embedding engine again"
    );
    assert_eq!(
        embed.embedded_text_count(),
        texts_after_first,
        "a second run must re-embed nothing"
    );
    assert_eq!(
        vector.index_points_call_count(),
        writes_after_first,
        "a second run must not write again"
    );
    assert_eq!(
        vector.collection_size(DATA_TYPE, FIELD).await.unwrap(),
        3,
        "still exactly three rows"
    );
}

/// Only the genuinely missing row is written when the collection is partially
/// populated — the case an interrupted backfill actually leaves behind.
#[tokio::test]
async fn only_the_missing_point_is_written() {
    let graph = seeded_graph().await;
    let vector = MockVectorDB::new();
    let embed = MockEmbeddingEngine::deterministic(DIM);

    // Pre-populate two of the three, as a partial earlier run would have.
    vector
        .create_collection(DATA_TYPE, FIELD, DIM)
        .await
        .unwrap();
    let preexisting: Vec<cognee_vector::VectorPoint> = ["Alice works at Acme", "Bob knows Carol"]
        .iter()
        .map(|text| {
            cognee_vector::VectorPoint::new(EdgeType::deterministic_id(text), vec![0.1; DIM])
                .with_metadata("relationship_name", json!(text))
        })
        .collect();
    vector
        .index_points(DATA_TYPE, FIELD, &preexisting)
        .await
        .unwrap();
    let writes_before = vector.index_points_call_count();

    let report = reindex_edge_types(
        &graph,
        &vector,
        &embed,
        &EdgeReindexOptions {
            apply: true,
            ..Default::default()
        },
    )
    .await
    .expect("reindex");

    assert_eq!(report.distinct_texts, 3);
    assert_eq!(report.orphaned_texts, 1, "only `located_in` is missing");
    assert_eq!(report.points_written, 1);
    assert_eq!(
        embed.embedded_text_count(),
        1,
        "only the missing text is embedded — the two present ones are not"
    );
    assert_eq!(vector.index_points_call_count(), writes_before + 1);

    // The written point is the one with `located_in`'s deterministic id.
    let located_in = EdgeType::deterministic_id("located_in");
    let found = vector
        .retrieve(DATA_TYPE, FIELD, &[located_in])
        .await
        .unwrap();
    assert_eq!(found.len(), 1, "the located_in point must now exist");
    assert_eq!(found[0].id, located_in);
}

/// An edge with neither `edge_text` nor a relationship name gets no `EdgeType`
/// row from cognify, so it must not be counted as an orphan here either.
#[tokio::test]
async fn textless_edges_are_excluded_from_the_orphan_count() {
    let graph = MockGraphDB::new();
    graph
        .add_edges(&[
            edge("a", "b", "works_at", Some("Alice works at Acme")),
            edge("x", "y", "   ", Some("   ")),
            edge("p", "q", "", None),
        ])
        .await
        .unwrap();
    let vector = MockVectorDB::new();
    let embed = MockEmbeddingEngine::deterministic(DIM);

    let report = reindex_edge_types(&graph, &vector, &embed, &EdgeReindexOptions::default())
        .await
        .expect("reindex");

    assert_eq!(report.edges_scanned, 3);
    assert_eq!(
        report.edges_without_text, 2,
        "two edges have no usable text"
    );
    assert_eq!(
        report.orphaned_texts, 1,
        "only the one edge with real text can be orphaned"
    );
}

/// The cursor makes an interrupted backfill resumable: a limited pass reports
/// where it stopped, and resuming from there completes the set without
/// re-embedding what the first pass already wrote.
#[tokio::test]
async fn a_limited_pass_resumes_from_its_cursor_without_redoing_work() {
    let graph = seeded_graph().await;
    let vector = MockVectorDB::new();
    let embed = MockEmbeddingEngine::deterministic(DIM);

    // Texts sort as: "Alice works at Acme" < "Bob knows Carol" < "located_in"
    // (uppercase sorts before lowercase), so a limit of 2 stops at "Bob…".
    let first = reindex_edge_types(
        &graph,
        &vector,
        &embed,
        &EdgeReindexOptions {
            apply: true,
            limit: Some(2),
            ..Default::default()
        },
    )
    .await
    .expect("first pass");

    assert_eq!(first.points_written, 2, "the limit is honoured exactly");
    assert_eq!(
        first.resume_cursor.as_deref(),
        Some("Bob knows Carol"),
        "the cursor is the last text actually written"
    );
    assert_eq!(embed.embedded_text_count(), 2);

    let second = reindex_edge_types(
        &graph,
        &vector,
        &embed,
        &EdgeReindexOptions {
            apply: true,
            resume_after: first.resume_cursor.clone(),
            ..Default::default()
        },
    )
    .await
    .expect("resumed pass");

    assert_eq!(
        second.distinct_texts, 1,
        "the cursor excludes the two already-handled texts from consideration"
    );
    assert_eq!(second.points_written, 1, "only the remainder is written");
    assert_eq!(second.resume_cursor, None, "the set is now complete");
    assert_eq!(
        embed.embedded_text_count(),
        3,
        "three texts embedded in total across both passes — nothing re-embedded"
    );

    assert_eq!(
        vector.collection_size(DATA_TYPE, FIELD).await.unwrap(),
        3,
        "both passes together produce the same three rows as one unlimited pass"
    );
}

/// The ids written must be the ones a reader recomputes at query time. This is
/// the contract SDK-699 collapsed into `EdgeType::point_id_for`; if the backfill
/// drifted from it, every retrieval lookup would silently miss.
#[tokio::test]
async fn written_ids_match_the_shared_derivation() {
    let graph = seeded_graph().await;
    let vector = MockVectorDB::new();
    let embed = MockEmbeddingEngine::deterministic(DIM);

    reindex_edge_types(
        &graph,
        &vector,
        &embed,
        &EdgeReindexOptions {
            apply: true,
            ..Default::default()
        },
    )
    .await
    .expect("reindex");

    // `point_id_for` is the reader's entry point; `deterministic_id` is the
    // writer's. Both must land on the row that is actually in the store.
    for (edge_text, relationship, expected_text) in [
        (
            Some("Alice works at Acme"),
            "works_at",
            "Alice works at Acme",
        ),
        (None, "located_in", "located_in"),
    ] {
        let id = EdgeType::point_id_for(edge_text, relationship)
            .expect("a nonblank edge must yield a point id");
        assert_eq!(
            id,
            EdgeType::deterministic_id(expected_text),
            "point_id_for must agree with deterministic_id for {expected_text:?}"
        );
        let found = vector.retrieve(DATA_TYPE, FIELD, &[id]).await.unwrap();
        assert_eq!(
            found.len(),
            1,
            "the backfilled row for {expected_text:?} must be found at the id a reader derives"
        );
        assert_eq!(found[0].id, id);
    }
}

/// The orphan set is every edge type the graph implies and the collection
/// lacks — which is wider than "what a killed run dropped".
///
/// Cognify builds `EdgeType` rows from `input.edges` alone, while
/// `get_graph_from_model`'s structural edges (`is_part_of`, `contains`,
/// `made_from`) and `extract_dlt_fk_edges`' foreign-key edges reach the graph by
/// separate `add_edges` calls that the edge-type counting never sees. So they
/// are reported — and with `--apply`, written — on a graph that never crashed.
///
/// Pinned because the *count* is what an operator acts on, and reading a
/// permanent structural floor as crash damage is the specific misread the
/// module docs and the CLI output now warn about. If this behaviour is ever
/// narrowed to crash orphans only, that warning becomes the wrong text and this
/// test is what says so.
#[tokio::test]
async fn structural_edge_kinds_are_reported_even_on_an_uncrashed_graph() {
    let graph = MockGraphDB::new();
    graph
        .add_edges(&[
            // What `get_graph_from_model` writes: no `edge_text`, so the
            // retrieval text is the bare relation name.
            edge("chunk", "doc", "is_part_of", None),
            edge("doc", "chunk", "contains", None),
            // What `extract_dlt_fk_edges` writes: an `edge_text` that is the
            // relation name with underscores spaced out.
            edge("row", "table", "is_row_of", Some("is row of")),
        ])
        .await
        .unwrap();
    let vector = MockVectorDB::new();
    let embed = MockEmbeddingEngine::deterministic(DIM);

    let report = reindex_edge_types(&graph, &vector, &embed, &EdgeReindexOptions::default())
        .await
        .expect("reindex");

    assert_eq!(
        report.orphaned_texts, 3,
        "all three structural/DLT edge kinds are reported, not filtered out"
    );
    assert_eq!(report.edges_without_text, 0);

    // And by exact id, so a future filter cannot pass by reporting three of
    // something else.
    let ids = [
        EdgeType::deterministic_id("is_part_of"),
        EdgeType::deterministic_id("contains"),
        EdgeType::deterministic_id("is row of"),
    ];
    let applied = reindex_edge_types(
        &graph,
        &vector,
        &embed,
        &EdgeReindexOptions {
            apply: true,
            ..Default::default()
        },
    )
    .await
    .expect("apply");
    assert_eq!(applied.points_written, 3);
    let found = vector.retrieve(DATA_TYPE, FIELD, &ids).await.unwrap();
    assert_eq!(
        found.len(),
        3,
        "each structural/DLT text lands at the id a reader derives"
    );
}

/// `dataset_id` is stamped as metadata but is deliberately NOT part of the id —
/// one row is shared by every dataset holding an edge with that text, matching
/// Python's `uuid5(NAMESPACE_OID, "EdgeType:" + normalized_text)`.
#[tokio::test]
async fn dataset_id_is_metadata_only_and_not_part_of_the_id() {
    let graph = seeded_graph().await;
    let vector = MockVectorDB::new();
    let embed = MockEmbeddingEngine::deterministic(DIM);
    let dataset_id = Uuid::new_v4();

    reindex_edge_types(
        &graph,
        &vector,
        &embed,
        &EdgeReindexOptions {
            apply: true,
            dataset_id: Some(dataset_id),
            ..Default::default()
        },
    )
    .await
    .expect("reindex");

    // Same id as the dataset-less derivation.
    let id = EdgeType::deterministic_id("located_in");
    let found = vector.retrieve(DATA_TYPE, FIELD, &[id]).await.unwrap();
    assert_eq!(
        found.len(),
        1,
        "the id must not change when a dataset is supplied"
    );
    assert_eq!(
        found[0].metadata.get("dataset_id"),
        Some(&json!(dataset_id.to_string())),
        "the dataset still reaches the payload as metadata"
    );
}
