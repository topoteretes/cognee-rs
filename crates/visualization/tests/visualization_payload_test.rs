#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! `build_visualization_payload` / `build_brain_summary_payload` and the
//! `search_events` hook of the HTML renderer — port of Python's
//! `test_visualization_payload.py` and
//! `test_orchestrator_assembly.py::test_search_events_kwarg_is_embedded`.

use std::borrow::Cow;
use std::collections::HashMap;

use cognee_graph::{EdgeData, GraphNode};
use cognee_visualization::payload::VISUALIZATION_PAYLOAD_KEYS;
use cognee_visualization::{
    build_brain_summary_payload, build_visualization_payload, render_graph_data,
    render_graph_data_with_events,
};
use serde_json::{Value, json};

fn node(id: &str, value: Value) -> GraphNode {
    (
        id.to_string(),
        value
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (Cow::Owned(k.clone()), v.clone()))
            .collect(),
    )
}

fn edge(s: &str, t: &str, r: &str) -> EdgeData {
    (s.to_string(), t.to_string(), r.to_string(), HashMap::new())
}

/// A small graph whose nodes carry provenance, so `provenance_index` and the
/// node-set colors are populated.
fn alice_like_graph() -> (Vec<GraphNode>, Vec<EdgeData>) {
    (
        vec![
            node(
                "doc",
                json!({"type": "TextDocument", "name": "alice.txt",
                       "source_pipeline": "add", "source_task": "ingest",
                       "source_node_set": "docs"}),
            ),
            node(
                "chunk",
                json!({"type": "DocumentChunk", "text": "Alice knows Bob",
                       "source_pipeline": "cognify", "source_task": "extract_chunks"}),
            ),
            node("alice", json!({"type": "Entity", "name": "Alice"})),
            node("person", json!({"type": "EntityType", "name": "Person"})),
        ],
        vec![
            edge("doc", "chunk", "contains"),
            edge("chunk", "alice", "contains"),
            edge("alice", "person", "is_a"),
        ],
    )
}

#[test]
fn payload_has_pythons_keys_in_order_and_survives_json() {
    let (nodes, edges) = alice_like_graph();
    let payload = build_visualization_payload(nodes, edges, None, vec![]).unwrap();
    let keys: Vec<&str> = payload
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, VISUALIZATION_PAYLOAD_KEYS);
    assert!(payload.get("semantic_positions").is_none());
    assert!(!payload["provenance_index"].as_object().unwrap().is_empty());
    let encoded = serde_json::to_string(&payload).unwrap();
    let decoded: Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded["provenance_index"], payload["provenance_index"]);
    assert_eq!(payload["schema_data"], Value::Null);
    assert_eq!(
        payload["color_maps"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        ["task", "pipeline", "node_set", "user"]
    );
}

#[test]
fn search_events_default_to_empty_and_pass_through() {
    let (nodes, edges) = alice_like_graph();
    let payload = build_visualization_payload(nodes.clone(), edges.clone(), None, vec![]).unwrap();
    assert_eq!(payload["search_events"], json!([]));

    let events = vec![json!({"kind": "search", "time": "2026-06-10T10:31:02", "qa_id": "q1"})];
    let payload =
        build_visualization_payload(nodes, edges, Some(json!({"tables": []})), events.clone())
            .unwrap();
    assert_eq!(payload["search_events"], Value::Array(events));
    assert_eq!(payload["schema_data"], json!({"tables": []}));
}

#[test]
fn brain_summary_payload_matches_the_business_export_contract() {
    let (nodes, edges) = alice_like_graph();
    let payload = build_brain_summary_payload("billing", nodes, edges).unwrap();
    let keys: Vec<&str> = payload
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, ["name", "nodes", "links", "node_set_colors"]);
    assert_eq!(payload["name"], json!("billing"));
    assert_eq!(payload["nodes"].as_array().unwrap().len(), 4);
    assert_eq!(payload["links"].as_array().unwrap().len(), 3);
    assert!(payload["node_set_colors"].get("docs").is_some());
}

/// The `const searchEvents = …;` value embedded in the Memory view.
fn embedded_search_events(html: &str) -> Value {
    let marker = "const searchEvents = ";
    let start = html.find(marker).expect("searchEvents declared") + marker.len();
    let end = start + html[start..].find(";\n").expect("statement ends");
    serde_json::from_str(&html[start..end].replace("<\\/", "</")).expect("valid JSON")
}

#[test]
fn search_events_are_embedded_in_the_html() {
    let (nodes, edges) = alice_like_graph();
    let events = vec![json!({
        "time": "2026-06-10T10:31:02",
        "qa_id": "qa-1",
        "question": "Who knows </script> Bob?",
        "answer": "Alice.",
        "node_ids": ["alice"],
        "edge_ids": [],
    })];
    let html = render_graph_data_with_events(nodes.clone(), edges.clone(), None, &events).unwrap();
    assert_eq!(embedded_search_events(&html), Value::Array(events));
    assert!(!html.contains("Who knows </script>"));

    let plain = render_graph_data(nodes, edges).unwrap();
    assert_eq!(embedded_search_events(&plain), json!([]));
}
