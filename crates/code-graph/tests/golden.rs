//! Golden equality with Python cognee.
//!
//! `tests/fixtures/goldens/*.json` hold what Python cognee (parity pin
//! a59433e6b) produced for the snapshots under `tests/fixtures/snapshots/`,
//! once untagged and once with `node_set=["team-a", "shared"]`. Regenerate
//! them with `scripts/generate_code_graph_goldens.py`.
//!
//! `basic` and `docs_and_flows` are Python's own unit-test fixtures. The
//! others are written in this repo: `insights` (evidence resolved by
//! `fact_id`, by name, by bare file, and evidence nothing resolves),
//! `all_kinds` (every fact kind the first two lack, scalar and non-scalar
//! props, a null prop, `-0`, and a symbol whose `symbol_kind` is null) and
//! `cluster` (two repositories, with relations that do not resolve).
//!
//! Each node is compared field for field, None-valued fields included, with
//! its `part_of` repository and its `belongs_to_set` points, and the nodes
//! come in the order Python returns them. A difference the Rust port makes on
//! purpose is listed in [`ALLOWED_DIFFERENCES`], with the README entry that
//! documents it; any other difference fails.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]

mod common;

use cognee_code_graph::{
    CodeNode, build_code_graph_edges, map_facts_to_data_points, parse_enola_snapshot,
};
use cognee_models::HasDataPoint;
use serde_json::{Map, Value, json};

const REPO_PATH: &str = "/tmp/shop";

/// A node property Rust leaves out where Python's node holds `null`.
struct AllowedDifference {
    /// The Python class of the node, or `*` for every class.
    node_type: &'static str,
    /// The property.
    key: &'static str,
    /// The `crates/code-graph/README.md` entry that documents it.
    readme: &'static str,
}

/// README entry of the base `DataPoint` fields Rust leaves out when unset.
const UNSET_DATA_POINT_FIELDS: &str =
    "Parity > Known differences > Unset `DataPoint` fields are not serialised";

/// The node properties Rust leaves out on purpose while Python's node holds
/// `null`. Nothing else may differ.
const ALLOWED_DIFFERENCES: &[AllowedDifference] = &[
    AllowedDifference {
        node_type: "CodeSymbol",
        key: "symbol_kind",
        readme: "Parity > Known differences > `symbol_kind: null` on a symbol",
    },
    AllowedDifference {
        node_type: "*",
        key: "ontology_uri",
        readme: UNSET_DATA_POINT_FIELDS,
    },
    AllowedDifference {
        node_type: "*",
        key: "source_pipeline",
        readme: UNSET_DATA_POINT_FIELDS,
    },
    AllowedDifference {
        node_type: "*",
        key: "source_task",
        readme: UNSET_DATA_POINT_FIELDS,
    },
    AllowedDifference {
        node_type: "*",
        key: "source_node_set",
        readme: UNSET_DATA_POINT_FIELDS,
    },
    AllowedDifference {
        node_type: "*",
        key: "source_user",
        readme: UNSET_DATA_POINT_FIELDS,
    },
    AllowedDifference {
        node_type: "*",
        key: "source_content_hash",
        readme: UNSET_DATA_POINT_FIELDS,
    },
];

/// Whether [`ALLOWED_DIFFERENCES`] lists `key` for `node_type`.
fn allowed(node_type: &str, key: &str) -> bool {
    ALLOWED_DIFFERENCES.iter().any(|allowed| {
        (allowed.node_type == "*" || allowed.node_type == node_type) && allowed.key == key
    })
}

fn golden(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/goldens")
        .join(format!("{name}.json"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// A node as the golden script writes it.
fn node_entry(node: &CodeNode) -> Value {
    let part_of = node.as_entity().and_then(|entity| entity.part_of);
    let belongs_to_set = node.data_point().belongs_to_set.as_ref().map(|sets| {
        sets.iter()
            .map(|set| json!({"id": set["id"], "name": set["name"]}))
            .collect::<Vec<_>>()
    });
    json!({
        "id": node.id().to_string(),
        "type": node.node_type().unwrap().class_name(),
        "part_of": part_of.map(|id| id.to_string()),
        "belongs_to_set": belongs_to_set,
        "properties": Value::Object(common::golden_properties(node)),
    })
}

/// `wanted`'s properties as Rust is expected to produce them: a property
/// [`ALLOWED_DIFFERENCES`] lists is dropped when Python's value is `null`.
fn expected_properties(node_type: &str, wanted: &Map<String, Value>) -> Map<String, Value> {
    wanted
        .iter()
        .filter(|(key, value)| !(value.is_null() && allowed(node_type, key)))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn check(snapshot: &str, tag: &str, node_set: Option<&[String]>) {
    let expected = golden(&format!("{snapshot}__{tag}"));
    let (facts, _receipt) = parse_enola_snapshot(common::snapshot_dir(snapshot)).unwrap();

    let facts_value: Vec<Value> = facts.iter().cloned().map(Value::Object).collect();
    assert_eq!(Value::Array(facts_value), expected["facts"], "parsed facts");

    let nodes = map_facts_to_data_points(&facts, Some(REPO_PATH), None, node_set);
    let order: Vec<Value> = nodes
        .iter()
        .map(|node| json!(node.id().to_string()))
        .collect();
    assert_eq!(Value::Array(order), expected["node_order"], "node order");

    let mut entries: Vec<Value> = nodes.iter().map(node_entry).collect();
    entries.sort_by(|left, right| left["id"].as_str().cmp(&right["id"].as_str()));
    let expected_nodes = expected["nodes"].as_array().unwrap();
    assert_eq!(entries.len(), expected_nodes.len(), "node count");
    for (actual, wanted) in entries.iter().zip(expected_nodes) {
        let name = &wanted["properties"]["name"];
        for key in ["id", "type", "part_of", "belongs_to_set"] {
            assert_eq!(actual[key], wanted[key], "{key} of node {name}");
        }
        let node_type = wanted["type"].as_str().unwrap();
        assert_eq!(
            actual["properties"].as_object().unwrap(),
            &expected_properties(node_type, wanted["properties"].as_object().unwrap()),
            "properties of node {name}"
        );
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

#[test]
fn insights_snapshot_untagged_matches_python() {
    check("insights", "untagged", None);
}

#[test]
fn insights_snapshot_tagged_matches_python() {
    check("insights", "tagged", Some(&tag()));
}

#[test]
fn all_kinds_snapshot_untagged_matches_python() {
    check("all_kinds", "untagged", None);
}

#[test]
fn all_kinds_snapshot_tagged_matches_python() {
    check("all_kinds", "tagged", Some(&tag()));
}

#[test]
fn cluster_snapshot_untagged_matches_python() {
    check("cluster", "untagged", None);
}

#[test]
fn cluster_snapshot_tagged_matches_python() {
    check("cluster", "tagged", Some(&tag()));
}

/// Every allowed difference is exercised by a golden, so a fixed one does not
/// linger in the list.
#[test]
fn every_allowed_difference_occurs_in_a_golden() {
    let nodes: Vec<Value> = ["basic__untagged", "all_kinds__untagged"]
        .iter()
        .flat_map(|name| golden(name)["nodes"].as_array().unwrap().clone())
        .collect();
    for allowed in ALLOWED_DIFFERENCES {
        let occurs = nodes.iter().any(|node| {
            (allowed.node_type == "*" || node["type"] == allowed.node_type)
                && node["properties"]
                    .get(allowed.key)
                    .is_some_and(Value::is_null)
        });
        assert!(
            occurs,
            "{}.{} ({}) no longer occurs",
            allowed.node_type, allowed.key, allowed.readme
        );
    }
}
