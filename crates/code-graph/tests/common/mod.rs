//! Helpers shared by the code graph integration tests.

#![allow(dead_code, reason = "each test binary uses a different subset")]

use std::path::{Path, PathBuf};

use cognee_code_graph::{CodeGraphEdge, CodeNode, Fact, fact_node_id};
use serde_json::{Map, Value};
use uuid::Uuid;

/// `tests/fixtures/snapshots/<name>`.
pub fn snapshot_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/snapshots")
        .join(name)
}

/// Python's `fixtures/` directory (no docs, no flows).
pub fn basic_dir() -> PathBuf {
    snapshot_dir("basic")
}

/// Python's `fixtures/docs_and_flows/` directory.
pub fn docs_and_flows_dir() -> PathBuf {
    snapshot_dir("docs_and_flows")
}

/// Facts from a JSON array literal.
pub fn facts(value: Value) -> Vec<Fact> {
    match value {
        Value::Array(items) => items
            .into_iter()
            .map(|item| match item {
                Value::Object(map) => map,
                other => panic!("fact is not an object: {other}"),
            })
            .collect(),
        other => panic!("facts are not an array: {other}"),
    }
}

/// Write `facts.jsonl` (one `json.dumps` per line), plus optional
/// `receipt.json`, `insights.json` and `flows.json`.
pub fn write_snapshot(
    dir: &Path,
    facts: &Value,
    receipt: Option<&Value>,
    insights: Option<&Value>,
    flows: Option<&Value>,
) {
    let lines: Vec<String> = facts
        .as_array()
        .expect("facts are an array")
        .iter()
        .map(Value::to_string)
        .collect();
    std::fs::write(dir.join("facts.jsonl"), lines.join("\n") + "\n").expect("write facts");
    for (name, value) in [
        ("receipt.json", receipt),
        ("insights.json", insights),
        ("flows.json", flows),
    ] {
        if let Some(value) = value {
            std::fs::write(dir.join(name), value.to_string()).expect("write artifact");
        }
    }
}

/// Node by name (the last one wins, as Python's dict comprehension does).
pub fn by_name<'a>(nodes: &'a [CodeNode], name: &str) -> &'a CodeNode {
    nodes
        .iter()
        .rev()
        .find(|node| node.name() == name)
        .unwrap_or_else(|| panic!("no node named {name}"))
}

/// Node by Python class name and node name.
pub fn by_type_and_name<'a>(nodes: &'a [CodeNode], class: &str, name: &str) -> &'a CodeNode {
    nodes
        .iter()
        .find(|node| {
            node.node_type().map(|node_type| node_type.class_name()) == Some(class)
                && node.name() == name
        })
        .unwrap_or_else(|| panic!("no {class} named {name}"))
}

/// The fact node, panicking for a repository.
pub fn entity(node: &CodeNode) -> &cognee_code_graph::CodeGraphEntity {
    node.as_entity().expect("a fact node")
}

/// `{name: fact_hash}` of every fact node (repositories excluded).
pub fn fact_hashes(nodes: &[CodeNode]) -> Vec<(String, String)> {
    let mut hashes: Vec<(String, String)> = nodes
        .iter()
        .filter_map(CodeNode::as_entity)
        .map(|entity| {
            (
                entity.name.clone(),
                entity.fact_hash.clone().expect("fact_hash"),
            )
        })
        .collect();
    hashes.sort();
    hashes
}

/// `(source, relationship, target)` id triples of `edges`.
pub fn triples(edges: &[CodeGraphEdge]) -> std::collections::HashSet<(Uuid, String, Uuid)> {
    edges
        .iter()
        .map(|edge| {
            (
                edge.source_id,
                edge.relationship_name.clone(),
                edge.target_id,
            )
        })
        .collect()
}

/// Sorted `(source name, relationship, target name)` triples.
pub fn name_triples(edges: &[CodeGraphEdge], nodes: &[CodeNode]) -> Vec<(String, String, String)> {
    let name = |id: Uuid| {
        nodes
            .iter()
            .find(|node| node.id() == id)
            .map(|node| node.name().to_string())
            .unwrap_or_else(|| panic!("edge endpoint {id} is not a node"))
    };
    let mut triples: Vec<_> = edges
        .iter()
        .map(|edge| {
            (
                name(edge.source_id),
                edge.relationship_name.clone(),
                name(edge.target_id),
            )
        })
        .collect();
    triples.sort();
    triples
}

/// `fact_node_id` shorthand.
pub fn id(repo: &str, kind: &str, name: &str) -> Uuid {
    fact_node_id(repo, kind, name)
}

/// A node's logical graph properties, as the golden script writes them:
/// no timestamps, no None-valued field.
pub fn logical_properties(node: &CodeNode) -> Map<String, Value> {
    node.graph_properties()
        .into_iter()
        .filter(|(key, value)| !value.is_null() && key != "created_at" && key != "updated_at")
        .collect()
}

/// A node's graph properties as the golden script writes them: no
/// timestamps, None-valued fields kept as null.
pub fn golden_properties(node: &CodeNode) -> Map<String, Value> {
    node.graph_properties()
        .into_iter()
        .filter(|(key, _)| key != "created_at" && key != "updated_at")
        .collect()
}
