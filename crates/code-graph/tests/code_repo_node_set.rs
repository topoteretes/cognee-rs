//! Port of the mapping tests of Python's
//! `tests/unit/tasks/code_graph/test_code_repo_node_set.py`: a call-level
//! `node_set` tags every code node and enters `fact_hash` only when given.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]

mod common;

use std::collections::BTreeMap;

use cognee_code_graph::{CodeNode, CodeNodeType, map_facts_to_data_points};
use cognee_utils::generate_node_id;
use common::facts;
use serde_json::json;

const REPO: &str = "demo_repo";

fn repo_facts() -> Vec<cognee_code_graph::Fact> {
    facts(json!([
        {"kind": "symbol", "name": "app/db.Database", "file": "app/db.py", "repo": REPO},
        {"kind": "symbol", "name": "app/api.handler", "file": "app/api.py", "repo": REPO,
         "relations": [{"kind": "calls", "target": "app/db.Database"}]},
    ]))
}

fn map(node_set: Option<&[&str]>) -> Vec<CodeNode> {
    let names: Option<Vec<String>> =
        node_set.map(|names| names.iter().map(|name| name.to_string()).collect());
    map_facts_to_data_points(
        &repo_facts(),
        Some(&format!("/repos/{REPO}")),
        None,
        names.as_deref(),
    )
}

fn hashes(nodes: &[CodeNode]) -> BTreeMap<String, Option<String>> {
    nodes
        .iter()
        .map(|node| {
            (
                node.name().to_string(),
                node.as_entity().and_then(|entity| entity.fact_hash.clone()),
            )
        })
        .collect()
}

fn symbol_hashes(nodes: &[CodeNode]) -> BTreeMap<String, Option<String>> {
    hashes(nodes)
        .into_iter()
        .filter(|(name, _)| name != REPO)
        .collect()
}

#[test]
fn every_node_including_repository_carries_the_tag() {
    let nodes = map(Some(&["team-x"]));
    assert_eq!(nodes.len(), 3);
    for node in &nodes {
        let base = cognee_models::HasDataPoint::data_point(node);
        assert_eq!(base.source_node_set.as_deref(), Some("team-x"));
        let sets = base.belongs_to_set.as_ref().unwrap();
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0]["name"], "team-x");
        assert_eq!(sets[0]["type"], "NodeSet");
        assert_eq!(
            sets[0]["id"],
            generate_node_id("NodeSet:team-x").to_string()
        );
        // The graph keeps the names, as Python's node does.
        assert_eq!(node.graph_properties()["belongs_to_set"], json!(["team-x"]));
    }
    assert!(
        nodes
            .iter()
            .any(|node| node.node_type() == Some(CodeNodeType::CodeRepository))
    );
}

#[test]
fn untagged_call_leaves_belongs_to_set_unset() {
    for node in map(None) {
        let base = cognee_models::HasDataPoint::data_point(&node);
        assert!(base.belongs_to_set.is_none());
        assert!(base.source_node_set.is_none());
    }
}

#[test]
fn untagged_hash_is_unaffected_by_the_node_set_parameter() {
    let baseline = hashes(&map(None));
    assert_eq!(hashes(&map(Some(&[]))), baseline);
    for node in map(Some(&[])) {
        assert!(
            cognee_models::HasDataPoint::data_point(&node)
                .belongs_to_set
                .is_none()
        );
    }
}

#[test]
fn node_set_changes_the_fact_hash() {
    let tagged_a = symbol_hashes(&map(Some(&["a"])));
    let tagged_b = symbol_hashes(&map(Some(&["b"])));
    let untagged = symbol_hashes(&map(None));
    assert_ne!(tagged_a, tagged_b);
    assert_ne!(tagged_a, untagged);
    // Re-tagging with the same names in another order changes nothing.
    assert_eq!(
        hashes(&map(Some(&["a", "b"]))),
        hashes(&map(Some(&["b", "a"])))
    );
}
