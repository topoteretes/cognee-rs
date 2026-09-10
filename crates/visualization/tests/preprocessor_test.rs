#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for the visualization preprocessor core.
//!
//! Mirrors `cognee/tests/unit/modules/visualization/test_preprocessor.py` for
//! the node/link/color/bundle parts of the port (the schema-graph and
//! operation-layer assertions live with those modules).
//!
//! These tests pin the contract:
//!   - every known node type maps to a non-default stage;
//!   - `visual_rank` prefers the stamped `topological_rank` and falls back to a
//!     fixed stage order when unset;
//!   - `contains` / `is_a` / `made_from` edges are `structural`;
//!   - edges between the same stage pair sharing a relation collapse into one
//!     `bundle_key`;
//!   - provenance is exposed only when at least one provenance field is set.

use std::borrow::Cow;
use std::collections::HashMap;

use cognee_graph::{EdgeData, GraphNode};
use cognee_visualization::{PreprocessedGraph, preprocess};
use serde_json::{Value, json};

/// Build a `(node_id, properties)` tuple from a JSON object literal.
fn node(id: &str, props: Value) -> GraphNode {
    let map: HashMap<Cow<'static, str>, Value> = props
        .as_object()
        .expect("node fixture is a JSON object")
        .iter()
        .map(|(key, value)| (Cow::Owned(key.clone()), value.clone()))
        .collect();
    (id.to_string(), map)
}

/// Build a `(source, target, relation, properties)` tuple.
fn edge(source: &str, target: &str, relation: &str, props: Value) -> EdgeData {
    let map: HashMap<Cow<'static, str>, Value> = props
        .as_object()
        .expect("edge fixture is a JSON object")
        .iter()
        .map(|(key, value)| (Cow::Owned(key.clone()), value.clone()))
        .collect();
    (
        source.to_string(),
        target.to_string(),
        relation.to_string(),
        map,
    )
}

/// A small graph that mirrors the shape of the canonical Alice example: one
/// document, two chunks, three entities of two types, and one summary.
/// Port of `_alice_like_graph()` (`test_preprocessor.py:28-106`).
fn alice_like_graph() -> (Vec<GraphNode>, Vec<EdgeData>) {
    let nodes = vec![
        node(
            "doc1",
            json!({"type": "TextDocument", "name": "alice.md", "topological_rank": 1}),
        ),
        node(
            "c1",
            json!({
                "type": "DocumentChunk",
                "text": "Alice knows Bob.",
                "source_pipeline": "cognify_pipeline",
                "source_task": "extract_chunks_from_documents",
                "topological_rank": 2,
            }),
        ),
        node(
            "c2",
            json!({
                "type": "DocumentChunk",
                "text": "NLP is a subfield of CS.",
                "source_pipeline": "cognify_pipeline",
                "source_task": "extract_chunks_from_documents",
                "topological_rank": 2,
            }),
        ),
        node(
            "alice",
            json!({
                "type": "Entity",
                "name": "Alice",
                "source_pipeline": "cognify_pipeline",
                "source_task": "extract_graph_from_data",
                "topological_rank": 3,
            }),
        ),
        node(
            "bob",
            json!({
                "type": "Entity",
                "name": "Bob",
                "source_pipeline": "cognify_pipeline",
                "source_task": "extract_graph_from_data",
                "topological_rank": 3,
            }),
        ),
        node(
            "nlp",
            json!({
                "type": "Entity",
                "name": "NLP",
                "source_pipeline": "cognify_pipeline",
                "source_task": "extract_graph_from_data",
                "topological_rank": 3,
            }),
        ),
        node(
            "person",
            json!({"type": "EntityType", "name": "Person", "topological_rank": 4}),
        ),
        node(
            "field",
            json!({"type": "EntityType", "name": "Field", "topological_rank": 4}),
        ),
        node(
            "sum1",
            json!({"type": "TextSummary", "text": "Alice and Bob in NLP.", "topological_rank": 5}),
        ),
    ];
    let edges = vec![
        edge("doc1", "c1", "contains", json!({})),
        edge("doc1", "c2", "contains", json!({})),
        edge("c1", "alice", "contains", json!({})),
        edge("c1", "bob", "contains", json!({})),
        edge("c2", "nlp", "contains", json!({})),
        edge("alice", "person", "is_a", json!({})),
        edge("bob", "person", "is_a", json!({})),
        edge("nlp", "field", "is_a", json!({})),
        edge(
            "alice",
            "bob",
            "knows",
            json!({"relationship_name": "knows"}),
        ),
        edge("c1", "sum1", "made_from", json!({})),
    ];
    (nodes, edges)
}

fn alice_like() -> PreprocessedGraph {
    let (nodes, edges) = alice_like_graph();
    preprocess(nodes, edges, None)
}

/// Index the emitted nodes by their `id`.
fn by_id(result: &PreprocessedGraph) -> HashMap<String, Value> {
    result
        .nodes
        .iter()
        .map(|node| {
            (
                node.get("id")
                    .and_then(Value::as_str)
                    .expect("every emitted node has a string id")
                    .to_string(),
                node.clone(),
            )
        })
        .collect()
}

fn field<'a>(node: &'a Value, key: &str) -> &'a Value {
    node.get(key).unwrap_or(&Value::Null)
}

#[test]
fn preprocess_returns_all_nodes_and_links() {
    let result = alice_like();
    assert_eq!(result.nodes.len(), 9);
    assert_eq!(result.links.len(), 10);
}

#[test]
fn stage_assignment_for_known_types() {
    let result = alice_like();
    let nodes = by_id(&result);
    for (id, stage) in [
        ("doc1", "document"),
        ("c1", "chunk"),
        ("c2", "chunk"),
        ("alice", "entity"),
        ("bob", "entity"),
        ("nlp", "entity"),
        ("person", "type"),
        ("field", "type"),
        ("sum1", "summary"),
    ] {
        assert_eq!(field(&nodes[id], "stage"), &json!(stage), "node {id}");
    }
}

#[test]
fn stage_assignment_covers_every_known_type() {
    // The full `_STAGE_BY_TYPE` table (preprocessor.py:55-68) plus the
    // unknown-type fallback.
    let cases = [
        ("TextDocument", "document"),
        ("DocumentChunk", "chunk"),
        ("TextSummary", "summary"),
        ("GlobalContextSummary", "context"),
        ("Entity", "entity"),
        ("EntityType", "type"),
        ("DatabaseSchema", "schema"),
        ("SchemaTable", "schema"),
        ("SchemaRelationship", "schema"),
        ("TableType", "schema"),
        ("TableRow", "schema"),
        ("ColumnValue", "schema"),
        ("MysteryType", "other"),
    ];
    let nodes: Vec<GraphNode> = cases
        .iter()
        .enumerate()
        .map(|(index, (node_type, _))| node(&format!("n{index}"), json!({"type": node_type})))
        .collect();
    let result = preprocess(nodes, vec![], None);
    for (index, (node_type, stage)) in cases.iter().enumerate() {
        assert_eq!(
            field(&result.nodes[index], "stage"),
            &json!(stage),
            "type {node_type}"
        );
    }
    // A node with no `type` at all also lands in "other" (never "default").
    let untyped = preprocess(vec![node("x", json!({}))], vec![], None);
    assert_eq!(field(&untyped.nodes[0], "stage"), &json!("other"));
}

#[test]
fn visual_rank_uses_stamped_topological_rank() {
    let result = alice_like();
    let nodes = by_id(&result);
    for (id, rank) in [
        ("doc1", 1),
        ("c1", 2),
        ("alice", 3),
        ("person", 4),
        ("sum1", 5),
    ] {
        assert_eq!(field(&nodes[id], "visual_rank"), &json!(rank), "node {id}");
    }
}

#[test]
fn visual_rank_truncates_float_ranks() {
    let result = preprocess(
        vec![node(
            "d",
            json!({"type": "TextDocument", "topological_rank": 3.9}),
        )],
        vec![],
        None,
    );
    assert_eq!(field(&result.nodes[0], "visual_rank"), &json!(3));

    // A positive fractional rank still takes the float branch, so `int(0.5)`
    // truncates to 0 rather than falling back to the stage order — surprising,
    // but exactly what Python does (`preprocessor.py:865-866`).
    let fractional = preprocess(
        vec![node(
            "d",
            json!({"type": "TextDocument", "topological_rank": 0.5}),
        )],
        vec![],
        None,
    );
    assert_eq!(field(&fractional.nodes[0], "visual_rank"), &json!(0));
}

#[test]
fn visual_rank_falls_back_when_topological_rank_zero_or_none() {
    let nodes = vec![
        node("d", json!({"type": "TextDocument", "topological_rank": 0})),
        node("c", json!({"type": "DocumentChunk"})), // no rank at all
        node("e", json!({"type": "Entity", "topological_rank": null})),
        node("o", json!({"type": "MysteryType"})),
    ];
    let edges = vec![
        edge("d", "c", "contains", json!({})),
        edge("c", "e", "contains", json!({})),
    ];
    let result = preprocess(nodes, edges, None);
    let nodes = by_id(&result);
    // 1-based STAGE_ORDER position: document=1, chunk=2, entity=3, other=8.
    assert_eq!(field(&nodes["d"], "visual_rank"), &json!(1));
    assert_eq!(field(&nodes["c"], "visual_rank"), &json!(2));
    assert_eq!(field(&nodes["e"], "visual_rank"), &json!(3));
    assert_eq!(field(&nodes["o"], "visual_rank"), &json!(8));
}

#[test]
fn has_meaningful_topological_rank_flag() {
    assert!(alice_like().has_meaningful_topological_rank);

    for rank in [json!(0), json!(null)] {
        let legacy = preprocess(
            vec![node(
                "d",
                json!({"type": "TextDocument", "topological_rank": rank.clone()}),
            )],
            vec![],
            None,
        );
        assert!(
            !legacy.has_meaningful_topological_rank,
            "rank {rank} must not count as meaningful"
        );
    }
    for rank in [json!(1), json!(-2), json!(2.5)] {
        let stamped = preprocess(
            vec![node(
                "d",
                json!({"type": "TextDocument", "topological_rank": rank.clone()}),
            )],
            vec![],
            None,
        );
        assert!(
            stamped.has_meaningful_topological_rank,
            "rank {rank} must count as meaningful"
        );
    }
}

#[test]
fn topological_rank_survives_into_the_emitted_node() {
    // The vendored JS re-derives its own copy of the flag from the raw field
    // (views/story_view.js:106-109), so it must not be dropped.
    let result = alice_like();
    let nodes = by_id(&result);
    assert_eq!(field(&nodes["doc1"], "topological_rank"), &json!(1));
}

#[test]
fn structural_edges_classified_correctly() {
    let result = alice_like();
    let classes: HashMap<(String, String, String), String> = result
        .links
        .iter()
        .map(|link| {
            (
                (
                    field(link, "source").as_str().unwrap_or("").to_string(),
                    field(link, "target").as_str().unwrap_or("").to_string(),
                    field(link, "relation").as_str().unwrap_or("").to_string(),
                ),
                field(link, "edge_class").as_str().unwrap_or("").to_string(),
            )
        })
        .collect();

    for key in [
        ("doc1", "c1", "contains"),
        ("doc1", "c2", "contains"),
        ("c1", "alice", "contains"),
        ("alice", "person", "is_a"),
        ("c1", "sum1", "made_from"),
    ] {
        let lookup = (key.0.to_string(), key.1.to_string(), key.2.to_string());
        assert_eq!(
            classes[&lookup], "structural",
            "{key:?} should be structural"
        );
    }
    let knows = ("alice".to_string(), "bob".to_string(), "knows".to_string());
    assert_eq!(classes[&knows], "semantic");
}

#[test]
fn edge_class_counts_summed() {
    let result = alice_like();
    // 5 contains + 3 is_a + 1 made_from = 9 structural, 1 knows = 1 semantic.
    assert_eq!(result.edge_classes.get("structural"), Some(&9));
    assert_eq!(result.edge_classes.get("semantic"), Some(&1));
}

#[test]
fn bundle_key_collapses_structural_edges_into_groups() {
    let result = alice_like();
    // The 5 `contains` edges fall into two bundles: doc->chunk (2 edges) and
    // chunk->entity (3 edges) — 5 lines become 2 ribbons.
    let mut counts: Vec<usize> = result
        .bundles
        .iter()
        .filter(|(key, _)| key.contains("|contains"))
        .map(|(_, count)| *count)
        .collect();
    counts.sort_unstable();
    assert_eq!(counts, vec![2, 3]);
    assert_eq!(
        result.bundles.get("document|chunk|structural|contains"),
        Some(&2)
    );
    assert_eq!(
        result.bundles.get("chunk|entity|structural|contains"),
        Some(&3)
    );
}

#[test]
fn links_carry_endpoint_stages_and_default_to_other() {
    let nodes = vec![node("a", json!({"type": "Entity"}))];
    // "ghost" is not in the node list at all.
    let edges = vec![edge("a", "ghost", "knows", json!({}))];
    let result = preprocess(nodes, edges, None);
    let link = &result.links[0];
    assert_eq!(field(link, "source_stage"), &json!("entity"));
    assert_eq!(field(link, "target_stage"), &json!("other"));
    assert_eq!(
        field(link, "bundle_key"),
        &json!("entity|other|semantic|knows")
    );
}

#[test]
fn empty_edge_properties_are_tolerated() {
    // Rust edges are always 4-tuples, so Python's 3-tuple tolerance maps onto
    // "the property map is empty" — and `if edge_info:` is false for {}.
    let nodes = vec![
        node("a", json!({"type": "Entity"})),
        node("b", json!({"type": "Entity"})),
    ];
    let result = preprocess(nodes, vec![edge("a", "b", "knows", json!({}))], None);
    assert_eq!(result.links.len(), 1);
    let link = &result.links[0];
    assert_eq!(field(link, "edge_class"), &json!("semantic"));
    assert_eq!(field(link, "weight"), &Value::Null);
    assert_eq!(field(link, "all_weights"), &json!({}));
    assert_eq!(field(link, "relationship_type"), &Value::Null);
    assert_eq!(field(link, "edge_info"), &json!({}));
    // Exactly the 11 keys Python emits — nothing more.
    let keys: Vec<&String> = link
        .as_object()
        .expect("link is an object")
        .keys()
        .collect();
    assert_eq!(keys.len(), 11, "unexpected link keys: {keys:?}");
    for key in [
        "source",
        "target",
        "relation",
        "weight",
        "all_weights",
        "relationship_type",
        "edge_info",
        "edge_class",
        "bundle_key",
        "source_stage",
        "target_stage",
    ] {
        assert!(link.get(key).is_some(), "missing link key {key}");
    }
}

#[test]
fn link_weights_are_flattened() {
    let result = preprocess(
        vec![],
        vec![edge(
            "a",
            "b",
            "knows",
            json!({
                "weight": 0.5,
                "weights": {"semantic": 0.8, "lexical": 0.3},
                "weight_trust": 0.9,
                "relationship_type": "KNOWS",
            }),
        )],
        None,
    );
    let link = &result.links[0];
    assert_eq!(field(link, "weight"), &json!(0.5));
    assert_eq!(
        field(link, "all_weights"),
        &json!({"default": 0.5, "semantic": 0.8, "lexical": 0.3, "trust": 0.9})
    );
    assert_eq!(field(link, "relationship_type"), &json!("KNOWS"));
}

#[test]
fn provenance_present_only_when_fields_set() {
    let result = alice_like();
    let nodes = by_id(&result);
    // doc1 has no provenance fields in the fixture — the section stays hidden.
    assert!(nodes["doc1"].get("provenance").is_none());
    assert_eq!(
        field(&nodes["c1"], "provenance"),
        &json!({
            "source_task": "extract_chunks_from_documents",
            "source_pipeline": "cognify_pipeline",
        })
    );
}

#[test]
fn provenance_index_indexes_only_nodes_with_provenance() {
    let result = alice_like();
    assert!(result.provenance_index.contains_key("c1"));
    assert!(!result.provenance_index.contains_key("doc1"));
}

#[test]
fn color_maps_have_expected_keys() {
    let result = alice_like();
    assert!(result.color_maps.pipeline.contains_key("cognify_pipeline"));
    assert!(
        result
            .color_maps
            .task
            .contains_key("extract_chunks_from_documents")
    );
    assert!(
        result
            .color_maps
            .task
            .contains_key("extract_graph_from_data")
    );
    assert!(result.color_maps.node_set.is_empty());
    assert!(result.color_maps.user.is_empty());
}

#[test]
fn memory_node_set_colors_are_pinned() {
    let nodes = vec![
        node(
            "l",
            json!({"type": "Entity", "name": "L", "source_node_set": "session_learnings"}),
        ),
        node(
            "o",
            json!({"type": "Entity", "name": "O", "source_node_set": "other_set"}),
        ),
    ];
    let result = preprocess(nodes, vec![], None);
    assert_eq!(
        result.color_maps.node_set.get("session_learnings"),
        Some(&"#FFC53D".to_string())
    );
    // Sets that are not pinned keep the deterministic hue rotation.
    assert_ne!(
        result.color_maps.node_set.get("other_set"),
        Some(&"#FFC53D".to_string())
    );
    let nodes = by_id(&result);
    assert_eq!(field(&nodes["l"], "is_memory_learning"), &json!(true));
    assert_eq!(field(&nodes["o"], "is_memory_learning"), &json!(false));
}

#[test]
fn pipeline_stages_in_canonical_order() {
    let result = alice_like();
    assert_eq!(
        result.pipeline_stages,
        vec!["document", "chunk", "entity", "type", "summary"]
    );
}

#[test]
fn degree_count_matches_edge_count() {
    let result = alice_like();
    let nodes = by_id(&result);
    // doc1 -> c1, doc1 -> c2 => degree 2
    assert_eq!(field(&nodes["doc1"], "degree"), &json!(2));
    // c1: doc1->c1, c1->alice, c1->bob, c1->sum1 => degree 4
    assert_eq!(field(&nodes["c1"], "degree"), &json!(4));
}

#[test]
fn degree_counts_self_loops_twice() {
    let nodes = vec![node("a", json!({"type": "Entity", "name": "A"}))];
    let result = preprocess(nodes, vec![edge("a", "a", "knows", json!({}))], None);
    assert_eq!(field(&result.nodes[0], "degree"), &json!(2));
    // Degree 2 is also the max, so importance normalizes to 1.0.
    assert_eq!(field(&result.nodes[0], "importance"), &json!(1.0));
}

#[test]
fn importance_is_zero_on_an_edgeless_graph() {
    let result = preprocess(vec![node("a", json!({"type": "Entity"}))], vec![], None);
    assert_eq!(field(&result.nodes[0], "importance"), &json!(0.0));
}

#[test]
fn label_priority_marks_documents_and_types_always() {
    let result = alice_like();
    let nodes = by_id(&result);
    for id in ["doc1", "person", "field"] {
        assert_eq!(field(&nodes[id], "label_priority"), &json!(true), "{id}");
    }
}

#[test]
fn unnamed_nodes_never_get_label_priority() {
    const UUID_NAME: &str = "13e52fce-2d52-4a8b-9f01-aabbccddeeff";
    let nodes = vec![
        node("d1", json!({"type": "TextDocument", "name": UUID_NAME})),
        node("d2", json!({"type": "TextDocument", "name": "alice.md"})),
    ];
    let result = preprocess(nodes, vec![], None);
    let nodes = by_id(&result);
    assert_eq!(field(&nodes["d1"], "is_unnamed"), &json!(true));
    assert_eq!(field(&nodes["d1"], "label_priority"), &json!(false));
    assert_eq!(field(&nodes["d2"], "is_unnamed"), &json!(false));
    assert_eq!(field(&nodes["d2"], "label_priority"), &json!(true));
}

#[test]
fn uuid_and_hash_names_get_readable_placeholders() {
    const UUID_NAME: &str = "13e52fce-2d52-4a8b-9f01-aabbccddeeff";
    let hash_name = "a".repeat(64);

    let result = preprocess(
        vec![node("n1", json!({"type": "Entity", "name": UUID_NAME}))],
        vec![],
        None,
    );
    let name = field(&result.nodes[0], "name")
        .as_str()
        .expect("name is a string")
        .to_string();
    assert_eq!(name, "Unnamed Entity (n1)");
    assert!(!name.contains(UUID_NAME));
    assert_eq!(field(&result.nodes[0], "is_unnamed"), &json!(true));

    // Identifier-shaped fallback fields are skipped too.
    let result = preprocess(
        vec![node(
            "n1",
            json!({"type": "DocumentChunk", "text": hash_name, "description": "real text"}),
        )],
        vec![],
        None,
    );
    assert_eq!(field(&result.nodes[0], "name"), &json!("real text"));

    // The placeholder id prefix is the first 8 characters of the node id.
    let result = preprocess(
        vec![node(UUID_NAME, json!({"type": "Entity"}))],
        vec![],
        None,
    );
    assert_eq!(
        field(&result.nodes[0], "name"),
        &json!("Unnamed Entity (13e52fce)")
    );
}

#[test]
fn regular_names_are_untouched() {
    let result = alice_like();
    let names: Vec<&str> = result
        .nodes
        .iter()
        .filter_map(|node| node.get("name").and_then(Value::as_str))
        .collect();
    assert!(names.contains(&"Alice"));
    assert!(!names.iter().any(|name| name.starts_with("Unnamed ")));
}

#[test]
fn node_color_preserved_from_type_map() {
    let result = alice_like();
    let nodes = by_id(&result);
    assert_eq!(field(&nodes["alice"], "color"), &json!("#6510F4")); // Entity
    assert_eq!(field(&nodes["person"], "color"), &json!("#D5C2FF")); // EntityType
    assert_eq!(field(&nodes["c1"], "color"), &json!("#0DFF00")); // DocumentChunk
    assert_eq!(field(&nodes["doc1"], "color"), &json!("#A550FF")); // TextDocument
}

#[test]
fn ontology_valid_overrides_color() {
    // Ontology-grounded nodes get a distinct fill — it must differ from the
    // unknown-type fallback gray so ontology matches stand apart visually.
    let nodes = vec![
        node(
            "e",
            json!({"type": "Entity", "name": "X", "ontology_valid": true}),
        ),
        node("u", json!({"type": "MysteryType", "name": "Y"})),
    ];
    let result = preprocess(nodes, vec![], None);
    assert_eq!(field(&result.nodes[0], "color"), &json!("#FF5CA8"));
    assert_ne!(
        field(&result.nodes[0], "color"),
        field(&result.nodes[1], "color")
    );
    assert_eq!(field(&result.nodes[1], "color"), &json!("#DBD8D8"));
}

#[test]
fn node_set_type_has_its_own_color() {
    let result = preprocess(
        vec![node("s", json!({"type": "NodeSet", "name": "learnings"}))],
        vec![],
        None,
    );
    assert_eq!(field(&result.nodes[0], "color"), &json!("#94A3B8"));
}

#[test]
fn absent_type_uses_the_default_color_but_null_type_does_not() {
    // Python: `_TYPE_COLOR_MAP.get(node_info.get("type", "default"), UNKNOWN)`
    // — the "default" entry is only reachable when the key is missing.
    let result = preprocess(
        vec![
            node("absent", json!({"name": "A"})),
            node("null", json!({"type": null, "name": "B"})),
            node("numeric", json!({"type": 7, "name": "C"})),
        ],
        vec![],
        None,
    );
    let nodes = by_id(&result);
    assert_eq!(field(&nodes["absent"], "color"), &json!("#7c3aed"));
    assert_eq!(field(&nodes["null"], "color"), &json!("#DBD8D8"));
    assert_eq!(field(&nodes["numeric"], "color"), &json!("#DBD8D8"));
}

#[test]
fn created_at_is_moved_to_t_created_and_audit_columns_dropped() {
    let result = preprocess(
        vec![
            node(
                "a",
                json!({"type": "Entity", "created_at": 1_768_164_683_000_i64, "updated_at": 5}),
            ),
            node("b", json!({"type": "Entity", "created_at": "2026-06-10"})),
            node("c", json!({"type": "Entity"})),
            // Python explicitly excludes `bool` (`isinstance(True, int)` is
            // True!) and plain floats from the integer check.
            node("d", json!({"type": "Entity", "created_at": true})),
            node("e", json!({"type": "Entity", "created_at": 1.5})),
        ],
        vec![],
        None,
    );
    let nodes = by_id(&result);
    assert_eq!(
        field(&nodes["a"], "t_created"),
        &json!(1_768_164_683_000_i64)
    );
    for id in ["a", "b", "c", "d", "e"] {
        assert!(nodes[id].get("created_at").is_none(), "{id}");
        assert!(nodes[id].get("updated_at").is_none(), "{id}");
    }
    // Non-integer / missing timestamps must never be fabricated.
    for id in ["b", "c", "d", "e"] {
        assert!(nodes[id].get("t_created").is_none(), "{id}");
    }
}

#[test]
fn empty_graph_does_not_crash() {
    let result = preprocess(vec![], vec![], None);
    assert!(result.nodes.is_empty());
    assert!(result.links.is_empty());
    assert!(!result.has_meaningful_topological_rank);
    assert!(result.pipeline_stages.is_empty());
    assert!(result.edge_classes.is_empty());
    assert!(result.bundles.is_empty());
    assert!(result.provenance_index.is_empty());
    assert!(result.schema_data.is_none());
}

#[test]
fn schema_data_is_passed_through() {
    let schema = json!({"tables": ["users"]});
    let result = preprocess(vec![], vec![], Some(schema.clone()));
    assert_eq!(result.schema_data, Some(schema));
}

/// The Schema tab's field selection must surface *database* properties, not the
/// renderer's own derived keys.
///
/// Python's `extract_type_schema_fields` orders equally-prevalent non-preferred
/// fields by `Counter.most_common()`, whose tie-break is first-seen key order —
/// i.e. the order the keys appear while iterating each node dict. In Python that
/// order is "database properties (JSON-blob order), then the keys `preprocess()`
/// appends", so a `document_id` at 100% coverage always beats the `degree`
/// stamped afterwards.
///
/// Rust reproduces the *grouping* by inserting the adapter's properties before
/// any derived key, which only holds when `serde_json::Map` is insertion-ordered.
/// Without the workspace's `serde_json/preserve_order` feature, `Map` is a
/// `BTreeMap`, every key is globally alphabetical, and `degree` / `importance`
/// displace `document_id` / `version` on the type card. The two expected lists
/// below are the Python-observed ones.
#[test]
fn type_card_fields_prefer_database_properties_over_derived_keys() {
    // Only `chunk_index` / `document_id` (chunk) and `version` (document) are
    // non-preferred, non-excluded database properties, so the remaining slots
    // are exactly where a derived key could wrongly win.
    let result = preprocess(
        vec![
            node(
                "chunk-1",
                json!({
                    "type": "DocumentChunk",
                    "name": "alice.md#0",
                    "text": "Alice knows Bob.",
                    "chunk_index": 0,
                    "document_id": "doc-1",
                    "source_task": "extract_chunks_from_documents",
                    "source_pipeline": "cognify_pipeline",
                    "topological_rank": 2,
                }),
            ),
            node(
                "doc-1",
                json!({
                    "type": "TextDocument",
                    "name": "alice.md",
                    "version": 1,
                    "source_task": "resolve_data_directories",
                    "source_pipeline": "cognify_pipeline",
                    "topological_rank": 1,
                }),
            ),
        ],
        vec![edge("doc-1", "chunk-1", "contains", json!({}))],
        None,
    );

    let card_fields = |type_name: &str| -> Vec<String> {
        result.schema_graph["nodes"]
            .as_array()
            .expect("schema graph nodes array")
            .iter()
            .find(|node| node["id"] == json!(format!("type:{type_name}")))
            .unwrap_or_else(|| panic!("no type card for {type_name}"))["fields"]
            .as_array()
            .expect("fields array")
            .iter()
            .map(|field| {
                field["name"]
                    .as_str()
                    .expect("field name is a string")
                    .to_string()
            })
            .collect()
    };

    assert_eq!(
        card_fields("DocumentChunk"),
        [
            "count",
            // `preferred_fields`, in the whitelist's own order.
            "source_task",
            "source_pipeline",
            "topological_rank",
            // …then the database properties. `degree` sorts between these two
            // alphabetically, which is exactly what must not happen.
            "chunk_index",
            "document_id",
        ],
        "DocumentChunk type card must read like Python's"
    );
    assert_eq!(
        card_fields("TextDocument"),
        [
            "count",
            "source_task",
            "source_pipeline",
            "topological_rank",
            // The document's single remaining database property…
            "version",
            // …and only then the first derived key, as Python has it.
            "is_memory_learning",
        ],
        "TextDocument type card must read like Python's"
    );
}
