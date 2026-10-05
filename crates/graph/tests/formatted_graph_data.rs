#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test code — panics are acceptable failures"
)]
//! Unit tests for `cognee_graph::get_formatted_graph_data`.
//!
//! These tests run against the `MockGraphDB` (in-memory) — they validate the
//! shape of the formatted snapshot without needing any real backend.
//!
//! Python parity reference:
//! `cognee/modules/graph/methods/get_formatted_graph_data.py`.

#![cfg(feature = "testing")]

use std::borrow::Cow;
use std::collections::HashMap;

use cognee_graph::{GraphDBTrait, MockGraphDB, get_formatted_graph_data};
use serde_json::json;
use uuid::Uuid;

/// Build a node payload that exercises every formatting branch:
/// - `name` present → label takes `name`
/// - `type` present → projected into top-level `type`
/// - `created_at` / `updated_at` → stripped from `properties`
/// - a null-valued key → stripped from `properties`
/// - an arbitrary scalar property → preserved
fn build_node(id: &str, type_str: &str, name: Option<&str>, extra: &str) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert("id".into(), json!(id));
    obj.insert("type".into(), json!(type_str));
    if let Some(n) = name {
        obj.insert("name".into(), json!(n));
    }
    obj.insert("created_at".into(), json!("2024-01-01T00:00:00Z"));
    obj.insert("updated_at".into(), json!("2024-01-02T00:00:00Z"));
    obj.insert("description".into(), json!(extra));
    obj.insert("nullable".into(), serde_json::Value::Null);
    serde_json::Value::Object(obj)
}

#[tokio::test]
async fn formats_nodes_and_edges_in_python_shape() {
    let mock = MockGraphDB::new();
    mock.add_node_raw(build_node("alice", "Person", Some("Alice"), "engineer"))
        .await
        .expect("add alice");
    mock.add_node_raw(build_node("bob", "Person", Some("Bob"), "scientist"))
        .await
        .expect("add bob");
    // Anonymous node — label must fall back to "{type}_{id}".
    mock.add_node_raw(build_node("anon-1", "Document", None, "no-name"))
        .await
        .expect("add anon");

    mock.add_edge(
        "alice",
        "bob",
        "KNOWS",
        Some(HashMap::from([(Cow::Borrowed("weight"), json!(0.9))])),
    )
    .await
    .expect("add edge");
    mock.add_edge("alice", "anon-1", "AUTHORED", None)
        .await
        .expect("add edge 2");

    let dataset_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let snap = get_formatted_graph_data(&mock, dataset_id, user_id)
        .await
        .expect("get_formatted_graph_data");

    // Top-level shape.
    assert!(snap.is_object(), "snap must be an object");
    let nodes = snap
        .get("nodes")
        .and_then(|v| v.as_array())
        .expect("nodes array");
    let edges = snap
        .get("edges")
        .and_then(|v| v.as_array())
        .expect("edges array");

    assert_eq!(nodes.len(), 3, "expected 3 nodes, got {}", nodes.len());
    assert_eq!(edges.len(), 2, "expected 2 edges, got {}", edges.len());

    // Each node must have exactly {id, label, type, properties}.
    for node in nodes {
        let obj = node.as_object().expect("node is object");
        assert!(obj.contains_key("id"), "node missing id: {obj:?}");
        assert!(obj.contains_key("label"), "node missing label: {obj:?}");
        assert!(obj.contains_key("type"), "node missing type: {obj:?}");
        assert!(
            obj.contains_key("properties"),
            "node missing properties: {obj:?}"
        );

        // properties must exclude id/type/name/created_at/updated_at and any null.
        let props = obj.get("properties").and_then(|v| v.as_object()).unwrap();
        for forbidden in ["id", "type", "name", "created_at", "updated_at"] {
            assert!(
                !props.contains_key(forbidden),
                "properties leaked '{forbidden}': {props:?}"
            );
        }
        // The null-valued key must have been stripped too.
        assert!(
            !props.contains_key("nullable"),
            "properties leaked null-valued 'nullable': {props:?}"
        );
        // The genuine non-null property must survive.
        assert!(
            props.contains_key("description"),
            "expected 'description' to survive in properties: {props:?}"
        );
    }

    // Spot-check the label fallback.
    let anon = nodes
        .iter()
        .find(|n| n["id"] == "anon-1")
        .expect("anon-1 in nodes");
    assert_eq!(
        anon["label"], "Document_anon-1",
        "label must fall back to '{{type}}_{{id}}' when name is missing"
    );

    // Spot-check a named label.
    let alice = nodes
        .iter()
        .find(|n| n["id"] == "alice")
        .expect("alice in nodes");
    assert_eq!(alice["label"], "Alice", "label must come from `name`");

    // Each edge must have exactly {source, target, label}.
    for edge in edges {
        let obj = edge.as_object().expect("edge is object");
        assert!(obj.contains_key("source"));
        assert!(obj.contains_key("target"));
        assert!(obj.contains_key("label"));
        assert_eq!(obj.len(), 3, "edge must have exactly 3 keys: {obj:?}");
    }

    // Verify a known edge survives.
    let knows = edges
        .iter()
        .find(|e| e["label"] == "KNOWS")
        .expect("KNOWS edge");
    assert_eq!(knows["source"], "alice");
    assert_eq!(knows["target"], "bob");
}

#[tokio::test]
async fn empty_graph_returns_empty_arrays() {
    let mock = MockGraphDB::new();
    let snap = get_formatted_graph_data(&mock, Uuid::new_v4(), Uuid::new_v4())
        .await
        .expect("get_formatted_graph_data");

    let nodes = snap
        .get("nodes")
        .and_then(|v| v.as_array())
        .expect("nodes array");
    let edges = snap
        .get("edges")
        .and_then(|v| v.as_array())
        .expect("edges array");
    assert!(nodes.is_empty(), "expected empty nodes array");
    assert!(edges.is_empty(), "expected empty edges array");
}

#[tokio::test]
async fn label_falls_back_when_name_is_empty_string() {
    let mock = MockGraphDB::new();
    // Empty-string name — matches Python's `node[1]["name"] != ""` branch.
    mock.add_node_raw(json!({
        "id": "x",
        "type": "Thing",
        "name": "",
    }))
    .await
    .expect("add x");

    let snap = get_formatted_graph_data(&mock, Uuid::new_v4(), Uuid::new_v4())
        .await
        .expect("get_formatted_graph_data");
    let nodes = snap.get("nodes").and_then(|v| v.as_array()).unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(
        nodes[0]["label"], "Thing_x",
        "empty name must fall back to '{{type}}_{{id}}'"
    );
}
