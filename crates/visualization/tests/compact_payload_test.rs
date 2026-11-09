#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! The streamed graph must render the same as the JSON one — port of Python's
//! `cognee/tests/unit/modules/visualization/test_compact_payload.py`.
//!
//! Each chunk is computed from that chunk alone and the graph-wide fields from
//! a small accumulator. What this pins is that nothing drifts from
//! `preprocess()`: a chunk node has the name, stage and placeholder flag
//! `preprocess()` gives it, a link its edge class, and the summary equals what
//! `preprocess()` assigns the same graph in one piece.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};

use cognee_graph::{BoundedNeighborhood, BoundedNode, EdgeData, GraphNode, NodeData};
use cognee_visualization::preprocess;
use cognee_visualization::preprocessor::{
    COMPACT_PROPERTY_KEYS, CompactGraphAccumulator, SEMANTIC_TYPE_KEY, compact_chunk, compact_link,
    compact_node,
};
use serde_json::{Map, Value, json};

fn props(value: Value) -> NodeData {
    value
        .as_object()
        .expect("object")
        .iter()
        .map(|(k, v)| (Cow::Owned(k.clone()), v.clone()))
        .collect()
}

/// A tiny deterministic generator (the fixture only needs variety).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) % bound as u64) as usize
    }
}

/// Nodes of every kind the naming rules treat differently, and random edges.
fn graph(seed: u64, count: usize) -> (Vec<GraphNode>, Vec<EdgeData>) {
    let mut rng = Lcg(seed);
    let chunk_text = "  Neon was  chosen\nfor its latency. ".repeat(20);
    let kinds = [
        json!({"type": "Entity", "name": "Neon"}),
        json!({"type": "DocumentChunk", "text": chunk_text}),
        json!({"type": "TextSummary", "summary": "Why neon won."}),
        json!({"type": "Entity", "name": "3f2b8c1e-8a3f-4a51-9a6e-0f7d2c4b9e11"}),
        json!({"type": "EntityType", "name": "Database"}),
        json!({"type": "TextDocument", "name": "adr-12.md", "source_node_set": "docs"}),
        json!({"type": "Entity", "name": "Rust", "belongs_to_set": ["session_learnings"]}),
        json!({"type": "Mystery"}),
    ];
    let node_sets = [
        Value::Null,
        json!("docs"),
        json!("code"),
        json!("docs,code"),
    ];
    let nodes: Vec<GraphNode> = (0..count)
        .map(|index| {
            let mut properties = props(kinds[rng.next(kinds.len())].clone());
            properties.insert(Cow::Borrowed("noise"), json!("x".repeat(rng.next(51))));
            properties.insert(
                Cow::Borrowed("source_node_set"),
                node_sets[rng.next(node_sets.len())].clone(),
            );
            (format!("n{index}"), properties)
        })
        .collect();
    let relations = ["contains", "is_a", "made_from", "mentions", "related_to"];
    let mut seen = BTreeSet::new();
    let mut edges = Vec::new();
    for _ in 0..count * 3 {
        let edge = (
            format!("n{}", rng.next(count)),
            format!("n{}", rng.next(count)),
            relations[rng.next(relations.len())].to_string(),
        );
        if seen.insert(edge.clone()) {
            edges.push((edge.0, edge.1, edge.2, HashMap::new()));
        }
    }
    (nodes, edges)
}

/// Python's `chunk_members`: nodes in order, each edge in the chunk holding
/// its later endpoint.
fn chunk_members(
    nodes: &[GraphNode],
    edges: &[EdgeData],
    chunk_size: usize,
) -> Vec<(Vec<GraphNode>, Vec<EdgeData>)> {
    BoundedNeighborhood {
        nodes: nodes
            .iter()
            .map(|(id, data)| BoundedNode {
                id: id.clone(),
                hops: 0,
                data: data.clone(),
            })
            .collect(),
        edges: edges.to_vec(),
    }
    .into_chunks(chunk_size)
}

fn by_id(nodes: &[Value]) -> HashMap<String, Value> {
    nodes
        .iter()
        .map(|n| (n["id"].as_str().unwrap().to_string(), n.clone()))
        .collect()
}

#[test]
fn the_projection_keeps_every_name_fallback() {
    for key in ["title", "text", "summary", "description", "content"] {
        assert!(COMPACT_PROPERTY_KEYS.contains(&key), "{key}");
    }
    assert_eq!(
        COMPACT_PROPERTY_KEYS,
        [
            "name",
            "title",
            "text",
            "summary",
            "description",
            "content",
            "belongs_to_set",
            "source_node_set"
        ]
    );
}

#[test]
fn compact_nodes_match_preprocess() {
    let (nodes, edges) = graph(7, 120);
    let expected = by_id(&preprocess(nodes.clone(), edges, None).nodes);
    for (node_id, properties) in &nodes {
        let node = compact_node(node_id, properties);
        let full = &expected[node_id];
        for key in ["name", "stage", "is_unnamed", "type"] {
            assert_eq!(node[key], full[key], "{node_id} {key}");
        }
        assert!(node.get("noise").is_none());
        assert!(node.get("text").is_none());
    }
}

#[test]
fn compact_node_has_python_key_order() {
    let node = compact_node(
        "a",
        &props(
            json!({"type": "Entity", "name": "A", "source_node_set": "docs",
                     "belongs_to_set": ["x"]}),
        ),
    );
    let keys: Vec<&str> = node
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "id",
            "name",
            "type",
            "entity_type",
            "stage",
            "is_unnamed",
            "belongs_to_set",
            "source_node_set"
        ]
    );
    // Null node-set values are left out, a missing type is null + "Node".
    let bare = compact_node("b", &props(json!({"source_node_set": null})));
    assert_eq!(
        bare,
        json!({"id": "b", "name": "Unnamed node (b)", "type": null, "entity_type": "Node",
               "stage": "other", "is_unnamed": true})
    );
}

#[test]
fn projected_properties_are_enough_for_the_compact_node() {
    let (nodes, _) = graph(7, 120);
    for (node_id, properties) in &nodes {
        let mut projected: NodeData = properties
            .iter()
            .filter(|(k, _)| COMPACT_PROPERTY_KEYS.contains(&k.as_ref()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        projected.insert(
            Cow::Borrowed("type"),
            properties.get("type").cloned().unwrap_or(Value::Null),
        );
        assert_eq!(
            compact_node(node_id, &projected),
            compact_node(node_id, properties)
        );
    }
}

#[test]
fn compact_links_match_preprocess() {
    let (nodes, edges) = graph(7, 120);
    let expected: HashMap<(String, String, String), Value> = preprocess(nodes, edges.clone(), None)
        .links
        .iter()
        .map(|l| {
            (
                (
                    l["source"].as_str().unwrap().to_string(),
                    l["target"].as_str().unwrap().to_string(),
                    l["relation"].as_str().unwrap().to_string(),
                ),
                l["edge_class"].clone(),
            )
        })
        .collect();
    for edge in &edges {
        let link = compact_link(edge);
        assert_eq!(
            link.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["source", "target", "relation", "edge_class"]
        );
        assert_eq!(
            link["edge_class"],
            expected[&(edge.0.clone(), edge.1.clone(), edge.2.clone())]
        );
    }
}

#[test]
fn the_summary_equals_preprocess_in_one_piece() {
    for chunk_size in [1, 7, 50, 1000] {
        let (nodes, edges) = graph(7, 120);
        let pre = preprocess(nodes.clone(), edges.clone(), None);

        let mut accumulator = CompactGraphAccumulator::new();
        for (chunk_nodes, chunk_edges) in chunk_members(&nodes, &edges, chunk_size) {
            let (compact_nodes, links) = compact_chunk(&chunk_nodes, &chunk_edges);
            accumulator.add(&compact_nodes, &links);
        }
        let mut summary = accumulator.summary();

        // A late entity_type correction, where present, is preprocess's type.
        let full = by_id(&pre.nodes);
        let entries = summary["nodes"].as_object_mut().unwrap();
        for (node_id, entry) in entries.iter_mut() {
            if let Some(entity_type) = entry.as_object_mut().unwrap().remove("entity_type") {
                assert_eq!(entity_type, full[node_id]["entity_type"], "{node_id}");
            }
        }
        let expected: Map<String, Value> = pre
            .nodes
            .iter()
            .map(|n| {
                (
                    n["id"].as_str().unwrap().to_string(),
                    json!({"importance": n["importance"], "label_priority": n["label_priority"]}),
                )
            })
            .collect();
        assert_eq!(
            summary["nodes"],
            Value::Object(expected),
            "chunk {chunk_size}"
        );
        assert_eq!(
            summary["color_maps"]["node_set"],
            serde_json::to_value(&pre.color_maps.node_set).unwrap()
        );
        assert_eq!(accumulator.node_count(), pre.nodes.len());
        assert_eq!(accumulator.link_count(), pre.links.len());
    }
}

#[test]
fn the_summary_of_an_empty_graph() {
    assert_eq!(
        CompactGraphAccumulator::new().summary(),
        json!({"nodes": {}, "color_maps": {"node_set": {}}})
    );
}

fn json_and_stream_types(
    nodes: Vec<GraphNode>,
    edges: Vec<EdgeData>,
) -> (HashMap<String, Value>, HashMap<String, Value>) {
    let (stream_nodes, _) = compact_chunk(&nodes, &edges);
    let json_types = preprocess(nodes, edges, None)
        .nodes
        .iter()
        .map(|n| {
            (
                n["id"].as_str().unwrap().to_string(),
                n["entity_type"].clone(),
            )
        })
        .collect();
    let stream_types = stream_nodes
        .iter()
        .map(|n| {
            (
                n["id"].as_str().unwrap().to_string(),
                n["entity_type"].clone(),
            )
        })
        .collect();
    (json_types, stream_types)
}

#[test]
fn json_and_stream_agree_on_entity_type() {
    let nodes = vec![
        (
            "alice".to_string(),
            props(json!({"type": "Entity", "name": "Alice", SEMANTIC_TYPE_KEY: "Person"})),
        ),
        (
            "bob".to_string(),
            props(json!({"type": "Entity", "name": "Bob"})),
        ),
        (
            "chunk".to_string(),
            props(json!({"type": "DocumentChunk", "text": "Alice knows Bob"})),
        ),
        ("untyped".to_string(), props(json!({"name": "x"}))),
    ];
    let (json_types, stream_types) = json_and_stream_types(nodes, vec![]);
    assert_eq!(json_types, stream_types);
    assert_eq!(
        json_types,
        HashMap::from([
            ("alice".to_string(), json!("Person")),
            ("bob".to_string(), json!("Entity")),
            ("chunk".to_string(), json!("DocumentChunk")),
            ("untyped".to_string(), json!("Node")),
        ])
    );
}

#[test]
fn json_and_stream_agree_when_only_the_is_a_link_types_an_entity() {
    let nodes = vec![
        (
            "alice".to_string(),
            props(json!({"type": "Entity", "name": "Alice"})),
        ),
        (
            "person".to_string(),
            props(json!({"type": "EntityType", "name": "Person"})),
        ),
    ];
    let edges = vec![(
        "alice".to_string(),
        "person".to_string(),
        "is_a".to_string(),
        HashMap::new(),
    )];
    let (json_types, stream_types) = json_and_stream_types(nodes, edges);
    assert_eq!(json_types, stream_types);
    assert_eq!(
        json_types,
        HashMap::from([
            ("alice".to_string(), json!("Person")),
            ("person".to_string(), json!("EntityType")),
        ])
    );
}

#[test]
fn a_late_is_a_link_is_corrected_in_the_summary() {
    // alice arrives in chunk 0 untyped; her type node and is_a link in chunk 1.
    let chunks = vec![
        (
            vec![(
                "alice".to_string(),
                props(json!({"type": "Entity", "name": "Alice"})),
            )],
            vec![],
        ),
        (
            vec![(
                "person".to_string(),
                props(json!({"type": "EntityType", "name": "Person"})),
            )],
            vec![(
                "alice".to_string(),
                "person".to_string(),
                "is_a".to_string(),
                HashMap::new(),
            )],
        ),
    ];
    let mut accumulator = CompactGraphAccumulator::new();
    for (nodes, edges) in &chunks {
        let (compact_nodes, links) = compact_chunk(nodes, edges);
        if nodes[0].0 == "alice" {
            assert_eq!(compact_nodes[0]["entity_type"], json!("Entity"));
        }
        accumulator.add(&compact_nodes, &links);
    }
    let summary = accumulator.summary();
    assert_eq!(summary["nodes"]["alice"]["entity_type"], json!("Person"));
    assert!(summary["nodes"]["person"].get("entity_type").is_none());
}
