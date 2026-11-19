//! Golden equality with Python cognee.
//!
//! `tests/fixtures/goldens/*.json` hold what Python cognee (parity pin
//! a59433e6b) produced for the snapshots under `tests/fixtures/snapshots/`,
//! once untagged and once with `node_set=["team-a", "shared"]`. Regenerate
//! them with `scripts/generate_code_graph_goldens.py`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]

mod common;

use cognee_code_graph::{build_code_graph_edges, map_facts_to_data_points, parse_enola_snapshot};
use serde_json::{Map, Value, json};

const REPO_PATH: &str = "/tmp/shop";

fn golden(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/goldens")
        .join(format!("{name}.json"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn check(snapshot: &str, tag: &str, node_set: Option<&[String]>) {
    let expected = golden(&format!("{snapshot}__{tag}"));
    let (facts, _receipt) = parse_enola_snapshot(common::snapshot_dir(snapshot)).unwrap();

    let facts_value: Vec<Value> = facts.iter().cloned().map(Value::Object).collect();
    assert_eq!(Value::Array(facts_value), expected["facts"], "parsed facts");

    let mut nodes: Vec<Value> = map_facts_to_data_points(&facts, Some(REPO_PATH), None, node_set)
        .iter()
        .map(|node| {
            json!({
                "id": node.id().to_string(),
                "type": node.node_type().unwrap().class_name(),
                "properties": Value::Object(common::logical_properties(node)),
            })
        })
        .collect();
    nodes.sort_by(|left, right| left["id"].as_str().cmp(&right["id"].as_str()));
    let expected_nodes = expected["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), expected_nodes.len(), "node count");
    for (actual, wanted) in nodes.iter().zip(expected_nodes) {
        assert_eq!(actual, wanted, "node {}", wanted["properties"]["name"]);
    }

    let (edges, skipped) = build_code_graph_edges(&facts, Some(REPO_PATH));
    assert_eq!(json!(skipped), expected["skipped"], "skipped relations");
    let edges: Vec<Value> = edges
        .iter()
        .map(|edge| {
            let properties: Map<String, Value> = edge
                .properties
                .iter()
                .filter(|(key, _)| key.as_str() != "updated_at")
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            json!({
                "source": edge.source_id.to_string(),
                "target": edge.target_id.to_string(),
                "relationship_name": edge.relationship_name,
                "properties": properties,
            })
        })
        .collect();
    assert_eq!(Value::Array(edges), expected["edges"], "edges, in order");
}

fn tag() -> Vec<String> {
    vec!["team-a".to_string(), "shared".to_string()]
}

#[test]
fn basic_snapshot_untagged_matches_python() {
    check("basic", "untagged", None);
}

#[test]
fn basic_snapshot_tagged_matches_python() {
    check("basic", "tagged", Some(&tag()));
}

#[test]
fn docs_and_flows_snapshot_untagged_matches_python() {
    check("docs_and_flows", "untagged", None);
}

#[test]
fn docs_and_flows_snapshot_tagged_matches_python() {
    check("docs_and_flows", "tagged", Some(&tag()));
}
